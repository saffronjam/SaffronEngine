+++
title = 'Editor settings'
weight = 12
+++

# Editor settings

Editor settings hold preferences and credentials that belong to the editor user rather than the open project. The gear button in the Topbar opens sections for keyboard bindings, dock-layout reset, and connector secrets.

## Persistence boundaries

The three sections deliberately write to different stores:

| Section | State | Storage |
|---|---|---|
| Keyboard | User overrides to command defaults | `<appDataDir>/settings.json` |
| Layout | Scene and asset-editor dock trees | Per-project webview storage |
| API & Secrets | Connector credentials | Operating-system keyring |

`appDataDir` comes from `SAFFRON_APPDATA_DIR` when set; an installed editor uses its platform data directory. The shell and spawned host receive the same path, but only the shell reads `settings.json`.

## Keyboard registry

Every rebindable command has one `CommandDef` in `COMMANDS`. The definition supplies its id, visible label, category, command kind, default binding, and conflict scope. Handlers resolve the effective binding through `bindingFor` or `matchesBinding`, so tooltips and input behavior change together after a rebind.

| Kind | Stored form | Matching rule | Example |
|---|---|---|---|
| `press` | Normalized `event.key` with ordered modifiers | Exact modifier set | `ctrl+shift+z` |
| `hold` | Physical `event.code` | Layout-independent held key | `KeyW` |
| `mouse` | Named mouse token | Middle or side-button dispatcher | `mouse:back` |

Conflict scopes match the listener that receives an input. Global commands share the window shortcut listener, fly commands share the viewport's held-key handler, and tab mouse commands share the mouse dispatcher. Hierarchy and Assets deletion use separate focused-panel scopes, so both can use Delete without a conflict.

The settings file stores only overrides:

```json
{
  "keyBindings": {
    "gizmo.rotate": "t",
    "camera.flyForward": "KeyR",
    "tab.close": "mouse:back"
  }
}
```

`bindingFor` falls back to the registry default when a command id is absent. Assigning a command's default removes its override, and Reset all writes an empty map. Hydration drops unknown ids; a missing, unreadable, or malformed file also produces the empty map.

## Capturing a binding

The Keyboard section groups commands by category and filters them by label or category. Clicking a binding button starts capture. Escape cancels; a bare modifier keeps capture open; a press command records the normalized key chord; a hold command records the physical code.

Mouse commands accept the middle, back, or forward button. The middle button arrives through the webview pointer event. Native side-button events cover platforms where the webview consumes navigation buttons before page input.

Capture listeners run in the capture phase and stop propagation. This prevents the new binding from also invoking an editor command or closing the dialog. A same-scope conflict remains accepted, but both rows show which command shares the binding.

Every accepted change updates the store and writes the delta immediately. A single reset removes one override; Reset all asks for confirmation before clearing the map.

## Layout reset

The Layout section restores the default Scene and Asset Editor dock trees while preserving the set of open panels. It also clears remembered panel locations, so an open panel returns to the default branch for its dock space. Normal dock mutations continue to persist through the [dock system](../dock-system/).

## API secrets

The API & Secrets section configures the API key used by the [Poly Pizza](https://poly.pizza/) asset-store connector. `ApiKeyField` can save, replace, or clear the key. The frontend receives only a presence boolean after storage; it never reads the secret back.

`Credentials` stores secrets under the `saffron-anima` service with the connector id as the account. When no keyring service is reachable, or `SAFFRON_NO_KEYRING` is set, credentials use process memory and disappear when the shell exits. Secrets never enter `settings.json` or a project file.

## In the code

| What | File | Symbols |
|---|---|---|
| Command registry and matching | `editor/src/lib/keybindings.ts` | `COMMANDS`, `bindingFor`, `matchesBinding`, `findConflict` |
| Settings UI | `editor/src/app/SettingsModal.tsx` | `SettingsModal`, `KeyboardSection`, `LayoutSection`, `ApiSecretsSection` |
| Topbar entry point | `editor/src/panels/Topbar.tsx` | `setSettingsOpen` |
| Keybinding store and hydration | `editor/src/state/store.ts` | `setKeyBinding`, `resetKeyBinding`, `resetAllKeyBindings`, `loadEditorSettings` |
| Settings file | `editor/shell/src/settings.rs` | `EditorSettings`, `read_settings`, `write_settings` |
| Dock reset | `editor/src/state/store.ts` | `resetDockLayout`, `resetLayoutPreservingOpen` |
| Secret field | `editor/src/storefront/ApiKeyField.tsx` | `ApiKeyField` |
| Credential backend | `editor/shell/src/connectors/credentials.rs` | `Credentials`, `CredentialError` |

## Related

- [Gizmo](../gizmo/) — transform commands whose shortcuts come from the registry
- [Viewport panel](../viewport-panel/) — held fly-camera bindings
- [Dock system](../dock-system/) — layouts restored by the Layout section
- [Assets panel and thumbnails](../assets-panel-and-thumbnails/) — focused-panel deletion binding
