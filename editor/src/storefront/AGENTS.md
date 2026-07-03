# storefront — the Asset Store frontend

The React half of the in-editor **Asset Store**: a `store` `ViewTab` (`StoreWorkspace`) that
browses, searches, previews, and imports assets from external providers. The HTTP backend and the
provider contract live in `editor/src-tauri/src/connectors/` (its own `AGENTS.md`); read that first
for the wire shapes and auth model.

## What talks to what

- This UI calls **editor-local `store_*` / `connector_*` Tauri commands** wrapped in `types.ts`
  (`invoke(...)`), **not** the generated control client and **not** the engine. Only the final
  *import* crosses to the host: `store_import` / `store_import_part` download the deliverable, hand
  it to the host importer, and the imported asset lands in the catalog (which the control plane then
  sees). Nothing else in this directory touches the control socket.
- The **enabled-provider list** is the exception: it is control-plane, per-project state
  (`client.getStores()` / `client.setStores()`, persisted in `project.json`, team-shared). Secrets
  are not — they live in the OS keyring and this UI only ever learns a presence boolean
  (`connectorSecretStatus`).

## The one rule that bites

`types.ts` is **hand-authored** and must mirror the Rust `connectors` module's camelCase wire types
field-for-field. The parent editor's "protocol types are generated — never edit by hand" rule is
scoped to `src/protocol/sa-types.ts` and does **not** reach here: there is no codegen for the store
surface. If you add a field to a Rust DTO in `connectors/`, add it to `types.ts` by hand.

## Conventions

- **Search commits on Enter only.** The query bar is the shared `anima` chip-search
  (`components/anima`) configured with a 10-minute debounce (`COMMIT_ONLY_DEBOUNCE_MS = 600_000`), so
  a search fires on Enter, not per keystroke — every provider hit is a real network round-trip.
  `provider:` chips scope the search to a subset.
- **Results are a windowed, infinite-scroll grid** (`StoreResultsGrid`): each source advances its own
  server-side cursor and the grid pulls the next round-robin batch near the end, stopping when the
  session reports all sources exhausted. A `pendingReset` ref swaps the result set on a new query
  without a blank-frame flash.
- **Galleries and parts are lazy** — resolved (`store_asset_gallery` / `store_asset_parts`) only when
  a card is hovered/expanded or the split-import dropdown opens.
- **`StoreWorkspace` is always mounted, gated by an `active` prop** (`active={activeKind === "store"}`
  in `App.tsx`) — like every `ViewTab` body, it keeps its state when hidden rather than unmounting;
  it suppresses its portaled provider modal while inactive.
- **The first-provider `ProviderModal` is undismissable until one provider is enabled**
  (`canClose = enabled.length > 0`) — it opens automatically when nothing is enabled and from the gear
  button. `ApiKeyField` is shared with `app/SettingsModal.tsx`.
- **Secrets never round-trip to the webview** — set via `connectorSetSecret`, cleared via
  `connectorClearSecret`, and only ever queried as a boolean. An `oauthLoopback` provider logs in via
  `connectorLogin` (the bridge runs the browser flow and stores the token).
- **External links go through the bridge** (`invoke("open_external", { url })`), never
  `window.open` / `<a target>` — WebKitGTK ignores those (see the parent editor `AGENTS.md`
  browser-APIs rule). Rejected calls surface via `notifyError`, like everywhere else.
