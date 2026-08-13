//! What the JavaScript side costs, per frame.
//!
//! The renderer has published real timings for a while: resolve, paint and
//! present. Script execution had none, so a profile could show a 4ms frame and
//! a UI that still felt slow, with nothing in between to look at. Every
//! reactive update, event handler, timer callback and microtask drain runs
//! through `ScriptDocument::poll`, so timing that one boundary accounts for the
//! whole language runtime without threading a clock through Boa.
//!
//! Deliberately mirrors `blitz_shell::frame_stats`: a bounded ring, means and
//! tails rather than a running average, and no data reported as zero when there
//! is no data at all.

use std::sync::Mutex;
use std::time::Duration;

/// How many polls to retain. Matches the frame ring so the two line up when
/// read together.
const CAPACITY: usize = 256;

/// A poll that ran JavaScript. Polls that found nothing to do are counted but
/// not retained: they are the idle case and would drag every average toward
/// zero, hiding the handler that actually costs something.
#[derive(Debug, Clone, Copy)]
struct Poll {
    duration: Duration,
}

/// Cumulative cost of one kind of work, so a poll can be attributed rather
/// than merely measured. "JavaScript is slow" is not actionable; "scroll
/// handlers cost 14ms of every 16ms poll" is.
#[derive(Debug, Default, Clone, Copy)]
struct Bucket {
    calls: u64,
    spent: Duration,
    worst: Duration,
}

impl Bucket {
    fn record(&mut self, duration: Duration) {
        self.calls += 1;
        self.spent += duration;
        if duration > self.worst {
            self.worst = duration;
        }
    }

    fn absorb(&mut self, other: &Bucket) {
        self.calls += other.calls;
        self.spent += other.spent;
        if other.worst > self.worst {
            self.worst = other.worst;
        }
    }
}

#[derive(Debug, Default)]
struct Log {
    /// Buckets keyed by a compile-time label, for call sites hot enough that
    /// allocating a `String` per call would be its own measurement error. DOM
    /// construction runs thousands of times per mount.
    statics: std::collections::BTreeMap<&'static str, Bucket>,
    /// Event names are dynamic, so they are interned into a small set rather
    /// than leaking a `String` per dispatch.
    dynamic: std::collections::BTreeMap<String, Bucket>,
    polls: Vec<Poll>,
    /// Every poll, including the ones that did no work.
    total: u64,
    /// Polls that actually ran script.
    productive: u64,
    /// Cumulative time in the script runtime, idle polls included.
    spent: Duration,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

thread_local! {
    /// Per-thread buckets for the static labels, folded into [`LOG`] once per
    /// poll.
    ///
    /// `record_static` runs per DOM node, so a 4,000-node mount calls it tens
    /// of thousands of times. Taking the process-global lock there cost more
    /// than several of the operations being timed, which inflated every
    /// absolute the profile reported: the instrument was a measurable share of
    /// the measurement. Script runs on one thread, so the accumulator can be
    /// thread-local and the hot path needs no synchronisation at all.
    static LOCAL_STATICS: std::cell::RefCell<Vec<(&'static str, Bucket)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Attribute a slice of script time to a fixed source, without allocating.
///
/// Use for anything called per DOM node. `record_work` takes a `&str` and
/// interns it, which is fine per event and far too expensive per element.
pub fn record_static(label: &'static str, duration: Duration) {
    // `try_with`/`try_borrow_mut` rather than the panicking forms: this runs
    // inside `Drop`, and a profiler that can panic during unwinding turns a
    // recoverable error into an abort.
    let _ = LOCAL_STATICS.try_with(|local| {
        let Ok(mut buckets) = local.try_borrow_mut() else {
            return;
        };
        // Linear scan over a fixed, tiny label set (one entry per DOM binding).
        // Cheaper than hashing or an ordered map at this size, and identical
        // literals share an address, so the common case is one word compare.
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(seen, _)| std::ptr::eq(*seen, label) || *seen == label)
        {
            bucket.record(duration);
            return;
        }
        let mut bucket = Bucket::default();
        bucket.record(duration);
        buckets.push((label, bucket));
    });
}

/// Fold the calling thread's static buckets into the shared log.
///
/// Only this thread's, by construction. Script and the diagnostics collection
/// that reads these both run on the document thread, so that is the thread
/// whose buckets matter; a reader on any other thread sees the totals as of the
/// last poll rather than a torn half-update.
fn drain_local_statics(log: &mut Log) {
    let _ = LOCAL_STATICS.try_with(|local| {
        let Ok(mut buckets) = local.try_borrow_mut() else {
            return;
        };
        for (label, bucket) in buckets.iter_mut() {
            log.statics.entry(*label).or_default().absorb(bucket);
            *bucket = Bucket::default();
        }
    });
}

/// Attribute a slice of script time to a named source.
///
/// Called from the runtime around timer callbacks and DOM event dispatch. The
/// label is the event name where there is one, so a profile says which handler
/// is expensive instead of only that something was.
pub fn record_work(label: &str, duration: Duration) {
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    let log = guard.get_or_insert_with(Log::default);
    log.dynamic
        .entry(label.to_string())
        .or_default()
        .record(duration);
}

/// The costliest sources seen so far, worst total first.
#[must_use]
pub fn work_breakdown() -> Vec<(String, u64, f64, f64)> {
    let Ok(mut guard) = LOG.lock() else {
        return Vec::new();
    };
    // Statics accumulate off-lock, so fold this thread's in before reading or
    // the breakdown reports the state as of the previous poll.
    let log = guard.get_or_insert_with(Log::default);
    drain_local_statics(log);
    let mut rows: Vec<(String, u64, f64, f64)> = log
        .statics
        .iter()
        .map(|(label, bucket)| {
            (
                (*label).to_string(),
                bucket.calls,
                bucket.spent.as_secs_f64() * 1_000.0,
                bucket.worst.as_secs_f64() * 1_000.0,
            )
        })
        .chain(log.dynamic.iter().map(|(label, bucket)| {
            (
                label.clone(),
                bucket.calls,
                bucket.spent.as_secs_f64() * 1_000.0,
                bucket.worst.as_secs_f64() * 1_000.0,
            )
        }))
        .collect();
    rows.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

/// Record one `poll`. Cheap enough to leave on: a lock and a push.
pub fn record_poll(duration: Duration, ran_script: bool) {
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    let log = guard.get_or_insert_with(Log::default);
    // Once per poll is the natural fold point: the lock is already held, and a
    // poll is the unit the rest of these numbers are reported in.
    drain_local_statics(log);
    log.total += 1;
    log.spent += duration;
    maybe_report(log);
    if !ran_script {
        return;
    }
    log.productive += 1;
    if log.polls.len() == CAPACITY {
        log.polls.remove(0);
    }
    log.polls.push(Poll { duration });
}

/// Mean, 95th percentile and worst case for the retained polls, in
/// milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScriptStatsSnapshot {
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Polls that ran script, out of the retained window.
    pub window_polls: u64,
    /// Every poll since launch.
    pub total_polls: u64,
    /// Polls that ran script since launch.
    pub productive_polls: u64,
    /// Total time in the script runtime since launch, in milliseconds.
    pub spent_ms: f64,
}

/// `None` until script has actually run, so a caller reports "no data" rather
/// than printing zeros that look like a measurement.
#[must_use]
pub fn latest_script_stats() -> Option<ScriptStatsSnapshot> {
    let guard = LOG.lock().ok()?;
    let log = guard.as_ref()?;
    if log.polls.is_empty() {
        return None;
    }
    let mut millis: Vec<f64> = log
        .polls
        .iter()
        .map(|poll| poll.duration.as_secs_f64() * 1_000.0)
        .collect();
    let sum: f64 = millis.iter().sum();
    let mean = sum / millis.len() as f64;
    millis.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Nearest rank, so a short window still reports a real observation rather
    // than an interpolation between two samples it does not have.
    let rank = ((millis.len() as f64) * 0.95).ceil() as usize;
    let p95 = millis[rank.saturating_sub(1).min(millis.len() - 1)];
    Some(ScriptStatsSnapshot {
        mean_ms: mean,
        p95_ms: p95,
        max_ms: *millis.last().unwrap_or(&0.0),
        window_polls: millis.len() as u64,
        total_polls: log.total,
        productive_polls: log.productive,
        spent_ms: log.spent.as_secs_f64() * 1_000.0,
    })
}

/// Times a scope and attributes it on drop.
///
/// Every early return and `?` in a DOM binding is an exit path, and a manual
/// stopwatch would miss most of them. This cannot.
///
/// Compiled out unless the `dom-stats` feature is on. The clock reads alone are
/// two `mach_absolute_time` calls per DOM operation, which a release build has
/// no reader for and should not pay. `debug-control` turns it on, so inspector
/// builds keep the attribution; it can also be enabled by itself to profile a
/// build shaped like the shipping one.
#[cfg(feature = "dom-stats")]
pub struct Timed {
    label: &'static str,
    started: std::time::Instant,
}

#[cfg(feature = "dom-stats")]
impl Timed {
    #[must_use]
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            started: std::time::Instant::now(),
        }
    }
}

#[cfg(feature = "dom-stats")]
impl Drop for Timed {
    fn drop(&mut self) {
        record_static(self.label, self.started.elapsed());
    }
}

/// The zero-cost stand-in. Same call sites, no clock, no bucket, no drop glue.
#[cfg(not(feature = "dom-stats"))]
pub struct Timed;

#[cfg(not(feature = "dom-stats"))]
impl Timed {
    #[must_use]
    #[inline(always)]
    pub fn new(_label: &'static str) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These share one process-global log, so they must not interleave. Without
    /// this the suite passes or fails depending on thread scheduling, which is
    /// worse than no test at all.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let guard = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *LOG.lock().unwrap() = None;
        // The static buckets outlive the shared log, so clearing only the log
        // would leak the previous test's DOM samples into the next one.
        LOCAL_STATICS.with(|local| local.borrow_mut().clear());
        guard
    }

    #[test]
    fn static_labels_reach_the_breakdown_without_locking_per_call() {
        let _serial = reset();
        for _ in 0..3 {
            record_static("dom:appendChild", Duration::from_micros(10));
        }
        record_static("dom:appendChild", Duration::from_micros(90));
        let rows = work_breakdown();
        let row = rows
            .iter()
            .find(|(label, ..)| label == "dom:appendChild")
            .expect("the static bucket is reported");
        assert_eq!(row.1, 4, "every call counted: {rows:?}");
        assert!(
            (row.3 - 0.09).abs() < 0.01,
            "the worst call survives the total: {rows:?}"
        );
    }

    #[test]
    fn folding_twice_does_not_double_count() {
        let _serial = reset();
        record_static("dom:createElement", Duration::from_micros(50));
        let first = work_breakdown();
        let second = work_breakdown();
        assert_eq!(
            first, second,
            "a drained bucket must not be added to the shared log again"
        );
    }

    #[test]
    fn nothing_is_reported_before_script_runs() {
        let _serial = reset();
        record_poll(Duration::from_millis(5), false);
        assert!(
            latest_script_stats().is_none(),
            "idle polls are not a measurement of script cost"
        );
    }

    #[test]
    fn the_worst_poll_survives_the_mean() {
        let _serial = reset();
        for _ in 0..40 {
            record_poll(Duration::from_millis(1), true);
        }
        record_poll(Duration::from_millis(60), true);
        let stats = latest_script_stats().expect("script ran");
        assert!(stats.mean_ms < 3.0, "one outlier must not move the mean");
        assert!(
            (stats.max_ms - 60.0).abs() < 1.0,
            "the outlier is the whole point: {stats:?}"
        );
    }

    #[test]
    fn idle_polls_are_counted_without_diluting_the_window() {
        let _serial = reset();
        record_poll(Duration::from_millis(2), true);
        for _ in 0..10 {
            record_poll(Duration::from_micros(10), false);
        }
        let stats = latest_script_stats().expect("script ran");
        assert_eq!(stats.window_polls, 1);
        assert_eq!(stats.total_polls, 11);
        assert_eq!(stats.productive_polls, 1);
    }
}

/// Print what the script runtime is costing, once a second, under
/// `BLITZ_SCRIPT_STATS=1`.
///
/// [`latest_script_stats`] existed with no caller anywhere in the workspace, so
/// none of this was reachable from a running browser: the frame log accounts
/// for resolve, paint and present on the main thread, and script ran in the gap
/// between them with nothing measuring it. On a page whose frame loop never
/// settles, that gap is where unexplained CPU hides.
fn maybe_report(log: &Log) {
    use std::sync::OnceLock;
    use std::time::Instant;

    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        matches!(
            std::env::var("BLITZ_SCRIPT_STATS").ok().as_deref(),
            Some("1") | Some("true")
        )
    }) {
        return;
    }

    static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    let Ok(mut last) = LAST.lock() else { return };
    let now = Instant::now();
    if last.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
        return;
    }
    let elapsed = last.map(|t| now.duration_since(t));
    *last = Some(now);
    drop(last);

    // Share of wall clock spent inside the script runtime since the last line,
    // which is the number that says whether script is the cost or a rounding
    // error. Cumulative totals cannot answer that on a long-running page.
    let spent_ms = log.spent.as_secs_f64() * 1000.0;
    static PREV_SPENT: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);
    let delta_ms = if let Ok(mut prev) = PREV_SPENT.lock() {
        let d = spent_ms - *prev;
        *prev = spent_ms;
        d
    } else {
        0.0
    };
    let share = elapsed
        .map(|e| delta_ms / (e.as_secs_f64() * 1000.0) * 100.0)
        .unwrap_or(0.0);

    eprintln!(
        "[script] polls={} productive={} spent={spent_ms:.0}ms last_second={delta_ms:.1}ms ({share:.1}% of wall clock)",
        log.total, log.productive,
    );
}
