//! OS→editor file drag-and-drop steps, backend-neutral. Each backend produces these from its own
//! drag source (Wayland: a `wl_data_device` on the compositor connection; AppKit: winit's
//! `HoveredFile`/`DroppedFile` events); the main loop drains them each tick and re-emits the
//! frontend's `drag-drop` event with the same payload on every platform.

use std::path::PathBuf;

/// One step of an OS→editor file drag. `Over` carries no paths (the file list is only read on
/// drop); `Drop` carries the resolved file paths. Positions are surface-local device pixels.
pub enum DndEvent {
    Over { x: i32, y: i32 },
    Leave,
    Drop { paths: Vec<PathBuf>, x: i32, y: i32 },
}
