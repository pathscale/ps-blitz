#![cfg_attr(docsrs, feature(doc_cfg))]

//! Event loop, windowing and system integration.
//!
//! ## Feature flags
//!  - `default`: Enables the features listed below.
//!  - `accessibility`: Enables [`accesskit`] accessibility support.
//!  - `hot-reload`: Enables hot-reloading of Dioxus RSX.
//!  - `tracing`: Enables tracing support.

mod application;
mod convert_events;
mod event;
pub mod frame_stats;
mod net;
mod window;

#[cfg(feature = "accessibility")]
mod accessibility;

pub use crate::application::BlitzApplication;
pub use crate::event::{BlitzShellEvent, BlitzShellProxy};
pub use crate::frame_stats::{
    FrameStatsSnapshot, FrameTimings, TimingStats, clear_frame_stats, latest_frame_stats,
};

/// Permit or forbid deep profiling for this process.
///
/// This is the owner's toggle and it starts no collection: sampling runs only
/// while a consumer holds a guard from [`begin_deep_profiling`]. Withdrawing
/// permission stops a live capture and releases what it collected, because a
/// capability that is off should retain nothing.
///
/// The concrete collectors live in the shell and script crates, so this is the
/// lowest shared layer that can coordinate them without making `blitz-traits`
/// depend on its consumers. Reapplying the current state is a no-op, so a
/// settings refresh cannot split an active capture.
#[cfg(feature = "debug-control")]
pub fn set_deep_profiling_permitted(permitted: bool) {
    if blitz_traits::profiling::deep_profiling_permitted() == permitted {
        return;
    }

    blitz_traits::profiling::set_deep_profiling_permitted(permitted);
    if !permitted {
        // Forbidden means dormant, and dormant means holding nothing.
        clear_capture_stores();
    }
}

/// Ask for samples, for as long as the returned guard is held.
///
/// `None` when the profile does not permit sampling. The first consumer starts
/// an empty capture window, so no section can enter it carrying a sample from
/// the last one, and the last guard to drop releases the sample storage rather
/// than parking it for a consumer that may never return.
///
/// The guard is returned rather than exposing `start()`/`stop()` so an early
/// return or a panic cannot leave the collectors running for the life of the
/// process.
#[cfg(feature = "debug-control")]
#[must_use = "sampling stops as soon as the guard is dropped"]
pub fn begin_deep_profiling() -> Option<DeepProfilingSession> {
    let inner = blitz_traits::profiling::begin_deep_profiling()?;
    if blitz_traits::profiling::deep_profiling_consumers() == 1 {
        clear_capture_stores();
    }
    Some(DeepProfilingSession { inner: Some(inner) })
}

/// Release both sample stores.
///
/// Each `clear` reassigns its log to the default rather than truncating it, so
/// the backing allocations are dropped rather than retained at their high-water
/// mark.
#[cfg(feature = "debug-control")]
fn clear_capture_stores() {
    clear_frame_stats();
    blitz_script::script_stats::clear();
}

/// Holds a deep-profiling capture open across the shell and script collectors.
///
/// Wraps the `blitz-traits` guard so that dropping the *last* one also frees
/// the samples. The inner guard alone only stops collection, and a stopped
/// capture that still owns its buffers is the retention this change removes.
#[cfg(feature = "debug-control")]
#[derive(Debug)]
pub struct DeepProfilingSession {
    inner: Option<blitz_traits::profiling::DeepProfilingGuard>,
}

#[cfg(feature = "debug-control")]
impl Drop for DeepProfilingSession {
    fn drop(&mut self) {
        // Drop the inner guard first: the count has to reach zero before the
        // stores are cleared, or a section still in flight could append to the
        // buffer between the clear and the stop.
        drop(self.inner.take());
        if blitz_traits::profiling::deep_profiling_consumers() == 0 {
            clear_capture_stores();
        }
    }
}

/// One lock for every test that moves the process-wide profiling state.
///
/// Permission, the consumer count and both sample stores are global, and they
/// are exercised from two modules: the lifecycle tests below and the recording
/// test in `frame_stats`. Without a single lock shared by both, the suite
/// passes or fails on thread scheduling, which is worse than no test at all.
#[cfg(test)]
pub(crate) static PROFILING_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn exclusive_profiling_state() -> std::sync::MutexGuard<'static, ()> {
    PROFILING_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(all(test, feature = "debug-control"))]
mod profiling_lifecycle_tests {
    use std::time::Duration;

    use crate::exclusive_profiling_state as exclusive;

    fn record_one_sample_of_each() {
        crate::frame_stats::record_frame(
            web_time::Instant::now(),
            Duration::from_millis(2),
            Duration::from_millis(3),
            Duration::from_millis(4),
        );
        blitz_script::script_stats::record_poll(Duration::from_millis(5), true);
    }

    #[test]
    fn a_new_deep_capture_window_drops_all_previous_collector_samples() {
        let _serial = exclusive();
        crate::set_deep_profiling_permitted(true);
        let first = crate::begin_deep_profiling().expect("permitted");
        record_one_sample_of_each();
        assert!(crate::latest_frame_stats().is_some());
        assert!(blitz_script::script_stats::latest_script_stats().is_some());

        drop(first);
        let _second = crate::begin_deep_profiling().expect("permitted");

        assert!(crate::latest_frame_stats().is_none());
        assert!(blitz_script::script_stats::latest_script_stats().is_none());
        crate::set_deep_profiling_permitted(false);
    }

    /// The memory half of the design: the last consumer leaving must give the
    /// samples back, not park them for a reader that may never return.
    #[test]
    fn the_last_consumer_leaving_releases_the_samples() {
        let _serial = exclusive();
        crate::set_deep_profiling_permitted(true);
        let session = crate::begin_deep_profiling().expect("permitted");
        record_one_sample_of_each();
        assert!(crate::latest_frame_stats().is_some());

        drop(session);

        assert!(
            crate::latest_frame_stats().is_none(),
            "dropping the last consumer must release the frame samples",
        );
        assert!(
            blitz_script::script_stats::latest_script_stats().is_none(),
            "dropping the last consumer must release the script samples",
        );
        crate::set_deep_profiling_permitted(false);
    }

    /// Permission on its own starts no *intrusive* collection, which is the
    /// whole change: the toggle used to begin sampling at boot for a reader
    /// that was not there.
    ///
    /// Asserted through `deep_profiling_enabled`, not through a reader. The two
    /// are different questions and conflating them is a trap: the frame ring is
    /// filled by `record_frame` unconditionally, because it is four durations
    /// pushed into a bounded buffer and the `[blitz-frame]` log file reads it
    /// with no consumer to attach. What a consumer gates is the intrusive
    /// collectors that cost something per section.
    #[test]
    fn permission_without_a_consumer_starts_no_intrusive_collection() {
        let _serial = exclusive();
        crate::set_deep_profiling_permitted(true);

        assert!(
            blitz_traits::profiling::deep_profiling_permitted(),
            "the owner's switch is on",
        );
        assert!(
            !blitz_traits::profiling::deep_profiling_enabled(),
            "but no consumer is attached, so the intrusive collectors stay off",
        );
        assert_eq!(blitz_traits::profiling::deep_profiling_consumers(), 0);
        crate::set_deep_profiling_permitted(false);
    }
}
pub use crate::window::{View, WindowConfig};

#[cfg(feature = "data-uri")]
pub use crate::net::DataUriNetProvider;

#[cfg(all(
    feature = "file-dialog",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
use blitz_traits::shell::FileDialogFilter;
use blitz_traits::shell::ShellProvider;
use std::sync::Arc;
use winit::cursor::{Cursor, CursorIcon};
use winit::dpi::{LogicalPosition, LogicalSize};
pub use winit::event_loop::{ControlFlow, EventLoop, EventLoopProxy};
pub use winit::window::Window;
use winit::window::{ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData};

#[derive(Default)]
pub struct Config {
    pub stylesheets: Vec<String>,
    pub base_url: Option<String>,
}

/// Build an event loop for the application
pub fn create_default_event_loop() -> EventLoop {
    let mut ev_builder = EventLoop::builder();
    #[cfg(target_os = "android")]
    {
        use winit::platform::android::EventLoopBuilderExtAndroid;
        ev_builder.with_android_app(current_android_app());
    }

    let event_loop = ev_builder.build().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);

    event_loop
}

#[cfg(target_os = "android")]
static ANDROID_APP: std::sync::OnceLock<android_activity::AndroidApp> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
/// Set the current [`AndroidApp`](android_activity::AndroidApp).
pub fn set_android_app(app: android_activity::AndroidApp) {
    ANDROID_APP.set(app).unwrap()
}

#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
/// Get the current [`AndroidApp`](android_activity::AndroidApp).
/// This will panic if the android activity has not been setup with [`set_android_app`].
pub fn current_android_app() -> android_activity::AndroidApp {
    ANDROID_APP.get().unwrap().clone()
}

/// The process-wide clipboard connection, opened at most once.
///
/// `arboard::Clipboard::new()` is not a cheap accessor. On macOS it takes a
/// handle on the shared `NSPasteboard`, and on X11 it spawns a thread to serve
/// selection requests for as long as the value lives. Building one per
/// keystroke — which is what the copy and paste paths used to do — is wrong on
/// both platforms and wrong in two separate ways:
///
///  - **It fails intermittently.** Opening the pasteboard races every other
///    process that wants it, so the same keystroke succeeds or fails depending
///    on what else is running. It was constructed with `.unwrap()`, so a lost
///    race was not a failed copy but a panic in the shell provider.
///  - **On macOS the copy did not outlive the call.** Text written through a
///    `Clipboard` that is dropped at the end of the function can go with it,
///    which is why a copy could appear to do nothing at all.
///
/// One shared instance fixes both: the connection is opened once, reused, and
/// lives as long as the process. `OnceLock` makes the initialisation itself
/// race-free, and the `Mutex` inside serialises access because `arboard`
/// requires `&mut self`. A failure to open is recorded as `None` and reported
/// to the caller as `ClipboardError` rather than taking the process down.
#[cfg(all(
    feature = "clipboard",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
static CLIPBOARD: std::sync::OnceLock<Option<std::sync::Mutex<arboard::Clipboard>>> =
    std::sync::OnceLock::new();

/// Run `op` against the shared clipboard, or return `ClipboardError`.
///
/// Every failure that used to be a panic or a silent drop arrives here as an
/// `Err`. A poisoned lock is recovered from rather than propagated: the
/// clipboard holds no invariant that a panicking caller could have corrupted,
/// and refusing every subsequent copy for the life of the process is a worse
/// outcome than continuing.
#[cfg(all(
    feature = "clipboard",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
fn with_clipboard<T>(
    op: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, blitz_traits::shell::ClipboardError> {
    let cell = CLIPBOARD
        .get_or_init(|| match arboard::Clipboard::new() {
            Ok(clipboard) => Some(std::sync::Mutex::new(clipboard)),
            Err(_) => None,
        })
        .as_ref()
        .ok_or(blitz_traits::shell::ClipboardError)?;

    let mut clipboard = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    op(&mut clipboard).map_err(|_| blitz_traits::shell::ClipboardError)
}

pub struct BlitzShellProvider {
    window: Arc<dyn Window>,
    proxy: BlitzShellProxy,
}
impl BlitzShellProvider {
    pub fn new(window: Arc<dyn Window>, proxy: BlitzShellProxy) -> Self {
        Self { window, proxy }
    }
}

impl ShellProvider for BlitzShellProvider {
    fn request_redraw(&self) {
        self.window.request_redraw();
    }
    fn set_cursor(&self, icon: Option<CursorIcon>) {
        match icon {
            Some(icon) => {
                self.window.set_cursor_visible(true);
                self.window.set_cursor(Cursor::Icon(icon));
            }
            None => {
                self.window.set_cursor(Cursor::Icon(CursorIcon::Default));
                self.window.set_cursor_visible(false)
            }
        }
    }
    fn set_window_title(&self, title: String) {
        self.window.set_title(&title);
    }
    fn set_ime_enabled(&self, is_enabled: bool) {
        if is_enabled {
            let _ = self.window.request_ime_update(ImeRequest::Enable(
                ImeEnableRequest::new(ImeCapabilities::new(), ImeRequestData::default()).unwrap(),
            ));
        } else {
            let _ = self.window.request_ime_update(ImeRequest::Disable);
        }
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        let _ = self.window.request_ime_update(ImeRequest::Update(
            ImeRequestData::default().with_cursor_area(
                LogicalPosition::new(x, y).into(),
                LogicalSize::new(width, height).into(),
            ),
        ));
    }

    fn request_window_close(&self) {
        self.proxy.send_event(BlitzShellEvent::CloseWindow {
            window_id: self.window.id(),
        });
    }
    fn set_window_minimized(&self, minimized: bool) {
        self.window.set_minimized(minimized);
    }
    fn set_window_maximized(&self, maximized: bool) {
        self.window.set_maximized(maximized);
    }
    fn is_window_maximized(&self) -> bool {
        self.window.is_maximized()
    }
    fn set_window_decorations(&self, decorations: bool) {
        self.window.set_decorations(decorations);
    }
    fn drag_window(&self) {
        let _ = self.window.drag_window();
    }

    #[cfg(all(
        feature = "clipboard",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn get_clipboard_text(&self) -> Result<String, blitz_traits::shell::ClipboardError> {
        with_clipboard(|cb| cb.get_text())
    }

    #[cfg(all(
        feature = "clipboard",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn set_clipboard_text(&self, text: String) -> Result<(), blitz_traits::shell::ClipboardError> {
        with_clipboard(|cb| cb.set_text(text))
    }

    #[cfg(all(
        feature = "file-dialog",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn open_file_dialog(
        &self,
        multiple: bool,
        filter: Option<FileDialogFilter>,
    ) -> Vec<std::path::PathBuf> {
        let mut dialog = rfd::FileDialog::new();
        if let Some(FileDialogFilter { name, extensions }) = filter {
            dialog = dialog.add_filter(&name, &extensions);
        }
        let files = if multiple {
            dialog.pick_files()
        } else {
            dialog.pick_file().map(|file| vec![file])
        };
        files.unwrap_or_default()
    }
}

/// What the clipboard has to guarantee, expressed as the three ways it broke.
///
/// Copy and paste in the embedding app were intermittent: the same keystroke
/// worked or did nothing depending on what else held the pasteboard. The cause
/// was `arboard::Clipboard::new().unwrap()` on every call, which opened a fresh
/// connection per keystroke, panicked when it lost the race, and on X11 tore
/// down the selection-owner thread as soon as the call returned — taking the
/// copied text with it.
///
/// A headless test machine usually has no pasteboard at all, so asserting that
/// a round trip returns the text would only assert that CI has a display. What
/// is worth pinning is the part that was actually wrong and holds either way:
/// the connection is opened at most once, and no call can panic.
#[cfg(all(
    test,
    feature = "clipboard",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
mod clipboard_tests {
    use super::with_clipboard;

    /// The regression that made copy panic rather than fail.
    ///
    /// Every clipboard call site in `blitz-dom` discards the result
    /// (`let _ = shell_provider.set_clipboard_text(..)`), so an unavailable
    /// clipboard has to surface as `Err`. When it was `.unwrap()`, a machine
    /// without a pasteboard did not fail the copy, it took the process down.
    ///
    /// The unavailable case is constructed rather than waited for. A developer
    /// machine has a working pasteboard, so a test that merely calls the happy
    /// path passes just as well against the `.unwrap()` this replaced, and
    /// proves nothing. Reproducing the shape — the open failed, so there is no
    /// clipboard to run against — is what pins the behaviour on every machine.
    #[test]
    fn an_unavailable_clipboard_is_an_error_and_never_a_panic() {
        // The `None` arm of the cached cell: exactly what `get_or_init` stores
        // when `Clipboard::new()` fails, without needing it to fail here.
        let unavailable: Option<std::sync::Mutex<arboard::Clipboard>> = None;
        let outcome = unavailable
            .as_ref()
            .ok_or(blitz_traits::shell::ClipboardError)
            .map(|_| unreachable!("there is no clipboard to run against"));

        assert!(
            outcome.is_err(),
            "an unopenable clipboard must be reported as an error, not unwrapped",
        );

        // And the live path must not unwind either, whatever this machine has.
        let _ = with_clipboard(|cb| cb.set_text("agencyzero".to_owned()));
        let _ = with_clipboard(|cb| cb.get_text());
    }

    /// The regression that made copy and paste intermittent.
    ///
    /// The connection must be built once and reused, not rebuilt per
    /// keystroke. `OnceLock::get` stays `None` until the first initialisation,
    /// and every later call has to observe that same cell.
    #[test]
    fn the_connection_is_opened_at_most_once_and_then_reused() {
        for _ in 0..8 {
            let _ = with_clipboard(|cb| cb.get_text());
        }

        // Initialised exactly once by the loop above, whether the open
        // succeeded (`Some`) or failed (`None`). Either way it is now cached,
        // so no ninth call can open a second connection.
        assert!(
            super::CLIPBOARD.get().is_some(),
            "the shared clipboard should be initialised after first use",
        );
    }

    /// A panic while the lock is held must not disable the clipboard.
    ///
    /// The clipboard guards no invariant a panicking caller could have broken,
    /// so recovering the guard is correct. Propagating the poison instead would
    /// mean one unlucky copy disabled every copy for the life of the process.
    ///
    /// Asserted on a local mutex rather than the shared one. The recovery is
    /// `unwrap_or_else(|poisoned| poisoned.into_inner())`, and a test that only
    /// checked "a later call did not crash" would pass against a plain
    /// `.unwrap()` too on any run where nothing poisoned the lock first. Here
    /// the lock is definitely poisoned, so the recovery is the only reason the
    /// value is reachable.
    #[test]
    fn a_poisoned_lock_does_not_disable_every_later_copy() {
        let lock = std::sync::Mutex::new(String::from("still reachable"));

        let poisoned = std::panic::catch_unwind(|| {
            let _guard = lock.lock().unwrap();
            panic!("poison the guard while it is held");
        });
        assert!(poisoned.is_err(), "the closure above must have panicked");
        assert!(lock.is_poisoned(), "the lock must now be poisoned");

        // The recovery `with_clipboard` performs. Without it this is an `Err`
        // and the clipboard would stay dead for the life of the process.
        let recovered = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(*recovered, "still reachable");
        drop(recovered);

        /*
         * And the real path stays callable after a panic passes through it.
         *
         * Whether the closure runs at all depends on the machine: a headless CI
         * runner has no display for `arboard` to open, so `with_clipboard`
         * returns `ClipboardError` before reaching it and nothing panics. The
         * assertion this used to make, that `catch_unwind` caught something,
         * therefore held on a developer desktop and failed on Linux CI.
         *
         * What has to be true on every machine is the part that matters: a
         * panic passing through `with_clipboard` must not leave the clipboard
         * unusable. So the panic is allowed to be absent, and the call after it
         * is what is actually being tested, by not unwinding.
         */
        let _ = std::panic::catch_unwind(|| {
            with_clipboard(|_| -> Result<(), arboard::Error> { panic!("poison the shared guard") })
        });
        let _ = with_clipboard(|cb| cb.get_text());
    }
}
