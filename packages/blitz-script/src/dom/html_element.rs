//! HTMLElement reflection, rendered text and synthetic activation.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use blitz_dom::NodeId;
use blitz_traits::events::DomEvent;
use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Trace,
};
use keyboard_types::Modifiers;

use super::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_or_null, node_wrapper,
    this_node_id, to_rust_string,
};

#[derive(Clone, Copy)]
enum Reflection {
    String,
    Nullable,
    Boolean,
    Hidden,
    TabIndex,
    Direction,
    Draggable,
    Editable,
    Translate,
    Spellcheck,
}

fn read(id: NodeId, name: &str, context: &mut Context) -> JsResult<Option<String>> {
    let ctx = dom_ctx(context)?;
    let doc = ctx.doc.borrow();
    let element = doc
        .get_node(id)
        .and_then(|node| node.element_data())
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid HTMLElement receiver"))?;
    Ok(element
        .attrs()
        .iter()
        .find(|attribute| {
            attribute.name.ns == markup5ever::ns!() && attribute.name.local.as_ref() == name
        })
        .map(|attribute| attribute.value.to_string()))
}

fn inherited(id: NodeId, name: &str, context: &mut Context) -> JsResult<Option<String>> {
    let ctx = dom_ctx(context)?;
    let doc = ctx.doc.borrow();
    let mut current = Some(id);
    while let Some(id) = current {
        let Some(node) = doc.get_node(id) else { break };
        if let Some(element) = node.element_data()
            && let Some(attribute) = element.attrs().iter().find(|attribute| {
                attribute.name.ns == markup5ever::ns!() && attribute.name.local.as_ref() == name
            })
        {
            let value = attribute.value.to_string().to_ascii_lowercase();
            let valid = match name {
                "contenteditable" => {
                    matches!(value.as_str(), "" | "true" | "false" | "plaintext-only")
                }
                "translate" => matches!(value.as_str(), "" | "yes" | "no"),
                "spellcheck" => matches!(value.as_str(), "" | "true" | "false"),
                _ => false,
            };
            if valid {
                return Ok(Some(value));
            }
        }
        current = node.parent;
    }
    Ok(None)
}

fn html_integer(value: &str) -> Option<i32> {
    let value = value.trim_start_matches(['\t', '\n', '\u{000c}', '\r', ' ']);
    let offset = usize::from(value.starts_with(['+', '-']));
    let digits = value[offset..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    value[..offset + digits].parse().ok()
}

fn reflect_get(
    kind: Reflection,
    name: &'static str,
    this: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let value = read(id, name, context)?;
    let lower = value.as_deref().map(str::to_ascii_lowercase);
    Ok(match kind {
        Reflection::String => js_str(value.as_deref().unwrap_or_default()),
        Reflection::Nullable => value.as_deref().map(js_str).unwrap_or_else(JsValue::null),
        Reflection::Boolean => JsValue::from(value.is_some()),
        Reflection::Hidden => {
            if lower.as_deref() == Some("until-found") {
                js_str("until-found")
            } else {
                JsValue::from(value.is_some())
            }
        }
        Reflection::Direction => js_str(match lower.as_deref() {
            Some("ltr") => "ltr",
            Some("rtl") => "rtl",
            Some("auto") => "auto",
            _ => "",
        }),
        Reflection::Editable => js_str(match lower.as_deref() {
            Some("") | Some("true") => "true",
            Some("false") => "false",
            Some("plaintext-only") => "plaintext-only",
            _ => "inherit",
        }),
        Reflection::Translate => {
            JsValue::from(inherited(id, name, context)?.as_deref() != Some("no"))
        }
        Reflection::Spellcheck => {
            let inherited = inherited(id, name, context)?;
            let default = read(id, "type", context)?
                .is_none_or(|value| !value.eq_ignore_ascii_case("password"));
            JsValue::from(
                inherited
                    .as_deref()
                    .map_or(default, |value| value != "false"),
            )
        }
        Reflection::Draggable => {
            let ctx = dom_ctx(context)?;
            let doc = ctx.doc.borrow();
            let element = doc
                .get_node(id)
                .and_then(|node| node.element_data())
                .expect("validated element");
            let automatic = element.name.local == markup5ever::local_name!("img")
                || (element.name.local == markup5ever::local_name!("a")
                    && element.attr(markup5ever::local_name!("href")).is_some());
            JsValue::from(match lower.as_deref() {
                Some("true") => true,
                Some("false") => false,
                _ => automatic,
            })
        }
        Reflection::TabIndex => {
            let explicit = value.as_deref().and_then(html_integer);
            let ctx = dom_ctx(context)?;
            let doc = ctx.doc.borrow();
            let node = doc.get_node(id).expect("validated element");
            let element = node.element_data().expect("validated element");
            let default = match element.name.local.as_ref() {
                "a" | "area" => element.attr(markup5ever::local_name!("href")).is_some(),
                "button" | "input" | "select" | "textarea" | "iframe" | "frame" | "object" => true,
                "summary" => {
                    node.parent
                        .and_then(|parent| doc.get_node(parent))
                        .is_some_and(|parent| {
                            parent
                                .data
                                .is_element_with_tag_name(&markup5ever::local_name!("details"))
                                && parent.children.iter().copied().find(|id| {
                                    doc.get_node(*id).is_some_and(|child| {
                                        child.data.is_element_with_tag_name(
                                            &markup5ever::local_name!("summary"),
                                        )
                                    })
                                }) == Some(id)
                        })
                }
                _ => {
                    inherited(id, "contenteditable", context)?.is_some_and(|value| value != "false")
                }
            };
            JsValue::from(explicit.unwrap_or(if default { 0 } else { -1 }))
        }
    })
}

fn reflect_set(
    kind: Reflection,
    name: &'static str,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    read(id, name, context)?;
    let value = &args.first().cloned().unwrap_or_default();
    let value = match kind {
        Reflection::Nullable if value.is_null() => None,
        Reflection::Boolean => value.to_boolean().then(String::new),
        Reflection::Hidden => {
            if value
                .as_string()
                .is_some_and(|value| value.to_std_string_lossy() == "until-found")
            {
                Some("until-found".to_owned())
            } else {
                value.to_boolean().then(String::new)
            }
        }
        Reflection::TabIndex => Some(value.to_i32(context)?.to_string()),
        Reflection::Draggable | Reflection::Spellcheck => Some(value.to_boolean().to_string()),
        Reflection::Translate => Some(if value.to_boolean() { "yes" } else { "no" }.to_owned()),
        Reflection::Editable => {
            let value = to_rust_string(value, context)?.to_ascii_lowercase();
            if !matches!(
                value.as_str(),
                "inherit" | "true" | "false" | "plaintext-only"
            ) {
                return Err(super::interfaces::exception(
                    "SyntaxError",
                    "Invalid contentEditable value",
                    context,
                ));
            }
            if value == "inherit" {
                None
            } else {
                Some(value)
            }
        }
        _ => Some(to_rust_string(value, context)?),
    };
    let ctx = dom_ctx(context)?;
    let mut doc = ctx.mutate_doc();
    let mut mutation = doc.mutate();
    match value {
        Some(value) => mutation.set_attribute(id, super::element::attr_name(name), &value),
        None => mutation.clear_attribute(id, super::element::attr_name(name)),
    }
    Ok(JsValue::undefined())
}

fn reflect(
    proto: &JsObject,
    property: &'static str,
    attribute: &'static str,
    kind: Reflection,
    context: &mut Context,
) {
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |this, _, context| {
            reflect_get(kind, attribute, this, context)
        }),
    )
    .name(JsString::from(format!("get {property}")))
    .length(0)
    .build();
    let setter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |this, args, context| {
            reflect_set(kind, attribute, this, args, context)
        }),
    )
    .name(JsString::from(format!("set {property}")))
    .length(1)
    .build();
    proto
        .define_property_or_throw(
            JsString::from(property),
            PropertyDescriptor::builder()
                .get(getter)
                .set(setter)
                .enumerable(true)
                .configurable(true),
            context,
        )
        .expect("failed to define reflected attribute");
}

fn is_content_editable(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    read(id, "contenteditable", context)?;
    Ok(JsValue::from(
        inherited(id, "contenteditable", context)?.is_some_and(|value| value != "false"),
    ))
}

fn inner_text(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let doc = ctx.doc.borrow();
    let node = doc
        .get_node(id)
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid HTMLElement receiver"))?;
    Ok(js_str(&node.rendered_inner_text()))
}

fn text_nodes(args: &[JsValue], context: &mut Context) -> JsResult<Vec<NodeId>> {
    let value = &args.first().cloned().unwrap_or_default();
    let text = if value.is_null() {
        String::new()
    } else {
        to_rust_string(value, context)?
    };
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let ctx = dom_ctx(context)?;
    let mut doc = ctx.doc.borrow_mut();
    let mut mutation = doc.mutate();
    let mut nodes = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index != 0 {
            nodes.push(mutation.create_element(super::qual_name("br"), Vec::new()));
        }
        if !line.is_empty() {
            nodes.push(mutation.create_text_node(line));
        }
    }
    Ok(nodes)
}

fn set_inner_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let nodes = text_nodes(args, context)?;
    let ctx = dom_ctx(context)?;
    let children = ctx
        .doc
        .borrow()
        .get_node(id)
        .map(|node| node.children.clone())
        .unwrap_or_default();
    for child in children {
        super::remove_and_free_node(&ctx, child, context);
    }
    ctx.mutate_doc().mutate().append_children(id, &nodes);
    for node in nodes {
        super::mark_node_reattached(&ctx, node);
    }
    Ok(JsValue::undefined())
}

fn set_outer_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    if ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.parent)
        .is_none()
    {
        return Err(super::interfaces::exception(
            "NoModificationAllowedError",
            "outerText requires a parent node",
            context,
        ));
    }
    let nodes = text_nodes(args, context)?;
    ctx.mutate_doc().mutate().insert_nodes_before(id, &nodes);
    for node in nodes {
        super::mark_node_reattached(&ctx, node);
    }
    super::remove_and_free_node(&ctx, id, context);
    Ok(JsValue::undefined())
}

fn offset_parent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let parent = ctx
        .doc
        .borrow()
        .get_node(id)
        .and_then(|node| node.cssom_offset_parent())
        .map(|node| node.id);
    Ok(node_or_null(&ctx, parent, context))
}

#[derive(Clone, Trace, Finalize, JsData)]
struct Clicks {
    #[unsafe_ignore_trace]
    active: Rc<RefCell<HashSet<NodeId>>>,
}

struct ClickGuard {
    id: NodeId,
    active: Rc<RefCell<HashSet<NodeId>>>,
}

impl Drop for ClickGuard {
    fn drop(&mut self) {
        self.active.borrow_mut().remove(&self.id);
    }
}

fn dispatch_native(event: &mut DomEvent, context: &mut Context) -> JsResult<bool> {
    let ctx = dom_ctx(context)?;
    let target: JsValue = node_wrapper(&ctx, event.target, context).into();
    let object = super::event::create_event_for_dom_event(
        &ctx,
        &event.data,
        event.bubbles,
        event.cancelable,
        &target,
        context,
    );
    define_value(&object, "isTrusted", JsValue::from(false), context);
    if event.name() == "click" {
        define_value(&object, "detail", JsValue::from(0), context);
    }
    let dispatch = ctx
        .state
        .borrow()
        .protos()
        .node
        .clone()
        .get(boa_engine::js_string!("dispatchEvent"), context)?
        .as_object()
        .expect("missing dispatchEvent");
    Ok(dispatch
        .call(&target, &[object.into()], context)?
        .to_boolean())
}

fn click(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    {
        let doc = ctx.doc.borrow();
        let element = doc
            .get_node(id)
            .and_then(|node| node.element_data())
            .ok_or_else(|| JsNativeError::typ().with_message("Invalid HTMLElement receiver"))?;
        if matches!(
            element.name.local.as_ref(),
            "button" | "input" | "select" | "textarea" | "option"
        ) && element.attr(markup5ever::local_name!("disabled")).is_some()
        {
            return Ok(JsValue::undefined());
        }
        let mut parent = doc.get_node(id).and_then(|node| node.parent);
        while let Some(parent_id) = parent {
            let node = doc.get_node(parent_id).expect("missing ancestor");
            if matches!(
                element.name.local.as_ref(),
                "button" | "input" | "select" | "textarea"
            ) && node
                .data
                .is_element_with_tag_name(&markup5ever::local_name!("fieldset"))
                && node.attr(markup5ever::local_name!("disabled")).is_some()
            {
                let legend = node.children.iter().copied().find(|id| {
                    doc.get_node(*id).is_some_and(|node| {
                        node.data
                            .is_element_with_tag_name(&markup5ever::local_name!("legend"))
                    })
                });
                let in_legend = legend.is_some_and(|legend| {
                    let mut current = Some(id);
                    while let Some(current_id) = current {
                        if current_id == legend {
                            return true;
                        }
                        if current_id == parent_id {
                            break;
                        }
                        current = doc.get_node(current_id).and_then(|node| node.parent);
                    }
                    false
                });
                if !in_legend {
                    return Ok(JsValue::undefined());
                }
            }
            parent = node.parent;
        }
    }
    let active = context
        .get_data::<Clicks>()
        .expect("missing click state")
        .active
        .clone();
    if !active.borrow_mut().insert(id) {
        return Ok(JsValue::undefined());
    }
    let _guard = ClickGuard { id, active };
    ctx.flush_layout();
    let data = ctx
        .doc
        .borrow()
        .get_node(id)
        .expect("validated element")
        .synthetic_click_event(Modifiers::empty());
    let mut queue = VecDeque::from([DomEvent::new(id, data)]);
    while let Some(mut event) = queue.pop_front() {
        let activation = if event.name() == "click" {
            ctx.mutate_doc().run_pre_click_activation(event.target)
        } else {
            None
        };
        match dispatch_native(&mut event, context) {
            Ok(true) => {
                ctx.mutate_doc()
                    .handle_dom_event(&mut event, |event| queue.push_back(event));
            }
            Ok(false) => {
                if let Some(activation) = activation {
                    ctx.mutate_doc().undo_pre_click_activation(activation);
                }
            }
            Err(error) => {
                if let Some(activation) = activation {
                    ctx.mutate_doc().undo_pre_click_activation(activation);
                }
                return Err(error);
            }
        }
    }
    Ok(JsValue::undefined())
}

pub(super) fn init(element: &JsObject, context: &mut Context) {
    let html = super::interfaces::prototype("HTMLElement", context);
    for (property, attribute, kind) in [
        ("hidden", "hidden", Reflection::Hidden),
        ("tabIndex", "tabindex", Reflection::TabIndex),
        ("title", "title", Reflection::String),
        ("lang", "lang", Reflection::String),
        ("dir", "dir", Reflection::Direction),
        ("draggable", "draggable", Reflection::Draggable),
        ("contentEditable", "contenteditable", Reflection::Editable),
        ("accessKey", "accesskey", Reflection::String),
        ("translate", "translate", Reflection::Translate),
        ("spellcheck", "spellcheck", Reflection::Spellcheck),
        ("inert", "inert", Reflection::Boolean),
    ] {
        reflect(&html, property, attribute, kind, context);
    }
    reflect(element, "role", "role", Reflection::Nullable, context);
    for (property, attribute) in [
        ("ariaActiveDescendant", "aria-activedescendant"),
        ("ariaAtomic", "aria-atomic"),
        ("ariaAutoComplete", "aria-autocomplete"),
        ("ariaBrailleLabel", "aria-braillelabel"),
        ("ariaBrailleRoleDescription", "aria-brailleroledescription"),
        ("ariaBusy", "aria-busy"),
        ("ariaChecked", "aria-checked"),
        ("ariaColCount", "aria-colcount"),
        ("ariaColIndex", "aria-colindex"),
        ("ariaColIndexText", "aria-colindextext"),
        ("ariaColSpan", "aria-colspan"),
        ("ariaControls", "aria-controls"),
        ("ariaCurrent", "aria-current"),
        ("ariaDescribedBy", "aria-describedby"),
        ("ariaDescription", "aria-description"),
        ("ariaDetails", "aria-details"),
        ("ariaDisabled", "aria-disabled"),
        ("ariaErrorMessage", "aria-errormessage"),
        ("ariaExpanded", "aria-expanded"),
        ("ariaFlowTo", "aria-flowto"),
        ("ariaHasPopup", "aria-haspopup"),
        ("ariaHidden", "aria-hidden"),
        ("ariaInvalid", "aria-invalid"),
        ("ariaKeyShortcuts", "aria-keyshortcuts"),
        ("ariaLabel", "aria-label"),
        ("ariaLabelledBy", "aria-labelledby"),
        ("ariaLevel", "aria-level"),
        ("ariaLive", "aria-live"),
        ("ariaModal", "aria-modal"),
        ("ariaMultiLine", "aria-multiline"),
        ("ariaMultiSelectable", "aria-multiselectable"),
        ("ariaOrientation", "aria-orientation"),
        ("ariaOwns", "aria-owns"),
        ("ariaPlaceholder", "aria-placeholder"),
        ("ariaPosInSet", "aria-posinset"),
        ("ariaPressed", "aria-pressed"),
        ("ariaReadOnly", "aria-readonly"),
        ("ariaRelevant", "aria-relevant"),
        ("ariaRequired", "aria-required"),
        ("ariaRoleDescription", "aria-roledescription"),
        ("ariaRowCount", "aria-rowcount"),
        ("ariaRowIndex", "aria-rowindex"),
        ("ariaRowIndexText", "aria-rowindextext"),
        ("ariaRowSpan", "aria-rowspan"),
        ("ariaSelected", "aria-selected"),
        ("ariaSetSize", "aria-setsize"),
        ("ariaSort", "aria-sort"),
        ("ariaValueMax", "aria-valuemax"),
        ("ariaValueMin", "aria-valuemin"),
        ("ariaValueNow", "aria-valuenow"),
        ("ariaValueText", "aria-valuetext"),
    ] {
        reflect(element, property, attribute, Reflection::Nullable, context);
    }
    define_accessor(
        &html,
        "isContentEditable",
        Some(is_content_editable),
        None,
        context,
    );
    define_accessor(
        &html,
        "innerText",
        Some(inner_text),
        Some(set_inner_text),
        context,
    );
    define_accessor(
        &html,
        "outerText",
        Some(inner_text),
        Some(set_outer_text),
        context,
    );
    define_accessor(&html, "offsetParent", Some(offset_parent), None, context);
    context.insert_data(Clicks {
        active: Rc::new(RefCell::new(HashSet::new())),
    });
    define_method(&html, "click", 0, click, context);
}
