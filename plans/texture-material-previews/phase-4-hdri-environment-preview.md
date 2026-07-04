# HDRI environment preview — Thumbnail / 3D Preview / Exposure

**Status:** NOT STARTED
**Scope:** `saffron-control`, `saffron-sceneedit`, `saffron-rendering`, editor
**Depends on:** phase-1, phase-3 (furnisher refactor + 3D tab), **`primitive-meshes/`** (balls)

## Goal

An HDRI opens the AmbientCG-style three-mode preview, reusing the existing IBL/sky/tonemap pipeline —
the only genuinely new rendering is swapping the env source to the imported equirect and adding an
exposure uniform.

## The three modes

- **A. Thumbnail** — raw equirectangular tonemapped at fixed exposure. Reuse the existing flat-texture
  HDR thumbnail path (already tonemaps HDR). Shows composition + where the light sources sit.
- **B. 3D Preview — balls in the environment.** In the isolated preview scene, set
  `SkyMode::Texture` with `sky_texture = hdri id` so the HDRI both lights the scene (via the existing
  IBL prefilter: irradiance + prefiltered specular + BRDF LUT) and is the visible backdrop. Spawn
  **three PBR balls** (built-in sphere ×3): chrome (metallic 1, roughness 0 → reflections/structure),
  diffuse grey (metallic 0, roughness 1 → irradiance/color-temp), colored satin (mid-roughness
  dielectric → color response). Orbit camera frames all three.
- **C. Exposure Preview — EV sweep.** Multiply the env by `2^EV` before tonemap; scrub −6…+6 EV. Low EV
  reveals detail inside blown-out sun/windows (confirms real range); high EV lifts shadow detail.
  Optional fixed-stop contact sheet.

## Touch points

- **`furnish_preview_scene` (refactored, phase-3)** — an HDRI variant: `SkyMode::Texture(hdri)` +
  spawn the 3-ball rig instead of a single subject.
- **`enter-asset-preview`** — route `Texture` with `colorspace == hdr` to the HDRI variant.
- **exposure uniform** — a per-view exposure override multiplied before the existing tonemap stage, for
  the preview view only (must not disturb the main viewport's exposure). Verify the tonemap stage can
  take a per-view override.
- **editor `AssetEditorWorkspace`** — the Thumbnail / 3D Preview / Exposure(EV) mode picker + the EV
  slider.

## Verification

- An imported HDRI opens with the environment + 3 balls; the chrome ball reflects the scene, the grey
  ball shows the dominant light direction/color, the EV slider reveals clipped-vs-real range.
- Main viewport exposure is unaffected while the preview EV is scrubbed.

## Risks

- **On-the-fly IBL prefilter cost** of an arbitrary imported equirect at interactive rates — cache the
  prefiltered set keyed by `content_hash`. Measure before assuming it's free.
- **Per-view exposure plumbing** is assumed feasible — confirm in the tonemap stage.
