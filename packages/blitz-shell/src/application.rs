use crate::event::{BlitzShellEvent, BlitzShellProxy};

use anyrender::WindowRenderer;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::window::WindowId;

#[cfg(target_os = "macos")]
use winit::platform::macos::ApplicationHandlerExtMacOS;

use crate::{View, WindowConfig};

pub struct BlitzApplication<Rend: WindowRenderer> {
    pub windows: HashMap<WindowId, View<Rend>>,
    pub pending_windows: Vec<WindowConfig<Rend>>,
    pub proxy: BlitzShellProxy,
    pub event_queue: Receiver<BlitzShellEvent>,
    #[cfg(feature = "debug-control")]
    debug_controller: Option<blitz_script::DebugController>,
}

impl<Rend: WindowRenderer> BlitzApplication<Rend> {
    pub fn new(proxy: BlitzShellProxy, event_queue: Receiver<BlitzShellEvent>) -> Self {
        BlitzApplication {
            windows: HashMap::new(),
            pending_windows: Vec::new(),
            proxy,
            event_queue,
            #[cfg(feature = "debug-control")]
            debug_controller: None,
        }
    }

    pub fn add_window(&mut self, window_config: WindowConfig<Rend>) {
        self.pending_windows.push(window_config);
    }

    #[cfg(feature = "debug-control")]
    pub fn set_debug_controller(&mut self, controller: blitz_script::DebugController) {
        // The server thread wakes the loop when a request lands, the same way
        // everything else here does. Before this the loop woke itself every
        // 10ms to look: 100 wakeups a second at idle, up to 10ms of latency on
        // each command, and a floor under every measurement taken with the
        // driver attached, which is most of them.
        let proxy = self.proxy.clone();
        controller.set_waker(move || proxy.wake_up());
        self.debug_controller = Some(controller);
    }

    #[cfg(feature = "debug-control")]
    fn service_debug_controller(&mut self, event_loop: &dyn ActiveEventLoop) {
        let (Some(controller), windows) = (self.debug_controller.as_mut(), &mut self.windows)
        else {
            return;
        };
        let Some((animation_time, document)) = windows.values_mut().find_map(|view| {
            let animation_time = view.current_animation_time();
            view.try_downcast_doc_mut::<blitz_script::ScriptDocument>()
                .map(|document| (animation_time, document))
        }) else {
            // No document to run against yet. The request stays queued, and
            // whatever creates the window brings the loop round again.
            return;
        };
        controller.service_pending_at(document, animation_time);
        if controller.exit_requested() {
            event_loop.exit();
        }
    }

    fn window_mut_by_doc_id(&mut self, doc_id: usize) -> Option<&mut View<Rend>> {
        self.windows.values_mut().find(|w| w.doc.id() == doc_id)
    }

    pub fn handle_blitz_shell_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        event: BlitzShellEvent,
    ) {
        match event {
            BlitzShellEvent::Poll { window_id } => {
                // Kept for embedders that send it. The poll itself happens in
                // `about_to_wait` with every other request, which runs before
                // the loop sleeps, so this is not deferred past this turn.
                if let Some(window) = self.windows.get(&window_id) {
                    window.request_poll();
                };
            }
            BlitzShellEvent::CloseWindow { window_id } => {
                // Drop window before exiting event loop
                // See https://github.com/rust-windowing/winit/issues/4135
                let window = self.windows.remove(&window_id);
                drop(window);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            BlitzShellEvent::ResumeReady { window_id } => {
                // The renderer fires `on_ready` after it has sent on the
                // channel, so `complete_resume` should always succeed here.
                // If a stale event survives a suspend, dropping it is safe.
                let _paint_committed = if let Some(window) = self.windows.get_mut(&window_id)
                    && window.waker.is_none()
                {
                    let ok = window.complete_resume();
                    debug_assert!(ok, "ResumeReady received but renderer not ready");
                    ok
                } else {
                    false
                };
                #[cfg(feature = "debug-control")]
                if _paint_committed && let Some(controller) = self.debug_controller.as_mut() {
                    controller.note_paint_committed();
                }
            }
            BlitzShellEvent::RequestRedraw { doc_id } => {
                // TODO: Handle multiple documents per window
                if let Some(window) = self.window_mut_by_doc_id(doc_id) {
                    window.request_redraw();
                }
            }

            #[cfg(feature = "accessibility")]
            BlitzShellEvent::Accessibility { window_id, data } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    match &*data {
                        accesskit_xplat::WindowEvent::InitialTreeRequested => {
                            window.build_accessibility_tree();
                        }
                        accesskit_xplat::WindowEvent::AccessibilityDeactivated => {
                            // TODO
                        }
                        accesskit_xplat::WindowEvent::ActionRequested(_req) => {
                            // TODO
                        }
                    }
                }
            }
            BlitzShellEvent::Embedder(_) => {
                // Do nothing. Should be handled by embedders (if required).
            }
            BlitzShellEvent::Navigate(_opts) => {
                // Do nothing. Should be handled by embedders (if required).
            }
            BlitzShellEvent::NavigationLoad { .. } => {
                // Do nothing. Should be handled by embedders (if required).
            }
            #[cfg(target_arch = "wasm32")]
            BlitzShellEvent::ResizeSettleCheck { window_id } => {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    window.apply_pending_resize_if_settled();
                }
            }
        }
    }
}

impl<Rend: WindowRenderer> ApplicationHandler for BlitzApplication<Rend> {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        #[cfg(feature = "debug-control")]
        let mut committed_frames = 0usize;
        // Resume existing windows
        for view in self.windows.values_mut() {
            view.resume();
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ok = view.complete_resume();
                debug_assert!(ok, "native renderer did not resume synchronously");
                #[cfg(feature = "debug-control")]
                if ok {
                    committed_frames += 1;
                }
            }
        }

        // Initialise pending windows. The renderer's resume is non-blocking —
        // on native it finishes inline, on wasm32 it spawns a future that will
        // dispatch BlitzShellEvent::ResumeReady when init completes. Either way
        // we insert the view immediately so the event handler can find it.
        for window_config in self.pending_windows.drain(..) {
            let mut view = View::init(window_config, event_loop, &self.proxy);
            view.resume();
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ok = view.complete_resume();
                debug_assert!(ok, "native renderer did not resume synchronously");
                #[cfg(feature = "debug-control")]
                if ok {
                    committed_frames += 1;
                }
            }
            self.windows.insert(view.window_id(), view);
        }
        #[cfg(feature = "debug-control")]
        if let Some(controller) = self.debug_controller.as_mut() {
            for _ in 0..committed_frames {
                controller.note_paint_committed();
            }
        }
    }

    fn destroy_surfaces(&mut self, _event_loop: &dyn ActiveEventLoop) {
        for view in self.windows.values_mut() {
            view.suspend();
        }
    }

    fn resumed(&mut self, _event_loop: &dyn ActiveEventLoop) {
        // TODO
    }

    fn suspended(&mut self, _event_loop: &dyn ActiveEventLoop) {
        // TODO
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // Exit the app when window close is requested.
        if matches!(event, WindowEvent::CloseRequested) {
            // Drop window before exiting event loop
            // See https://github.com/rust-windowing/winit/issues/4135
            let window = self.windows.remove(&window_id);
            drop(window);
            if self.windows.is_empty() {
                event_loop.exit();
            }
            return;
        }

        let _paint_committed = if let Some(window) = self.windows.get_mut(&window_id) {
            let committed = window.handle_winit_event(event);
            // Flag rather than a queued event and a wake: this runs on the
            // event loop's own thread, `about_to_wait` follows before the loop
            // sleeps, and a drag delivers hundreds of these a second.
            window.request_poll();
            committed
        } else {
            false
        };
        #[cfg(feature = "debug-control")]
        if _paint_committed && let Some(controller) = self.debug_controller.as_mut() {
            controller.note_paint_committed();
        }
    }

    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        while let Ok(event) = self.event_queue.try_recv() {
            self.handle_blitz_shell_event(event_loop, event);
        }
        #[cfg(feature = "debug-control")]
        self.service_debug_controller(event_loop);
    }

    #[cfg(target_os = "macos")]
    fn macos_handler(&mut self) -> Option<&mut dyn ApplicationHandlerExtMacOS> {
        Some(self)
    }

    fn about_to_wait(&mut self, event_loop: &dyn ActiveEventLoop) {
        let _ = event_loop;
        #[cfg(feature = "debug-control")]
        self.service_debug_controller(event_loop);

        // Every poll asked for since the loop last slept, coalesced to one per
        // window. Before the animation deadline below, because a poll is what
        // schedules the next animation frame.
        for window in self.windows.values_mut() {
            window.poll_if_requested();
        }

        #[cfg(target_os = "ios")]
        for view in self.windows.values_mut() {
            if view.ios_request_redraw.get() {
                view.window.request_redraw();
            }
        }

        // Animation frames are paced here rather than requested at the end of
        // the last one, which is what would run them at the display's rate. The
        // earliest deadline across every window becomes the wait, so a window
        // that is animating does not stop the others sleeping.
        //
        // Restored to `Wait` when nothing is animating, rather than left alone.
        //
        // `ControlFlow::Wait` is the default, but it is not what the loop is
        // still set to once an animation has ended: the last frame's
        // `WaitUntil` stays in force, its deadline is already in the past, and
        // the loop then wakes immediately, forever, with nothing to do. It
        // costs 76% of a core on an idle window, and it is invisible in a
        // profile of the app because no frame of ours is on the stack: the
        // whole main thread sits in `__CFRunLoopDoTimers` re-arming a timer.
        //
        // web_time, not std: on wasm they are genuinely distinct types, and
        // both `poll_animation_frame` and winit's own `ControlFlow::WaitUntil`
        // are in web_time's. On native web_time re-exports std's, which is why
        // std compiled here and broke only the wasm job.
        let now = web_time::Instant::now();
        let next_frame = self
            .windows
            .values()
            .filter_map(|view| view.poll_animation_frame(now))
            .min();
        if let Some(deadline) = next_frame {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

#[cfg(target_os = "macos")]
impl<Rend: WindowRenderer> ApplicationHandlerExtMacOS for BlitzApplication<Rend> {
    fn standard_key_binding(
        &mut self,
        _event_loop: &dyn ActiveEventLoop,
        window_id: WindowId,
        action: &str,
    ) {
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.handle_apple_standard_keybinding(action);
            window.request_poll();
        }
    }
}
