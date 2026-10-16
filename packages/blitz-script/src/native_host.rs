//! Engine-neutral contracts for native guest values and retained callbacks.
//!
//! These traits describe an opt-in boundary only. They do not provide a guest
//! runtime or imply that a document backend executes through this interface.

/// The active identity of a guest realm.
///
/// A generation distinguishes successive guest instances that use the same
/// realm identifier. Hosts should advance it when an instance is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RealmScope<R> {
    realm: R,
    generation: u64,
}

impl<R> RealmScope<R> {
    /// Create a scope for the currently active guest instance.
    pub const fn new(realm: R, generation: u64) -> Self {
        Self { realm, generation }
    }

    /// The identifier of the realm.
    pub const fn realm(&self) -> &R {
        &self.realm
    }

    /// The generation of the guest instance in this realm.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// A retained callback identifier scoped to the guest instance that created it.
///
/// The token is opaque to this crate. Its identifier type belongs to the guest
/// implementation and should identify a callback retained by that instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetainedCallback<R, C> {
    realm: R,
    generation: u64,
    callback: C,
}

impl<R, C> RetainedCallback<R, C> {
    /// Create a token for a callback retained in the given realm generation.
    pub const fn new(realm: R, generation: u64, callback: C) -> Self {
        Self {
            realm,
            generation,
            callback,
        }
    }

    /// The identifier of the realm that retained this callback.
    pub const fn realm(&self) -> &R {
        &self.realm
    }

    /// The generation that retained this callback.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The guest-defined callback identifier.
    pub const fn callback(&self) -> &C {
        &self.callback
    }

    /// Whether this token belongs to the active realm generation.
    pub fn is_current_in(&self, scope: &RealmScope<R>) -> bool
    where
        R: PartialEq,
    {
        self.realm == scope.realm && self.generation == scope.generation
    }
}

/// A guest implementation that exposes typed values and retained callbacks.
///
/// `Value`, `RealmId`, `CallbackId`, and `Error` remain implementation-defined.
/// In particular, this interface does not impose a shared host value enum.
pub trait NativeGuest {
    /// The guest's native value representation.
    type Value;
    /// An identifier for one realm owned by this guest.
    type RealmId: Clone + PartialEq;
    /// An identifier for a callback retained by this guest.
    type CallbackId: Clone;
    /// Errors produced while invoking a callback.
    type Error;

    /// Return this guest's current realm and generation, rather than accepting
    /// a caller-supplied scope as authority to enter the guest.
    fn scope(&self) -> RealmScope<Self::RealmId>;

    /// Root an executable guest callback until release or realm teardown.
    /// Reject values that are not callable without evaluating their source.
    fn retain_callback(
        &mut self,
        callback: &Self::Value,
    ) -> Result<RetainedCallback<Self::RealmId, Self::CallbackId>, Self::Error>;

    /// Revoke dispatch and release the guest root. Other token clones must
    /// become unusable. Unknown, released and stale tokens must be rejected.
    fn release_callback(
        &mut self,
        callback: &RetainedCallback<Self::RealmId, Self::CallbackId>,
    ) -> Result<(), Self::Error>;

    /// Install a compiled typed host entry point before page scripts execute.
    /// Guest-to-host conversions belong to the backend and must preserve value
    /// identity, UTF-16 code units and buffer ownership without serializing JS.
    fn register_intrinsic(
        &mut self,
        name: &'static str,
        arity: u8,
        callback: NativeIntrinsic<Self>,
    ) -> Result<(), Self::Error>
    where
        Self: Sized;

    /// Invoke a retained callback with typed arguments.
    ///
    /// Implementations must check that `callback` is current in `self.scope()` before
    /// executing it, and reject an unknown callback identifier. This method's
    /// contract does not claim that any particular guest currently enforces
    /// those checks.
    fn invoke_retained_callback(
        &mut self,
        callback: &RetainedCallback<Self::RealmId, Self::CallbackId>,
        arguments: &[Self::Value],
    ) -> Result<Self::Value, Self::Error>;
}

/// A native entry point that executes on the guest's owning realm thread.
pub type NativeIntrinsic<G> = fn(
    &mut G,
    &[<G as NativeGuest>::Value],
) -> Result<<G as NativeGuest>::Value, <G as NativeGuest>::Error>;

/// Failure returned by the host callback dispatch boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCallbackError<E> {
    /// The callback token belongs to another realm or guest generation.
    StaleCallback,
    /// The guest rejected the callback or its execution failed.
    Guest(E),
}

/// A host-side owner that can dispatch calls to a native guest.
pub trait NativeHost {
    /// The guest implementation owned by this host.
    type Guest: NativeGuest;

    /// Borrow the guest for an invocation.
    fn guest_mut(&mut self) -> &mut Self::Guest;

    /// Dispatch a typed call to a retained callback.
    ///
    /// Rejects mismatched realm or generation metadata before entering the
    /// guest. The guest implementation remains responsible for rejecting an
    /// unknown callback identifier before callback code runs.
    fn invoke_retained_callback(
        &mut self,
        callback: &RetainedCallback<
            <Self::Guest as NativeGuest>::RealmId,
            <Self::Guest as NativeGuest>::CallbackId,
        >,
        arguments: &[<Self::Guest as NativeGuest>::Value],
    ) -> Result<
        <Self::Guest as NativeGuest>::Value,
        NativeCallbackError<<Self::Guest as NativeGuest>::Error>,
    > {
        let guest = self.guest_mut();
        if !callback.is_current_in(&guest.scope()) {
            return Err(NativeCallbackError::StaleCallback);
        }
        guest
            .invoke_retained_callback(callback, arguments)
            .map_err(NativeCallbackError::Guest)
    }
}
