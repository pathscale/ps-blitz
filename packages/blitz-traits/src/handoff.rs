//! Consumer-owned event batches with typed pointer publication.

use std::collections::{LinkedList, linked_list};
use std::sync::{Arc, Mutex, Weak};

struct Pending<T> {
    values: LinkedList<Arc<T>>,
    closed: bool,
}

struct Queue<T> {
    pending: Mutex<Pending<T>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// The single owner of an unbounded FIFO of typed event pointers.
///
/// Publication schedules the owner's wake callback on nagoya only when an
/// empty batch becomes nonempty. Taking a batch is one atomic transfer, with
/// no idle receive loop and no payload copies. Events published while a batch
/// is being handled form another batch and schedule another wake.
pub struct EventQueue<T>(Arc<Queue<T>>);

impl<T> EventQueue<T> {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Queue {
            pending: Mutex::new(Pending {
                values: LinkedList::new(),
                closed: false,
            }),
            wake: Arc::new(wake),
        }))
    }

    /// A publication capability that does not keep the consumer alive.
    pub fn target(&self) -> EventTarget<T> {
        EventTarget(Arc::downgrade(&self.0))
    }

    /// Take the current batch in publication order, releasing the lock before
    /// the caller handles any event or drops any payload.
    pub fn take_pending(&mut self) -> linked_list::IntoIter<Arc<T>> {
        std::mem::take(&mut self.0.pending.lock().unwrap().values).into_iter()
    }
}

impl<T> Drop for EventQueue<T> {
    fn drop(&mut self) {
        let pending = {
            let mut state = self.0.pending.lock().unwrap();
            state.closed = true;
            std::mem::take(&mut state.values)
        };
        drop(pending);
    }
}

/// Weak capability for handing an Arc to its consumer's event queue.
pub struct EventTarget<T>(Weak<Queue<T>>);

impl<T> Clone for EventTarget<T> {
    fn clone(&self) -> Self {
        Self(Weak::clone(&self.0))
    }
}

impl<T> EventTarget<T> {
    /// Returns the original pointer if the consumer has been dropped.
    /// Callers that later move the payload out must transfer a unique Arc.
    pub fn publish(&self, value: Arc<T>) -> Result<(), Arc<T>> {
        let Some(queue) = self.0.upgrade() else {
            return Err(value);
        };
        let needs_wake = {
            let mut pending = queue.pending.lock().unwrap();
            if pending.closed {
                return Err(value);
            }
            let empty = pending.values.is_empty();
            pending.values.push_back(value);
            empty
        };
        if needs_wake {
            let wake = Arc::clone(&queue.wake);
            // Dropping nagoya's JoinHandle detaches this finite wake task.
            let _ = nagoya::spawn(async move { wake() });
        }
        Ok(())
    }
}
