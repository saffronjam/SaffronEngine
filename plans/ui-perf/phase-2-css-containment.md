# Phase 2 — CSS containment + scroll-lock no-op

**Status:** CODE COMPLETE (runtime re-measure pending). `styles.css`: `html,body,#root { overflow:hidden }`
so the root never scrolls → `react-remove-scroll`'s body write measures a zero gap and adds no
compensating padding (no layout shift on overlay open). Added a `.contain-panel { contain: layout paint }`
utility and applied it to every dock panel host (`DockPanelsHost.hostFor`) and the three kept-mounted
main-tab regions (`App.tsx`: scene dock, store, asset editor). `size` omitted (panels flex); `style`
omitted (it only scopes counters and does not stop the stylesheet-insertion recalc — that is Phase 4).
`tsc` clean, oxlint clean. `scrollbar-gutter` proved unnecessary once the root is `overflow:hidden`.

Two low-risk, no-behavior-change CSS levers that directly attack the ~60 ms-per-recalc and the forced
layout: (a) make the root non-scrollable so `react-remove-scroll`'s `<body>` write is a **no-op**
instead of a real layout shift, and (b) wrap each opaque panel region in a **containment boundary** so a
`<body>`-level style invalidation cannot recascade style/layout into it.

## Why

From the trace, each full-document style recalc is ~60 ms and each forced layout ~28 ms. Two cheap CSS
facts shrink both:

- **RC-3 — the root scrolls, so the scroll-lock actually shifts layout.** `styles.css:103-108` sets
  `html, body, #root { height:100% }` but never `overflow:hidden` and never `scrollbar-gutter`. When
  RemoveScroll writes `overflow:hidden` + gap-compensation padding to `<body>` on every overlay open,
  the body genuinely transitions (the classic "fixed header jumps on menu open"). If the document root
  never scrolled in the first place, that write invalidates far less. This app is a fixed full-viewport
  desktop shell — the root should never scroll; only inner panes do.
- **`contain` stops the cascade.** CSS `contain: layout style` (and `paint`) on a subtree tells the
  engine that style/layout changes outside cannot affect inside and vice-versa, so a body-level
  invalidation stops at the boundary instead of re-styling and re-laying-out the whole editor. Store
  cards already use `[contain:paint]` (`StoreResultsGrid.tsx` `StoreCard`) — that helps *paint* but not
  *style/layout*, which is exactly the gap the trace shows. Extend to `layout style paint` at the panel
  level (coarse boundaries, not per-card, to keep the containment count low).

## Current code anchors

- `styles.css:103-108` — `html, body, #root { height:100%; margin:0 }`; no overflow/gutter.
- `styles.css:110-123` — `body` is intentionally transparent (viewport hole); do not add a background.
- `components/dock/DockPanelsHost.tsx` `LeafBody` — the per-leaf container that holds panel host divs.
- `app/App.tsx:345,354,372,388` — the shell root, the dock container, and each hidden `ViewTab` region.
- `StoreResultsGrid.tsx` `StoreCard` — existing `[contain:paint]` (keep; do not widen per-card).

## Changes

1. `styles.css`: add to the `html, body` base rule `overflow: hidden` and `scrollbar-gutter: stable`.
   Verify the app already relies on inner `overflow-auto` panes (it does — e.g. `StoreResultsGrid`'s
   `absolute inset-0 overflow-auto`); the root itself is not meant to scroll. Keep `#root` at
   `height:100%` and transparent.
2. Add `contain: layout style paint` (Tailwind arbitrary `[contain:layout_style_paint]` or a small
   utility class) to the **opaque panel boundary** — the `LeafBody` container and each top-level
   `ViewTab` workspace region (Store/asset-editor/material-graph/etc.). These are natural containment
   boundaries: a panel's internals never affect siblings' layout.
3. Do **not** add `contain: size` (panels must size to their flex parent). Do not contain the transparent
   `viewport-host` (it is a positioning hole).

## Risks / caveats

- `contain: layout` establishes a containing block for absolutely-positioned descendants and a new
  stacking context. Audit that no panel relies on an `absolute` child escaping its panel (portaled
  overlays are unaffected — they render at the app root, not inside the panel). The dock's re-parenting
  (`appendChild`) is DOM-level and unaffected by containment.
- `scrollbar-gutter: stable` + `overflow:hidden` on the root is belt-and-suspenders; confirm no
  double-gutter on inner scrollers. WebKitGTK supports `scrollbar-gutter` (verify version in Phase 1).
- Overlays that must be wider than their panel (a `Select` popper, a tooltip) portal to the app root and
  are **not** clipped by panel `contain:paint` — confirm in the capture that no popup is cut off.

## Verification / gate

- Re-run the Phase 1 protocol. Expect the **Forced Layout** portion to shrink materially (the body write
  becomes a no-op) and each recalc to drop where a big hidden region is now contained.
- Visually confirm: no layout shift on overlay open, no clipped popups, panels still scroll internally,
  no double scrollbars, viewport hole still transparent.
- `just prepare-for-commit` clean; `bun run check` + `vite build` green.

## Expected impact

Removes the layout-shift share of the base (~tens of ms) and prevents recascade into contained panels.
On its own it will *not* fully fix the O(nodes) `hideOthers` walk — that is Phase 3 — but it is the
cheapest, safest first cut and de-risks the deeper phases.
