//! Per-operation instrumentation.
//!
//! Three numbers per operation, and deliberately no timing: a timing number
//! measured on one machine, in one build profile, against an interpreter is
//! not evidence of anything, whereas a byte count is the same on every machine
//! and is exactly what the boundary design is meant to change.
//!
//! # The two directions
//!
//! Payload bytes travel one way or the other, never both, and the two are
//! counted in separate fields so that no total can quietly mix them:
//!
//! - [`OpCounters::bytes_copied`] — **guest to host.** Bytes the host read
//!   *out of* guest linear memory. `intern`, `create_text` and `set_text`.
//! - [`OpCounters::bytes_written`] — **host to guest.** Bytes the host wrote
//!   *into* guest linear memory. `get_attribute` and `text_content`.
//!
//! An operation whose arguments are all atoms and handles moves zero bytes in
//! either direction, and that is the point being measured.
//! [`Op::payload_direction`] states which way each one can move bytes at all,
//! so a report can be grouped by direction without a reader having to remember
//! the table.
//!
//! Interning is counted as its own operation rather than being amortised into
//! the operations that use the atom. That is the honest split: a name crosses
//! once, at `intern`, and never again. Reporting `set_attribute` as free
//! *without* also reporting what `intern` cost would be a real number used to
//! tell a misleading story, so [`Counters`] exposes both and the end-to-end
//! test asserts both.
//!
//! # What `host_string_bytes` counts, and why the read direction needs it
//!
//! Bytes of owned host-side `String` that had to exist in order to service the
//! call. It is the *second* copy, the one that never crosses the boundary and
//! that a byte-across-the-boundary count therefore cannot see.
//!
//! Both directions pay it, for different reasons:
//!
//! - **Writing**, the host copies guest memory into a `String` before touching
//!   the document, because [`read_string`](crate::read_string) must drop its
//!   borrow of guest memory first — that is the reentrancy rule, and it is not
//!   negotiable. The facade then copies that `String` into the node.
//! - **Reading**, every reader in `blitz-dom-api` returns an owned `String` by
//!   design (see its MAPPING.md, "Readers allocate a `String`, so the wasm path
//!   pays two copies"). The host receives that `String` and copies it again,
//!   into guest memory.
//!
//! So for a string operation of `n` payload bytes, roughly `2n` bytes are
//! copied in total, and only `n` of them show up as boundary traffic. A read of
//! an `n`-byte attribute reports `bytes_written == n` **and**
//! `host_string_bytes == n`, and ABI.md quotes both. Quoting only the first
//! would understate a read by half.
//!
//! # What `host_allocs` counts
//!
//! Allocations **this crate** makes or takes ownership of while servicing the
//! call: the `String` it builds from guest memory on a write, and the `String`
//! a facade reader hands back on a read. It does not count allocations the
//! facade makes and keeps, because this crate cannot see them without
//! instrumenting a package it does not own.
//!
//! Those exist and are not negligible:
//!
//! - `document::create_element` lowercases the tag into a fresh `String`.
//! - `element::get_attribute` and `element::has_attribute` lowercase the
//!   attribute *name* into a fresh `String` before the lookup.
//! - `element::has_attribute` is the sharpest one: it goes through the same
//!   `read_attr` as `get_attribute`, so it **clones the attribute value into a
//!   `String` and immediately discards it** to answer a boolean. Its
//!   `bytes_written` is a structural zero and its `host_string_bytes` reads
//!   zero, and neither of those zeros means no string was built. Counting it
//!   would mean reading the value twice purely to measure it, which is a worse
//!   trade than saying so here.
//!
//! `add_listener` is another case: it grows two collections in
//! [`ListenerTable`](crate::ListenerTable) by an amortised entry, which is not
//! a per-call allocation and is not counted. Its `host_allocs` therefore reads
//! zero, and that zero means "no string was built", not "no memory moved".
//!
//! # `dispatch` is an export, not an import
//!
//! Every other counter here is a call the guest made into the host.
//! [`Op::Dispatch`] is the one call the *host* makes into the guest, counted
//! here anyway because it is the number that answers the question this crate
//! exists to answer: what does a click cost at the boundary. Its
//! `bytes_copied` is structurally zero — the argument is a listener id, and
//! there is no pointer in the signature to copy anything through.

/// Counters for a single host function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpCounters {
    /// How many times the guest called it, including calls that errored.
    pub calls: u64,
    /// **Guest to host.** Bytes read out of guest linear memory.
    pub bytes_copied: u64,
    /// **Host to guest.** Bytes written into guest linear memory.
    ///
    /// A read that did not fit the guest's buffer wrote nothing, so it adds
    /// nothing here even though the host did all the work of producing the
    /// value. See [`Op::payload_direction`] and ABI.md, "The read direction".
    pub bytes_written: u64,
    /// Bytes of owned host-side `String` this operation had to materialise:
    /// the copy that never crosses the boundary. See the module docs.
    pub host_string_bytes: u64,
    /// Allocations made or taken ownership of by this crate while servicing
    /// it. See the module docs for what is and is not included.
    pub host_allocs: u64,
}

impl OpCounters {
    fn call(&mut self) {
        self.calls += 1;
    }

    /// Bytes that crossed the boundary in either direction.
    ///
    /// Only ever one of the two is non-zero for a given operation, so this is a
    /// convenience rather than a mixture; [`Op::payload_direction`] says which.
    pub fn bytes_crossed(&self) -> u64 {
        self.bytes_copied + self.bytes_written
    }
}

/// Which way payload bytes can move for an operation.
///
/// The requirement this satisfies is that a report cannot silently add a read
/// byte to a write byte: the two are different fields, and this says which
/// field an operation is even capable of touching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Guest to host: the host reads guest linear memory.
    GuestToHost,
    /// Host to guest: the host writes guest linear memory.
    HostToGuest,
    /// Handles, atoms and ids only. Structurally zero, not measured-zero.
    None,
}

/// Every counter for one instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub intern: OpCounters,
    pub create_element: OpCounters,
    pub create_text: OpCounters,
    pub append_child: OpCounters,
    pub set_attribute: OpCounters,
    pub set_text: OpCounters,
    pub add_listener: OpCounters,
    pub remove_listener: OpCounters,
    /// The read direction. Writes into guest memory, so its traffic lands in
    /// `bytes_written` and never in `bytes_copied`.
    pub get_attribute: OpCounters,
    /// The read direction. See [`Counters::get_attribute`].
    pub text_content: OpCounters,
    /// A read that returns a boolean, so it moves no payload at all — and
    /// still allocates host-side. See the module docs.
    pub has_attribute: OpCounters,
    /// Calls the host made *into* the guest's `dispatch` export. See the module
    /// docs: it is the only outbound counter, and its `bytes_copied` is
    /// structurally zero.
    pub dispatch: OpCounters,
    /// The last non-`OK` status returned to the guest, or `None`.
    pub last_error: Option<i32>,
    /// The last negative status the guest's `dispatch` export returned, or
    /// `None`.
    ///
    /// The guest's status codes are its own, not this crate's: a guest is free
    /// to mean anything by `-3`. It is kept only so a failing test can say
    /// which listener reported trouble instead of "a click did nothing".
    pub last_guest_status: Option<i32>,
    /// The last `DomError`, rendered, or `None`.
    ///
    /// The ABI collapses every `DomError` into `ERR_DOM`, so without this the
    /// host-side detail would be unrecoverable and a failing test could say
    /// only "the guest got -5".
    pub last_dom_error: Option<String>,
}

/// Which operation a counter update belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Intern,
    CreateElement,
    CreateText,
    AppendChild,
    SetAttribute,
    SetText,
    AddListener,
    RemoveListener,
    GetAttribute,
    TextContent,
    HasAttribute,
    /// The outbound one: a host call into the guest's `dispatch` export.
    Dispatch,
}

impl Op {
    /// The name this operation is registered under in the [`MODULE`] namespace.
    ///
    /// [`MODULE`]: crate::MODULE
    pub fn name(self) -> &'static str {
        match self {
            Op::Intern => "intern",
            Op::CreateElement => "create_element",
            Op::CreateText => "create_text",
            Op::AppendChild => "append_child",
            Op::SetAttribute => "set_attribute",
            Op::SetText => "set_text",
            Op::AddListener => "add_listener",
            Op::RemoveListener => "remove_listener",
            Op::GetAttribute => "get_attribute",
            Op::TextContent => "text_content",
            Op::HasAttribute => "has_attribute",
            Op::Dispatch => "dispatch",
        }
    }

    /// Which way this operation can move payload bytes.
    ///
    /// `Direction::None` is a statement about the signature, not about a
    /// measurement: there is no pointer in the signature for bytes to travel
    /// through, so the zero cannot become non-zero later.
    pub fn payload_direction(self) -> Direction {
        match self {
            Op::Intern | Op::CreateText | Op::SetText => Direction::GuestToHost,
            Op::GetAttribute | Op::TextContent => Direction::HostToGuest,
            Op::CreateElement
            | Op::AppendChild
            | Op::SetAttribute
            | Op::AddListener
            | Op::RemoveListener
            | Op::HasAttribute
            | Op::Dispatch => Direction::None,
        }
    }
}

impl Counters {
    /// Every operation, in ABI order, so a report can iterate rather than
    /// naming eleven fields and forgetting the twelfth.
    pub const OPS: [Op; 12] = [
        Op::Intern,
        Op::CreateElement,
        Op::CreateText,
        Op::AppendChild,
        Op::SetAttribute,
        Op::SetText,
        Op::AddListener,
        Op::RemoveListener,
        Op::GetAttribute,
        Op::TextContent,
        Op::HasAttribute,
        Op::Dispatch,
    ];

    /// Read one operation's counters.
    pub fn get(&self, op: Op) -> OpCounters {
        match op {
            Op::Intern => self.intern,
            Op::CreateElement => self.create_element,
            Op::CreateText => self.create_text,
            Op::AppendChild => self.append_child,
            Op::SetAttribute => self.set_attribute,
            Op::SetText => self.set_text,
            Op::AddListener => self.add_listener,
            Op::RemoveListener => self.remove_listener,
            Op::GetAttribute => self.get_attribute,
            Op::TextContent => self.text_content,
            Op::HasAttribute => self.has_attribute,
            Op::Dispatch => self.dispatch,
        }
    }

    pub(crate) fn slot(&mut self, op: Op) -> &mut OpCounters {
        match op {
            Op::Intern => &mut self.intern,
            Op::CreateElement => &mut self.create_element,
            Op::CreateText => &mut self.create_text,
            Op::AppendChild => &mut self.append_child,
            Op::SetAttribute => &mut self.set_attribute,
            Op::SetText => &mut self.set_text,
            Op::AddListener => &mut self.add_listener,
            Op::RemoveListener => &mut self.remove_listener,
            Op::GetAttribute => &mut self.get_attribute,
            Op::TextContent => &mut self.text_content,
            Op::HasAttribute => &mut self.has_attribute,
            Op::Dispatch => &mut self.dispatch,
        }
    }

    pub(crate) fn record_call(&mut self, op: Op) {
        self.slot(op).call();
    }

    /// Guest to host: the host read `bytes` out of guest memory, into a
    /// `String` it now owns.
    pub(crate) fn record_copy(&mut self, op: Op, bytes: usize) {
        let slot = self.slot(op);
        slot.bytes_copied += bytes as u64;
        // One `String` per string read out of guest memory, and it is exactly
        // as long as what crossed.
        slot.host_string_bytes += bytes as u64;
        slot.host_allocs += 1;
    }

    /// A facade reader handed this crate an owned `String` of `bytes`.
    ///
    /// Recorded *before* the write into guest memory, and separately from it,
    /// because the two are different costs and a read that does not fit the
    /// guest's buffer pays this one and not the other.
    pub(crate) fn record_host_string(&mut self, op: Op, bytes: usize) {
        let slot = self.slot(op);
        slot.host_string_bytes += bytes as u64;
        slot.host_allocs += 1;
    }

    /// Host to guest: the host wrote `bytes` into guest memory.
    pub(crate) fn record_write(&mut self, op: Op, bytes: usize) {
        self.slot(op).bytes_written += bytes as u64;
    }

    pub(crate) fn record_error(&mut self, status: i32) {
        self.last_error = Some(status);
    }

    pub(crate) fn record_dom_error(&mut self, error: blitz_dom_api::DomError) {
        self.last_error = Some(crate::status::ERR_DOM);
        self.last_dom_error = Some(error.to_string());
    }

    /// Every call the guest made into the host.
    ///
    /// `dispatch` is not in this sum: it goes the other way, and adding an
    /// outbound call to a count of inbound ones would produce a number that
    /// answers no question.
    pub fn total_calls(&self) -> u64 {
        Self::OPS
            .iter()
            .filter(|op| !matches!(op, Op::Dispatch))
            .map(|op| self.get(*op).calls)
            .sum()
    }

    /// Every byte the host read **out of** guest memory: the write direction,
    /// interning included.
    ///
    /// Deliberately *not* every byte that crossed. Reads travel the other way
    /// and are in [`total_bytes_written`](Self::total_bytes_written); summing
    /// the two into one number would hide which direction a change moved, and
    /// the two directions cost different things.
    pub fn total_bytes_copied(&self) -> u64 {
        Self::OPS.iter().map(|op| self.get(*op).bytes_copied).sum()
    }

    /// Every byte the host wrote **into** guest memory: the read direction.
    pub fn total_bytes_written(&self) -> u64 {
        Self::OPS.iter().map(|op| self.get(*op).bytes_written).sum()
    }

    /// Both directions. Use it for a grand total, never for a claim about one
    /// direction.
    pub fn total_bytes_crossed(&self) -> u64 {
        self.total_bytes_copied() + self.total_bytes_written()
    }

    /// Every byte of owned host-side `String` this crate materialised, in
    /// either direction: the copies that never crossed the boundary.
    ///
    /// The number that keeps a boundary-byte count from flattering itself. See
    /// the module docs.
    pub fn total_host_string_bytes(&self) -> u64 {
        Self::OPS
            .iter()
            .map(|op| self.get(*op).host_string_bytes)
            .sum()
    }

    /// Bytes that crossed guest-to-host for anything other than interning a
    /// name.
    ///
    /// The one number that answers "what does a mutation cost once the names
    /// are known", which is the steady state a running page is actually in.
    pub fn bytes_copied_excluding_interning(&self) -> u64 {
        self.total_bytes_copied() - self.intern.bytes_copied
    }
}
