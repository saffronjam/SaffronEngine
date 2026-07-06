# Phase 6 — Cache `saffron-img://` so a repaint doesn't re-request

**Status:** CODE COMPLETE (runtime re-measure pending). Added a bounded (64 MiB) in-memory LRU to
`ResourceCache` (`src-tauri/src/connectors/cache.rs`): `bytes()` now serves repeat requests from RAM
(`mem_get`/`mem_put`, LRU-evicted) instead of `std::fs::read`-ing the blob on every webview request —
so WebKit re-requesting each visible thumbnail on a repaint no longer hits the disk N× per repaint. A
miss still falls through to the on-disk blob (unchanged). `cargo check` clean; `cargo clippy -D
warnings` reports **no** issue in `cache.rs`.

> **Pre-existing gate blocker (NOT this work):** `cargo clippy -D warnings` fails on
> `src-tauri/src/wayland_viewport.rs:537` (`field_reassign_with_default` — `let mut state =
> State::default(); state.refresh_out = …`). That file is **unmodified** (`git status` clean for it),
> so this is pre-existing debt in the viewport/camera area currently under active work — left
> untouched per the "don't fix others' work / stay scoped" rule. It must be cleared (a one-line
> `State { refresh_out, ..Default::default() }`) for Phase 7's clippy gate to pass; flagging for the
> user to fix or authorize.

Kill the "Network Requests" burst that fires every time an overlay opens over the Store grid. It is a
downstream symptom of the repaint (fixed by Phases 2–4 reducing repaints), but the underlying defect —
custom-scheme responses that WebKit never caches — is worth fixing on its own: it also bites on scroll,
resize, and tab-return.

## Why

`storefront/cachedImage.ts` routes every remote thumbnail through `saffron-img://fetch/?u=<url>`, served
by the async URI-scheme handler registered at `src-tauri/src/lib.rs:1253`. WebKit's
`WebKitURISchemeRequest` responses **bypass the network/resource cache and ignore `Cache-Control`**, so
the `public, max-age=…, immutable` header set at `src-tauri/src/lib.rs:1273` is inert. When a
full-document repaint re-touches the visible `loading="lazy"` `<img>`s (and WebKitGTK, under its
`MemoryPressureHandler`, has evicted their decoded surfaces), WebKit re-issues the load → the Rust
handler is re-invoked per visible thumbnail → the blue burst that scales with visible card count.

The name `cachedImage`/`ResourceCache` refers to the **bridge's on-disk fetch cache** (avoids
re-hitting the provider CDN) — real and working. The gap is the **webview-side** cache: WebKit re-asks
the bridge for bytes it already has, and even a fast bridge round-trip × N images on the main webview
work queue is a stall.

## Current code anchors

- `storefront/cachedImage.ts:15` — deterministic `saffron-img://fetch/?u=…` URL (confirmed stable; not
  a cache-buster).
- `storefront/GalleryViewer.tsx:56,58` and `AssetDetailModal.tsx:83,86` — `<img loading="lazy" src=…>`.
- `src-tauri/src/lib.rs:1253` — `register_asynchronous_uri_scheme` (or equivalent) for `saffron-img`.
- `src-tauri/src/lib.rs:1273` — the inert `Cache-Control: immutable` header.
- `editor/AGENTS.md` custom-scheme / bridge notes; `storefront/AGENTS.md` image-cache rule.

## Options (pick the modern correct one; likely a combination)

1. **Serve decoded, keep-alive bytes without re-reading disk per request** — ensure the handler answers
   from an in-memory LRU of already-fetched bytes so a re-request is cheap even if WebKit re-asks. This
   bounds the cost regardless of WebKit's caching.
2. **Reduce re-requests at the source** — Phases 2–4 stop the whole-document repaint that re-touches the
   `<img>`s, so the burst largely disappears on overlay-open. Verify how much remains on scroll/resize.
3. **Hand the webview cacheable resources** — investigate whether registering the scheme as a
   *standard/secure* scheme (or serving via the Tauri asset protocol with range + validators) lets
   WebKitGTK cache decoded images across repaints. If WebKitGTK genuinely never caches custom-scheme
   responses, prefer option 1 + reducing eviction pressure (Phase 5 lowers memory pressure).
4. **Prevent decoded-surface eviction for on-screen thumbnails** — `decoding="sync"` /
   `fetchpriority`/pinning strategies; measure whether it helps under WebKitGTK's memory handler.

Confirm the actual WebKitGTK caching behavior for the chosen scheme registration before committing —
this is a runtime fact to verify, not assume.

## Risks / caveats

- Changing scheme registration or headers is a `src-tauri` (Rust bridge) change — gate with
  `just engine`/`cargo clippy -D warnings` in addition to the frontend build.
- An in-memory LRU must be bounded (memory pressure is already a factor); size it and evict sanely.
- Do not change `cachedImage()`'s URL shape casually — it is deterministic by design; a nonce would
  re-introduce the re-fetch it was built to avoid.

## Verification / gate

- Re-run Phase 1 protocol with the Network lane visible: opening an overlay over a full grid shows **no**
  `saffron-img://` re-request burst for already-onscreen thumbnails; scroll/resize re-requests are
  bounded/cheap.
- `just engine` (Rust bridge) + `just prepare-for-commit` + `bun run check` + `vite build` green.

## Expected impact

Eliminates the network burst and its main-thread contention. Independent of the layout phases — can land
in parallel — but lower priority since Phases 2–4 already suppress the overlay-open repaint that triggers
it.
