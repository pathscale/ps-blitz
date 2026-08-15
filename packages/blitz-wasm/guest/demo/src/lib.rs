//! A demo guest: builds a small page through the host imports and nothing
//! else.
//!
//! There is no JavaScript anywhere in this path. This module is compiled to
//! `wasm32-unknown-unknown`, instantiated by wasmi, and the tree it produces
//! is a real `blitz-dom` document that can be laid out.
//!
//! The page it builds:
//!
//! ```html
//! <div class="panel" id="root">
//!   <h1>Blitz</h1>
//!   <p class="row">one</p>
//!   <p class="row">two</p>
//!   <p class="row">three</p>
//! </div>
//! ```
//!
//! Three rows rather than one, because the interesting number is what the
//! *second* and *third* row cost: `class="row"` is interned once and is free
//! thereafter, which is the claim the host's counters are asserted against.

use std::cell::Cell;

use blitz_wasm_guest::{Atom, Element, Error, Node, text};

thread_local! {
    /// The first row's text node, kept so `update` can rewrite it.
    ///
    /// This is what a framework's reactive graph would hold: handles are
    /// stable for the life of the instance, so a guest that remembers one can
    /// come back to that node later without searching for it.
    static FIRST_ROW_TEXT: Cell<Option<Node>> = const { Cell::new(None) };
}

/// Build the page. Returns 0 on success, or the host status code that failed.
///
/// A status code rather than a trap, all the way out to the export: a trap
/// takes the instance down and the host learns nothing except that it died.
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
    match build() {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

fn build() -> Result<(), Error> {
    let panel = Element::new("div")?;
    panel.set_attribute("class", "panel")?;
    panel.set_attribute("id", "root")?;

    let heading = Element::new("h1")?;
    heading.append(text("Blitz")?)?;
    panel.append(heading.node())?;

    // Hoist the attribute name and value out of the loop. Both are interned on
    // the first `Atom::intern` and reused after, so the three `set_attribute`
    // calls below copy nothing at all.
    let class = Atom::intern("class")?;
    let row = Atom::intern("row")?;

    for label in ["one", "two", "three"] {
        let p = Element::new("p")?;
        p.set_attribute_atoms(class, row)?;
        let content = text(label)?;
        p.append(content)?;
        panel.append(p.node())?;
        if label == "one" {
            FIRST_ROW_TEXT.with(|slot| slot.set(Some(content)));
        }
    }

    Node::mount().append(panel.node())?;
    Ok(())
}

/// Rewrite the first row's text, exercising the update path rather than only
/// construction.
///
/// Returns 0, or the failing status code. `ERR_BAD_HANDLE` if `run` was never
/// called, which is the honest answer rather than a trap.
#[unsafe(no_mangle)]
pub extern "C" fn update() -> i32 {
    let Some(node) = FIRST_ROW_TEXT.with(|slot| slot.get()) else {
        return -1; // ERR_BAD_HANDLE: nothing was built, so there is nothing to update.
    };
    match node.set_text("rewritten") {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}
