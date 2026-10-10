//! Realm-owned DOM interface objects and wrapper prototype selection.

use std::collections::HashMap;

use blitz_dom::NodeData;
use boa_engine::object::{FunctionObjectBuilder, JsObject};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace,
};
use boa_gc::GcRefCell;

use super::{define_value, js_str};
use crate::state::DomProtos;

#[derive(Trace, Finalize, JsData)]
struct Interfaces {
    prototypes: GcRefCell<HashMap<String, JsObject>>,
}

/// SITESA can install its HTML-constructor algorithm here. The default HTML
/// constructor rejects direct construction, including construction of an
/// unregistered subclass.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct HtmlConstructorHook {
    #[unsafe_ignore_trace]
    pub construct: fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
}

pub(super) fn prototype(name: &str, context: &Context) -> JsObject {
    context
        .get_data::<Interfaces>()
        .expect("DOM interfaces not initialised")
        .prototypes
        .borrow()
        .get(name)
        .unwrap_or_else(|| panic!("missing DOM interface {name}"))
        .clone()
}

pub(super) fn constructor(name: &str, context: &mut Context) -> JsObject {
    prototype(name, context)
        .get(boa_engine::js_string!("constructor"), context)
        .expect("missing DOM constructor")
        .as_object()
        .expect("DOM constructor is not an object")
}

pub(super) fn register(
    name: &'static str,
    parent: Option<&'static str>,
    proto: JsObject,
    length: usize,
    body: NativeFunction,
    context: &mut Context,
) {
    let constructor = FunctionObjectBuilder::new(context.realm(), body)
        .name(JsString::from(name))
        .length(length)
        .constructor(true)
        .build();
    constructor
        .define_property_or_throw(
            boa_engine::js_string!("prototype"),
            PropertyDescriptor::builder()
                .value(proto.clone())
                .writable(false)
                .enumerable(false)
                .configurable(false),
            context,
        )
        .expect("failed to define interface prototype");
    if let Some(parent) = parent {
        constructor.set_prototype(Some(self::constructor(parent, context)));
        proto.set_prototype(Some(prototype(parent, context)));
    }
    define_value(&proto, "constructor", constructor.clone().into(), context);
    proto
        .define_property_or_throw(
            JsSymbol::to_string_tag(),
            PropertyDescriptor::builder()
                .value(js_str(name))
                .writable(false)
                .enumerable(false)
                .configurable(true),
            context,
        )
        .expect("failed to define DOM toStringTag");
    let global = context.global_object().clone();
    define_value(&global, name, constructor.into(), context);
    context
        .get_data::<Interfaces>()
        .expect("DOM interfaces not initialised")
        .prototypes
        .borrow_mut()
        .insert(name.to_owned(), proto);
}

pub(super) fn illegal(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::typ()
        .with_message("Illegal constructor")
        .into())
}

fn html_constructor(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let hook = context
        .get_data::<HtmlConstructorHook>()
        .map(|hook| hook.construct);
    match hook {
        Some(construct) => construct(new_target, args, context),
        None => illegal(new_target, args, context),
    }
}

pub(super) fn construction_prototype(
    new_target: &JsValue,
    fallback: &str,
    context: &mut Context,
) -> JsResult<JsObject> {
    let constructor = new_target
        .as_object()
        .filter(|object| object.is_constructor())
        .ok_or_else(|| JsNativeError::typ().with_message("Constructor requires 'new'"))?;
    Ok(constructor
        .get(boa_engine::js_string!("prototype"), context)?
        .as_object()
        .unwrap_or_else(|| prototype(fallback, context)))
}

pub(super) fn exception(name: &str, message: &str, context: &mut Context) -> JsError {
    crate::dom_exception::error(name, message, context)
}

fn node_constructor(
    name: &'static str,
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = construction_prototype(new_target, name, context)?;
    let ctx = super::dom_ctx(context)?;
    let contents = match args.first() {
        None => String::new(),
        Some(value) if value.is_undefined() => String::new(),
        Some(value) => super::to_rust_string(value, context)?,
    };
    let id = {
        let mut doc = ctx.doc.borrow_mut();
        let mut mutation = doc.mutate();
        match name {
            "Text" => mutation.create_text_node(&contents),
            "Comment" => mutation.create_comment_node(&contents),
            "DocumentFragment" => mutation.create_document_fragment(),
            _ => unreachable!(),
        }
    };
    let wrapper = super::node_wrapper(&ctx, id, context);
    wrapper.set_prototype(Some(proto));
    Ok(wrapper.into())
}

const HTML_INTERFACES: &[(&str, &str)] = &[
    ("a", "HTMLAnchorElement"),
    ("area", "HTMLAreaElement"),
    ("audio", "HTMLAudioElement"),
    ("base", "HTMLBaseElement"),
    ("body", "HTMLBodyElement"),
    ("br", "HTMLBRElement"),
    ("button", "HTMLButtonElement"),
    ("canvas", "HTMLCanvasElement"),
    ("caption", "HTMLTableCaptionElement"),
    ("col", "HTMLTableColElement"),
    ("colgroup", "HTMLTableColElement"),
    ("data", "HTMLDataElement"),
    ("datalist", "HTMLDataListElement"),
    ("del", "HTMLModElement"),
    ("details", "HTMLDetailsElement"),
    ("dialog", "HTMLDialogElement"),
    ("dir", "HTMLDirectoryElement"),
    ("div", "HTMLDivElement"),
    ("dl", "HTMLDListElement"),
    ("embed", "HTMLEmbedElement"),
    ("fieldset", "HTMLFieldSetElement"),
    ("font", "HTMLFontElement"),
    ("form", "HTMLFormElement"),
    ("frame", "HTMLFrameElement"),
    ("frameset", "HTMLFrameSetElement"),
    ("h1", "HTMLHeadingElement"),
    ("h2", "HTMLHeadingElement"),
    ("h3", "HTMLHeadingElement"),
    ("h4", "HTMLHeadingElement"),
    ("h5", "HTMLHeadingElement"),
    ("h6", "HTMLHeadingElement"),
    ("head", "HTMLHeadElement"),
    ("hr", "HTMLHRElement"),
    ("html", "HTMLHtmlElement"),
    ("iframe", "HTMLIFrameElement"),
    ("img", "HTMLImageElement"),
    ("input", "HTMLInputElement"),
    ("ins", "HTMLModElement"),
    ("label", "HTMLLabelElement"),
    ("legend", "HTMLLegendElement"),
    ("li", "HTMLLIElement"),
    ("link", "HTMLLinkElement"),
    ("listing", "HTMLPreElement"),
    ("map", "HTMLMapElement"),
    ("marquee", "HTMLMarqueeElement"),
    ("menu", "HTMLMenuElement"),
    ("meta", "HTMLMetaElement"),
    ("meter", "HTMLMeterElement"),
    ("object", "HTMLObjectElement"),
    ("ol", "HTMLOListElement"),
    ("optgroup", "HTMLOptGroupElement"),
    ("option", "HTMLOptionElement"),
    ("output", "HTMLOutputElement"),
    ("p", "HTMLParagraphElement"),
    ("param", "HTMLParamElement"),
    ("picture", "HTMLPictureElement"),
    ("pre", "HTMLPreElement"),
    ("progress", "HTMLProgressElement"),
    ("q", "HTMLQuoteElement"),
    ("blockquote", "HTMLQuoteElement"),
    ("script", "HTMLScriptElement"),
    ("select", "HTMLSelectElement"),
    ("selectedcontent", "HTMLSelectedContentElement"),
    ("slot", "HTMLSlotElement"),
    ("source", "HTMLSourceElement"),
    ("span", "HTMLSpanElement"),
    ("style", "HTMLStyleElement"),
    ("table", "HTMLTableElement"),
    ("tbody", "HTMLTableSectionElement"),
    ("td", "HTMLTableCellElement"),
    ("template", "HTMLTemplateElement"),
    ("textarea", "HTMLTextAreaElement"),
    ("tfoot", "HTMLTableSectionElement"),
    ("th", "HTMLTableCellElement"),
    ("thead", "HTMLTableSectionElement"),
    ("time", "HTMLTimeElement"),
    ("title", "HTMLTitleElement"),
    ("tr", "HTMLTableRowElement"),
    ("track", "HTMLTrackElement"),
    ("ul", "HTMLUListElement"),
    ("video", "HTMLVideoElement"),
    ("xmp", "HTMLPreElement"),
];

const GENERIC_HTML_TAGS: &[&str] = &[
    "abbr",
    "acronym",
    "address",
    "article",
    "aside",
    "b",
    "basefont",
    "bdi",
    "bdo",
    "big",
    "center",
    "cite",
    "code",
    "dd",
    "dfn",
    "dt",
    "em",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "i",
    "kbd",
    "main",
    "mark",
    "nav",
    "nobr",
    "noembed",
    "noframes",
    "noscript",
    "plaintext",
    "rb",
    "rp",
    "rt",
    "rtc",
    "ruby",
    "s",
    "samp",
    "search",
    "section",
    "small",
    "strike",
    "strong",
    "sub",
    "summary",
    "sup",
    "tt",
    "u",
    "var",
    "wbr",
];

fn autonomous_custom_name(name: &str) -> bool {
    name.contains('-')
        && name.starts_with(|character: char| character.is_ascii_lowercase())
        && !matches!(
            name,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '.' | '_')
                || matches!(
                    character as u32,
                    0x00b7
                        | 0x00c0..=0x00d6
                        | 0x00d8..=0x00f6
                        | 0x00f8..=0x037d
                        | 0x037f..=0x1fff
                        | 0x200c..=0x200d
                        | 0x203f..=0x2040
                        | 0x2070..=0x218f
                        | 0x2c00..=0x2fef
                        | 0x3001..=0xd7ff
                        | 0xf900..=0xfdcf
                        | 0xfdf0..=0xfffd
                        | 0x10000..=0xeffff
                )
        })
}

pub(super) fn node_prototype(
    data: Option<&NodeData>,
    fallback: &DomProtos,
    context: &Context,
) -> JsObject {
    let name = match data {
        Some(NodeData::Document(_)) => "HTMLDocument",
        Some(NodeData::Text(_)) => "Text",
        Some(NodeData::Comment { .. }) => "Comment",
        Some(NodeData::DocumentFragment) => "DocumentFragment",
        Some(NodeData::Element(element)) => {
            let local = element.name.local.as_ref();
            match element.name.ns.as_ref() {
                "http://www.w3.org/1999/xhtml" => HTML_INTERFACES
                    .iter()
                    .find_map(|(tag, interface)| (*tag == local).then_some(*interface))
                    .unwrap_or_else(|| {
                        if GENERIC_HTML_TAGS.contains(&local) || autonomous_custom_name(local) {
                            "HTMLElement"
                        } else {
                            "HTMLUnknownElement"
                        }
                    }),
                "http://www.w3.org/2000/svg" if local == "svg" => "SVGSVGElement",
                "http://www.w3.org/2000/svg" => "SVGElement",
                _ => "Element",
            }
        }
        Some(NodeData::AnonymousBlock(_)) => "Element",
        Some(NodeData::ShadowRoot(_)) | None => return fallback.node.clone(),
    };
    prototype(name, context)
}

pub(super) fn init(context: &mut Context) {
    context.insert_data(Interfaces {
        prototypes: GcRefCell::new(HashMap::new()),
    });
    let ctx = super::dom_ctx(context).expect("missing DOM context");
    let (node, element, character_data, document) = {
        let state = ctx.state.borrow();
        let protos = state.protos();
        (
            protos.node.clone(),
            protos.element.clone(),
            protos.character_data.clone(),
            protos.document.clone(),
        )
    };
    let object = || JsObject::with_object_proto(context.intrinsics());
    let event_target = object();
    register(
        "EventTarget",
        None,
        event_target,
        0,
        NativeFunction::from_fn_ptr(super::event_target::construct),
        context,
    );
    for (name, parent, proto) in [
        ("Node", "EventTarget", node.clone()),
        ("Element", "Node", element.clone()),
        ("CharacterData", "Node", character_data),
        ("Document", "Node", document.clone()),
    ] {
        register(
            name,
            Some(parent),
            proto,
            0,
            NativeFunction::from_fn_ptr(illegal),
            context,
        );
    }
    for (name, parent) in [
        ("HTMLElement", "Element"),
        ("HTMLUnknownElement", "HTMLElement"),
        ("HTMLMediaElement", "HTMLElement"),
        ("HTMLDocument", "Document"),
        ("SVGElement", "Element"),
        ("SVGSVGElement", "SVGElement"),
    ] {
        let proto = JsObject::with_object_proto(context.intrinsics());
        let body = if name.starts_with("HTML") && name != "HTMLDocument" {
            NativeFunction::from_fn_ptr(html_constructor)
        } else {
            NativeFunction::from_fn_ptr(illegal)
        };
        register(name, Some(parent), proto, 0, body, context);
    }
    for &(_, name) in HTML_INTERFACES {
        if context
            .get_data::<Interfaces>()
            .expect("missing DOM interfaces")
            .prototypes
            .borrow()
            .contains_key(name)
        {
            continue;
        }
        let parent = match name {
            "HTMLAudioElement" | "HTMLVideoElement" => "HTMLMediaElement",
            _ => "HTMLElement",
        };
        let proto = JsObject::with_object_proto(context.intrinsics());
        register(
            name,
            Some(parent),
            proto,
            0,
            NativeFunction::from_fn_ptr(html_constructor),
            context,
        );
    }
    for (name, parent, length) in [
        ("Text", "CharacterData", 0),
        ("Comment", "CharacterData", 0),
        ("DocumentFragment", "Node", 0),
    ] {
        let proto = JsObject::with_object_proto(context.intrinsics());
        register(
            name,
            Some(parent),
            proto,
            length,
            NativeFunction::from_copy_closure(move |target, args, context| {
                node_constructor(name, target, args, context)
            }),
            context,
        );
    }
    // Window: the global object's interface. Its own properties (location,
    // addEventListener, ...) stay where they are; the chain under it becomes
    // Window.prototype > EventTarget.prototype > Object.prototype, so
    // `window instanceof Window` and `Window.prototype` patching work.
    let window_proto = JsObject::with_object_proto(context.intrinsics());
    register(
        "Window",
        Some("EventTarget"),
        window_proto.clone(),
        0,
        NativeFunction::from_fn_ptr(illegal),
        context,
    );
    let global = context.global_object();
    global.set_prototype(Some(window_proto));
    // Node types the HTML tree never creates, but whose interfaces scripts
    // patch or test with instanceof (ShadyDOM walks every one of them).
    for (name, parent) in [
        ("CDATASection", "Text"),
        ("ProcessingInstruction", "CharacterData"),
        ("DocumentType", "Node"),
    ] {
        let proto = JsObject::with_object_proto(context.intrinsics());
        register(
            name,
            Some(parent),
            proto,
            0,
            NativeFunction::from_fn_ptr(illegal),
            context,
        );
    }
    for (name, value) in [
        ("ELEMENT_NODE", 1),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", 3),
        ("CDATA_SECTION_NODE", 4),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", 8),
        ("DOCUMENT_NODE", 9),
        ("DOCUMENT_TYPE_NODE", 10),
        ("DOCUMENT_FRAGMENT_NODE", 11),
        ("DOCUMENT_POSITION_DISCONNECTED", 1),
        ("DOCUMENT_POSITION_PRECEDING", 2),
        ("DOCUMENT_POSITION_FOLLOWING", 4),
        ("DOCUMENT_POSITION_CONTAINS", 8),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32),
    ] {
        node.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(value)
                .writable(false)
                .enumerable(true)
                .configurable(false),
            context,
        )
        .expect("failed to define Node constant");
    }
    super::event_target::init(&node, context);
    super::collections::init(&node, &element, &document, context);
    super::geometry::init(&element, context);
    super::html_element::init(&element, context);
    crate::media::install(&prototype("HTMLMediaElement", context), context);
}
