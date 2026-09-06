//! Layout that only exists once text has shaped, quarantined to macOS.
//!
//! Every test here measures a box whose size comes from its own text: an
//! autosizing composer, a bubble that grows with the reply streaming into it, a
//! field that has to come back down when it is cleared. None of that can be
//! asserted without a face. With no font registered parley shapes nothing,
//! every line collapses, and the numbers come back `0.0` — so the assertions do
//! not fail honestly, they stop being able to fail at all.
//!
//! The answer is not to give a headless Linux runner a font. Nothing this crate
//! ships reads a font catalogue, `system-fonts` is off, and a headless build
//! reaches no system library; keeping it that way is the point. So these run
//! where fonts are guaranteed and cost nothing: macOS resolves them through
//! Core Text, with no `fontconfig` and nothing to install. On Linux the whole
//! file compiles out.
#![cfg(target_os = "macos")]

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

#[test]
fn a_shrink_to_fit_box_grows_when_its_text_does() {
    // The agent bubble's exact shape: a flex column item that is itself a flex
    // column, `align-self: flex-start` so it is as wide as its own content,
    // clamped by a max-width. Text arrives a token at a time while the agent
    // writes, so the width it settles on has to keep up with the text.
    //
    // The measurement behind that width is cached and is not keyed on the text.
    // If a text change does not invalidate it the bubble keeps the width it had
    // for its first line, and every later word paints outside it: text spilling
    // past the right edge of its own background.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          body { margin: 0; width: 900px; font: 16px monospace }
          #column { display: flex; flex-direction: column }
          #bubble {
            display: flex;
            flex-direction: column;
            align-self: flex-start;
            max-width: 88%;
          }
        </style>
        <div style="width: 900px"><div id="column"><div id="bubble"><div id="text">short</div></div></div></div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(900, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let sizes = doc
        .eval_json(
            r#"
            const bubble = document.getElementById("bubble");
            const text = document.getElementById("text");
            const before = bubble.clientWidth;
            const probe = { column: document.getElementById("column").clientWidth,
                            bubbleH: bubble.clientHeight,
                            textH: document.getElementById("text").clientHeight };
            text.textContent = "short and then a good deal more text arrived here";
            ({ before, width: bubble.clientWidth, scroll: bubble.scrollWidth,
               textWidth: text.clientWidth, textScroll: text.scrollWidth, probe })
            "#,
        )
        .expect("bubble geometry should evaluate");

    let before = sizes["before"].as_f64().expect("before");
    let width = sizes["width"].as_f64().expect("width");
    let scroll = sizes["scroll"].as_f64().expect("scroll");

    assert!(
        before > 0.0,
        "the bubble should size itself to its first line: {sizes}"
    );
    assert!(
        width > before,
        "the bubble should grow with its text: {sizes}"
    );
    assert!(
        scroll <= width + 0.5,
        "text must not overflow the box that sizes itself to it: {sizes}"
    );
}

#[test]
fn a_textarea_answers_its_height_without_a_document_resolve() {
    // What typing costs. An autosizing composer measures `scrollHeight` on
    // every keystroke; every geometry read flushes layout so the answer covers
    // pending mutations; and that flush is a full `doc.resolve`, measured at
    // 19.16ms of a 21.55ms keystroke.
    //
    // The document is never resolved between the mutation and the read below,
    // so the height can only come from the editor's own layout. Getting the
    // right answer here is what makes skipping the flush safe.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          body { margin: 0; width: 400px; font: 16px monospace }
          textarea {
            width: 300px; border: 0; padding: 0;
            font: 16px monospace; line-height: 20px;
            overflow-y: auto; overflow-wrap: anywhere;
          }
        </style>
        <textarea id="field" rows="1" wrap="soft"></textarea>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let one_row = doc
        .eval_json(r#"(() => document.getElementById("field").scrollHeight)()"#)
        .expect("empty height")
        .as_f64()
        .expect("number");

    // Type, and read back without resolving in between.
    let grown = doc
        .eval_json(
            r#"
            (() => {
              const field = document.getElementById("field");
              field.value = "j;alksjf;laskj;lkjs;flsajdslaj;slkfjs;lfkjj;".repeat(6);
              return field.scrollHeight;
            })()
            "#,
        )
        .expect("grown height")
        .as_f64()
        .expect("number");

    assert!(one_row > 0.0, "an empty field still measures one row");
    assert!(
        grown >= one_row * 3.0,
        "the wrapped height has to be visible to the very next read, with no \
         resolve between: one_row={one_row}, grown={grown}"
    );
}

#[test]
fn a_textarea_measures_the_same_on_a_hidpi_display() {
    // CSS pixels do not depend on the display. A textarea 300px wide holding
    // the same text has the same client width and the same scroll height on a
    // 1x monitor and a 2x one, because both are CSS-pixel quantities.
    //
    // Why this is a separate test and not a parameter on the others: every
    // textarea test in this file builds its viewport at scale 1.0, which is the
    // one scale at which a device-pixel/CSS-pixel confusion cannot show. The
    // suite therefore agreed the wrapping was fixed while the app on a retina
    // display still wrapped its composer at half the box width, expanding to a
    // second line at roughly half a line of typing.
    fn geometry(scale: f32) -> (f64, f64) {
        let mut doc = ScriptDocument::from_html(
            r#"
            <style>
              body { margin: 0; width: 400px; font: 16px monospace }
              textarea {
                width: 300px; border: 0; padding: 0;
                font: 16px monospace; line-height: 20px;
                overflow-y: auto; overflow-wrap: anywhere;
              }
            </style>
            <textarea id="field" rows="1" wrap="soft"></textarea>
            "#,
            DocumentConfig {
                viewport: Some(Viewport::new(400, 300, scale, ColorScheme::Light)),
                ..DocumentConfig::default()
            },
        );
        doc.execute_scripts();
        doc.inner_mut().resolve(0.0);

        let sizes = doc
            .eval_json(
                r#"
                const field = document.getElementById("field");
                field.value = "the quick brown fox jumps over the lazy dog, twice over";
                ({ width: field.clientWidth, height: field.scrollHeight })
                "#,
            )
            .expect("textarea geometry should evaluate");
        (
            sizes["width"].as_f64().expect("width"),
            sizes["height"].as_f64().expect("height"),
        )
    }

    let (width_1x, height_1x) = geometry(1.0);
    let (width_2x, height_2x) = geometry(2.0);

    // Paired with the equality checks below. Both of those are satisfied by
    // `0.0 == 0.0`, which is exactly what a fontless run produces, so the
    // height has to be shown to be a real measurement before its stability
    // across scales means anything.
    assert!(
        height_1x > 0.0,
        "the text has to have shaped for this test to be checking anything: \
         1x={height_1x}"
    );

    assert_eq!(
        width_1x, width_2x,
        "a 300px box is 300 CSS pixels wide on any display: 1x={width_1x}, 2x={width_2x}"
    );
    assert_eq!(
        height_1x, height_2x,
        "the same text in the same box wraps to the same height on any display, \
         so a mismatch means the editor was handed a width or read a height in \
         device pixels: 1x={height_1x}, 2x={height_2x}"
    );
}

#[test]
fn a_textarea_reports_a_smaller_height_when_its_text_shrinks() {
    // The other half of autosizing. A field that grows must also come back
    // down, and it cannot if the measurement it is driven by is floored at the
    // size it currently has: that makes the height a high-water mark, so
    // clearing the text leaves the box several lines tall with nothing in it.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          body { margin: 0; width: 400px; font: 16px monospace }
          textarea {
            width: 300px; border: 0; padding: 0;
            font: 16px monospace; line-height: 20px;
            overflow-y: auto; overflow-wrap: anywhere;
          }
        </style>
        <textarea id="field" rows="1" wrap="soft"></textarea>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    // Drive it exactly as the composer does: measure, write the measurement
    // into the height, lay out, repeat. The height has to settle on the text,
    // and it has to come back down when the text does.
    doc.eval(
        r#"(() => {
             const field = document.getElementById("field");
             field.value = "j;alksjf;laskj;lkjs;flsajdslaj;slkfjs;lfkjj;".repeat(6);
           })()"#,
    );
    doc.inner_mut().resolve(0.0);
    doc.eval(
        r#"(() => {
             const field = document.getElementById("field");
             globalThis.grown = field.scrollHeight;
             field.style.height = globalThis.grown + "px";
           })()"#,
    );
    doc.inner_mut().resolve(0.0);

    doc.eval(r#"document.getElementById("field").value = "j";"#);
    doc.inner_mut().resolve(0.0);

    let sizes = doc
        .eval_json(
            r#"
            (() => {
              const field = document.getElementById("field");
              return { grown: globalThis.grown, shrunk: field.scrollHeight };
            })()
            "#,
        )
        .expect("textarea geometry should evaluate");

    let grown = sizes["grown"].as_f64().expect("grown");
    let shrunk = sizes["shrunk"].as_f64().expect("shrunk");

    assert!(
        grown > 60.0,
        "long text should measure several rows: {sizes}"
    );
    assert!(
        shrunk < grown / 2.0,
        "one character must not still measure the height of the old text: {sizes}"
    );
}

#[test]
fn a_textarea_wraps_its_text_and_reports_the_height_it_needs() {
    // Two halves of the same gap. The editor behind a textarea was built with
    // `set_width(None)` and never told the box's width, so `wrap="soft"` had
    // nothing to act on: a long line walked out past the right edge and became
    // invisible. And because the measured height was `rows * line-height`
    // regardless of content, `scrollHeight` never exceeded one row, so the
    // measure-and-grow idiom every autosizing composer uses could not grow it.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          body { margin: 0; width: 400px; font: 16px monospace }
          textarea {
            width: 300px;
            border: 0;
            padding: 0;
            font: 16px monospace;
            line-height: 20px;
            overflow-y: auto;
            /* What the composer sets, so an unbroken run of characters wraps
               rather than walking off the edge. */
            overflow-wrap: anywhere;
          }
        </style>
        <textarea id="field" rows="1" wrap="soft"></textarea>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let sizes = doc
        .eval_json(
            r#"
            const field = document.getElementById("field");
            const empty = field.scrollHeight;
            field.value = "j;alksjf;laskj;lkjs;flsajdslaj;slkjfs;lfkjj;".repeat(6);
            ({ empty, filled: field.scrollHeight, width: field.clientWidth })
            "#,
        )
        .expect("textarea geometry should evaluate");

    let empty = sizes["empty"].as_f64().expect("empty");
    let filled = sizes["filled"].as_f64().expect("filled");
    let width = sizes["width"].as_f64().expect("width");

    assert_eq!(
        width, 300.0,
        "the box keeps the width it was given: {sizes}"
    );
    assert!(empty > 0.0, "an empty field is still one row tall: {sizes}");
    assert!(
        filled >= empty * 3.0,
        "long text must wrap into several rows rather than run off the edge: {sizes}"
    );
}

#[test]
fn editing_an_existing_text_node_relays_out_its_container() {
    // The streaming path, which nothing else here covers.
    //
    // Solid does not replace a text node per token; it writes the new string
    // into the one already there, so `element.textContent = ...` is the wrong
    // route to test with. That takes the branch which removes children and
    // creates a fresh node. Writing `.data` on the text node itself is what a
    // streaming reply actually does, and it is the only thing that reaches
    // `BaseDocument::set_node_text`.
    //
    // Worth stating plainly because it was measured: before this test, that
    // function had zero coverage across the DOM suite, while carrying the
    // damage decision for every token the agent writes.
    let mut doc = ScriptDocument::from_html(
        r#"
        <style>
          body { margin: 0; width: 900px; font: 16px monospace }
          #column { display: flex; flex-direction: column }
          #bubble { display: flex; flex-direction: column; align-self: flex-start; max-width: 88% }
        </style>
        <div style="width: 900px"><div id="column"><div id="bubble">short</div></div></div>
        "#,
        DocumentConfig {
            viewport: Some(Viewport::new(900, 600, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        },
    );
    doc.execute_scripts();
    doc.inner_mut().resolve(0.0);

    let sizes = doc
        .eval_json(
            r#"
            const bubble = document.getElementById("bubble");
            const node = bubble.firstChild;
            const before = bubble.clientWidth;
            const kind = node.nodeType;
            node.data = "short and then a good deal more text arrived here";
            ({ before, kind, width: bubble.clientWidth, scroll: bubble.scrollWidth })
            "#,
        )
        .expect("bubble geometry should evaluate");

    assert_eq!(
        sizes["kind"].as_f64(),
        Some(3.0),
        "the edit must land on a text node, or it takes a different path: {sizes}"
    );
    let before = sizes["before"].as_f64().expect("before");
    let width = sizes["width"].as_f64().expect("width");
    let scroll = sizes["scroll"].as_f64().expect("scroll");
    assert!(
        before > 0.0,
        "the box sizes itself to its first text: {sizes}"
    );
    assert!(
        width > before,
        "editing the text node in place must relay out its container: {sizes}"
    );
    assert!(
        scroll <= width + 0.5,
        "text must not overflow the box that sizes itself to it: {sizes}"
    );
}
