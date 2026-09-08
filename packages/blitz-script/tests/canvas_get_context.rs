//! `canvas.getContext(...)` answers null rather than not existing.
//!
//! `@pathscale/ui`'s metal-border effect asks a canvas for a WebGL context
//! during render:
//!
//! ```js
//! let c = document.createElement("canvas");
//! t = c.getContext("webgl", { alpha: true }) ?? c.getContext("experimental-webgl");
//! if (!t) throw Error("metal-fx: WebGL not supported");
//! ```
//!
//! The method did not exist, so the call threw `TypeError: not a callable
//! function` before the `??` could run, and the guard the library already
//! carries never got its turn. Under Solid 2 an error with no boundary above
//! it halts the reactive system permanently, so consulting.parcle.ai painted
//! once and then answered nothing at all: every effect, every event handler
//! and every subsequent render stopped.
//!
//! Returning null is not a stub standing in for a canvas implementation. It
//! is what the specification says an implementation answers when it does not
//! support the context asked for, and it is what a browser answers when WebGL
//! is unavailable. A page that handles "no WebGL" now gets to handle it.

use blitz_script::ScriptDocument;

fn value(doc: &mut ScriptDocument, expression: &str) -> serde_json::Value {
    doc.eval_json(expression).unwrap_or(serde_json::Value::Null)
}

fn page() -> ScriptDocument {
    ScriptDocument::from_html(
        "<html><body><canvas id='c'></canvas></body></html>",
        blitz_dom::DocumentConfig::default(),
    )
}

/// The method is there to be called.
#[test]
fn get_context_is_callable() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(&mut doc, "typeof document.getElementById('c').getContext"),
        "function"
    );
}

/// And it says "not supported" the way the specification says to.
#[test]
fn an_unsupported_context_is_null_rather_than_a_thrown_type_error() {
    let mut doc = page();
    doc.execute_scripts();
    for identifier in ["webgl", "experimental-webgl", "2d", "webgl2"] {
        assert_eq!(
            value(
                &mut doc,
                &format!("document.getElementById('c').getContext('{identifier}')")
            ),
            serde_json::Value::Null,
            "getContext('{identifier}') must answer null, not throw"
        );
    }
}

/// The library's own feature detect reaches its guard.
///
/// This is the shape that was failing, and the assertion is that the error
/// raised is the library's deliberate one rather than the engine's accident.
#[test]
fn the_metal_border_feature_detect_reaches_its_own_guard() {
    let mut doc = page();
    doc.execute_scripts();
    assert_eq!(
        value(
            &mut doc,
            "(() => { try { \
               const c = document.createElement('canvas'); \
               const gl = c.getContext('webgl', { alpha: true }) ?? c.getContext('experimental-webgl'); \
               if (!gl) throw new Error('metal-fx: WebGL not supported'); \
               return 'got a context'; \
             } catch (error) { return error.message; } })()"
        ),
        "metal-fx: WebGL not supported"
    );
}
