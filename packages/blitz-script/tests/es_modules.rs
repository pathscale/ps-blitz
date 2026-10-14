//! `<script type="module">`: parsing in module goal, resolving imports over the
//! document's fetcher, and `import.meta`.
//!
//! Before this existed, a module was handed to the classic-script evaluator and
//! died on its own first line with
//! `SyntaxError: expected token '.', got '{' in import.meta`. Every assertion
//! here is about a page that could not start at all.

#[path = "../src/loopback_modules.rs"]
mod loopback_modules;
use loopback_modules::{LoopbackFetcher, serve_modules};

use blitz_dom::DocumentConfig;
use blitz_script::ScriptDocument;

fn config_with_base(base_url: &str) -> DocumentConfig {
    DocumentConfig {
        base_url: Some(base_url.to_owned()),
        ..Default::default()
    }
}

fn eval_string(doc: &mut ScriptDocument, expression: &str) -> String {
    match doc.eval_json(expression) {
        Ok(serde_json::Value::String(value)) => value,
        other => panic!("expected {expression} to be a string, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A loopback HTTP server and a fetcher that speaks to it.
//
// A real socket rather than an in-memory map, because the property under test
// is that the loader reaches the document's fetcher for a URL it resolved
// itself. A map keyed by the string the test already wrote down would pass
// without the resolution ever being right.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An inline module runs at all.
///
/// The narrowest statement of the original bug: this markup produced a
/// `SyntaxError` on `import.meta` and set nothing, because the classic-script
/// goal has no top-level `await`, no `export`, and no module `import`.
#[test]
fn an_inline_module_is_parsed_in_module_goal() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">
             const value = await Promise.resolve("module ran");
             globalThis.result = value;
           </script>"#,
        DocumentConfig::default(),
    );
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "module ran");
}

/// A classic script is unchanged.
///
/// The regression that matters: every page's scripts go through the same
/// routing decision, so a module path that captured classic scripts would break
/// far more than it fixed.
#[test]
fn a_classic_script_still_runs_unchanged() {
    let mut doc = ScriptDocument::from_html(
        r#"<div id="root"></div>
           <script>
             const el = document.createElement("span");
             el.textContent = "classic";
             document.getElementById("root").appendChild(el);
             globalThis.classicRan = "yes";
           </script>"#,
        DocumentConfig::default(),
    );
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.classicRan"), "yes");
    assert_eq!(
        eval_string(&mut doc, "document.getElementById('root').textContent"),
        "classic"
    );
}

/// `import.meta.url` names the module's own URL.
///
/// Asset helpers are built on it (`new URL("./icon.svg", import.meta.url)`),
/// and an undefined value there throws inside module initialisation, taking the
/// whole module with it.
#[test]
fn import_meta_url_is_the_module_url() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">globalThis.metaUrl = import.meta.url;</script>"#,
        config_with_base("https://example.invalid/page/index.html"),
    );
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(
        eval_string(&mut doc, "globalThis.metaUrl"),
        "https://example.invalid/page/index.html"
    );
}

/// A module imports another module, fetched over a real socket.
///
/// This is the case that decides whether the fix is worth anything: every site
/// in the corpus that failed this way uses `import ... from`, not bare
/// `import.meta`.
#[test]
fn a_module_imports_a_relative_module_over_the_fetcher() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import { greeting } from "./dep.js";
               globalThis.result = greeting + " from the loader";"#
                .to_owned(),
        ),
        ("/dep.js", r#"export const greeting = "hello";"#.to_owned()),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(
        eval_string(&mut doc, "globalThis.result"),
        "hello from the loader"
    );

    let requested = server.join().expect("the server thread finishes");
    assert!(
        requested.iter().any(|path| path == "/dep.js"),
        "the imported module should have been fetched, saw: {requested:?}"
    );
}

/// The same module imported twice is instantiated once.
///
/// The spec requires it, and the observable consequence of getting it wrong is
/// subtle: two copies of a module's top-level state, so a store, a router, or a
/// framework registry silently splits in half.
#[test]
fn a_module_imported_twice_is_one_instance() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import { bump, count } from "./counter.js";
               import "./sibling.js";
               bump();
               globalThis.result = String(count());"#
                .to_owned(),
        ),
        (
            "/sibling.js",
            r#"import { bump } from "./counter.js";
               bump();"#
                .to_owned(),
        ),
        (
            "/counter.js",
            r#"let n = 0;
               export function bump() { n += 1; }
               export function count() { return n; }"#
                .to_owned(),
        ),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "2");

    let requested = server.join().expect("the server thread finishes");
    let counter_fetches = requested
        .iter()
        .filter(|path| *path == "/counter.js")
        .count();
    assert_eq!(
        counter_fetches, 1,
        "the shared module should be fetched once, saw: {requested:?}"
    );
}

/// A classic fallback marked `nomodule` does not also run.
///
/// Pages that ship both a module bundle and an ES5 one rely on the engine
/// skipping exactly one of them. Now that modules run, running the fallback too
/// would mount the application twice.
#[test]
fn a_nomodule_fallback_is_skipped() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">globalThis.ran = "module";</script>
           <script nomodule>globalThis.ran = "fallback";</script>"#,
        DocumentConfig::default(),
    );
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.ran"), "module");
}

/// A bare specifier resolves through the page's import map.
///
/// This is what a page that ships unbundled modules is built on:
/// `import { h } from "preact"` names nothing at all without the map, so the
/// graph stops at the first dependency of the entry point.
#[test]
fn an_import_map_resolves_a_bare_specifier() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import { greeting } from "shared";
               globalThis.result = greeting;"#
                .to_owned(),
        ),
        (
            "/vendor/shared.js",
            r#"export const greeting = "mapped";"#.to_owned(),
        ),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="importmap">
             {"imports": {"shared": "/vendor/shared.js"}}
           </script>
           <script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "mapped");

    let requested = server.join().expect("the server thread finishes");
    assert!(
        requested.iter().any(|path| path == "/vendor/shared.js"),
        "the mapped URL should have been fetched, saw: {requested:?}"
    );
}

/// Dynamic `import()` from a classic script resolves against that script.
///
/// A bundle served from `/assets/` that splits its chunks calls
/// `import("./chunk.js")`, and resolving that against the document instead of
/// the script looks for the chunk beside the HTML, where it is not.
#[test]
fn dynamic_import_from_a_classic_script_resolves_against_the_script() {
    let (origin, server) = serve_modules(vec![
        (
            "/assets/entry.js",
            r#"globalThis.ready = import("./chunk.js").then((m) => {
                 globalThis.result = m.value;
               });"#
                .to_owned(),
        ),
        (
            "/assets/chunk.js",
            r#"export const value = "dynamic";"#.to_owned(),
        ),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script src="/assets/entry.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "dynamic");

    let requested = server.join().expect("the server thread finishes");
    assert!(
        requested.iter().any(|path| path == "/assets/chunk.js"),
        "the chunk should be resolved beside its script, saw: {requested:?}"
    );
}

/// An unresolvable bare specifier says so, rather than 404ing on an invented
/// URL the page never wrote.
///
/// The diagnostic is the deliverable here: this is the failure mode a page with
/// a missing or misspelled import map entry actually hits.
#[test]
fn an_unmapped_bare_specifier_reports_what_is_wrong() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">
             globalThis.error = "";
             try {
               await import("preact");
             } catch (e) {
               globalThis.error = String(e);
             }
           </script>"#,
        config_with_base("https://example.invalid/index.html"),
    );
    doc.run_until_idle().expect("script startup completes");

    let error = eval_string(&mut doc, "globalThis.error");
    assert!(
        error.contains("bare module specifier") && error.contains("preact"),
        "the error should name the specifier and the reason, got: {error}"
    );
}

/// `import config from "./config.json" with { type: "json" }`.
///
/// A JSON module is data, not source: running it through the JavaScript parser
/// would reject the object literal most JSON files begin with.
#[test]
fn a_json_module_exports_its_document_as_the_default() {
    let (origin, server) = serve_modules(vec![
        (
            "/app.js",
            r#"import config from "./config.json" with { type: "json" };
               globalThis.result = config.title;"#
                .to_owned(),
        ),
        ("/config.json", r#"{"title": "from json"}"#.to_owned()),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script type="module" src="/app.js"></script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "from json");
    let _ = server.join();
}

/// A module whose top-level `await` waits on a timer is not a failure.
///
/// Its evaluation promise is still pending when the script returns, and a
/// browser settles it on a later turn of the event loop. Reporting that as an
/// error at evaluation time would fire against every module on the modern web
/// that waits for anything.
#[test]
fn a_module_awaiting_a_timer_settles_on_a_later_poll() {
    let mut doc = ScriptDocument::from_html(
        r#"<script type="module">
             await new Promise((resolve) => setTimeout(resolve, 1));
             globalThis.result = "settled";
           </script>"#,
        DocumentConfig::default(),
    );
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(eval_string(&mut doc, "globalThis.result"), "settled");
}

/// Modules and `defer` run after every parser-blocking classic script.
///
/// Document order alone was right until modules existed. It is now wrong in a
/// way that bites: an inline classic script writing `window.__CONFIG__` after a
/// module tag runs *before* that module in a real engine, and the module reads
/// the config it expects.
///
/// The expected script sequence below was recorded from this mixture of
/// script forms served to a stock engine over loopback. DOMContentLoaded
/// must follow the completed sequence.
#[test]
fn deferred_scripts_run_after_the_parser_blocking_ones() {
    let (origin, server) = serve_modules(vec![
        (
            "/m.js",
            r#"globalThis.order.push("m.js external module");"#.to_owned(),
        ),
        (
            "/c.js",
            r#"globalThis.order.push("c.js external classic");"#.to_owned(),
        ),
        (
            "/d.js",
            r#"globalThis.order.push("d.js external classic defer");"#.to_owned(),
        ),
    ]);

    let mut doc = ScriptDocument::from_html(
        r#"<script>
             globalThis.order = [];
             document.addEventListener("DOMContentLoaded", () => {
               globalThis.order.push("DOMContentLoaded");
             });
           </script>
           <script type="module">globalThis.order.push("A inline module");</script>
           <script>globalThis.order.push("B inline classic");</script>
           <script type="module" src="/m.js"></script>
           <script src="/c.js"></script>
           <script defer src="/d.js"></script>
           <script>globalThis.order.push("C inline classic after everything");</script>
           <script type="module">globalThis.order.push("D inline module last");</script>"#,
        config_with_base(&format!("{origin}/index.html")),
    )
    .with_fetcher(LoopbackFetcher);
    doc.run_until_idle().expect("script startup completes");

    assert_eq!(
        eval_string(&mut doc, "globalThis.order.join('|')"),
        [
            "B inline classic",
            "c.js external classic",
            "C inline classic after everything",
            "A inline module",
            "m.js external module",
            "d.js external classic defer",
            "D inline module last",
            "DOMContentLoaded",
        ]
        .join("|")
    );
    let _ = server.join();
}
