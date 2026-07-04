# Polish + keep-current

**Status:** NOT STARTED
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
