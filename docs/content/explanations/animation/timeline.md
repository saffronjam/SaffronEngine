+++
title = 'Timeline panel'
weight = 4
+++

# Timeline panel

The Timeline panel is the editor's sequencer for the selected entity's animation: track header on
the left, a millisecond ruler on top, the clip drawn as a bar with its keyframe ticks, a draggable
playhead, and a transport bar. It is a viewer of the engine's [playback runtime](../playback-runtime/)
— the engine owns the pose, the panel mirrors the player's state and drives Edit-mode preview over
the control plane.

## One surface, two mounts

The panel itself is a thin composition. `TimelineTransport` (the button bar with a clip picker) and
`TimelineSurface` (headers, canvas lanes, scrub area, footer) live in `components/timeline/` and
render against a `TimelineTarget`:

```ts
export interface TimelineTarget {
  entityId: string | null;            // who the transport commands
  state: AnimationStateResult | null; // the polled player mirror
  clips: AnimationClipDto[];          // the clip picker's options
  enabled: boolean;                   // the rig gate
}
```

The dock panel builds its target from the scene selection; the [asset editor](../../ui-and-editor/asset-editor/)
mounts the same two components against the previewed model and hides the clip picker (its clip list
panel owns picking). Both targets are entities in the active scene, so the commands are identical.

The header column is 140 px wide and shows one row per track: a teal accent swatch (`TRACK_ACCENT`,
`#2dd4bf`) and the clip name. A footer summarizes the model, `Duration 1.25s · 1 track · 1 clip`,
with a monospace `time / duration` readout on the right.

## Canvas, not React

The ruler, clip bars, and playhead draw on one 2D canvas. The webview composites over the live
engine viewport, so editor CPU competes with render-frequency work; a React re-render per playhead
move would pay component-tree cost for a one-line redraw. `TimelineCanvas` is created once per
mount and fed imperatively through `setModel` and `setPlayhead`, which coalesce into a single
[`requestAnimationFrame`](https://developer.mozilla.org/en-US/docs/Web/API/Window/requestAnimationFrame)
redraw.

The ruler picks its tick step from a fixed millisecond ladder (10 ms up to 60 s), taking the
smallest step that keeps ticks at least 64 px apart (`chooseTickStepMs`). A 2 s clip in an 800 px
lane gives 0.4 px/ms: 100 ms ticks would sit 40 px apart, so the ruler steps up to 250 ms and
labels every 100 px.

Each clip is one bar spanning its duration, tinted by the track accent with a 2 px accent rail on
its leading edge. Along the bar's lower edge the canvas draws a short vertical tick at every
keyframe time of every channel in the active clip — the real per-channel sample times that
`list-clips` returns in `AnimationChannelDto.times`, clipped to the bar.

## Reading playback state

The panel never calls the wire to read. The store's reconcile poll ticks every 50 ms while the
editor is focused and the engine ready; each tick runs `get-selection`, whose reply carries an
`animationVersion` stamp. Every playback mutation bumps it (`play-animation`, `seek-animation`,
`set-animation-playing`, `set-animation-loop`, `stop-preview`), whatever the origin, so a shell
command moves this panel too:

```sh
sa play-animation <rig> Walk --loop   # the panel's bar, playhead, and Play button follow
sa seek-animation <rig> 0.5           # the playhead jumps on the next poll
```

When the stamp bumps, or the selection changes, `refreshAnimation` fetches `get-animation-state`
and `list-clips` in parallel and writes both into the store slice. `get-animation-state` rejects
with `entity has no animation player` when the entity carries none; the slice clears silently,
since an unrigged selection is the normal case, not an error.

`list-clips` returns the whole project catalog for any entity, so the clip list alone cannot gate
the panel — an unrigged cube would show a phantom track. `isAnimatable` checks the inspected
component map for `AnimationPlayer`, `SkinnedMesh`, or `Morph`, the three clip-driven sources
(skeletal, [node-TRS](../node-trs-animation/), and [blend shapes](../morph-targets/)).

## Playhead motion between polls

Time advancing bumps no version, so a playing clip generates zero poll traffic. Instead
`TimelineSurface` self-advances a local playhead in its own `requestAnimationFrame` loop, stepping
by frame delta times the player's `speed` and honoring the wrap mode: `once` clamps at the end and
stops, `loop` wraps, `pingpong` reflects. Whenever the slice changes, the loop snaps to the
engine's authoritative `time` and re-arms. Each step touches only the canvas; React never renders.

## Edit-mode preview transport

The transport drives preview of the selected rig in Edit, decoupled from the global play state.
Every control is a typed wrapper over one command:

| Control | Command | Effect |
|---|---|---|
| Play / pause | `set-animation-playing` | resume or pause without moving the playhead |
| Clip picker | `play-animation` | load the clip at frame 0 and play it with the current wrap |
| Loop toggle | `set-animation-loop` | `loop` ↔ `once` (a `pingpong` player reads as looping) |
| Jump to start / end | `seek-animation` | seek to `0` / the clip duration |
| Step back / forward | `seek-animation` | nudge the playhead ±1/30 s (`STEP_SEC`) |

The preview commands set the player's `preview_in_edit` flag. In Edit the animation tick samples
only flagged rigs; in Play it animates every rig, and entering Play resets each player and clears
the flag. The sampled pose lands in a runtime `PoseOverride` per bone and never touches the
authored bone `Transform`, so `stop-preview` (clearing the flag) reverts the rig to its rest pose
on the next tick.

## Scrubbing

The lane area is one full-width pointer-capture surface. On drag it converts pointer x to seconds
(`xToSec`), moves the canvas playhead directly, and pushes the value through two throttles:
`useScrubValue` keeps a drag-local value and emits at most once per frame, and a `makeCoalescer`
sends at most one `seek-animation` at a time, 50 ms apart, latest value wins. Release flushes the
final value so the wire lands where the pointer let go, not a frame earlier.

Each seek passes `seekBlend: 0.05`. The engine treats it as a self-transition that eases the pose
toward the seeked time, so sparse ~20 Hz seeks read as one continuous drag in the viewport. A
paused rig scrubs the same way: the Edit tick samples every preview rig at its playhead each
frame, so the pose follows the drag without playing.

> [!NOTE]
> The panel edits playback state only: clip choice, playhead, wrap, play/pause. The keyframe
> ticks visualize the imported clip's channel sample times; there is no keyframe editing surface.

## In the code

| What | File | Symbols |
|---|---|---|
| The panel composition + rig gate | `panels/TimelinePanel.tsx` | `TimelinePanel`, `isAnimatable` |
| Transport bar | `components/timeline/TimelineTransport.tsx` | `TimelineTransport` |
| Surface: headers, scrub, playhead loop | `components/timeline/TimelineSurface.tsx` | `TimelineSurface` |
| Shared target seam + constants | `components/timeline/shared.ts` | `TimelineTarget`, `TRACK_ACCENT`, `STEP_SEC` |
| Canvas renderer | `lib/timelineCanvas.ts` | `TimelineCanvas`, `TimelineModel`, `chooseTickStepMs` |
| The poll gate + state slice | `state/store.ts` | `startReconcile`, `refreshAnimation`, `setAnimationState` |
| Typed control wrappers | `control/client.ts` | `getAnimationState`, `listClips`, `playAnimation`, `seekAnimation`, `setAnimationLoop` |
| Scrub + coalesce primitives | `lib/useScrubValue.ts`, `control/coalesce.ts` | `useScrubValue`, `makeCoalescer` |
| Engine command handlers | `control/src/commands_animation.rs` | `register_animation_commands`, `state_of` |
| Preview evaluation | `animation/src/runtime.rs` | `tick_animation`, `AnimMode` |

## Related

- [Playback runtime](../playback-runtime/) — the evaluator whose `time` the playhead shows
- [Skeleton overlay](../skeleton-overlay/) — the viewport bones the preview moves
- [Animation data model](../animation-data-model/) — the clip and channel types behind the bars
- [Asset editor](../../ui-and-editor/asset-editor/) — the second mount of the same transport + surface
- [Control plane](../../tooling-and-control/control-plane-architecture/) — the version-stamp polling model
