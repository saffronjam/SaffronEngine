# Polish + keep-current

**Status:** IMPLEMENTED. **Exposure:** the preview pane's header carries a −6…+6 EV `Slider` (→
`set-exposure`), reusing the engine exposure stash/restore around the preview (enter stashes,
`exit-asset-preview` / `set-active-view → Scene` restore), so the sweep never touches the authored
viewport. **`sa`:** confirmed scriptable with no new command — `sa` is a generic passthrough, so
`sa enter-asset-preview <materialId>` reaches the phase-1 `Material` branch the moment the engine
registers it (a material previews from the shell just like a model). **Docs:** added
`docs/content/explanations/ui-and-editor/material-graph-live-preview.md` (what it is, the reused modal
`assetPreview` view + modal swap, the by-id live-edit reflection, pan-only orbit, EV) + the hub
`_index.md` row, and refreshed the asset-editor page's routing paragraph (materials now route to the
asset editor too); `hugo --gc --minify` builds clean (240 pages). **Deferred stretch (own follow-up):**
the shape toggle (sphere / plane / cube) and a middle-grey exposure-calibration region. Verified:
`tsc` + `oxlint` (0 new warnings) + `cargo clippy --workspace -D warnings` + `cargo fmt --check` + docs
build clean.
**Scope:** editor, `saffron-control`/`sa`, `docs/`
**Depends on:** phase-3

## Goal

Round out the live material preview and satisfy the AGENTS.md keep-current rules.

## Touch points

- **Exposure control** — expose the preview view's exposure (reuse the per-view exposure override from
  `texture-material-previews/phase-4`) so a material can be judged across a stop range. v1 may ship a
  fixed sensible exposure and add the slider here.
- **`sa` / `saffron-control`** — the material-preview path is reachable from the shell (it rides the
  existing `enter-asset-preview` command, so confirm scriptability rather than adding a new command).
- **`docs/`** — a page under `docs/content/explanations/ui-and-editor/` for the material-graph live
  preview (what it is, that it reuses the asset-preview view, pan-only camera), plus the hub `_index.md`
  row.
- **(Stretch, own follow-up)** shape toggle (sphere / plane / cube — the standard preview trio) once the
  primitives exist; and a middle-grey reference region for exposure calibration. The plane at grazing
  angle is also the honest way to preview displacement once `displacement/phase-a` lands.

## Verification

- `just check` green; docs build clean; hub row present.
