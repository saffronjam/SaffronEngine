+++
title = 'Native materials'
weight = 5
+++

# Native materials

A native material is a catalog asset that describes one renderable surface. It owns the PBR factors,
texture references, blend and rasterization choices, height treatment, optional node graph, and
instance inheritance data. Scene entities refer to the asset instead of copying its full contents.

The asset model separates durable authoring data from per-frame GPU data. Editing a factor or texture
invalidates the resolved material cache, and the next frame rebuilds the compact parameter record.
Only a non-foldable [node graph](../node-graph-codegen/) needs a material-specific shader.

## Asset document

A standalone material lives at `materials/<uuid>.smat`. Its JSON groups scalar values under
`factors` and texture UUIDs under `textures`. UUIDs are written as decimal strings, with `"0"`
representing an unassigned texture.

```jsonc
{
  "version": 1,
  "shader": "mesh",
  "blend": "opaque",
  "unlit": false,
  "doubleSided": false,
  "heightMode": "bump",
  "normalConvention": "gl",
  "factors": {
    "baseColor": [0.8, 0.8, 0.8, 1.0],
    "metallic": 0.0,
    "roughness": 0.7,
    "emissive": [0.0, 0.0, 0.0],
    "emissiveStrength": 1.0,
    "normalStrength": 1.0,
    "alphaCutoff": 0.5,
    "heightScale": 0.05,
    "uvTiling": [1.0, 1.0],
    "uvOffset": [0.0, 0.0]
  },
  "textures": {
    "albedo": "0",
    "ormOrMr": "0",
    "normal": "0",
    "emissive": "0",
    "height": "0",
    "vectorDisplacement": "0"
  },
  "graph": {},
  "parent": "0",
  "overrides": {}
}
```

The JSON references texture assets and never contains pixel data. A folder import instead creates a
self-contained `.smatx` container whose material chunk uses the same JSON shape and whose texture
chunks hold the maps. Model containers can also carry material chunks with this representation.

## Masters and instances

A material with `parent = "0"` is a master. A nonzero parent makes the document an instance whose
sparse `overrides` object applies over the resolved parent. Resolution follows at most eight parent
links, which bounds cycles and malformed chains.

The exposed-parameter schema defines the keys accepted in material-instance and entity-slot
overrides. It includes PBR colors and scalars, UV controls, blend and raster flags, and the five
standard texture references. Each key also declares a value kind, allowing `material-set-override`
and the Inspector to reject an ill-typed value before it reaches rendering.

`heightMode` and `vectorDisplacementTexture` are authored on the material asset rather than through
the exposed override schema. The material update path edits those fields directly.

## Entity binding

An entity has one `MaterialSet` component containing an ordered `slots` array. Each `MaterialSlot`
holds a material UUID and a sparse per-object override object. UUID zero selects the built-in white,
fully rough, non-metallic material.

Each mesh submesh stores a `material_slot` index. Resolution loads every referenced material once,
applies its parent chain, then applies the slot overrides. A submesh index beyond the available slots
uses the last slot; an absent or empty `MaterialSet` leaves the draw path on engine defaults.

The resolved `unlit` flag, proxy albedo, and node-graph shader follow slot zero because they apply to
the whole mesh item. Blend mode and double-sided state remain per submesh and participate in
[pipeline selection](../material-and-pso-selection/).

## GPU parameter table

Every resolved submesh material lowers to one 96-byte `MaterialParamsData` record in descriptor set
2, binding 2. The record contains six 16-byte blocks:

| Block | Contents |
|---|---|
| `base_color` | Linear RGBA factor |
| `pbr` | Metallic, roughness, normal strength, alpha cutoff |
| `emissive` | Emissive radiance and height scale |
| `uv` | Tiling and offset |
| `tex0` | Albedo, ORM, normal, and emissive bindless indices |
| `tex1` | Height index, occlusion index, reserved lane, feature bits |

The renderer hashes these records by their raw bytes and interns identical values into one per-frame
table entry. `InstanceData.texture.w` carries the resulting material index. Editing one entity's
override therefore creates a distinct record only when its resolved bytes differ.

Feature bits gate optional shader work for normal, emissive, occlusion, parallax, alpha clipping,
displacement, and height-bump sampling. They do not create separate pipelines. Unlit, blend,
double-sided, alpha-to-coverage, and shader identity form the relevant pipeline axes.

## Surface seam

`mesh.slang` reads `MaterialParamsData` and produces a `SurfaceData` value through `evalSurface`.
The shared lighting module consumes that value without knowing whether it came from the fixed PBR
path or generated graph code.

```hlsl
SurfaceData surf = evalSurface(makeMaterialInput(input));
return evalViewMode(input, surf, kUnlit, kTranslucent);
```

The fixed path multiplies base color by the albedo texture, reads roughness and metallic from the ORM
green and blue channels, and reads occlusion from its red channel. It can also apply normal and
emissive maps. Missing textures resolve to the default white bindless slot.

Height mode selects one of three treatments. `bump` changes only the shading normal, `parallax`
marches UVs while keeping a flat silhouette, and `displacement` moves geometry through the
[compute displacement](../../frame-and-render-graph/compute-displacement/) path. Masked materials
alpha-test or use alpha-to-coverage under MSAA; translucent materials render in the sorted blend
pass.

## In the code

| What | File | Symbols |
|---|---|---|
| Asset model and JSON | `assets/src/material.rs` | `MaterialAsset`, `material_asset_to_json`, `material_asset_from_json` |
| Parent and override resolution | `assets/src/material.rs` | `load_catalog_material_asset`, `apply_overrides` |
| Override vocabulary | `assets/src/material_schema.rs` | `pbr_exposed_parameters`, `ExposedParamKind` |
| Entity slots | `scene/src/component.rs` | `MaterialSet`, `MaterialSlot` |
| Render resolution | `assets/src/render_material.rs` | `AssetServer::resolve_entity_materials`, `build_submesh_material` |
| GPU record and interning | `rendering/src/gpu_types.rs`, `rendering/src/instancing.rs` | `MaterialParamsData`, `intern_material` |
| Surface evaluation | `assets/shaders/mesh.slang`, `assets/shaders/lighting.slang` | `evalSurface`, `SurfaceData`, `evalLighting` |

## Related

- [Node-graph codegen](../node-graph-codegen/) explains foldable graphs and generated shader variants.
- [Materials and PSOs](../material-and-pso-selection/) covers per-submesh pipeline selection.
- [Übershader](../ubershader-and-specialization/) covers the fixed shader permutations.
- [Bindless textures](../bindless-textures/) explains the texture indices stored in the parameter table.
