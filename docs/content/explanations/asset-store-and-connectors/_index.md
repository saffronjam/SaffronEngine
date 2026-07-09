+++
title = 'Asset store & connectors'
weight = 15
bookCollapseSection = true
+++

# Asset store & connectors

The editor's **Store** tab is not a hosted marketplace. It is a thin client over external
asset services that already publish CC0 / Creative-Commons content — Poly Haven first, with
more behind the same interface. Each service is a **connector**; a search runs against the one
connector picked from the Store's store dropdown, and importing a result hands the downloaded file
to the engine's existing importer. A solo open-source project cannot run payments, hosting, or
moderation, so it builds none of that — only the search-and-import client.

The connectors live editor-side, in the shell (`editor/shell`), so service calls
never hit browser CORS, credentials never reach the renderer, and provider thumbnails render
straight from their URLs. Only the import crosses to the host, over the control plane.

## Pages

| Page | Covers | Code |
|---|---|---|
| `connector-framework` | the `StoreConnector` trait, the normalized `StoreResult`, single-store search (a store dropdown, scroll-driven paging), credentials + per-project enablement, and the download → `import-model` path | `editor/shell/src/connectors/` · `StoreConnector`, `SearchSession` |
| `oauth-and-sketchfab` | the reusable OAuth loopback capability (implicit flow, fragment-bridge page, CSRF state) and the Sketchfab connector + attribution credits | `editor/shell/src/connectors/oauth_loopback.rs` · `run_loopback_login` |
