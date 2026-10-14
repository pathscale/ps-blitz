use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// A receipt contains no DOM, realm, callbacks, request bodies or guest roots.
pub struct Teardown {
    owners: AtomicUsize,
    retiring: AtomicBool,
    readers: Mutex<Vec<Waker>>,
    children: Mutex<Vec<Arc<Teardown>>>,
}

impl Teardown {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { owners: AtomicUsize::new(0), retiring: AtomicBool::new(false),
            readers: Mutex::new(Vec::new()), children: Mutex::new(Vec::new()) })
    }

    pub fn lease(self: &Arc<Self>) -> ReleaseLease {
        self.owners.fetch_add(1, Ordering::AcqRel);
        ReleaseLease(Arc::clone(self))
    }

    pub fn retire(&self, children: Vec<Arc<Teardown>>) {
        self.children.lock().unwrap().extend(children);
        self.retiring.store(true, Ordering::Release);
        self.wake();
    }

    fn wake(&self) {
        for reader in std::mem::take(&mut *self.readers.lock().unwrap()) { reader.wake(); }
    }

    pub fn complete(&self) -> bool {
        self.retiring.load(Ordering::Acquire) && self.owners.load(Ordering::Acquire) == 0
            && self.children.lock().unwrap().iter().all(|child| child.complete())
    }

    fn poll_release(&self, cx: &mut Context<'_>) -> Poll<()> {
        {
            let mut readers = self.readers.lock().unwrap();
            if !readers.iter().any(|reader| reader.will_wake(cx.waker())) { readers.push(cx.waker().clone()); }
        }
        let mut children_ready = true;
        for child in self.children.lock().unwrap().iter() { children_ready &= child.poll_release(cx).is_ready(); }
        if children_ready && self.retiring.load(Ordering::Acquire) && self.owners.load(Ordering::Acquire) == 0 {
            let readers = std::mem::take(&mut *self.readers.lock().unwrap());
            drop(readers);
            Poll::Ready(())
        } else { Poll::Pending }
    }

    pub fn released(self: Arc<Self>) -> Released { Released(self) }
}

pub struct ReleaseLease(Arc<Teardown>);

impl Drop for ReleaseLease {
    fn drop(&mut self) {
        if self.0.owners.fetch_sub(1, Ordering::AcqRel) == 1 { self.0.wake(); }
    }
}

#[cfg(feature = "custom-widget")]
pub struct ResourceRetirement {
    pub resources: Vec<anyrender::ResourceId>,
    pub leases: Vec<ReleaseLease>,
}

pub struct Released(Arc<Teardown>);

impl Future for Released {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> { self.0.poll_release(cx) }
}
