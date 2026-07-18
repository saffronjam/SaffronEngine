# Editor-shell platform backends (Wayland ⇄ AppKit)

**Status:** IN PROGRESS — Phases 1–4 + 6 COMPLETED; Phase 5 core input + window-drag user-verified on macOS (dialogs/Keychain/Finder-drops and the user's Linux gate remain).

Give `editor/shell` a **compile-time modular backend architecture**: OS → backend selected by
`cfg(target_os)` (zero runtime detection), each backend a self-contained module behind an explicit
contract, so the existing Linux/Wayland implementation becomes `backend/wayland` and a macOS/AppKit
backend (`backend/appkit`) makes `just run` work on macOS. Future backends (Windows, an
accelerated-OSR variant) drop in as one folder + one cfg arm + one Cargo target table.

## Why

The shell is Wayland-native by construction: `ShellWindow::new` hard-errors `NotWayland`, the UI
compositor uploads CEF OSR frames via `wl_shm`/`memfd`, the engine-viewport presenter drives
`wl_subsurface`s, and the wayland crates' build script panics on macOS (pkg-config probe) — the
crate cannot compile there. The engine side already runs on macOS (MoltenVK, e2e-verified); the
editor is the missing half.

## Mechanism (decided)

`std::sys`-style cfg-selected module contract — no traits, no generics:

```rust
// editor/shell/src/backend/mod.rs
#[cfg(target_os = "linux")]  #[path = "wayland/mod.rs"] mod imp;
#[cfg(target_os = "macos")]  #[path = "appkit/mod.rs"]  mod imp;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("saffron-editor-shell has no backend for this target OS");
pub use imp::{Handles, UiCompositor, presenter, keys, bootstrap, env};
```

The contract is the shared call sites (compile-checked per target) plus the doc-comment item list
in `backend/mod.rs`. Frozen across backends: the frontend ABI (`editor/src/shell/index.ts`, every
command in `commands.rs::dispatch`) and the engine wire (control socket + shm ring `SFV2`).

macOS window chrome (decided): **native decorations** — transparent titlebar + full-size content
view + traffic lights; the frontend hides its custom window buttons on macOS (UA sniff, no ABI
change). macOS presentation (researched): IOSurface pools as `CALayer.contents`
(Chromium/Firefox architecture), paced by `NSView.displayLink` (CADisplayLink; CVDisplayLink is
deprecated); CEF loads via `cef::library_loader::LibraryLoader` inside a dev `.app` bundle
assembled by `cef::build_util::mac` (bundle + 5 helpers — mandatory on macOS, no single-exe model).

## Phases

| Phase | File | Delivers |
|---|---|---|
| 1 | `phase-1-seam-extraction.md` | Wayland code → `backend/wayland`; portable `viewport.rs`/`dnd.rs`; Cargo target tables; shm names ≤31 chars. Zero Linux behavior change. |
| 2 | `phase-2-macos-bootstrap.md` | `backend/appkit` window + CEF bootstrap (LibraryLoader), helper bin, dev-bundle bin. CEF initializes + paints on macOS. |
| 3 | `phase-3-ui-compositor.md` | IOSurface pool + backdrop/UI CALayers; Retina `screen_info`; frontend traffic-light inset. |
| 4 | `phase-4-engine-presenter.md` | Engine shm → per-view IOSurface layers, display-link pacing, `refresh_mhz`, park/bounds. |
| 5 | `phase-5-input-dnd-deps.md` | `kVK_*` key table, winit file-drop → `DndEvent`, pointer lock, dialog threading, Keychain. |
| 6 | `phase-6-devloop-docs.md` | justfile Darwin branches (bundle-in-dev-loop, no Ozone), docs updates, Linux diff audit. |

Full research + design record: session workflow `wf_fce86596-3c9` (seam map, CEF-on-macOS brief,
presentation-stack brief, dep brief, design). The Linux gate is validated by the user on a Linux
box after Phase 1 and Phase 6 (this work happens on macOS).
