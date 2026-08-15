//! A demo guest: a counter, driven by a reactive core, with no JavaScript
//! anywhere in the path.
//!
//! The page it builds:
//!
//! ```html
//! <div class="counter">
//!   <button class="increment">+1</button>
//!   <span class="count">0</span>
//! </div>
//! ```
//!
//! and the chain one click runs through:
//!
//! ```text
//! click -> host queue -> guest dispatch -> signal write
//!       -> microtask drain -> effect -> set_text -> redraw
//! ```
//!
//! # Why the effect writes somewhere other than the button
//!
//! The count is rendered into the `<span>`, not into the `<button>` that was
//! clicked. If the clicked element were also the one that changed, a guest
//! that simply called `set_text` on its own event target would pass the same
//! test, and the reactive graph in the middle would be unproven. Routing the
//! click through a signal to a node the handler never mentions is what makes
//! the graph load-bearing rather than decorative.
//!
//! # The `echo` export
//!
//! [`run`] is the write direction. [`echo`] is the read direction: it reads
//! attributes and text back out of the host and writes what came back into the
//! document, verbatim, on nodes the host test reads for itself. It is a
//! separate export so that every number `run` produces stays exactly what it
//! was — the mounting measurement in ABI.md does not move because reads exist.
//!
//! # Why the count text node starts empty
//!
//! `text("")` copies nothing, and the effect's first run — on the flush at the
//! end of [`run`] — is what puts `0` there. So initial render and update are
//! the *same* code path, and a bug in the update path cannot hide behind a
//! correct-looking first paint.

use std::cell::Cell;

use blitz_wasm_guest::{Element, Error, Node, text};
use solid_rs::{create_effect, create_root, create_signal, flush};

thread_local! {
    /// The panel and the readout, kept so [`echo`] can read back off the tree
    /// [`run`] built.
    ///
    /// These are the guest's own copies of two handles, which are `i32`s. The
    /// guest is not holding a DOM reference here — there is no such thing to
    /// hold — it is holding two integers the host will validate again on every
    /// use.
    static PANEL: Cell<Option<Element>> = const { Cell::new(None) };
    static READOUT: Cell<Option<Element>> = const { Cell::new(None) };
}

/// Build the page and wire it up. Returns 0, or the host status code that
/// failed.
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
    panel.set_attribute("class", "counter")?;

    let button = Element::new("button")?;
    button.set_attribute("class", "increment")?;
    button.append(text("+1")?)?;
    panel.append(button.node())?;

    let readout = Element::new("span")?;
    readout.set_attribute("class", "count")?;
    // Empty: the effect owns this node's content from here on. See the module
    // docs — first paint and every update take the same path.
    let count_text = text("")?;
    readout.append(count_text)?;
    panel.append(readout.node())?;

    Node::mount().append(panel.node())?;

    PANEL.with(|slot| slot.set(Some(panel)));
    READOUT.with(|slot| slot.set(Some(readout)));

    // The reactive graph. `create_root` gives the effect an owner; without one
    // it has no lifetime and `solid_rs` refuses to build it.
    let wiring = create_root(|_| -> Result<(), Error> {
        let (count, set_count) = create_signal(0i32);

        // The click handler's entire job. It does not touch the DOM, does not
        // know which node shows the count, and does not know whether anything
        // shows it at all — it writes a number. Everything downstream is the
        // graph's problem, which is the property being demonstrated.
        //
        // `update` rather than `set(count.get() + 1)`: a `solid_rs` write
        // stages, so a read outside a tracking scope sees the *old* value
        // until the next flush. `update` derives from the pending value and is
        // therefore correct even for two writes between flushes.
        button.on("click", move || set_count.update(|current| current + 1))?;

        // compute tracks, apply writes. The split is `solid_rs`'s, and it is
        // the right shape here: the DOM write must not subscribe to anything,
        // or a future read of the DOM would make the effect depend on itself.
        create_effect(
            move |_| count.get(),
            move |value: &i32, _| {
                // An `Err` here is a host status code with nowhere to go: an
                // effect's return type is its cleanup, not a result. It is
                // recorded so `last_error` can report it rather than the
                // failure showing up only as text that never changed.
                if let Err(error) = count_text.set_text(itoa(*value).as_str()) {
                    LAST_ERROR.with(|slot| slot.set(error.code()));
                }
            },
        );
        Ok(())
    });
    wiring?;

    // The initial render. Effects are queued, not run eagerly, so without this
    // the span stays empty until the first click — which would make the first
    // click look like it set the count to 1 from nothing.
    flush();

    LAST_ERROR.with(|slot| match slot.get() {
        0 => Ok(()),
        code => Err(Error(code)),
    })
}

thread_local! {
    /// The last host status an effect saw. See the effect body for why it
    /// cannot simply return one.
    static LAST_ERROR: Cell<i32> = const { Cell::new(0) };
}

/// Read the tree back out of the host and put what came back into the DOM.
///
/// This is the read direction's end-to-end proof, and it is written to make a
/// *lying* implementation fail. The bytes this reads are written straight back
/// into the document, unformatted, on nodes the host test then reads
/// independently — so "the guest returned 0" is not what is being trusted. A
/// read that returned nothing would show up as an empty echo node, and a read
/// that returned the wrong bytes would show up as the wrong echo node.
///
/// The three outcomes that *cannot* be made DOM-visible without the guest
/// inventing bytes for them — absent, present-and-empty, and the two booleans —
/// are reported through the return code instead, one code each, so that a
/// failure names which check failed rather than saying "echo returned -1".
///
/// Returns 0 on success.
#[unsafe(no_mangle)]
pub extern "C" fn echo() -> i32 {
    match read_back() {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

/// Guest-side status codes for the checks that have no DOM to land in.
///
/// Below the host's range, which stops at -9, so a failure is never mistaken
/// for a host status. The host keeps the last one in
/// `Counters::last_guest_status`.
const ERR_HAS_SAID_NO: i32 = -100;
const ERR_HAS_SAID_YES: i32 = -101;
const ERR_NOT_ABSENT: i32 = -102;
const ERR_EMPTY_NOT_PRESENT: i32 = -103;
const ERR_NOT_BUILT: i32 = -104;
const ERR_CLASS_MISSING: i32 = -105;

fn read_back() -> Result<(), Error> {
    let (panel, readout) = match (
        PANEL.with(|slot| slot.get()),
        READOUT.with(|slot| slot.get()),
    ) {
        (Some(panel), Some(readout)) => (panel, readout),
        _ => return Err(Error(ERR_NOT_BUILT)),
    };

    // === The two string reads, echoed into the DOM verbatim. ===
    //
    // One buffer, reused across both reads. That is the shape a real framework
    // would use and the one the `_into` variants exist for: the buffer grows
    // once and every later read costs the host's copy and nothing of the
    // guest's.
    let mut buf = Vec::new();

    let echo_class = Element::new("span")?;
    echo_class.set_attribute("class", "echo-class")?;
    match readout.get_attribute_into("class", &mut buf)? {
        Some(len) => echo_class
            .node()
            .set_text(core::str::from_utf8(&buf[..len]).unwrap_or(""))?,
        // Written as its own case rather than folded into an `unwrap_or("")`:
        // an empty echo node has to mean "the attribute was empty", never "the
        // read came back with nothing and we papered over it".
        None => return Err(Error(ERR_CLASS_MISSING)),
    }
    Node::mount().append(echo_class.node())?;

    let echo_text = Element::new("span")?;
    echo_text.set_attribute("class", "echo-text")?;
    let len = panel.node().text_content_into(&mut buf)?;
    echo_text
        .node()
        .set_text(core::str::from_utf8(&buf[..len]).unwrap_or(""))?;
    Node::mount().append(echo_text.node())?;

    // === The outcomes with no bytes to show. ===

    if !readout.has_attribute("class")? {
        return Err(Error(ERR_HAS_SAID_NO));
    }
    if readout.has_attribute("id")? {
        return Err(Error(ERR_HAS_SAID_YES));
    }
    // Absent is not an error and not an empty string.
    if readout.get_attribute_into("id", &mut buf)?.is_some() {
        return Err(Error(ERR_NOT_ABSENT));
    }
    // And present-and-empty is not absent. Set it here so the host can assert
    // the same distinction from its own side of the boundary.
    readout.set_attribute("data-empty", "")?;
    match readout.get_attribute_into("data-empty", &mut buf)? {
        Some(0) => {}
        _ => return Err(Error(ERR_EMPTY_NOT_PRESENT)),
    }

    // === The value that does not fit. ===
    //
    // Read only if the host put one there, which is the realistic shape: a
    // guest does not know how long an attribute is before it asks. When it is
    // there it is longer than the bindings' first guess, so this one read is
    // two host calls and two host-side allocations of the whole value — the
    // failure mode of the chosen mechanism, exercised rather than described.
    if let Some(len) = readout.get_attribute_into("data-long", &mut buf)? {
        let echo_long = Element::new("span")?;
        echo_long.set_attribute("class", "echo-long")?;
        echo_long
            .node()
            .set_text(core::str::from_utf8(&buf[..len]).unwrap_or(""))?;
        Node::mount().append(echo_long.node())?;
    }

    Ok(())
}

/// The host's call-in for one listener.
///
/// This is the export the ABI names, and it lives here rather than in the
/// bindings because of what the host requires of it: it must run the handler
/// **and** leave the guest settled before returning. `solid_rs` stages writes
/// and queues effects, so "settled" here means `flush()` — but that is a fact
/// about this guest's framework, not about the ABI. A host that knew to call
/// `flush` would be a host that knows what a microtask is, and the boundary
/// would no longer be framework-neutral.
///
/// So the whole chain — handler, signal write, queue drain, effect, `set_text`
/// — happens inside this one call, and the host gets the document back with
/// the DOM already correct.
#[unsafe(no_mangle)]
pub extern "C" fn dispatch(listener_id: u32) -> i32 {
    let status = blitz_wasm_guest::run_listener(listener_id);
    if status < 0 {
        return status;
    }
    flush();
    LAST_ERROR.with(|slot| slot.replace(0))
}

/// Call the readers with arguments the host must reject, and report what it
/// said.
///
/// Returns 0 only if every deliberately-bad call came back with the exact
/// status the ABI promises. This is the guest half of "a forged handle is an
/// error return, never a trap": nothing else can exercise a host function's
/// validation, because the host's linker closures are not reachable from a
/// test.
#[unsafe(no_mangle)]
pub extern "C" fn probe_forged() -> i32 {
    const ERR_BAD_HANDLE: i32 = -1;
    const ERR_BAD_MEMORY: i32 = -3;

    // A handle this instance never issued. `from_raw` needs no `unsafe`
    // because forging one is not an escalation — see its documentation.
    let ghost = Element::from_raw(9_999);

    match ghost.get_attribute("class") {
        Err(error) if error.code() == ERR_BAD_HANDLE => {}
        _ => return -110,
    }
    match ghost.text_content() {
        Err(error) if error.code() == ERR_BAD_HANDLE => {}
        _ => return -111,
    }
    match ghost.has_attribute("class") {
        Err(error) if error.code() == ERR_BAD_HANDLE => {}
        _ => return -112,
    }

    // A buffer outside this module's linear memory. The safe bindings cannot
    // express this — they always pass a real `Vec`'s pointer — so the one
    // import needed for it is declared here rather than weakening the bindings
    // with a raw or `unsafe` reader that exists only for a test.
    #[link(wasm_import_module = "blitz")]
    unsafe extern "C" {
        fn get_attribute(node: i32, name: i32, out_ptr: u32, out_cap: u32) -> i32;
    }
    let readout = match READOUT.with(|slot| slot.get()) {
        Some(readout) => readout,
        None => return ERR_NOT_BUILT,
    };
    let class = match blitz_wasm_guest::Atom::intern("class") {
        Ok(class) => class,
        Err(error) => return error.code(),
    };
    // Far past the end of any plausible linear memory, and the `+ cap` does not
    // wrap, so this is rejected on the bound rather than on the arithmetic.
    let status = unsafe { get_attribute(readout.node().raw(), class.raw(), 0x7FFF_0000, 16) };
    if status != ERR_BAD_MEMORY {
        return -113;
    }

    0
}

/// Format a non-negative `i32` without `alloc::format!`.
///
/// `format!` pulls in `core::fmt`'s machinery, which is a meaningful share of
/// a module this small. The count is an integer and this is nine lines.
fn itoa(mut value: i32) -> Digits {
    if value == 0 {
        return Digits::new(b"0");
    }
    let negative = value < 0;
    let mut buffer = [0u8; 11];
    let mut end = buffer.len();
    while value != 0 {
        end -= 1;
        buffer[end] = b'0' + (value % 10).unsigned_abs() as u8;
        value /= 10;
    }
    if negative {
        end -= 1;
        buffer[end] = b'-';
    }
    Digits::new(&buffer[end..])
}

/// A stack buffer holding a formatted integer, so `itoa` allocates nothing.
struct Digits {
    buffer: [u8; 11],
    len: usize,
}

impl Digits {
    fn new(bytes: &[u8]) -> Digits {
        let mut buffer = [0u8; 11];
        buffer[..bytes.len()].copy_from_slice(bytes);
        Digits {
            buffer,
            len: bytes.len(),
        }
    }

    fn as_str(&self) -> &str {
        // Every byte written is an ASCII digit or `-`.
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("")
    }
}
