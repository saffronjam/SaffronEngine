+++
title = 'Undo/redo'
weight = 10
+++

# Undo/redo

Undo and redo are editor-owned command histories. The engine exposes no history command; a replay sends the same control operation as a normal edit, with either the captured prior value or the recorded next value.

## Per-tab histories

`historyByTab` maps each editable main-tab id to a `TabHistory`. The Scene tab uses the stable id `scene`, while a material graph uses `materialGraph:<asset-id>`. Scene edit sites explicitly record against `scene`; the material graph records against its own tab id.

Eligible tab kinds are `scene`, `materialGraph`, and `assetEditor`. Viewer and Store tabs do not accept history entries. Undo, redo, button labels, and enabled states use the active tab, so commands in one material graph cannot replay Scene edits or another graph's snapshots.

Each history has a `past` stack and a `future` stack. The newest past entry is the next undo; the first future entry is the next redo. A fresh edit clears the future branch, and each tab retains at most 200 past entries.

```text
past:   [Rename, Reparent, Set roughness]  <- next undo
future: [Set exposure]                     <- next redo
```

Closing a tab deletes its history. A project or scene replacement clears every history because the stored ids and prior values belong to the replaced scene. A backwards `sceneVersion` step clears the Scene history while preserving histories for live non-Scene tabs.

## Inverse-command entries

`UndoableEdit` stores a label, async `undo` and `redo` closures, optional selection context, and an optional `redoable` flag. Most Scene edits capture values already present in the store, then build both closures from the same control method.

```ts
useEditorStore.getState().pushEdit(
  {
    label: "Reparent",
    selectionId: id,
    undo: () => client.setParent(id, previous ?? null),
    redo: () => client.setParent(id, parentId),
  },
  "scene",
);
```

This form covers transform and component fields, MaterialSet slots, script overrides, component order, add or remove component, entity rename and reparent, environment fields, post-processing, and project render settings. Replays increment the engine's normal scene stamps, so the reconcile poll refreshes the same surfaces as it does after a direct edit.

Entity creation is undoable but not redoable. Its entry destroys the minted uuid and sets `redoable: false`; undo drops that entry instead of adding it to the future stack. This prevents a recreated entity with a different uuid from invalidating later entries.

Selection, play transport, animation preview, asset catalog and filesystem operations, collider fitting, debug overlays, and view modes do not push history entries. Entity deletion also has no entry because its command does not return a restorable subtree snapshot.

## Gesture boundaries

Continuous controls send many live values but record one history entry. Inspector scrubs, environment controls, post-processing controls, and script overrides capture a prior value when the gesture starts. Release sends the exact final value and compares it with the prior value before recording.

The transform gizmo captures the selected Transform on press. After the engine applies the release sample, the viewport inspects the entity and records one entry from the original snapshot to the settled transform. `dragActive` keeps reconcile writes from replacing optimistic gesture state; it is separate from history replay.

Discrete actions such as a toggle, asset-field choice, rename, reparent, component add, or component removal record after one accepted command. Rejected commands do not create an entry at these sites. `beginEdit` provides the same prior/commit bracket for an edit surface that can express its inverse with closures.

## Snapshot entries

`useTabSnapshotHistory` handles an editor whose canonical state is a local model applied through one command. The material graph supplies `read`, `write`, and normalized `equals` functions. It seeds the baseline after load, compares snapshots at each apply boundary, and records `{before, after}` only when the graph changes.

Undo calls `write(before)` and redo calls `write(after)`, which updates the local graph and persists it through `materialSetGraph`. The hook owns a replay flag that survives the awaited control call. `consumeReplay` lets the graph's debounced apply effect ignore the settle produced by replay rather than recording it as a new edit.

## Replay

`replayHistory` removes the next entry from the appropriate stack and raises `historyReplaying` while its closure is in flight. `pushEdit` ignores writes during that interval. When replay settles, the entry moves to its destination stack and the flag clears.

A rejected replay produces an error toast and console entry. The stack transition still completes in `finally`, and the reconcile poll restores the visible engine state. For Scene entries with `selectionId`, replay also selects that entity so the hierarchy, Inspector, and gizmo return to the edit's context.

Scene replay is paused while Playing or Paused because the active engine scene is the throwaway play duplicate. The Scene history remains stored and becomes available again in Edit. Non-Scene tab histories remain independent of play state.

## Input and controls

The Topbar buttons show the next entry label and disable when the active history cannot move in that direction. The keyboard bindings default to Ctrl+Z and Ctrl+Shift+Z; Ctrl+Y is a fixed redo alias. The registered Undo and Redo chords can be changed in [editor settings](../editor-settings/).

Keyboard undo is disabled while a text control has focus, which leaves text editing to the webview. The settings dialog and non-ready engine states also gate the shortcut. Alt+Left and Alt+Right map to undo and redo even when a text control is focused, preventing webview navigation away from the editor.

By default, Mouse Back and Forward have a separate purpose: they traverse main-tab activation history, or Assets folder history while the pointer is over that panel. Middle click closes the hovered tab. These mouse commands never call the undo or redo store actions.

## In the code

| What | File | Symbols |
|---|---|---|
| Pure stack transitions | `editor/src/lib/undo.ts` | `UndoableEdit`, `TabHistory`, `appendEdit`, `takeUndo`, `takeRedo`, `HISTORY_CAP` |
| Per-tab store and replay | `editor/src/state/store.ts` | `historyByTab`, `pushEdit`, `beginEdit`, `replayHistory`, `restoreSelectionContext` |
| Creation entry | `editor/src/state/store.ts` | `recordEntityCreation` |
| Snapshot history | `editor/src/lib/useTabSnapshotHistory.ts` | `useTabSnapshotHistory`, `seed`, `record`, `consumeReplay` |
| Material-graph consumer | `editor/src/panels/MaterialGraphEditor.tsx` | `MaterialGraphEditor`, `graphsEqual` |
| Keyboard dispatch | `editor/src/app/useUndoRedoShortcuts.ts` | `useUndoRedoShortcuts` |
| Mouse tab navigation | `editor/src/app/useMouseBindings.ts` | `useMouseBindings`, `dispatch` |
| Topbar affordances | `editor/src/panels/Topbar.tsx` | `Topbar`, `canUndo`, `canRedo`, `undoLabel`, `redoLabel` |
| Scene recording sites | `editor/src/panels/InspectorPanel.tsx` · `ViewportPanel.tsx` · `EnvironmentPanel.tsx` | `recordFieldEdit`, `gizmoGesture`, `recordEnvEdit` |

## Related

- [Inspector](../inspector/) — optimistic field writes and gesture brackets
- [Gizmo](../gizmo/) — transform snapshot captured around one drag
- [Play mode](../play-mode/) — authored history while the play duplicate is active
- [Editor settings](../editor-settings/) — keyboard and mouse binding registry
- [Material graph live preview](../material-graph-live-preview/) — local model persisted by the snapshot history
