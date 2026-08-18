//! Process-wide switch for the intrusive performance collectors.
//!
//! Inspection and input control do not need these collectors. They stay dormant
//! until the embedder explicitly starts a profiling session, so shipping the
//! capability does not mean continuously paying clocks, locks, maps, or sample
//! retention.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Whether this process is *allowed* to sample, not whether it is sampling.
///
/// The distinction is the point. This used to be the activation switch, so a
/// profile that had been enabled once began collecting at boot and kept
/// collecting for the life of the process, for a reader that in the ordinary
/// case never attached: the readers are the inspector and `blitz-bench`, and
/// both are separate processes. Samples that go nowhere have no value and are
/// not free.
///
/// Permission is now separate from activation. Collection runs only while a
/// consumer holds a [`DeepProfilingGuard`], and this flag decides whether one
/// can be taken at all.
static DEEP_PROFILING_PERMITTED: AtomicBool = AtomicBool::new(false);

/// How many consumers currently want samples.
///
/// A counter rather than a flag because two consumers must be able to overlap:
/// the inspector attaching while a benchmark is mid-profile is normal, and the
/// first one to finish must not stop the second one's collection.
static DEEP_PROFILING_CONSUMERS: AtomicUsize = AtomicUsize::new(0);

/// The two owner-controlled debug capabilities shared by Blitz embedders.
///
/// Socket lifecycle belongs to the embedder (for example tauri-runtime-blitz
/// or Chuzz), while the engine profiling decision is common to every stack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebugOptions {
    pub inspection_and_agent_control: bool,
    pub deep_intrusive_profiling: bool,
}

impl DebugOptions {
    /// Whether the intrusive collectors should run.
    ///
    /// This is the deep-profiling switch alone. It was once ANDed with
    /// `inspection_and_agent_control`, on the reasoning that samples are only
    /// useful while a plane exists to read them back — but the collectors also
    /// feed the frame log and phase timings, which need no socket, and the AND
    /// made the embedder's toggle silently inert with inspection off. Turning
    /// inspection off and on then lost the setting entirely.
    ///
    /// Kept as a named method rather than a field read because embedders call
    /// it across a version boundary.
    #[must_use]
    pub const fn effective_deep_profiling(self) -> bool {
        self.deep_intrusive_profiling
    }
}

/// Permit or forbid intrusive collection in this process.
///
/// This is the owner's toggle, and it starts nothing on its own. Turning it off
/// while consumers are still attached also stops collection, because permission
/// is withdrawn: [`deep_profiling_enabled`] requires both.
///
/// Relaxed ordering is sufficient: this is a performance mode, not a memory
/// publication boundary. Each whole frame, resolve, or script section observes
/// either state and the next section observes a newly selected state.
pub fn set_deep_profiling_permitted(permitted: bool) {
    DEEP_PROFILING_PERMITTED.store(permitted, Ordering::Relaxed);
}

/// Whether the owner has allowed sampling, regardless of any consumer.
#[inline]
#[must_use]
pub fn deep_profiling_permitted() -> bool {
    DEEP_PROFILING_PERMITTED.load(Ordering::Relaxed)
}

/// Ask for samples, for as long as the returned guard is held.
///
/// `None` means the profile does not permit sampling. That check lives here
/// rather than at each call site, so a consumer cannot start collection by
/// forgetting it.
///
/// The guard is the whole reason this is not a bare `start()`/`stop()` pair: an
/// early return, a `?`, or a panic on the way out of a profiling request would
/// otherwise leave the collectors running for the life of the process, which is
/// the failure this function exists to prevent.
#[must_use = "sampling stops as soon as the guard is dropped"]
pub fn begin_deep_profiling() -> Option<DeepProfilingGuard> {
    if !deep_profiling_permitted() {
        return None;
    }
    DEEP_PROFILING_CONSUMERS.fetch_add(1, Ordering::Relaxed);
    Some(DeepProfilingGuard { _private: () })
}

/// How many consumers are currently holding sampling open.
#[inline]
#[must_use]
pub fn deep_profiling_consumers() -> usize {
    DEEP_PROFILING_CONSUMERS.load(Ordering::Relaxed)
}

/// Holds intrusive collection open. Sampling stops when the last one drops.
///
/// Not constructible except through [`begin_deep_profiling`], so the count can
/// only be raised by a consumer that passed the permission check, and not
/// `Clone`, so one guard is one consumer.
#[derive(Debug)]
pub struct DeepProfilingGuard {
    _private: (),
}

impl Drop for DeepProfilingGuard {
    fn drop(&mut self) {
        // Saturating rather than wrapping: a count that underflowed would read
        // as "a consumer is attached" forever, which is precisely the state
        // this type exists to make impossible.
        let _ =
            DEEP_PROFILING_CONSUMERS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_sub(1))
            });
    }
}

/// Whether intrusive collection is active for the next whole measured section.
///
/// Both halves are required: the owner permits it, and some consumer is
/// actually reading. Either alone collects nothing.
#[inline(always)]
pub fn deep_profiling_enabled() -> bool {
    DEEP_PROFILING_PERMITTED.load(Ordering::Relaxed)
        && DEEP_PROFILING_CONSUMERS.load(Ordering::Relaxed) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statics are process-wide, so the tests that move them share one
    /// lock rather than racing each other under the default parallel harness.
    static SERIALISE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        let guard = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        set_deep_profiling_permitted(false);
        assert_eq!(
            deep_profiling_consumers(),
            0,
            "a previous test leaked a guard",
        );
        guard
    }

    /// Permission alone must collect nothing.
    ///
    /// This is the behaviour the whole change is for: the toggle had been the
    /// activation switch, so a profile enabled once began sampling at boot and
    /// never stopped, for a reader that was not there.
    #[test]
    fn permission_alone_does_not_start_sampling() {
        let _lock = exclusive();

        set_deep_profiling_permitted(true);
        assert!(deep_profiling_permitted());
        assert!(
            !deep_profiling_enabled(),
            "sampling must wait for a consumer, not begin with the setting",
        );

        set_deep_profiling_permitted(false);
    }

    /// A consumer with no permission gets nothing to hold.
    #[test]
    fn a_forbidden_profile_hands_back_no_guard() {
        let _lock = exclusive();

        assert!(begin_deep_profiling().is_none());
        assert!(!deep_profiling_enabled());
    }

    /// Two consumers overlap, and the first to finish does not stop the second.
    #[test]
    fn sampling_runs_until_the_last_consumer_drops() {
        let _lock = exclusive();
        set_deep_profiling_permitted(true);

        let first = begin_deep_profiling().expect("permitted, so a guard is available");
        let second = begin_deep_profiling().expect("a second consumer may overlap the first");
        assert!(deep_profiling_enabled());

        drop(first);
        assert!(
            deep_profiling_enabled(),
            "one consumer finishing must not stop the other's collection",
        );

        drop(second);
        assert!(!deep_profiling_enabled(), "the last drop stops sampling");
        assert_eq!(deep_profiling_consumers(), 0);

        set_deep_profiling_permitted(false);
    }

    /// A panic on the way out of a profiling request still stops sampling.
    ///
    /// The reason this is a guard rather than `start()`/`stop()`: an early
    /// return or an unwind past a bare `stop()` would leave the collectors
    /// running for the life of the process.
    #[test]
    fn an_unwound_guard_still_stops_sampling() {
        let _lock = exclusive();
        set_deep_profiling_permitted(true);

        let panicked = std::panic::catch_unwind(|| {
            let _guard = begin_deep_profiling().expect("permitted");
            panic!("a profiling request failed mid-flight");
        });

        assert!(panicked.is_err(), "the closure above must have panicked");
        assert_eq!(
            deep_profiling_consumers(),
            0,
            "the guard's drop must have run while unwinding",
        );
        assert!(!deep_profiling_enabled());

        set_deep_profiling_permitted(false);
    }

    /// Withdrawing permission stops collection even while a consumer is held.
    ///
    /// The owner's switch is the outer authority: a tool that is still attached
    /// does not get to keep sampling after the profile forbids it.
    #[test]
    fn withdrawing_permission_stops_a_live_consumer() {
        let _lock = exclusive();
        set_deep_profiling_permitted(true);

        let guard = begin_deep_profiling().expect("permitted");
        assert!(deep_profiling_enabled());

        set_deep_profiling_permitted(false);
        assert!(!deep_profiling_enabled());

        drop(guard);
        assert_eq!(deep_profiling_consumers(), 0);
    }

    /// Deep profiling answers for itself. It used to be ANDed with inspection,
    /// which made the switch inert whenever the inspection plane was off — the
    /// embedder's toggle appeared to do nothing, and the stored preference was
    /// then cleared to match, losing it.
    #[test]
    fn deep_profiling_is_independent_of_the_inspection_plane() {
        assert!(!DebugOptions::default().effective_deep_profiling());
        for inspection in [false, true] {
            assert!(
                DebugOptions {
                    inspection_and_agent_control: inspection,
                    deep_intrusive_profiling: true,
                }
                .effective_deep_profiling(),
                "deep profiling should follow its own switch (inspection {inspection})"
            );
            assert!(
                !DebugOptions {
                    inspection_and_agent_control: inspection,
                    deep_intrusive_profiling: false,
                }
                .effective_deep_profiling(),
                "deep profiling off means off (inspection {inspection})"
            );
        }
    }
}
