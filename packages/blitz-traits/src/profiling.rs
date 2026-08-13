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
    /// Deep samples are useful only while the local inspection plane exists to
    /// retrieve them. This also prevents an inconsistent persisted pair from
    /// silently collecting in the background.
    #[must_use]
    pub const fn effective_deep_profiling(self) -> bool {
        self.inspection_and_agent_control && self.deep_intrusive_profiling
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

    #[test]
    fn deep_profiling_requires_the_inspection_plane() {
        assert!(!DebugOptions::default().effective_deep_profiling());
        assert!(
            DebugOptions {
                inspection_and_agent_control: true,
                deep_intrusive_profiling: true,
            }
            .effective_deep_profiling()
        );
        assert!(
            !DebugOptions {
                inspection_and_agent_control: false,
                deep_intrusive_profiling: true,
            }
            .effective_deep_profiling()
        );
    }
}
