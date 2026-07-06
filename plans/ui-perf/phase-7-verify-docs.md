# Phase 7 — Re-measure, remove instrumentation, document

**Status:** DONE (cleanup + docs). Final re-measure is a user runtime capture (see below).

**Cleanup landed:** deleted `storefront/perfDebug.ts`; removed all `logRender`/`noteInteractive`/
`timeSelectOpen` call sites (`ImportControls`, `StoreResultsGrid`, `StoreWorkspace`), replacing the
store Selects'/parts-menu ad-hoc timing with the permanent `perfLabel` harness
(`perfLabel="provider"|"resolution"|"parts"`); restored `<StrictMode>` in `main.tsx`. `lib/overlayPerf.ts`
kept (dev-gated, zero-cost when off — a lasting affordance like `logRender`).

**Docs landed:** new "Rules that are easy to break" entry in `editor/AGENTS.md` — overlays must be
non-modal on the never-unmounted DOM (the custom-property recalc), the app `Select` is Popover-based,
menus default `modal={false}`, panels are `.contain-panel` boundaries, the root is `overflow:hidden`,
and large grids use one shared overlay per surface.

**Gate (my changes):** frontend `tsc` 0, `oxlint` 0 (on all 14 changed files), `oxfmt` clean, `vite
build` 0; `src-tauri` `cargo check` 0, and `cache.rs` is `rustfmt`- and `clippy`-clean.

> **`just prepare-for-commit` is not crate-wide green — but not because of this work.** `cargo fmt
> --check` flags `lib.rs` / `connectors/ambientcg.rs` / `connectors/polyhaven.rs`, and `cargo clippy -D
> warnings` fails on `wayland_viewport.rs:537` (`field_reassign_with_default`). All are **in-flight /
> pre-existing** files this task did not touch (store-import + viewport work), left alone per the
> concurrent-edit rule. They must be cleared for a clean commit; not this planset's to fix.

**Remaining — user runtime verification (needs the running editor; an agent can't drive the webview):**
1. Fill the before/after `[overlay-perf]` numbers (Phase 1 baseline table) — target < 32 ms on the
   Store page, < 16 ms on Scene; Timeline shows no 3× full-document recalc.
2. Phase 4's `Select` checklist (keyboard nav, every call site, preset labels, disabled).
3. No `saffron-img://` re-request burst on overlay open (Phase 6).

Once the user confirms 1–3, flip `README.md` and each phase to COMPLETED.

Close the loop: prove the gate is met with a fresh capture, strip every temporary hack from the tree,
and record the perf discipline so it does not regress.

## Verify against the gate

Re-run the Phase 1 protocol one final time, on both pages, for every overlay, and fill an **after**
table beside the baseline. Confirm the README success criteria:

- Store page, full grid: any overlay open→paint **< 32 ms**.
- Scene page: **< 16 ms**.
- No `saffron-img://` re-request burst on overlay open.
- Open→paint no longer scales with mounted card count.
- Timeline record shows at most one small, contained style recalc per open — not the 3× full-document
  recalc + 2× forced layout from the baseline.

If any target is missed, identify which lever fell short and loop back to that phase — do not lower the
bar.

## Remove temporary instrumentation

- Delete `storefront/perfDebug.ts`.
- Remove its call sites: `logRender`/`noteInteractive`/`timeSelectOpen` (`onOpenChange`) in
  `ImportControls.tsx`, `StoreResultsGrid.tsx`, `StoreWorkspace.tsx` — **re-read each first**, they are
  under active dialog edits.
- Restore `<StrictMode>` in `main.tsx` (it was disabled only to rule it out).
- Decide on `lib/overlayPerf.ts` (Phase 1): default **keep** — it is dev-gated, zero-cost when off, and a
  genuine debugging affordance like `logRender`. Remove only if it added meaningful surface area.

## Document

- Add a **"Rules that are easy to break"** entry to `editor/AGENTS.md`: overlays must not run
  whole-document modal machinery (scroll-lock / `hideOthers`) — use the shared non-modal, scoped-container
  primitives; hidden tabs/panels are `content-visibility:hidden` + `inert`, never plain `display:none`;
  panels are containment boundaries (`contain: layout style paint`). State *why* (a `<body>` mutation over
  a never-unmounted DOM forces an O(nodes) recalc — the 250–500 ms overlay-open stall this planset fixed).
- If the change alters an engine/editor concept meaningfully, add/adjust the matching `docs/` page per
  the keep-docs-current rule (a short "editor overlay & containment model" note under the editor docs).
- Mark this planset **COMPLETED** in `README.md` and set each phase's `**Status:**` to COMPLETED; delete
  a phase file only after the whole set is COMPLETED (per the `plans/` convention).

## Gate

`just prepare-for-commit` (cargo fmt + clippy `-D warnings` + oxlint + oxfmt) clean; `bun run check` +
`vite build` green; `just engine` green if Phase 6 touched the bridge. Leave all changes **unstaged** for
the user to review and commit.
