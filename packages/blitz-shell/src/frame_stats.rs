//! Process-global publication of real per-frame timings.
//!
//! [`View::redraw`](crate::View::redraw) already measures every frame it
//! presents: how long `resolve` (style plus layout) took, how long `paint_scene`
//! took, and how long the renderer took to submit and present. Those numbers used
//! to exist only inside a once-per-second `[blitz-frame]` log line, so anything
//! that wanted to report renderer performance had no way to read them. The MCP
//! diagnostics endpoint in `tauri-runtime-blitz` is the case that motivated this
//! module: with no accessor it timed its own snapshot collection and reported that
//! as frame cost, which measures the observer rather than the application.
//!
//! Recording here is unconditional. `BLITZ_FRAME_STATS` still gates the log line,
//! but gating the shared data on it too would mean a normally launched app reports
//! no frame data at all, which is what pushed the previous consumer into inventing
//! numbers.
//!
//! All windows in the process feed the same log. A multi-window app therefore sees
//! its windows interleaved; the aggregate still describes real presented frames,
//! it just does not attribute them per window.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use web_time::Instant;

/// Frames retained for the aggregate statistics. At 120 Hz this is a little over
/// two seconds of history, which is enough for a stable p95 while keeping the
/// buffer small enough to sort on every read.
const WINDOW_CAPACITY: usize = 256;

/// Frame intervals longer than this are idle gaps, not slow frames. Blitz is
/// deliberately zero-FPS when nothing changes, so counting the wait between two
/// interaction bursts would wreck both the fps figure and the interval p95. This
/// matches the threshold the `[blitz-frame]` log line has always used.
const MAX_ACTIVE_INTERVAL: Duration = Duration::from_millis(100);

/// A frame is considered to have missed a refresh when it arrives later than this
/// multiple of the display's refresh period.
const MISSED_REFRESH_FACTOR: f64 = 1.5;

#[derive(Clone, Copy)]
struct FrameRecord {
    started: Instant,
    resolve: Duration,
    paint: Duration,
    renderer: Duration,
}

#[derive(Default)]
struct FrameLog {
    frames: VecDeque<FrameRecord>,
    total: u64,
    refresh_millihertz: Option<u32>,
}

static FRAME_LOG: LazyLock<Mutex<FrameLog>> = LazyLock::new(|| Mutex::new(FrameLog::default()));

/// Mean, 95th percentile and worst case for one timing series, in milliseconds.
///
/// The percentile and the maximum are carried alongside the mean on purpose: a
/// one-second average hides the single 40 ms frame that is the only thing the
/// user actually perceives.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimingStats {
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// Timings of one presented frame, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameTimings {
    /// Style recalculation and layout. Blitz runs both inside a single `resolve`
    /// pass and does not time them separately, so this is their combined cost.
    pub resolve_ms: f64,
    /// Scene building, i.e. the `paint_scene` call that turns the resolved
    /// document into renderer commands.
    pub paint_ms: f64,
    /// Everything the renderer did around scene building: encoding, GPU submit
    /// and present. The renderer reports this as one figure.
    pub renderer_ms: f64,
    /// `resolve_ms + paint_ms + renderer_ms`. This is CPU time spent inside
    /// `redraw`, not the wall time from input to pixels on screen.
    pub total_ms: f64,
    /// How long ago this frame started, measured when the snapshot was taken.
    /// A large value means the app has been idle and the numbers are stale.
    pub age_ms: f64,
}

/// Aggregate view of the recently presented frames.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameStatsSnapshot {
    /// Frames presented since process start.
    pub frames_total: u64,
    /// Frames the aggregate statistics below were computed over.
    pub window_frames: u64,
    /// The most recently presented frame.
    pub latest: FrameTimings,
    pub resolve: TimingStats,
    pub paint: TimingStats,
    pub renderer: TimingStats,
    /// `resolve + paint + renderer` per frame.
    pub frame_total: TimingStats,
    /// Gap between the starts of consecutive frames, with idle gaps excluded.
    pub interval: TimingStats,
    /// Frames per second across the active intervals only. Zero when the window
    /// holds fewer than two frames, or when every gap in it was an idle gap.
    pub active_fps: f64,
    /// Active intervals longer than 1.5 display refresh periods. Always zero when
    /// `display_refresh_hz` is unknown, because there is nothing to compare to.
    pub missed_refreshes: u64,
    /// Refresh rate reported by the monitor the window was created on, when the
    /// platform exposes it.
    pub display_refresh_hz: Option<f64>,
}

/// Publish the display refresh rate so [`FrameStatsSnapshot::missed_refreshes`]
/// has something to compare frame intervals against.
pub(crate) fn set_display_refresh_millihertz(rate: Option<u32>) {
    if let Ok(mut log) = FRAME_LOG.lock() {
        // Keep the first rate we learn rather than letting a second window with no
        // reported rate erase it.
        if rate.is_some() {
            log.refresh_millihertz = rate;
        }
    }
}

/// Record one presented frame. Called from `View::redraw` for every frame.
pub(crate) fn record_frame(
    started: Instant,
    resolve: Duration,
    paint: Duration,
    renderer: Duration,
) {
    // A poisoned lock means some other thread panicked mid-update. Performance
    // bookkeeping is not worth propagating that panic into the render loop.
    let Ok(mut log) = FRAME_LOG.lock() else {
        return;
    };
    if log.frames.len() == WINDOW_CAPACITY {
        log.frames.pop_front();
    }
    log.frames.push_back(FrameRecord {
        started,
        resolve,
        paint,
        renderer,
    });
    log.total = log.total.saturating_add(1);
}

/// Read the most recent frame timings.
///
/// Returns `None` until the first frame has been presented, so that callers can
/// report "no data yet" instead of reporting zeros as if they were measurements.
pub fn latest_frame_stats() -> Option<FrameStatsSnapshot> {
    let log = FRAME_LOG.lock().ok()?;
    summarise(
        &log.frames,
        log.total,
        log.refresh_millihertz,
        Instant::now(),
    )
}

fn summarise(
    frames: &VecDeque<FrameRecord>,
    frames_total: u64,
    refresh_millihertz: Option<u32>,
    now: Instant,
) -> Option<FrameStatsSnapshot> {
    let newest = frames.back()?;

    let mut resolve = Vec::with_capacity(frames.len());
    let mut paint = Vec::with_capacity(frames.len());
    let mut renderer = Vec::with_capacity(frames.len());
    let mut frame_total = Vec::with_capacity(frames.len());
    let mut intervals = Vec::with_capacity(frames.len());
    let mut interval_sum = Duration::ZERO;
    let mut missed_refreshes = 0u64;

    let target = refresh_millihertz
        .filter(|rate| *rate > 0)
        .map(|rate| Duration::from_secs_f64(1000.0 / f64::from(rate)));

    let mut previous: Option<Instant> = None;
    for frame in frames {
        resolve.push(to_ms(frame.resolve));
        paint.push(to_ms(frame.paint));
        renderer.push(to_ms(frame.renderer));
        frame_total.push(to_ms(frame.resolve + frame.paint + frame.renderer));

        if let Some(previous) = previous.replace(frame.started) {
            let interval = frame.started.saturating_duration_since(previous);
            if interval <= MAX_ACTIVE_INTERVAL {
                intervals.push(to_ms(interval));
                interval_sum += interval;
                if target.is_some_and(|target| interval > target.mul_f64(MISSED_REFRESH_FACTOR)) {
                    missed_refreshes += 1;
                }
            }
        }
    }

    let active_fps = if interval_sum.is_zero() {
        0.0
    } else {
        intervals.len() as f64 / interval_sum.as_secs_f64()
    };

    Some(FrameStatsSnapshot {
        frames_total,
        window_frames: frames.len() as u64,
        latest: FrameTimings {
            resolve_ms: to_ms(newest.resolve),
            paint_ms: to_ms(newest.paint),
            renderer_ms: to_ms(newest.renderer),
            total_ms: to_ms(newest.resolve + newest.paint + newest.renderer),
            age_ms: to_ms(now.saturating_duration_since(newest.started)),
        },
        resolve: TimingStats::from_samples(&mut resolve),
        paint: TimingStats::from_samples(&mut paint),
        renderer: TimingStats::from_samples(&mut renderer),
        frame_total: TimingStats::from_samples(&mut frame_total),
        interval: TimingStats::from_samples(&mut intervals),
        active_fps,
        missed_refreshes,
        display_refresh_hz: refresh_millihertz.map(|rate| f64::from(rate) / 1000.0),
    })
}

fn to_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl TimingStats {
    /// Sorts `samples` in place and reduces them to mean, p95 and max.
    ///
    /// p95 uses the nearest-rank definition, so with fewer than 20 samples it
    /// simply reports the worst one. That is the honest answer for a short
    /// window: there is no 95th percentile to interpolate towards.
    fn from_samples(samples: &mut [f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(f64::total_cmp);
        let count = samples.len();
        let sum: f64 = samples.iter().sum();
        let rank = ((count as f64) * 0.95).ceil() as usize;
        let index = rank.clamp(1, count) - 1;
        Self {
            mean_ms: sum / count as f64,
            p95_ms: samples[index],
            max_ms: samples[count - 1],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(frames: &[(u64, u64, u64, u64)]) -> VecDeque<FrameRecord> {
        let origin = Instant::now();
        frames
            .iter()
            .map(|(offset_ms, resolve, paint, renderer)| FrameRecord {
                started: origin + Duration::from_millis(*offset_ms),
                resolve: Duration::from_millis(*resolve),
                paint: Duration::from_millis(*paint),
                renderer: Duration::from_millis(*renderer),
            })
            .collect()
    }

    #[test]
    fn empty_log_reports_nothing_rather_than_zeroes() {
        assert!(summarise(&VecDeque::new(), 0, None, Instant::now()).is_none());
    }

    #[test]
    fn latest_frame_is_the_newest_record() {
        let frames = log(&[(0, 1, 2, 3), (16, 4, 5, 6)]);
        let stats = summarise(&frames, 2, None, Instant::now()).unwrap();
        assert_eq!(stats.latest.resolve_ms, 4.0);
        assert_eq!(stats.latest.paint_ms, 5.0);
        assert_eq!(stats.latest.renderer_ms, 6.0);
        assert_eq!(stats.latest.total_ms, 15.0);
        assert_eq!(stats.frames_total, 2);
        assert_eq!(stats.window_frames, 2);
    }

    #[test]
    fn worst_frame_survives_the_mean() {
        let mut frames: Vec<(u64, u64, u64, u64)> = (0..40).map(|i| (i * 16, 1, 1, 1)).collect();
        frames.push((40 * 16, 30, 1, 1));
        let stats = summarise(&log(&frames), 41, None, Instant::now()).unwrap();
        assert!(stats.resolve.mean_ms < 2.0);
        assert_eq!(stats.resolve.max_ms, 30.0);
        assert_eq!(stats.resolve.p95_ms, 1.0);
        assert_eq!(stats.frame_total.max_ms, 32.0);
    }

    #[test]
    fn idle_gaps_do_not_count_as_slow_frames() {
        // Two 16 ms frames, then a five second idle gap, then another frame.
        let frames = log(&[(0, 1, 1, 1), (16, 1, 1, 1), (5016, 1, 1, 1)]);
        let stats = summarise(&frames, 3, Some(60_000), Instant::now()).unwrap();
        assert_eq!(stats.interval.max_ms, 16.0);
        assert!((stats.active_fps - 62.5).abs() < 0.01);
        assert_eq!(stats.missed_refreshes, 0);
    }

    #[test]
    fn a_late_frame_counts_as_a_missed_refresh() {
        // 60 Hz means a 16.67 ms period; 40 ms is well past the 1.5x threshold.
        let frames = log(&[(0, 1, 1, 1), (40, 1, 1, 1)]);
        let stats = summarise(&frames, 2, Some(60_000), Instant::now()).unwrap();
        assert_eq!(stats.missed_refreshes, 1);
        assert_eq!(stats.display_refresh_hz, Some(60.0));
    }

    #[test]
    fn missed_refreshes_stay_zero_without_a_known_refresh_rate() {
        let frames = log(&[(0, 1, 1, 1), (90, 1, 1, 1)]);
        let stats = summarise(&frames, 2, None, Instant::now()).unwrap();
        assert_eq!(stats.missed_refreshes, 0);
        assert_eq!(stats.display_refresh_hz, None);
    }

    #[test]
    fn p95_picks_the_nearest_rank() {
        let mut samples: Vec<f64> = (1..=20).map(f64::from).collect();
        let stats = TimingStats::from_samples(&mut samples);
        assert_eq!(stats.p95_ms, 19.0);
        assert_eq!(stats.max_ms, 20.0);
        assert_eq!(stats.mean_ms, 10.5);
    }

    #[test]
    fn recording_publishes_to_the_process_global_log() {
        record_frame(
            Instant::now(),
            Duration::from_millis(2),
            Duration::from_millis(3),
            Duration::from_millis(4),
        );
        let stats = latest_frame_stats().expect("a frame was just recorded");
        assert!(stats.frames_total >= 1);
        assert!(stats.latest.total_ms >= 9.0);
    }
}
