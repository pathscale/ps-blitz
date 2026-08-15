//! Event dispatch, deferred.
//!
//! # The seam
//!
//! This mirrors `blitz-script` exactly, and deliberately: that crate does not
//! extend the engine to get events either. It keeps its own listener registry
//! and implements [`EventHandler`], which [`EventDriver`] calls during
//! propagation. Nothing in `blitz-dom` or `blitz-traits` had to change for
//! either of us.
//!
//! # Why dispatch is deferred
//!
//! [`EventHandler::handle_event`] runs *while the `EventDriver` holds the
//! document*. Calling the guest from inside it would break the rule this crate
//! enforces structurally — no document borrow is held across a call into the
//! guest — and it would not be a theoretical break: a guest's first act on a
//! click is to mutate the DOM, which needs the document the driver is holding.
//!
//! So the guest is not called from there at all. Instead:
//!
//! 1. [`WasmEventHandler::handle_event`] does one thing: pushes the matching
//!    listener ids onto [`Host::pending`]. It calls no guest code and returns
//!    immediately. Its capabilities are, by construction, "read the registry,
//!    push a `u32`" — there is no `Store` in scope, so it *could not* call the
//!    guest even if it wanted to.
//! 2. The driver finishes propagation and default actions, then drops, and the
//!    document borrow ends with it.
//! 3. Only then does [`dispatch_dom_event`] drain the queue, calling the
//!    guest's `dispatch` export once per listener with the document owned
//!    again.
//! 4. A redraw is requested once, after the queue is empty.
//!
//! The three phases are three separate statements in one function, and phase 1
//! is inside a block so that its borrow *cannot* reach phase 2. That is the
//! same trick `read_string` uses in the parent module: make the rule a thing
//! the compiler checks rather than a thing a reviewer remembers.
//!
//! # What this costs
//!
//! A guest handler cannot `preventDefault` or `stopPropagation`. By the time
//! it runs, propagation is over and the default action has already happened.
//! See ABI.md, "Deferred dispatch and what it gives up" — this is a known
//! deviation from the DOM, taken knowingly, and the price of the reentrancy
//! guarantee above.

use std::collections::HashMap;

use blitz_dom::{Document, EventDriver, EventHandler, NodeId};
use blitz_dom_api::{AtomId, Interner};
use blitz_traits::events::{DomEvent, EventState};
use wasmi::{Instance, Store};

use crate::Host;
use crate::counters::Op;
use crate::status::{ERR_BAD_LISTENER, ERR_TOO_MANY_LISTENERS};

/// A registered listener, as the guest names it.
///
/// The guest never learns which node or event a listener id belongs to. It
/// gave both, so it already knows; handing them back would be a second way to
/// address a node, and one addressing scheme is the whole point of
/// [`HandleTable`](crate::HandleTable).
pub type ListenerId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Listener {
    node: NodeId,
    event: AtomId,
}

/// Every listener one instance has registered.
///
/// Two structures over the same set, because the two access patterns are
/// different shapes. `slots` answers "does this id still exist", which is what
/// the drain does per listener. `by_node` answers "what is registered here",
/// which is what propagation does per node in the chain — and a chain is
/// walked on every event, so scanning every listener in the instance to answer
/// it would make a deep tree quadratic in listeners.
#[derive(Debug, Clone, Default)]
pub struct ListenerTable {
    /// `id -> listener`, with `None` for a removed one. Ids are never reused,
    /// for the same reason handles are not: a stale id must be an error, not a
    /// silent hit on whatever took its place.
    slots: Vec<Option<Listener>>,
    /// `node -> ids registered on it, in registration order`.
    by_node: HashMap<NodeId, Vec<ListenerId>>,
}

impl ListenerTable {
    /// Register `event` on `node` and return the id the guest will be called
    /// back with.
    pub fn add(&mut self, node: NodeId, event: AtomId) -> Result<ListenerId, i32> {
        let id = ListenerId::try_from(self.slots.len()).map_err(|_| ERR_TOO_MANY_LISTENERS)?;
        if id > i32::MAX as ListenerId {
            return Err(ERR_TOO_MANY_LISTENERS);
        }
        self.slots.push(Some(Listener { node, event }));
        self.by_node.entry(node).or_default().push(id);
        Ok(id)
    }

    /// Unregister a listener. A second removal of the same id is
    /// [`ERR_BAD_LISTENER`], not a silent success: a guest that double-removes
    /// has a bug and should hear about it.
    pub fn remove(&mut self, id: ListenerId) -> Result<(), i32> {
        let slot = self
            .slots
            .get_mut(id as usize)
            .ok_or(ERR_BAD_LISTENER)?
            .take()
            .ok_or(ERR_BAD_LISTENER)?;
        if let Some(ids) = self.by_node.get_mut(&slot.node) {
            ids.retain(|candidate| *candidate != id);
            if ids.is_empty() {
                self.by_node.remove(&slot.node);
            }
        }
        Ok(())
    }

    /// Whether `id` names a listener that is still registered.
    ///
    /// Checked again at drain time, not only at queue time: a guest handler
    /// may remove a listener that is already queued behind it, and a listener
    /// removed before it runs must not run. That is the one piece of DOM
    /// listener semantics deferred dispatch can still honour, so it does.
    pub fn is_live(&self, id: ListenerId) -> bool {
        matches!(self.slots.get(id as usize), Some(Some(_)))
    }

    /// The listeners registered on `node` for `event`, in registration order.
    fn matching(&self, node: NodeId, event: AtomId) -> impl Iterator<Item = ListenerId> + '_ {
        self.by_node
            .get(&node)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(move |id| {
                self.slots
                    .get(*id as usize)
                    .and_then(|slot| slot.as_ref())
                    .is_some_and(|listener| listener.event == event)
            })
    }

    /// How many listeners are still registered.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Whether no listener is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The [`EventHandler`] the driver calls during propagation.
///
/// It borrows three disjoint fields of [`Host`] and *not* the document, which
/// the driver has. There is deliberately no way to reach the guest from here:
/// no `Store`, no [`Instance`], nothing that could call an export. The
/// reentrancy rule is not enforced by this type remembering to behave, it is
/// enforced by this type not having been given the means to misbehave.
struct WasmEventHandler<'a> {
    names: &'a Interner,
    listeners: &'a ListenerTable,
    pending: &'a mut Vec<ListenerId>,
}

impl EventHandler for WasmEventHandler<'_> {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        _doc: &mut dyn Document,
        _event_state: &mut EventState,
    ) {
        // A listener's event is an atom, so an event name this instance has
        // never interned cannot have a listener: no `add_listener` could have
        // named it. That makes the miss free rather than a walk of the chain.
        let Some(event) = self.names.get(event.name()) else {
            return;
        };

        // `chain` is target-first, ancestors after, which is bubble order.
        // Capture-phase listeners would be the same walk reversed; the ABI has
        // no capture flag today, so there is one order and this is it.
        for node in chain {
            self.pending.extend(self.listeners.matching(*node, event));
        }
    }
}

/// What one [`dispatch_dom_event`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dispatched {
    /// Listeners propagation matched.
    pub queued: usize,
    /// Listeners the guest was actually called for. Lower than `queued` when a
    /// handler removed a listener queued behind it.
    pub ran: usize,
    /// Calls where the guest returned a negative status. The guest's own last
    /// status is in [`Counters::last_guest_status`](crate::Counters).
    pub failed: usize,
}

/// Deliver `event`, then run whatever listeners it matched.
///
/// The two happen in that order and never interleave — see the module docs for
/// why that is the whole design rather than an implementation detail. The
/// guest export called is `dispatch(listener_id: u32) -> i32`, once per
/// listener, and it is the guest's job to make that call *complete*: run the
/// handler and drain its own microtask queue before returning. The host does
/// not know what a microtask is, and must not learn.
///
/// Errors are `wasmi` errors only: a missing or mistyped `dispatch` export, or
/// a guest trap. A guest that merely returns a bad status is counted in
/// [`Dispatched::failed`], because a status is a report and a trap is a death.
pub fn dispatch_dom_event(
    store: &mut Store<Host>,
    instance: &Instance,
    event: DomEvent,
) -> Result<Dispatched, wasmi::Error> {
    // === Phase 1: propagation. ===
    //
    // Scoped, so the borrow of the document ends at the closing brace and
    // cannot reach phase 2. Nothing in here can call the guest.
    {
        let Host {
            doc,
            names,
            listeners,
            pending,
            ..
        } = store.data_mut();
        let handler = WasmEventHandler {
            names,
            listeners,
            pending,
        };
        EventDriver::new(doc, handler).handle_dom_event(event);
    }

    // === Phase 2: the guest. ===
    //
    // The document is the host's again, so a handler may mutate it. Taken out
    // of the host rather than iterated in place: a handler can register a
    // listener, and a listener registered by a handler must not fire for the
    // event that is already being delivered.
    let queued = std::mem::take(&mut store.data_mut().pending);
    let mut result = Dispatched {
        queued: queued.len(),
        ..Default::default()
    };

    if !queued.is_empty() {
        let dispatch = instance.get_typed_func::<u32, i32>(&*store, "dispatch")?;
        for id in queued {
            // Re-checked, not assumed: a handler already run may have removed
            // a listener still sitting in this queue behind it.
            if !store.data().listeners.is_live(id) {
                continue;
            }
            store.data_mut().counters.record_call(Op::Dispatch);
            let status = dispatch.call(&mut *store, id)?;
            result.ran += 1;
            if status < 0 {
                result.failed += 1;
                store.data_mut().counters.last_guest_status = Some(status);
            }
        }
    }

    // === Phase 3: the frame. ===
    //
    // Once, after the queue is empty, rather than once per listener: three
    // handlers responding to one click are one frame's worth of work.
    //
    // Requested whenever a listener ran, which is deliberately coarser than
    // `mutated()`. A handler that changed nothing costs a redundant frame; a
    // handler whose change is not drawn costs the user a stale screen, and
    // those two are not the same size of mistake.
    if result.ran > 0 {
        store.data_mut().redraw_requested = true;
    }

    Ok(result)
}
