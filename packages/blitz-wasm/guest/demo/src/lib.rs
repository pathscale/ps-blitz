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
//! # Why the count text node starts empty
//!
//! `text("")` copies nothing, and the effect's first run — on the flush at the
//! end of [`run`] — is what puts `0` there. So initial render and update are
//! the *same* code path, and a bug in the update path cannot hide behind a
//! correct-looking first paint.

use std::cell::Cell;

use blitz_wasm_guest::{Element, Error, Node, text};
use solid_rs::{create_effect, create_root, create_signal, flush};

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
