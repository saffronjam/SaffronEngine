# Phase 1 — Measurement harness & baseline

**Status:** CODE COMPLETE — baseline table pending a user runtime capture (WebKit Timeline needs the
running editor). `lib/overlayPerf.ts` provides `measureOverlayOpen(label)` + `withOverlayPerf(label,
onOpenChange)`; the `perfLabel?: string` prop is wired into the `Dialog`, `DropdownMenu`, `Popover`,
`ContextMenu`, and `Tooltip` Root wrappers (Select gets it in the Phase 4 rewrite). Dev-gated, no-op
when dev mode is off. `tsc` clean, oxlint clean (the lone `slider.tsx` warning is pre-existing).

Every later phase is judged by "did open→paint drop." So first make that number **reproducible and
comparable**, replace the ad-hoc investigation hacks with one clean dev-gated probe, and record a
baseline table across overlay types and pages. Without this, "faster" is a feeling.

## Why first

The whole planset is an optimization with a hard success gate (README). We must be able to re-run the
exact same measurement before and after each phase, on both the Store page (worst case) and the Scene
page (baseline), for each overlay primitive. The investigation's `perfDebug.ts` timing (double-rAF
open→paint) works but is bolted onto the Store `Select`s only and is entangled with `logRender` /
`noteInteractive`; generalize it into one reusable helper any overlay can call.

## Current code anchors

- `storefront/perfDebug.ts` — the throwaway `timeSelectOpen()` (double-rAF open→paint) + `noteInteractive`.
- `storefront/ImportControls.tsx`, `StoreWorkspace.tsx` — `onOpenChange={(o) => o && timeSelectOpen(...)}`.
- `lib/renderLog.ts` — the existing dev-mode-gated `logRender` pattern (console flush, `getState()` so
  it never subscribes). Mirror its shape: dev-gated, zero cost when off, never triggers a re-render.
- `state/store.ts` — `devMode` flag (five-click footer gesture / `VITE_SAFFRON_DEV_MODE=1`).

## Changes

1. Add `lib/overlayPerf.ts` — a dev-mode-gated `measureOverlayOpen(label: string)` that stamps
   `performance.now()` and logs elapsed on the second `requestAnimationFrame` (open→first-paint proxy),
   prefixed `[overlay-perf] <label> open→paint <ms>`. No-op unless `devMode`. Reads `getState()`, never
   subscribes.
2. Wire it into the shared primitives (`components/ui/select.tsx`, `dropdown-menu.tsx`, `tooltip.tsx`,
   `context-menu.tsx`, `dialog.tsx`) behind an opt-in `data-perf-label` prop on the Root/Content, so any
   overlay in the app can be timed by adding one attribute — not just the Store `Select`s. Default off,
   so shipped call sites are untouched.
3. Keep it **dev-only and additive** — this helper is allowed to survive past Phase 7 (it is a genuine
   debugging affordance like `logRender`), unlike `perfDebug.ts` which is deleted. Decide at Phase 7
   whether to keep it; default: keep, since it is cheap and gated.

## Baseline capture protocol (record in this file when run)

For each of {Scene page, Store page with a full ambientCG grid}, and each overlay {provider Select,
resolution Select, parts DropdownMenu, a card Tooltip, a right-click ContextMenu, the detail Dialog}:

1. `just run` (hardware webview path), open the WebKit Web Inspector → Timeline.
2. Enable dev mode; open each overlay 5×; record median `[overlay-perf]` ms.
3. For one representative open, expand the Timeline record and note the split (Recalc Styles ms / Forced
   Layout ms / Scripting ms) — this is the ground-truth attribution each phase must move.
4. Note mounted card count for the Store captures (the scaling variable).

Baseline table (fill in):

| Page | Overlay | median open→paint | recalc ms | layout ms | mounted cards |
|---|---|---|---|---|---|
| Scene | … | | | | — |
| Store | provider Select | | | | |
| … | | | | | |

## Verification / gate

- `bun run check` + `oxlint` clean; helper is a no-op with dev mode off (verify no `[overlay-perf]`
  output in a normal run).
- A filled baseline table committed into this phase file. This *is* the deliverable — no engine gate
  needed (frontend-only, no Rust change).

## Notes

- Do **not** remove `perfDebug.ts` yet — Phase 7 owns cleanup, and keeping it lets us cross-check the
  new helper against the numbers already gathered during the investigation.
- Store files are under active dialog edits — re-read before wiring the `data-perf-label` into any
  storefront call site (see README coordination note).
