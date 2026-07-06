# Phase 5 — Reduce always-mounted DOM weight

**Status:** CODE COMPLETE (runtime verification of the tooltip pending). Shared-tooltip landed; the
other candidates verified already-handled by the architecture.

**What landed / was verified:**
- **Per-card Tooltip roots → one shared tooltip per surface** (`StoreResultsGrid.tsx`), per the
  AGENTS.md "one shared overlay per surface, not a Radix root per row" rule. A single grid-level
  `Tooltip` is anchored to a fixed-position, non-interactive box moved onto the hovered card title via
  one delegated `onPointerOver` (`closest("[data-card-title]")`); it now shows **only when the title is
  actually truncated** (also satisfies "a tooltip must add information"). Removes N Radix Tooltip roots
  from the visible grid and their per-virtual-row reconciliation churn on scroll. tsc + oxlint clean.
- **React Flow material graph, image viewer, flamegraph** — verified they are conditionally rendered in
  `App.tsx` (`activeKind === …`) and therefore **already unmount when inactive**; no change needed.
- **Kept-mounted views** (scene dock, store, asset editor) stay mounted by design for instant
  state-restore and are `display:none` when hidden (elided from recalc) — correct as-is; not unmounted.

**Note on impact for the reported bug:** a *closed* Radix Tooltip mounts no portal DOM, so removing the
per-card roots does not shrink the overlay-open style-recalc (Phase 4 is what fixes that); this change
is correctness-per-house-rules + smoother scroll reconciliation. Runtime verify: hovering a truncated
store-card title shows the full name; non-truncated titles show nothing.

Every pass whose cost is O(mounted nodes) — style recalc, layout, the a11y walk — gets cheaper if there
are simply fewer nodes mounted. Phases 2–3 make hidden nodes *free to recalc*; this phase trims the
*count* where keeping a subtree mounted buys nothing, and removes duplicated Radix roots.

## Why

The editor keeps everything mounted for instant restore. That is right for cheap panels (Inspector,
Hierarchy) but wasteful for heavy ones whose state is either reconstructable or persisted elsewhere.
Fewer mounted nodes shrinks the constant factor on every layout/recalc/walk and lowers webview memory
pressure (which also feeds the Phase 6 thumbnail-eviction burst).

## Candidates (measure each; only cut where it pays)

1. **Per-card Radix `Tooltip` root in the Store grid** (`StoreResultsGrid.tsx:247-254`) — one `Tooltip`
   Root per card × dozens of visible cards. Replace with **one shared tooltip per grid surface** driven
   by a `data-*` id + `closest()` on hover (the pattern already mandated for context menus and asset
   tiles in `editor/AGENTS.md` "ONE shared context menu per surface, not a Radix root per row"). This
   removes N provider roots from the visible grid.
2. **React Flow material graph** (`@xyflow/react`, `MaterialGraphWorkspace`) — currently conditionally
   rendered (`activeKind === "materialGraph"`), so likely already unmounted when inactive; **verify**. If
   any large graph subtree lingers mounted, ensure it unmounts (its model persists in the store per the
   snapshot-history design).
3. **Heavy `ViewTab` workspaces that persist only for convenience** — decide per tab whether "instant
   restore" justifies staying mounted vs. remounting from persisted store state. The Store tab has a
   real reason (live backend session + scroll); the asset editor has embedded viewport state. Others may
   not. Do **not** regress the documented keep-mounted behaviors (Store search/results/scroll; asset
   editor suspend/resume) — only unmount where state is fully reconstructable.
4. **Deep static class strings / decorative wrappers** — minor; note but do not over-optimize.

## Current code anchors

- `StoreResultsGrid.tsx:247` — per-card `<Tooltip>`.
- `app/App.tsx:366-393` — the conditionally-rendered vs kept-mounted workspaces (imageViewer/flamegraph
  are conditional; store/assetEditor are kept mounted).
- `editor/AGENTS.md` — "ONE shared context menu per surface" and "large list re-renders only changed
  rows" rules; the shared-tooltip refactor must follow the same three rules (memoized rows, stable
  props, one shared overlay reading a ref).
- `components/dock/DockPanelsHost.tsx` — `onlyWhenVisible` panels already unmount when hidden; see if
  more panels can opt into that.

## Risks / caveats

- Unmounting trades restore-latency for lower steady-state cost. Only cut where restore is instant from
  persisted state; keep the mount where the user would notice a rebuild flash or lost live session.
- The shared-tooltip refactor touches a Store file under active edit — re-read `StoreResultsGrid.tsx`
  first and rebase on the landed dialog changes.

## Verification / gate

- Re-run Phase 1 protocol; confirm the residual O(nodes) term drops (Store-page opens get closer to the
  Scene-page number as the visible grid sheds per-card roots).
- No regression to documented keep-mounted behaviors; `logRender` counters stay healthy (no new
  re-render storms from the shared-tooltip change — follow the AGENTS.md large-list rules).
- `just prepare-for-commit` + `bun run check` + `vite build` green.

## Expected impact

Secondary but compounding: lowers the constant on every recalc/layout and reduces memory pressure. Most
valuable *after* Phases 2–4, as a polish pass toward the < 32 ms Store-page target.
