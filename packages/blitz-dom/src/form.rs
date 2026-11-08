use blitz_traits::node_id::NodeId;
use markup5ever::{LocalName, local_name};

use crate::{
    BaseDocument, ElementData,
    traversal::{AncestorTraverser, TreeTraverser},
};
use blitz_traits::{
    navigation::NavigationOptions,
    net::{Body, Entry, EntryValue, FormData, Method},
};
use core::str::FromStr;
use std::{cell::RefCell, fmt::Display, marker::PhantomData, rc::Rc};

/// https://url.spec.whatwg.org/#default-encode-set
const DEFAULT_ENCODE_SET: percent_encoding::AsciiSet = percent_encoding::CONTROLS
    // Query Set
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    // Path Set
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

enum FormSubmissionGate {
    Unrestricted,
    Restricted(Option<LookupFormSubmission>),
}

thread_local! {
    static FORM_SUBMISSION_GATE: RefCell<FormSubmissionGate> =
        const { RefCell::new(FormSubmissionGate::Unrestricted) };
}

/// Scope a native form policy to the embedder's document thread, including
/// any nested document events dispatched on that thread.
#[must_use]
pub struct FormSubmissionScope {
    previous: FormSubmissionGate,
    _thread: PhantomData<Rc<()>>,
}

impl FormSubmissionScope {
    /// Deny native form submission until the scope ends.
    pub fn restricted() -> Self {
        Self::enter(FormSubmissionGate::Restricted(None))
    }

    /// Permit one matching native submission within the action dispatch.
    pub fn allow(grant: LookupFormSubmission) -> Self {
        Self::enter(FormSubmissionGate::Restricted(Some(grant)))
    }

    fn enter(gate: FormSubmissionGate) -> Self {
        Self {
            previous: FORM_SUBMISSION_GATE.with(|current| current.replace(gate)),
            _thread: PhantomData,
        }
    }
}

impl Drop for FormSubmissionScope {
    fn drop(&mut self) {
        let previous = std::mem::replace(&mut self.previous, FormSubmissionGate::Unrestricted);
        FORM_SUBMISSION_GATE.with(|current| current.replace(previous));
    }
}

/// A native GET submission prepared for one submitter in the live document.
#[derive(Debug)]
pub struct LookupFormSubmission {
    document: usize,
    form: NodeId,
    submitter: NodeId,
    destination: url::Url,
}

impl LookupFormSubmission {
    pub fn form_id(&self) -> NodeId {
        self.form
    }

    pub fn destination(&self) -> &url::Url {
        &self.destination
    }
}

#[derive(Debug)]
pub struct LookupFormSubmissionDenied;

impl BaseDocument {
    /// Resolve the exact form owner and effective route using the same attributes
    /// that the native default action will read after page handlers run.
    pub fn lookup_form_submission(
        &self,
        submitter: NodeId,
    ) -> Result<Option<LookupFormSubmission>, LookupFormSubmissionDenied> {
        let Some(form) = self.controls_to_form.get(&submitter).copied() else {
            return Ok(None);
        };
        let control = self
            .get_node(submitter)
            .and_then(|node| node.element_data())
            .ok_or(LookupFormSubmissionDenied)?;
        if !is_native_submitter(control) {
            return Ok(None);
        }
        let element = self
            .get_node(form)
            .and_then(|node| node.element_data())
            .filter(|element| element.name.local == local_name!("form"))
            .ok_or(LookupFormSubmissionDenied)?;
        let (method, destination) =
            form_route(self, element, submitter).ok_or(LookupFormSubmissionDenied)?;
        if method != FormMethod::Get
            || destination.scheme() != "https"
            || destination.origin() != self.url().origin()
            || !destination.username().is_empty()
            || destination.password().is_some()
        {
            return Err(LookupFormSubmissionDenied);
        }
        Ok(Some(LookupFormSubmission {
            document: self.id(),
            form,
            submitter,
            destination,
        }))
    }

    /// Resets the form owner for a given node by either using an explicit form attribute
    /// or finding the nearest ancestor form element
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node whose form owner needs to be reset
    ///
    /// <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#reset-the-form-owner>
    pub fn reset_form_owner(&mut self, node_id: NodeId) {
        let node = &self.nodes[node_id];
        let Some(element) = node.element_data() else {
            return;
        };

        // First try explicit form attribute
        let final_owner_id = element
            .attr(local_name!("form"))
            .and_then(|owner| self.get_element_by_id(owner))
            .filter(|owner_id| {
                self.get_node(*owner_id)
                    .is_some_and(|node| node.data.is_element_with_tag_name(&local_name!("form")))
            })
            .or_else(|| {
                AncestorTraverser::new(self, node_id).find(|ancestor_id| {
                    self.nodes[*ancestor_id]
                        .data
                        .is_element_with_tag_name(&local_name!("form"))
                })
            });

        if let Some(final_owner_id) = final_owner_id {
            self.controls_to_form.insert(node_id, final_owner_id);
        }
    }

    /// Submits a form with the given form node ID and submitter node ID
    ///
    /// # Arguments
    /// * `node_id` - The ID of the form node to submit
    /// * `submitter_id` - The ID of the node that triggered the submission
    ///
    /// <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm>
    pub fn submit_form(&self, node_id: NodeId, submitter_id: NodeId) {
        let Some(element) = self.get_node(node_id).and_then(|node| node.element_data()) else {
            return;
        };
        let Some((method, mut parsed_action)) = form_route(self, element, submitter_id) else {
            return;
        };
        let permitted = FORM_SUBMISSION_GATE.with(|gate| match &mut *gate.borrow_mut() {
            FormSubmissionGate::Unrestricted => true,
            FormSubmissionGate::Restricted(grant) => grant.take().is_some_and(|grant| {
                grant.document == self.id()
                    && grant.form == node_id
                    && grant.submitter == submitter_id
                    && self.controls_to_form.get(&submitter_id) == Some(&node_id)
                    && method == FormMethod::Get
                    && parsed_action == grant.destination
            }),
        });
        if !permitted {
            return;
        }

        let entry = construct_entry_list(self, node_id, submitter_id);

        let scheme = parsed_action.scheme();

        let enctype = get_form_attr(
            self,
            element,
            local_name!("enctype"),
            submitter_id,
            local_name!("formenctype"),
        )
        .and_then(|enctype| enctype.parse::<RequestContentType>().ok())
        .unwrap_or(RequestContentType::FormUrlEncoded);

        let mut post_resource = Body::Empty;

        match (scheme, method) {
            ("http" | "https" | "data", FormMethod::Get) => {
                let pairs = convert_to_list_of_name_value_pairs(entry);
                let mut query = String::new();
                url::form_urlencoded::Serializer::new(&mut query).extend_pairs(pairs);
                parsed_action.set_query(Some(&query));
            }
            ("http" | "https", FormMethod::Post) => post_resource = Body::Form(entry),
            ("mailto", FormMethod::Get) => {
                let pairs = convert_to_list_of_name_value_pairs(entry);
                parsed_action.query_pairs_mut().extend_pairs(pairs);
            }
            ("mailto", FormMethod::Post) => {
                let pairs = convert_to_list_of_name_value_pairs(entry);
                let body = match enctype {
                    RequestContentType::TextPlain => {
                        let body = encode_text_plain(&pairs);
                        percent_encoding::utf8_percent_encode(&body, &DEFAULT_ENCODE_SET)
                            .to_string()
                    }
                    _ => {
                        let mut body = String::new();
                        url::form_urlencoded::Serializer::new(&mut body).extend_pairs(pairs);
                        body
                    }
                };
                let mut query = if let Some(query) = parsed_action.query() {
                    let mut query = query.to_string();
                    query.push('&');
                    query
                } else {
                    String::new()
                };
                query.push_str("body=");
                query.push_str(&body);
                parsed_action.set_query(Some(&query));
            }
            _ => {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "Scheme {} with method {:?} is not implemented",
                    scheme,
                    method
                );
                return;
            }
        }

        let method = method.try_into().unwrap_or_default();

        let navigation_options =
            NavigationOptions::new(parsed_action, Some(enctype.to_string()), self.id())
                .set_document_resource(post_resource)
                .set_method(method);

        self.navigation_provider.navigate_to(navigation_options)
    }
}

/// Constructs a list of form entries from form controls
///
/// # Arguments
/// * `doc` - Reference to the base document
/// * `form_id` - ID of the form element
/// * `submitter_id` - ID of the element that triggered form submission
///
/// # Returns
/// Returns an EntryList containing all valid form control entries
///
/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-the-form-data-set
fn construct_entry_list(doc: &BaseDocument, form_id: NodeId, submitter_id: NodeId) -> FormData {
    let mut entry_list = FormData::new();

    let mut create_entry = |name: &str, value: EntryValue| {
        entry_list.0.push(Entry {
            name: name.to_string(),
            value,
        });
    };

    fn datalist_ancestor(doc: &BaseDocument, node_id: NodeId) -> bool {
        AncestorTraverser::new(doc, node_id).any(|node_id| {
            doc.nodes[node_id]
                .data
                .is_element_with_tag_name(&local_name!("datalist"))
        })
    }

    // For each element field in controls, in tree order:
    for control_id in TreeTraverser::new(doc) {
        let Some(node) = doc.get_node(control_id) else {
            continue;
        };
        let Some(element) = node.element_data() else {
            continue;
        };

        // Check if the form owner is same as form_id
        if doc
            .controls_to_form
            .get(&control_id)
            .map(|owner_id| *owner_id != form_id)
            .unwrap_or(true)
        {
            continue;
        }

        let element_type = element.attr(local_name!("type"));

        //  If any of the following are true:
        //   field has a datalist element ancestor;
        //   field is disabled;
        //   field is a button but it is not submitter;
        //   field is an input element whose type attribute is in the Checkbox state and whose checkedness is false; or
        //   field is an input element whose type attribute is in the Radio Button state and whose checkedness is false,
        //  then continue.
        if datalist_ancestor(doc, node.id)
            || element.attr(local_name!("disabled")).is_some()
            || (element.name.local == local_name!("button") && node.id != submitter_id)
            || element.name.local == local_name!("input")
                && ((matches!(element_type, Some("checkbox" | "radio"))
                    && !element.checkbox_input_checked().unwrap_or(false))
                    || matches!(element_type, Some("submit" | "button")))
        {
            continue;
        }

        // If the field element is an input element whose type attribute is in the Image Button state, then:
        if element_type == Some("image") {
            // If the field element is not submitter, then continue.
            if node.id != submitter_id {
                continue;
            }
            // TODO: If the field element has a name attribute specified and its value is not the empty string, let name be that value followed by U+002E (.). Otherwise, let name be the empty string.
            //   Let namex be the concatenation of name and U+0078 (x).
            //   Let namey be the concatenation of name and U+0079 (y).
            //   Let (x, y) be the selected coordinate.
            //   Create an entry with namex and x, and append it to entry list.
            //   Create an entry with namey and y, and append it to entry list.
            //   Continue.
            continue;
        }

        // TODO: If the field is a form-associated custom element,
        //  then perform the entry construction algorithm given field and entry list,
        //  then continue.

        // If either the field element does not have a name attribute specified, or its name attribute's value is the empty string, then continue.
        // Let name be the value of the field element's name attribute.
        let Some(name) = element
            .attr(local_name!("name"))
            .filter(|str| !str.is_empty())
        else {
            continue;
        };

        // If the field element is a select element, then for each option
        // element in the select element's list of options whose selectedness is
        // true and that is not disabled, create an entry with name and the
        // value of the option element, and append it to entry list.
        //
        // Without this a select fell through to the generic tail below and
        // submitted its own literal `value` attribute, which a select does not
        // have. Every form containing a picker posted the wrong body.
        if element.name.local == local_name!("select") {
            let options = doc.select_options(control_id);
            for (index, option_id) in options.iter().enumerate() {
                let is_selected = element
                    .select_data()
                    .map(|data| data.is_selected(index))
                    // Before layout construction has run there is no live
                    // state, and the content attribute is all there is.
                    .unwrap_or_else(|| doc.option_is_selected(*option_id));
                if is_selected && !doc.option_is_disabled(*option_id) {
                    create_entry(name, doc.option_value(*option_id).as_str().into());
                }
            }
            continue;
        }

        // Otherwise, if the field element is an input element whose type attribute is in the Checkbox state or the Radio Button state, then:
        if element.name.local == local_name!("input")
            && matches!(element_type, Some("checkbox" | "radio"))
        {
            // If the field element has a value attribute specified, then let value be the value of that attribute; otherwise, let value be the string "on".
            let value = element.attr(local_name!("value")).unwrap_or("on");
            // Create an entry with name and value, and append it to entry list.
            create_entry(name, value.into());
            continue;
        }
        // Otherwise, if the field element is an input element whose type attribute is in the File Upload state, then:
        #[cfg(feature = "file-input")]
        if element.name.local == local_name!("input") && matches!(element_type, Some("file")) {
            // If there are no selected files, then create an entry with name and a new File object with an empty name, application/octet-stream as type, and an empty body, and append it to entry list.
            let Some(files) = element.file_data() else {
                create_entry(name, EntryValue::EmptyFile);
                continue;
            };
            if files.is_empty() {
                create_entry(name, EntryValue::EmptyFile);
            }
            // Otherwise, for each file in selected files, create an entry with name and a File object representing the file, and append it to entry list.
            else {
                for path_buf in files.iter() {
                    create_entry(name, path_buf.clone().into());
                }
            }
            continue;
        }
        //Otherwise, if the field element is an input element whose type attribute is in the Hidden state and name is an ASCII case-insensitive match for "_charset_":
        if element.name.local == local_name!("input")
            && element_type == Some("hidden")
            && name.eq_ignore_ascii_case("_charset_")
        {
            // Let charset be the name of encoding.
            let charset = "UTF-8"; // TODO: Support multiple encodings.
            // Create an entry with name and charset, and append it to entry list.
            create_entry(name, charset.into());
        }
        // Otherwise, create an entry with name and the value of the field element, and append it to entry list.
        else if let Some(text) = element.text_input_data() {
            create_entry(name, text.editor.text().to_string().as_str().into());
        } else if let Some(value) = element.attr(local_name!("value")) {
            create_entry(name, value.into());
        }
    }
    entry_list
}

fn form_route(
    doc: &BaseDocument,
    form: &ElementData,
    submitter: NodeId,
) -> Option<(FormMethod, url::Url)> {
    let method = get_form_attr(
        doc,
        form,
        local_name!("method"),
        submitter,
        local_name!("formmethod"),
    )
    .and_then(|method| method.parse::<FormMethod>().ok())
    .unwrap_or(FormMethod::Get);
    let action = get_form_attr(
        doc,
        form,
        local_name!("action"),
        submitter,
        local_name!("formaction"),
    )
    .unwrap_or_default();
    Some((method, doc.url.resolve_relative(action)?))
}

fn get_form_attr<'a>(
    doc: &'a BaseDocument,
    form: &'a ElementData,
    form_local: impl PartialEq<LocalName>,
    submitter_id: NodeId,
    submitter_local: impl PartialEq<LocalName>,
) -> Option<&'a str> {
    get_submitter_attr(doc, submitter_id, submitter_local).or_else(|| form.attr(form_local))
}

fn is_native_submitter(element: &ElementData) -> bool {
    element.is_submit_button()
        || (element.name.local == local_name!("input")
            && element.attr(local_name!("type")) == Some("submit"))
}

fn get_submitter_attr(
    doc: &BaseDocument,
    submitter_id: NodeId,
    local_name: impl PartialEq<LocalName>,
) -> Option<&str> {
    doc.get_node(submitter_id)
        .and_then(|node| node.element_data())
        .filter(|element| is_native_submitter(element))
        .and_then(|element| element.attr(local_name))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum FormMethod {
    Get,
    Post,
    Dialog,
}
impl FromStr for FormMethod {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "get" => FormMethod::Get,
            "post" => FormMethod::Post,
            "dialog" => FormMethod::Dialog,
            _ => return Err(()),
        })
    }
}
impl TryFrom<FormMethod> for Method {
    type Error = &'static str;
    fn try_from(method: FormMethod) -> Result<Self, Self::Error> {
        Ok(match method {
            FormMethod::Get => Method::GET,
            FormMethod::Post => Method::POST,
            FormMethod::Dialog => return Err("Dialog is not an HTTP method"),
        })
    }
}
/// Supported content types for HTTP requests
#[derive(Debug, Clone)]
pub enum RequestContentType {
    /// application/x-www-form-urlencoded
    FormUrlEncoded,
    /// multipart/form-data
    MultipartFormData,
    /// text/plain
    TextPlain,
}

impl FromStr for RequestContentType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "application/x-www-form-urlencoded" => RequestContentType::FormUrlEncoded,
            "multipart/form-data" => RequestContentType::MultipartFormData,
            "text/plain" => RequestContentType::TextPlain,
            _ => return Err(()),
        })
    }
}

impl Display for RequestContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestContentType::FormUrlEncoded => write!(f, "application/x-www-form-urlencoded"),
            RequestContentType::MultipartFormData => write!(f, "multipart/form-data"),
            RequestContentType::TextPlain => write!(f, "text/plain"),
        }
    }
}

/// Converts the entry list to a vector of name-value pairs with normalized line endings
/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#converting-an-entry-list-to-a-list-of-name-value-pairs
fn convert_to_list_of_name_value_pairs(form_data: FormData) -> Vec<(String, String)> {
    form_data
        .iter()
        .map(|Entry { name, value }| {
            let name = normalize_line_endings(name.as_ref());
            let value = normalize_line_endings(value.as_ref());
            (name, value)
        })
        .collect()
}

/// Normalizes line endings in a string according to HTML spec
/// Converts single CR or LF to CRLF pairs according to HTML form submission requirements
fn normalize_line_endings(input: &str) -> String {
    // Replace every occurrence of U+000D (CR) not followed by U+000A (LF),
    // and every occurrence of U+000A (LF) not preceded by U+000D (CR),
    // in value, by a string consisting of U+000D (CR) and U+000A (LF).

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(current) = chars.next() {
        match (current, chars.peek()) {
            ('\r', Some('\n')) => {
                result.push_str("\r\n");
                chars.next();
            }
            ('\r' | '\n', _) => {
                result.push_str("\r\n");
            }
            _ => result.push(current),
        }
    }

    result
}

/// Encodes form data as text/plain according to HTML spec given an slice of name-value pairs
/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#text/plain-encoding-algorithm
fn encode_text_plain<T: AsRef<str>, U: AsRef<str>>(input: &[(T, U)]) -> String {
    let mut out = String::new();
    for (name, value) in input {
        out.push_str(name.as_ref());
        out.push('=');
        out.push_str(value.as_ref());
        out.push_str("\r\n");
    }
    out
}
