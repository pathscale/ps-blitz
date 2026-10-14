//! Consumer-owned event batches with typed pointer publication.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

struct Segment<T> {
    producer: spsc::Producer<Arc<T>>,
    consumer: spsc::Consumer<Arc<T>>,
}

struct Pending<T> {
    segments: VecDeque<Segment<T>>,
    closed: bool,
}

struct Queue<T> {
    pending: Mutex<Pending<T>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// The single owner of an unbounded FIFO of typed event pointers.
///
/// Publication schedules the owner's wake callback on nagoya only when an
/// empty batch becomes nonempty. Producers share one serialized ring lane,
/// following the debug-control handoff. Full rings append another segment so
/// publication stays unbounded. Taking a batch transfers the ring handles in
/// one operation, releasing the lock before any event is handled. There is no
/// idle receive loop and no payload copying.
pub struct EventQueue<T>(Arc<Queue<T>>);

impl<T> EventQueue<T> {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Queue {
            pending: Mutex::new(Pending {
                segments: VecDeque::new(),
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
    pub fn take_pending(&mut self) -> EventBatch<T> {
        EventBatch(std::mem::take(&mut self.0.pending.lock().unwrap().segments))
    }
}

impl<T> Drop for EventQueue<T> {
    fn drop(&mut self) {
        let pending = {
            let mut state = self.0.pending.lock().unwrap();
            state.closed = true;
            std::mem::take(&mut state.segments)
        };
        drop(pending);
    }
}

/// A finite batch of already published event pointers, owned by the consumer.
pub struct EventBatch<T>(VecDeque<Segment<T>>);

impl<T> Iterator for EventBatch<T> {
    type Item = Arc<T>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(segment) = self.0.front() {
            if let Some(value) = segment.consumer.try_pop() {
                return Some(value);
            }
            self.0.pop_front();
        }
        None
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
            let empty = pending.segments.is_empty();
            let value = match pending.segments.back() {
                Some(segment) => match segment.producer.try_push(value) {
                    None => return Ok(()),
                    Some(value) => value,
                },
                None => value,
            };
            let (producer, consumer) = spsc::make(64);
            assert!(producer.try_push(value).is_none());
            pending.segments.push_back(Segment { producer, consumer });
            empty
        };
        if needs_wake {
            let wake = Arc::clone(&queue.wake);
            // Dropping nagoya's JoinHandle detaches this finite wake task.
            drop(nagoya::spawn(async move { wake() }));
        }
        Ok(())
    }
}
