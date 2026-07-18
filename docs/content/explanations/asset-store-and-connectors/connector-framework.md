+++
title = 'Connector framework'
weight = 1
+++

# Connector framework

A connector is one external asset service the editor's Store tab can search and import from. The
framework behind them has three jobs: present every service through one normalized result shape,
search the single store picked from the Store's dropdown, and turn a chosen result into a catalog
asset via the host's import commands.

Connectors live in the editor shell (`editor/shell/src/connectors/`), not the webview and not
the engine. Native HTTP avoids browser CORS, credentials stay out of the renderer, and this module
is the product's only outbound-HTTP surface — the engine crates make no network requests. Only the
final import crosses to the host, over the control plane.

## One trait, one result shape

A connector implements `StoreConnector` and joins the fixed list in `ConnectorRegistry::new`:
[Poly Haven](https://polyhaven.com), [ambientCG](https://ambientcg.com),
[Poly Pizza](https://poly.pizza), and [Sketchfab](https://sketchfab.com).

```rust
#[async_trait]
pub trait StoreConnector: Send + Sync {
    fn id(&self) -> &'static str;
    fn auth_kind(&self) -> AuthKind;
    async fn search(&self, query: &SearchQuery, cursor: Option<StoreCursor>)
        -> Result<SearchPage, ConnectorError>;
    async fn download(&self, descriptor: &StoreImportDescriptor, progress: &ProgressFn)
        -> Result<PathBuf, ConnectorError>;
    // Defaulted capabilities: gallery(), parts(), download_part().
}
```

Every provider response maps onto the canonical `StoreResult`; no provider-shaped struct leaks
past the trait. Two fields do most of the work. `kind` (`model` / `hdri` / `material` / `texture`)
selects the host importer, and `license` is structured (`StoreLicense { id,
requiresAttribution, url }`, never a free string), so
[CC0](https://creativecommons.org/publicdomain/zero/1.0/) versus
[CC-BY](https://creativecommons.org/licenses/by/4.0/) is machine-readable and attribution can be
enforced at import.

`auth_kind` is the one switch the rest of the framework branches on:

| `AuthKind` | Credential | Connectors |
|---|---|---|
| `none` | a unique `User-Agent` header only | Poly Haven, ambientCG |
| `api_key` | a pasted key in the OS keyring | Poly Pizza |
| `oauth_loopback` | a browser sign-in token | Sketchfab |

## Enablement is project state, secrets are machine state

Which connectors a project uses is shared, committed state: a `stores` block in `project.json`
(the `ProjectSidecar.stores` value), read and written over the control plane. A teammate who opens
the project sees the same enabled stores.

```sh
sa get-stores -o json
# { "enabled": ["polyhaven", "ambientcg"] }
sa set-stores --enabled '["polyhaven", "poly-pizza"]'
```

A connector's secret is the opposite: machine-local and never committed. It lives in the OS
keyring under service `saffron-anima` with the connector id as the account, readable only in the
bridge; the webview can set, clear, or query the presence of a key, never its value. So the
enabled set travels with the project while each teammate enters their own key.

When `SAFFRON_NO_KEYRING` is set or no Secret Service answers (the toolbox and CI case), the
credential store degrades to an in-memory map, and `SAFFRON_SECRET_<ID>` injects a secret for
tests, so a headless run boots without a daemon. Opening the Store with nothing enabled auto-opens
the provider modal, which cannot be dismissed until one provider is enabled.

## One store at a time

Services rank their catalogs differently, so a merged grid has no principled global order. The
Store does not merge: a dropdown left of the search bar picks which enabled connector to search,
and results arrive in that store's own order. Switching the dropdown re-runs the query against the
new store. The selected store and last query persist in localStorage, and disabling the store
being viewed auto-selects another enabled one.

Paging is scroll-driven rather than numbered, because heterogeneous services cannot share a page
number. A `SearchSession` holds the connector's opaque cursor and exhaustion flag and refills a
small buffer as the grid's scroll nears the end; a failed fetch marks the session exhausted rather
than leaving the grid spinning. A search fires only on a committed query (Enter or a chip commit
in the shared searchbar), never per keystroke.

## Import

Importing downloads the deliverable to a local file, then calls one host import command per
deliverable shape, shared by every connector:

| `kind` | Deliverable | Host command | Engine path |
|---|---|---|---|
| `model` | a glTF file set | `import-model` | `import_model` → `.smodel` container |
| `material`, `texture` | a folder of role-named PBR maps | `material-import` | `import_material_folder` → `.smatx` container |
| `hdri` | one `.hdr` | `import-texture` with role `hdri` | `import_texture`, uploaded as HDR |

`import-model` and `material-import` take an optional `attribution` (an `AssetAttributionDto`:
license id and URL, the requires-attribution flag, author, source URL, store id), recorded on
the catalog entry so a CC-BY credit travels with the asset. `import-texture` takes a
`role` hint instead of attribution; the engine derives the upload colorspace from it
(`colorspace_for_role_explicit`: albedo and emissive as sRGB, HDRI as HDR, every data map linear).

## Per-part import

A result whose `has_parts` is set exposes its constituent files through `parts()`, and the card's
Import button becomes a split button: the main action imports the whole asset, the dropdown lists
the individual maps and `download_part()` fetches one at the chosen resolution. A single map then
imports through `import-texture` with the part's role, so a normal map uploads linear and an
albedo uploads sRGB.

Each provider fills the capability from what its API offers. Poly Haven's `/files` endpoint lists
every map as its own URL, so a part is a direct fetch. ambientCG knows the map roles but ships a
per-resolution zip, so a part carries a `bundle`: `download_part` fetches the zip once (cached)
and extracts it into a cached folder reused by every map picked from it. Poly Pizza and Sketchfab
expose no parts and show a plain Import button.

## Gallery

`gallery()` returns an asset's preview images and defaults to the card thumbnail. Like `parts()`
it resolves lazily: the webview calls `store_asset_gallery` only when a card is hovered or its
detail modal opens, so scrolling a hundred cards fires no gallery requests. The card and the modal
share one `useGallery` fetch but keep separate slide indices, so navigating the modal does not
move the card behind it.

Poly Haven overrides the default with the hero render (plus a 1024 px `full_url` for the modal),
the site's orthographic and clay renders, and one image per map. The renders live at predictable
CDN paths the JSON API does not list, so each is HEAD-probed and included only when it exists.

## One cache for every fetch

Every remote fetch, from thumbnails to deliverable downloads, goes through the shared
`ResourceCache`, rooted at `appdata/cache/` so it survives restarts. Single files are blobs keyed
by a hash of the URL, or by an explicit key when the URL is signed and ephemeral (a Sketchfab
archive, keyed by model uid). Each blob carries a `{key}.meta.json` sidecar recording its source
URL, content type, and fetch time. Multi-file deliverables materialize as `derived/` directories
built in a `.partial` sibling and atomically renamed into place, so a crashed build never looks
cached.

A bounded semaphore (6 permits) caps upstream fetches; cache hits take no permit. Thumbnails ride
the same path: the webview loads `saffron-img://fetch/?u=<provider-url>`, a custom scheme the
shell serves from the cache, so a screenful of tiles drains through the gate instead of
stampeding a provider CDN. An in-RAM LRU in front of the disk blobs answers repeat requests
without touching disk, and responses carry `Cache-Control: immutable` so Chromium's own cache
absorbs repaints.

A connector never owns storage of its own: it holds an `Arc<ResourceCache>` and uses `client()`
for dynamic API calls (search listings, manifests, HEAD probes) or the cache's fetch and store
methods for anything cacheable.

## In the code

| What | File | Symbols |
|---|---|---|
| Trait + normalized types | `editor/shell/src/connectors/mod.rs` | `StoreConnector`, `StoreResult`, `StoreLicense`, `AuthKind`, `ConnectorRuntime` |
| Registry | `registry.rs` | `ConnectorRegistry`, `ConnectorInfo` |
| Search session | `session.rs` | `SearchSession`, `next_batch` |
| Credentials | `credentials.rs` | `Credentials` |
| Resource cache | `cache.rs` | `ResourceCache`, `Derived` |
| Shell commands | `editor/shell/src/store_commands.rs` | `store_import`, `store_import_part`, `store_asset_gallery` |
| Image scheme | `editor/shell/src/scheme.rs` | the `saffron-img` scheme |
| Store tab + gallery hook | `StoreWorkspace.tsx`, `useGallery.ts` | `StoreWorkspace`, `useGallery` |
| Per-project enablement | `engine/crates/assets/src/project.rs`, `commands_asset.rs` | `ProjectSidecar`, `get-stores`, `set-stores` |
| Import commands | `engine/crates/control/src/commands_asset.rs` | `import-model`, `material-import`, `import-texture` |

## Related

- [OAuth loopback and Sketchfab](../oauth-and-sketchfab/) — the browser-login capability and the Credits view
- [Asset server and catalog](../../geometry-and-assets/asset-server-and-catalog/) — where an import lands
- [Import pipeline](../../geometry-and-assets/import-pipeline/) — the host-side model bake
- [Native materials](../../materials-and-pipelines/native-materials/) — the material assets a map folder becomes
- [Editor shell and viewport bridge](../../ui-and-editor/editor-shell-and-viewport-bridge/) — the shell process the connectors run in
