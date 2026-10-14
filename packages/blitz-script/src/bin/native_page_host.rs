//! Linked page entry for the ps-qa native-page functional witness.

use std::cell::RefCell;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig, StyleThreading};
use blitz_script::{ApplicationDocument, EkojitDocument, NativePageDefect, ScriptBackend};
use blitz_traits::shell::{ColorScheme, Viewport};
use nagoya::reactor::{Reactor, TaskSet, block_on_with};
use serde_json::{Value, json};

struct Parameters {
    page: PathBuf,
    engine: ScriptBackend,
    defect: NativePageDefect,
}

fn parameters() -> Result<Parameters, String> {
    let mut arguments = std::env::args_os().skip(1);
    let mut page = None;
    let mut engine = ScriptBackend::default();
    let mut defect = NativePageDefect::None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--page") => page = Some(PathBuf::from(arguments.next().ok_or("--page requires a path")?)),
            Some("--engine") => engine = match arguments.next().and_then(|value| value.into_string().ok()).as_deref() {
                Some("boa") => ScriptBackend::Boa,
                Some("ekojit") => ScriptBackend::Ekojit,
                _ => return Err("--engine requires boa or ekojit".into()),
            },
            Some("--plant") => defect = match arguments.next().and_then(|value| value.into_string().ok()).as_deref() {
                Some("host-text-append-noop") => NativePageDefect::HostTextAppendNoop,
                _ => return Err("unknown native-page defect".into()),
            },
            _ => return Err("expected --page, --engine or --plant".into()),
        }
    }
    if defect != NativePageDefect::None && engine != ScriptBackend::Ekojit {
        return Err("native-page defect requires --engine ekojit".into());
    }
    Ok(Parameters { page: page.ok_or("--page is required")?, engine, defect })
}

fn observe(parameters: Parameters) -> Result<Arc<Value>, String> {
    let mut source = String::new();
    std::fs::File::open(&parameters.page).map_err(|error| error.to_string())?
        .take(65_537).read_to_string(&mut source).map_err(|error| error.to_string())?;
    if source.len() > 65_536 { return Err("native-page HTML exceeds 64 KiB".into()); }
    let source: Arc<str> = Arc::from(source);
    let config = DocumentConfig { viewport: Some(Viewport::new(640, 480, 1.0, ColorScheme::Light)),
        style_threading: StyleThreading::Sequential,
        ..DocumentConfig::default() };
    let mut document = if parameters.defect == NativePageDefect::None {
        ApplicationDocument::from_html(source, config, parameters.engine)?
    } else {
        ApplicationDocument::Ekojit(EkojitDocument::from_html(source, config, parameters.defect)
            .map_err(|error| error.to_string())?)
    };
    document.execute_scripts()?;
    document.inner_mut().resolve(0.0);
    let rgba: Arc<[u8]> = Arc::from(render_to_buffer::<VelloCpuImageRenderer, _>(|scene| {
        blitz_paint::paint_scene_at_time(scene, &mut document.inner_mut(), 1.0, 640, 480, 0, 0, 0.0)
    }, 640, 480));
    let (backend, title, console, errors, receipts) = match &document {
        ApplicationDocument::Ekojit(native) => (
            "ekojit", native.title(), native.console_entries(), native.runtime_errors(),
            native.native_receipts().iter().map(|receipt| json!({
                "source_url": receipt.source_url.as_ref(), "backend": receipt.backend,
                "entry_abi": receipt.entry_abi, "native_entries": receipt.native_entries,
                "host_calls": receipt.host_calls, "code_bytes": receipt.code_bytes,
            })).collect::<Vec<_>>(),
        ),
        ApplicationDocument::Boa(boa) => {
            let diagnostics = boa.diagnostic_batch(0, 0, 1000);
            let title = boa.inner().tree().iter().find_map(|(_, node)| {
                node.element_data().filter(|element| element.name.local == blitz_dom::local_name!("title"))
                    .map(|_| node.text_content())
            }).unwrap_or_default();
            ("boa", title, diagnostics.console.entries, diagnostics.errors.entries, Vec::new())
        }
    };
    let inner = document.inner();
    let mut texts = Vec::new();
    let mut stack = vec![inner.root_node().id];
    while let Some(id) = stack.pop() {
        let Some(node) = inner.get_node(id) else { continue; };
        stack.extend(node.children.iter().rev().copied());
        let Some(text) = node.text_data() else { continue; };
        if text.content.trim().is_empty() { continue; }
        let parent = node.layout_parent.get().or(node.parent);
        let bounds = parent.and_then(|parent| inner.get_client_bounding_rect(parent));
        let (bounds, ink) = if let Some(rect) = bounds {
            let x0 = rect.x.floor().clamp(0.0, 640.0) as usize;
            let y0 = rect.y.floor().clamp(0.0, 480.0) as usize;
            let x1 = (rect.x + rect.width).ceil().clamp(0.0, 640.0) as usize;
            let y1 = (rect.y + rect.height).ceil().clamp(0.0, 480.0) as usize;
            let mut ink = 0;
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = (y * 640 + x) * 4;
                    if rgba[at + 3] > 0 && rgba[at..at + 3].iter().any(|value| *value < 180) { ink += 1; }
                }
            }
            (Some([rect.x, rect.y, rect.width, rect.height]), ink)
        } else { (None, 0) };
        texts.push(json!({ "node_id": id.as_u64(), "text": text.content,
            "bounds": bounds, "ink_pixels": ink }));
    }
    Ok(Arc::new(json!({ "schema": "eko-page-observation-v1", "backend": backend,
        "title": title, "console": console.into_iter().map(|entry| entry.message).collect::<Vec<_>>(),
        "errors": errors.into_iter().map(|entry| entry.message).collect::<Vec<_>>(),
        "native": receipts, "texts": texts, "viewport": [640, 480] })))
}

fn run() -> Result<(), String> {
    let parameters = parameters()?;
    let reactor = Reactor::local().map_err(|error| error.to_string())?;
    let output = Rc::new(RefCell::new(None));
    let completed = Rc::clone(&output);
    let mut tasks = TaskSet::new();
    // The local nagoya task is the sole DOM owner. Immutable observations
    // leave that task through Arc; no live DOM or host handle escapes.
    tasks.push(async move { *completed.borrow_mut() = Some(observe(parameters)); });
    block_on_with(&reactor, tasks);
    reactor.shutdown();
    let observation = output.borrow_mut().take().ok_or("native owner task did not finish")??;
    println!("{}", serde_json::to_string(&*observation).map_err(|error| error.to_string())?);
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("native-page-host: {error}");
        std::process::exit(2);
    }
}
