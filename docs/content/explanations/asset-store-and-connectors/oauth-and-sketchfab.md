+++
title = 'OAuth loopback & the Sketchfab connector'
weight = 2
+++

# OAuth loopback & the Sketchfab connector

Most connectors are keyless or take a pasted key. Sketchfab — the largest catalog — needs a full
OAuth login, so the framework grows one reusable capability for it: a loopback browser sign-in. It is
provider-agnostic; a connector supplies a descriptor and the capability does the rest.

| What | File | Symbols |
|---|---|---|
| Loopback flow + landing page | `editor/shell/src/connectors/oauth_loopback.rs` | `run_loopback_login`, `OAuthLoopbackConfig` |
| Sketchfab connector | `editor/shell/src/connectors/sketchfab.rs` | `Sketchfab` |
| Login command | `editor/shell/src/store_commands.rs` | `connector_login` |
| Attribution surface | `engine/crates/protocol/src/dto.rs` | `AssetEntryDto.attribution` |

## Why implicit flow over a loopback redirect

Sketchfab offers Authorization Code, Implicit, and Username/Password. Authorization Code needs a
client *secret*, which an open-source desktop app cannot keep, and Sketchfab does not offer PKCE — so
the fit is the **implicit flow with a loopback redirect**, the RFC 8252 native-app pattern that `gh`
and `gcloud` use. Sketchfab explicitly allows `http://127.0.0.1:<port>` redirect URIs.

The flow binds an ephemeral `127.0.0.1` listener (loopback only, never a routable address), opens the
provider's authorize page in the system browser with a random `state`, and waits for the redirect. The
implicit flow returns the token in the URL **fragment**, which the browser never sends to a server — so
the landing page is functional, not decorative: its inline JS reads `location.hash` and `POST`s the
token back to `/callback` on the same loopback origin. The `state` is checked (CSRF), exactly one
callback is accepted, the listener closes, and the token goes to the keyring. The page is fully
self-contained (inline CSS, no external fetches) and styled to match the editor.

Implicit tokens last about a month with no refresh token, so a 401 surfaces as "not configured" and the
editor re-runs the login. The client id is read from `SAFFRON_SKETCHFAB_CLIENT_ID` (no secret is
shipped). Sketchfab is now folding into Epic's Fab, so the standalone API's longevity is a known risk.

## Attribution follows the asset

Importing records the structured license plus author and source URL on the catalog entry (Phase 1), and
`list-assets` now carries that attribution. The Store's **Credits** view lists every imported asset whose
license requires attribution — author, license, source link, and originating store — the one place a
project's CC-BY / Sketchfab credits live. Sketchfab results also show their source so the crediting
requirement is visible at browse time.
