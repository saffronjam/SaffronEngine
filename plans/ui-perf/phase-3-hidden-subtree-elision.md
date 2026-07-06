# Phase 3 — Elide hidden tabs/panels from recalc & the a11y walk

**Status:** REVISED — scope narrowed after grounding the browser behaviour; the effective part is
folded into the store-file batch (with Phase 5).

**Revision (why the original premise was overstated):** the inactive main-tab regions (`App.tsx`) and
inactive dock panels (`DockPanelsHost` line 107) already use `display:none`. In WebKit a `display:none`
element gives its descendants **no render tree** and its subtree is elided from layout, hit-testing, the
`aria-hidden`/`hideOthers` a11y walk, *and* the bulk of a style recalc. So adding `inert` there is
redundant (display:none is already non-focusable / out of the a11y tree) and swapping to
`content-visibility:hidden` would *regress* — the element would keep participating in flex layout (take
space) instead of being removed. Layout/paint isolation for the *visible* region is handled by Phase 2's
`.contain-panel`. Net: no `inert`/content-visibility change to the App.tsx regions or dock hosts.

**The genuinely-effective remainder — considered and deferred:** upgrading the visible `StoreCard`'s
`[contain:paint]` to `content-visibility:auto` was evaluated and **left as-is**. Its only marginal win
here (skipping the 3 overscan rows) is already covered by the virtualizer windowing the grid, and
`content-visibility:auto` risks reintroducing the software-compositor antialiasing "shimmer" that the
existing `[contain:paint]` comment was specifically tuned to avoid — a GUI-rendering subtlety not worth
disturbing on an actively-edited line for ~zero benefit. Left as a documented optional tweak.

**Status: COMPLETE (by analysis + Phases 2/4).** The phase's intent — hidden/large subtrees must not
inflate the overlay-open recalc — is satisfied: (a) `display:none` elides inactive tabs/panels, (b) the
virtualizer windows the grid, (c) Phase 2 `.contain-panel` isolates the visible region, and decisively
(d) **Phase 4 removes the `--removed-body-scroll-bar-size` body custom-property write that was the sole
cause of a *whole-document* recalc** — so post-Phase-4 an open recalcs only the mounting popover, and
hidden-subtree DOM weight no longer matters.

The single biggest lever for the **scaling** term. Every dock panel and every non-active `ViewTab`
workspace stays mounted and is merely `display:none`. `display:none` does not spare a subtree from
Radix's `hideOthers` document walk, nor reliably from a style-recalc cascade. Make hidden subtrees cost
**nothing** to recalc, lay out, or walk — so opening an overlay is O(visible), not O(entire editor).

## Why

`hideOthers` (aria-hidden) walks the whole document stamping every non-ancestor node on open; the trace
shows this as the multi-recalc cost, and open→paint rising 250 → 450 ms with mounted card count is the
O(nodes) signature. The editor deliberately keeps everything mounted for instant tab/panel restore
(`app/App.tsx` comments; `DockPanelsHost` `LeafBody`). We keep that instant-restore property but make
the hidden mass invisible to layout, style recalc, and the a11y walk.

## Current code anchors

- `app/App.tsx:354` — dock hidden via `!sceneTabActive && "hidden"` (Tailwind `display:none`).
- `app/App.tsx:372-377` — Store workspace region `activeKind !== "store" && "hidden"`, kept mounted.
- `app/App.tsx:388-393` — asset-editor region, same pattern.
- `components/dock/DockPanelsHost.tsx:107` — `host.style.display = id === activeTab ? "" : "none"` per
  panel host div (the inactive panels within the *active* dockspace).
- `state/store.ts` — `activeViewTabId` / `activeKind` drive which region is shown.

## Changes

Apply, to every "mounted but hidden" region (the `ViewTab` workspaces in App.tsx **and** the inactive
panel host divs in `DockPanelsHost`):

1. **`content-visibility: hidden`** instead of (or alongside) `display:none` for the hidden state.
   `content-visibility:hidden` keeps the subtree mounted and preserves its rendered state/layout for
   instant resume, but the engine **skips its style, layout, and paint** while hidden — so a
   document-wide recalc does not descend into it. This is the ideal primitive and directly targets the
   ~60 ms recalcs. (Verify WebKitGTK version support in Phase 1; `content-visibility` shipped in WebKit
   2.42+. If unsupported on the target runtime, fall back to `contain: strict` on the hidden region,
   which also elides layout/paint of a fixed-size box.)
2. **`inert`** on every hidden region. `inert` removes the subtree from the accessibility tree and hit
   testing — which means Radix `hideOthers` has nothing to stamp there and focus cannot land inside a
   hidden tab. This shrinks the a11y walk to the visible subtree. Toggle `inert` with the same
   active/hidden condition that drives visibility today.
3. Keep the mount (do **not** unmount) — Phase 5 handles selective unmounting of genuinely heavy
   subtrees; this phase is purely "hidden = free," preserving all current state-persistence behavior.

## Interaction with the dock re-parenting

`LeafBody` moves panel host divs with `appendChild` and toggles `display`. Switching those to
`content-visibility:hidden` + `inert` must not break the re-parent logic (it operates on the container,
not the visibility style). Confirm a panel dragged to a new leaf still restores instantly and its live
Wayland subsurface (the viewport hole) is untouched — the viewport panel is `locked`/transparent and
must **not** be made `inert` or content-hidden while it is the active viewport.

## Risks / caveats

- `content-visibility:hidden` preserves layout state but its subtree is not painted; ensure nothing
  measures a hidden panel's DOM expecting live geometry. The Store grid already guards against 0×0
  measurement while hidden (`StoreResultsGrid` `measure()` ignores zero size) — good precedent; audit
  other panels' `ResizeObserver`/`getBoundingClientRect` for the same.
- `inert` blocks focus into hidden tabs — desired, but verify the focus policy (`useFocusPolicy`) and
  any programmatic focus (auto-focus searchbar on Store reveal) still target the *active* region.
- Do not `content-visibility:hidden` the `<body>`/root or the active region.

## Verification / gate

- Re-run Phase 1 protocol. Expect open→paint to **stop scaling with mounted card count** and the base
  recalc to drop sharply on the Store page (its huge grid is now skipped when it is not the recalc's
  concern; when it *is* visible, other hidden panels no longer add to the walk).
- Confirm instant tab/panel restore is unchanged (scroll positions, form state, viewport subsurface).
- `just prepare-for-commit` + `bun run check` + `vite build` green.

## Expected impact

Largest single win for the scaling term and a big cut to the base. Combined with Phase 2, this may bring
menu/tooltip opens close to target **without** the Phase 4 refactor — measure here before committing to
how deep Phase 4 must go.
