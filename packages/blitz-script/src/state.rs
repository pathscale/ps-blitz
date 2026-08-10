//! Shared state accessible from both the Rust side (`ScriptDocument`) and the
//! JavaScript side (native functions registered with the Boa `Context`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use blitz_dom::BaseDocument;
use boa_engine::object::JsObject;
use boa_engine::{Finalize, JsData, Trace};
use rustc_hash::FxHashMap;

use crate::timers::TimerQueue;

/// Prototype objects for the DOM wrapper classes
pub(crate) struct DomProtos {
    pub node: JsObject,
    pub element: JsObject,
    pub character_data: JsObject,
    pub document: JsObject,
    pub event: JsObject,
    pub style: JsObject,
}

/// An event listener registered via `addEventListener`
#[derive(Clone)]
pub(crate) struct Listener {
    pub callback: JsObject,
    pub capture: bool,
    pub once: bool,
}

pub(crate) type ListenerMap = HashMap<String, Vec<Listener>>;
pub(crate) type IpcHandler = std::rc::Rc<dyn Fn(String)>;

/// State owned by the script runtime but shared (via `Rc`) with the native
/// functions exposed to JavaScript.
///
/// Note: this struct stores Boa GC handles (`JsObject`s) in ordinary Rust
/// collections. That is sound because Boa GC handles held outside of the GC
/// heap act as roots (they keep their referents alive).
#[derive(Default)]
pub(crate) struct RuntimeState {
    /// Prototypes for DOM wrapper objects. Set once during runtime initialisation.
    pub protos: Option<DomProtos>,
    /// Cache of JS wrapper objects, keyed by node id.
    ///
    /// DOM wrappers must be cached so that a given DOM node is always represented
    /// by the *same* JS object: scripts rely on object identity (`===`) and on
    /// expando properties persisting across accesses.
    ///
    /// Keys are node ids the document allocated, not anything a page controls,
    /// so there is nothing here for HashDoS resistance to defend and the
    /// default SipHash is pure cost. This map is hit on every node touch, so
    /// that cost is paid thousands of times per mount.
    pub node_wrappers: FxHashMap<usize, JsObject>,
    /// Cache of `DOMStringMap` proxy objects returned by `Element.dataset`.
    pub dataset_wrappers: FxHashMap<usize, JsObject>,
    /// Cache of `DOMTokenList` objects returned by `Element.classList`.
    pub class_list_wrappers: FxHashMap<usize, JsObject>,
    /// Event listeners registered on nodes, keyed by node id then event type.
    pub node_listeners: FxHashMap<usize, ListenerMap>,
    /// Event listeners registered on `window`.
    pub window_listeners: ListenerMap,
    /// Active Pointer Events capture target, keyed by the web-facing pointer id.
    pub pointer_capture: FxHashMap<u64, usize>,
    /// Host callback backing `window.ipc.postMessage`, installed by an embedder.
    pub ipc_handler: Option<IpcHandler>,
    /// Pending timers (`setTimeout`/`setInterval`/`requestAnimationFrame`)
    pub timers: TimerQueue,
}

impl RuntimeState {
    pub fn protos(&self) -> &DomProtos {
        self.protos
            .as_ref()
            .expect("DOM prototypes not initialised")
    }
}

/// Cloneable handle to the document and the runtime state. This is stored as
/// host-defined data on the Boa [`Context`](boa_engine::Context) so that native
/// functions can access the DOM.
#[derive(Clone, Trace, Finalize, JsData)]
pub(crate) struct DomCtx {
    #[unsafe_ignore_trace]
    pub doc: Rc<RefCell<BaseDocument>>,
    #[unsafe_ignore_trace]
    pub state: Rc<RefCell<RuntimeState>>,
    /// Whether the DOM has been mutated since layout last ran.
    ///
    /// Browsers flush layout synchronously when script reads geometry, which
    /// is why `element.scrollHeight` immediately after an insertion returns
    /// the new height. Blitz read `final_layout` directly, so the same read
    /// returned the height from *before* the mutation. Code that measures,
    /// mutates, then re-measures to restore scroll position therefore did its
    /// arithmetic on stale numbers and put the viewport in the wrong place,
    /// which is what a reader sees as the view jumping.
    #[unsafe_ignore_trace]
    pub layout_dirty: Rc<std::cell::Cell<bool>>,
}

impl DomCtx {
    pub fn new(doc: Rc<RefCell<BaseDocument>>) -> Self {
        Self {
            doc,
            state: Rc::new(RefCell::new(RuntimeState::default())),
            layout_dirty: Rc::new(std::cell::Cell::new(true)),
        }
    }
}

impl DomCtx {
    /// Note that script changed the DOM, so the next geometry read flushes.
    pub fn mark_layout_dirty(&self) {
        self.layout_dirty.set(true);
    }

    /// Borrow the document for a mutation that can move layout.
    ///
    /// The dirty flag was originally set at each call site, and the call sites
    /// it reached were the tree ones. Attribute writes through the reflected
    /// properties (`className`, `id`), every inline style write, `nodeValue`
    /// and `remove()` all mutated without setting it, so the stale-geometry
    /// bug this flag exists to prevent was still reachable through them: set a
    /// class, read `offsetHeight`, get the height from before the change.
    ///
    /// Going through here means a mutation cannot forget. Node *creation* is
    /// deliberately not routed through it: a detached node changes no layout
    /// until it is inserted, and marking there would flush on the next read
    /// for nothing.
    pub fn mutate_doc(&self) -> std::cell::RefMut<'_, BaseDocument> {
        self.layout_dirty.set(true);
        self.doc.borrow_mut()
    }

    /// Bring layout up to date before script observes geometry.
    ///
    /// Only resolves when something actually changed: a reader that measures
    /// in a loop pays once, not once per element. Incremental layout makes the
    /// flush itself cheap, which is what makes doing this at all affordable.
    pub fn flush_layout(&self) {
        if !self.layout_dirty.replace(false) {
            return;
        }
        // Attributed separately from the read that triggered it. A geometry
        // read is nanoseconds; the resolve it forces is not, and rolling the
        // two together reports an accessor as expensive when what is expensive
        // is the mutation that preceded it. The count matters as much as the
        // total: a handler that measures, mutates and measures again pays this
        // once per cycle, and the cycles are what to remove.
        let _t = crate::script_stats::Timed::new("layout:flush_from_script");
        if let Ok(mut doc) = self.doc.try_borrow_mut() {
            doc.resolve(0.0);
        } else {
            // Already borrowed further up the stack, so a resolve here would
            // panic. Leave the flag set so the next read tries again rather
            // than silently serving stale geometry forever.
            self.layout_dirty.set(true);
        }
    }
}
