# Per-type texture & material previews — on a sphere, in the "View" tab

**Status:** COMPLETED (phases 1–4 fully; phase 5 wired, its true-displacement mechanism deferred to the
separate research-designated `displacement/phase-a`). Every texture now previews per-role on a lit
sphere as a thumbnail and in the interactive 3D "View" tab, plus the AmbientCG-style HDRI environment
(three-ball rig + EV sweep). See each `phase-*.md` `**Status:**` line for the per-phase detail.

Materials already render on a ball for thumbnails; **textures do not** (they show as a flat downscaled
image), and neither textures nor materials open the interactive 3D "View" tab as a lit sphere. This
plan makes every texture *type* preview meaningfully — as a thumbnail and in the real-renderer 3D tab —
and adds the AmbientCG-style HDRI environment preview.

## Design principle — lit-by-default, flat-channel-on-demand

A grey swatch is the *wrong* inspection view for a map that has a physical role. Every reference tool
(Substance's channel cycle, Marmoset, Blender shader balls, AmbientCG's Thumbnail/3D/Exposure tabs)
shows the map *doing its job* on lit curved geometry, with the raw channel as a secondary toggle. We
adopt: a smart per-type default view **plus** a channel/representation picker (shipping in v1).

Direct answers to the design questions:
- **Roughness / metallic / normal** — a grey swatch is *not* enough. Feed the map into its real channel
  under appropriate lighting (roughness/normal) or IBL (metallic) so the highlight/reflection variation
  — which *is* the map — becomes visible.
- **Height** — near-term ships **VS-displacement on the preview sphere** (real silhouettes), per
  `displacement/phase-a` — not POM. Flat height swatch remains available in the picker.
- **HDRI** — the AmbientCG three-mode pattern: **Thumbnail** (lat-long) / **3D Preview** (HDRI as the
  environment + chrome + diffuse-grey + colored balls) / **Exposure Preview** (EV slider). Reuses the
  existing IBL prefilter + sky + tonemap.

## Routing table (role → representation)

| Role | Default preview | Ephemeral material synthesis |
|---|---|---|
| albedo / color | Lit sphere, roughness ~0.5, metallic 0 | tex → albedo slot |
| normal | Mid-grey sphere, `FEATURE_NORMAL`, grazing key; GL/DX flip toggle | tex → normal slot |
| roughness | Dielectric sphere, roughness ← map, one sharp source | tex → MR (G) |
| metallic | Sphere metallic ← map **under IBL** | tex → MR (B) |
| ao | Greyscale-modulated lit sphere | tex → occlusion slot |
| height | **VS-displaced sphere** (real silhouette) + flat swatch | tex → height slot, `height_scale` |
| emissive | Glowing sphere on dark bg | tex → emissive slot |
| opacity / mask | Alpha-cutout over checkerboard | tex → albedo alpha, masked blend |
| orm (packed) | Applied on sphere + R/G/B split strip | tex → MR (+occlusion) |
| hdr (equirect) | **Environment sphere + 3-ball rig + EV** | (scene env, not a material) |
| unknown | Flat downscaled image (fallback) | none |

## Diagnosis (current code)

- `preview.slang` (`engine/assets/shaders/`) reads only 3 of 6 slots (albedo / MR / normal), fixed
  2-light studio, **no IBL** — the thumbnail shader must gain occlusion / emissive / height / alpha.
- `enter-asset-preview` (`control/src/commands_asset.rs`) always `instantiate_model` and hard-rejects
  `container_id == 0` — must branch for texture/HDRI subjects.
- `furnish_preview_scene`, `compute_preview_bounds`, `spawn_preview_floor` are **hardwired to
  `ctx.scene_edit.preview_scene`** — they must be refactored to take a `&mut Scene` before they can
  furnish a texture/material sphere subject (shared prerequisite with `material-graph-live-preview/`).
- The engine **computes a texture's role** at import (`detect_material_role`, `assets/src/scan.rs`)
  then **throws it away**; `AssetEntryDto` exposes neither `colorspace` nor `role`, so the editor
  cannot route. Fix in Phase 1.
- `routeView` (`editor/src/panels/AssetsPanel.tsx`) sends every texture to the flat `<img>`
  `openImageViewerTab` — rerouted in Phase 3.

## Phases

1. **`phase-1-role-data-model.md`** — persist `TextureRole`, plumb connector role, surface
   `colorspace`/`role` on the DTO. *Independent — start early.*
2. **`phase-2-thumbnail-routing.md`** — ephemeral-material thumbnails + `preview.slang` slot completion.
3. **`phase-3-interactive-texture-viewer.md`** — refactor the furnisher to `&mut Scene`; texture-on-
   sphere in the 3D tab; reroute `routeView`; the representation picker.
4. **`phase-4-hdri-environment-preview.md`** — HDRI env + 3-ball rig + exposure/EV.
5. **`phase-5-height-displacement-preview.md`** — wire the VS-displaced sphere for height maps
   (thin — the mechanism lives in `displacement/phase-a`).

## Dependencies

- Phases 3–5 need `BUILTIN_SPHERE_MESH_ID` spawnable → **`plans/primitive-meshes/`**.
- Phase 5 needs VS-displacement → **`plans/displacement/phase-a`**.
- The furnisher-`&mut Scene` refactor (Phase 3) is shared with **`plans/material-graph-live-preview/`**.
