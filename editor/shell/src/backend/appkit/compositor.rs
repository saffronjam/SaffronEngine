//! Composite CEF's OSR `on_paint` buffer onto CALayers under the winit `NSView`, alpha preserved.
//! The view is layer-backed; three z-ordered planes hang off its root layer: the opaque backdrop
//! (z −2, a stretched 1×1 `#0a0a0a` surface — the editor's transparent regions resolve against it,
//! not the desktop), the engine viewport layers (z −1, owned by `presenter`), and the UI layer
//! (z 0, non-opaque) whose contents rotate through a BGRA [`IoSurfacePool`] — the WindowServer
//! samples IOSurface contents directly on the GPU, so a UI frame costs one memcpy and a pointer
//! swap inside a `CATransaction` with implicit animations disabled.
//!
//! OS file drags arrive through winit (`HoveredFile`/`DroppedFile` from `NSDraggingDestination`),
//! folded into the portable [`DndEvent`] steps `pump_dnd` drains.

use super::iosurface::{IoSurfacePool, write_bgra};
use super::window::Handles;
use crate::ShellError;
use crate::dnd::DndEvent;
use cef::Rect;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_quartz_core::{CALayer, CATransaction};
use std::path::PathBuf;
use winit::event::WindowEvent;

/// The z-plane each layer occupies under the root; siblings order by `zPosition`, so insertion
/// order never matters.
pub(super) const Z_BACKDROP: f64 = -2.0;
pub(super) const Z_ENGINE: f64 = -1.0;
const Z_UI: f64 = 0.0;

/// View an IOSurface as the `id`-typed object `CALayer.contents` accepts. CF types are
/// Objective-C-object-compatible, and `setContents` retains its argument, so the layer keeps the
/// surface alive after the caller's reference drops.
///
/// # Safety
/// `surface` must be a live IOSurface.
pub(super) unsafe fn surface_as_contents(surface: &objc2_io_surface::IOSurfaceRef) -> &AnyObject {
    unsafe { &*(surface as *const objc2_io_surface::IOSurfaceRef).cast::<AnyObject>() }
}

/// Composites CPU `on_paint` frames onto the UI layer. Lives on the main thread (CEF OSR
/// callbacks fire there), so `paint` is called straight from `on_paint`.
pub struct UiCompositor {
    ns_view: usize,
    backdrop: Retained<CALayer>,
    ui: Retained<CALayer>,
    pool: IoSurfacePool,
    first_commit: bool,
    /// Last pointer position observed from winit, in device pixels — the position stamped onto
    /// drag steps (macOS drag events carry no position of their own through winit).
    cursor: (i32, i32),
    /// Paths dropped since the last `pump_dnd` drain (winit emits one `DroppedFile` per file).
    dropped: Vec<PathBuf>,
    /// Whether a file drag is hovering the window (drives `Over`/`Leave` steps).
    hovering: bool,
    /// Drag steps produced since the last drain.
    dnd_queue: Vec<DndEvent>,
}

impl UiCompositor {
    /// Build the layer planes on the winit view. Main thread (Tauri of AppKit: every layer-backed
    /// view mutation happens where the view lives).
    pub fn new(handles: &Handles) -> Result<Self, ShellError> {
        let view = handles.ns_view() as *const NSView;
        // SAFETY: `ns_view` is winit's live NSView (main thread, guaranteed by the caller — this
        // runs in `resumed` on the event-loop thread).
        let (backdrop, ui) = unsafe {
            let view = &*view;
            view.setWantsLayer(true);
            let root = view
                .layer()
                .ok_or_else(|| ShellError::Handle("view has no backing layer".into()))?;

            let backdrop = CALayer::new();
            backdrop.setZPosition(Z_BACKDROP);
            backdrop.setOpaque(true);
            // A stretched 1×1 surface paints the theme background (`oklch(0.145 0 0)` ≈ #0a0a0a);
            // the default `contentsGravity` ("resize") fills the layer.
            let mut px = IoSurfacePool::new();
            px.ensure(1, 1);
            if let Some(surface) = px.acquire() {
                write_bgra(&surface, &[10, 10, 10, 255], 1, 1);
                backdrop.setContents(Some(surface_as_contents(&surface)));
            }
            root.addSublayer(&backdrop);

            let ui = CALayer::new();
            ui.setZPosition(Z_UI);
            ui.setOpaque(false);
            root.addSublayer(&ui);
            (backdrop, ui)
        };
        Ok(Self {
            ns_view: handles.ns_view(),
            backdrop,
            ui,
            pool: IoSurfacePool::new(),
            first_commit: true,
            cursor: (0, 0),
            dropped: Vec::new(),
            hovering: false,
            dnd_queue: Vec::new(),
        })
    }

    /// Copy one CEF BGRA frame into a pool surface and swap it in as the UI layer's contents,
    /// resizing the layer planes to the view. `w`/`h` are device pixels; layer frames are points.
    pub fn paint(
        &mut self,
        bgra: &[u8],
        w: i32,
        h: i32,
        _dirty: Option<&[Rect]>,
    ) -> Result<(), ShellError> {
        if w <= 0 || h <= 0 {
            return Ok(());
        }
        let need = (w as usize) * (h as usize) * 4;
        if bgra.len() < need {
            return Ok(());
        }
        self.pool.ensure(w as usize, h as usize);
        let Some(surface) = self.pool.acquire() else {
            // Every pool surface is still held by the WindowServer; drop this frame.
            return Ok(());
        };
        write_bgra(&surface, bgra, w as usize, h as usize);

        // SAFETY: main thread (CEF's external pump). The view outlives the compositor; the
        // retained surface is handed to Core Animation as the layer contents.
        unsafe {
            let view = &*(self.ns_view as *const NSView);
            let bounds = view.bounds();
            let scale = if bounds.size.width > 0.0 {
                f64::from(w) / bounds.size.width
            } else {
                1.0
            };
            let frame = CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(bounds.size.width, bounds.size.height),
            );

            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.backdrop.setFrame(frame);
            self.ui.setFrame(frame);
            self.ui.setContentsScale(scale);
            self.ui.setContents(Some(surface_as_contents(&surface)));
            CATransaction::commit();
        }
        if self.first_commit {
            tracing::info!(target: "shell", "first UI commit to layer ({w}x{h})");
            self.first_commit = false;
        }
        Ok(())
    }

    /// Fold winit events into the drag state: cursor position for stamping, `HoveredFile` opens a
    /// hover, `DroppedFile` accumulates paths (one event per file; flushed as one `Drop` step by
    /// `pump_dnd`), `HoveredFileCancelled` ends it.
    pub fn observe_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as i32, position.y as i32);
                if self.hovering {
                    self.dnd_queue.push(DndEvent::Over {
                        x: self.cursor.0,
                        y: self.cursor.1,
                    });
                }
            }
            WindowEvent::HoveredFile(_) => {
                if !self.hovering {
                    self.hovering = true;
                    self.dnd_queue.push(DndEvent::Over {
                        x: self.cursor.0,
                        y: self.cursor.1,
                    });
                }
            }
            WindowEvent::DroppedFile(path) => {
                self.dropped.push(path.clone());
                self.hovering = false;
            }
            WindowEvent::HoveredFileCancelled if self.hovering => {
                self.hovering = false;
                self.dnd_queue.push(DndEvent::Leave);
            }
            _ => {}
        }
    }

    /// Drain the drag steps produced since the last tick. Dropped files accumulate across the
    /// per-file `DroppedFile` events and flush as one `Drop` step here.
    pub fn pump_dnd(&mut self) -> Vec<DndEvent> {
        if !self.dropped.is_empty() {
            let paths = std::mem::take(&mut self.dropped);
            self.dnd_queue.push(DndEvent::Drop {
                paths,
                x: self.cursor.0,
                y: self.cursor.1,
            });
        }
        std::mem::take(&mut self.dnd_queue)
    }
}
