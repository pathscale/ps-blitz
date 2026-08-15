//! Per-operation instrumentation.
//!
//! Three numbers per operation, and deliberately no timing: a timing number
//! measured on one machine, in one build profile, against an interpreter is
//! not evidence of anything, whereas a byte count is the same on every machine
//! and is exactly what the boundary design is meant to change.
//!
//! # What `bytes_copied` counts
//!
//! Bytes read out of guest linear memory by the host, for that operation.
//! Nothing else. An operation whose arguments are all atoms and handles reads
//! zero bytes, and that is the point being measured.
//!
//! Interning is counted as its own operation rather than being amortised into
//! the operations that use the atom. That is the honest split: a name crosses
//! once, at `intern`, and never again. Reporting `set_attribute` as free
//! *without* also reporting what `intern` cost would be a real number used to
//! tell a misleading story, so [`Counters`] exposes both and the end-to-end
//! test asserts both.
//!
//! # What `host_allocs` counts
//!
//! Allocations **this crate** makes while servicing the call: the `String` it
//! builds from guest memory, and interner growth. It does not count
//! allocations inside `blitz-dom-api` or `blitz-dom`, because this crate
//! cannot see them without instrumenting a package it does not own.
//!
//! Those exist and are not negligible. `document::create_element` lowercases
//! the tag into a fresh `String`; every reader in the facade returns an owned
//! `String` by design (see its MAPPING.md, "Readers allocate a `String`").
//! ABI.md says so next to the table, so a reader of these counters does not
//! mistake "one host alloc" for "one allocation happened".
//!
//! `add_listener` is a third case: it grows two collections in
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
    /// Bytes read out of guest linear memory by this operation.
    pub bytes_copied: u64,
    /// Allocations made by this crate while servicing it. See the module docs
    /// for what is and is not included.
    pub host_allocs: u64,
}

impl OpCounters {
    fn call(&mut self) {
        self.calls += 1;
    }
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
    /// The outbound one: a host call into the guest's `dispatch` export.
    Dispatch,
}

impl Counters {
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
            Op::Dispatch => &mut self.dispatch,
        }
    }

    pub(crate) fn record_call(&mut self, op: Op) {
        self.slot(op).call();
    }

    pub(crate) fn record_copy(&mut self, op: Op, bytes: usize) {
        let slot = self.slot(op);
        slot.bytes_copied += bytes as u64;
        // One `String` per string read out of guest memory.
        slot.host_allocs += 1;
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
        self.intern.calls
            + self.create_element.calls
            + self.create_text.calls
            + self.append_child.calls
            + self.set_attribute.calls
            + self.set_text.calls
            + self.add_listener.calls
            + self.remove_listener.calls
    }

    /// Every byte that crossed the boundary, interning included, in either
    /// direction.
    pub fn total_bytes_copied(&self) -> u64 {
        self.intern.bytes_copied
            + self.create_element.bytes_copied
            + self.create_text.bytes_copied
            + self.append_child.bytes_copied
            + self.set_attribute.bytes_copied
            + self.set_text.bytes_copied
            + self.add_listener.bytes_copied
            + self.remove_listener.bytes_copied
            + self.dispatch.bytes_copied
    }

    /// Bytes that crossed for anything other than interning a name.
    ///
    /// The one number that answers "what does a mutation cost once the names
    /// are known", which is the steady state a running page is actually in.
    pub fn bytes_copied_excluding_interning(&self) -> u64 {
        self.total_bytes_copied() - self.intern.bytes_copied
    }
}
