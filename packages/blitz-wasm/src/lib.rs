//! A wasmi host binding over [`blitz_dom_api`], so a WebAssembly guest can
//! build and mutate a DOM with no JavaScript anywhere in the path.
//!
//! This is the sibling of `blitz-script`: both bind the same facade, and
//! neither depends on the other. Nothing here may import `blitz-script`, and
//! `blitz-dom-api` must never learn that a wasm runtime exists.
//!
//! Today `blitz-script` still talks to `blitz-dom` directly, so this crate is
//! in fact the facade's *first* consumer rather than its second. That matters
//! when reading ABI.md: anything awkward here is evidence about the facade,
//! not just about this binding.
//!
//! # The five operations, plus one, plus events
//!
//! `create_element`, `create_text`, `append_child`, `set_attribute` and
//! `set_text` are enough to build a page. `intern` is the sixth, and it is not
//! optional: every one of the five takes names as atoms, and nothing else in
//! the ABI produces an atom. `add_listener` and `remove_listener` are the
//! seventh and eighth, and they are what makes the page respond rather than
//! merely exist. See ABI.md.
//!
//! # The reentrancy rule
//!
//! **No document borrow is held across a call into the guest.**
//!
//! Enforced by construction rather than by discipline: [`Host`] *owns* the
//! `BaseDocument`, and a host function reaches it only through
//! `Caller::data_mut` for the duration of its own body. There is nowhere to
//! put a `&mut BaseDocument` that outlives a call, because this crate never
//! stores one.
//!
//! Event dispatch is where that rule had to be paid for, and [`events`] is
//! where it was paid: `EventHandler::handle_event` runs while the
//! `EventDriver` holds the document, so the guest is not called from there at
//! all. The handler queues listener ids; [`dispatch_dom_event`] drains the
//! queue afterwards, with the document owned again. The handler is not
//! *trusted* to avoid the guest — it is handed no `Store`, so it has no means
//! to reach one.
//!
//! Reading a guest string is where the rule is visible: [`read_string`]
//! borrows guest memory, copies out, and drops the borrow *before* the
//! document is touched, because the alternative does not compile.
//!
//! # What this binding owes the facade
//!
//! `blitz-dom-api` deliberately does not mark layout dirty, does not request a
//! redraw, and does not flush layout before a geometry read. Those are the
//! embedding's job and this crate is the embedding. None of the five
//! operations reads geometry, so the flush obligation does not bite yet; the
//! dirty flag does, and [`Host::mutated`] is where it is recorded. See
//! ABI.md, "Obligations this binding inherits".

pub mod counters;
pub mod events;
pub mod handles;
pub mod status;

use blitz_dom::{BaseDocument, NodeId};
use blitz_dom_api::{AtomId, DomError, Interner, document, element, node};
use wasmi::{Caller, Extern, Linker};

pub use counters::{Counters, Op, OpCounters};
pub use events::{Dispatched, ListenerId, ListenerTable, dispatch_dom_event};
pub use handles::{Handle, HandleTable, MOUNT};
pub use status::{
    ERR_BAD_ATOM, ERR_BAD_HANDLE, ERR_BAD_LISTENER, ERR_BAD_MEMORY, ERR_BAD_UTF8, ERR_DOM,
    ERR_TOO_MANY_HANDLES, ERR_TOO_MANY_LISTENERS, OK,
};

/// The import module name every host function is registered under.
pub const MODULE: &str = "blitz";

/// Everything one instance owns.
///
/// The document lives here, not behind a lock or a cell, which is what makes
/// the reentrancy rule structural: a host function can only reach it through
/// `Caller::data_mut`, and only for as long as it is running.
pub struct Host {
    doc: BaseDocument,
    names: Interner,
    handles: HandleTable,
    counters: Counters,
    mutated: bool,
    /// Registered event listeners. See [`events`].
    listeners: ListenerTable,
    /// Listener ids propagation matched but the guest has not been called for
    /// yet.
    ///
    /// This queue *is* the deferred-dispatch design. It exists so that
    /// `EventHandler::handle_event` — which runs with the document borrowed —
    /// has somewhere to put its answer that is not "call the guest".
    pending: Vec<ListenerId>,
    redraw_requested: bool,
}

impl Host {
    /// A host bound to `doc`, with the guest's mount point seeded as
    /// [`MOUNT`].
    ///
    /// The seed is not a convenience. All five operations either create a
    /// detached node or need one that already exists, so without a handle to
    /// start from a guest can build a tree and has nowhere to put it.
    pub fn new(doc: BaseDocument, mount: NodeId) -> Self {
        Self {
            doc,
            names: Interner::new(),
            handles: HandleTable::with_mount(mount),
            counters: Counters::default(),
            mutated: false,
            listeners: ListenerTable::default(),
            pending: Vec::new(),
            redraw_requested: false,
        }
    }

    /// The document, for a caller that wants to inspect or lay out the result.
    pub fn document(&self) -> &BaseDocument {
        &self.doc
    }

    /// The document, mutably. Taking this back is also how an embedder
    /// discharges the obligations the facade leaves to it.
    pub fn document_mut(&mut self) -> &mut BaseDocument {
        &mut self.doc
    }

    /// Give the document back.
    pub fn into_document(self) -> BaseDocument {
        self.doc
    }

    /// The instrumentation.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// The interner, so a host-side test can check what a guest interned.
    pub fn names(&self) -> &Interner {
        &self.names
    }

    /// The handle table, mainly so a test can resolve a handle to a node.
    pub fn handles(&self) -> &HandleTable {
        &self.handles
    }

    /// Whether the guest has mutated the document since this was last cleared.
    ///
    /// This is the facade's dirty flag, which `blitz-dom-api` deliberately
    /// does not keep (see its MAPPING.md). An embedder with a shell reads this
    /// to decide whether to resolve layout and ask for a frame; the end-to-end
    /// test reads it to prove the binding tracks it at all.
    pub fn mutated(&self) -> bool {
        self.mutated
    }

    /// Clear the dirty flag, after resolving layout.
    pub fn clear_mutated(&mut self) {
        self.mutated = false;
    }

    /// The listener table, mainly so a test can count what a guest registered.
    pub fn listeners(&self) -> &ListenerTable {
        &self.listeners
    }

    /// The listener table, mutably.
    ///
    /// Registering from here rather than from the guest is legitimate: an
    /// embedder with a host-side widget wants the same queue, and it is the
    /// same table either way. What it does *not* get is a handler — the guest
    /// is still the only thing `dispatch` can reach, so an id registered here
    /// that the guest does not know is a reported failure and not a trap.
    pub fn listeners_mut(&mut self) -> &mut ListenerTable {
        &mut self.listeners
    }

    /// Whether a frame has been asked for since this was last cleared.
    ///
    /// The second obligation `blitz-dom-api` leaves to its caller (see
    /// ABI.md). There is no shell here, so a request is recorded rather than
    /// sent; an embedder with one reads this after
    /// [`dispatch_dom_event`] and asks its window for the frame.
    pub fn redraw_requested(&self) -> bool {
        self.redraw_requested
    }

    /// Clear the redraw request, after asking for the frame.
    pub fn clear_redraw_request(&mut self) {
        self.redraw_requested = false;
    }

    fn atom(&mut self, raw: i32) -> Result<AtomId, i32> {
        let raw = u32::try_from(raw).map_err(|_| ERR_BAD_ATOM)?;
        let atom = AtomId::from_u32(raw);
        // Validate against this instance's interner. An atom minted elsewhere
        // is the same class of forgery as a bad handle.
        self.names
            .resolve(atom)
            .map(|_| atom)
            .map_err(|_| ERR_BAD_ATOM)
    }

    fn fail(&mut self, status: i32) -> i32 {
        self.counters.record_error(status);
        status
    }

    fn fail_dom(&mut self, error: DomError) -> i32 {
        self.counters.record_dom_error(error);
        ERR_DOM
    }
}

/// Copy a UTF-8 string out of guest linear memory.
///
/// This function is where the reentrancy rule is enforced rather than merely
/// stated: the borrow of guest memory ends when this returns, so the caller
/// holds an owned `String` and nothing else by the time it touches the
/// document. Writing it the other way round does not compile.
fn read_string(caller: &Caller<'_, Host>, ptr: i32, len: i32) -> Result<String, i32> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(ERR_BAD_MEMORY)?;
    let start = usize::try_from(ptr).map_err(|_| ERR_BAD_MEMORY)?;
    let len = usize::try_from(len).map_err(|_| ERR_BAD_MEMORY)?;
    let end = start.checked_add(len).ok_or(ERR_BAD_MEMORY)?;

    let data = memory.data(caller);
    let bytes = data.get(start..end).ok_or(ERR_BAD_MEMORY)?;
    core::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ERR_BAD_UTF8)
}

/// Register every host function on `linker`, under the [`MODULE`] name.
pub fn add_to_linker(linker: &mut Linker<Host>) -> Result<(), wasmi::Error> {
    // === intern(ptr, len) -> atom | error ===
    //
    // The one place a name crosses the boundary. Every other operation takes
    // the atom this returns, so a name is copied exactly once no matter how
    // many times it is used afterwards. That amortisation is the whole claim,
    // and this is the function that has to be paid for it.
    linker.func_wrap(
        MODULE,
        "intern",
        |mut caller: Caller<'_, Host>, ptr: i32, len: i32| -> i32 {
            caller.data_mut().counters.record_call(Op::Intern);
            let text = match read_string(&caller, ptr, len) {
                Ok(text) => text,
                Err(status) => return caller.data_mut().fail(status),
            };
            let host = caller.data_mut();
            host.counters.record_copy(Op::Intern, text.len());
            let atom = host.names.intern(&text);
            match i32::try_from(atom.to_u32()) {
                Ok(raw) => raw,
                Err(_) => host.fail(ERR_BAD_ATOM),
            }
        },
    )?;

    // === create_element(tag_atom) -> handle | error ===
    //
    // Zero bytes copied: the tag is an atom.
    linker.func_wrap(
        MODULE,
        "create_element",
        |mut caller: Caller<'_, Host>, tag: i32| -> i32 {
            let host = caller.data_mut();
            host.counters.record_call(Op::CreateElement);
            let atom = match host.atom(tag) {
                Ok(atom) => atom,
                Err(status) => return host.fail(status),
            };
            // Destructured so the interned name can be passed to the facade as
            // a borrow rather than copied into a fresh `String`. `names` and
            // `doc` are disjoint fields, so the borrow checker allows one
            // immutably and the other mutably at the same time; going through
            // `host.` for both would not compile, and the obvious fix,
            // `.to_owned()`, would put an allocation back into the operation
            // this whole design exists to make allocation-free.
            let Host {
                doc,
                names,
                handles,
                counters,
                ..
            } = host;
            let Ok(tag) = names.resolve(atom) else {
                counters.record_error(ERR_BAD_ATOM);
                return ERR_BAD_ATOM;
            };
            let node_id = match document::create_element(doc, tag) {
                Ok(node_id) => node_id,
                Err(error) => {
                    counters.record_dom_error(error);
                    return ERR_DOM;
                }
            };
            match handles.insert(node_id) {
                Ok(handle) => handle as i32,
                Err(status) => {
                    counters.record_error(status);
                    status
                }
            }
        },
    )?;

    // === create_text(ptr, len) -> handle | error ===
    //
    // Copies. Text is the content of a page, not a name from a small fixed
    // vocabulary, so interning it would grow the table without bound and save
    // nothing.
    linker.func_wrap(
        MODULE,
        "create_text",
        |mut caller: Caller<'_, Host>, ptr: i32, len: i32| -> i32 {
            caller.data_mut().counters.record_call(Op::CreateText);
            let text = match read_string(&caller, ptr, len) {
                Ok(text) => text,
                Err(status) => return caller.data_mut().fail(status),
            };
            let host = caller.data_mut();
            host.counters.record_copy(Op::CreateText, text.len());
            let node_id = match document::create_text_node(&mut host.doc, &text) {
                Ok(node_id) => node_id,
                Err(error) => return host.fail_dom(error),
            };
            match host.handles.insert(node_id) {
                Ok(handle) => handle as i32,
                Err(status) => host.fail(status),
            }
        },
    )?;

    // === append_child(parent, child) -> OK | error ===
    //
    // Zero bytes copied: both arguments are handles.
    linker.func_wrap(
        MODULE,
        "append_child",
        |mut caller: Caller<'_, Host>, parent: i32, child: i32| -> i32 {
            let host = caller.data_mut();
            host.counters.record_call(Op::AppendChild);
            let (parent, child) = match (
                u32::try_from(parent).map_err(|_| ERR_BAD_HANDLE),
                u32::try_from(child).map_err(|_| ERR_BAD_HANDLE),
            ) {
                (Ok(parent), Ok(child)) => (parent, child),
                _ => return host.fail(ERR_BAD_HANDLE),
            };
            let parent_id = match host.handles.get(parent) {
                Ok(id) => id,
                Err(status) => return host.fail(status),
            };
            let child_id = match host.handles.get(child) {
                Ok(id) => id,
                Err(status) => return host.fail(status),
            };
            match node::append_child(&mut host.doc, parent_id, child_id) {
                Ok(_) => {
                    host.mutated = true;
                    OK
                }
                Err(error) => host.fail_dom(error),
            }
        },
    )?;

    // === set_attribute(node, name_atom, value_atom) -> OK | error ===
    //
    // The thesis, in one import: a handle and two atoms, so nothing crosses
    // the boundary at all. See the end-to-end test's counter assertion.
    linker.func_wrap(
        MODULE,
        "set_attribute",
        |mut caller: Caller<'_, Host>, node: i32, name: i32, value: i32| -> i32 {
            let host = caller.data_mut();
            host.counters.record_call(Op::SetAttribute);
            let handle = match u32::try_from(node) {
                Ok(handle) => handle,
                Err(_) => return host.fail(ERR_BAD_HANDLE),
            };
            let node_id = match host.handles.get(handle) {
                Ok(id) => id,
                Err(status) => return host.fail(status),
            };
            let name_atom = match host.atom(name) {
                Ok(atom) => atom,
                Err(status) => return host.fail(status),
            };
            let value_atom = match host.atom(value) {
                Ok(atom) => atom,
                Err(status) => return host.fail(status),
            };
            // Borrowed out of the interner, not copied into fresh `String`s.
            // See the note in `create_element`: this is the difference between
            // "zero bytes crossed the boundary" and "zero bytes crossed and
            // nothing was allocated either", and only the second one is worth
            // claiming.
            let Host {
                doc,
                names,
                counters,
                mutated,
                ..
            } = host;
            let (Ok(name), Ok(value)) = (names.resolve(name_atom), names.resolve(value_atom))
            else {
                counters.record_error(ERR_BAD_ATOM);
                return ERR_BAD_ATOM;
            };
            match element::set_attribute(doc, node_id, name, value) {
                Ok(()) => {
                    *mutated = true;
                    OK
                }
                Err(error) => {
                    counters.record_dom_error(error);
                    ERR_DOM
                }
            }
        },
    )?;

    // === set_text(node, ptr, len) -> OK | error ===
    //
    // Copies, for the same reason `create_text` does. Maps onto
    // `node::set_text_content`, which rewrites a text node in place and
    // replaces an element's children with a single text node, so one import
    // covers both the update and the "empty this and put a string in it" case.
    linker.func_wrap(
        MODULE,
        "set_text",
        |mut caller: Caller<'_, Host>, node: i32, ptr: i32, len: i32| -> i32 {
            caller.data_mut().counters.record_call(Op::SetText);
            let text = match read_string(&caller, ptr, len) {
                Ok(text) => text,
                Err(status) => return caller.data_mut().fail(status),
            };
            let host = caller.data_mut();
            host.counters.record_copy(Op::SetText, text.len());
            let handle = match u32::try_from(node) {
                Ok(handle) => handle,
                Err(_) => return host.fail(ERR_BAD_HANDLE),
            };
            let node_id = match host.handles.get(handle) {
                Ok(id) => id,
                Err(status) => return host.fail(status),
            };
            match node::set_text_content(&mut host.doc, node_id, &text) {
                Ok(()) => {
                    host.mutated = true;
                    OK
                }
                Err(error) => host.fail_dom(error),
            }
        },
    )?;

    // === add_listener(node, event_atom) -> listener_id | error ===
    //
    // Zero bytes copied: a handle and an atom, exactly like `set_attribute`.
    // The event name is interned once and is an integer for the life of the
    // instance, which is the same trade tag names get and for the same reason:
    // "click" is drawn from a small fixed vocabulary.
    //
    // Registering a listener does not mutate the document. `mutated` stays
    // where it is, because nothing about the tree or its styles has changed
    // and a frame drawn now would be identical to the last one.
    linker.func_wrap(
        MODULE,
        "add_listener",
        |mut caller: Caller<'_, Host>, node: i32, event: i32| -> i32 {
            let host = caller.data_mut();
            host.counters.record_call(Op::AddListener);
            let handle = match u32::try_from(node) {
                Ok(handle) => handle,
                Err(_) => return host.fail(ERR_BAD_HANDLE),
            };
            let node_id = match host.handles.get(handle) {
                Ok(id) => id,
                Err(status) => return host.fail(status),
            };
            let event = match host.atom(event) {
                Ok(atom) => atom,
                Err(status) => return host.fail(status),
            };
            match host.listeners.add(node_id, event) {
                Ok(id) => match i32::try_from(id) {
                    Ok(raw) => raw,
                    Err(_) => host.fail(ERR_TOO_MANY_LISTENERS),
                },
                Err(status) => host.fail(status),
            }
        },
    )?;

    // === remove_listener(listener_id) -> OK | error ===
    //
    // Zero bytes copied. A listener id is not a handle: it indexes the
    // listener table, not the node table, and passing one where the other
    // belongs is `ERR_BAD_LISTENER` rather than a silent hit on an unrelated
    // node.
    linker.func_wrap(
        MODULE,
        "remove_listener",
        |mut caller: Caller<'_, Host>, listener: i32| -> i32 {
            let host = caller.data_mut();
            host.counters.record_call(Op::RemoveListener);
            let id = match u32::try_from(listener) {
                Ok(id) => id,
                Err(_) => return host.fail(ERR_BAD_LISTENER),
            };
            match host.listeners.remove(id) {
                Ok(()) => OK,
                Err(status) => host.fail(status),
            }
        },
    )?;

    Ok(())
}
