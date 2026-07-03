# Transparency (glTF alpha modes: blend + coverage-AA masking)

**Status:** IN PROGRESS — Phase 1 COMPLETED; Phase 2 core (alpha-to-coverage) COMPLETED, two
non-default-config refinements remain (see Phase 2 status).

Make the renderer honour all three glTF alpha modes correctly. Today `OPAQUE` and `MASK` render
right, but `BLEND` is silently collapsed to opaque — a `BLEND` material draws exactly like an opaque
one. This plan adds a real **sorted, back-to-front translucent pass** (the modern baseline every
shipping engine uses), then a distinct **coverage-based anti-aliasing** capability (alpha-to-coverage
+ hashed alpha) for masked foliage, which is what AAA engines actually use for grass/leaves/hair.

## Why

`AlphaBlendModeTest` and `CompareAlphaCoverage` (the "fur" grass) both render their `BLEND` material
as opaque. The chain that drops it:

- **Import is correct.** glTF `BLEND` → `AlphaMode::Blend` (`engine/crates/geometry/src/gltf_import.rs`,
  `extract_gltf_material`) → `.smat` `blend: "translucent"` (`engine/crates/assets/src/import.rs`,
  `material_chunk_json`). The `.smat` on disk says `translucent`.
- **The draw path drops it.** `build_submesh_material` (`engine/crates/assets/src/render_material.rs`)
  derives `alpha_clip: material.blend == "masked"` — so `translucent` yields `alpha_clip = false` and is
  otherwise ignored (a test even pins `("translucent", false)`). `SubmeshMaterial`
  (`engine/crates/rendering/src/draw_list.rs`) carries only a masked `alpha_clip` bool; there is no
  blend axis past this point.
- **One opaque PSO.** `PsoKey` (`engine/crates/rendering/src/pipelines.rs`) has no alpha axis, and the
  mesh PSO `build_mesh_pipeline_with_module` hardcodes `blend_enable(false)` with depth-write on. So
  opaque / mask / blend all resolve to one cached opaque pipeline.
- **One pass, no sorting.** The forward `"scene"` pass (`engine/crates/rendering/src/renderer.rs`,
  `scene-opaque` scope → `record_scene_draw_list`) is the only geometry pass; `submit_draw_list`
  (`engine/crates/rendering/src/instancing.rs`) buckets by `(pipeline, mesh)` in first-seen order with no
  depth sort.

The shading side is already blend-ready: the übershader computes `surf.opacity = base.a * baseColor.a`
(`engine/assets/shaders/mesh.slang`, `evalSurface`) and emits it in the output alpha
(`engine/assets/shaders/lighting.slang`, `evalLighting` / `evalViewMode` → `float4(color, surf.opacity)`),
and the offscreen target is HDR `R16G16B16A16_SFLOAT`. Everything missing is on the **pipeline + pass**
side.

## Design stance (grounded in current engine practice)

A separate transparent pass is **not** a legacy fallback — it is structurally required and universal
(UE5, Unity HDRP, Frostbite, Godot 4): blending reads the framebuffer beneath the fragment (so lower
geometry must already be resolved → a later pass), depth-write must be off, and a deferred G-buffer
holds one layer per pixel. The real design axis is *ordering within that pass*, and it splits by
content type:

- **Genuine translucency** (glass, the `AlphaBlendModeTest` panels): **sorted back-to-front** is the
  shipping baseline everywhere. Order-independent transparency (weighted-blended / moment-based /
  multi-layer alpha blending / per-pixel linked lists) is a *case-by-case upgrade* for when sorting
  visibly breaks — nobody starts there.
- **Foliage / grass / hair / fur** (the `CompareAlphaCoverage` grass): engines deliberately do **not**
  alpha-blend these (dense intersecting geometry is exactly where per-object sort fails). They render
  it as **masked + alpha-to-coverage (MSAA)** or **hashed/dithered alpha resolved by TAA**. This engine
  already has MSAA + TAA, so it is well-positioned for both.

glTF defines only `OPAQUE` / `MASK` / `BLEND`, so a conformant viewer must make `BLEND` actually blend
— that is the non-negotiable core (Phase 1). Coverage-AA is the orthogonal quality capability the fur
model exists to demonstrate (Phase 2).

## Goal

- **`BLEND` blends.** A `translucent` material draws in a sorted, back-to-front translucent pass with a
  blend-enabled, depth-test-on / depth-write-off PSO. `AlphaBlendModeTest`'s Blend panel and
  `CompareAlphaCoverage`'s `fur_blend` patch match their references.
- **One blend-mode axis, no bool.** The masked `alpha_clip: bool` is replaced end-to-end by a
  `BlendMode { Opaque, Masked, Blend }` enum (NO-LEGACY: replace the bool and every caller in the same
  change), which drives both material resolution and PSO selection.
- **Coverage-AA masking.** Masked materials render with alpha-to-coverage (under MSAA) and/or hashed
  alpha (resolved by TAA), giving anti-aliased foliage edges without blending or sorting — the modern
  grass/leaf path.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-sorted-blended-pass.md` | `BlendMode` enum replacing `alpha_clip`; a blend axis on `PsoKey` + a blend-enabled / depth-write-off mesh PSO; split the draw list into opaque vs translucent; a back-to-front sorted **translucent** graphics pass after `"scene"` reading shared depth; exclude translucents from the depth pre-pass. Fixes `BLEND` for both test models. | — |
| 2 | `phase-2-coverage-foliage-aa.md` | Alpha-to-coverage on the masked PSO under MSAA (`fwidth` edge-sharpen), plus hashed/dithered alpha discard in the übershader seeded by the TAA jitter, and a "preserve coverage" mipmap option on masked-texture import. The modern foliage-AA path for `CompareAlphaCoverage`'s masked patch. | Phase 1 |

## Future (unscheduled)

Order-independent transparency (weighted-blended OIT as a cheap approximation, or moment-based / MLAB
for higher quality) is the correct upgrade **iff** sorted back-to-front shows artifacts on complex
overlapping translucency in real content — the same case-by-case call shipping engines make. Not
scheduled here; Phase 1's sorted pass is the baseline it would replace, and the draw-list split it
introduces is the seam an OIT accumulate/resolve pair would hook into.

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| glTF alpha-mode import (correct) | `engine/crates/geometry/src/gltf_import.rs` | `extract_gltf_material` |
| `.smat` `blend` string axis | `engine/crates/assets/src/material.rs` | `MaterialAsset.blend`, `material_asset_from_json` |
| Where `translucent` is dropped to opaque | `engine/crates/assets/src/render_material.rs` | `build_submesh_material` (`alpha_clip = blend == "masked"`), the graph-lowered path, the blend-mode test |
| Raster material (only a masked bool today) | `engine/crates/rendering/src/draw_list.rs` | `SubmeshMaterial.alpha_clip`, `DrawItem`, `SceneDrawList`, `DrawBatch` |
| PSO selector + key (no alpha axis) | `engine/crates/rendering/src/gpu_types.rs`, `engine/crates/rendering/src/pipelines.rs` | `Material`, `PsoKey`, `request_mesh_pipeline` |
| Mesh PSO (`blend_enable(false)`, depth-write on) + the existing blend attachment | `engine/crates/rendering/src/pipelines.rs` | `build_mesh_pipeline_with_module`, `alpha_blend_attachment` |
| Draw-list batching (bucket by pipeline+mesh, no sort) + feature bits | `engine/crates/rendering/src/instancing.rs` | `DrawListInputs`, `submit_draw_list`, `resolve_material`, `FEATURE_ALPHACLIP` |
| Scene-pass recording | `engine/crates/rendering/src/scene_pass.rs` | `record_scene_draw_list`, `record_batch_submeshes` |
| Forward `"scene"` + depth-prepass pass assembly | `engine/crates/rendering/src/renderer.rs` | `RgPass::graphics("scene")` (`scene-opaque` scope), `RgPass::graphics("depth-prepass")` |
| Shader already emits opacity | `engine/assets/shaders/mesh.slang`, `engine/assets/shaders/lighting.slang` | `evalSurface` (`surf.opacity`), `evalLighting` / `evalViewMode`, `FEATURE_ALPHACLIP` |
| MSAA + TAA substrate (Phase 2) | `engine/crates/rendering/src/aa.rs`, `engine/assets/shaders/taa.slang` | MSAA sample count in `PsoKey`, TAA jitter |

## Test models

- `AlphaBlendModeTest` — five panels: Opaque / Blend / three Cutoff (Mask). Blend must fade with the
  striped alpha; the Cutoff panels already work.
- `CompareAlphaCoverage` — the "fur" grass: `fur_opaque` (OPAQUE), `fur_mask` (MASK 0.2), `fur_blend`
  (BLEND). Phase 1 fixes `fur_blend`; Phase 2 improves `fur_mask` edges. All materials are
  `doubleSided`, alpha in `FurBaseColorAlpha.png`'s alpha channel.

Both live under `/var/home/saffronjam/repos/glTF-Sample-Assets/Models/`. The `tests/e2e` suite is the
place to assert the wire/behaviour once the pass lands.
