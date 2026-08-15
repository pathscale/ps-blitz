//! The status codes every host function returns.
//!
//! One convention for all of them, so a guest never has to remember which
//! shape a particular import uses:
//!
//! - **Negative is an error.** The value is one of the constants below.
//! - **Non-negative is success.** For an operation that creates something
//!   (`intern`, `create_element`, `create_text`) the value *is* the handle or
//!   atom. For everything else it is [`OK`], which is zero.
//!
//! Handles and atoms are therefore capped at `i32::MAX` rather than `u32::MAX`.
//! That is 2.1 billion nodes, which is not the constraint anyone will hit
//! first, and it buys a single return value instead of an out-pointer plus the
//! bounds check that out-pointer would need.
//!
//! **Nothing here traps.** A trap tears down the instance and takes the reason
//! with it, so a guest that passed a bad handle would learn nothing except
//! that it died. Every guest mistake is one of these codes.

/// The operation succeeded and produced no value.
pub const OK: i32 = 0;

/// The handle does not name a node this instance was given.
pub const ERR_BAD_HANDLE: i32 = -1;

/// The atom was not produced by this instance's interner.
pub const ERR_BAD_ATOM: i32 = -2;

/// The `(ptr, len)` pair does not lie inside the guest's linear memory, or the
/// guest exports no memory at all.
pub const ERR_BAD_MEMORY: i32 = -3;

/// The bytes at `(ptr, len)` are not valid UTF-8.
pub const ERR_BAD_UTF8: i32 = -4;

/// The underlying `blitz-dom-api` operation returned a [`DomError`].
///
/// Deliberately one code rather than one per variant. A guest cannot act
/// differently on `TreeInvariant` than on `NodeNotFound`, and a stable ABI is
/// worth more than a taxonomy the caller ignores. The host-side error is not
/// discarded: it goes to the counters' last-error slot for a test to read.
///
/// [`DomError`]: blitz_dom_api::DomError
pub const ERR_DOM: i32 = -5;

/// The handle table is full: more than `i32::MAX` live handles.
pub const ERR_TOO_MANY_HANDLES: i32 = -6;

/// The listener id was never issued by this instance, or has been removed.
///
/// Separate from [`ERR_BAD_HANDLE`] because it is a different namespace: a
/// listener id indexes the listener table, a handle indexes the node table,
/// and a guest that confuses the two should be told which one it got wrong.
pub const ERR_BAD_LISTENER: i32 = -7;

/// The listener table is full: more than `i32::MAX` listeners registered.
pub const ERR_TOO_MANY_LISTENERS: i32 = -8;

/// A human-readable name for a status code, for test failure messages.
pub fn name(status: i32) -> &'static str {
    match status {
        OK => "OK",
        ERR_BAD_HANDLE => "ERR_BAD_HANDLE",
        ERR_BAD_ATOM => "ERR_BAD_ATOM",
        ERR_BAD_MEMORY => "ERR_BAD_MEMORY",
        ERR_BAD_UTF8 => "ERR_BAD_UTF8",
        ERR_DOM => "ERR_DOM",
        ERR_TOO_MANY_HANDLES => "ERR_TOO_MANY_HANDLES",
        ERR_BAD_LISTENER => "ERR_BAD_LISTENER",
        ERR_TOO_MANY_LISTENERS => "ERR_TOO_MANY_LISTENERS",
        n if n >= 0 => "OK (value)",
        _ => "unknown",
    }
}
