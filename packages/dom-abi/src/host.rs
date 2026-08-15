//! The calling convention: what crosses the boundary, and what comes back.
//!
//! This module is `blitz-wasm`'s ABI.md turned into types both sides compile
//! against. An error code that lives in a markdown table, as an `i32` literal
//! in the host, and as another `i32` literal in the guest bindings is three
//! places to get it wrong and only one of them fails a build.
//!
//! Nothing here calls anything. There are no function signatures for the
//! imports, because the host declares those to its wasm runtime and the guest
//! declares them in an `extern` block, and a shared signature type would be a
//! third declaration that neither side actually uses. What is shared is the
//! vocabulary: the id types, the status codes, and the two protocols — reads
//! and string tiers — that a caller can get wrong silently.

use serde::{Deserialize, Serialize};

/// The version of the calling convention.
///
/// **Independent of [`TEMPLATE_FORMAT_VERSION`] and [`RUNTIME_ABI_VERSION`].**
/// Adding an import, changing what a status code means, or changing the read
/// protocol bumps this and leaves every cached template untouched, because none
/// of it describes a template.
///
/// [`TEMPLATE_FORMAT_VERSION`]: crate::template::TEMPLATE_FORMAT_VERSION
/// [`RUNTIME_ABI_VERSION`]: crate::runtime::RUNTIME_ABI_VERSION
pub const HOST_ABI_VERSION: u32 = 1;

/// A node, as the guest sees it.
///
/// **Opaque, and validated against a per-instance table on every call.** A
/// handle is an index into *this instance's* table, and that table contains the
/// mount point the host seeded plus the nodes this guest created. It is
/// deliberately not a `NodeId`: a `NodeId` is an index into the document's
/// arena, so a guest handed raw ids could address every node in the document by
/// counting up from zero, including nodes belonging to a page it was never
/// given.
///
/// So a forged handle is not an escalation. It is either out of range — which
/// is [`Status::ERR_BAD_HANDLE`] — or it names a node this guest already holds
/// a handle for, and there is nothing there it could not already reach.
///
/// **A forged handle is an error return, never a trap.** A trap tears down the
/// instance and takes the reason with it, so a guest that passed a bad handle
/// would learn only that it died.
///
/// Handles are never reused and never freed. A detached node keeps its handle,
/// so a handle to it stays meaningful. The cost is that a long-lived guest's
/// table only grows; the fix, when a guest churns nodes in a loop, is a
/// generational handle, and it is not needed before then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Handle(pub u32);

impl Handle {
    /// The mount point, seeded by the host when the instance is created.
    ///
    /// **Without it a guest can build a tree and have nowhere to put it.** Every
    /// operation either creates a *detached* node or needs an existing one, so
    /// nothing in the operation set can produce the first handle. The host
    /// supplies it, and it is `0` because a constant is one less thing to
    /// negotiate.
    pub const MOUNT: Handle = Handle(0);
}

/// An interned string, as the guest sees it.
///
/// # Rule 4: nothing data-derived becomes an atom
///
/// **Atoms are never released.** There is no free list, no refcount and no
/// eviction, and that is the correct design for what atoms are for: tag names,
/// attribute names, event names, and attribute values drawn from a set fixed at
/// compile time. A page's vocabulary is small and stops growing once it is
/// known, so an interned name costs its bytes exactly once and is free
/// thereafter, however many elements use it.
///
/// Applied to anything the data produces, the same property is an unbounded
/// leak: interning per-frame text, or a row's key, or a value from a picker,
/// adds entries the host can never reclaim. The guest binding cannot catch this
/// and neither can this type — an `Atom` obtained from a data-derived string is
/// perfectly well-formed. The alternative is always the copied tier; see
/// [`Tier`].
///
/// This is the same concept as [`crate::template::Atom`], on the far side of
/// the load step: a template carries the string, and the numeric id is issued
/// when the template is loaded into a particular host, because atom ids are
/// per-instance and a template cached on a CDN cannot know them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Atom(pub u32);

/// A registered event listener.
///
/// A separate namespace from [`Handle`] on purpose: a listener id indexes the
/// listener table and a handle indexes the node table, and a guest that passes
/// one where the other belongs should be told which one it got wrong — see
/// [`Status::ERR_BAD_LISTENER`] — rather than hitting an unrelated node.
///
/// Never reused, for the same reason handles are not: a stale id must be an
/// error rather than a silent hit on whatever took its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ListenerId(pub u32);

/// A live row scope in a [`For`], as issued by the guest.
///
/// # Rule 2: stable ids, not positions
///
/// **This is what crosses for a row, and it is not the key.** The guest issues
/// one id per live row scope; the host reconciles on it and never sees anything
/// else about the row's identity. That is settled by reading `SolidRS`'s
/// `map.rs` rather than designed against it, and the three alternatives each
/// fail against something that file actually does:
///
/// - **Not the key.** A key is a type, not a value: `map_array` requires
///   `K: Eq + Hash` and nothing more, so there is frequently no string to send.
///   It never leaves the guest and is never stored — it is recomputed from the
///   item on every pass, dropped into a `HashMap<K, isize>` for the duration of
///   that pass, and discarded. Sending it would mean the whole list's keys
///   crossing on every pass.
/// - **Not a hash of the key.** `map.rs` resolves collisions with full `Eq`
///   inside that `HashMap`. A host reconciling on a hash has strictly less
///   fidelity than the guest, so a colliding pair would fuse two distinct rows
///   into one — silently, and only under collision.
/// - **Not an [`Atom`].** Atoms are never released and row keys come from the
///   data. A list that scrolls a million rows past would add a million entries
///   the interner can never free. A row id needs a free list; an atom is
///   precisely the thing that does not have one.
///
/// # Why this is better than getting the key right
///
/// The reason matters more than the rule. If the host reconciled by key and the
/// guest reconciled by key, **two independent reconciliations would have to
/// agree**, and they agree only if they run the same algorithm down to
/// duplicate handling and removal order. `map.rs` permits duplicate keys and
/// chains them through `new_indices_next`, scanning backwards so duplicates
/// match in natural order. Any host that did not reproduce that exactly would
/// bind the guest's scope for one row to the host's node for another, silently
/// and only on some edits.
///
/// Reconciling on a row id removes the requirement instead of documenting it.
/// Exactly one row exists per id, so the host's side is a lookup with no
/// ambiguity left in it, and the algorithm that resolved the ambiguity is the
/// one in `map.rs` — which is the one with the tests. Two rows sharing a key
/// are two scopes, so they are two ids: the guest disambiguated them with full
/// `Eq` before anything crossed.
///
/// # The ordering constraint
///
/// **A new row's id may be issued before the displaced row's id is dropped.**
/// `map.rs` follows upstream's staged-commit discipline: rows are created into
/// temporaries and removals are deferred, so the exiting rows are disposed
/// *after* the pass's new rows exist. A host that assumed disposal-then-create
/// — by asserting an id is free before issuing it, or by reusing a slot on
/// drop — would fail on exactly the reorderings this design exists to handle.
///
/// [`For`]: crate::template::For
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowId(pub u32);

/// The largest value a [`Handle`], [`Atom`], [`ListenerId`] or [`RowId`] may
/// take.
///
/// Every host function returns `i32`, and for an operation that creates
/// something the return value *is* the id. So ids are capped at `i32::MAX`
/// rather than `u32::MAX`. That buys a single return value instead of an
/// out-pointer plus the bounds check the out-pointer would need, and 2.1 billion
/// live nodes is not the constraint anything hits first.
pub const MAX_ID: u32 = i32::MAX as u32;

/// What a host function returned.
///
/// **Negative is failure, non-negative is a value**, with exactly one
/// exception, [`Status::ABSENT`].
///
/// For an operation that creates something the value is the [`Handle`],
/// [`Atom`] or [`ListenerId`]. For a reader it is the byte length of the value.
/// For everything else it is [`Status::OK`], which is zero. One convention for
/// every import, so a guest never has to remember which shape a particular one
/// uses.
///
/// **Nothing traps on a guest mistake.** Every one of these is a return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Status(pub i32);

impl Status {
    /// The operation succeeded and produced no value.
    pub const OK: Status = Status(0);

    /// The handle does not name a node this instance was given.
    pub const ERR_BAD_HANDLE: Status = Status(-1);

    /// The atom was not produced by this instance's interner.
    pub const ERR_BAD_ATOM: Status = Status(-2);

    /// The `(ptr, len)` pair does not lie inside the guest's linear memory, or
    /// the guest exports no memory at all.
    pub const ERR_BAD_MEMORY: Status = Status(-3);

    /// The bytes at `(ptr, len)` are not valid UTF-8.
    pub const ERR_BAD_UTF8: Status = Status(-4);

    /// The underlying DOM operation failed.
    ///
    /// Deliberately one code rather than one per variant. A guest cannot act
    /// differently on a tree-invariant violation than on a missing node, and a
    /// stable ABI is worth more than a taxonomy nobody branches on. The detail
    /// is not discarded — the host keeps the rendered error where a failing
    /// test can read it — it just is not in the return value.
    pub const ERR_DOM: Status = Status(-5);

    /// The handle table is full: more than [`MAX_ID`] live handles.
    pub const ERR_TOO_MANY_HANDLES: Status = Status(-6);

    /// The listener id was never issued by this instance, or has been removed.
    pub const ERR_BAD_LISTENER: Status = Status(-7);

    /// The listener table is full: more than [`MAX_ID`] listeners.
    pub const ERR_TOO_MANY_LISTENERS: Status = Status(-8);

    /// The attribute is not present. **The one negative that is not a
    /// failure.**
    ///
    /// `getAttribute` returns `null` for an absent attribute, and `null` is not
    /// the same as present-and-empty: a guest that cannot tell them apart
    /// cannot implement `hasAttribute` on top of a read. So a reader needs
    /// three outcomes from one `i32` — a length, "not there", and a genuine
    /// failure — and this is the third.
    ///
    /// Both alternatives were worse. Calling `has_attribute` first doubles the
    /// crossings of the very operation being measured, for a reason that has
    /// nothing to do with strings. Returning `len + 1` and reserving `0` for
    /// absent puts arithmetic in the host and in every guest binding forever,
    /// to save spending one code.
    ///
    /// A guest binding maps this to `Ok(None)` and every other negative to an
    /// error, so nothing above the bindings sees it as a failure. The host does
    /// not record it as an error either: a guest polling for an optional
    /// attribute would otherwise leave the last-error slot permanently set to
    /// something that never went wrong.
    pub const ABSENT: Status = Status(-9);

    /// The raw `i32` that crossed.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// Whether this is a failure — negative, and not [`Status::ABSENT`].
    ///
    /// The whole reason this is a method rather than `status < 0` written at
    /// each call site is that `status < 0` is wrong at exactly one of them, and
    /// wrong in the direction that turns a present-and-empty attribute into an
    /// error.
    pub fn is_failure(self) -> bool {
        self.0 < 0 && self != Status::ABSENT
    }

    /// The non-negative payload, if there is one.
    ///
    /// `None` for [`Status::ABSENT`] and for every failure.
    pub fn value(self) -> Option<u32> {
        (self.0 >= 0).then_some(self.0 as u32)
    }

    /// A human-readable name, for diagnostics and test failures.
    pub fn name(self) -> &'static str {
        match self {
            Status::OK => "OK",
            Status::ERR_BAD_HANDLE => "ERR_BAD_HANDLE",
            Status::ERR_BAD_ATOM => "ERR_BAD_ATOM",
            Status::ERR_BAD_MEMORY => "ERR_BAD_MEMORY",
            Status::ERR_BAD_UTF8 => "ERR_BAD_UTF8",
            Status::ERR_DOM => "ERR_DOM",
            Status::ERR_TOO_MANY_HANDLES => "ERR_TOO_MANY_HANDLES",
            Status::ERR_BAD_LISTENER => "ERR_BAD_LISTENER",
            Status::ERR_TOO_MANY_LISTENERS => "ERR_TOO_MANY_LISTENERS",
            Status::ABSENT => "ABSENT (not an error)",
            Status(n) if n >= 0 => "OK (value)",
            _ => "unknown",
        }
    }
}

/// Which tier a string crosses in.
///
/// **Two tiers, and choosing the wrong one is a leak rather than a bug.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Tag names, attribute names, event names, and attribute values from a
    /// closed set.
    ///
    /// Crosses once as UTF-8 bytes through the interner and is an [`Atom`]
    /// thereafter. **Zero bytes per use.** A guest setting `class="row"` in a
    /// loop interns each distinct string once for the life of the instance, not
    /// once per iteration.
    Interned,

    /// Dynamic text.
    ///
    /// Crosses as `(ptr, len)` into guest linear memory, which the host reads.
    /// **`len` bytes per use, every time.** This is the tier for anything the
    /// data produces: the second occurrence of a given sentence is rare, so
    /// interning it would save nothing and cost an entry that is never
    /// released.
    Copied,
}

/// How many bytes a value costs to cross, in a given tier, on the *n*-th use.
///
/// Present so that the claim "an interned name is free after the first use" is
/// a function rather than a sentence in a document. `n` is 1-based.
pub fn bytes_crossed(tier: Tier, len: u32, n: u32) -> u32 {
    match tier {
        Tier::Interned if n <= 1 => len,
        Tier::Interned => 0,
        Tier::Copied => len,
    }
}

/// The buffer a guest offers a reader.
///
/// # The read protocol: mechanism (b), the guest supplies the buffer
///
/// The guest passes `(out_ptr, out_cap)`; the host returns **the value's full
/// byte length, whether or not it fit**, and writes the bytes only if they fit.
/// This is `snprintf`'s convention.
///
/// - `len <= cap` — the bytes are at `out_ptr` and `len` of them are valid.
/// - `len > cap` — **nothing was written.** The guest resizes to `len` and
///   calls again; the second call is sized from the host's own answer, so it
///   cannot come back short.
/// - `cap == 0` — legal, and is how a guest asks for a length with nowhere to
///   put the value.
///
/// **The failure mode is cost, not corruption.** A value longer than the buffer
/// costs a second crossing and a second host-side allocation of the whole
/// value; the guest sees the same bytes either way. Nothing is ever truncated —
/// half a UTF-8 string is not a string — and nothing is ever stale, because the
/// value is produced fresh on the call that delivers it.
///
/// # The two mechanisms this is not
///
/// **(a) Two calls: length, then bytes.** The value can change between them,
/// and the ABI would be promising something it does not enforce; the first
/// embedder that mutates the document from a host-side widget in between gets a
/// truncated or over-read value with no error. It also costs two crossings for
/// *every* read rather than only for the ones that do not fit.
///
/// **(c) The host allocates in guest memory.** The guest exports `alloc`, the
/// host calls it, the host returns a pointer. This breaks the one rule the
/// binding exists to hold: a host function would call *into* the guest
/// mid-call, with a document borrow live. It also puts ownership of every
/// returned buffer on the guest, so a guest that forgets to free leaks and a
/// guest that frees twice corrupts its own heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutBuffer {
    /// Where in guest linear memory to write.
    pub ptr: u32,
    /// How many bytes may be written there.
    pub cap: u32,
}

/// What a reader's [`Status`] means, given the capacity that was offered.
///
/// The classification is here rather than at each call site because it is three
/// cases that look like two, and the case that gets dropped is
/// [`Self::TooSmall`] — which succeeds on every short value and fails only on
/// the long one that arrives in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// The value fit. `len` bytes were written at the buffer's pointer.
    Fit {
        /// How many bytes are valid.
        len: u32,
    },
    /// The value did not fit and **nothing was written**. Resize to `needed`
    /// and call again.
    TooSmall {
        /// The value's full byte length.
        needed: u32,
    },
    /// The attribute is not present. Distinct from a zero-length value; see
    /// [`Status::ABSENT`].
    Absent,
    /// The read failed.
    Failed(Status),
}

impl ReadOutcome {
    /// Classify a reader's return value against the capacity it was given.
    pub fn classify(status: Status, buffer: OutBuffer) -> ReadOutcome {
        if status == Status::ABSENT {
            return ReadOutcome::Absent;
        }
        match status.value() {
            None => ReadOutcome::Failed(status),
            Some(len) if len <= buffer.cap => ReadOutcome::Fit { len },
            Some(needed) => ReadOutcome::TooSmall { needed },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_negative_and_is_not_a_failure() {
        assert!(Status::ABSENT.raw() < 0);
        assert!(!Status::ABSENT.is_failure());
        assert_eq!(Status::ABSENT.value(), None);

        // Every other negative is.
        for status in [
            Status::ERR_BAD_HANDLE,
            Status::ERR_BAD_ATOM,
            Status::ERR_BAD_MEMORY,
            Status::ERR_BAD_UTF8,
            Status::ERR_DOM,
            Status::ERR_TOO_MANY_HANDLES,
            Status::ERR_BAD_LISTENER,
            Status::ERR_TOO_MANY_LISTENERS,
        ] {
            assert!(status.is_failure(), "{} should be a failure", status.name());
        }
    }

    #[test]
    fn status_codes_are_distinct() {
        let mut codes = [
            Status::OK,
            Status::ERR_BAD_HANDLE,
            Status::ERR_BAD_ATOM,
            Status::ERR_BAD_MEMORY,
            Status::ERR_BAD_UTF8,
            Status::ERR_DOM,
            Status::ERR_TOO_MANY_HANDLES,
            Status::ERR_BAD_LISTENER,
            Status::ERR_TOO_MANY_LISTENERS,
            Status::ABSENT,
        ];
        let count = codes.len();
        codes.sort_unstable();
        let mut seen = codes.to_vec();
        seen.dedup();
        assert_eq!(seen.len(), count, "two status codes share a value");
    }

    #[test]
    fn a_zero_length_value_is_not_absent() {
        let buffer = OutBuffer { ptr: 16, cap: 64 };

        // The distinction the whole three-outcome design exists for.
        assert_eq!(
            ReadOutcome::classify(Status(0), buffer),
            ReadOutcome::Fit { len: 0 }
        );
        assert_eq!(
            ReadOutcome::classify(Status::ABSENT, buffer),
            ReadOutcome::Absent
        );
    }

    #[test]
    fn a_value_that_exactly_fills_the_buffer_fits() {
        let buffer = OutBuffer { ptr: 16, cap: 64 };
        assert_eq!(
            ReadOutcome::classify(Status(64), buffer),
            ReadOutcome::Fit { len: 64 }
        );
        assert_eq!(
            ReadOutcome::classify(Status(65), buffer),
            ReadOutcome::TooSmall { needed: 65 }
        );
    }

    #[test]
    fn a_zero_capacity_read_asks_for_a_length_and_gets_one() {
        let buffer = OutBuffer { ptr: 0, cap: 0 };
        assert_eq!(
            ReadOutcome::classify(Status(200), buffer),
            ReadOutcome::TooSmall { needed: 200 }
        );
        // Except for the empty value, which needs nothing written.
        assert_eq!(
            ReadOutcome::classify(Status(0), buffer),
            ReadOutcome::Fit { len: 0 }
        );
    }

    #[test]
    fn a_failed_read_is_never_mistaken_for_a_length() {
        let buffer = OutBuffer { ptr: 16, cap: 64 };
        assert_eq!(
            ReadOutcome::classify(Status::ERR_BAD_HANDLE, buffer),
            ReadOutcome::Failed(Status::ERR_BAD_HANDLE)
        );
    }

    #[test]
    fn an_interned_name_is_free_after_its_first_use() {
        assert_eq!(bytes_crossed(Tier::Interned, 5, 1), 5);
        assert_eq!(bytes_crossed(Tier::Interned, 5, 2), 0);
        assert_eq!(bytes_crossed(Tier::Interned, 5, 1_000), 0);

        // Copied text never amortises, which is the whole difference.
        assert_eq!(bytes_crossed(Tier::Copied, 5, 1), 5);
        assert_eq!(bytes_crossed(Tier::Copied, 5, 1_000), 5);
    }

    #[test]
    fn the_mount_point_is_handle_zero() {
        assert_eq!(Handle::MOUNT, Handle(0));
    }
}
