# connectors — the Asset Store backend

The editor-local backend for the in-editor **Asset Store**: connectors call external asset
services (Poly Haven, ambientCG, Poly Pizza, Sketchfab) over HTTP, normalize each provider's
response onto one canonical result shape, and download a deliverable to a local path the host
importer reads. This module is the **only** component in the product that makes outbound HTTP
requests — the engine crates make none (no `reqwest` dependency anywhere under `engine/`). The
frontend half is `editor/src/storefront/` (its own `AGENTS.md`); the two are bridged by the
`store_*` / `connector_*` Tauri commands in `editor/src-tauri/src/lib.rs`.

## Files

```
mod.rs           the framework: the StoreConnector trait; the normalized DTOs (StoreResult,
                 StoreImportDescriptor, AssetPart, StoreLicense, StoreKind, AuthKind, …);
                 ConnectorRuntime (Tauri-managed registry + live search sessions); and the
                 shared HTTP helpers (stream_get, cached_fetch, extract_zip, store_cache_dir)
registry.rs      ConnectorRegistry — the fixed set of connectors the editor knows about
aggregator.rs    SearchSession — a round-robin multi-provider search cursor (no ranking)
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
- **Download to a temp cache, then hand off by `StoreKind`.** Deliverables land under
  `store_cache_dir()` (content-addressed via `cached_fetch`); the bridge routes the path to the host
  importer by kind (a material map set ships as a flat, role-named zip that the host's
  `import_material_folder` scans by filename).
- The aggregator is **round-robin with no cross-provider ranking**; per-project enablement is applied
  by the caller via the `providers` scope on a search (the editor passes the project's enabled set),
  not inside the registry.

## Trap

Some module/phase doc-comments still describe **unbuilt** or since-changed work (e.g. "Phase 1
enables Poly Haven by default"). Trust the code, not the phase markers — the registry already wires
all four providers and enablement is per-project.
