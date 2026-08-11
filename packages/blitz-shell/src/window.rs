use crate::BlitzShellProvider;
use crate::convert_events::{
    button_source_to_blitz, color_scheme_to_theme, pointer_kind_to_blitz, pointer_source_to_blitz,
    pointer_source_to_blitz_details, theme_to_color_scheme, winit_ime_to_blitz,
    winit_key_event_to_blitz, winit_modifiers_to_kbt_modifiers,
};
use crate::event::{BlitzShellEvent, BlitzShellProxy, create_waker};
use anyrender::WindowRenderer;
use blitz_dom::Document;
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, MouseEventButton,
    MouseEventButtons, PointerCoords, PointerDetails, UiEvent,
};
use blitz_traits::shell::Viewport;
use winit::dpi::{LogicalPosition, PhysicalInsets, PhysicalPosition};
use winit::keyboard::PhysicalKey;

use atomic_refcell::AtomicRefCell;
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::task::Waker;
use std::time::Duration;
use web_time::Instant;
use winit::event::{ButtonSource, ElementState, MouseButton};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Theme, WindowAttributes, WindowId};
use winit::{event::Modifiers, event::WindowEvent, keyboard::KeyCode, window::Window};

#[cfg(feature = "accessibility")]
use crate::accessibility::AccessibilityState;

// Ignore safe_area_insets on macOS because we don't want to avoid
// drawing in the titlebar.
#[cfg(target_os = "macos")]
fn get_safe_area_insets(_window: &dyn Window) -> PhysicalInsets<u32> {
    Default::default()
}
#[cfg(not(target_os = "macos"))]
fn get_safe_area_insets(window: &dyn Window) -> PhysicalInsets<u32> {
    window.safe_area()
}

pub struct WindowConfig<Rend: WindowRenderer> {
    doc: Box<dyn Document>,
    pub(crate) attributes: WindowAttributes,
    renderer: Rend,
    on_created: Option<WindowCreatedCallback>,
}

type WindowCreatedCallback = Box<dyn FnOnce(Arc<dyn Window>) + 'static>;

impl<Rend: WindowRenderer> WindowConfig<Rend> {
    pub fn new(doc: Box<dyn Document>, renderer: Rend) -> Self {
        Self::with_attributes(doc, renderer, WindowAttributes::default())
    }

    pub fn with_attributes(
        doc: Box<dyn Document>,
        renderer: Rend,
        attributes: WindowAttributes,
    ) -> Self {
        WindowConfig {
            doc,
            attributes,
            renderer,
            on_created: None,
        }
    }

    /// Run a callback after the native window is created and before the first frame is prepared.
    pub fn with_on_created(mut self, callback: impl FnOnce(Arc<dyn Window>) + 'static) -> Self {
        self.on_created = Some(Box::new(callback));
        self
    }
}

pub struct View<Rend: WindowRenderer> {
    pub doc: Box<dyn Document>,

    pub renderer: Rend,
    pub waker: Option<Waker>,

    pub proxy: BlitzShellProxy,
    pub window: Arc<dyn Window>,

    /// The state of the keyboard modifiers (ctrl, shift, etc). Winit/Tao don't track these for us so we
    /// need to store them in order to have access to them when processing keypress events
    pub theme_override: Option<Theme>,
    pub keyboard_modifiers: Modifiers,
    pub buttons: MouseEventButtons,
    pub pointer_pos: PhysicalPosition<f64>,
    /// The non-mouse pointers (touch/pen) that are currently pressed, in the
    /// order they were pressed.
    ///
    /// This serves two purposes:
    /// - Multi-touch: it is cloned (cheaply, via [`Arc`]) into every dispatched
    ///   [`BlitzPointerEvent`] so that touch events can report all concurrent
    ///   touches via their `touches` list.
    /// - Cancellation detection: winit signals a cancelled touch with a
    ///   [`WindowEvent::PointerLeft`] that is *not* preceded by a
    ///   [`WindowEvent::PointerButton`] with [`ElementState::Released`]. If a
    ///   pointer is still in this list when it leaves, it was cancelled.
    ///
    /// The events stored here always have an empty `active_pointers` list to
    /// avoid a reference cycle.
    pub active_events: Arc<AtomicRefCell<Vec<BlitzPointerEvent>>>,
    pub animation_timer: Option<Instant>,
    pub is_visible: bool,
    pub safe_area_insets: PhysicalInsets<u32>,

    /// Whether a platform redraw has already been requested and has not yet
    /// entered [`Self::redraw`]. DOM mutations can invalidate a window many
    /// times during one input burst; the platform only needs one frame request.
    redraw_pending: std::cell::Cell<bool>,

    frame_stats: FrameStats,

    #[cfg(target_arch = "wasm32")]
    pending_resize: Option<winit::dpi::PhysicalSize<u32>>,
    #[cfg(target_arch = "wasm32")]
    last_resize_at: Option<web_time::Instant>,
    /// True iff a setTimeout has been scheduled and not yet observed by
    /// `apply_pending_resize_if_settled`. Prevents the timer storm that would
    /// otherwise allocate a fresh `Closure` per resize event during a drag.
    #[cfg(target_arch = "wasm32")]
    resize_timer_scheduled: bool,

    #[cfg(feature = "accessibility")]
    /// Accessibility adapter for `accesskit`.
    pub accessibility: AccessibilityState,

    // Calling request_redraw within a WindowEvent doesn't work on iOS. So on iOS we track the state
    // with a boolean and call request_redraw in about_to_wait
    //
    // See https://github.com/rust-windowing/winit/issues/3406
    #[cfg(target_os = "ios")]
    pub ios_request_redraw: std::cell::Cell<bool>,

    /// When the next animation-only frame is due, if one is.
    ///
    /// An animation drives frames by asking for the next redraw at the end of
    /// the last one, which runs it at the display's rate. Set this instead of
    /// asking immediately, and `about_to_wait` turns it into a
    /// `ControlFlow::WaitUntil`, so the loop sleeps in between rather than
    /// spinning. `None` means nothing is animating and the loop can wait
    /// indefinitely for input.
    pub animation_frame_due: std::cell::Cell<Option<Instant>>,
}

/// Frames per second to aim for on animation-only frames.
///
/// A browser cannot negotiate with the pages it renders: an arbitrary site's
/// `animation: fade 2s infinite` otherwise pins the process at the display's
/// refresh rate, repainting the whole window each time, for as long as the tab
/// is open. 30fps is indistinguishable on the decorative animations this is
/// aimed at.
///
/// This governs *animation-only* frames. Input, resize, navigation and every
/// other event still redraw immediately, so nothing this clamps is something a
/// user is waiting on.
const ANIMATION_TARGET_FPS: u32 = 30;

/// Used only when the display will not say what its refresh rate is.
const ANIMATION_FALLBACK_INTERVAL: Duration = Duration::from_millis(33);

/// The gap between animation-only frames, as a whole number of the display's
/// own refresh intervals.
///
/// Rounding to a multiple of the refresh rate rather than picking a wall-clock
/// constant: a fixed 33ms against an 8.3ms refresh is a period the display
/// cannot hit, so frames land one refresh late at an irregular beat, and the
/// clamp reads as jitter rather than as a lower frame rate. On a 120Hz display
/// this is every 4th refresh, on 60Hz every 2nd, and both are exactly 30fps.
fn animation_frame_interval() -> Duration {
    let Some(millihertz) = crate::frame_stats::display_refresh_millihertz() else {
        return ANIMATION_FALLBACK_INTERVAL;
    };
    let refresh_hz = f64::from(millihertz) / 1000.0;
    if refresh_hz <= f64::from(ANIMATION_TARGET_FPS) {
        // A display slower than the target cannot be clamped toward it, and
        // asking for every refresh is what it would already be doing.
        return Duration::from_secs_f64(1.0 / refresh_hz);
    }
    let every_nth = (refresh_hz / f64::from(ANIMATION_TARGET_FPS)).round().max(1.0);
    Duration::from_secs_f64(every_nth / refresh_hz)
}

impl<Rend: WindowRenderer> Drop for View<Rend> {
    fn drop(&mut self) {
        // Release the renderer's window surface before the window is dropped.
        // The renderer may be shared (e.g. provided as a context to user code),
        // in which case it can outlive the `View`. A GPU surface must not
        // outlive the window/display it is attached to: dropping it after the
        // event loop has shut down segfaults on Wayland.
        self.renderer.suspend();
    }
}

impl<Rend: WindowRenderer> View<Rend> {
    pub fn init(
        mut config: WindowConfig<Rend>,
        event_loop: &dyn ActiveEventLoop,
        proxy: &BlitzShellProxy,
    ) -> Self {
        // We create window as invisble and then later make window visible
        // after AccessKit has initialised to avoid AccessKit panics
        let is_visible = config.attributes.visible;
        // Capture the requested surface size before consuming `attributes`, so we can
        // seed the viewport on platforms (winit-web) that report `surface_size() == 0×0`
        // until a layout pass fires.
        let requested_surface_size = config.attributes.surface_size;
        let attrs = config.attributes.with_visible(false);

        let winit_window: Arc<dyn Window> = Arc::from(event_loop.create_window(attrs).unwrap());
        if let Some(on_created) = config.on_created.take() {
            on_created(Arc::clone(&winit_window));
        }
        #[cfg(feature = "accessibility")]
        let accessibility = AccessibilityState::new(&*winit_window, proxy.clone());

        if is_visible {
            winit_window.set_visible(true);
        }

        // Create viewport
        // TODO: account for the "safe area"
        let scale = winit_window.scale_factor() as f32;
        let mut size = winit_window.surface_size();
        if (size.width == 0 || size.height == 0)
            && let Some(requested) = requested_surface_size
        {
            size = requested.to_physical(scale as f64);
        }
        // On wasm, when the embedder didn't call `with_surface_size`, winit-web's
        // initial `surface_size()` is 0×0 — its ResizeObserver hasn't fired yet.
        // Resuming the renderer at 0×0 trips a wgpu swapchain-size-0 error, so
        // seed from the canvas element's CSS layout box (host-stylesheet result).
        #[cfg(target_arch = "wasm32")]
        if size.width == 0 || size.height == 0 {
            use winit::platform::web::WindowExtWeb;
            if let Some(canvas) = winit_window.canvas() {
                let css_w = canvas.offset_width().max(0) as u32;
                let css_h = canvas.offset_height().max(0) as u32;
                if css_w > 0 && css_h > 0 {
                    size = winit::dpi::LogicalSize::new(css_w, css_h).to_physical(scale as f64);
                }
            }
        }
        let safe_area_insets = get_safe_area_insets(&*winit_window);
        let theme = winit_window.theme().unwrap_or(Theme::Light);
        let color_scheme = theme_to_color_scheme(theme);
        let viewport = Viewport::new(size.width, size.height, scale, color_scheme);

        // Create shell provider
        let shell_provider = BlitzShellProvider::new(winit_window.clone(), proxy.clone());

        let mut doc = config.doc;
        let mut inner = doc.inner_mut();
        inner.set_viewport(viewport);
        inner.set_shell_provider(Arc::new(shell_provider));

        // If the document title is set prior to the window being created then it will
        // have been sent to a dummy ShellProvider and won't get picked up.
        // So we look for it here and set it if present.
        let title = inner.find_title_node().map(|node| node.text_content());
        if let Some(title) = title {
            winit_window.set_title(&title);
        }

        drop(inner);

        Self {
            renderer: config.renderer,
            waker: None,
            animation_timer: None,
            keyboard_modifiers: Default::default(),
            proxy: proxy.clone(),
            window: winit_window.clone(),
            doc,
            theme_override: None,
            buttons: MouseEventButtons::None,
            active_events: Arc::new(AtomicRefCell::new(Vec::new())),
            safe_area_insets,
            #[cfg(target_arch = "wasm32")]
            pending_resize: None,
            #[cfg(target_arch = "wasm32")]
            last_resize_at: None,
            #[cfg(target_arch = "wasm32")]
            resize_timer_scheduled: false,
            pointer_pos: Default::default(),
            is_visible: winit_window.is_visible().unwrap_or(true),
            redraw_pending: std::cell::Cell::new(false),
            frame_stats: FrameStats::new(&*winit_window),
            #[cfg(feature = "accessibility")]
            accessibility,

            #[cfg(target_os = "ios")]
            ios_request_redraw: std::cell::Cell::new(false),

            animation_frame_due: std::cell::Cell::new(None),
        }
    }

    pub fn replace_document(&mut self, new_doc: Box<dyn Document>, retain_scroll_position: bool) {
        let inner = self.doc.inner();
        let scroll = inner.viewport_scroll();
        let viewport = inner.viewport().clone();
        let shell_provider = inner.shell_provider.clone();
        drop(inner);

        self.doc = new_doc;

        let mut inner = self.doc.inner_mut();
        inner.set_viewport(viewport);
        inner.set_shell_provider(shell_provider);
        drop(inner);

        self.poll();
        self.request_redraw();

        if retain_scroll_position {
            self.doc.inner_mut().set_viewport_scroll(scroll);
        }
    }

    pub fn theme_override(&self) -> Option<Theme> {
        self.theme_override
    }

    pub fn current_theme(&self) -> Theme {
        color_scheme_to_theme(self.doc.inner().viewport().color_scheme)
    }

    pub fn set_theme_override(&mut self, theme: Option<Theme>) {
        self.theme_override = theme;
        let theme = theme.or(self.window.theme()).unwrap_or(Theme::Light);
        self.with_viewport(|v| v.color_scheme = theme_to_color_scheme(theme));
    }

    pub fn downcast_doc_mut<T: 'static>(&mut self) -> &mut T {
        (&mut *self.doc as &mut dyn Any)
            .downcast_mut::<T>()
            .unwrap()
    }

    pub fn try_downcast_doc_mut<T: 'static>(&mut self) -> Option<&mut T> {
        (&mut *self.doc as &mut dyn Any).downcast_mut::<T>()
    }

    pub fn current_animation_time(&mut self) -> f64 {
        match &self.animation_timer {
            Some(start) => Instant::now().duration_since(*start).as_secs_f64(),
            None => {
                self.animation_timer = Some(Instant::now());
                0.0
            }
        }
    }
}

impl<Rend: WindowRenderer> View<Rend> {
    /// Start resuming the renderer. Dispatches [`BlitzShellEvent::ResumeReady`]
    /// when initialization completes — synchronously on native, asynchronously
    /// on wasm32. The embedder must call [`complete_resume`](Self::complete_resume)
    /// in response.
    pub fn resume(&mut self) {
        let window_id = self.window_id();
        let animation_time = self.current_animation_time();

        let (width, height) = {
            let mut inner = self.doc.inner_mut();
            inner.resolve(animation_time);
            inner.viewport().window_size
        };

        let proxy = self.proxy.clone();
        self.renderer
            .resume(Arc::new(self.window.clone()), width, height, move || {
                proxy.send_event(BlitzShellEvent::ResumeReady { window_id });
            });
    }

    /// Finalize a previously-started resume. Should be called in response to a
    /// [`BlitzShellEvent::ResumeReady`] event. Paints the first frame and
    /// installs the doc poll waker. Returns `true` if the renderer is now active.
    pub fn complete_resume(&mut self) -> bool {
        if !self.renderer.complete_resume() {
            return false;
        }

        let window_id = self.window_id();

        // Resync the renderer to the current viewport. Resize/scale events that
        // arrived while the renderer was Pending were no-ops on the renderer
        // (its `set_size` only matches Active), so the surface created during
        // resume could be at a stale size by the time we get here.
        let animation_time = self.current_animation_time();
        let mut inner = self.doc.inner_mut();
        inner.resolve(animation_time);
        let (width, height) = inner.viewport().window_size;
        let scale = inner.viewport().scale_f64();
        let insets = self.safe_area_insets.to_logical(scale);

        #[cfg(feature = "custom-widget")]
        inner.can_create_surfaces(&mut self.renderer as _);

        self.renderer.set_size(width, height);

        self.renderer.render(|scene| {
            paint_scene(
                scene,
                &mut inner,
                scale,
                width,
                height,
                insets.left,
                insets.top,
            )
        });
        drop(inner);
        self.redraw_pending.set(false);

        self.waker = Some(create_waker(&self.proxy, window_id));
        // Scripts can schedule timers before the native surface exists. Their timer thread has
        // nothing to wake until this point, so poll once after installing the event-loop waker
        // to run already-due work and re-arm future deadlines.
        self.poll();
        true
    }

    pub fn suspend(&mut self) {
        self.waker = None;
        self.redraw_pending.set(false);
        self.renderer.suspend();

        #[cfg(feature = "custom-widget")]
        self.doc.inner_mut().destroy_surfaces();
    }

    pub fn poll(&mut self) -> bool {
        if let Some(waker) = &self.waker {
            let cx = std::task::Context::from_waker(waker);
            if self.doc.poll(Some(cx)) {
                #[cfg(feature = "accessibility")]
                {
                    let inner = self.doc.inner();
                    if inner.has_changes() {
                        self.accessibility.update_tree(&inner);
                    }
                }

                self.request_redraw();
                return true;
            }
        }

        false
    }

    pub fn request_redraw(&self) {
        if self.renderer.is_active() && !self.redraw_pending.replace(true) {
            self.window.request_redraw();
            #[cfg(target_os = "ios")]
            self.ios_request_redraw.set(true);
        }
    }

    pub fn redraw(&mut self) {
        let frame_started = Instant::now();
        self.redraw_pending.set(false);
        #[cfg(target_os = "ios")]
        self.ios_request_redraw.set(false);
        let animation_time = self.current_animation_time();
        let is_visible = self.is_visible;

        let resolve_started = Instant::now();
        let mut inner = self.doc.inner_mut();
        inner.resolve(animation_time);
        let resolve_time = resolve_started.elapsed();

        // Unregister resources (e.g. textures) from dropped custom widget nodes
        #[cfg(feature = "custom-widget")]
        for id in inner.take_pending_resource_deallocations() {
            self.renderer.unregister_resource(id);
        }

        let (width, height) = inner.viewport().window_size;
        let scale = inner.viewport().scale_f64();
        let is_animating = inner.is_animating();
        let is_blocked = inner.has_pending_critical_resources();
        let insets = self.safe_area_insets.to_logical(scale);

        let mut paint_time = Duration::ZERO;
        let render_started = Instant::now();
        if !is_blocked && is_visible {
            self.renderer.render(|scene| {
                let paint_started = Instant::now();
                paint_scene(
                    scene,
                    &mut inner,
                    scale,
                    width,
                    height,
                    insets.left,
                    insets.top,
                );
                paint_time = paint_started.elapsed();
            });
        }
        let renderer_time = render_started.elapsed().saturating_sub(paint_time);

        drop(inner);

        self.frame_stats
            .record(frame_started, resolve_time, paint_time, renderer_time);

        if !is_blocked && is_visible && is_animating {
            // Due rather than requested. Requesting here is what runs an
            // animation at the display's rate; `about_to_wait` waits out the
            // remainder of the interval and asks then.
            //
            // Measured from when this frame *started*, not from now, so the
            // interval covers the frame's own cost instead of following it. The
            // other way round, a 6ms frame plus a 33ms wait is a 39ms cadence,
            // and the clamp silently runs slower than it claims: 24fps measured
            // where 30 was asked for.
            self.animation_frame_due
                .set(Some(frame_started + animation_frame_interval()));
        } else {
            self.animation_frame_due.set(None);
        }
    }

    /// Ask for the pending animation frame if it is due, and report when the
    /// next one falls due so the event loop can sleep until then.
    ///
    /// Returns `None` when nothing is animating, which lets the loop wait for
    /// input instead of on a clock.
    pub fn poll_animation_frame(&self, now: Instant) -> Option<Instant> {
        let due = self.animation_frame_due.get()?;
        if now >= due {
            self.animation_frame_due.set(None);
            self.request_redraw();
            None
        } else {
            Some(due)
        }
    }

    pub fn pointer_coords(&self, position: PhysicalPosition<f64>) -> PointerCoords {
        let inner = self.doc.inner();
        let scale = inner.viewport().scale_f64();
        let LogicalPosition::<f32> {
            x: screen_x,
            y: screen_y,
        } = position.to_logical(scale);
        let viewport_scroll_offset = inner.viewport_scroll();
        let client_x = screen_x - (self.safe_area_insets.left as f64 / scale) as f32;
        let client_y = screen_y - (self.safe_area_insets.top as f64 / scale) as f32;
        let page_x = client_x + viewport_scroll_offset.x as f32;
        let page_y = client_y + viewport_scroll_offset.y as f32;

        PointerCoords {
            screen_x,
            screen_y,
            client_x,
            client_y,
            page_x,
            page_y,
        }
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Store `event` as an active pointer, replacing any existing entry with the
    /// same id. The stored event has an empty `active_pointers` list to avoid a
    /// reference cycle.
    fn set_active_pointer(&self, event: &BlitzPointerEvent) {
        let mut stored = event.clone();
        stored.active_pointers = Default::default();

        let mut active = self.active_events.borrow_mut();
        if let Some(existing) = active.iter_mut().find(|e| e.id == stored.id) {
            *existing = stored;
        } else {
            active.push(stored);
        }
    }

    /// Update the stored position/state of an already-active pointer. Does
    /// nothing if the pointer is not currently active (e.g. a hovering pen).
    fn update_active_pointer(&self, event: &BlitzPointerEvent) {
        let mut active = self.active_events.borrow_mut();
        if let Some(existing) = active.iter_mut().find(|e| e.id == event.id) {
            let mut stored = event.clone();
            stored.active_pointers = Default::default();
            *existing = stored;
        }
    }

    /// Remove an active pointer by id. Returns `true` if it was present.
    fn remove_active_pointer(&self, id: BlitzPointerId) -> bool {
        let mut active = self.active_events.borrow_mut();
        let len_before = active.len();
        active.retain(|e| e.id != id);
        active.len() != len_before
    }

    #[inline]
    pub fn with_viewport(&mut self, cb: impl FnOnce(&mut Viewport)) {
        let mut inner = self.doc.inner_mut();
        let mut viewport = inner.viewport_mut();
        cb(&mut viewport);
        let (width, height) = viewport.window_size;
        drop(viewport);
        drop(inner);
        if width > 0 && height > 0 {
            let insets = self.safe_area_insets;
            self.renderer.set_size(
                width + insets.left + insets.right,
                height + insets.top + insets.bottom,
            );
            self.request_redraw();
        }
    }

    #[cfg(feature = "accessibility")]
    pub fn build_accessibility_tree(&mut self) {
        let inner = self.doc.inner();
        self.accessibility.update_tree(&inner);
    }

    #[cfg(target_arch = "wasm32")]
    const RESIZE_DEBOUNCE_MS: u32 = 100;

    #[cfg(target_arch = "wasm32")]
    fn schedule_resize_settle_check(&mut self, delay_ms: u32) {
        use wasm_bindgen::JsCast;
        use wasm_bindgen::closure::Closure;

        let proxy = self.proxy.clone();
        let window_id = self.window_id();
        let cb = Closure::once_into_js(move || {
            proxy.send_event(BlitzShellEvent::ResizeSettleCheck { window_id });
        });
        if let Some(win) = web_sys::window() {
            let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                delay_ms as i32,
            );
            self.resize_timer_scheduled = true;
        }
    }

    /// Applies the pending resize iff motion has been quiet for the debounce
    /// window; otherwise re-arms the timer for the remaining time. Called
    /// when a previously scheduled timer fires.
    #[cfg(target_arch = "wasm32")]
    pub fn apply_pending_resize_if_settled(&mut self) {
        self.resize_timer_scheduled = false;
        let Some(last) = self.last_resize_at else {
            return;
        };
        let debounce = std::time::Duration::from_millis(Self::RESIZE_DEBOUNCE_MS as u64);
        let elapsed = web_time::Instant::now().saturating_duration_since(last);
        if elapsed < debounce {
            // Motion ongoing — wait out the rest of the window before re-checking.
            let remaining_ms = (debounce - elapsed).as_millis() as u32;
            self.schedule_resize_settle_check(remaining_ms);
            return;
        }
        let Some(size) = self.pending_resize.take() else {
            return;
        };
        self.last_resize_at = None;

        let insets = self.safe_area_insets;
        let width = size.width.saturating_sub(insets.left + insets.right);
        let height = size.height.saturating_sub(insets.top + insets.bottom);
        self.with_viewport(|v| v.window_size = (width, height));
        self.request_redraw();
    }

    #[cfg(target_os = "macos")]
    pub fn handle_apple_standard_keybinding(&mut self, command: &str) {
        use blitz_traits::SmolStr;
        let event = UiEvent::AppleStandardKeybinding(SmolStr::new(command));
        self.doc.handle_ui_event(event);
    }

    pub fn handle_winit_event(&mut self, event: WindowEvent) {
        // Update accessibility focus and window size state in response to a Winit WindowEvent
        #[cfg(feature = "accessibility")]
        self.accessibility
            .process_window_event(&*self.window, &event);

        match event {
            WindowEvent::Destroyed => {}
            WindowEvent::ActivationTokenDone { .. } => {},
            WindowEvent::CloseRequested => {
                // Currently handled at the level above in application.rs
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
            }
            WindowEvent::Moved(_) => {}
            WindowEvent::Occluded(is_occluded) => {
                self.is_visible = !is_occluded;
                if self.is_visible {
                    self.request_redraw();
                }
            },
            WindowEvent::SurfaceResized(physical_size) => {
                self.safe_area_insets = get_safe_area_insets(&*self.window);
                // On WASM, defer the apply: wgpu's surface.configure clears the canvas,
                // so running it every frame flickers during a drag. The browser stretches
                // the stale backing store until the debounce timer settles.
                #[cfg(target_arch = "wasm32")]
                {
                    self.pending_resize = Some(physical_size);
                    self.last_resize_at = Some(web_time::Instant::now());
                    if !self.resize_timer_scheduled {
                        self.schedule_resize_settle_check(Self::RESIZE_DEBOUNCE_MS);
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let insets = self.safe_area_insets;
                    let width = physical_size.width - insets.left - insets.right;
                    let height = physical_size.height - insets.top - insets.bottom;
                    self.with_viewport(|v| v.window_size = (width, height));
                    self.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.with_viewport(|v| v.set_hidpi_scale(scale_factor as f32));
                self.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                let color_scheme = theme_to_color_scheme(self.theme_override.unwrap_or(theme));
                let mut inner = self.doc.inner_mut();
                inner.viewport_mut().color_scheme = color_scheme;
            }
            WindowEvent::Ime(ime_event) => {
                self.doc.handle_ui_event(UiEvent::Ime(winit_ime_to_blitz(ime_event)));
                self.request_redraw();
            },
            WindowEvent::ModifiersChanged(new_state) => {
                // Store new keyboard modifier (ctrl, shift, etc) state for later use
                self.keyboard_modifiers = new_state;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key_code) = event.physical_key && event.state.is_pressed() {
                        let ctrl = self.keyboard_modifiers.state().control_key();
                        let meta = self.keyboard_modifiers.state().meta_key();
                        let alt = self.keyboard_modifiers.state().alt_key();

                        // Ctrl/Super keyboard shortcuts
                        if ctrl | meta {
                            match key_code {
                                KeyCode::Equal => {
                                    self.doc.inner_mut().viewport_mut().zoom_by(0.1);
                                },
                                KeyCode::Minus => {
                                    self.doc.inner_mut().viewport_mut().zoom_by(-0.1);
                                },
                                KeyCode::Digit0 => {
                                    self.doc.inner_mut().viewport_mut().set_zoom(1.0);
                                }
                                _ => {}
                            };
                        }

                        // Alt keyboard shortcuts
                        if alt {
                            match key_code {
                                KeyCode::KeyD => {
                                    let mut inner = self.doc.inner_mut();
                                    inner.devtools_mut().toggle_show_layout();
                                    drop(inner);
                                    self.request_redraw();
                                }
                                KeyCode::KeyH => {
                                    let mut inner = self.doc.inner_mut();
                                    inner.devtools_mut().toggle_highlight_hover();
                                    drop(inner);
                                    self.request_redraw();
                                }
                                KeyCode::KeyT => self.doc.inner().print_taffy_tree(),
                                _ => {}
                            };
                        }

                }

                // Unmodified keypresses
                let key_event_data = winit_key_event_to_blitz(&event, self.keyboard_modifiers.state());
                let event = if event.state.is_pressed() {
                    UiEvent::KeyDown(key_event_data)
                } else {
                    UiEvent::KeyUp(key_event_data)
                };

                self.doc.handle_ui_event(event);
            }
            WindowEvent::PointerEntered { /*device_id*/.. } => {}
            WindowEvent::PointerLeft { position, primary, kind, .. } => {
                let id = pointer_kind_to_blitz(&kind);

                // A `PointerLeft` for a non-mouse pointer that is still pressed
                // (i.e. we never saw a `PointerButton` with `Released` for it)
                // means the system cancelled tracking of this touch/pen. Emit a
                // pointercancel in that case. A mouse simply leaving the window,
                // or a touch that was already released, is not a cancellation.
                // Remove from the active list first so the cancelled pointer is
                // excluded from this event's `touches`. `remove_active_pointer`
                // reports whether the pointer was actually active.
                if id != BlitzPointerId::Mouse && self.remove_active_pointer(id) {
                    let position = position.unwrap_or(self.pointer_pos);
                    self.pointer_pos = position;

                    // The pointer is no longer pressed.
                    self.buttons ^= MouseEventButton::Main.into();

                    let event = BlitzPointerEvent {
                        id,
                        is_primary: primary,
                        coords: self.pointer_coords(position),
                        button: MouseEventButton::Main,
                        buttons: self.buttons,
                        mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                        details: PointerDetails::default(),
                        element: Default::default(),
                        active_pointers: Arc::clone(&self.active_events),
                    };

                    self.doc.handle_ui_event(UiEvent::PointerCancel(event));
                    self.request_redraw();
                }
            }
            WindowEvent::PointerMoved { position, source, primary, .. } => {
                self.pointer_pos = position;
                let id = pointer_source_to_blitz(&source);
                let event = BlitzPointerEvent {
                    id,
                    is_primary: primary,
                    coords: self.pointer_coords(position),
                    button: Default::default(),
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                    details: pointer_source_to_blitz_details(&source),
                    element: Default::default(),
                    active_pointers: Arc::clone(&self.active_events),
                };
                // Keep multi-touch positions current (no-op for non-active pointers).
                if id != BlitzPointerId::Mouse {
                    self.update_active_pointer(&event);
                }
                self.doc.handle_ui_event(UiEvent::PointerMove(event));
            }
            WindowEvent::PointerButton { button, state, primary, position, .. } => {
                let id = button_source_to_blitz(&button);
                let coords = self.pointer_coords(position);
                self.pointer_pos = position;
                let button = match &button {
                    ButtonSource::Mouse(mouse_button) => match mouse_button {
                        MouseButton::Left => MouseEventButton::Main,
                        MouseButton::Right => MouseEventButton::Secondary,
                        MouseButton::Middle => MouseEventButton::Auxiliary,
                        // TODO: handle other button types
                        _ => MouseEventButton::Auxiliary,
                    }
                    _ => MouseEventButton::Main,
                };

                match state {
                    ElementState::Pressed => self.buttons |= button.into(),
                    ElementState::Released => self.buttons ^= button.into(),
                }

                let pointer_event = BlitzPointerEvent {
                    id,
                    is_primary: primary,
                    coords,
                    button,
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),

                    // TODO: details for pointer up/down events
                    details: PointerDetails::default(),
                    element: Default::default(),
                    active_pointers: Arc::clone(&self.active_events),
                };

                // Maintain the list of active (pressed) non-mouse pointers. A
                // press adds the pointer *before* dispatch (so touchstart's
                // `touches` includes it). A release is handled after the
                // synthetic move below so the move still sees it, but before the
                // pointerup so touchend's `touches` excludes it.
                if id != BlitzPointerId::Mouse && state == ElementState::Pressed {
                    self.set_active_pointer(&pointer_event);
                }

                // Touch input doesn't emit a `PointerMoved` before the button
                // event the way a mouse does, so synthesise a move to update the
                // hover/hit position to the touch location.
                if id != BlitzPointerId::Mouse {
                    let event = BlitzPointerEvent {
                        id,
                        is_primary: primary,
                        coords,
                        button: Default::default(),
                        buttons: self.buttons,
                        mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                        details: PointerDetails::default(),
                        element: Default::default(),
                        active_pointers: Arc::clone(&self.active_events),
                    };
                    self.doc.handle_ui_event(UiEvent::PointerMove(event));
                }

                if id != BlitzPointerId::Mouse && state == ElementState::Released {
                    self.remove_active_pointer(id);
                }

                let event = pointer_event;

                let event = match state {
                    ElementState::Pressed => UiEvent::PointerDown(event),
                    ElementState::Released => UiEvent::PointerUp(event),
                };

                self.doc.handle_ui_event(event);
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let blitz_delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => BlitzWheelDelta::Lines(x as f64, y as f64),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => BlitzWheelDelta::Pixels(pos.x, pos.y),
                };

                let event = BlitzWheelEvent {
                    delta: blitz_delta,
                    coords: self.pointer_coords(self.pointer_pos),
                    buttons: self.buttons,
                    mods: winit_modifiers_to_kbt_modifiers(self.keyboard_modifiers.state()),
                    element: Default::default()
                };

                self.doc.handle_ui_event(UiEvent::Wheel(event));
            }
            WindowEvent::Focused(_) => {}
            WindowEvent::TouchpadPressure { .. } => {}
            WindowEvent::PinchGesture { .. } => {},
            WindowEvent::PanGesture { .. } => {},
            WindowEvent::DoubleTapGesture { .. } => {},
            WindowEvent::RotationGesture { .. } => {},
            WindowEvent::DragEntered { .. } => {},
            WindowEvent::DragMoved { .. } => {},
            WindowEvent::DragDropped { .. } => {},
            WindowEvent::DragLeft { .. } => {},
        }
    }
}

struct FrameStats {
    enabled: bool,
    output_path: Option<PathBuf>,
    refresh_millihertz: Option<u32>,
    last_frame_started: Option<Instant>,
    sample_started: Instant,
    frames: u32,
    active_intervals: u32,
    missed_refreshes: u32,
    interval_total: Duration,
    interval_max: Duration,
    resolve_total: Duration,
    paint_total: Duration,
    renderer_total: Duration,
}

impl FrameStats {
    fn emit(output_path: Option<&PathBuf>, message: &str) {
        eprintln!("{message}");
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = output_path
            && let Ok(mut output) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = std::io::Write::write_all(&mut output, message.as_bytes());
            let _ = std::io::Write::write_all(&mut output, b"\n");
        }
    }

    fn new(window: &dyn Window) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var_os("BLITZ_FRAME_STATS").is_some();
        #[cfg(target_arch = "wasm32")]
        let enabled = false;
        #[cfg(not(target_arch = "wasm32"))]
        let output_path = std::env::var_os("BLITZ_FRAME_STATS_FILE").map(PathBuf::from);
        #[cfg(target_arch = "wasm32")]
        let output_path = None;

        let refresh_millihertz = window
            .current_monitor()
            .and_then(|monitor| monitor.current_video_mode())
            .and_then(|mode| mode.refresh_rate_millihertz())
            .map(std::num::NonZeroU32::get);

        // Publish the refresh rate even when the log line is off. The shared frame
        // log needs it to tell a late frame from an on-time one, and that readout
        // is not gated on BLITZ_FRAME_STATS.
        crate::frame_stats::set_display_refresh_millihertz(refresh_millihertz);

        if enabled {
            let message = match refresh_millihertz {
                Some(rate) => format!(
                    "[blitz-frame] display_refresh_hz={:.3}",
                    f64::from(rate) / 1000.0
                ),
                None => "[blitz-frame] display_refresh_hz=unknown".to_owned(),
            };
            Self::emit(output_path.as_ref(), &message);
        }

        Self {
            enabled,
            output_path,
            refresh_millihertz,
            last_frame_started: None,
            sample_started: Instant::now(),
            frames: 0,
            active_intervals: 0,
            missed_refreshes: 0,
            interval_total: Duration::ZERO,
            interval_max: Duration::ZERO,
            resolve_total: Duration::ZERO,
            paint_total: Duration::ZERO,
            renderer_total: Duration::ZERO,
        }
    }

    fn record(
        &mut self,
        frame_started: Instant,
        resolve: Duration,
        paint: Duration,
        renderer: Duration,
    ) {
        // Publish every frame to the process-global log before the enabled check.
        // Out-of-band readers (the MCP diagnostics endpoint) need real numbers from
        // a normally launched app; gating this on BLITZ_FRAME_STATS would leave them
        // with nothing to report, which is what previously drove that endpoint to
        // time its own snapshot collection and present it as frame cost.
        crate::frame_stats::record_frame(frame_started, resolve, paint, renderer);

        if !self.enabled {
            return;
        }

        if let Some(previous) = self.last_frame_started.replace(frame_started) {
            let interval = frame_started.duration_since(previous);
            // Ignore idle gaps. These statistics describe active interaction bursts,
            // not the intentional zero-FPS idle state.
            if interval <= Duration::from_millis(100) {
                self.active_intervals += 1;
                self.interval_total += interval;
                self.interval_max = self.interval_max.max(interval);

                if let Some(rate) = self.refresh_millihertz {
                    let target = Duration::from_secs_f64(1000.0 / f64::from(rate));
                    if interval > target.mul_f64(1.5) {
                        self.missed_refreshes += 1;
                    }
                }
            }
        }

        self.frames += 1;
        self.resolve_total += resolve;
        self.paint_total += paint;
        self.renderer_total += renderer;

        let sample_elapsed = self.sample_started.elapsed();
        if sample_elapsed < Duration::from_secs(1) || self.frames < 2 {
            return;
        }

        let active_fps = if self.interval_total.is_zero() {
            0.0
        } else {
            f64::from(self.active_intervals) / self.interval_total.as_secs_f64()
        };
        let frames = f64::from(self.frames);
        let message = format!(
            "[blitz-frame] active_fps={active_fps:.1} frames={} active_intervals={} missed_refreshes={} max_interval_ms={:.2} resolve_avg_ms={:.2} paint_avg_ms={:.2} renderer_avg_ms={:.2}",
            self.frames,
            self.active_intervals,
            self.missed_refreshes,
            self.interval_max.as_secs_f64() * 1000.0,
            self.resolve_total.as_secs_f64() * 1000.0 / frames,
            self.paint_total.as_secs_f64() * 1000.0 / frames,
            self.renderer_total.as_secs_f64() * 1000.0 / frames,
        );
        Self::emit(self.output_path.as_ref(), &message);

        self.sample_started = frame_started;
        self.frames = 0;
        self.active_intervals = 0;
        self.missed_refreshes = 0;
        self.interval_total = Duration::ZERO;
        self.interval_max = Duration::ZERO;
        self.resolve_total = Duration::ZERO;
        self.paint_total = Duration::ZERO;
        self.renderer_total = Duration::ZERO;
    }
}
