# Phase 4 — AppKit engine presenter

**Status:** COMPLETED

`backend/appkit/presenter.rs`, implementing `presenter::install(&Handles, scene_shm, asset_shm,
&Viewports)` over CALayers between the backdrop and the UI layer.

- Per-view reader thread reuses portable `viewport::{open_shm, stat_shm}` and the volatile seqlock
  header loop — structure ported from the preserved Tauri-era presenter
  (`git show 98be6959:editor/src-tauri/src/macos_viewport.rs`, `run_view`): magic/seq reads,
  `seq % slots` slot addressing, inode/size re-probe + remap, park/geometry change detection,
  torn-read acceptance (4-slot ring).
- Pixels: engine `Xrgb8888` slots copied byte-for-byte into per-view `'BGRA'` IOSurface pools on
  the reader thread; "frame ready" flag per view.
- Pacing: `objc2::define_class!` display-link target; `NSView.displayLink(target:selector:)` on the
  main run loop (`NSRunLoopCommonModes`); each tick swaps ready layers' `contents` +
  applies `ViewportShared` bounds/park (park → `setHidden:true`) in one `CATransaction`.
- `Viewports::refresh_mhz` published from display-link timestamps /
  `NSScreen.maximumFramesPerSecond` (replaces `wp_presentation`).
- Y-orientation: verify empirically — winit's flipped view may absorb the manual Y-flip the Tauri
  code applied against `contentView` bounds.

## Verify (macOS)

`start_engine` → live scene frames under the UI; pane stays glued through dock drags
(`set_viewport_bounds`); `set_viewport_parked` hides/reveals with backdrop showing through;
`viewport_refresh_hz` reports the real display rate (120 on ProMotion); teardown leaves no shm
segments (`shm_unlink`).
