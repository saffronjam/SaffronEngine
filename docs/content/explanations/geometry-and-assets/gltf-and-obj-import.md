+++
title = 'Model import'
weight = 2
+++

# Model import

Model import reads a 3D model file and converts it into the engine's own geometry: a
[`Mesh`](../mesh-and-vertex-layout/) plus a table of `ImportedMaterial`s, one per source
material. Two source formats are supported — glTF and OBJ — and each has its own parser, but
both produce the same `ImportedModel`.

The format is chosen by file extension, and the caller never sees which parser ran. glTF
goes through the `gltf` crate, OBJ through `tobj`. Every fallible step returns
`Result<_, Error>`, so a parse failure becomes an `Err` rather than a panic, matching the
engine's [error-as-value rule](../../core-and-conventions/error-handling/).

## Dispatch by extension

`translate_model` parses a model file and returns an `ImportedModel` (mesh + material table,
plus an optional skin payload). It branches on a case-insensitive suffix check: `.gltf`/`.glb`
route to the glTF importer, `.obj` to the OBJ importer; any other extension returns an `Err`.

## glTF through the `gltf` crate

`gltf::import` parses the JSON and loads the buffers; a failure returns `Err`. The importer
walks every mesh's triangle primitives and reads each into a fresh submesh via
`append_primitive`. Attributes are looked up by semantic:

- `POSITION` is required; a primitive without it is skipped.
- `NORMAL` and `TEXCOORD_0` are optional.

Each primitive gets a `vertex_offset` equal to the current vertex count, so its indices stay
zero-based against its own block. A primitive with no index buffer gets a synthesized
`0..vertex_count` sequence. One source mesh with several primitives becomes several submeshes
over the shared buffers, and each submesh's `material_slot` is set to the slot of its glTF
material (deduplicated in first-seen order).

## OBJ through `tobj`

`tobj::load_obj` resolves the `.mtl` and its textures relative to the OBJ's own directory.
OBJ stores position, normal, and texcoord as three independent index streams, so the same
`(v, vn, vt)` triple can recur; a `BTreeMap` keyed on the `[i32; 3]` triple collapses
duplicates into unique vertices. The ordered map is deliberate, not a `HashMap`: it emits
the deduplicated vertices in a deterministic order across runs, so the subsequent
[`.smesh`](../smesh-format/) bake is byte-stable.

```rust
let key = [index.vertex_index, index.normal_index, index.texcoord_index];
let slot = unique_vertices.entry(key).or_insert_with(|| /* push a new vertex */);
```

An OBJ shape can mix materials across its faces, so the importer groups faces by their
`material_id` (`tobj` triangulates by default, giving one id per triangle) and emits one
submesh per material, each tagged with its slot. Because the indices already point into the
shared array, OBJ submeshes leave `vertex_offset` at 0, the opposite choice from glTF. OBJ's
texture V origin is bottom-left while Vulkan samples top-left, so the importer flips V on
read (`1.0 - v`). glTF needs no flip.

## Missing normals

Both paths share a fallback. `any_normals_present` scans the assembled mesh, and if every
normal is near-zero, `generate_normals` recomputes smooth per-vertex normals by summing the
cross-product face normals of each triangle and normalizing. A vertex with no contributing
face falls back to `+Y`.

## Skeletal clips

When a glTF declares a skin, the importer also walks its animations through `decode_clips`.
Each animation becomes an `AnimClip`, and each of its channels an `AnimTrack`: the channel's
target node is matched to a joint by its position in the skin's joint list, and its sampler's
keyframe times and values are read into the flat `times`/`values` arrays the sampler expects.
A track records both that joint index and the node's name, so a later reimport can re-resolve
a stale index. The decoded clips ride on the skin payload (`SkinPayload.animations`); the
[import pipeline](../import-pipeline/) bakes each to a [`.sanim`](../sanim-format/) chunk and
registers it as an `AssetType::Animation` catalog entry. The mechanics of the clip and track
types are the [animation data model](../../animation/animation-data-model/).

## The material table

Both importers build a `Vec<ImportedMaterial>`, one entry per distinct source material, in
first-seen order. Each `Submesh::material_slot` indexes the table.

```rust
pub struct ImportedMaterial {
    pub name: String,
    pub base_color: Vec4,
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: Vec3,
    pub emissive_strength: f32,
    pub albedo: Option<TextureSource>,             // sRGB color
    pub metallic_roughness: Option<TextureSource>, // roughness=G, metalness=B; linear
    pub normal: Option<TextureSource>,             // linear
    pub occlusion: Option<TextureSource>,          // AO in R; linear
    pub emissive_tex: Option<TextureSource>,       // sRGB
}
```

Each optional texture is one `Option<TextureSource>` — the encoded (png/jpg) bytes plus their
extension — so a presence flag can never disagree with the bytes. `extract_gltf_material`
reads each material's base-color/metallic/roughness/emissive factors (with the
`KHR_materials_emissive_strength` multiplier) and the base-color, metallic-roughness, normal,
occlusion, and emissive textures via `read_texture_bytes` (from an embedded buffer view or an
external file resolved next to the glTF, percent-decoding the URI). `extract_obj_material`
reads `diffuse`, the `Pm`/`Pr` MTL keys, `emissive`, and `diffuse_texture` per material. The
encoded texture bytes are carried as-is; decoding happens later, in
[image decoding](../image-decoding/).

The downstream [import pipeline](../import-pipeline/) bakes each material into the `.smodel` as an
`SMAT` chunk (textures colorspace-tagged per role) and lowers the table into the scene as a
[`MaterialSet`](../../scene-and-ecs/built-in-components/) whose slots **reference** those baked
`.smat` chunks — one slot per source material, so editing a baked material re-renders every instance.

## In the code

| What | File | Symbols |
|---|---|---|
| Extension dispatch | `geometry/src/translate.rs` | `translate_model` |
| glTF parse + walk | `geometry/src/gltf_import.rs` | `import_gltf_model`, `append_primitive` |
| Skeletal clip decode | `geometry/src/gltf_import.rs` | `decode_clips`, `build_skin` |
| OBJ parse + dedup | `geometry/src/obj_import.rs` | `import_obj_model` |
| Missing-normal fallback | `geometry/src/picking.rs`; `geometry/src/gltf_import.rs` | `generate_normals`, `any_normals_present` |
| Material extraction | `geometry/src/gltf_import.rs`; `geometry/src/obj_import.rs` | `ImportedMaterial`, `extract_gltf_material`, `read_texture_bytes`, `extract_obj_material` |

> [!NOTE]
> A glTF texture embedded as a `data:` URI is logged and skipped; the importer imports the
> geometry without that texture. Embedded buffer-view images and external files both work.

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the common output
- [Image decoding](../image-decoding/) — where the texture bytes get decoded
- [Import pipeline](../import-pipeline/) — what calls these and bakes the result
- [Error handling](../../core-and-conventions/error-handling/) — the error-as-value boundary
