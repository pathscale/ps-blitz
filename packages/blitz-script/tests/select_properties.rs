//! `HTMLSelectElement` and `HTMLOptionElement` properties.
//!
//! `HTMLSelectElement` existed only as an `instanceof` brand. `select.value`
//! fell through to the generic attribute read, and a select has no `value`
//! attribute, so every picker reported the empty string; there was no
//! `selectedIndex`, no `options`, and no `option.selected` at all. A
//! controlled component rendering a `<select>` from its own state could
//! neither read what it showed nor drive it.

use blitz_dom::{Document, DocumentConfig};
use blitz_script::ScriptDocument;

fn doc_from_html(html: &str) -> ScriptDocument {
    let mut doc = ScriptDocument::from_html(
        html,
        DocumentConfig {
            viewport: Some(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Light,
            )),
            ..Default::default()
        },
    );
    // Resolve before the scripts run: the live selectedness is created by
    // layout construction, so a script that reads a select before the first
    // resolve is reading the pre-construction fallback.
    doc.inner_mut().resolve(0.0);
    doc.execute_scripts();
    doc
}

fn text_of(doc: &ScriptDocument, selector: &str) -> String {
    let inner = doc.inner();
    let node_id = inner
        .query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no node matching {selector}"));
    inner.get_node(node_id).unwrap().text_content()
}

const PICKER: &str = r#"
    <select id="country">
        <option value="us">United States</option>
        <option value="fr" selected>France</option>
        <option value="jp">Japan</option>
    </select>
    <div id="out"></div>
"#;

fn run(script: &str) -> String {
    let doc = doc_from_html(&format!(
        "<html><body>{PICKER}<script>const out = document.getElementById(\"out\");{script}</script></body></html>"
    ));
    text_of(&doc, "#out")
}

#[test]
fn a_select_reports_the_selected_options_value() {
    assert_eq!(
        run(r#"out.textContent = document.getElementById("country").value;"#),
        "fr"
    );
}

#[test]
fn a_select_reports_its_selected_index() {
    assert_eq!(
        run(r#"out.textContent = String(document.getElementById("country").selectedIndex);"#),
        "1"
    );
}

#[test]
fn setting_value_moves_the_selection() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            select.value = "jp";
            out.textContent = select.value + ":" + select.selectedIndex;
        "#),
        "jp:2"
    );
}

#[test]
fn setting_a_value_no_option_carries_clears_the_selection() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            select.value = "nowhere";
            out.textContent = "[" + select.value + "]:" + select.selectedIndex;
        "#),
        "[]:-1",
        "a value no option carries deselects everything, and selectedIndex is then -1"
    );
}

#[test]
fn setting_selected_index_moves_the_selection() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            select.selectedIndex = 0;
            out.textContent = select.value;
        "#),
        "us"
    );
}

#[test]
fn options_lists_every_option_in_order() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            out.textContent = select.options.map((o) => o.value).join(",");
        "#),
        "us,fr,jp"
    );
}

#[test]
fn an_options_selected_property_reads_the_live_state() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            out.textContent = select.options.map((o) => o.selected).join(",");
        "#),
        "false,true,false"
    );
}

#[test]
fn setting_an_options_selected_property_moves_the_selection() {
    assert_eq!(
        run(r#"
            const select = document.getElementById("country");
            select.options[2].selected = true;
            out.textContent = select.value + ":" + select.options.map((o) => o.selected).join(",");
        "#),
        "jp:false,false,true",
        "selecting one option on a single select must clear the others"
    );
}
