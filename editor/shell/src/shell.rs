//! The host shell: the winit toplevel, the windowless CEF browser bound to it, and the event loop
//! that forwards input, paces the pump, and drives teardown.

use crate::backend::{self, UiCompositor};
use crate::dnd::DndEvent;
use crate::handlers::{ClientBuilder, DragRegions, DragState, ShellRenderHandler};
use crate::keymap::{
    EVENTFLAG_ALT_DOWN, EVENTFLAG_COMMAND_DOWN, EVENTFLAG_CONTROL_DOWN,
    EVENTFLAG_LEFT_MOUSE_BUTTON, EVENTFLAG_MIDDLE_MOUSE_BUTTON, EVENTFLAG_RIGHT_MOUSE_BUTTON,
    EVENTFLAG_SHIFT_DOWN, vk_from_keycode,
};
use crate::state::{self, ResizeEdge, ShellState, WindowAction};
use crate::window::ShellWindow;
use crate::{engine, geometry};
use cef::wrapper::message_router::BrowserSideRouter;
use cef::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::PhysicalKey,
    window::{ResizeDirection, WindowAttributes, WindowId},
};

/// The host shell: owns the one winit toplevel (via `ShellWindow`) and the windowless CEF browser
/// bound to it. CEF is created lazily in `resumed`, once the toplevel (and thus its size) exists.
pub(crate) struct Shell {
    pub(crate) shell_window: Option<ShellWindow>,
    browser: Option<cef::Browser>,
    /// The browser-side message router; the `Client` forwards `on_process_message_received` to it.
    router: Arc<BrowserSideRouter>,
    /// Shared with the `RenderHandler` so a resize reaches CEF's `view_rect` (single-threaded: CEF
    /// OSR callbacks fire on this same main thread that owns the winit loop).
    size: Rc<RefCell<(i32, i32)>>,
    /// The backend's OSR device-scale, shared with the `RenderHandler`'s `view_rect`/`screen_info`.
    /// Input coordinates hand CEF physical ÷ this (CEF's OSR view is logical at this scale).
    scale: Rc<RefCell<f64>>,
    /// Shared with the `RenderHandler`; populated in `resumed` once the toplevel exists.
    pub(crate) compositor: Rc<RefCell<Option<UiCompositor>>>,
    paints: Arc<AtomicU64>,
    url: String,
    /// Shared engine/window/IPC state; the main loop drains its request inbox and reads/writes its
    /// window tracker. The IPC router holds its own clone of the same `Arc`.
    state: Arc<ShellState>,
    pub(crate) revealed: bool,
    /// Last pointer position in device pixels, carried into CEF click/wheel events (which do not
    /// themselves carry a position). CEF's OSR view is device-pixel, matching winit's physical
    /// `CursorMoved` — full fractional-scale hit-test correctness is a Phase-3 follow-up.
    cursor: (i32, i32),
    /// Fixed pacing override (`SAFFRON_SHELL_TARGET_HZ`) for headless measurement; when set, the
    /// monitor-refresh auto-detection below is bypassed.
    target_hz_override: Option<f64>,
    /// Frame rate currently applied to CEF and the pump loop, in Hz. Provisional until the toplevel
    /// maps, then retargeted to the monitor's refresh (see `retarget_refresh`) — so the UI renders at
    /// the display's rate (vsync), not a fixed 240 that wastes work on a 144 Hz panel.
    pub(crate) refresh_hz: f64,
    /// Current keyboard modifiers as CEF event-flag bits, tracked from `ModifiersChanged` and applied
    /// to every key and mouse event so shortcuts (Ctrl/Alt/Shift/Super) and shift-click work.
    modifiers: u32,
    /// Currently-held mouse buttons as CEF event-flag bits. Carried on every mouse *move* so Chromium
    /// sees a button-held drag (not a hover) — without it, click-drag gestures (dock tab tear-out,
    /// text selection, slider scrubs) break once past the initiating element.
    mouse_buttons: u32,
    /// The previous mouse-press (button, when, position) and the running click count, so a press
    /// within the double-click window + radius bumps the count to 2/3. CEF needs `click_count >= 2`
    /// on the second press for the DOM `dblclick` to fire (e.g. double-click an asset to open it).
    last_press: Option<(MouseButton, Instant, (i32, i32))>,
    click_count: i32,
    /// The in-flight OSR drag, shared with the `RenderHandler` so its `start_dragging` opens the drag
    /// and the winit pointer handlers drive `drag_target_drag_over`/`drag_target_drop` to completion.
    drag: Rc<RefCell<DragState>>,
    /// The frontend's `-webkit-app-region: drag` titlebar rectangles, shared with the `DragHandler`.
    /// A left press inside one starts a native window drag instead of a content click.
    drag_regions: DragRegions,
    /// Whether the RMB fly-cam has grabbed the pointer (CEF's windowless OSR can't do DOM pointer lock,
    /// so the shell locks the cursor natively). While set, `CursorMoved` stops and the relative motion
    /// arrives as `DeviceEvent::MouseMotion`, accumulated in `look_accum` and streamed to the frontend
    /// as a `fly-look` event.
    pub(crate) pointer_locked: bool,
    /// Raw relative-motion delta accumulated since the last `fly-look` emit (only while `pointer_locked`).
    pub(crate) look_accum: (f64, f64),
    /// The staged-exit state (see `new_events`): `None` while running; `Some(n)` counts the grace
    /// iterations between teardown and `event_loop.exit()` that let the platform replay the
    /// window close's queued events into the still-installed handler.
    teardown_grace: Option<u8>,
}

impl Shell {
    /// Build the shell around the browser-side router and the shared engine/window state. The
    /// browser itself is created lazily in `resumed`, once the toplevel (and thus its size) exists.
    pub(crate) fn new(
        router: Arc<BrowserSideRouter>,
        state: Arc<ShellState>,
        url: String,
        target_hz_override: Option<f64>,
        paints: Arc<AtomicU64>,
    ) -> Self {
        Self {
            shell_window: None,
            browser: None,
            router,
            size: Rc::new(RefCell::new((1600, 900))),
            scale: Rc::new(RefCell::new(1.0)),
            compositor: Rc::new(RefCell::new(None)),
            paints,
            url,
            state,
            revealed: false,
            cursor: (0, 0),
            target_hz_override,
            refresh_hz: target_hz_override.unwrap_or(60.0),
            modifiers: 0,
            mouse_buttons: 0,
            last_press: None,
            click_count: 0,
            drag: Rc::new(RefCell::new(DragState::default())),
            drag_regions: Rc::new(RefCell::new(Vec::new())),
            pointer_locked: false,
            look_accum: (0.0, 0.0),
            teardown_grace: None,
        }
    }

    /// The loop-entangled half of teardown, run from `new_events` while the loop is still
    /// pumping. Dependency order: the engine (socket + shm), the CEF browser ref, the platform
    /// CEF pump, the compositor (built over the window's handles), and the toplevel itself — the
    /// shell holds the window's only strong ref (CEF's display handler is `Weak`), so the drop
    /// here closes the platform window while its events can still be delivered. `cef::shutdown`
    /// deliberately does NOT run here: CEF aborts when shut down from inside a handler callback's
    /// nested context, so `main` calls it after the pump returns — by then the browser ref is
    /// gone and every CEF callback that can still fire (`on_paint`, cursor changes) no-ops
    /// against the emptied `Option`s/`Weak`.
    fn teardown(&mut self) {
        engine::teardown(&self.state);
        self.flush_geometry();
        self.browser = None;
        // Stop the platform CEF pump before `cef::shutdown`, which drains the loop itself — the
        // pump firing into that would call `do_message_loop_work` re-entrantly mid-shutdown.
        backend::bootstrap::uninstall_cef_pump();
        self.compositor.borrow_mut().take();
        self.shell_window = None;
    }

    /// Whether a logical-coordinate point starts a window drag: the topmost (last-painted) region
    /// containing it is draggable. CEF reports the `no-drag` holes (tabs, buttons) as `false`
    /// regions on top of the titlebar's `true` region, so a press on a control falls through to
    /// CEF instead of dragging the window.
    fn in_drag_region(&self, (x, y): (i32, i32)) -> bool {
        self.drag_regions
            .borrow()
            .iter()
            .rev()
            .find(|(bounds, _)| {
                x >= bounds.x
                    && x < bounds.x + bounds.width
                    && y >= bounds.y
                    && y < bounds.y + bounds.height
            })
            .is_some_and(|(_, draggable)| *draggable)
    }

    /// The pointer position in CEF's logical view units (physical ÷ the backend's OSR scale).
    fn cursor_logical(&self) -> (i32, i32) {
        let scale = *self.scale.borrow();
        (
            (f64::from(self.cursor.0) / scale).round() as i32,
            (f64::from(self.cursor.1) / scale).round() as i32,
        )
    }

    fn mouse_event(&self) -> MouseEvent {
        let (x, y) = self.cursor_logical();
        MouseEvent {
            x,
            y,
            modifiers: self.modifiers | self.mouse_buttons,
        }
    }

    /// Forward a winit key event to CEF: a `RAWKEYDOWN`/`KEYUP` carrying the Windows VK code, plus a
    /// `CHAR` per produced UTF-16 unit on press so text enters focused fields. This RAWKEYDOWN→CHAR→
    /// KEYUP sequence is the cefclient Linux-OSR recipe; text entry rides on the CHAR events while the
    /// VK code drives editing/navigation keys and shortcuts.
    fn send_key(&self, event: &winit::event::KeyEvent) {
        let Some(host) = self.browser.as_ref().and_then(|b| b.host()) else {
            return;
        };
        let (vk, native) = match event.physical_key {
            PhysicalKey::Code(code) => (
                vk_from_keycode(code),
                backend::keys::native_from_keycode(code),
            ),
            PhysicalKey::Unidentified(_) => (0, 0),
        };
        match event.state {
            ElementState::Pressed => {
                host.send_key_event(Some(&KeyEvent {
                    type_: KeyEventType::RAWKEYDOWN,
                    modifiers: self.modifiers,
                    windows_key_code: vk,
                    native_key_code: native,
                    ..Default::default()
                }));
                if let Some(text) = &event.text {
                    for unit in text.encode_utf16() {
                        host.send_key_event(Some(&KeyEvent {
                            type_: KeyEventType::CHAR,
                            modifiers: self.modifiers,
                            windows_key_code: i32::from(unit),
                            character: unit,
                            unmodified_character: unit,
                            ..Default::default()
                        }));
                    }
                }
            }
            ElementState::Released => {
                host.send_key_event(Some(&KeyEvent {
                    type_: KeyEventType::KEYUP,
                    modifiers: self.modifiers,
                    windows_key_code: vk,
                    native_key_code: native,
                    ..Default::default()
                }));
            }
        }
    }

    /// Retarget CEF's windowless frame rate to the monitor the toplevel is on (its refresh rate), or
    /// to the fixed `SAFFRON_SHELL_TARGET_HZ` override. Returns the new rate when it changes so the
    /// pump loop can repace. A no-op until the surface has entered an output (post-map on Wayland,
    /// where `current_monitor` is `None` before) or when the rate is unchanged — so it's safe to poll.
    pub(crate) fn retarget_refresh(&mut self) -> Option<f64> {
        let desired = match self.target_hz_override {
            Some(hz) => hz,
            None => {
                let mhz = self
                    .shell_window
                    .as_ref()?
                    .window()
                    .current_monitor()?
                    .refresh_rate_millihertz()?;
                (f64::from(mhz) / 1000.0).round()
            }
        };
        if (desired - self.refresh_hz).abs() < 0.5 {
            return None;
        }
        self.refresh_hz = desired;
        if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
            host.set_windowless_frame_rate(desired as i32);
        }
        tracing::info!(target: "shell", "vsync: pacing CEF to {desired} Hz (monitor refresh)");
        Some(desired)
    }

    /// Apply a marshaled window control on the main thread (where the winit window lives).
    pub(crate) fn apply_window_action(&mut self, action: WindowAction) {
        let Some(shell_window) = self.shell_window.as_ref() else {
            return;
        };
        match action {
            WindowAction::Minimize => shell_window.minimize(),
            WindowAction::ToggleMaximize => shell_window.toggle_maximize(),
            WindowAction::StartResize(edge) => {
                let direction = match edge {
                    ResizeEdge::North => ResizeDirection::North,
                    ResizeEdge::South => ResizeDirection::South,
                    ResizeEdge::East => ResizeDirection::East,
                    ResizeEdge::West => ResizeDirection::West,
                    ResizeEdge::NorthEast => ResizeDirection::NorthEast,
                    ResizeEdge::NorthWest => ResizeDirection::NorthWest,
                    ResizeEdge::SouthEast => ResizeDirection::SouthEast,
                    ResizeEdge::SouthWest => ResizeDirection::SouthWest,
                };
                shell_window.drag_resize(direction);
            }
            WindowAction::SetPointerLock(locked) => {
                shell_window.set_pointer_lock(locked);
                self.pointer_locked = locked;
                self.look_accum = (0.0, 0.0);
            }
            WindowAction::Show => {
                shell_window.reveal();
                self.revealed = true;
            }
        }
    }

    /// Re-emit a compositor file-drag step as the frontend's `drag-drop` event. The payload discriminant
    /// (`over`/`leave`/`drop`) + `paths` + `position` match the union the frontend already consumes; the
    /// positions are the drag's own surface-local device pixels.
    pub(crate) fn emit_dnd(&self, ev: &DndEvent) {
        let payload = match ev {
            DndEvent::Over { x, y } => serde_json::json!({
                "type": "over",
                "paths": serde_json::Value::Array(Vec::new()),
                "position": { "x": x, "y": y },
            }),
            DndEvent::Leave => serde_json::json!({ "type": "leave" }),
            DndEvent::Drop { paths, x, y } => serde_json::json!({
                "type": "drop",
                "paths": paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                "position": { "x": x, "y": y },
            }),
        };
        self.state.emit("drag-drop", payload);
    }

    /// Push a frontend event by running `window.__saffronShellEvent(event, payload)` in the main
    /// frame. `payload` is a ready JSON literal; `event` is escaped as a JSON string.
    pub(crate) fn emit_to_js(&self, event: &str, payload: &str) {
        let Some(frame) = self.browser.as_ref().and_then(|b| b.main_frame()) else {
            return;
        };
        let event = serde_json::to_string(event).unwrap_or_else(|_| "\"\"".to_string());
        let code = CefString::from(
            format!("window.__saffronShellEvent&&window.__saffronShellEvent({event},{payload})")
                .as_str(),
        );
        frame.execute_java_script(Some(&code), None, 0);
    }

    fn flush_geometry(&self) {
        let snapshot = self
            .state
            .window
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Err(err) = geometry::write_state_file(&geometry::RememberedState {
            window: Some(snapshot),
        }) {
            tracing::warn!(target: "shell", "geometry flush failed: {err}");
        }
    }
}

impl ApplicationHandler for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shell_window.is_some() {
            return;
        }
        let window = Arc::new(
            match event_loop.create_window(backend::window::attributes(
                WindowAttributes::default()
                    .with_title("Saffron Anima")
                    .with_inner_size(LogicalSize::new(
                        geometry::MAIN_WINDOW_WIDTH,
                        geometry::MAIN_WINDOW_HEIGHT,
                    ))
                    .with_min_inner_size(LogicalSize::new(
                        geometry::MAIN_WINDOW_MIN_WIDTH,
                        geometry::MAIN_WINDOW_MIN_HEIGHT,
                    ))
                    .with_visible(false),
            )) {
                Ok(window) => window,
                Err(err) => {
                    tracing::error!(target: "shell", "fatal: create toplevel: {err}");
                    self.state.exit_requested.store(true, Ordering::Relaxed);
                    return;
                }
            },
        );

        // One-time platform window setup the attribute builder can't express (e.g. disarming
        // AppKit's automatic titlebar dragging — the frontend is the drag authority).
        backend::window::configure(&window);

        let shell_window = match ShellWindow::new(Arc::clone(&window)) {
            Ok(shell_window) => shell_window,
            Err(err) => {
                tracing::error!(target: "shell", "fatal: {err}");
                self.state.exit_requested.store(true, Ordering::Relaxed);
                return;
            }
        };
        tracing::info!(
            target: "shell",
            "toplevel up; {}",
            shell_window.handles().describe()
        );

        // The UI compositor on the backend's window-system handles. Failure is non-fatal here so
        // the rest of the shell still comes up and the cause is visible in the log.
        match UiCompositor::new(shell_window.handles()) {
            Ok(compositor) => {
                self.compositor.borrow_mut().replace(compositor);
                tracing::info!(target: "shell", "toplevel compositor ready");
            }
            Err(err) => tracing::error!(target: "shell", "compositor init failed: {err}"),
        }

        // The viewport present loop: presents the engine's shared-memory frames below the UI (above
        // the compositor's backdrop). It retries opening each view's shm segment until the engine
        // creates it.
        backend::presenter::install(
            shell_window.handles(),
            state::viewport_shm_name("scene"),
            state::viewport_shm_name("assetPreview"),
            &self.state.viewports,
        );

        // Restore remembered geometry; seed the live tracker from what was applied.
        let applied = geometry::configure_main_window(&window);
        *self
            .state
            .window
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = applied;

        let sz = window.inner_size();
        *self.size.borrow_mut() = (sz.width as i32, sz.height as i32);
        *self.scale.borrow_mut() = backend::window::osr_scale(&window);

        let render_handler = ShellRenderHandler {
            paints: Arc::clone(&self.paints),
            size: Rc::clone(&self.size),
            scale: Rc::clone(&self.scale),
            compositor: Rc::clone(&self.compositor),
            drag: Rc::clone(&self.drag),
        };
        // CEF self-drives its frame clock at `windowless_frame_rate` (240). Measured on CEF 149:
        // this holds a steady 240 Hz `on_paint`, whereas driving frames ourselves with
        // external-begin-frame caps at ~223 (our pump loop's per-iteration overhead rate-limits the
        // begin-frame calls). So external-begin-frame is deliberately off — self-drive is the faster,
        // simpler path here.
        //
        // CPU `on_paint` compositing (not accelerated dma-buf OSR): on NVIDIA + Mutter, CEF only
        // produces the accelerated buffer via ANGLE-Vulkan, and Mutter's Cogl GL backend cannot bind
        // that Vulkan export (fatal "Could not bind the given EGLImage to a CoglTexture2D") — while
        // the ANGLE-GL backend produces no accelerated frames on NVIDIA at all. The CPU path sustains
        // the monitor refresh anyway (with dirty-rect damage), so it is the one path.
        let window_info = WindowInfo {
            windowless_rendering_enabled: true as _,
            ..Default::default()
        };
        let browser_settings = BrowserSettings {
            // Provisional until the toplevel maps and `retarget_refresh` pins it to the monitor.
            windowless_frame_rate: self.refresh_hz.round() as i32,
            ..Default::default()
        };
        let mut client = ClientBuilder::build(
            render_handler,
            Arc::clone(&self.router),
            Arc::clone(&window),
            Rc::clone(&self.drag_regions),
        );
        let url = CefString::from(self.url.as_str());
        self.browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&browser_settings),
            None,
            None,
        );
        assert!(
            self.browser.is_some(),
            "failed to create windowless browser"
        );

        // The winit toplevel is created hidden (revealed after the first paint), so CEF's windowless
        // browser starts occluded and suspends its compositor — it then only paints when an input
        // event wakes it, never for the page's own render. Mark it visible so it paints on damage.
        if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
            host.was_hidden(0);
        }

        window.request_redraw();
        self.shell_window = Some(shell_window);
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let window = match self.shell_window.as_ref() {
            Some(shell_window) => Arc::clone(shell_window.window()),
            None => return,
        };
        // Offer every winit event to the compositor's OS-drag source first (a backend whose file
        // drops arrive through winit accumulates them here; the Wayland backend no-ops). The steps
        // are drained by `pump_dnd` in the main loop, uniformly across backends.
        if let Some(compositor) = self.compositor.borrow_mut().as_mut() {
            compositor.observe_window_event(&event);
        }
        match event {
            // Fold into the staged exit (`new_events`): teardown while the loop still runs, then
            // exit after the grace iterations.
            WindowEvent::CloseRequested => self.state.exit_requested.store(true, Ordering::Relaxed),
            WindowEvent::Resized(sz) => {
                *self.size.borrow_mut() = (sz.width as i32, sz.height as i32);
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.was_resized();
                }
                geometry::capture_window_geometry(&window, &self.state.window);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                *self.scale.borrow_mut() = backend::window::osr_scale(&window);
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.notify_screen_info_changed();
                    host.was_resized();
                }
                geometry::capture_window_geometry(&window, &self.state.window);
            }
            WindowEvent::RedrawRequested => {
                // Compositing happens in `on_paint` (main thread); just keep the redraw alive.
                window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    let drag = self.drag.borrow();
                    if drag.active {
                        host.drag_target_drag_over(Some(&self.mouse_event()), drag.allowed_ops);
                    } else {
                        drop(drag);
                        host.send_mouse_move_event(Some(&self.mouse_event()), 0);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.send_mouse_move_event(Some(&self.mouse_event()), 1);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // A left press inside a frontend-declared titlebar drag region starts a NATIVE
                // window drag, synchronously — the winit mousedown is still the platform's current
                // event, so the OS anchors the drag to the grab point. (A round-tripped request
                // would run against a stale current event and teleport the window.) A second click
                // of a double toggles maximize, the standard titlebar affordance. `no-drag`
                // children (tabs, window controls) are excluded from the regions, so they click
                // through to CEF as normal.
                if matches!((state, button), (ElementState::Pressed, MouseButton::Left))
                    && self.in_drag_region(self.cursor_logical())
                {
                    let now = Instant::now();
                    let double = self.last_press.is_some_and(|(b, t, (px, py))| {
                        b == button
                            && now.duration_since(t) < Duration::from_millis(500)
                            && (px - self.cursor.0).abs() <= 4
                            && (py - self.cursor.1).abs() <= 4
                    });
                    if let Some(shell_window) = self.shell_window.as_ref() {
                        if double {
                            shell_window.toggle_maximize();
                        } else {
                            shell_window.drag_window();
                        }
                    }
                    // Record the press for double-click detection, unless this WAS the second click
                    // (so a third click starts a fresh drag rather than un-maximizing).
                    self.last_press = if double {
                        None
                    } else {
                        Some((button, now, self.cursor))
                    };
                    return;
                }
                let button_type = match button {
                    MouseButton::Left => MouseButtonType::LEFT,
                    MouseButton::Right => MouseButtonType::RIGHT,
                    MouseButton::Middle => MouseButtonType::MIDDLE,
                    // Side buttons (back/forward = 8/9) drive editor shortcuts, not the DOM: push a
                    // `mouse-button` event on press, the editor's mouse-button shortcut path.
                    other => {
                        if matches!(state, ElementState::Pressed) {
                            let code = match other {
                                MouseButton::Back => 8,
                                MouseButton::Forward => 9,
                                MouseButton::Other(n) => n as i64,
                                _ => return,
                            };
                            // Marshaled through the inbox like any emit; drained on this same tick.
                            self.state
                                .emit("mouse-button", serde_json::Value::from(code));
                        }
                        return;
                    }
                };
                // Track the held-button state before building the event so the down carries its own
                // flag and every subsequent move carries it too (a real drag, not a hover).
                let bit = match button {
                    MouseButton::Left => EVENTFLAG_LEFT_MOUSE_BUTTON,
                    MouseButton::Right => EVENTFLAG_RIGHT_MOUSE_BUTTON,
                    MouseButton::Middle => EVENTFLAG_MIDDLE_MOUSE_BUTTON,
                    _ => 0,
                };
                match state {
                    ElementState::Pressed => {
                        self.mouse_buttons |= bit;
                        // Bump the click count for a fast re-press of the same button near the last
                        // one; else restart at 1. The release reuses this count (matching CEF's
                        // down/up pairing), so `dblclick` fires on the second press.
                        let now = Instant::now();
                        let (lx, ly) = self.cursor;
                        let repeat = self.last_press.is_some_and(|(b, t, (px, py))| {
                            b == button
                                && now.duration_since(t) < Duration::from_millis(500)
                                && (px - lx).abs() <= 4
                                && (py - ly).abs() <= 4
                        });
                        self.click_count = if repeat {
                            (self.click_count + 1).min(3)
                        } else {
                            1
                        };
                        self.last_press = Some((button, now, self.cursor));
                    }
                    ElementState::Released => self.mouse_buttons &= !bit,
                }
                // A left-button release that ends an in-flight OSR drag drives the drop sequence
                // (drop → source-ended → system-drag-ended), not a normal click-up.
                if matches!(state, ElementState::Released)
                    && matches!(button, MouseButton::Left)
                    && self.drag.borrow().active
                {
                    if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                        let op = self.drag.borrow().current_op;
                        let event = self.mouse_event();
                        host.drag_target_drop(Some(&event));
                        let (lx, ly) = self.cursor_logical();
                        host.drag_source_ended_at(lx, ly, op);
                        host.drag_source_system_drag_ended();
                    }
                    self.drag.borrow_mut().active = false;
                    return;
                }
                let mouse_up = matches!(state, ElementState::Released) as i32;
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.send_mouse_click_event(
                        Some(&self.mouse_event()),
                        button_type,
                        mouse_up,
                        self.click_count,
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y) = match delta {
                    // Chromium wheel ticks are ~40px/line.
                    MouseScrollDelta::LineDelta(x, y) => ((x * 40.0) as i32, (y * 40.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => {
                        // Physical pixels → CEF's logical view units.
                        let scale = *self.scale.borrow();
                        ((p.x / scale) as i32, (p.y / scale) as i32)
                    }
                };
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.send_mouse_wheel_event(Some(&self.mouse_event()), delta_x, delta_y);
                }
            }
            WindowEvent::Focused(focused) => {
                if let Some(host) = self.browser.as_ref().and_then(|b| b.host()) {
                    host.set_focus(focused as i32);
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                let state = mods.state();
                let mut modifiers = 0;
                if state.shift_key() {
                    modifiers |= EVENTFLAG_SHIFT_DOWN;
                }
                if state.control_key() {
                    modifiers |= EVENTFLAG_CONTROL_DOWN;
                }
                if state.alt_key() {
                    modifiers |= EVENTFLAG_ALT_DOWN;
                }
                if state.super_key() {
                    modifiers |= EVENTFLAG_COMMAND_DOWN;
                }
                self.modifiers = modifiers;
            }
            // Synthetic key events winit fabricates on focus change carry no real intent; skip them.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.send_key(&event),
            // OS file drag-drop is not a winit `WindowEvent` on Wayland (winit's Wayland backend has no
            // `wl_data_device`); it's received on the compositor's connection and drained in the pump
            // loop (`emit_dnd`).
            _ => {}
        }
    }

    /// Raw relative pointer motion (delivered while the fly-cam has the cursor locked). Accumulate it;
    /// the pump loop streams it to the frontend as a `fly-look` event. Ignored when not locked (a
    /// normal mouse move rides `WindowEvent::CursorMoved` → CEF instead).
    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if self.pointer_locked
            && let DeviceEvent::MouseMotion { delta } = event
        {
            self.look_accum.0 += delta.0;
            self.look_accum.1 += delta.1;
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        // The staged exit. Every exit trigger (window close, IPC quit, measurement deadline)
        // raises `exit_requested`; teardown then runs HERE, while the loop is still running, and
        // the loop keeps pumping for a few grace iterations before `event_loop.exit()`. The grace
        // matters: closing the platform window dispatches synchronous events that winit queues
        // (the handler is in use) and replays on the next default-mode run-loop pass — the pass
        // must happen while the handler is still installed, or the replay lands in `cef::shutdown`
        // and is dropped with an error logged. The replayed events hit the `shell_window == None`
        // guards and are absorbed.
        if !self.state.exit_requested.load(Ordering::Relaxed) {
            return;
        }
        match self.teardown_grace {
            None => {
                self.teardown();
                self.teardown_grace = Some(2);
            }
            Some(0) => event_loop.exit(),
            Some(n) => self.teardown_grace = Some(n - 1),
        }
    }
}
