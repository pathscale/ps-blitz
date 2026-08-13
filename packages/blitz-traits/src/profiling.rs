//! Process-wide switch for the intrusive performance collectors.
//!
//! Inspection and input control do not need these collectors. They stay dormant
//! until the embedder explicitly starts a profiling session, so shipping the
//! capability does not mean continuously paying clocks, locks, maps, or sample
//! retention.

use std::sync::atomic::{AtomicBool, Ordering};

static DEEP_PROFILING_ENABLED: AtomicBool = AtomicBool::new(false);

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

/// Enable or disable every intrusive collector in this process.
///
/// Relaxed ordering is sufficient: this is a performance mode, not a memory
/// publication boundary. Each whole frame, resolve, or script section observes
/// either state and the next section observes a newly selected state.
pub fn set_deep_profiling_enabled(enabled: bool) {
    DEEP_PROFILING_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether intrusive collection is active for the next whole measured section.
#[inline(always)]
pub fn deep_profiling_enabled() -> bool {
    DEEP_PROFILING_ENABLED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiling_starts_off_and_can_be_selected_at_runtime() {
        set_deep_profiling_enabled(false);
        assert!(!deep_profiling_enabled());
        set_deep_profiling_enabled(true);
        assert!(deep_profiling_enabled());
        set_deep_profiling_enabled(false);
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
