# Phase 3 — AppKit UI compositor (IOSurface + CALayer)

**Status:** COMPLETED

- `backend/appkit/iosurface.rs`: `IoSurfacePool` — 3 × `'BGRA'` IOSurfaces per stream; acquire
  where `IOSurfaceIsInUse() == false`; `IOSurfaceLock` → copy → `IOSurfaceUnlock` (seed bump);
  never re-set the currently-displayed surface (CA compares contents pointers).
- `backend/appkit/compositor.rs` (`UiCompositor`): layer-backed winit NSView
  (`setWantsLayer(true)`); bottom→top: opaque backdrop `CALayer` (`#0a0a0a`) → [engine layers,
  Phase 4] → UI `CALayer` (`isOpaque = false`). `paint(bgra, w, h, dirty)` copies the full CEF
  buffer into a pool surface and swaps `contents` inside `CATransaction` with actions disabled —
  `on_paint` arrives on the main thread (external pump), no marshaling. Account for the flipped
  `WinitView` (top-left origin). `ScaleFactorChanged` → pool realloc + `contentsScale`.
- `RenderHandler::screen_info` feeds winit `scale_factor()` as `device_scale_factor` (else CEF
  paints at 1× on Retina).
- Frontend (macOS-only visual accommodation, `navigator.userAgent`, no bridge/ABI change): hide
  the custom min/max/close buttons; inset the tab strip clear of the traffic lights;
  `window_start_resize` = Ok no-op in the appkit backend (native edges resize).

## Verify (macOS)

React UI renders; transparent regions resolve to `#0a0a0a`, not desktop; reveal-after-first-paint
fires; live resize tracks without animation ghosting; traffic lights + titlebar drag work;
no per-frame allocation churn in Activity Monitor.
