+++
title = 'Native materials'
weight = 5
+++

# Native materials

A material is a first-class, editable asset. It lives on disk as a `.smat` file, is tracked in the asset catalog like a mesh or texture, can be assigned to an entity, edited in the material panel against a live preview sphere, and — its endgame — authored in a [node graph](../node-graph-codegen/). This page covers what a material *is* once it is a native asset; the codegen page covers how a graph becomes a shader.

The split that makes this cheap is the same one [PSO selection](../material-and-pso-selection/) relies on: the *shape* of a material (which textures, which features) decides the pipeline; the *values* (base color, roughness, texture indices) live in a buffer the shader reads per draw. Editing a slider is a buffer write, not a recompile.

## The asset

A `.smat` is reference-only JSON: scalar factors plus texture references as decimal-string `Uuid`s into the catalog. It never embeds pixels.

```jsonc
{
  "blend": "opaque",
  "baseColor": [0.8, 0.8, 0.8, 1.0],
  "metallic": 0.0, "roughness": 0.7,
  "albedoTexture": "12876…",   // Uuid into the catalog, or "0" for none
  "normalTexture": "0",
  "graph": { /* optional node graph — see codegen */ }
}
```

`MaterialAsset` (the in-memory form) adds a `parent` `Uuid` and an `overrides` set, so an **instance**
material inherits a base and overrides only named fields — the UE material-instance model.
`load_material_asset` resolves the parent chain, applies overrides, then folds any graph;
`resolve_entity_materials` decides precedence between a mesh's built-in material and an assigned
`MaterialAssetComponent`. `material_asset_to_json` / `material_asset_from_json` are the frozen JSON
contract.

## The params buffer and the surface seam

At draw time a material resolves to a `MaterialParamsData` record (96 bytes) in a per-frame SSBO (set
2, binding 2). Identical materials dedup to one record (`intern_material` hashes the raw bytes);
`InstanceData.texture.w` carries the material index. The übershader reads it through one seam:

```hlsl
SurfaceData evalSurface(MaterialInput m);   // material half (mesh.slang)
// … the lighting module (lighting.slang) consumes SurfaceData unchanged
```

Everything material-specific lives behind `evalSurface`; the lighting code never changes. Feature
bits in the params (`FEATURE_NORMAL`, `FEATURE_EMISSIVE_TEX`, `FEATURE_OCCLUSION`, `FEATURE_HEIGHT`,
`FEATURE_ALPHACLIP`) gate the optional work, so a plain color material pays for none of it.

## PBR slots

Beyond albedo, a material carries normal, packed ORM (occlusion-R, roughness-G, metallic-B), emissive, and height maps, plus `normalStrength`, `uvTiling`/`uvOffset`, `heightScale`, and the `blend` mode + `alphaCutoff`. The shader applies them feature-gated:

- a derivative-based (Schüler) tangent frame perturbs the normal — no per-vertex tangents needed (`perturbNormal`);
- height drives **parallax occlusion mapping** (`parallaxUv`, a multi-step UV march) for silhouette-deepening relief;
- the `masked` blend mode alpha-tests against `alphaCutoff` — a hard `discard`, upgraded to derivative-sharpened [alpha-to-coverage](../ubershader-and-specialization/) under MSAA; `translucent` instead draws in the sorted blend pass.

This is what lets an imported Poly-Haven-style texture set — diffuse + normal + roughness + displacement — render with depth rather than as a flat decal.

## Authoring

`import_material_folder` suffix-detects roles (`detect_material_role`: `_diff`, `_nor`, `_rough`,
`_disp`, …) and bakes a `.smat` plus catalog entries from a folder of textures. The **material
panel** picks a material, shows it on a studio-lit preview sphere (`preview-render` →
`render_material_preview`), and edits factors live (coalesced `material-update` → re-render). Every
operation has a control command, so the `sa` CLI and the editor drive the same surface.

## In the code

| What | File | Symbols |
|---|---|---|
| Asset model + IO | `material.rs` | `MaterialAsset`, `material_asset_to_json`, `load_material_asset`, `save_material_asset` |
| Import + role detection | `manage.rs`; `scan.rs` | `import_material_folder`; `detect_material_role` |
| Entity precedence | `render_material.rs` | `resolve_entity_materials`, `MaterialAssetComponent` |
| Params record + dedup | `gpu_types.rs`; `instancing.rs` | `MaterialParamsData`; `intern_material`, `ensure_material_capacity` |
| Surface seam + slots | `mesh.slang`; `lighting.slang` | `evalSurface`, `perturbNormal`, `parallaxUv` |
| Preview render | `thumbnail_render.rs` | `render_material_preview` |
| Control commands | `commands_asset.rs` | `material-create/-get/-update/-import/-assign/-set-override` |

## Related

- [Node-graph codegen](../node-graph-codegen/) — authoring a material as a graph that generates a shader
- [Übershader](../ubershader-and-specialization/) — the one shader `evalSurface` plugs into
- [Bindless textures](../bindless-textures/) — how a texture index in the params resolves to a sampler
