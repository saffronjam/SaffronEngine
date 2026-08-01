+++
title = 'Viewport panel'
weight = 2
math = true
+++

# Viewport panel

The Viewport panel is the transparent Scene region through which the engine's native presentation surface is visible. The React panel does not draw the scene. It owns the surface bounds, the loading cover, and the browser-side input that must cross the control plane.

The shell maintains two presenter views: `scene` and `assetPreview`. Each has its own shared-frame ring, native surface, geometry, park state, and engine render target. The Scene panel owns the `scene` view. The asset editor and material-graph preview take turns placing the shared `assetPreview` view in their active pane.

On Wayland, a view is a subsurface below the CEF webview. On macOS, it is an AppKit layer below the transparent browser content. The rest of the editor addresses both backends through the same view IDs and shell commands. See [viewport compositing](../viewport-compositing/) for the frame transport.

## Geometry and render size

`useSubsurfaceBounds(hostRef, "scene")` reads the panel's logical CSS rectangle and the window scale factor. It sends both through `set_viewport_bounds`, which updates native geometry immediately. A settled update also asks the engine to set that view's render size in device pixels:

$$
(w_{render}, h_{render}) = \operatorname{round}(scale \cdot (w_{css}, h_{css})).
$$

The hook separates interactive geometry from render-target recreation:

| Update | Cadence | Shell work | Engine work |
|---|---:|---|---|
| Live geometry | at most every 16 ms | move and stretch the current frame | none |
| Settled geometry | 150 ms after the last change | commit exact bounds | `set-viewport-size` for that view |
| Forced settle | immediately | commit exact bounds | resize before a reveal |

A [`ResizeObserver`](https://developer.mozilla.org/en-US/docs/Web/API/ResizeObserver) drives changes to the host element. Window resize and the editor's layout-settled bus cover changes that do not produce another observed dock mutation. Degenerate rectangles are ignored, and unchanged live bounds are deduplicated.

The split keeps a divider drag responsive: the native surface follows the panel while the existing image stretches, then the engine recreates the offscreen target once at the final device-pixel size. Each hidden view retains its last committed bounds. The `assetPreview` view receives new bounds when another preview-bearing tab becomes active.

## Startup and parking

The Viewport panel owns renderer readiness. Until the control socket answers `viewport-native-info`, it probes with a 1.5-second timeout and retries after 150 ms. A successful response moves the editor to `ready`; `LoadingOverlay` covers the viewport until then.

`App` decides which view is visible. A modal parks both views. A Scene tab unparks `scene`, while an asset-editor or material-graph tab unparks `assetPreview` and calls `set-active-view` so the engine routes the matching scene, camera, and render target.

Parking hides the AppKit layer or detaches the Wayland buffer. The shared-frame ring keeps the last image, but that image is not visible while parked. Unparking happens immediately and reattaches the retained frame before a new render arrives. Parking is delayed by two animation frames so the incoming opaque tab paints before the outgoing native surface disappears.

When a modal hides the Scene region, the panel paints `bg-background` over its normally transparent area. Tabs without any viewport also mark the host as occluded to stop background rendering; play-mode simulation continues because only rendering is gated.

## Picking and gizmo input

The engine window receives no direct pointer events, so the panel maps DOM coordinates into clamped viewport UVs. A left-button gesture follows this protocol:

1. Press captures the pointer and sends `gizmo-pointer begin` in normalized device coordinates.
2. Movement beyond 3 CSS pixels on either axis becomes a drag. Drag samples are coalesced to a 16 ms cadence, and `dragActive` pauses editor reconciliation.
3. Release sends `gizmo-pointer end`. A completed transform change records one Scene-tab undo entry after the authoritative transform is inspected.
4. A press and release below the threshold runs `pick` at the press UV. A miss clears selection.

Unpressed movement streams `gizmo-pointer hover` through a separate 16 ms coalescer. The engine tests editor billboards before mesh bounds, then returns the selected UUID. See [Gizmo](../gizmo/) and [Selection](../selection/) for those engine-side paths.

A press that [Vegetation mode](../vegetation-mode/) claims owns the whole gesture instead: no `gizmo-pointer` stream, no transform snapshot, and the release commits whatever the tool captured.

## Editor camera and gameplay keys

Holding the right mouse button asks the shell to lock and hide the cursor. CEF windowless rendering does not supply usable DOM motion while the native grab is active, so the shell emits relative `fly-look` events. The panel accumulates those deltas and the configured fly-key state, then sends `fly-input` at most every 16 ms. Releasing the button, pressing Escape, losing focus, or unmounting ends the grab and sends an inactive state.

Fly bindings use physical key codes from Editor Settings. Their defaults are W, S, A, D, Space, and Left Shift for forward, back, left, right, up, and down. The camera remains available in Play as the fallback when the scene has no primary camera.

While Playing or Paused, a separate window-level listener forwards gameplay keys through `script-input`. It ignores key presses owned by text inputs and ignores Meta-modified input. The pressed-key set is sent only when it changes and is cleared on window blur, document hiding, return to Edit, or effect teardown. This prevents a lost key-up event from leaving a script action held.

## Model placement

Asset drags carry `application/x-sa-asset`. When the payload contains a model, drag-over samples are coalesced through `asset-placement {phase: "preview"}`. The engine maintains a transient preview subtree and positions it on the scene surface or ground plane under the cursor.

Drop sends one final preview position followed by `phase: "commit"`; leaving the region or a failed drop sends `phase: "clear"`. Placement is accepted only in Edit on the Scene view. The preview stream allows only one request in flight and keeps the latest cursor sample, so asset dragging cannot queue behind camera or gizmo input.

## In the code

| What | File | Symbols |
|---|---|---|
| Scene host, input, and model drop | `editor/src/panels/ViewportPanel/` | `ViewportPanel`, `eventToUv`, `DRAG_THRESHOLD_PX`, `FLY_STREAM_MS` |
| Two-tier per-view geometry | `editor/src/lib/useSubsurfaceBounds.ts` | `useSubsurfaceBounds`, `computeBounds`, `liveSync`, `scheduleEndCommit` |
| View selection and parking policy | `editor/src/app/App.tsx` | `activeRenderView`, `sceneParked`, `assetParked` |
| Shell command bridge | `editor/shell/src/commands.rs` | `set_viewport_bounds`, `set_viewport_parked` |
| Shared view state and presenters | `editor/shell/src/viewport.rs` · `editor/shell/src/backend/*/presenter.rs` | `Viewports`, `ViewportShared`, `install` |
| Render view and input commands | `engine/crates/control/src/commands_render/` · `commands_asset.rs` · `commands_scene.rs` | `set-viewport-size`, `viewport-native-info`, `set-active-view`, `asset-placement`, `script-input` |

## Related

- [Viewport compositing](../viewport-compositing/) - shared frames and native presentation
- [Editor shell and viewport bridge](../editor-shell-and-viewport-bridge/) - shell ownership and platform backends
- [Asset pickers and drag-and-drop](../asset-pickers-and-drag-drop/) - asset payloads and drop targets
- [Editor camera](../editor-camera/) - fly controls, smoothing, and persistence
- [Play mode](../play-mode/) - primary-camera handover and gameplay input lifetime
- [Vegetation mode](../vegetation-mode/) - the tool palette that pre-empts a viewport press
