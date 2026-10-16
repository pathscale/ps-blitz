//! Executable contract witness. This is a synthetic guest, not a script engine.

use blitz_script::native_host::{
    NativeCallbackError, NativeGuest, NativeHost, NativeIntrinsic, RealmScope, RetainedCallback,
};
use std::rc::Rc;

type Callback = RetainedCallback<Rc<u8>, Rc<()>>;

struct Guest {
    scope: RealmScope<Rc<u8>>,
    root: Option<Rc<()>>,
    intrinsic: Option<NativeIntrinsic<Self>>,
    calls: usize,
}

impl NativeGuest for Guest {
    type Value = i64;
    type RealmId = Rc<u8>;
    type CallbackId = Rc<()>;
    type Error = &'static str;

    fn scope(&self) -> RealmScope<Self::RealmId> {
        self.scope.clone()
    }

    fn retain_callback(&mut self, value: &i64) -> Result<Callback, Self::Error> {
        if *value != 7 || self.root.is_some() {
            return Err("not an available callback");
        }
        let root = Rc::new(());
        self.root = Some(Rc::clone(&root));
        Ok(RetainedCallback::new(
            Rc::clone(self.scope.realm()),
            self.scope.generation(),
            root,
        ))
    }

    fn release_callback(&mut self, callback: &Callback) -> Result<(), Self::Error> {
        if !self.current(callback) {
            return Err("callback unavailable");
        }
        self.root = None;
        Ok(())
    }

    fn register_intrinsic(
        &mut self,
        name: &'static str,
        arity: u8,
        callback: NativeIntrinsic<Self>,
    ) -> Result<(), Self::Error> {
        if name != "increment" || arity != 1 {
            return Err("invalid intrinsic");
        }
        self.intrinsic = Some(callback);
        Ok(())
    }

    fn invoke_retained_callback(
        &mut self,
        callback: &Callback,
        arguments: &[i64],
    ) -> Result<i64, Self::Error> {
        if !self.current(callback) {
            return Err("callback unavailable");
        }
        self.calls += 1;
        self.intrinsic.ok_or("intrinsic unavailable")?(self, arguments)
    }
}

impl Guest {
    fn current(&self, callback: &Callback) -> bool {
        callback.is_current_in(&self.scope)
            && self
                .root
                .as_ref()
                .is_some_and(|root| Rc::ptr_eq(root, callback.callback()))
    }
}

impl NativeHost for Guest {
    type Guest = Self;
    fn guest_mut(&mut self) -> &mut Self {
        self
    }
}

fn main() {
    let mut guest = Guest {
        scope: RealmScope::new(Rc::new(1), 1),
        root: None,
        intrinsic: None,
        calls: 0,
    };
    guest
        .register_intrinsic("increment", 1, |_, arguments| {
            arguments
                .first()
                .copied()
                .map(|value| value + 1)
                .ok_or("missing argument")
        })
        .expect("register compiled intrinsic");
    let callback = guest.retain_callback(&7).expect("retain callback");
    assert_eq!(
        NativeHost::invoke_retained_callback(&mut guest, &callback, &[5]),
        Ok(6)
    );
    let foreign = RetainedCallback::new(Rc::new(2), 1, Rc::clone(callback.callback()));
    assert_eq!(
        NativeHost::invoke_retained_callback(&mut guest, &foreign, &[5]),
        Err(NativeCallbackError::StaleCallback)
    );
    assert_eq!(
        guest.calls, 1,
        "foreign scope must not enter guest dispatch"
    );
    guest.release_callback(&callback).expect("release callback");
    assert!(NativeHost::invoke_retained_callback(&mut guest, &callback.clone(), &[5]).is_err());
    assert_eq!(guest.calls, 1, "release must revoke cloned tokens");
    let next = guest.retain_callback(&7).expect("retain next callback");
    guest.scope = RealmScope::new(Rc::clone(guest.scope.realm()), 2);
    assert_eq!(
        NativeHost::invoke_retained_callback(&mut guest, &next, &[5]),
        Err(NativeCallbackError::StaleCallback)
    );
    assert_eq!(
        guest.calls, 1,
        "stale generation must not enter guest dispatch"
    );
    println!("typed native dispatch, root release, realm and generation checks passed");
}
