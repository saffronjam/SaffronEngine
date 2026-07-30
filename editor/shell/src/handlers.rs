//! The windowless-OSR client handlers: paint, cursor and console, drag-and-drop, draggable
//! titlebar regions, and the context menu the editor replaces with its own.

use crate::backend::UiCompositor;
use crate::ipc;
use cef::wrapper::message_router::BrowserSideRouter;
use cef::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use winit::window::{CursorIcon, Window};

use crate::keymap::EVENTFLAG_LEFT_MOUSE_BUTTON;

/// Drives HTML5 drag-and-drop for the windowless OSR browser. CEF hands a drag it has started to the
/// client through `RenderHandler::start_dragging`, then relies on the client to feed pointer motion
/// back as `drag_target_drag_over` and the button release as `drag_target_drop` + `drag_source_ended_at`
/// — without that loop the DOM `dragover`/`drop` events never fire. Source and target are the same
/// browser (an asset tile dragged onto a viewport / hierarchy / picker drop zone), so the whole gesture
/// runs internally. Shared `Rc<RefCell<…>>` between the render handler (`start_dragging` /
/// `update_drag_cursor`) and the winit pointer handlers, all on the one main thread CEF's OSR callbacks
/// fire on.
#[derive(Default)]
pub(crate) struct DragState {
    pub(crate) active: bool,
    pub(crate) allowed_ops: DragOperationsMask,
    pub(crate) current_op: DragOperationsMask,
}

/// The OSR render handler: composites each `on_paint` frame onto the toplevel via the shared
/// `ToplevelCompositor` and counts it. The size cell is shared with the shell so a winit resize
/// propagates to CEF's `view_rect`. All shared via `Rc`/`RefCell` — CEF OSR callbacks fire on the
/// main thread that owns the winit loop, so single-threaded interior mutability is sound.
#[derive(Clone)]
pub(crate) struct ShellRenderHandler {
    pub(crate) paints: Arc<AtomicU64>,
    pub(crate) size: Rc<RefCell<(i32, i32)>>,
    /// The backend's OSR device-scale (`backend::window::osr_scale`): the view rect is physical ÷
    /// scale (CEF lays out in logical units and paints at physical resolution).
    pub(crate) scale: Rc<RefCell<f64>>,
    pub(crate) compositor: Rc<RefCell<Option<UiCompositor>>>,
    pub(crate) drag: Rc<RefCell<DragState>>,
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
pub(crate) type DragRegions = Rc<RefCell<Vec<(Rect, bool)>>>;

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
    pub(crate) struct ClientBuilder {
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
    pub(crate) fn build(
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
