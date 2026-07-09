+++
title = 'Viewport panel'
weight = 2
+++

# Viewport panel

The viewport panel is the editor's window onto the 3D scene: a transparent `div` that owns
a screen rectangle and keeps the engine's **Scene-view** subsurface glued to it. The panel
renders no pixels of its own. The scene inside it is the engine's render showing through the
transparent page ([viewport compositing](../viewport-compositing/)).

It is one of two viewport panes — the [asset-editor](../asset-editor/) preview is the other —
and each pane drives its **own** subsurface + render target through the shared
`useSubsurfaceBounds` hook, parameterized by a view id (`"scene"` here). The subsurfaces sit
below the webview, so this panel's role is bounds-sync (report where the Scene view should be)
and input forwarding (translate pointer input into engine intent). Parking the region — when a
modal or another tab owns it — is driven from `App` per view.

## Bounds-sync

`useSubsurfaceBounds(hostRef, "scene")` reports the panel's logical CSS rect plus the window
scale factor through one shell command, `set_viewport_bounds(view, …)`. Rust fans it out for
that view: the logical rect (the UI *is* the toplevel surface in the CEF shell, so its offset
within the toplevel is zero) positions and sizes the Scene subsurface, and the device-pixel size goes to the
engine as `set-viewport-size {scene}` so the render matches the panel one-to-one. The
asset-editor preview drives the same hook with `"assetPreview"`, so the two surfaces are sized
independently.

Each surface is permanently its pane's size, so a tab switch never resizes it — only an
in-pane resize (a dock-divider drag) does. That resize uses two tiers so it never pays the
engine's target-recreation cost per drag tick:

- a **throttled live sync** (~16ms) on every geometry change — a `ResizeObserver` on the
  host div fires during a drag, and each tick moves and stretches the subsurface only
  (the current frame scales into the new rect);
- a **debounced resize-end commit** (~150ms) that sends one final exact bounds *and* the
  engine render size, so the scene re-renders sharp at the settled rect — once per
  gesture instead of per tick.

```ts
const observer = new ResizeObserver(onGeometryChange);  // live sync + schedule end-commit
observer.observe(el);
window.addEventListener("resize", onGeometryChange);
const offLayout = onLayoutSettled(scheduleEndCommit);   // a settled panel-split commits too
```

Both paths share a diff guard (skip if the bounds are unchanged). On mount the panel also
probes `viewport-native-info` until the engine's socket answers, then flips the phase to
`ready`, dismissing the loading overlay that covers the region until the first frame.

## Parking

Web UI composites freely over the live viewport — that is the point of the architecture —
but when a pane should show no scene (its tab is inactive, or a modal owns the region), its
surface is *parked*. `App` owns this per view: it calls `set_viewport_parked(view, true)` so
the surface keeps its last frame frozen rather than going black (the shared backdrop fills any
transparent gap). The Scene view is parked when the scene tab is inactive or a modal hides it
(the store's `viewportHidden` flag, now modal-only); the asset-preview view is parked whenever
its tab is not the active one. Because `App` drives it, parking works even while this panel is
unmounted, and the panel still paints an opaque background over its own region when
`viewportHidden` so the page never exposes the desktop.

## Pointer forwarding

The panel turns DOM pointer events into engine intent — the engine's hidden window
receives no input at all. A left press sends [`gizmo-pointer begin`](../gizmo/); travel
past a few pixels makes it a `drag` (streamed, with `dragActive` set so the poll backs
off); the release sends `end`. A press that did not travel is a click — it
[ray-picks](../selection/) at the press UV. A bare move with no button streams `hover`,
so the engine highlights the handle under the cursor.

Holding the **right button** flies the [editor camera](../editor-camera/): the panel takes
pointer lock, accumulates relative deltas (`movementX/Y`) and the WASD/Space/Shift key
state, and streams them over `fly-input` (~16ms cadence; deltas accumulate between sends,
so nothing is lost). Releasing the button or pressing Escape (which exits pointer lock
natively) ends the fly. The six fly keys default to WASD/Space/Shift and are rebindable in
[Editor Settings](../editor-settings/) (the `camera.fly*` commands, matched on the physical
key code).

## In the code

| What | File | Symbols |
|---|---|---|
| The panel | `editor/src/panels/ViewportPanel.tsx` | `ViewportPanel`, `eventToUv` |
| Per-view two-tier bounds-sync hook | `editor/src/lib/useSubsurfaceBounds.ts` | `useSubsurfaceBounds`, `computeBounds`, `liveSync`, `scheduleEndCommit` |
| Pointer-lock fly streaming | `editor/src/panels/ViewportPanel.tsx` | the fly `useEffect`, `FLY_STREAM_MS` |
| Per-view rect + park bridge (Rust) | `editor/shell/src/commands.rs` | `set_viewport_bounds`, `set_viewport_parked` |
| Subsurface side | `editor/shell/src/presenter.rs` | `Viewports`, `ViewportShared`, `ViewSurface`, `install` |
| Render size + active view (engine) | `engine/crates/control/src/commands_render.rs` · `commands_asset.rs` | `set-viewport-size`, `viewport-native-info`, `set-active-view` |

## Related

- [Viewport compositing](../viewport-compositing/) — the transport this panel positions
- [Editor shell and the viewport bridge](../editor-shell-and-viewport-bridge/) — the shell and lifecycle around it
- [Gizmo](../gizmo/) — the pointer phases this panel forwards
- [Selection](../selection/) — click-pick from a non-drag press
- [Editor camera](../editor-camera/) — the fly input this panel streams
