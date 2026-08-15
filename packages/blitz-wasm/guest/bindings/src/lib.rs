//! Safe guest-side bindings for the `blitz-wasm` host imports.
//!
//! A guest writes
//!
//! ```ignore
//! let div = Element::new("div")?;
//! div.set_attribute("class", "panel")?;
//! Node::mount().append(div.node())?;
//! ```
//!
//! and never an `extern "C"` block, a raw pointer, or a status code. The
//! `unsafe` and the integer protocol live here, once.
//!
//! # Events
//!
//! [`Node::on`] registers a handler and keeps it on this side; the host is
//! given an id and never sees a function. The host calls back through a
//! `dispatch` export that this crate deliberately does not provide — see
//! [`run_listener`] for why that last step belongs to the guest that knows
//! what its framework considers "settled".
//!
//! # Atoms
//!
//! `Element::new` and `set_attribute` take `&str` and intern it for you, so
//! the ergonomic path is also the cheap one *after the first call*: a name
//! crosses the boundary once, at intern time, and the resulting atom is what
//! every later call carries. [`Atom::intern`] is public for a guest that wants
//! to hoist the interning out of a loop, which is what a real framework would
//! do at module init.
//!
//! Interning is memoised on this side too, so calling `set_attribute(node,
//! "class", ...)` a thousand times performs one host call for the name, not a
//! thousand.
//!
//! # Why this is not `no_std`
//!
//! A `no_std` guest on `wasm32-unknown-unknown` has no global allocator and no
//! panic handler, and this crate needs both: the atom memo allocates, and a
//! failed host call has to be reportable. Supplying them by hand would shrink
//! the module, and module size is not what is being measured here. Bytes
//! across the boundary is, and that number does not move.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

// The raw imports. Everything below this is what makes them safe to use.
// (A `///` comment here is dropped: rustdoc does not document extern blocks.)
#[link(wasm_import_module = "blitz")]
unsafe extern "C" {
    fn intern(ptr: u32, len: u32) -> i32;
    fn create_element(tag: i32) -> i32;
    fn create_text(ptr: u32, len: u32) -> i32;
    fn append_child(parent: i32, child: i32) -> i32;
    fn set_attribute(node: i32, name: i32, value: i32) -> i32;
    fn set_text(node: i32, ptr: u32, len: u32) -> i32;
    fn add_listener(node: i32, event: i32) -> i32;
    fn remove_listener(listener: i32) -> i32;
}

/// A host status code that was not a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl Error {
    /// The raw status, matching the constants in `blitz_wasm::status`.
    pub fn code(self) -> i32 {
        self.0
    }

    /// What went wrong, for a guest that wants to report it.
    pub fn as_str(self) -> &'static str {
        match self.0 {
            -1 => "bad handle",
            -2 => "bad atom",
            -3 => "bad memory range",
            -4 => "invalid utf-8",
            -5 => "dom error",
            -6 => "too many handles",
            -7 => "bad listener id",
            -8 => "too many listeners",
            _ => "unknown error",
        }
    }
}

/// The result of any host call.
pub type Result<T> = core::result::Result<T, Error>;

fn check(status: i32) -> Result<i32> {
    if status < 0 {
        Err(Error(status))
    } else {
        Ok(status)
    }
}

/// An interned name. Copies nothing when passed to a host function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Atom(i32);

// Guest-side memo, so repeated `set_attribute(.., "class", ..)` calls do not
// each pay for an intern. The host would return the same atom either way; this
// saves the crossing, which is the thing being measured.
//
// A guest module is single-threaded, so a `RefCell` in a `thread_local` is the
// whole synchronisation story. `static mut` would be less honest about the
// aliasing and no faster.
thread_local! {
    static ATOMS: RefCell<BTreeMap<String, Atom>> = const { RefCell::new(BTreeMap::new()) };
}

impl Atom {
    /// Intern a name, or return the atom already held for it.
    ///
    /// The first call for a given string crosses the boundary and copies
    /// `s.len()` bytes. Every later call for the same string crosses nothing.
    pub fn intern(s: &str) -> Result<Atom> {
        if let Some(atom) = ATOMS.with(|atoms| atoms.borrow().get(s).copied()) {
            return Ok(atom);
        }
        let raw = check(unsafe { intern(s.as_ptr() as u32, s.len() as u32) })?;
        let atom = Atom(raw);
        ATOMS.with(|atoms| {
            atoms.borrow_mut().insert(String::from(s), atom);
        });
        Ok(atom)
    }

    /// The raw atom id, for a guest passing it straight back to a host call.
    pub fn raw(self) -> i32 {
        self.0
    }
}

/// A handle to a node in the host's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node(i32);

impl Node {
    /// The mount point the host seeded this instance with.
    ///
    /// Every tree a guest builds has to be attached to something, and this is
    /// the only node it is given for free.
    pub const fn mount() -> Node {
        Node(0)
    }

    /// The raw handle.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// Append `child`, detaching it from any current parent first.
    pub fn append(self, child: Node) -> Result<()> {
        check(unsafe { append_child(self.0, child.0) })?;
        Ok(())
    }

    /// Replace this node's text content. On a text node it is an in-place
    /// rewrite; on an element it replaces the children with one text node.
    ///
    /// Copies `text.len()` bytes.
    pub fn set_text(self, text: &str) -> Result<()> {
        check(unsafe { set_text(self.0, text.as_ptr() as u32, text.len() as u32) })?;
        Ok(())
    }

    /// Listen for `event` on this node.
    ///
    /// The event name is interned, so registering `"click"` on a thousand rows
    /// copies five bytes once and nothing thereafter. `handler` stays on this
    /// side of the boundary: the host is given an id and never sees a
    /// function.
    ///
    /// The listener fires for events that reach this node by bubbling, not
    /// only for events targeted at it directly. Note the limitation the host's
    /// deferred dispatch imposes — see [`Listener`].
    pub fn on(self, event: &str, handler: impl FnMut() + 'static) -> Result<Listener> {
        let event = Atom::intern(event)?;
        let id = check(unsafe { add_listener(self.0, event.raw()) })? as u32;
        HANDLERS.with(|handlers| {
            handlers
                .borrow_mut()
                .insert(id, Rc::new(RefCell::new(Box::new(handler) as Handler)))
        });
        Ok(Listener(id))
    }
}

type Handler = Box<dyn FnMut()>;

// The guest half of the listener registry. The host holds `(node, event) ->
// id`; this holds `id -> closure`. Neither knows what the other stores, which
// is what keeps a function pointer from ever needing to cross the boundary.
//
// `Rc<RefCell<..>>` rather than the closure inline, so a handler can be *taken
// out* and run with no borrow of the map outstanding. A handler that registers
// or removes a listener while running is ordinary — the counter demo's does
// not, but a real framework's will — and with the map borrowed across the call
// that would panic. This is the guest-side mirror of the host's reentrancy
// rule, and it exists for the same reason.
thread_local! {
    static HANDLERS: RefCell<BTreeMap<u32, Rc<RefCell<Handler>>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// A registered event listener.
///
/// # The one thing this cannot do
///
/// A handler cannot cancel the event. The host queues listener ids during
/// propagation and calls the guest only after the `EventDriver` has finished
/// and released the document, so by the time a handler runs, propagation is
/// over and the default action has already happened. `preventDefault` and
/// `stopPropagation` have nothing left to prevent or stop, so the ABI does not
/// offer them rather than offering them broken. See the host's ABI.md,
/// "Deferred dispatch and what it gives up".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listener(u32);

impl Listener {
    /// The raw listener id, which is what the host calls `dispatch` with.
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Unregister. The handler is dropped on this side and the id becomes an
    /// error on the host's.
    pub fn remove(self) -> Result<()> {
        check(unsafe { remove_listener(self.0 as i32) })?;
        HANDLERS.with(|handlers| handlers.borrow_mut().remove(&self.0));
        Ok(())
    }
}

/// Run the handler registered for `listener_id`, or `-7` (`ERR_BAD_LISTENER`)
/// if this guest holds none.
///
/// # This is not the `dispatch` export
///
/// It is deliberately only half of it. The host's contract is that `dispatch`
/// *completes* the handler: it runs it **and** drains whatever the guest's
/// framework queued as a result, so that the DOM is settled before the host
/// takes the document back and asks for a frame. Only the guest knows what
/// that drain is — `solid_rs::flush()`, a microtask loop, nothing at all — and
/// the moment this crate picks one, the ABI stops being framework-neutral and
/// starts being that framework's ABI.
///
/// So the export lives in the guest that knows, and is three lines:
///
/// ```ignore
/// #[unsafe(no_mangle)]
/// pub extern "C" fn dispatch(listener_id: u32) -> i32 {
///     let status = blitz_wasm_guest::run_listener(listener_id);
///     solid_rs::flush();
///     status
/// }
/// ```
pub fn run_listener(listener_id: u32) -> i32 {
    // Cloned out under a borrow that ends on this line. Running the handler
    // with the map still borrowed would make "register a listener from a
    // handler" a panic instead of a feature.
    let handler = HANDLERS.with(|handlers| handlers.borrow().get(&listener_id).cloned());
    let Some(handler) = handler else {
        return -7; // ERR_BAD_LISTENER: the host knows an id this guest does not.
    };
    // The `Rc` keeps the closure alive even if the handler removes itself.
    (handler.borrow_mut())();
    0
}

/// An element, which is a [`Node`] with attribute operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element(Node);

impl Element {
    /// Create a detached element. The tag is interned.
    pub fn new(tag: &str) -> Result<Element> {
        let tag = Atom::intern(tag)?;
        let handle = check(unsafe { create_element(tag.raw()) })?;
        Ok(Element(Node(handle)))
    }

    /// This element as a plain node.
    pub fn node(self) -> Node {
        self.0
    }

    /// Set an attribute. Both the name and the value are interned, so this
    /// copies nothing once each has been seen before.
    ///
    /// Interning the *value* is right for a class list or an id, and wrong for
    /// a value drawn from an unbounded set: an atom is never released, so
    /// interning per-frame text would grow the host's table without bound.
    /// Use [`Node::set_text`] for content.
    pub fn set_attribute(self, name: &str, value: &str) -> Result<()> {
        let name = Atom::intern(name)?;
        let value = Atom::intern(value)?;
        self.set_attribute_atoms(name, value)
    }

    /// Set an attribute from atoms already in hand. This is the zero-copy
    /// path, with the interning hoisted out by the caller.
    pub fn set_attribute_atoms(self, name: Atom, value: Atom) -> Result<()> {
        check(unsafe { set_attribute(self.0.0, name.raw(), value.raw()) })?;
        Ok(())
    }

    /// Append a child to this element.
    pub fn append(self, child: Node) -> Result<()> {
        self.0.append(child)
    }

    /// Listen for `event` on this element. See [`Node::on`].
    pub fn on(self, event: &str, handler: impl FnMut() + 'static) -> Result<Listener> {
        self.0.on(event, handler)
    }
}

/// Create a detached text node. Copies `text.len()` bytes.
pub fn text(text: &str) -> Result<Node> {
    let handle = check(unsafe { create_text(text.as_ptr() as u32, text.len() as u32) })?;
    Ok(Node(handle))
}

/// Guest allocator exports.
///
/// The host does not call either of these today: with the current five
/// operations every string travels guest to host, so the guest allocates its
/// own and the host only reads. They are exported now because the first
/// operation that returns a string to the guest, or that asks the guest for a
/// buffer to write into, needs them to already exist and to have a settled
/// signature.
///
/// # Safety
///
/// `dealloc` must be called with exactly the `ptr` and `len` a previous
/// `alloc` returned.
pub mod exports {
    use std::alloc::{Layout, alloc as raw_alloc, dealloc as raw_dealloc};

    /// Allocate `len` bytes and return the pointer, or 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn alloc(len: u32) -> u32 {
        if len == 0 {
            return 0;
        }
        let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
            return 0;
        };
        unsafe { raw_alloc(layout) as u32 }
    }

    /// Free a block previously returned by [`alloc`].
    #[unsafe(no_mangle)]
    pub extern "C" fn dealloc(ptr: u32, len: u32) {
        if ptr == 0 || len == 0 {
            return;
        }
        let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
            return;
        };
        unsafe { raw_dealloc(ptr as *mut u8, layout) }
    }
}
