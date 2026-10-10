//! Realm-local document input streams.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use blitz_dom::{NodeData, NodeId, local_name};
use blitz_html::DocumentHtmlParser;
use blitz_html::stream::{StreamInput, StreamingParser};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsValue, Source, Trace, js_string,
};

use crate::dom::{
    define_accessor, define_method, dom_ctx, js_str, node_id_of_value, node_wrapper, this_node_id,
    to_rust_string,
};
use crate::module::{BlitzModuleLoader, ImportMap, SharedFetcher};
use crate::state::DomCtx;

#[derive(Trace, Finalize, JsData)]
struct InputState {
    #[unsafe_ignore_trace]
    original: RefCell<Option<Arc<str>>>,
    #[unsafe_ignore_trace]
    parser: RefCell<Option<Rc<RefCell<StreamingParser>>>>,
    #[unsafe_ignore_trace]
    boundary_pending: Cell<bool>,
    #[unsafe_ignore_trace]
    script_created: Cell<bool>,
    #[unsafe_ignore_trace]
    restart: Cell<bool>,
    #[unsafe_ignore_trace]
    generation: Cell<usize>,
    #[unsafe_ignore_trace]
    is_xml: Cell<bool>,
    #[unsafe_ignore_trace]
    executed: RefCell<HashSet<NodeId>>,
    #[unsafe_ignore_trace]
    fetcher: RefCell<Option<SharedFetcher>>,
    #[unsafe_ignore_trace]
    loader: Rc<BlitzModuleLoader>,
}

fn state(context: &Context) -> &InputState {
    context
        .get_data::<InputState>()
        .expect("document input state missing")
}

pub(crate) fn install(context: &mut Context, loader: Rc<BlitzModuleLoader>) {
    context.insert_data(InputState {
        original: RefCell::new(None),
        parser: RefCell::new(None),
        boundary_pending: Cell::new(false),
        script_created: Cell::new(false),
        restart: Cell::new(false),
        generation: Cell::new(0),
        is_xml: Cell::new(false),
        executed: RefCell::new(HashSet::new()),
        fetcher: RefCell::new(None),
        loader,
    });
    let ctx = dom_ctx(context).expect("document input needs a DOM context");
    let prototype = ctx.state.borrow().protos().document.clone();
    define_method(&prototype, "open", 0, open, context);
    define_method(&prototype, "write", 0, write, context);
    define_method(&prototype, "writeln", 0, writeln, context);
    define_method(&prototype, "close", 0, close, context);
    define_method(&prototype, "hasFocus", 0, has_focus, context);
    define_accessor(&prototype, "referrer", Some(referrer), None, context);
    define_accessor(&prototype, "contentType", Some(content_type), None, context);
    define_accessor(&prototype, "compatMode", Some(compat_mode), None, context);
}

pub(crate) fn prepare(
    context: &Context,
    html: &str,
    fetcher: SharedFetcher,
    parser: Option<StreamingParser>,
) {
    let input = state(context);
    input.is_xml.set(DocumentHtmlParser::is_xhtml_document(html));
    input.boundary_pending.set(parser.is_some());
    // Only an unfinished navigation needs a source scan for prefetch. At EOF
    // the live tree is complete, including on pages without scripts.
    *input.original.borrow_mut() = parser.as_ref().map(|_| Arc::from(html));
    *input.parser.borrow_mut() = parser.map(|parser| Rc::new(RefCell::new(parser)));
    *input.fetcher.borrow_mut() = Some(fetcher);
}

pub(crate) fn original(context: &Context) -> Option<Arc<str>> {
    state(context).original.borrow().clone()
}

fn document_receiver(this: &JsValue, context: &mut Context) -> JsResult<(DomCtx, bool)> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    let main = {
        let document = ctx.doc.borrow();
        if !matches!(
            document.get_node(id).map(|node| &node.data),
            Some(NodeData::Document(_))
        ) {
            return Err(JsNativeError::typ()
                .with_message("Illegal Document receiver")
                .into());
        }
        id == document.root_node().id
    };
    Ok((ctx, main))
}

fn main_document(this: &JsValue, context: &mut Context) -> JsResult<DomCtx> {
    let (ctx, main) = document_receiver(this, context)?;
    if !main {
        return Err(crate::domc::exception(
            "NotSupportedError",
            "Detached document input streams are not supported",
            context,
        ));
    }
    if state(context).is_xml.get() {
        return Err(crate::domc::exception(
            "InvalidStateError",
            "XML documents do not support HTML input streams",
            context,
        ));
    }
    Ok(ctx)
}

fn reset(context: &mut Context, clear_listeners: bool) -> DomCtx {
    let ctx = dom_ctx(context).expect("document input needs a DOM context");
    let children = ctx.doc.borrow().root_node().children.clone();
    for child in children {
        crate::dom::remove_and_free_node(&ctx, child, context);
    }
    ctx.mutate_doc().begin_document_stream();
    if clear_listeners {
        let mut runtime = ctx.state.borrow_mut();
        runtime.node_listeners.clear();
        runtime.window_listeners.clear();
        runtime.pointer_capture.clear();
    }
    state(context).executed.borrow_mut().clear();
    state(context).boundary_pending.set(false);
    let next = state(context).generation.get().wrapping_add(1);
    state(context).generation.set(next);
    crate::domc::set_current_script(context, None);
    crate::domc::set_ready_state(context, "loading");
    ctx
}

pub(crate) fn begin(context: &mut Context) {
    // Construction already positioned the live parser at its first boundary.
    // Keep its nodes, resources and shim attachments. document.open() remains
    // the operation that replaces a document and resets its input stream.
    state(context).original.borrow_mut().take();
}

pub(crate) fn active(context: &Context) -> bool {
    state(context).parser.borrow().is_some()
}

pub(crate) fn generation(context: &Context) -> usize {
    state(context).generation.get()
}

pub(crate) fn take_restart(context: &Context) -> bool {
    state(context).restart.replace(false)
}

pub(crate) fn was_executed(context: &Context, id: NodeId) -> bool {
    state(context).executed.borrow().contains(&id)
}

fn is_current_parser(context: &Context, parser: &Rc<RefCell<StreamingParser>>) -> bool {
    state(context)
        .parser
        .borrow()
        .as_ref()
        .is_some_and(|current| Rc::ptr_eq(current, parser))
}

/// Advance navigation input to its next script boundary.
pub(crate) fn advance(context: &mut Context) -> bool {
    if state(context).script_created.get() {
        return false;
    }
    // Execute the boundary reached during construction before feeding more
    // navigation input, even when that first script is deferred or inert.
    if state(context).boundary_pending.replace(false) {
        return true;
    }
    let parser = state(context).parser.borrow().clone();
    let Some(parser) = parser else {
        return false;
    };
    crate::dom::custom_elements::parser_scope(context, |context| {
        let script = parser.borrow_mut().next_script();
        if script.is_none() {
            parser.borrow_mut().finish();
            state(context).parser.borrow_mut().take();
        }
    });
    true
}

fn open(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    main_document(this, context)?;
    if active(context)
        && (!state(context).script_created.get() || current_script(context)?.is_some())
    {
        return Ok(this.clone());
    }
    state(context).original.borrow_mut().take();
    let ctx = reset(context, true);
    *state(context).parser.borrow_mut() = Some(Rc::new(RefCell::new(StreamingParser::new(
        Rc::clone(&ctx.doc),
        "",
    ))));
    state(context).script_created.set(true);
    state(context).restart.set(true);
    Ok(this.clone())
}

fn current_script(context: &mut Context) -> JsResult<Option<NodeId>> {
    let global = context.global_object().clone();
    let document = global
        .get(js_string!("document"), context)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Document is missing"))?;
    Ok(node_id_of_value(
        &document.get(js_string!("currentScript"), context)?,
    ))
}

fn write_input(
    this: &JsValue,
    arguments: &[JsValue],
    newline: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let ctx = main_document(this, context)?;
    let mut text = String::new();
    for argument in arguments {
        text.push_str(&to_rust_string(argument, context)?);
    }
    if newline {
        text.push('\n');
    }
    if !active(context) {
        open(this, &[], context)?;
    }
    let parser = state(context)
        .parser
        .borrow()
        .clone()
        .expect("input stream missing");
    let input = StreamInput::new(&text);
    loop {
        let next = crate::dom::custom_elements::parser_scope(context, |_| {
            parser.borrow_mut().feed_written(&input)
        });
        // Reactions can open, close, or replace the input stream before the
        // script returned by the old parser gets a chance to execute.
        if !is_current_parser(context, &parser) {
            break;
        }
        let Some(script) = next else {
            break;
        };
        execute_written(script, context)?;
        if !is_current_parser(context, &parser) {
            break;
        }
    }
    ctx.mark_layout_dirty();
    ctx.doc.borrow().shell_provider.request_redraw();
    Ok(JsValue::undefined())
}

fn write(this: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_input(this, arguments, false, context)
}

fn writeln(this: &JsValue, arguments: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_input(this, arguments, true, context)
}

fn close(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = main_document(this, context)?;
    if state(context).script_created.get() {
        let parser = state(context).parser.borrow_mut().take();
        state(context).script_created.set(false);
        if let Some(parser) = parser {
            crate::dom::custom_elements::parser_scope(context, |_| {
                parser.borrow_mut().finish();
            });
        }
        ctx.mark_layout_dirty();
        ctx.doc.borrow().shell_provider.request_redraw();
    }
    Ok(JsValue::undefined())
}

fn dispatch_resource_event(
    ctx: &DomCtx,
    id: NodeId,
    name: &str,
    context: &mut Context,
) -> JsResult<()> {
    let node = node_wrapper(ctx, id, context);
    let target: JsValue = node.clone().into();
    let event = crate::dom::event::create_event(ctx, name, false, false, &target, context);
    crate::dom::event::set_event_field(&event, "isTrusted", &JsValue::from(true));
    if let Some(dispatch) = node.get(js_string!("dispatchEvent"), context)?.as_object() {
        dispatch.call(&target, &[event.into()], context)?;
    }
    Ok(())
}

fn execute_written(id: NodeId, context: &mut Context) -> JsResult<()> {
    let ctx = dom_ctx(context)?;
    let (kind, source, text, deferred, base) = {
        let document = ctx.doc.borrow();
        let Some(node) = document.get_node(id) else {
            return Ok(());
        };
        let Some(element) = node.element_data() else {
            return Ok(());
        };
        let kind = element
            .attr(local_name!("type"))
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let source = element.attr(local_name!("src")).map(str::to_string);
        let deferred = kind == "module"
            || (source.is_some()
                && element.attr(local_name!("defer")).is_some()
                && element.attr(local_name!("async")).is_none());
        if element.attr(local_name!("nomodule")).is_some() {
            return Ok(());
        }
        (
            kind,
            source,
            node.text_content(),
            deferred,
            document.url().clone(),
        )
    };
    if deferred {
        return Ok(());
    }
    if kind == "importmap" {
        let loader = Rc::clone(&state(context).loader);
        loader.set_import_map(ImportMap::parse(&text, Some(&base)));
        state(context).executed.borrow_mut().insert(id);
        return Ok(());
    }
    if !matches!(
        kind.as_str(),
        "" | "text/javascript" | "application/javascript"
    ) {
        return Ok(());
    }
    state(context).executed.borrow_mut().insert(id);
    let external = source.is_some();
    let (code, url) = if let Some(source) = source {
        let url = match base.join(&source) {
            Ok(url) => url,
            Err(_) => {
                dispatch_resource_event(&ctx, id, "error", context)?;
                return Ok(());
            }
        };
        let shared = state(context)
            .fetcher
            .borrow()
            .clone()
            .expect("script fetcher missing");
        let fetcher = Rc::clone(&shared.borrow());
        match fetcher.fetch(&url) {
            Ok(code) => (code, url),
            Err(error) => {
                eprintln!("blitz-script: written script fetch failed: {error}");
                dispatch_resource_event(&ctx, id, "error", context)?;
                return Ok(());
            }
        }
    } else {
        (text, base)
    };
    let previous = current_script(context)?;
    crate::domc::set_current_script(context, Some(id));
    let path = url.to_string();
    let source = Source::from_bytes(code.as_bytes()).with_path(Path::new(&path));
    let result = context.eval(source);
    crate::domc::set_current_script(context, previous);
    if let Err(error) = result {
        eprintln!("blitz-script: written script failed: {error}");
    }
    if external {
        dispatch_resource_event(&ctx, id, "load", context)?;
    }
    Ok(())
}

fn has_focus(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (ctx, main) = document_receiver(this, context)?;
    Ok(JsValue::from(main && ctx.doc.borrow().window_focused()))
}

fn referrer(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (ctx, main) = document_receiver(this, context)?;
    let document = ctx.doc.borrow();
    Ok(js_str(if main { document.referrer() } else { "" }))
}

fn content_type(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (ctx, main) = document_receiver(this, context)?;
    let doc = ctx.doc.borrow();
    if main {
        return Ok(js_str(doc.content_type()));
    }
    // A detached document (DOMParser, createHTMLDocument) keeps its own type.
    match doc.get_node(this_node_id(this)?).map(|node| &node.data) {
        Some(NodeData::Document(data)) => Ok(js_str(data.content_type)),
        _ => Ok(js_str(doc.content_type())),
    }
}

fn compat_mode(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_node_id(this)?;
    let (ctx, _) = document_receiver(this, context)?;
    let document = ctx.doc.borrow();
    let quirks = matches!(
        document.get_node(id).map(|node| &node.data),
        Some(NodeData::Document(data)) if data.quirks_mode == 2
    );
    Ok(js_str(if quirks { "BackCompat" } else { "CSS1Compat" }))
}
