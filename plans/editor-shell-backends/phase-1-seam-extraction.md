# Phase 1 — Seam extraction: Wayland → `backend/wayland`, zero Linux behavior change

**Status:** COMPLETED — Linux gate pending user validation on a Linux box.

Relocate every platform-specific piece of `editor/shell/src` into `backend/wayland/`, split the
portable halves out, and gate the wayland crates to Linux — after this phase Linux compiles the
identical logic from new paths, and macOS `cargo check` fails only on the (not yet written)
`backend/appkit` module.

## Moves

- `src/backend/mod.rs` (new): cfg selection + contract doc + `compile_error!` arm.
- `compositor.rs` → `backend/wayland/compositor.rs` verbatim (wl_shm/memfd UI upload, backdrop
  subsurface + opaque region, `wl_data_device` DnD, `apply_damage`). `DndEvent` moves out to
  portable `src/dnd.rs`; `parse_uri_list`/`read_uri_list` stay in the wayland backend.
- `presenter.rs` splits: portable `src/viewport.rs` gets `View`, `ViewportShared`, `Viewports`,
  `pack_pair`/`unpack_pair`, `SHM_MAGIC`, `SHM_HEADER_BYTES`, `open_shm`, `stat_shm`;
  `backend/wayland/presenter.rs` keeps `install`/`run`/`step_view` (subsurfaces, frame-callback
  pacing, `wp_presentation`).
- `window.rs`: `Handles` (wl_display/wl_surface recovery, `NotWayland` →
  `ShellError::UnsupportedWindowSystem`) + platform window ops (`drag_resize`, pointer lock with
  `Confined` fallback) → `backend/wayland/window.rs`; `src/window.rs` becomes the portable
  `ShellWindow` over `backend::Handles`.
- XKB native-keycode table (evdev+8) from `main.rs` → `backend/wayland/keys.rs`
  (`native_from_keycode`); the Windows-VK map stays portable in `main.rs`.
- `backend/wayland/bootstrap.rs`: `load_cef()` no-op + Linux CEF switches seam.
- `backend/wayland/env.rs`: NVIDIA `VK_ICD_FILENAMES` guard (from `engine.rs`), `/dev/shm`
  cleanup, `$XDG_RUNTIME_DIR` socket dir, XDG data dir, `xdg-open` opener candidates.
- `state.rs`: `socket_path` via `backend::env::runtime_socket_dir`; shm names shortened to
  `/sfv-s-{pid}` / `/sfv-a-{pid}` (macOS `PSHMNAMLEN` ≈ 31; engine reads names from
  `SAFFRON_VIEWPORT_SHM_*` env — nothing engine-side changes).
- `Cargo.toml`: wayland crates + `xdg-portal` rfd + `sync-secret-service` keyring under
  `[target.'cfg(target_os = "linux")']`; keyring `apple-native`, rfd plain, objc2 family under
  `[target.'cfg(target_os = "macos")']`.

## Invariants

- Teardown ordering survives the move: CEF browser released → compositor `Connection` dropped →
  winit display freed.
- `git diff --color-moved=dimmed-zebra` shows mechanical moves; `cargo metadata` confines wayland
  crates to Linux.

## Verify

macOS: portable code typechecks (full proof lands with Phase 2's appkit skeleton). Linux (user):
`just editor` gate + a `just run` session — paint, viewport, file DnD, window controls, engine
spawn/teardown, no stale `/dev/shm/sfv-*`.
