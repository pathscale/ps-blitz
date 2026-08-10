//! Timer support (`setTimeout` / `setInterval` / `requestAnimationFrame`)

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use boa_engine::JsValue;
use boa_engine::object::JsObject;
use rustc_hash::FxHashSet;
use web_time::{Duration, Instant};

pub(crate) struct Timer {
    pub id: u64,
    pub deadline: Instant,
    /// `Some` for `setInterval` timers, which reschedule themselves.
    pub interval: Option<Duration>,
    pub callback: JsObject,
    pub args: Vec<JsValue>,
}

/// Ordered by deadline, then by id so that two timers due at the same instant
/// fire in the order they were created, as they would in a browser.
///
/// Reversed, because [`BinaryHeap`] is a max-heap and the soonest deadline is
/// the one to pop first.
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.id == other.id
    }
}

impl Eq for Timer {}

/// Pending timers, ordered by deadline.
///
/// Was a `Vec` scanned linearly, so every operation was O(n) including
/// `next_deadline`, which `arm_timer_thread` calls after every `eval`, every UI
/// event and every poll. Draining a queue of n script tasks therefore cost n
/// full scans of every pending timer. A heap makes the scheduling decision O(1)
/// and each pop O(log n).
#[derive(Default)]
pub(crate) struct TimerQueue {
    next_id: u64,
    timers: BinaryHeap<Timer>,
    /// Ids that are still scheduled.
    ///
    /// Removing from the middle of a heap means rebuilding it, so cancellation
    /// is lazy: `remove` drops the id here and the heap entry is skipped when
    /// it surfaces. Tracking what is live rather than what was cancelled keeps
    /// this the size of the pending set, so cancelling an id that already fired
    /// costs nothing and retains nothing.
    live: FxHashSet<u64>,
}

impl TimerQueue {
    pub fn add(
        &mut self,
        delay: Duration,
        interval: Option<Duration>,
        callback: JsObject,
        args: Vec<JsValue>,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.live.insert(id);
        self.timers.push(Timer {
            id,
            deadline: Instant::now() + delay,
            interval,
            callback,
            args,
        });
        id
    }

    pub fn remove(&mut self, id: u64) {
        if !self.live.remove(&id) {
            return;
        }
        self.discard_dead_front();
    }

    /// The deadline of the timer which is due soonest (if any).
    ///
    /// The front is always a live timer: every mutation discards dead entries
    /// that reach it. Without that the event loop would arm a wakeup for a
    /// timer that has been cancelled and will never run.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.timers.peek().map(|timer| timer.deadline)
    }

    /// Remove and return all timers that are due at `now`, soonest first.
    /// Interval timers are rescheduled.
    pub fn take_due(&mut self, now: Instant) -> Vec<Timer> {
        let mut due: Vec<Timer> = Vec::new();

        while let Some(timer) = self.timers.peek() {
            if timer.deadline > now {
                break;
            }
            let timer = self.timers.pop().expect("peeked");
            if !self.live.contains(&timer.id) {
                continue;
            }
            match timer.interval {
                // Rescheduled before the callback runs, and never at `now`
                // itself, so an interval cannot be collected twice by one
                // drain. The id stays live so `clearInterval` still reaches it.
                Some(interval) => self.timers.push(Timer {
                    id: timer.id,
                    deadline: now + interval.max(Duration::from_millis(1)),
                    interval: timer.interval,
                    callback: timer.callback.clone(),
                    args: timer.args.clone(),
                }),
                None => {
                    self.live.remove(&timer.id);
                }
            }
            due.push(timer);
        }

        self.discard_dead_front();
        due
    }

    /// Drop cancelled entries that have reached the front of the heap, so that
    /// [`next_deadline`](Self::next_deadline) never reports one.
    fn discard_dead_front(&mut self) {
        while let Some(timer) = self.timers.peek() {
            if self.live.contains(&timer.id) {
                return;
            }
            self.timers.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boa_engine::Context;

    /// Timers hold a JS callback, but none of the ordering or cancellation
    /// logic touches it, so any object stands in.
    fn callback(context: &mut Context) -> JsObject {
        JsObject::with_object_proto(context.intrinsics())
    }

    #[test]
    fn the_soonest_deadline_is_the_one_reported() {
        let mut context = Context::default();
        let mut queue = TimerQueue::default();
        queue.add(
            Duration::from_millis(500),
            None,
            callback(&mut context),
            Vec::new(),
        );
        let near = Instant::now() + Duration::from_millis(10);
        queue.add(
            Duration::from_millis(10),
            None,
            callback(&mut context),
            Vec::new(),
        );
        queue.add(
            Duration::from_millis(900),
            None,
            callback(&mut context),
            Vec::new(),
        );

        let deadline = queue.next_deadline().expect("three timers are pending");
        assert!(
            deadline <= near + Duration::from_millis(5),
            "the heap must surface the nearest deadline, not the first inserted"
        );
    }

    #[test]
    fn same_deadline_fires_in_creation_order() {
        let mut context = Context::default();
        let mut queue = TimerQueue::default();
        let first = queue.add(
            Duration::from_millis(0),
            None,
            callback(&mut context),
            Vec::new(),
        );
        let second = queue.add(
            Duration::from_millis(0),
            None,
            callback(&mut context),
            Vec::new(),
        );

        let due = queue.take_due(Instant::now() + Duration::from_millis(1));
        let ids: Vec<u64> = due.iter().map(|timer| timer.id).collect();
        assert_eq!(ids, vec![first, second]);
    }

    #[test]
    fn a_cancelled_timeout_never_runs_and_stops_arming_the_loop() {
        let mut context = Context::default();
        let mut queue = TimerQueue::default();
        let id = queue.add(
            Duration::from_millis(0),
            None,
            callback(&mut context),
            Vec::new(),
        );
        queue.remove(id);

        assert!(
            queue.next_deadline().is_none(),
            "a cancelled timer must not keep waking the event loop"
        );
        assert!(queue.take_due(Instant::now()).is_empty());
    }

    #[test]
    fn an_interval_reschedules_until_cancelled() {
        let mut context = Context::default();
        let mut queue = TimerQueue::default();
        let id = queue.add(
            Duration::from_millis(0),
            Some(Duration::from_millis(1)),
            callback(&mut context),
            Vec::new(),
        );

        let now = Instant::now() + Duration::from_millis(1);
        assert_eq!(queue.take_due(now).len(), 1);
        assert!(
            queue.next_deadline().is_some(),
            "an interval reschedules itself"
        );

        queue.remove(id);
        assert!(
            queue.next_deadline().is_none(),
            "clearInterval has to reach the rescheduled copy"
        );
    }

    #[test]
    fn cancelling_an_id_that_already_fired_retains_nothing() {
        let mut context = Context::default();
        let mut queue = TimerQueue::default();
        let id = queue.add(
            Duration::from_millis(0),
            None,
            callback(&mut context),
            Vec::new(),
        );
        // A long-lived interval keeps the heap non-empty, which is the case
        // where a set of cancelled ids would grow without bound.
        queue.add(
            Duration::from_secs(30),
            Some(Duration::from_secs(30)),
            callback(&mut context),
            Vec::new(),
        );

        assert_eq!(queue.take_due(Instant::now()).len(), 1);
        for _ in 0..1_000 {
            queue.remove(id);
        }
        assert_eq!(
            queue.live.len(),
            1,
            "only the pending interval is tracked: {:?}",
            queue.live
        );
    }
}
