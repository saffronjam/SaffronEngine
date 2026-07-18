# Phase 5 — Input, DnD, dialogs, credentials

**Status:** IN PROGRESS — implemented and user-verified on macOS: keyboard/pointer input, the native titlebar window drag (CEF draggable regions + synchronous drag), tab drag, double-click maximize, and clean staged teardown. Remaining unverified: Finder drag-drop, native open/save dialogs, connector Keychain round-trip.

- `backend/appkit/keys.rs`: complete `kVK_*` virtual-keycode table covering every `KeyCode` the
  XKB table covers — Chromium's mac `NativeKeycodeToDomCode` derives DOM `event.code` from it, and
  a wrong native keycode silently breaks every registry shortcut plus the fly-cam.
- DnD: winit `HoveredFile` / `DroppedFile` / `HoveredFileCancelled` →
  `UiCompositor::observe_window_event` accumulates into the portable `DndEvent` queue; `pump_dnd`
  drains identically on both platforms (frontend `drag-drop` payload unchanged).
- Pointer lock: `CursorGrabMode::Locked` only on macOS (`Confined` is NotSupported there — the
  fallback branch is wayland-backend-only).
- Dialogs: validate `AsyncFileDialog` creation off the main thread under AppKit; if it misbehaves,
  marshal creation through the existing `state.inbox` main-thread mechanism (`ShellRequest`).
- Credentials: Keychain round-trip smoke via a connector secret (keyring `apple-native`); assert a
  real store, not the silent mock.

## Verify (macOS)

Typed text + every shortcut class (letters, digits, F-keys, arrows, modifiers) shows the correct
DOM `event.code` under inspect; fly-cam relative motion streams under pointer lock; a Finder drag
emits enter/over/leave/drop with correct paths + positions; native open/save panels appear;
connector secret persists across restarts via Keychain.
