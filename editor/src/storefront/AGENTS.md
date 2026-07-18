# storefront — the Asset Store frontend

The React half of the in-editor **Asset Store**: a `store` `ViewTab` (`StoreWorkspace`) that
browses, searches, previews, and imports assets from external providers. The HTTP backend and the
provider contract live in `editor/shell/src/connectors/` (its own `AGENTS.md`); read that first
for the wire shapes and auth model.

## What talks to what

- This UI calls **editor-local `store_*` / `connector_*` shell commands** wrapped in `types.ts`
  (`invoke(...)`), **not** the generated control client and **not** the engine. Only the final
  _import_ crosses to the host: `store_import` / `store_import_part` download the deliverable, hand
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

- **One store at a time.** A store dropdown (left of the search bar) picks which enabled connector
  to search — there is no cross-store mixing. Switching the dropdown re-runs the current query on the
  new store; disabling the store you're viewing (in the provider modal) auto-selects another enabled
  one. `SearchQuery.provider` carries the single store id.
- **Search commits on Enter only.** The query bar is the shared `anima` chip-search
  (`components/anima`) configured with a 10-minute debounce (`COMMIT_ONLY_DEBOUNCE_MS = 600_000`), so
  a search fires on Enter, not per keystroke — every provider hit is a real network round-trip. A
  `type:` chip filters by asset kind.
- **Browse state lives in the Zustand store, not the component** (`state/store.ts` `storeBrowse`
  slice): `storeSelected` / `storeSearchText` / `storeKind` persist to localStorage (reopening the
  Store returns you to where you left off); `storeSession` / `storeResults` / `storeScrollTop` are
  in-memory (a live backend session dies on a bridge restart, so a restart re-runs the last search).
- **Results are a row-virtualized, infinite-scroll grid** (`StoreResultsGrid`, via
  `@tanstack/react-virtual`'s `useVirtualizer` — one virtual row per grid row, each laying out
  `columns` cards): the store advances its server-side cursor and the grid pulls the next batch near
  the end, stopping when the session reports the store exhausted. The virtualizer re-renders only when
  the visible row range changes (not per scroll pixel), so don't reintroduce scroll-position React
  state. A `pendingReset` ref swaps the result set on a new query without a blank-frame flash; a
  remount whose results already match the session restores them (and scroll) without refetching.
- **Galleries and parts are lazy** — resolved (`store_asset_gallery` / `store_asset_parts`) only when
  a card is hovered/expanded or the split-import dropdown opens.
- **Provider images load through the cache, never a raw CDN URL.** Every `<img>` showing a remote
  thumbnail/preview wraps its src in `cachedImage(url)` (`cachedImage.ts`), which points at the
  `saffron-img://` scheme the bridge serves from the shared `ResourceCache` — fetched once,
  throttled, kept on disk. Loading a provider URL directly bursts the CDN (broken-image tiles) and
  never caches. `GalleryViewer` and `AssetDetailModal` are the only image sites; keep it that way.
- **`StoreWorkspace` is always mounted, gated by an `active` prop** (`active={activeKind === "store"}`
  in `App.tsx`) — like every `ViewTab` body, it keeps its state when hidden rather than unmounting.
  `active` drives the auto-focus and landing-search effects; it does **not** gate the dialogs.
- **The Store dialogs (`AssetDetailModal`, `ProviderModal`) are non-modal and scoped to the Store
  view.** They portal into `StoreWorkspace`'s root region (via `storeOverlay.ts`'s
  `useStoreOverlayContainer`) instead of `document.body`, run `modal={false}`, and bring their own
  backdrop (`DialogScopedOverlay` from `components/ui/dialog`). So they dim only the Store view and
  leave the **main tab strip live and uncovered** — you can switch tabs with a dialog open (their
  `onInteractOutside`/`onPointerDownOutside` are prevented so a tab click doesn't dismiss them). They
  stay open across tab switches, hidden with the view (`display:none` when inactive), so returning to
  the Store shows the same dialog with the same selection. Never revert them to a body-portaled modal
  `Dialog` — that covers the tab strip and locks out navigation.
- **The Store tab restores exactly as left — no rebuild, no re-animate.** Two `display:none` side
  effects are neutralized so a tab switch doesn't _look_ like a re-open: (1) the results grid's
  virtualizer is given a custom `observeElementRect` that ignores `0×0` (`StoreResultsGrid`), so the
  hidden grid doesn't collapse its window and tear down/rebuild cards on reveal — which also keeps a
  card's open detail modal alive; (2) the scoped dialogs omit the CSS _entrance_ animation
  (`DialogContent` gates `animate-in` on `container == null`; `DialogScopedOverlay` carries only the
  exit fade), because `display:none → visible` restarts CSS `animation`, which would replay the open
  animation on every reveal. Keep both: an element that persists across a `display:none` must not use
  an entrance `animation`, and a hidden virtual grid must not trust a `0×0` measurement.
- **The first-provider `ProviderModal` is undismissable until one provider is enabled**
  (`canClose = enabled.length > 0`) — it opens automatically when nothing is enabled and from the gear
  button. `ApiKeyField` is shared with `app/SettingsModal.tsx`.
- **Secrets never round-trip to the webview** — set via `connectorSetSecret`, cleared via
  `connectorClearSecret`, and only ever queried as a boolean. An `oauthLoopback` provider logs in via
  `connectorLogin` (the bridge runs the browser flow and stores the token).
- **External links go through the bridge** (`invoke("open_external", { url })`), never
  `window.open` / `<a target>` — WebKitGTK ignores those (see the parent editor `AGENTS.md`
  browser-APIs rule). Rejected calls surface via `notifyError`, like everywhere else.
