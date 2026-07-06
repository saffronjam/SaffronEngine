# Phase 4 — Non-modal overlay primitives, app-wide

**Status:** CODE COMPLETE — **REQUIRES USER RUNTIME VERIFICATION** (a from-scratch `Select` an agent
cannot drive; see checklist). tsc + oxlint + `vite build` all green.

**What landed:**
- `components/ui/select.tsx` rebuilt on a **non-modal Radix Popover** + a hand-rolled roving listbox
  (no cmdk dependency, to avoid its filter/focus unknowns). No `react-remove-scroll`, no `hideOthers`,
  no body-level `--removed-body-scroll-bar-size` write → no whole-document recalc on open. API is a
  drop-in: `Select` / `SelectTrigger` / `SelectValue` / `SelectContent` / `SelectItem` with the same
  props (`value`/`onValueChange`/`disabled`/`size`/`align`). Selected value's label is resolved by
  walking declared children (`collectItems`, exported), so a preset value shows its label without
  opening. Keyboard: Arrow/Home/End move highlight, Enter/Space commit, typeahead by first char of
  string labels, Escape/outside-click dismiss via Popover. Dead exports removed (NO LEGACY):
  `SelectGroup`/`SelectLabel`/`SelectSeparator`/`SelectScrollUpButton`/`SelectScrollDownButton` were
  unused anywhere. Redundant `position="popper"` dropped at both call sites (`CaptureControls`,
  `StoreWorkspace`).
- `DropdownMenu` + `ContextMenu` wrappers now default `modal={false}` (opt-in `modal` to restore),
  removing their scroll-lock too — this fixes the store **parts** `DropdownMenu` and every menu.
- `Popover`/`Dialog` were already non-modal / scoped (the latter from the in-flight dialog work).

**USER VERIFICATION CHECKLIST (do before marking COMPLETED):**
1. Overlay open→paint on the Store page is now ≈instant (`[overlay-perf]` in the console, or the
   Timeline no longer shows the 3× full-document style recalc).
2. Every `Select` still works at each call site: Inspector enum fields (`EnumField`), `RenderPanel`
   AA/view-mode + disabled RT toggles, `EnvironmentPanel`, `CaptureControls`, `BoneSelect`,
   `ComboField`, `TimelineTransport`, `MaterialEditorPanel`/`MaterialGraphEditor`, and the Store
   provider + resolution pickers — open, mouse-select, keyboard (arrows/enter/typeahead), preset
   value shows the correct label on load, `disabled` selects don't open.
3. Menus/context-menus still dismiss on outside click and don't lock background scroll oddly.

The deep structural fix: stop overlays from doing whole-document work at all. Phases 2–3 make a recalc
*cheap*; this phase removes the *reason* for the multi-recalc + forced-layout — Radix's modal scroll-lock
and `hideOthers` document walk — from the app's shared overlay primitives.

**Re-measure after Phase 3 before doing all of this.** If containment + elision already hit the gate for
menus/tooltips/context-menus, this phase narrows to just `Select` (the one primitive with no non-modal
option). Do not over-build.

## Why

`react-remove-scroll` (body write + sync measure) and `aria-hidden`/`hideOthers` (whole-document walk)
are what the trace attributes the 3 recalcs + 2 layouts to. For most Radix primitives these are opt-out;
for `Select` they are not.

## The proven template (already in the tree)

The in-flight Store dialog work built exactly the right pattern and it is documented as a hard rule in
`storefront/AGENTS.md`: the Store dialogs are **`modal={false}`**, portal into a **scoped container**
(`storefront/storeOverlay.ts` `useStoreOverlayContainer`), and bring their **own backdrop**
(`DialogScopedOverlay` in `components/ui/dialog.tsx`) — so they dim only the Store view, leave the tab
strip live, and never run the body-wide scroll-lock. Generalize this from "Store dialogs" into shared
primitives; do **not** reinvent it, and never revert the Store dialogs to body-portaled modal.

## Changes, per primitive

1. **`DropdownMenu`, `Popover`, `ContextMenu`, `Dialog` (`components/ui/*.tsx`)** — these accept
   `modal={false}`. Default the app's wrappers to non-modal (no `react-remove-scroll`, no `hideOthers`),
   and where a backdrop/scroll-lock is genuinely wanted (a true blocking dialog), opt in explicitly per
   call site. Audit every call site so removing the default modal behavior does not silently drop a
   needed dismiss/scroll-lock.
2. **`Select` (`components/ui/select.tsx`)** — Radix `Select` has **no** `modal` prop (radix #1496). The
   modern correct fix is to stop using `SelectPrimitive` for the app's dropdowns and build the shared
   `Select` on a **non-modal `Popover` + a listbox/`cmdk` command list** (roving-tabindex, typeahead,
   keyboard nav) that portals into a scoped container and does no body scroll-lock or document
   `hideOthers`. Keep the existing `Select*` export names and props so all ~N call sites
   (`StoreWorkspace`, `ImportControls`, inspector fields, settings, environment, render panels, …) are
   drop-in — replace the *implementation*, not the API. This is a real component, built once, correct:
   no compat shim, delete the old `SelectPrimitive`-based body in the same change.
3. **`Tooltip` (`components/ui/tooltip.tsx`)** — tooltips are already non-modal; confirm the
   `TooltipProvider delayDuration={300}` (`app/App.tsx:343`) and that opening one does not pull in any
   scroll-lock. If Phase 3 made recalc cheap, tooltips likely need no change beyond verification.
4. Provide a shared **scoped overlay container** primitive (generalize `storeOverlay.ts` into
   `components/ui`), so any view — not just the Store — can host its overlays scoped to itself rather
   than `document.body`. The Store keeps its existing container; other views adopt as needed.

## Risks / caveats

- **Behavior parity is the hard part**, not perf. A `Select` rebuilt on Popover+listbox must match: full
  keyboard nav (arrows/home/end/typeahead), selected-item scroll-into-view, correct ARIA
  (`role=listbox`/`option`, `aria-activedescendant`), form integration, and controlled/uncontrolled
  value. Lean on `cmdk` (already used elsewhere?) or Radix's own listbox pieces where they are
  non-modal; write focused tests for keyboard + selection.
- Non-modal menus do not scroll-lock the background — acceptable and usually desirable in a desktop
  editor (scrolling a panel dismisses the menu). Confirm outside-scroll dismiss behaves.
- Store files are under active dialog edits — this phase overlaps them most. Re-read
  `dialog.tsx`/`storeOverlay.ts`/store call sites immediately before editing and rebase; coordinate
  timing with the user so we are not both editing `StoreWorkspace`/`ImportControls` at once.

## Verification / gate

- Re-run Phase 1 protocol for **every** overlay type on the Store page: target **< 32 ms** open→paint,
  and confirm the Timeline record no longer shows the 3× full-document recalc — at most one small,
  contained recalc.
- Keyboard/selection parity tests for the rebuilt `Select` pass; every migrated call site behaves
  identically (spot-check inspector enum fields, settings, environment).
- `just prepare-for-commit` + `bun run check` + `vite build` green. Old `SelectPrimitive`-based
  implementation fully removed (NO LEGACY).

## Expected impact

Removes the root cause for the remaining overlays. After Phases 2–4 no overlay open should trigger a
whole-document recalc anywhere in the app.
