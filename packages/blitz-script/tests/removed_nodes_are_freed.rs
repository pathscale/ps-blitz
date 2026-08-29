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
    // Add a row and remove it, two thousand times over, holding no reference to
    // any of them. This is what a paged list does as its window slides.
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        for (let turn = 0; turn < 2000; turn += 1) {
          const row = document.createElement('div');
          row.textContent = 'row ' + turn;
          list.appendChild(row);
          list.removeChild(row);
        }
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    // Ordinary polling must bound the detached backlog itself. Requiring an
    // embedder or a test to force Boa's collector merely turns the permanent
    // leak into an application-specific one.
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let after = node_count(&doc);

    // Two thousand rows, each a div plus its text, would be 4000 abandoned
    // nodes if none were freed. The baseline document is a handful, so the
    // bound is generous and still three orders of magnitude below the leak.
    assert!(
        after < 40,
        "churning 2000 rows left {after} nodes in the document; \
         removed nodes are not being freed"
    );
}

#[test]
fn listeners_do_not_turn_removed_nodes_into_permanent_roots() {
    // A listener is owned by the node. It must keep working while script owns
    // that detached node, but the listener registry must not itself become an
    // external root after every other reference is gone. Real component trees
    // put listeners on nearly every button, so this distinction is the
    // difference between bounded navigation and retaining an entire Solid
    // owner graph on every remount.
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        for (let turn = 0; turn < 2000; turn += 1) {
          const row = document.createElement('button');
          row.addEventListener('click', () => {});
          list.appendChild(row);
          list.removeChild(row);
        }
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let after = node_count(&doc);
    assert!(
        after < 40,
        "churning listener-bearing rows left {after} nodes in the document; \
         the listener registry is incorrectly rooting detached nodes"
    );
}

#[test]
fn every_dom_removal_path_is_reclaimed() {
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        for (let turn = 0; turn < 500; turn += 1) {
          const byRemove = document.createElement('div');
          list.appendChild(byRemove);
          byRemove.remove();

          const byReplaceChild = document.createElement('div');
          list.appendChild(byReplaceChild);
          list.replaceChild(document.createElement('span'), byReplaceChild);

          const byReplaceChildren = document.createElement('div');
          list.replaceChildren(byReplaceChildren);

          const byTextContent = document.createElement('div');
          byTextContent.appendChild(document.createElement('button'));
          list.appendChild(byTextContent);
          byTextContent.textContent = '';
        }
        list.replaceChildren();
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let after = node_count(&doc);
    assert!(
        after < 40,
        "DOM removal APIs left {after} nodes in the document; one or more paths bypass reclamation"
    );
}

#[test]
fn a_listener_closure_capturing_its_node_is_not_an_external_root() {
    let mut doc = document(
        r#"
        const list = document.getElementById('list');
        for (let turn = 0; turn < 2000; turn += 1) {
          const row = document.createElement('button');
          row.addEventListener('click', () => row.textContent);
          list.appendChild(row);
          row.remove();
        }
        "#,
    );
    doc.poll(None);
    doc.inner_mut().resolve(0.0);

    let after = node_count(&doc);
    assert!(
        after < 40,
        "listener closures capturing their node left {after} nodes in the document"
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

    let text = doc
        .eval_json("globalThis.__keptText")
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("after"),
        "a removed node that script still holds must stay usable, got {text:?}"
    );
}

#[test]
fn a_held_detached_subtree_keeps_descendant_listeners_when_reinserted() {
    let mut doc = document(
        r#"
        globalThis.__detachedClicks = 0;
        (() => {
          const root = document.createElement('div');
          const child = document.createElement('button');
          child.addEventListener('click', () => { globalThis.__detachedClicks += 1; });
          root.appendChild(child);
          document.body.appendChild(root);
          root.remove();
          globalThis.__keptDetachedRoot = root;
        })();
        "#,
    );
    boa_gc::force_collect();
    doc.poll(None);

    let clicks = doc
        .eval_json(
            r#"
            document.body.appendChild(globalThis.__keptDetachedRoot);
            globalThis.__keptDetachedRoot.firstChild.dispatchEvent(new Event('click'));
            globalThis.__detachedClicks;
            "#,
        )
        .unwrap_or_default();
    assert_eq!(
        clicks.as_u64(),
        Some(1),
        "a listener on a held detached descendant must survive reinsertion"
    );
}
