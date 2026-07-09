+++
title = 'Editor settings'
weight = 12
+++

# Editor settings

Editor settings are the editor-wide preferences that belong to the *application*, not to any one project — the first of them being the keyboard shortcuts. A gear button at the top-right of the titlebar opens a modal where every editor shortcut can be rebound, and the choices persist across restarts and across projects.

The split matters: a project's scene, assets, and layout live with the project, but which key triggers the translate gizmo is a property of the person using the editor. So these settings save to one file in the editor's app-data directory, alongside the recent-projects list, never inside a project.

## The keybinding registry

Every rebindable command is declared once, in a frontend registry (`editor/src/lib/keybindings.ts`). A command carries an id (`gizmo.translate`), a label and category for the modal, a default key, and two classifying fields:

- **kind** — `press` commands are one-shot actions matched on a normalized key-string (`event.key` plus modifier prefixes in a fixed order, e.g. `"shift+f"`). `hold` commands are the held-state fly-camera keys, matched on the physical `event.code` (`"KeyW"`, `"ShiftLeft"`) so they survive keyboard-layout differences and never carry modifiers.
- **scope** — which commands can collide. `global` press commands share the one window-level shortcut listener, so a shared key would genuinely double-fire; the six `fly` keys share the viewport's fly listener; `hierarchy` and `assets` deletes are each scoped to their own focused panel, so the same Delete key in both is fine and never flagged.

The registry is the single source of truth. Handlers do not compare key literals anymore — they ask `matchesBinding(event, id, overrides)`, and the Topbar tooltips render `formatBinding(...)` so "Translate (W)" tracks a rebind to "Translate (T)" automatically.

This is the same model major editors use — VS Code, Unity's Shortcut Manager, Unreal's keyboard-shortcut preferences — defaults defined in code, the user file holding only what they changed.

## Deltas, not snapshots

The settings file stores **only the overrides**. A command the user never touched is absent from the file and resolves to its registry default; resetting a command deletes its key. This is VS Code's `keybindings.json` philosophy rather than Unreal's full-snapshot `.ini`: adding a command in a later version automatically gives every existing user its default, and a "reset" is just a key removal.

```json
{
  "keyBindings": { "gizmo.rotate": "t", "camera.flyForward": "KeyR" }
}
```

The file is `appdata/settings.json`, written next to `recent-projects.json` through the same Rust path helpers. The Rust side keeps the binding map untyped (`HashMap<String, String>`), so adding or renaming a command never touches Rust; the frontend drops any unknown command id on load, so an older binary reading a newer file degrades cleanly.

## The modal

The modal is a shadcn `Dialog` (a plain centered dialog — it does not park the viewport, unlike the asset viewer). A search box filters the command list, which groups by category. Each row shows the command label and a binding chip; clicking the chip enters **capture** mode, and the next keydown becomes the binding (Escape cancels). A bare modifier press (Shift alone) is ignored so capture keeps waiting for a real key; a `hold` row records the physical code instead, so `Left Shift` and `Space` are capturable there.

Conflicts are **advisory**, the VS Code way: a rebind that collides with another command in the same scope is still accepted, and both rows surface an "Also bound to …" warning rather than blocking the change. An overridden row gains a reset button; a footer "Reset all" clears every override after a confirm. Every change applies and persists immediately — there is no Apply/Cancel.

While the modal is open, the global shortcut hook is gated off (the dialog holds focus on non-text elements, so the text-entry guard alone would let shortcuts fire underneath it), and the capture listener runs in the capture phase so it pre-empts both that hook and the dialog's own Escape-to-close.

## In the code

| What | File | Symbols |
|---|---|---|
| Command registry + helpers | `editor/src/lib/keybindings.ts` | `COMMANDS`, `matchesBinding`, `bindingFor`, `normalizePressEvent`, `formatBinding`, `findConflict` |
| The settings modal | `editor/src/app/SettingsModal.tsx` | `SettingsModal`, `KeyboardSection`, `BindingRow` |
| Gear button | `editor/src/app/WindowTitlebar.tsx` | the `Settings` `TitlebarButton` |
| Store slice + hydration | `editor/src/state/store.ts` | `keyBindings`, `settingsOpen`, `setKeyBinding`, `resetKeyBinding`, `loadEditorSettings` |
| Persistence bridge | `editor/shell/src/settings.rs` | `load_editor_settings`, `save_editor_settings`, `EditorSettings`, `settings_path` |
| Client wrappers | `editor/src/control/client.ts` | `loadEditorSettings`, `saveEditorSettings`, `EditorSettings` |

## Related

- [Transform gizmo](../gizmo/) — the W/E/R defaults bound here
- [Viewport panel](../viewport-panel/) — the fly-camera keys bound here
- [Hierarchy panel](../hierarchy-panel/) and [Assets panel](../assets-panel-and-thumbnails/) — the Delete shortcuts
