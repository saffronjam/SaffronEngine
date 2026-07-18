//! Saffron editor shell — CEF/OSR shell skeleton.
//!
//! Phase 1 proved windowless CEF OSR sustains the monitor rate here under app-issued
//! external-begin-frame. This shell gives that OSR a home: one winit-owned Wayland **toplevel** and
//! the event loop that pumps CEF. CEF renders the UI windowless; the shell composites its `on_paint`
//! buffer onto the toplevel `wl_surface` (Phase 3), keeping the engine's Vulkan viewport frames as
//! `wl_subsurface`s below (Phase 6). The `RenderHandler` here still only counts paints — the
//! `wl_shm` upload lands in Phase 3.

mod appscheme;
mod async_rt;
mod backend;
mod commands;
mod connectors;
mod control;
mod dialog;
mod dnd;
mod engine;
mod geometry;
mod ipc;
mod ipc_render;
mod os;
mod scheme;
mod schemes;
mod settings;
mod state;
mod store_commands;
mod viewport;
mod window;

pub(crate) use os::open_url_in_browser;

use backend::UiCompositor;
use cef::wrapper::message_router::BrowserSideRouter;
use cef::{args::Args, *};
use dnd::DndEvent;
use state::{ResizeEdge, ShellRequest, ShellState, WindowAction};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use window::ShellWindow;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    platform::pump_events::{EventLoopExtPumpEvents, PumpStatus},
    window::{CursorIcon, ResizeDirection, Window, WindowAttributes, WindowId},
};

/// The shell's error type — modern-Rust `thiserror`, not `Result<T, String>` (the bar the old
/// a stringly-typed `Result<T, String>` command boundary would not hold; the wire-string contract lives
/// at the IPC boundary, wired in Phase 4).
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("window system not supported by the compiled shell backend")]
    UnsupportedWindowSystem,
    #[error("window handle: {0}")]
    Handle(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("engine: {0}")]
    Engine(String),
}

/// The CEF application handler: sets the Chromium command-line switches that bring CEF's GPU
/// subprocess up on this NVIDIA + Wayland box (Phase 1 §3; runtime-tunable via `SAFFRON_CEF_SWITCHES`).
#[derive(Clone)]
struct ShellApp;

wrap_app! {
    struct AppBuilder {
        app: ShellApp,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&cef::CefStringUtf16>,
            command_line: Option<&mut cef::CommandLine>,
        ) {
            let Some(cl) = command_line else {
                return;
            };
            cl.append_switch(Some(&"no-sandbox".into()));
            cl.append_switch(Some(&"noerrdialogs".into()));
            // Overlay scrollbars: draw scrollbars on top of content (auto-hiding, no reserved layout
            // gutter) instead of the classic space-reserving bar. The theme tint comes from
            // `scrollbar-color` in the frontend `styles.css`; this feature makes them overlay.
            cl.append_switch_with_value(
                Some(&"enable-features".into()),
                Some(&"OverlayScrollbar".into()),
            );
            // Windowless OSR reports no hover-capable pointer, so Blink evaluates `(hover: none)` /
            // `(pointer: coarse)` and disables every hover media query — and Tailwind v4 gates `hover:`
            // / `group-hover:` behind `@media (hover: hover)`, so all hover styling silently dies. The
            // editor is always a desktop mouse app; declare a fine, hovering pointer to Blink.
            // (`HoverType::kHover = 2`, `PointerType::kFine = 4` — a single comma-joined switch value,
            // so it must not go through the comma-split `SAFFRON_CEF_SWITCHES` path below.)
            cl.append_switch_with_value(
                Some(&"blink-settings".into()),
                Some(
                    &"primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4"
                        .into(),
                ),
            );
            // The Ozone/GL set that brings CEF's GPU subprocess up on this NVIDIA + Wayland box is
            // found empirically (Phase 1 §3): passed at runtime (comma-separated `k=v` / bare flag),
            // not baked in. Phase 1 locked `ozone-platform=x11` on the real NVIDIA card.
            if let Ok(extra) = std::env::var("SAFFRON_CEF_SWITCHES") {
                for sw in extra.split(',').filter(|s| !s.is_empty()) {
                    match sw.split_once('=') {
                        Some((k, v)) => cl.append_switch_with_value(Some(&k.into()), Some(&v.into())),
                        None => cl.append_switch(Some(&sw.into())),
                    }
                }
            }
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            // Invoked in the render subprocess: registers `cefQuery` via the renderer-side router.
            Some(ipc_render::render_process_handler())
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            // Custom schemes must be registered identically in every process; the handlers are
            // registered in the browser process after `initialize`.
            if let Some(registrar) = registrar {
                schemes::register(registrar);
            }
        }
    }
}

impl AppBuilder {
    fn build(app: ShellApp) -> cef::App {
        Self::new(app)
    }
}

/// Drives HTML5 drag-and-drop for the windowless OSR browser. CEF hands a drag it has started to the
/// client through `RenderHandler::start_dragging`, then relies on the client to feed pointer motion
/// back as `drag_target_drag_over` and the button release as `drag_target_drop` + `drag_source_ended_at`
/// — without that loop the DOM `dragover`/`drop` events never fire. Source and target are the same
/// browser (an asset tile dragged onto a viewport / hierarchy / picker drop zone), so the whole gesture
/// runs internally. Shared `Rc<RefCell<…>>` between the render handler (`start_dragging` /
/// `update_drag_cursor`) and the winit pointer handlers, all on the one main thread CEF's OSR callbacks
/// fire on.
#[derive(Default)]
struct DragState {
    active: bool,
    allowed_ops: DragOperationsMask,
    current_op: DragOperationsMask,
}

/// The OSR render handler: composites each `on_paint` frame onto the toplevel via the shared
/// `ToplevelCompositor` and counts it. The size cell is shared with the shell so a winit resize
/// propagates to CEF's `view_rect`. All shared via `Rc`/`RefCell` — CEF OSR callbacks fire on the
/// main thread that owns the winit loop, so single-threaded interior mutability is sound.
#[derive(Clone)]
struct ShellRenderHandler {
    paints: Arc<AtomicU64>,
    size: Rc<RefCell<(i32, i32)>>,
    /// The backend's OSR device-scale (`backend::window::osr_scale`): the view rect is physical ÷
    /// scale (CEF lays out in logical units and paints at physical resolution).
    scale: Rc<RefCell<f64>>,
    compositor: Rc<RefCell<Option<UiCompositor>>>,
    drag: Rc<RefCell<DragState>>,
}

/// The OSR display handler: CEF reports every CSS cursor change here (windowless OSR has no window of
/// its own), so the shell applies it to the winit toplevel — otherwise the pointer stays the default
/// arrow everywhere (over buttons, text fields, dock splitters, and the window's own resize edges).
/// Holds the window `Weak`: the shell is the window's one owner, so teardown (`Shell::exiting`)
/// fully releases the toplevel while the event loop still runs — a CEF-side strong ref would defer
/// the platform window close into `cef::shutdown`, outside winit's handler.
#[derive(Clone)]
struct ShellDisplayHandler {
    window: std::sync::Weak<Window>,
}

/// The platform cursor-handle type in CEF's `on_cursor_change` (an `NSCursor*` on macOS, an X11
/// cursor id elsewhere). Unused — the shell maps the portable `CursorType` to winit icons instead.
#[cfg(target_os = "macos")]
type CefCursorHandle = *mut u8;
#[cfg(not(target_os = "macos"))]
type CefCursorHandle = ::std::os::raw::c_ulong;

wrap_render_handler! {
    struct RenderHandlerBuilder {
        handler: ShellRenderHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (w, h) = *self.handler.size.borrow();
                let scale = *self.handler.scale.borrow();
                rect.x = 0;
                rect.y = 0;
                if w > 0 && h > 0 {
                    // Logical units: CEF multiplies by `screen_info`'s device_scale_factor to
                    // pick the physical paint resolution.
                    rect.width = ((f64::from(w) / scale).round() as i32).max(1);
                    rect.height = ((f64::from(h) / scale).round() as i32).max(1);
                }
            }
        }

        /// Report the backend's OSR device-scale so CEF lays the page out in logical units and
        /// paints at physical resolution (2× on Retina). Without this, scale defaults to 1 and
        /// the UI renders at the wrong visual size on a scaled display.
        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(info) = screen_info else {
                return 0;
            };
            let (w, h) = *self.handler.size.borrow();
            let scale = *self.handler.scale.borrow();
            let rect = Rect {
                x: 0,
                y: 0,
                width: ((f64::from(w.max(1)) / scale).round() as i32).max(1),
                height: ((f64::from(h.max(1)) / scale).round() as i32).max(1),
            };
            info.device_scale_factor = scale as f32;
            info.depth = 24;
            info.depth_per_component = 8;
            info.is_monochrome = 0;
            info.rect = rect.clone();
            info.available_rect = rect;
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if !buffer.is_null()
                && width > 0
                && height > 0
                && let Some(compositor) = self.handler.compositor.borrow_mut().as_mut()
            {
                let len = (width as usize) * (height as usize) * 4;
                let frame = unsafe { std::slice::from_raw_parts(buffer, len) };
                let _ = compositor.paint(frame, width, height, dirty_rects);
            }
            self.handler.paints.fetch_add(1, Ordering::Relaxed);
        }

        /// CEF started a drag in the page content. Accept it (return 1) and open the internal
        /// drop loop: enter the drag at its start point, then the winit pointer handlers feed
        /// `drag_target_drag_over` on motion and `drag_target_drop` on release.
        fn start_dragging(
            &self,
            browser: Option<&mut Browser>,
            drag_data: Option<&mut DragData>,
            allowed_ops: DragOperationsMask,
            x: ::std::os::raw::c_int,
            y: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(host) = browser.and_then(|b| b.host()) else {
                return 0;
            };
            let event = MouseEvent {
                x,
                y,
                modifiers: EVENTFLAG_LEFT_MOUSE_BUTTON,
            };
            host.drag_target_drag_enter(drag_data, Some(&event), allowed_ops);
            let mut drag = self.handler.drag.borrow_mut();
            drag.active = true;
            drag.allowed_ops = allowed_ops;
            drag.current_op = DragOperationsMask::default();
            1
        }

        /// CEF reports the drop operation valid under the cursor as the drag moves; remember it so the
        /// release can tell the source what happened via `drag_source_ended_at`.
        fn update_drag_cursor(&self, _browser: Option<&mut Browser>, operation: DragOperationsMask) {
            self.handler.drag.borrow_mut().current_op = operation;
        }
    }
}

wrap_display_handler! {
    struct DisplayHandlerBuilder {
        handler: ShellDisplayHandler,
    }

    impl DisplayHandler {
        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: CefCursorHandle,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(window) = self.handler.window.upgrade() else {
                return 1;
            };
            match cursor_icon_for(type_) {
                Some(icon) => {
                    window.set_cursor_visible(true);
                    window.set_cursor(icon);
                }
                // `CT_NONE` — the page asked for no cursor (e.g. pointer-lock fly-cam).
                None => window.set_cursor_visible(false),
            }
            1
        }

        /// Route the React UI's `console.*` into the unified log so it shows in the `just run`
        /// terminal; otherwise it lands only in the CEF renderer subprocess and is invisible. Return
        /// 1 to suppress CEF's own default console output (we've forwarded it). `source` is the
        /// script URL, `line` its line.
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let message = message.map(ToString::to_string).unwrap_or_default();
            let source = source.map(ToString::to_string).unwrap_or_default();
            if level == LogSeverity::ERROR || level == LogSeverity::FATAL {
                tracing::error!(target: "webview", "{message}  ({source}:{line})");
            } else if level == LogSeverity::WARNING {
                tracing::warn!(target: "webview", "{message}  ({source}:{line})");
            } else if level == LogSeverity::VERBOSE {
                tracing::debug!(target: "webview", "{message}  ({source}:{line})");
            } else {
                tracing::info!(target: "webview", "{message}  ({source}:{line})");
            }
            1
        }
    }
}

impl DisplayHandlerBuilder {
    fn build(handler: ShellDisplayHandler) -> DisplayHandler {
        Self::new(handler)
    }
}

/// Map a CEF OSR cursor type to the winit `CursorIcon` to show. `None` means hide the cursor
/// (`CT_NONE`); an unmapped or custom type falls back to the arrow. Note CEF's `CT_POINTER` is the
/// plain arrow and `CT_HAND` is the link/clickable pointer — the reverse of the web `cursor` names.
fn cursor_icon_for(cursor: CursorType) -> Option<CursorIcon> {
    let icon = match cursor {
        CursorType::HAND => CursorIcon::Pointer,
        CursorType::IBEAM => CursorIcon::Text,
        CursorType::VERTICALTEXT => CursorIcon::VerticalText,
        CursorType::CROSS => CursorIcon::Crosshair,
        CursorType::WAIT => CursorIcon::Wait,
        CursorType::PROGRESS => CursorIcon::Progress,
        CursorType::HELP => CursorIcon::Help,
        CursorType::CELL => CursorIcon::Cell,
        CursorType::CONTEXTMENU => CursorIcon::ContextMenu,
        CursorType::ALIAS => CursorIcon::Alias,
        CursorType::COPY | CursorType::DND_COPY => CursorIcon::Copy,
        CursorType::DND_LINK => CursorIcon::Alias,
        CursorType::NODROP => CursorIcon::NoDrop,
        CursorType::NOTALLOWED => CursorIcon::NotAllowed,
        CursorType::MOVE | CursorType::DND_MOVE => CursorIcon::Move,
        CursorType::GRAB => CursorIcon::Grab,
        CursorType::GRABBING => CursorIcon::Grabbing,
        CursorType::ZOOMIN => CursorIcon::ZoomIn,
        CursorType::ZOOMOUT => CursorIcon::ZoomOut,
        CursorType::EASTRESIZE => CursorIcon::EResize,
        CursorType::WESTRESIZE => CursorIcon::WResize,
        CursorType::NORTHRESIZE => CursorIcon::NResize,
        CursorType::SOUTHRESIZE => CursorIcon::SResize,
        CursorType::NORTHEASTRESIZE => CursorIcon::NeResize,
        CursorType::NORTHWESTRESIZE => CursorIcon::NwResize,
        CursorType::SOUTHEASTRESIZE => CursorIcon::SeResize,
        CursorType::SOUTHWESTRESIZE => CursorIcon::SwResize,
        CursorType::EASTWESTRESIZE => CursorIcon::EwResize,
        CursorType::NORTHSOUTHRESIZE => CursorIcon::NsResize,
        CursorType::NORTHEASTSOUTHWESTRESIZE => CursorIcon::NeswResize,
        CursorType::NORTHWESTSOUTHEASTRESIZE => CursorIcon::NwseResize,
        CursorType::COLUMNRESIZE => CursorIcon::ColResize,
        CursorType::ROWRESIZE => CursorIcon::RowResize,
        CursorType::MIDDLEPANNING
        | CursorType::EASTPANNING
        | CursorType::NORTHPANNING
        | CursorType::NORTHEASTPANNING
        | CursorType::NORTHWESTPANNING
        | CursorType::SOUTHPANNING
        | CursorType::SOUTHEASTPANNING
        | CursorType::SOUTHWESTPANNING
        | CursorType::WESTPANNING
        | CursorType::MIDDLE_PANNING_VERTICAL
        | CursorType::MIDDLE_PANNING_HORIZONTAL => CursorIcon::AllScroll,
        CursorType::NONE => return None,
        // `CT_POINTER` (the plain arrow), `CT_CUSTOM`, `CT_DND_NONE`, and any future type.
        _ => CursorIcon::Default,
    };
    Some(icon)
}

impl RenderHandlerBuilder {
    fn build(handler: ShellRenderHandler) -> RenderHandler {
        Self::new(handler)
    }
}

wrap_context_menu_handler! {
    struct ShellContextMenuHandler;

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            // The editor supplies its own React context menus; CEF's default OSR menu is never
            // wanted, and in windowless mode it has no window handle so it only logs an error
            // ("Window handle is required for default OSR context menu"). Clearing the model leaves
            // an empty menu, which CEF skips silently.
            if let Some(model) = model {
                model.clear();
            }
        }
    }
}

/// The page's `-webkit-app-region` rectangles, in CEF's logical view units, in paint order. CEF
/// reports BOTH the draggable rects and the `no-drag` holes (tabs, window controls) — the host
/// honors both: a point is draggable when the topmost (last-painted) region containing it is
/// draggable. Shared single-threaded between the `DragHandler` (CEF updates it on layout changes)
/// and the `Shell`'s mouse handler (hit-tests a left press).
type DragRegions = Rc<RefCell<Vec<(Rect, bool)>>>;

/// The OSR drag handler: windowless CEF has no native window, so the host owns titlebar dragging.
/// CEF reports the page's `-webkit-app-region` rectangles here; the shell starts a native window
/// drag when a left press lands in one (see the `MouseInput` handler).
#[derive(Clone)]
struct ShellDragHandler {
    regions: DragRegions,
}

wrap_drag_handler! {
    struct DragHandlerBuilder {
        handler: ShellDragHandler,
    }

    impl DragHandler {
        fn on_draggable_regions_changed(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            let mut store = self.handler.regions.borrow_mut();
            store.clear();
            if let Some(regions) = regions {
                store.extend(
                    regions
                        .iter()
                        .map(|region| (region.bounds.clone(), region.draggable != 0)),
                );
            }
        }
    }
}

impl DragHandlerBuilder {
    fn build(handler: ShellDragHandler) -> DragHandler {
        Self::new(handler)
    }
}

wrap_client! {
    struct ClientBuilder {
        render_handler: RenderHandler,
        router: Arc<BrowserSideRouter>,
        life_span: LifeSpanHandler,
        context_menu: ContextMenuHandler,
        display: DisplayHandler,
        drag: DragHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<cef::RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn display_handler(&self) -> Option<cef::DisplayHandler> {
            Some(self.display.clone())
        }

        fn context_menu_handler(&self) -> Option<cef::ContextMenuHandler> {
            Some(self.context_menu.clone())
        }

        fn drag_handler(&self) -> Option<cef::DragHandler> {
            Some(self.drag.clone())
        }

        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            ipc::browser_on_process_message(&self.router, browser, frame, source_process, message)
        }
    }
}

impl ClientBuilder {
    fn build(
        handler: ShellRenderHandler,
        router: Arc<BrowserSideRouter>,
        window: Arc<Window>,
        drag_regions: DragRegions,
    ) -> Client {
        let life_span = ipc::life_span_handler(Arc::clone(&router));
        Self::new(
            RenderHandlerBuilder::build(handler),
            router,
            life_span,
            ShellContextMenuHandler::new(),
            DisplayHandlerBuilder::build(ShellDisplayHandler {
                window: Arc::downgrade(&window),
            }),
            DragHandlerBuilder::build(ShellDragHandler {
                regions: drag_regions,
            }),
        )
    }
}

/// The host shell: owns the one winit toplevel (via `ShellWindow`) and the windowless CEF browser
/// bound to it. CEF is created lazily in `resumed`, once the toplevel (and thus its size) exists.
struct Shell {
    shell_window: Option<ShellWindow>,
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
    compositor: Rc<RefCell<Option<UiCompositor>>>,
    paints: Arc<AtomicU64>,
    url: String,
    /// Shared engine/window/IPC state; the main loop drains its request inbox and reads/writes its
    /// window tracker. The IPC router holds its own clone of the same `Arc`.
    state: Arc<ShellState>,
    revealed: bool,
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
    refresh_hz: f64,
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
    pointer_locked: bool,
    /// Raw relative-motion delta accumulated since the last `fly-look` emit (only while `pointer_locked`).
    look_accum: (f64, f64),
    /// The staged-exit state (see `new_events`): `None` while running; `Some(n)` counts the grace
    /// iterations between teardown and `event_loop.exit()` that let the platform replay the
    /// window close's queued events into the still-installed handler.
    teardown_grace: Option<u8>,
}

/// CEF `cef_event_flags_t` modifier bits carried in `KeyEvent`/`MouseEvent.modifiers`.
const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;
const EVENTFLAG_LEFT_MOUSE_BUTTON: u32 = 1 << 4;
const EVENTFLAG_MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
const EVENTFLAG_RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7;

impl Shell {
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
    fn retarget_refresh(&mut self) -> Option<f64> {
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
    fn apply_window_action(&mut self, action: WindowAction) {
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
    fn emit_dnd(&self, ev: &DndEvent) {
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
    fn emit_to_js(&self, event: &str, payload: &str) {
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

/// Map a winit physical `KeyCode` to a Windows virtual-key code (Chromium `VKEY_*` == `VK_*`), which
/// is what CEF's `KeyEvent.windows_key_code` expects. Text entry rides on the CHAR events; this table
/// is what makes editing/navigation keys, function keys, and shortcuts work. Unmapped keys return 0.
fn vk_from_keycode(code: KeyCode) -> i32 {
    match code {
        KeyCode::Backspace => 0x08,
        KeyCode::Tab => 0x09,
        KeyCode::Enter | KeyCode::NumpadEnter => 0x0D,
        KeyCode::Escape => 0x1B,
        KeyCode::Space => 0x20,
        KeyCode::PageUp => 0x21,
        KeyCode::PageDown => 0x22,
        KeyCode::End => 0x23,
        KeyCode::Home => 0x24,
        KeyCode::ArrowLeft => 0x25,
        KeyCode::ArrowUp => 0x26,
        KeyCode::ArrowRight => 0x27,
        KeyCode::ArrowDown => 0x28,
        KeyCode::Insert => 0x2D,
        KeyCode::Delete => 0x2E,
        KeyCode::CapsLock => 0x14,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => 0x10,
        KeyCode::ControlLeft | KeyCode::ControlRight => 0x11,
        KeyCode::AltLeft | KeyCode::AltRight => 0x12,
        KeyCode::SuperLeft => 0x5B,
        KeyCode::SuperRight => 0x5C,
        KeyCode::ContextMenu => 0x5D,
        KeyCode::Digit0 => 0x30,
        KeyCode::Digit1 => 0x31,
        KeyCode::Digit2 => 0x32,
        KeyCode::Digit3 => 0x33,
        KeyCode::Digit4 => 0x34,
        KeyCode::Digit5 => 0x35,
        KeyCode::Digit6 => 0x36,
        KeyCode::Digit7 => 0x37,
        KeyCode::Digit8 => 0x38,
        KeyCode::Digit9 => 0x39,
        KeyCode::KeyA => 0x41,
        KeyCode::KeyB => 0x42,
        KeyCode::KeyC => 0x43,
        KeyCode::KeyD => 0x44,
        KeyCode::KeyE => 0x45,
        KeyCode::KeyF => 0x46,
        KeyCode::KeyG => 0x47,
        KeyCode::KeyH => 0x48,
        KeyCode::KeyI => 0x49,
        KeyCode::KeyJ => 0x4A,
        KeyCode::KeyK => 0x4B,
        KeyCode::KeyL => 0x4C,
        KeyCode::KeyM => 0x4D,
        KeyCode::KeyN => 0x4E,
        KeyCode::KeyO => 0x4F,
        KeyCode::KeyP => 0x50,
        KeyCode::KeyQ => 0x51,
        KeyCode::KeyR => 0x52,
        KeyCode::KeyS => 0x53,
        KeyCode::KeyT => 0x54,
        KeyCode::KeyU => 0x55,
        KeyCode::KeyV => 0x56,
        KeyCode::KeyW => 0x57,
        KeyCode::KeyX => 0x58,
        KeyCode::KeyY => 0x59,
        KeyCode::KeyZ => 0x5A,
        KeyCode::Numpad0 => 0x60,
        KeyCode::Numpad1 => 0x61,
        KeyCode::Numpad2 => 0x62,
        KeyCode::Numpad3 => 0x63,
        KeyCode::Numpad4 => 0x64,
        KeyCode::Numpad5 => 0x65,
        KeyCode::Numpad6 => 0x66,
        KeyCode::Numpad7 => 0x67,
        KeyCode::Numpad8 => 0x68,
        KeyCode::Numpad9 => 0x69,
        KeyCode::NumpadMultiply => 0x6A,
        KeyCode::NumpadAdd => 0x6B,
        KeyCode::NumpadSubtract => 0x6D,
        KeyCode::NumpadDecimal => 0x6E,
        KeyCode::NumpadDivide => 0x6F,
        KeyCode::F1 => 0x70,
        KeyCode::F2 => 0x71,
        KeyCode::F3 => 0x72,
        KeyCode::F4 => 0x73,
        KeyCode::F5 => 0x74,
        KeyCode::F6 => 0x75,
        KeyCode::F7 => 0x76,
        KeyCode::F8 => 0x77,
        KeyCode::F9 => 0x78,
        KeyCode::F10 => 0x79,
        KeyCode::F11 => 0x7A,
        KeyCode::F12 => 0x7B,
        KeyCode::Semicolon => 0xBA,
        KeyCode::Equal => 0xBB,
        KeyCode::Comma => 0xBC,
        KeyCode::Minus => 0xBD,
        KeyCode::Period => 0xBE,
        KeyCode::Slash => 0xBF,
        KeyCode::Backquote => 0xC0,
        KeyCode::BracketLeft => 0xDB,
        KeyCode::Backslash => 0xDC,
        KeyCode::BracketRight => 0xDD,
        KeyCode::Quote => 0xDE,
        _ => 0,
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

fn main() -> std::process::ExitCode {
    // Whatever must happen before the first CEF call — the AppKit backend loads the CEF framework
    // here; a platform that links libcef directly is a no-op.
    if let Err(err) = backend::bootstrap::load_cef() {
        eprintln!("fatal: load CEF: {err}");
        return std::process::ExitCode::from(1);
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let cmd = args.as_cmd_line().unwrap();
    let type_switch = CefString::from("type");
    let is_browser_process = cmd.has_switch(Some(&type_switch)) != 1;

    let mut app = AppBuilder::build(ShellApp);
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );

    if is_browser_process {
        assert_eq!(ret, -1, "browser process execute_process should return -1");
    } else {
        return std::process::ExitCode::from(0);
    }

    // Install the shared `saffron-log` subscriber so the shell's own lines carry the same
    // timestamp · level · subsystem format as the spawned host's — one uniform stream in the combined
    // stdout. Browser process only; the CEF helper subprocesses returned above.
    saffron_log::init_logging();

    // CEF's profile/cache root: a per-repo dir so it never shares the default profile (which forces
    // a cross-process singleton — the "profile in use by another Chromium process" crash — and is
    // what the `root_cache_path` warning is about). Created before `initialize`.
    let cache_dir = geometry::app_data_dir().join("cef-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let settings = Settings {
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        root_cache_path: CefString::from(cache_dir.to_string_lossy().as_ref()),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        ),
        1,
        "cef initialize failed"
    );

    // On a platform whose run loop is shared with winit, CEF is pumped from run-loop context (see
    // the backend contract); where the shell's loop pumps directly this is a no-op.
    backend::bootstrap::install_cef_pump();

    // The engine-facing state container (socket, trace server); command handlers receive it in
    // Phase 4. Constructing it starts the profiler-trace loopback server.
    let state = Arc::new(ShellState::default());
    tracing::info!(
        target: "shell",
        "state ready; socket={} trace_port={:?}",
        state.socket_path, state.trace_port
    );

    // Serve `saffron-img://` thumbnails from the shared connector cache (browser process, post-init).
    register_scheme_handler_factory(
        Some(&schemes::IMG_SCHEME_NAME.into()),
        None,
        Some(&mut scheme::factory(state.connectors.cache())),
    );

    // Serve the bundled React UI over `saffron-app://` in a packaged build. `SAFFRON_UI_DIR` is set by
    // the AppImage's AppRun; it is empty in dev, where the UI loads from Vite via `SAFFRON_DEV_URL`.
    let ui_dir = std::env::var_os("SAFFRON_UI_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    register_scheme_handler_factory(
        Some(&schemes::APP_SCHEME_NAME.into()),
        None,
        Some(&mut appscheme::factory(ui_dir.clone())),
    );

    // The browser-side IPC router (created after `initialize`, on the UI thread) that carries
    // `cefQuery` requests to the control passthrough.
    let router = ipc::browser_router(Arc::clone(&state));

    // A fixed pacing override (headless measurement); otherwise the shell paces to the monitor's
    // refresh once the toplevel maps. Provisional 60 Hz until then (raised, not lowered, on detect).
    let target_hz_override: Option<f64> = std::env::var("SAFFRON_SHELL_TARGET_HZ")
        .ok()
        .and_then(|s| s.parse().ok());

    let paints = Arc::new(AtomicU64::new(0));
    let mut shell = Shell {
        shell_window: None,
        browser: None,
        router,
        size: Rc::new(RefCell::new((1600, 900))),
        scale: Rc::new(RefCell::new(1.0)),
        compositor: Rc::new(RefCell::new(None)),
        paints: Arc::clone(&paints),
        // The React UI URL. The dev loop sets `SAFFRON_DEV_URL` (Vite); a packaged build has
        // `SAFFRON_UI_DIR` set and loads the bundle over the `saffron-app://` scheme. With neither the
        // shell is misconfigured — show a plain theme-colored notice rather than a blank window.
        url: std::env::var("SAFFRON_DEV_URL").unwrap_or_else(|_| {
            if ui_dir.as_os_str().is_empty() {
                concat!(
                    "data:text/html,",
                    "<html><body style='margin:0;background:%230a0a0a;color:%23888;",
                    "font:14px sans-serif;display:grid;place-items:center;height:100vh'>",
                    "no UI source: run via `just run` or a packaged build</body></html>"
                )
                .to_string()
            } else {
                appscheme::INDEX_URL.to_string()
            }
        }),
        state: Arc::clone(&state),
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
    };

    let mut event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);

    // 0 = run until the window is closed; a positive value bounds a headless measurement run.
    let measure_secs: u64 = std::env::var("SAFFRON_SHELL_MEASURE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut frame_dt = Duration::from_secs_f64(1.0 / shell.refresh_hz);
    let mut last_refresh_check = Instant::now();

    tracing::info!(target: "shell", "event loop up; CEF self-driven, pacing to monitor refresh");

    // Auto-start the engine at launch (at launch): spawn + watchdog phase events.
    // Skipped for the shell-only OSR smoke (`SAFFRON_SHELL_NO_ENGINE`), which never needs the host.
    if std::env::var_os("SAFFRON_SHELL_NO_ENGINE").is_none()
        && let Err(err) = engine::auto_start(&state)
    {
        tracing::error!(target: "shell", "engine auto-start failed: {err}");
    }

    let start = Instant::now();
    let mut next_frame = Instant::now();
    let mut last_report = Instant::now();
    let mut total: u64 = 0;
    let mut reports: u64 = 0;

    let code = loop {
        // Where the loop pumps CEF itself (Wayland), the pump returns immediately and the sleep
        // below paces the iteration. Where the run loop is shared (AppKit), the loop blocks HERE
        // for up to a frame: winit's handler is only installed inside `pump_app_events`, so all
        // run-loop dispatch — CEF's timer-driven pump, the display link, input NSEvents — must
        // happen within this window or the events are dropped.
        let pump_timeout = if backend::bootstrap::PUMPS_IN_LOOP {
            Duration::ZERO
        } else {
            frame_dt
        };
        if let PumpStatus::Exit(code) = event_loop.pump_app_events(Some(pump_timeout), &mut shell) {
            break code as u8;
        }

        if backend::bootstrap::PUMPS_IN_LOOP {
            do_message_loop_work();
        }

        // Apply requests posted by IPC worker threads (window controls + JS event emits) on this,
        // the thread that owns the winit window and CEF browser.
        let requests: Vec<ShellRequest> = state
            .inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect();
        for request in requests {
            match request {
                ShellRequest::Window(action) => shell.apply_window_action(action),
                ShellRequest::Emit { event, payload } => shell.emit_to_js(&event, &payload),
            }
        }

        // Drain the compositor's OS file drag-drop steps (received on the backend's own drag
        // source) and re-emit them to the frontend. Owned `Vec` so the compositor borrow ends
        // before `emit_dnd`.
        let dnd = shell
            .compositor
            .borrow_mut()
            .as_mut()
            .map(UiCompositor::pump_dnd)
            .unwrap_or_default();
        for ev in &dnd {
            shell.emit_dnd(ev);
        }

        // Stream accumulated fly-cam look motion (from locked-pointer `DeviceEvent::MouseMotion`) to the
        // frontend as one `fly-look` per iteration. Frame-paced, and only while the pointer is locked.
        if shell.pointer_locked && shell.look_accum != (0.0, 0.0) {
            let (dx, dy) = shell.look_accum;
            shell.look_accum = (0.0, 0.0);
            shell.emit_to_js("fly-look", &format!("{{\"dx\":{dx},\"dy\":{dy}}}"));
        }

        // Pace to the monitor's refresh: pick it up once the surface maps, and follow a move to a
        // different-refresh output. Throttled — `current_monitor` locks Wayland surface state.
        if last_refresh_check.elapsed() >= Duration::from_millis(200) {
            last_refresh_check = Instant::now();
            if let Some(hz) = shell.retarget_refresh() {
                frame_dt = Duration::from_secs_f64(1.0 / hz);
            }
        }

        // Reveal only once the UI has actually painted, so the toplevel maps with real pixels
        // rather than flashing empty (the "reveal after first React frame" contract).
        if !shell.revealed && paints.load(Ordering::Relaxed) > 0 {
            if let Some(shell_window) = shell.shell_window.as_ref() {
                shell_window.reveal();
            }
            shell.revealed = true;
            tracing::debug!(target: "shell", "revealed after first paint");
        }

        // The per-second paint-rate report is a measurement aid, not routine output — only accumulate
        // + print it during a bounded measurement run (`SAFFRON_SHELL_MEASURE_SECS`).
        if measure_secs > 0 && last_report.elapsed() >= Duration::from_secs(1) {
            let n = paints.swap(0, Ordering::Relaxed);
            total += n;
            reports += 1;
            tracing::info!(target: "shell", "on_paint: {n} paints/s");
            last_report = Instant::now();
        }
        if measure_secs > 0 && start.elapsed() >= Duration::from_secs(measure_secs) {
            // Funnel through the one exit path: the flag is picked up by `new_events` inside the
            // next pump, which tears down and exits the loop.
            state.exit_requested.store(true, Ordering::Relaxed);
        }

        // Sleep-pace only when the pump returns immediately; a blocking pump already waited.
        if backend::bootstrap::PUMPS_IN_LOOP {
            next_frame += frame_dt;
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
    };

    if let Some(avg) = total.checked_div(reports) {
        tracing::info!(
            target: "shell",
            "SUSTAINED on_paint average: {avg} paints/s over {reports}s (target {} Hz)",
            shell.refresh_hz
        );
    }

    // The loop-entangled teardown already ran inside the loop (the staged exit in `new_events`).
    // Only CEF finalization remains, which must happen OUTSIDE any handler callback (CEF aborts on
    // a nested-context shutdown): the browser ref is already released, and the CEF callbacks that
    // can fire during the drain no-op against the emptied state. The drain also bounces stray
    // platform events off winit's global hooks (its application subclass and delegates), which
    // winit logs-and-ignores since its loop is finished — that target carries no signal past this
    // boundary. Nothing may pump the winit loop again after this point.
    saffron_log::silence_target("winit");
    cef::shutdown();
    drop(shell);
    drop(event_loop);
    std::process::ExitCode::from(code)
}
