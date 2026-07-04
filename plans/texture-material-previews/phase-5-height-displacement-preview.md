# Height map preview — real VS-displacement on the sphere

**Status:** NOT STARTED
**Scope:** editor (`AssetEditorWorkspace` representation picker), `saffron-control`
**Depends on:** phase-3, **`displacement/phase-a`** (the VS-displacement mechanism)

## Goal

A height map previews as a **real displaced sphere** with a true silhouette — the chosen near-term
answer over POM — plus the flat height swatch in the picker.

The displacement *mechanism* (pre-subdivided base + vertex-shader scalar displacement, analytic normal
recompute) lives in `displacement/phase-a` and is authored against the preview sphere / `preview.slang`
there. This phase is the **thin wiring**: route a `Height` texture (or a material with `height_texture`)
into a preview scene whose sphere uses the displaced material path, and expose it in the viewer.

## Touch points

- **`enter-asset-preview` height branch** — synthesize the ephemeral material with the texture in the
  height slot + a sensible `height_scale`, on the displaced preview sphere (subdivided base from
  `displacement/phase-a`).
- **`AssetEditorWorkspace` picker** — "Displaced" (default for height) / "Flat" (the raw greyscale
  height image). Optionally a `height_scale` slider.

## Verification

- A height map on the sphere shows a **deformed silhouette** at grazing angle (not the smooth-outline
  POM look), and the flat swatch reads the raw elevation image.

## Notes

- This supersedes the POM height thumbnail from phase-2 *for the interactive tab*. If `displacement/
  phase-a` lands before phase-2, prefer VS-displacement in the thumbnail too and skip the POM path
  entirely (NO-LEGACY — don't ship a POM preview then delete it). Sequencing note tracked in the
  displacement README.
