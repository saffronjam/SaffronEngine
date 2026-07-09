# connectors — the Asset Store backend

The editor-local backend for the in-editor **Asset Store**: connectors call external asset
services (Poly Haven, ambientCG, Poly Pizza, Sketchfab) over HTTP, normalize each provider's
response onto one canonical result shape, and download a deliverable to a local path the host
importer reads. This module is the **only** component in the product that makes outbound HTTP
requests — the engine crates make none (no `reqwest` dependency anywhere under `engine/`). The
frontend half is `editor/src/storefront/` (its own `AGENTS.md`); the two are bridged by the
`store_*` / `connector_*` shell commands in `editor/shell/src/store_commands.rs`.

## Files

```
mod.rs           the framework: the StoreConnector trait; the normalized DTOs (StoreResult,
                 StoreImportDescriptor, AssetPart, StoreLicense, StoreKind, AuthKind, …);
                 ConnectorRuntime (shell-managed registry + resource cache + live search
                 sessions); ProgressFn + extract_zip
cache.rs         ResourceCache — the ONE outbound-HTTP + disk-cache + throttle layer (see below)
registry.rs      ConnectorRegistry — the fixed set of connectors the editor knows about
session.rs       SearchSession — a scroll-driven cursor over the one selected store
credentials.rs   the OS-keyring credential store (secrets never touch the project file)
oauth_loopback.rs a reusable RFC 8252 loopback OAuth (implicit-flow) login
polyhaven.rs · ambientcg.rs · polypizza.rs · sketchfab.rs   one StoreConnector each
```

## Add a provider

Implement `trait StoreConnector` (`mod.rs`) and add it to the `vec!` in `ConnectorRegistry::new`
(`registry.rs`) — that fixed list is the whole registry. A connector maps its provider's API onto
the canonical DTOs and downloads deliverables:

- `search(query, cursor) -> SearchPage` — one page of `StoreResult`s plus an opaque `next_cursor`.
- `download(descriptor, progress) -> PathBuf` — the whole-asset deliverable, reporting a 0.0–1.0
  fraction as bytes arrive.
- `gallery` / `parts` / `download_part` are optional (defaulted) — override them for assets with
  preview galleries or individually-selectable maps/meshes.

Everything a connector returns is **normalized**: never leak a provider-shaped struct past the
trait. Errors are the typed `ConnectorError` enum (no stringly errors, per the workspace rule); a
missing/expired credential maps to `ConnectorError::NotConfigured`.

## Auth & secrets

`AuthKind` is the one switch the framework branches on: `None` (a unique `User-Agent` only — Poly
Haven, ambientCG), `ApiKey` (a pasted key — Poly Pizza), `OauthLoopback` (a browser login —
Sketchfab). Secrets live **only** in the OS keyring (`credentials.rs`, service `saffron-anima`,
account = connector id), machine/user-global, **never** in the project file; the webview only ever
sees a presence boolean. Env hooks:

- `SAFFRON_NO_KEYRING` — force the in-memory backend (the toolbox / CI / e2e case, where no Secret
  Service daemon is reachable); the store degrades gracefully.
- `SAFFRON_SECRET_<ID>` — inject a secret for connector `<ID>` (uppercased, `-`→`_`); always takes
  precedence over the keyring (test/CI seam).
- the OAuth `client_id` is read from a `SAFFRON_*_CLIENT_ID` env var (e.g.
  `SAFFRON_SKETCHFAB_CLIENT_ID`); absent it, login fails with `NotConfigured`.

`oauth_loopback.rs` binds an ephemeral `127.0.0.1` listener, opens the provider's authorize page in
the system browser, and serves a self-contained landing page whose inline JS reads the URL fragment
(where the implicit flow returns the token) and `POST`s it back; `state` is validated (CSRF), one
callback is accepted, then the token is written to the keyring.

## Conventions

- **camelCase serde** on every wire DTO (`#[serde(rename_all = "camelCase")]`); the shapes mirror
  `editor/src/storefront/types.ts` field-for-field — change one, change both.
- **Structured licenses, never a free string** (`StoreLicense { id, requires_attribution, url }`) so
  attribution can be enforced at import; default to an attribution-safe license when a provider is
  vague.
- **Everything remote goes through `ResourceCache` — never a bespoke client, folder, or download.**
  A connector holds an `Arc<ResourceCache>` and uses `cache.client()` for **dynamic** calls (search
  listings, download manifests, HEAD probes) and the cache's fetch/store methods for anything
  cacheable. The bridge then routes the returned path to the host importer by `StoreKind` (a
  material map set ships as a flat, role-named zip the host's `import_material_folder` scans).
- A search runs against **one store** (`SearchQuery.provider`, resolved by `ConnectorRuntime::
  start_session` via `ConnectorRegistry::by_id`); there is no cross-provider merge. Per-project
  enablement (which stores the dropdown offers) is control-plane state the editor owns, not the
  registry — the registry knows every connector and resolves the one a search names.

## The resource cache (`cache.rs`)

`ResourceCache` is the **one** place the bridge fetches a remote resource, keeps it on disk
(rooted at `appdata/cache/`, so it **survives restarts** — not the temp dir), and serves it back.
Anything that needs "fetch a URL once, keep it, serve it" routes through it; do not mint a second
cache or a per-feature directory.

- `client()` — the shared `reqwest::Client` (User-Agent baked in) for **uncacheable** dynamic calls.
- `bytes(url)` — content-addressed blob (bytes + content type), fetch-once. Backs the
  `saffron-img://` image scheme (registered in `lib.rs`) that thumbnails/gallery previews load
  through, so a screenful of tiles never stampedes a provider CDN.
- `file(url, ext, progress)` / `file_keyed(key, url, ext, progress)` — a cached single file by url,
  or by an explicit key when the url is signed/ephemeral (Sketchfab archives keyed by model uid).
- `fetch(url, progress)` — throttled, **un**-cached bytes for content materialized elsewhere.
- `derived(key)` — a materialized directory (extracted map set, multi-file glTF) built once into a
  `.partial` sibling and atomically renamed in, so a crashed build never looks cached.

Each blob carries a `{key}.meta.json` sidecar (source url, ext, content type, fetched-at). A
bounded `Semaphore` caps upstream concurrency; only cache misses take a permit.

## Trap

Some module/phase doc-comments still describe **unbuilt** or since-changed work (e.g. "Phase 1
enables Poly Haven by default"). Trust the code, not the phase markers — the registry already wires
all four providers and enablement is per-project.
