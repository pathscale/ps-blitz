//! A node removed from the document must stop existing, not just stop painting.
//!
//! `removeChild` detached rather than dropped, so that a JS wrapper holding the
//! node stayed valid. That is correct for a node script still references, and a
//! leak for every other one: the node stayed in the document for the rest of the
//! session, and every layout, hit-test and accessibility pass kept walking it.
//!
//! Measured on a real application after a few hours of use: 98,646 nodes where a
//! fresh window holds 635, 86,325 of them detached and boxless. A list that
//! pages its rows removes one subtree per row that scrolls out, so the growth is
//! unbounded and proportional to how much the reader scrolls. It showed up as a
//! composer that rendered wrongly and an inspector that timed out, and 241 QA
//! checks passed throughout, because each one asks about a control it can name
//! and the abandoned nodes are invisible.

use blitz_dom::Document as _;
use blitz_script::ScriptDocument;

/// Every node in the document, live or detached.
fn node_count(doc: &ScriptDocument) -> usize {
    doc.inner().tree().iter().count()
}

fn document(script: &str) -> ScriptDocument {
    let html = format!(
        r#"<html><body style="margin:0">
             <div id="list"></div>
             <script>{script}</script>
           </body></html>"#
    );
    let mut doc = ScriptDocument::from_html(&html, Default::default());
    doc.poll(None);
    doc.inner_mut().resolve(0.0);
    doc
}

#[test]
fn churning_rows_does_not_grow_the_document() {
    // Add a row and remove it, a hundred times over, holding no reference to
    // any of them. This is what a paged list does as its window slides.
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        for (let turn = 0; turn < 100; turn += 1) {
          const row = document.createElement('div');
          row.textContent = 'row ' + turn;
          list.appendChild(row);
          list.removeChild(row);
        }
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    // Collect first. The wrapper cache is weak, so a removed node is freed once
    // its wrapper is collected, and nothing here would otherwise trigger a
    // collection: measuring immediately after the churn reports nodes that are
    // unreachable but not yet swept, which is not the leak this is about.
    boa_gc::force_collect();
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let after = node_count(&doc);

    // A hundred rows, each a div plus its text, would be 200 abandoned nodes if
    // none were freed. The baseline document is a handful, so the bound is
    // generous and still nowhere near a leak.
    assert!(
        after < 40,
        "churning 100 rows left {after} nodes in the document; \
         removed nodes are not being freed"
    );
}

#[test]
fn a_removed_node_script_still_holds_stays_usable() {
    // The other half of the contract, and the reason the old code detached
    // rather than dropped: a node script kept a reference to has to keep
    // working after removal.
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        const kept = document.createElement('div');
        kept.id = 'kept';
        kept.textContent = 'before';
        list.appendChild(kept);
        list.removeChild(kept);
        // Reachable only through this closure variable now.
        kept.textContent = 'after';
        globalThis.__keptText = kept.textContent;
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let text = doc.eval_json("globalThis.__keptText").unwrap_or_default().to_string();
    assert!(
        text.contains("after"),
        "a removed node that script still holds must stay usable, got {text:?}"
    );
}
