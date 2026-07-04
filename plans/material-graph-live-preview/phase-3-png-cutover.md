# Cutover: delete the PNG preview path in the graph tab

**Status:** IMPLEMENTED. `MaterialGraphEditor` has exactly one preview path now — the live sphere. The
`<img>` block, the module-level `previewCache`, the `preview`/`setPreview` state, and all three
`client.previewRender(...)` call sites (load, debounced apply, undo replay) are gone. The 500 ms
graph-push debounce survives but calls only `materialSetGraph`; the live view re-renders on its own
because `material-set-graph` → `update_material_asset` invalidates the material cache, so the next
frame re-resolves the edited `.smat` on the id-referencing sphere — no readback-to-PNG round trip, and
the user can orbit continuously. `preview-render` / `render_material_preview` / `preview.slang` are
untouched (they still back thumbnails via `get-thumbnail` / the asset grid + picker swatches — a
distinct code path). Verified: no `previewRender` remains in the component; editor `build` clean.
**Scope:** editor (`MaterialGraphEditor`)
**Depends on:** phase-2

## Goal

The material-graph tab has exactly **one** preview path — the live view. The static PNG is removed here
(NO-LEGACY: no dual path).

## Touch points

- **`MaterialGraphEditor.tsx`** — remove the `<img>` block and the module-level `previewCache`. Remove
  the `client.previewRender(...)` call sites in this component. Keep the 500 ms **graph-push** debounce
  but rewire it to call only `client.materialSetGraph(...)` — the live view re-renders on its own once
  the material updates; the readback-to-PNG round trip disappears and the user can orbit continuously.

## What survives (not a shim)

`preview-render` / `render_material_preview` / `preview.slang` still back **thumbnails** elsewhere
(`get-thumbnail`/`view-asset`, the asset grid + picker swatches) — a distinct feature with a distinct
code path. Only the material-graph tab's use of the PNG is removed.

## Verification

- Editing the graph updates the sphere live; there is no PNG fetch on the material-graph tab (verify
  via the render/network counters and that `previewRender` is no longer called from this component).
- `just prepare-for-commit` clean.
