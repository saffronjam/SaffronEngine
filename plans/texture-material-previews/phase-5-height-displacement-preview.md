# Height map preview — real VS-displacement on the sphere

**Status:** WIRED — mechanism deferred to `displacement/phase-a`. A `Height` texture already opens the
interactive sphere (phase-3 routing: `preview_material_for_texture` puts it in the height slot on the
studio sphere) and reads through the übershader's existing **parallax-occlusion mapping**
(`parallaxUv` in `mesh.slang`/`lighting.slang`), with a slightly deeper preview `height_scale` (0.08)
so the relief is legible; the Flat picker still reaches the raw greyscale swatch. That completes every
touch point **inside the `texture-material-previews` plan-set**. The remaining piece — the *true
deformed silhouette* via vertex-shader displacement — is the mechanism in **`displacement/phase-a`**, a
**separate plan-set the user explicitly designated research-first** and which is **not part of this
`/goal`**. Per NO-LEGACY nothing throwaway was shipped: the preview reuses the übershader's existing POM
(no new POM path to retire), and the routing here is unchanged when phase-a swaps the sphere's shading
path to real displacement — only the shading path changes, not this wiring. The optional live
`height_scale` slider is deferred with the mechanism (a POM-depth slider alone is marginal; it becomes
meaningful once the displacement is real). Verified: workspace build + `clippy -D warnings` + shader
compile + frontend `tsc`/`oxlint`/`build` clean.
**Scope:** editor (`AssetEditorWorkspace` representation picker), `saffron-control`
**Depends on:** phase-3, **`displacement/phase-a`** (the VS-displacement mechanism — external, research-first)

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
