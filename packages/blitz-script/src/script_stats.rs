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

/// Attribute a slice of script time to a fixed source, without allocating.
///
/// Use for anything called per DOM node. `record_work` takes a `&str` and
/// interns it, which is fine per event and far too expensive per element.
pub fn record_static(label: &'static str, duration: Duration) {
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    let log = guard.get_or_insert_with(Log::default);
    let bucket = log.statics.entry(label).or_default();
    bucket.calls += 1;
    bucket.spent += duration;
    if duration > bucket.worst {
        bucket.worst = duration;
    }
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
    let bucket = log.dynamic.entry(label.to_string()).or_default();
    bucket.calls += 1;
    bucket.spent += duration;
    if duration > bucket.worst {
        bucket.worst = duration;
    }
}

/// The costliest sources seen so far, worst total first.
#[must_use]
pub fn work_breakdown() -> Vec<(String, u64, f64, f64)> {
    let Ok(guard) = LOG.lock() else {
        return Vec::new();
    };
    let Some(log) = guard.as_ref() else {
        return Vec::new();
    };
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
    log.total += 1;
    log.spent += duration;
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
        guard
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

/// Times a scope and attributes it on drop.
///
/// Every early return and `?` in a DOM binding is an exit path, and a manual
/// stopwatch would miss most of them. This cannot.
pub struct Timed {
    label: &'static str,
    started: std::time::Instant,
}

impl Timed {
    #[must_use]
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        record_static(self.label, self.started.elapsed());
    }
}
