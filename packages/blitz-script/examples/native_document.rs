//! Document-driver witness with a synthetic compiled guest, not a JavaScript engine.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Context;

use blitz_dom::{BaseDocument, Document, DocumentConfig};
use blitz_script::native_host::{
    NativeGuest, NativeHost, NativeIntrinsic, RealmScope, RetainedCallback,
};
use blitz_script::{
    BoundScriptDocument, NativeHostDocumentBackend, ScriptDocumentBackend, ScriptKind,
    TypedNativeGuest,
};
use blitz_traits::events::UiEvent;
use url::Url;

type Trace = Rc<RefCell<Vec<&'static str>>>;
type Callback = RetainedCallback<u64, u64>;

struct Guest {
    trace: Trace,
    storage: BTreeMap<String, i64>,
    intrinsic: Option<NativeIntrinsic<Self>>,
    callback: bool,
    omit_write: bool,
    intl: bool,
}

impl NativeGuest for Guest {
    type Value = i64;
    type RealmId = u64;
    type CallbackId = u64;
    type Error = &'static str;

    fn scope(&self) -> RealmScope<u64> {
        RealmScope::new(1, 1)
    }
    fn retain_callback(&mut self, value: &i64) -> Result<Callback, Self::Error> {
        if *value != 7 || self.callback {
            return Err("callback unavailable");
        }
        self.callback = true;
        Ok(RetainedCallback::new(1, 1, 1))
    }
    fn release_callback(&mut self, callback: &Callback) -> Result<(), Self::Error> {
        if !callback.is_current_in(&self.scope()) || !self.callback || *callback.callback() != 1 {
            return Err("callback unavailable");
        }
        self.callback = false;
        Ok(())
    }
    fn register_intrinsic(
        &mut self,
        name: &'static str,
        arity: u8,
        callback: NativeIntrinsic<Self>,
    ) -> Result<(), Self::Error> {
        if name != "paint" || arity != 1 {
            return Err("unsupported intrinsic");
        }
        self.intrinsic = Some(callback);
        Ok(())
    }
    fn invoke_retained_callback(
        &mut self,
        callback: &Callback,
        args: &[i64],
    ) -> Result<i64, Self::Error> {
        if !callback.is_current_in(&self.scope()) || !self.callback || *callback.callback() != 1 {
            return Err("callback unavailable");
        }
        self.intrinsic.ok_or("intrinsic unavailable")?(self, args)
    }
}

impl NativeHost for Guest {
    type Guest = Self;
    fn guest_mut(&mut self) -> &mut Self {
        self
    }
}

impl TypedNativeGuest for Guest {
    fn supports_intl(&self) -> bool {
        self.intl
    }
    fn install_document(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("install");
        self.register_intrinsic("paint", 1, |_, args| {
            args.first().copied().map(|v| v + 1).ok_or("missing value")
        })
    }
    fn execute_source(
        &mut self,
        doc: &mut BaseDocument,
        source: &str,
        _: Option<&Url>,
        kind: &ScriptKind,
    ) -> Result<(), Self::Error> {
        // This fixture has one precompiled entry. Unknown input is rejected,
        // never interpreted or reported as successful JavaScript execution.
        if source != "paint()" || *kind != ScriptKind::Classic {
            return Err("unknown compiled fixture");
        }
        self.trace.borrow_mut().push("execute");
        let seed = *self.storage.get("seed").ok_or("storage was not seeded")?;
        let callback = self.retain_callback(&7)?;
        let result = NativeHost::invoke_retained_callback(self, &callback, &[seed])
            .map_err(|_| "compiled call failed")?;
        self.release_callback(&callback)?;
        if !self.omit_write {
            let subject = doc
                .query_selector("#subject")
                .map_err(|_| "invalid selector")?
                .ok_or("missing subject")?;
            let text = *doc
                .get_node(subject)
                .ok_or("missing node")?
                .children
                .first()
                .ok_or("missing text")?;
            doc.mutate().set_node_text(text, &result.to_string());
        }
        Ok(())
    }
    fn install_import_map(&mut self, _: &str, _: Option<&Url>) -> Result<(), Self::Error> {
        Err("import maps unsupported")
    }
    fn dispatch_ui_event(&mut self, _: &mut BaseDocument, _: UiEvent) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("event");
        Ok(())
    }
    fn poll(
        &mut self,
        _: &mut BaseDocument,
        context: Option<Context>,
    ) -> Result<bool, Self::Error> {
        self.trace.borrow_mut().push("poll");
        if let Some(context) = context {
            context.waker().wake_by_ref();
        }
        Ok(true)
    }
    fn poll_for_settle(
        &mut self,
        _: &mut BaseDocument,
        _: Option<Context>,
        timers: bool,
    ) -> Result<bool, Self::Error> {
        self.trace
            .borrow_mut()
            .push(if timers { "timers" } else { "jobs" });
        Ok(true)
    }
    fn prepare_teardown(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("teardown");
        self.callback = false;
        Ok(())
    }
    fn dispose_document(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("dispose");
        Ok(())
    }
    fn finish_loading(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("ready");
        Ok(())
    }
    fn set_page_visible(&mut self, _: &mut BaseDocument, _: bool) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("visibility");
        Ok(())
    }
    fn freeze(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("freeze");
        Ok(())
    }
    fn finish_restore(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("restore");
        Ok(())
    }
    fn resume(&mut self, _: &mut BaseDocument) -> Result<(), Self::Error> {
        self.trace.borrow_mut().push("resume");
        Ok(())
    }
    fn storage_get(&mut self, key: &str) -> Result<Option<i64>, Self::Error> {
        Ok(self.storage.get(key).copied())
    }
    fn storage_set(&mut self, key: &str, value: i64) -> Result<(), Self::Error> {
        self.storage.insert(key.to_owned(), value);
        Ok(())
    }
    fn storage_remove(&mut self, key: &str) -> Result<(), Self::Error> {
        self.storage.remove(key);
        Ok(())
    }
    fn storage_clear(&mut self) -> Result<(), Self::Error> {
        self.storage.clear();
        Ok(())
    }
}

fn guest(trace: &Trace, intl: bool) -> Guest {
    Guest {
        trace: Rc::clone(trace),
        storage: BTreeMap::new(),
        intrinsic: None,
        callback: false,
        omit_write: std::env::args().any(|arg| arg == "--omit-dom-write"),
        intl,
    }
}

type Page = BoundScriptDocument<Guest, Guest>;

struct WakeCount(AtomicUsize);

impl std::task::Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn main() {
    let trace = Rc::new(RefCell::new(Vec::new()));
    let html = Arc::from(
        "<output id=subject>pending</output><template><script>inert</script></template><script nomodule>inert</script><script type=application/json>inert</script><script>paint()</script><script src='data:text/javascript,paint()'></script>",
    );
    let mut page = Page::from_html(html, DocumentConfig::default()).expect("parse fixture");
    assert!(
        page.execute_scripts().is_err(),
        "unbound execution must fail"
    );
    page.attach_native_host(guest(&trace, true))
        .expect("bind compiled guest");
    page.storage_set("seed", 8).expect("seed before scripts");
    page.execute_scripts().expect("execute compiled fixture");
    page.execute_scripts().expect("bootstrap is idempotent");
    let subject = page.inner().query_selector("#subject").unwrap().unwrap();
    assert_eq!(
        page.inner().get_node(subject).unwrap().text_content(),
        "9",
        "rendered-bootstrap"
    );
    assert_eq!(
        &*trace.borrow(),
        &["install", "execute", "execute", "ready"],
        "script-order-and-inert-subtrees"
    );
    page.handle_ui_event(UiEvent::AppleStandardKeybinding("selectAll:".into()));
    page.set_page_visible(false);
    page.freeze();
    assert!(!page.poll(None), "frozen guest must not run");
    page.resume();
    page.finish_restore();
    let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
    let waker = std::task::Waker::from(Arc::clone(&wakes));
    assert!(page.poll(Some(Context::from_waker(&waker))));
    assert_eq!(
        wakes.0.load(Ordering::Relaxed),
        1,
        "document-waker-propagation"
    );
    assert!(page.poll_for_settle(None, false));
    assert!(page.poll_for_settle(None, true));
    assert_eq!(
        &trace.borrow()[4..],
        &[
            "event",
            "visibility",
            "freeze",
            "resume",
            "restore",
            "poll",
            "jobs",
            "timers"
        ],
        "lifecycle-and-timer-policy"
    );
    assert_eq!(page.storage_get("seed").unwrap(), Some(8));
    page.storage_remove("seed").unwrap();
    assert_eq!(page.storage_get("seed").unwrap(), None);
    page.storage_set("seed", 3).unwrap();
    page.storage_clear().unwrap();
    assert_eq!(page.storage_get("seed").unwrap(), None);
    page.prepare_teardown();
    let calls = trace.borrow().len();
    assert!(!page.poll(None));
    assert!(page.execute_scripts().is_err());
    assert!(page.storage_set("seed", 1).is_err());
    page.handle_ui_event(UiEvent::AppleStandardKeybinding("selectAll:".into()));
    assert_eq!(trace.borrow().len(), calls, "retired-guest-entry");
    drop(page);
    assert_eq!(
        trace.borrow().iter().filter(|&&v| v == "teardown").count(),
        1
    );
    assert_eq!(trace.borrow().last(), Some(&"dispose"));
    let drop_trace = Rc::new(RefCell::new(Vec::new()));
    let page = Page::new_bound(
        Arc::from("<p>plain</p>"),
        DocumentConfig::default(),
        guest(&drop_trace, true),
    )
    .unwrap();
    drop(page);
    assert_eq!(
        &*drop_trace.borrow(),
        &["install", "teardown", "dispose"],
        "drop-cleanup"
    );
    #[cfg(feature = "intl")]
    {
        let mut page = Page::new_bound(
            Arc::from("<script>paint()</script>"),
            DocumentConfig::default(),
            guest(&drop_trace, false),
        )
        .unwrap();
        assert!(page.execute_scripts().unwrap_err().contains("Intl"));
    }
    println!(
        "PASS bound document driver: compiled fixture, DOM, storage, lifecycle and teardown; no JavaScript engine claimed"
    );
}
