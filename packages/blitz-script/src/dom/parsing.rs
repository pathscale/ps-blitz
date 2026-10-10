//! Native inert document parsing and XML serialization.

use blitz_dom::node::{MarkupNode, NodeData};
use blitz_dom::{BaseDocument, NodeId};
use blitz_html::DocumentHtmlParser;
use boa_engine::object::JsObject;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsValue, NativeFunction, Trace,
};

use super::{
    define_accessor, define_method, dom_ctx, interfaces, js_str, node_id_of_value, node_or_null,
    node_wrapper, this_node_id, to_rust_string,
};
use crate::state::DomCtx;

const HTML: &str = "http://www.w3.org/1999/xhtml";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

#[derive(Trace, Finalize, JsData)]
struct Parser;

#[derive(Trace, Finalize, JsData)]
struct Serializer;

pub(super) fn install(ctx: &DomCtx, context: &mut Context) {
    for (name, parent) in [
        ("XMLDocument", "Document"),
        ("DocumentType", "Node"),
        ("ProcessingInstruction", "CharacterData"),
    ] {
        interfaces::register(
            name,
            Some(parent),
            JsObject::with_object_proto(context.intrinsics()),
            0,
            NativeFunction::from_fn_ptr(illegal_constructor),
            context,
        );
    }
    let doctype = interfaces::prototype("DocumentType", context);
    define_accessor(&doctype, "name", Some(doctype_name), None, context);
    define_accessor(&doctype, "publicId", Some(public_id), None, context);
    define_accessor(&doctype, "systemId", Some(system_id), None, context);
    let pi = interfaces::prototype("ProcessingInstruction", context);
    define_accessor(&pi, "target", Some(pi_target), None, context);

    let parser = JsObject::with_object_proto(context.intrinsics());
    define_method(&parser, "parseFromString", 2, parse_from_string, context);
    interfaces::register(
        "DOMParser",
        None,
        parser,
        0,
        NativeFunction::from_fn_ptr(parser_constructor),
        context,
    );
    let serializer = JsObject::with_object_proto(context.intrinsics());
    define_method(
        &serializer,
        "serializeToString",
        1,
        serialize_to_string,
        context,
    );
    interfaces::register(
        "XMLSerializer",
        None,
        serializer,
        0,
        NativeFunction::from_fn_ptr(serializer_constructor),
        context,
    );

    let document = ctx.state.borrow().protos().document.clone();
    define_accessor(&document, "contentType", Some(content_type), None, context);
    define_accessor(&document, "doctype", Some(document_doctype), None, context);
}

fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("Illegal constructor")
        .into())
}

fn parser_constructor(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let prototype = interfaces::construction_prototype(new_target, "DOMParser", context)?;
    Ok(JsObject::from_proto_and_data(Some(prototype), Parser).into())
}

fn serializer_constructor(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let prototype = interfaces::construction_prototype(new_target, "XMLSerializer", context)?;
    Ok(JsObject::from_proto_and_data(Some(prototype), Serializer).into())
}

fn parse_from_string(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if this.as_object().is_none_or(|object| !object.is::<Parser>()) {
        return Err(JsNativeError::typ()
            .with_message("Invalid DOMParser receiver")
            .into());
    }
    if args.len() < 2 {
        return Err(JsNativeError::typ()
            .with_message("parseFromString requires two arguments")
            .into());
    }
    let source = to_rust_string(&args[0], context)?;
    let requested_type = to_rust_string(&args[1], context)?;
    let content_type = match requested_type.as_str() {
        "text/html" => "text/html",
        "application/xml" => "application/xml",
        "text/xml" => "text/xml",
        "application/xhtml+xml" => "application/xhtml+xml",
        "image/svg+xml" => "image/svg+xml",
        _ => {
            return Err(JsNativeError::typ()
                .with_message("Unsupported DOMParser MIME type")
                .into());
        }
    };
    let ctx = dom_ctx(context)?;
    let document_id = {
        let mut doc = ctx.doc.borrow_mut();
        let mut mutr = doc.mutate();
        let document_id = mutr.create_document(content_type);
        let errors = DocumentHtmlParser::parse_inert_into_mutator(
            &mut mutr,
            document_id,
            &source,
            content_type != "text/html",
        );
        let root_count = mutr
            .child_ids(document_id)
            .iter()
            .filter(|id| {
                mutr.doc
                    .get_node(**id)
                    .is_some_and(|node| node.is_element())
            })
            .count();
        if content_type != "text/html" && (!errors.is_empty() || root_count != 1) {
            let message = if errors.is_empty() {
                "XML must contain exactly one document element".to_owned()
            } else {
                errors
                    .iter()
                    .map(|error| error.as_ref())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            mutr.remove_and_drop_all_children(document_id);
            let error = mutr.create_element(
                blitz_dom::QualName::new(
                    None,
                    blitz_dom::Namespace::from(
                        "http://www.mozilla.org/newlayout/xml/parsererror.xml",
                    ),
                    blitz_dom::LocalName::from("parsererror"),
                ),
                Vec::new(),
            );
            mutr.adopt_node(error, document_id);
            let text = mutr.create_text_node(&message);
            mutr.adopt_node(text, document_id);
            mutr.append_children(error, &[text]);
            mutr.append_children(document_id, &[error]);
        }
        document_id
    };
    ctx.state.borrow_mut().detached_nodes.push(document_id);
    Ok(node_wrapper(&ctx, document_id, context).into())
}

fn content_type(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    match doc.get_node(id).map(|node| &node.data) {
        Some(NodeData::Document(data)) => Ok(js_str(data.content_type)),
        _ => Err(JsNativeError::typ()
            .with_message("Invalid Document receiver")
            .into()),
    }
}

fn document_doctype(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let found = {
        let doc = ctx.doc.borrow();
        doc.get_node(id).and_then(|node| {
            node.children.iter().copied().find(|child| {
                matches!(
                    doc.get_node(*child).and_then(|node| node.markup.as_deref()),
                    Some(MarkupNode::Doctype { .. })
                )
            })
        })
    };
    Ok(node_or_null(&ctx, found, context))
}

fn doctype_field(this: &JsValue, context: &mut Context, field: usize) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    match doc.get_node(id).and_then(|node| node.markup.as_deref()) {
        Some(MarkupNode::Doctype {
            name,
            public_id,
            system_id,
        }) => Ok(js_str(match field {
            0 => name,
            1 => public_id,
            _ => system_id,
        })),
        _ => Err(JsNativeError::typ()
            .with_message("Invalid DocumentType receiver")
            .into()),
    }
}

fn doctype_name(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    doctype_field(this, context, 0)
}

fn public_id(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    doctype_field(this, context, 1)
}

fn system_id(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    doctype_field(this, context, 2)
}

fn pi_target(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let id = this_node_id(this)?;
    let doc = ctx.doc.borrow();
    match doc.get_node(id).and_then(|node| node.markup.as_deref()) {
        Some(MarkupNode::ProcessingInstruction { target }) => Ok(js_str(target)),
        _ => Err(JsNativeError::typ()
            .with_message("Invalid ProcessingInstruction receiver")
            .into()),
    }
}

fn serialize_to_string(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if this
        .as_object()
        .is_none_or(|object| !object.is::<Serializer>())
    {
        return Err(JsNativeError::typ()
            .with_message("Invalid XMLSerializer receiver")
            .into());
    }
    let id = args.first().and_then(node_id_of_value).ok_or_else(|| {
        JsNativeError::typ().with_message("serializeToString requires a DOM node")
    })?;
    let ctx = dom_ctx(context)?;
    let output = serialize(&ctx.doc.borrow(), id)
        .map_err(|message| super::doma::dom_error("InvalidStateError", message, context))?;
    Ok(js_str(&output))
}

fn escaped(output: &mut String, value: &str, attribute: bool) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' if attribute => output.push_str("&quot;"),
            '\t' if attribute => output.push_str("&#9;"),
            '\n' if attribute => output.push_str("&#10;"),
            '\r' => output.push_str("&#13;"),
            other => output.push(other),
        }
    }
}

fn binding<'a>(bindings: &'a [(String, String)], prefix: &str) -> Option<&'a str> {
    bindings
        .iter()
        .rev()
        .find_map(|(key, value)| (key == prefix).then_some(value.as_str()))
}

fn declare(
    bindings: &mut Vec<(String, String)>,
    declarations: &mut Vec<(String, String)>,
    prefix: &str,
    uri: &str,
) {
    if binding(bindings, prefix) == Some(uri) {
        return;
    }
    bindings.push((prefix.to_owned(), uri.to_owned()));
    if let Some((_, value)) = declarations.iter_mut().find(|(key, _)| key == prefix) {
        *value = uri.to_owned();
    } else {
        declarations.push((prefix.to_owned(), uri.to_owned()));
    }
}

fn attribute_prefix(
    bindings: &mut Vec<(String, String)>,
    declarations: &mut Vec<(String, String)>,
    requested: Option<&str>,
    uri: &str,
    counter: &mut usize,
) -> String {
    if uri == XML {
        return "xml".to_owned();
    }
    if let Some(prefix) = requested.filter(|prefix| !prefix.is_empty() && *prefix != "xmlns") {
        if binding(bindings, prefix).is_none_or(|bound| bound == uri) {
            declare(bindings, declarations, prefix, uri);
            return prefix.to_owned();
        }
    }
    for (prefix, value) in bindings.iter().rev() {
        if !prefix.is_empty() && value == uri && binding(bindings, prefix) == Some(uri) {
            return prefix.clone();
        }
    }
    loop {
        *counter += 1;
        let prefix = format!("ns{counter}");
        if binding(bindings, &prefix).is_none() {
            declare(bindings, declarations, &prefix, uri);
            return prefix;
        }
    }
}

fn quoted_identifier(output: &mut String, value: &str) -> Result<(), &'static str> {
    let quote = if !value.contains('"') { '"' } else { '\'' };
    if value.contains(quote) {
        return Err("Doctype identifier contains both quote characters");
    }
    output.push(quote);
    output.push_str(value);
    output.push(quote);
    Ok(())
}

enum SerializationStep {
    Node(NodeId),
    End(String, usize),
}

/// Namespace bindings are scoped by stack length, without cloning a map for
/// each descendant. The traversal does not create JavaScript wrappers.
fn serialize(doc: &BaseDocument, root: NodeId) -> Result<String, &'static str> {
    let mut output = String::new();
    let mut bindings = vec![
        (String::new(), String::new()),
        ("xml".to_owned(), XML.to_owned()),
    ];
    let mut counter = 0;
    let mut stack = vec![SerializationStep::Node(root)];
    while let Some(step) = stack.pop() {
        let id = match step {
            SerializationStep::End(name, scope) => {
                output.push_str("</");
                output.push_str(&name);
                output.push('>');
                bindings.truncate(scope);
                continue;
            }
            SerializationStep::Node(id) => id,
        };
        let node = doc.get_node(id).ok_or("Node no longer exists")?;
        if let Some(markup) = node.markup.as_deref() {
            match markup {
                MarkupNode::ProcessingInstruction { target } => {
                    let NodeData::Comment { contents } = &node.data else {
                        return Err("Invalid processing instruction storage");
                    };
                    if contents.contains("?>") {
                        return Err("Processing instruction contains ?>");
                    }
                    output.push_str("<?");
                    output.push_str(target);
                    if !contents.is_empty() {
                        output.push(' ');
                        output.push_str(contents);
                    }
                    output.push_str("?>");
                }
                MarkupNode::Doctype {
                    name,
                    public_id,
                    system_id,
                } => {
                    output.push_str("<!DOCTYPE ");
                    output.push_str(name);
                    if !public_id.is_empty() {
                        output.push_str(" PUBLIC ");
                        quoted_identifier(&mut output, public_id)?;
                        output.push(' ');
                        quoted_identifier(&mut output, system_id)?;
                    } else if !system_id.is_empty() {
                        output.push_str(" SYSTEM ");
                        quoted_identifier(&mut output, system_id)?;
                    }
                    output.push('>');
                }
            }
            continue;
        }
        match &node.data {
            NodeData::Text(text) => escaped(&mut output, &text.content, false),
            NodeData::Comment { contents } => {
                if contents.contains("--") || contents.ends_with('-') {
                    return Err("Comment cannot be represented in XML");
                }
                output.push_str("<!--");
                output.push_str(contents);
                output.push_str("-->");
            }
            NodeData::Document(_) | NodeData::DocumentFragment | NodeData::ShadowRoot(_) => {
                stack.extend(
                    node.children
                        .iter()
                        .rev()
                        .copied()
                        .map(SerializationStep::Node),
                );
            }
            NodeData::AnonymousBlock(_) => return Err("Cannot serialize a layout-only node"),
            NodeData::Element(element) => {
                let scope = bindings.len();
                let mut declarations = Vec::new();
                for attribute in element
                    .attrs()
                    .iter()
                    .filter(|attribute| attribute.name.ns.as_ref() == XMLNS)
                {
                    let prefix = if attribute.name.prefix.is_none()
                        && attribute.name.local.as_ref() == "xmlns"
                    {
                        ""
                    } else {
                        attribute.name.local.as_ref()
                    };
                    declare(&mut bindings, &mut declarations, prefix, &attribute.value);
                }

                let uri = element.name.ns.as_ref();
                let prefix = element
                    .name
                    .prefix
                    .as_ref()
                    .map(|prefix| prefix.as_ref())
                    .unwrap_or("");
                let prefix = if uri.is_empty() {
                    declare(&mut bindings, &mut declarations, "", "");
                    String::new()
                } else if uri == XML {
                    "xml".to_owned()
                } else if prefix.is_empty() {
                    declare(&mut bindings, &mut declarations, "", uri);
                    String::new()
                } else {
                    attribute_prefix(
                        &mut bindings,
                        &mut declarations,
                        Some(prefix),
                        uri,
                        &mut counter,
                    )
                };
                let name = if prefix.is_empty() {
                    element.name.local.to_string()
                } else {
                    format!("{prefix}:{}", element.name.local)
                };
                output.push('<');
                output.push_str(&name);
                for attribute in element
                    .attrs()
                    .iter()
                    .filter(|attribute| attribute.name.ns.as_ref() != XMLNS)
                {
                    output.push(' ');
                    let uri = attribute.name.ns.as_ref();
                    if !uri.is_empty() {
                        let prefix = attribute_prefix(
                            &mut bindings,
                            &mut declarations,
                            attribute.name.prefix.as_ref().map(|prefix| prefix.as_ref()),
                            uri,
                            &mut counter,
                        );
                        output.push_str(&prefix);
                        output.push(':');
                    }
                    output.push_str(&attribute.name.local);
                    output.push_str("=\"");
                    escaped(&mut output, &attribute.value, true);
                    output.push('"');
                }
                for (prefix, uri) in declarations {
                    output.push_str(" xmlns");
                    if !prefix.is_empty() {
                        output.push(':');
                        output.push_str(&prefix);
                    }
                    output.push_str("=\"");
                    escaped(&mut output, &uri, true);
                    output.push('"');
                }

                let children = element
                    .template_contents
                    .and_then(|contents| doc.get_node(contents))
                    .map(|contents| &contents.children)
                    .unwrap_or(&node.children);
                let html_void = uri == HTML
                    && matches!(
                        element.name.local.as_ref(),
                        "area"
                            | "base"
                            | "br"
                            | "col"
                            | "embed"
                            | "hr"
                            | "img"
                            | "input"
                            | "link"
                            | "meta"
                            | "param"
                            | "source"
                            | "track"
                            | "wbr"
                    );
                if children.is_empty() && (uri != HTML || html_void) {
                    output.push_str(if html_void { " />" } else { "/>" });
                    bindings.truncate(scope);
                } else {
                    output.push('>');
                    stack.push(SerializationStep::End(name, scope));
                    stack.extend(children.iter().rev().copied().map(SerializationStep::Node));
                }
            }
        }
    }
    Ok(output)
}
