+++
title = 'Play mode'
weight = 5
+++

# Play mode

Play mode runs physics, scripts, and animation against a disposable copy of the authored scene. Stop drops that copy and returns to the untouched edit scene. This boundary makes discard reliable: runtime changes never need to be reversed.

The state machine is `Edit -> Playing <-> Paused -> Edit`. Play enters or resumes simulation, Pause holds the current runtime state, Step grants fixed ticks while paused, and Stop returns to Edit.

## A scene built through the save format

Entering Play serializes the authored scene and loads the result into a fresh `Scene`. The duplicate therefore has the same component data that a project reload produces. Its asset catalog is shared, so referenced meshes, textures, and materials are not duplicated as GPU resources.

`active_scene` is the engine-side switch between the authored scene and the play duplicate. Scene queries and control commands use that switch, which lets the Hierarchy and Inspector inspect or edit the running copy without exposing the authored world. The current selection crosses the boundary by UUID; a runtime-created selection clears on Stop when no authored entity has the same UUID.

Animation players also start from runtime state rather than an editor preview. Each player begins at time zero, follows its authored `autoplay` setting, and discards preview and transition state. The play tick and script log/error rings start clean for the new session.

## Runtime lifetime and tick gate

The Edit-to-Play edge starts one `RuntimeSession`. It builds the physics world, starts the script VM, and retains the animation runtime until Stop. Pause and resume keep that session alive so its bodies, script instances, and component state remain available for inspection.

On each host update, animation runs first. `play_step_dt` then decides whether physics and scripts advance:

| State | Simulation delta |
|---|---|
| Edit | no simulation step |
| Playing | frame delta, clamped to $1/3$ second |
| Paused | no step unless one is granted |
| Paused with Step | exactly $1/60$ second per granted frame |

A Step command may grant more than one frame through its `frames` parameter. The host consumes one grant per update and increments `play_tick` only when simulation runs. Script failures are contained, recorded, and converted to a pause after the current step; the state transition does not re-enter the runtime while it is executing.

## Camera and editor controls

Edit renders through the editor camera. Playing and Paused render through the active scene's first primary `Camera`, including its parent-composed transform. If the scene has no primary camera, rendering falls back to the editor camera and the editor reports that fallback after Play succeeds.

Pause stops simulation, not the host loop. Rendering, the control socket, inspection, and editor-camera input remain available. The Hierarchy and Inspector therefore show the held play scene, and their writes remain disposable.

The editor marks Playing and Paused with an amber ring and top-bar tint. It also locks operations whose meaning belongs to the authored scene:

- Gizmo controls and shortcuts are disabled, and editor overlays are omitted.
- Scene-tab undo and redo are suspended without clearing their pre-play history.
- New, save, save-as, open, recent-project, reload, and import-project actions are disabled.

These locks do not make the play scene read-only. Inspector and control-plane writes can still tune the running copy for diagnosis.

## Commands and reconciliation

The playback controls call the same control commands exposed through `sa`:

```text
play
pause
step {"frames": 1}
stop
get-play-state
```

`play` enters from Edit and resumes from Paused. `pause` accepts only Playing, and `step` accepts only Paused. `stop` is idempotent in Edit. Entering Play is rejected while the asset editor owns the preview scene.

The React store updates optimistically when a playback button or shortcut is used. The regular `get-selection` reconciliation response carries `playState` and `playVersion`, so rejected commands and changes made by another client converge without a separate polling lane. The fixed shortcuts are Ctrl/Cmd+P for Play or Stop, Ctrl/Cmd+Shift+P for Pause or Resume, and Ctrl/Cmd+Alt+P for Step.

## In the code

| What | File | Symbols |
|---|---|---|
| State machine, duplicate, camera, and tick gate | `engine/crates/sceneedit/src/play.rs` | `PlayState`, `enter_play`, `play_step_dt`, `render_camera_view`, `stop_play` |
| Active-scene routing | `engine/crates/sceneedit/src/context.rs` | `active_scene`, `registry_and_active_scene`, `play_scene_and_input` |
| Shared simulation session | `engine/crates/runtime/src/session.rs` | `RuntimeSession::start`, `RuntimeSession::step`, `RuntimeSession::stop` |
| Host edge and update ordering | `engine/crates/host/src/layer.rs` | `HostLayer::reconcile_play_edge`, `HostLayer::update_session`, `HostLayer::drain_runtime_sinks` |
| Playback control commands | `engine/crates/control/src/commands_scene.rs` | `play`, `pause`, `step`, `stop`, `get-play-state` |
| Optimistic playback controls | `editor/src/panels/Topbar.tsx` | `onPlayPause`, `onStop`, `onStep` |
| Reconciliation and shortcuts | `editor/src/state/store.ts` · `editor/src/app/useGizmoShortcuts.ts` | `playState`, `playVersion`, `startReconcile` |

## Related

- [Editor camera](../editor-camera/) - the edit view and the fallback when a play scene has no primary camera
- [Scene hierarchy](../../scene-and-ecs/scene-hierarchy/) - UUID identity and parent-composed transforms
- [Script logs panel](../script-logs-panel/) - runtime output and contained script failures
- [Scene commands](../../tooling-and-control/scene-commands/) - the control-plane surface used by playback and editor actions
