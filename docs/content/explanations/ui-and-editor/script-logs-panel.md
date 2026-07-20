+++
title = 'Script logs panel'
weight = 9
+++

# Script logs panel

The Script Logs panel shows `sa.log` output from gameplay scripts. It belongs to **Tools > Diagnostics** and opens in the lower Assets leaf unless the saved dock layout has another location for it.

The panel polls only while it is open and Play is active. Closing it or returning to Edit removes its control-plane traffic, while the engine continues to retain its bounded log history.

## From a script to the panel

The play VM replaces the no-scene `sa.log` binding with a function that performs two writes. It sends the message to the engine log, then calls `ScriptHostBridge::log_sink` with the UUID of the script instance whose handler is running.

`RuntimeScriptBridge` appends that pair to the session's shared sink. After runtime startup and each simulation step, the host drains the sink into `SceneEditContext`. The context adds a monotonic sequence number, the current `play_tick`, and a wall-clock millisecond timestamp used only for display.

The engine ring holds the newest 1,024 lines. `drain-script-logs` accepts a sequence cursor and returns:

```json
{
  "events": [],
  "highWaterSeq": 42,
  "oldestSeq": 1,
  "overflowed": false
}
```

An `overflowed` result means the requested cursor predates the oldest surviving line. Sequence numbers remain monotonic across play sessions even though entering Play clears the engine ring, so a client can continue from its previous high-water mark.

## Editor retention and overflow

The editor stores lines in chronological order and retains the newest 2,000. When the open panel observes a fresh transition from Edit to Playing, it resets its cursor, visible buffer, and sticky overflow warning. Stop preserves the visible rows for review.

If the panel is closed when a new session begins, no reset request runs. Opening it later keeps the existing editor rows and appends lines from the active session. This follows from the open-and-playing poll gate; the engine-side sequence cursor still prevents duplicate delivery.

An overflow warning appears above the list when the engine reports dropped lines. It remains visible until the editor buffer is cleared at an observed fresh-play edge. The list mounts only the visible 20-pixel rows plus eight rows of overscan on each side, which bounds React work as the buffer grows.

The view follows new output while the scroll position is within two rows of the bottom. Scrolling upward disables that behavior until the reader returns to the end. Each row renders local time, the sender entity, and the message:

```text
14:08:03.127 [Player] grounded
```

Entity UUID `0` is shown as an em dash. A sender missing from the current entity list falls back to a shortened UUID.

## Find and entity filters

Ctrl/Cmd+F opens a compact find overlay in the panel. Escape or its close button clears the query and dismisses it. The overlay uses `AnimaSearchbar` with two kinds of criteria:

- Free text performs a case-insensitive substring match against the message.
- An `Entity:` token autocompletes at most 20 matching scene entities and becomes a chip containing the UUID.

Multiple entity chips are ORed together. Free text is ANDed with that entity set, so `Entity: Player grounded` finds messages containing `grounded` from any selected sender. The chip-search parser and serializer are independent of React and have their own unit tests.

## In the code

| What | File | Symbols |
|---|---|---|
| Panel, filtering, and windowed list | `editor/src/panels/ScriptLogsPanel.tsx` | `ScriptLogsPanel`, `LogRow`, `ROW_HEIGHT`, `OVERSCAN` |
| Chip search UI and model | `editor/src/components/anima/AnimaSearchbar.tsx` · `editor/src/components/anima/chipSearch.ts` | `AnimaSearchbar`, `SearchState`, `parseQuery`, `serialize` |
| Dock registration and default reopen target | `editor/src/components/dock/panelRegistry.tsx` · `editor/src/state/dockLayout.ts` | `SCENE_PANEL_REGISTRY.scriptLogs`, `DEFAULT_LEAF.scriptLogs` |
| Editor buffer and gated polling | `editor/src/state/store.ts` | `SCRIPT_LOG_LIMIT`, `appendScriptLogs`, `clearScriptLogs`, `startReconcile` |
| Client command wrapper | `editor/src/control/client.ts` | `drainScriptLogs` |
| Script binding and runtime sink | `engine/crates/script/src/bindings.rs` · `engine/crates/runtime/src/bridge.rs` | `register_scene_globals`, `ScriptHostBridge::log_sink`, `RuntimeScriptBridge` |
| Engine ring and drain command | `engine/crates/sceneedit/src/play.rs` · `engine/crates/control/src/commands_scene.rs` | `ScriptLog`, `SCRIPT_LOG_RING_CAP`, `push_script_log`, `drain-script-logs` |

## Related

- [Play mode](../play-mode/) - session lifetime, fixed stepping, and automatic pause on script failure
- [Script components and runtime](../../scripting/script-components-and-runtime/) - script slots and instance ownership
- [Script-declared fields](../../scripting/script-declared-fields/) - editable instance inputs exposed by scripts
- [Dock system](../dock-system/) - panel registration, reopen locations, and layout persistence
