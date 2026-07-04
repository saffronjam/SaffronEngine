+++
title = 'Undo/redo'
weight = 10
+++

# Undo/redo

Undo/redo lives **entirely in the editor**. The engine has no undo concept — no history, no snapshot, no "undo" command. An undo is the editor replaying an *inverse control command* it already has: for each edit the editor records "how to undo it" onto a client-side history, and undo dispatches that recorded inverse against the same control surface a normal edit uses. The engine just sees another `set-transform` (or `rename-entity`, or `material-set-graph`); it never learns that the command came from an undo. This is the same flat answer the [play mode](../play-mode/) page gives about the authored scene: the engine is unaware.

Because the inverse is "the same command fed the value I held before," an operation is undoable only when the editor already has its prior value — which it does for the things you edit through the [inspector](../inspector/), the [hierarchy](../hierarchy-panel/), the Environment and Render panels, and the material graph. Operations whose inverse isn't a command the editor can replay are deferred (see below), never faked with engine support.

## History is per main tab

There is one history per **main tab**, keyed on the tab's stable `ViewTab.id` (`historyByTab` in the store). The scene tab (`"scene"`) owns the scene-edit history for the whole session; each material-graph tab (`"materialGraph:<id>"`) owns its own graph history; the asset editor will own its own when it gains authoring. Undo and redo always act on the **active** tab's history, so Ctrl+Z in the scene tab never touches a graph's stack and vice-versa. Read-only tabs (a flame graph, an image viewer) never get a history — `isHistoryTab` allowlists only the editable kinds (`scene`, `materialGraph`, `assetEditor`).

Every scene edit records explicitly on the `"scene"` tab regardless of which tab is active, because the inspector and hierarchy stay visible across tabs. Only the material graph records on its own tab.

## One gesture is one entry

A gizmo drag or an inspector scrub is a burst of `set-transform` / `set-component` / `set-component-field` ticks that the [write coalescer](../inspector/) already collapses to one in-flight send. The history collapses the same burst into a single entry: it captures the prior value at the gesture's *begin* bracket and pushes one entry at the *end* bracket — never one per tick. Discrete edits (a toggle, an asset pick, a rename, a reparent, add/remove component) push one entry each. Text fields are bracketed by focus → blur, so editing a name is one entry, not one per keystroke.

Undo itself replays through the same `client` calls a normal edit uses, so the reconcile poll re-syncs from the engine's bumped `sceneVersion`/`selectionVersion` exactly as after any edit. A replay is atomic — it awaits one command and is repaired by the next poll — so it does **not** raise `dragActive` (which guards a live optimistic gesture); the `historyReplaying` flag instead suppresses an edit site recording the replayed command as a fresh entry. Undo restores the selection the edit acted on (scene tab only) so the gizmo lands back on the changed entity.

## What's undoable

In the scene tab: transforms, material factors and asset assignment, component field writes, add/remove component, script-field overrides, rename, reparent, environment + atmosphere fields, and the render toggles + AA + exposure. Creating an entity (add/duplicate/instantiate) is **undo-only** — undo destroys the created entity, but it cannot be redone, because re-creating mints a new id that would dangle later history entries. In a material-graph tab: the graph, snapshotted around each debounced apply.

Deferred (an op is undoable only when its inverse is the same existing command fed a value the editor held):

- **Entity deletion** — the id is gone; resurrecting a subtree needs a client snapshot + id remap the engine can't help with under the constraint.
- **Selection / deselection** — editor view-state mirrored from the engine, not an authored-scene edit (it is restored as context, but is not its own entry).
- **Play transport** and **animation preview** — transport, not edits; `step` is irreversible.
- **All asset-catalog / filesystem operations** — rename/move/delete/import/material-asset writes touch disk; their inverses either don't exist on the surface or are destructive.

## Keyboard and mouse

Ctrl+Z undoes; Ctrl+Shift+Z and Ctrl+Y redo. These are rebindable in [editor settings](../editor-settings/) (the `edit.undo`/`edit.redo` commands) and gated like the gizmo shortcuts — off while a text field is focused (the browser keeps its own text undo there), while the settings modal is open, and until the engine is ready.

Mouse **Back** (button 3) undoes and **Forward** (button 4) redoes, over every panel, and they are *not* text-gated — a side-button click is not typing. The same handler `preventDefault`s the webview's default Back/Forward navigation, and the keyboard hook traps `Alt+Left`/`Alt+Right` the same way, so no input ever navigates the single-page app off its one URL (a reload would drop all editor state).

The visible affordance is a pair of undo/redo icon buttons in the Topbar, beside the project menu. They disable when there is nothing to undo/redo on the active tab (so they grey out for the scene tab during play), and their tooltips name the next action and its shortcut.

## History invalidation

The scene history clears when the scene under it is replaced: loading a project or scene (`resetSceneState`), or a backwards `sceneVersion` step in the poll (an external/CLI scene swap — `sceneVersion` is otherwise monotonic, so play and stop never trip it). Entering play does **not** clear it: scene recording and undo are *paused* while playing (the scene tab targets a throwaway duplicate that Stop discards), so the pre-play history stays intact and resumes on Stop. Material-graph histories are unaffected by play.

## Extending undo to a new editable surface

There are exactly two shapes a new edit joins through; the keybindings, the toolbar buttons, and the mouse handler are all generic over the active tab, so nothing else changes.

- **Shape A — an inverse-command edit site** (the scene-edit pattern): at the mutation, capture the prior value the editor already holds *before* the optimistic write, then `pushEdit({ undo: () => client.X(id, prior), redo: () => client.X(id, next) })`. Wrap a continuous gesture in `beginEdit` so the burst is one entry. Use it when the engine is the source of truth and the inverse is the same command fed the prior value.
- **Shape B — a `useTabSnapshotHistory` editable tab** (the material-graph pattern): the tab holds a canonical local model flushed by one apply command. Call `useTabSnapshotHistory<M>(tabId, { read, write, equals })`, `seed` after load, `record()` at the apply boundary, and `if (consumeReplay()) return` at the top of the apply effect. Use it when there is no engine prior-value to read back between saves.

The **asset editor** is the worked Shape-B example: when it gains authoring it opens as `assetEditor:<id>`, holds a local model, and persists via one apply command, so wiring undo is `useTabSnapshotHistory<Model>(\`assetEditor:${id}\`, { read, write, equals })` — no change to the history core. The one precondition: a new editor must keep a local model and a single replayable apply command, or it stays deferred.

## In the code

| What | File | Symbols |
|---|---|---|
| Pure history primitive | `editor/src/lib/undo.ts` | `UndoableEdit`, `TabHistory`, `appendEdit`, `takeUndo`, `takeRedo`, `canUndo`, `canRedo` |
| Store slice + replay | `editor/src/state/store.ts` | `historyByTab`, `historyReplaying`, `pushEdit`, `undo`, `redo`, `beginEdit`, `clearSceneHistory`, `replayHistory`, `recordEntityCreation` |
| Reusable snapshot hook | `editor/src/lib/useTabSnapshotHistory.ts` | `useTabSnapshotHistory`, `seed`, `record`, `consumeReplay` |
| Keyboard + mouse | `editor/src/app/useUndoRedoShortcuts.ts` · `useMouseHistoryNav.ts` · `lib/keybindings.ts` | `edit.undo`, `edit.redo` |
| Undo/redo toolbar buttons | `editor/src/panels/Topbar.tsx` | the per-active-tab `history` selector (enabled state + next-entry label) |
| Scene-edit recording | `editor/src/panels/InspectorPanel.tsx` · `ViewportPanel.tsx` · `EnvironmentPanel.tsx` · `RenderPanel.tsx` | `recordFieldEdit`, the gizmo gesture, `recordEnvEdit`, `recordRender` |

## Related

- [Inspector](../inspector/) — the RMW field writes and the coalescer the history brackets ride
- [Play mode](../play-mode/) — why undo pauses (not clears) during a play session
- [Editor settings](../editor-settings/) — rebinding `edit.undo`/`edit.redo`
- [Asset editor](../asset-editor/) — the Shape-B surface that adopts the snapshot hook when it gains authoring
