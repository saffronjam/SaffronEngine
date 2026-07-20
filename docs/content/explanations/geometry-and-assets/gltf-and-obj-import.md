+++
title = 'Model import'
weight = 2
+++

# Model import

Model import reads a 3D model file and translates it into the engine's in-memory import graph,
`ImportedModel`. Two source formats are supported:
[glTF](https://github.com/KhronosGroup/glTF) through the [`gltf`](https://docs.rs/gltf) crate and
[Wavefront OBJ](https://paulbourke.net/dataformats/obj/) through [`tobj`](https://docs.rs/tobj).
Each format has its own parser, and both produce the same graph shape.

That shape is uniform: a node forest (`Vec<ImportedNode>` with name, parent index, and local TRS
per node) whose mesh-bearing nodes carry a node-local [`Mesh`](../mesh-and-vertex-layout/), a
table of `ImportedMaterial`s, the decoded `AnimClip`s, and optional skin and morph payloads.
There is no top-level mesh; an OBJ rides a single identity root node. Every fallible step
returns `Result<_, Error>`, per the engine's
[error-as-value rule](../../core-and-conventions/error-handling/).

## Dispatch by extension

`translate_model` branches on a case-insensitive suffix: `.gltf` and `.glb` route to the glTF
importer, `.obj` to the OBJ importer, and any other extension returns an `Err`. The caller
never sees which parser ran.

## glTF into a node forest

`gltf::Gltf::open` parses the document and `gltf::import_buffers` loads its binary buffers.
The crate's `Node` exposes children but no parent, so `build_parents` inverts every node's
child list into a parent index (`-1` for a root). `build_node_forest` then records each node's
name, parent, and local TRS in document order; a matrix transform is decomposed to TRS through
`Affine3A`. Geometry stays node-local with no world-transform bake, so a node an animation
track drives keeps its drivable local transform.

For each mesh-bearing node, `append_primitive` reads every triangle primitive into that node's
local mesh. A non-triangle primitive, or one without `POSITION`, is skipped. `NORMAL`,
`TEXCOORD_0`, `TANGENT`, and the `JOINTS_0`/`WEIGHTS_0` skin pair are optional; absent
attributes fill with zeros.

Each primitive gets a `vertex_offset` equal to the node mesh's current vertex count, so its
indices stay zero-based against its own block. A primitive with no index buffer gets a
synthesized `0..vertex_count` sequence, and an out-of-range index is an `Err`. One glTF mesh
with several primitives becomes several submeshes over the node's shared buffers, each tagged
with a `material_slot` assigned in first-seen order (keyed by the material's document index;
a primitive with no material gets a default slot).

## Normals and tangents

Both paths share a normal fallback. `any_normals_present` scans the assembled mesh, and if
every normal is near-zero, `generate_normals` recomputes smooth per-vertex normals by summing
cross-product face normals and normalizing. A vertex with no contributing face falls back
to `+Y`.

Tangents follow the same keep-or-compute policy. A glTF `TANGENT` accessor is already
UV-aligned with a ±1 handedness in `w`, so it is kept; when any vertex lacks one,
`compute_tangents` rebuilds the frame with
[Lengyel's method](https://terathon.com/blog/tangent-space.html) — UV-gradient accumulation,
then Gram-Schmidt against the normal. A degenerate vertex (no usable UVs, zero-area triangles)
gets a branchless basis from the normal
([Duff et al. 2017](https://jcgt.org/published/0006/01/01/paper.pdf)), so every tangent is
finite and unit-length. OBJ carries no tangents, so its importer always computes them.

## Skin and clips

A skin payload is decoded only when the document's first skin covers every triangle primitive.
A model mixing skinned and unskinned primitives would deform its unweighted vertices to the
origin, so it imports as plain geometry with a warning. `build_skin_desc` reads the joint node
indices (in `jointMatrices[]` order), the inverse-bind matrices, the skeleton root, and the
skinned mesh node into an `ImportedSkin`; `SkinPayload` pairs it with the per-vertex
joint/weight stream.

`decode_clips` turns each glTF animation into an `AnimClip` of heterogeneous tracks, skinned
or not — the clips are top-level on `ImportedModel::animations`. A channel targeting a skin
joint becomes a bone track keyed by its position in the joint list; any other node becomes a
node track bound by name; a morph-weights channel becomes a weights track carrying the N-wide
weight stream. Sampler keyframes land in flat `times`/`values` arrays (a cubic-spline sampler
stores three values per key), and the clip's duration is the latest track end. The track and
clip types themselves are the [animation data model](../../animation/animation-data-model/).

## Morph targets

Each primitive's morph targets are read as dense position/normal delta streams, then compacted:
a delta below a squared magnitude of `1e-12` is dropped, and the survivors are stored sparsely
with their vertex index shifted by the primitive's base. One mesh-global `MorphData` is kept from
the first mesh-bearing node that has targets.

`finalize_morph` reconciles the target count against the mesh-level rest weights and seeds each
target's `rest_weight`. It preserves names from the glTF exporter convention
`mesh.extras.targetNames`, using `morph_{k}` only when a target has no authored name. [Morph
targets](../../animation/morph-targets/) covers what the deltas drive at runtime.

## OBJ through tobj

`tobj::load_obj` runs with explicit `LoadOptions { triangulate: true, single_index: false }`
and resolves the `.mtl` next to the OBJ; a missing or broken `.mtl` does not fail the geometry
load. OBJ stores position, normal, and texcoord as three independent index streams, so the
same `(v, vn, vt)` triple can recur across faces. `resolve_vertex` collapses duplicates
through a `BTreeMap` keyed on the triple:

```rust
let key = [vertex_index, normal_index, texcoord_index];
if let Some(&existing) = unique_vertices.get(&key) {
    return Ok(existing);
}
```

The ordered map is deliberate: unlike a `HashMap`, it emits the deduplicated vertices in the
same order on every run, so the bytes of the subsequent [`.smesh`](../smesh-format/) bake stay
stable. A re-import-determinism test pins that choice. OBJ's texture V origin is bottom-left
while Vulkan samples top-left, so the importer flips V on read (`1.0 - v`); glTF needs no flip.

Faces are grouped into first-seen material slots by a `SlotMap` keyed on the tobj material id
(`-1` for none), so the same material in two shapes merges into one slot and one submesh.
Because the indices already point into the shared vertex array, OBJ submeshes leave
`vertex_offset` at 0, the opposite choice from glTF. The finished mesh rides a single identity
root `ImportedNode` named after the file stem, so it spawns exactly like a single-node glTF.

## The material table

Both importers build a `Vec<ImportedMaterial>` in first-seen order, always at least one entry
(a default material when the source declares none). Each `Submesh::material_slot` indexes it.

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
    pub alpha_mode: AlphaMode,                     // Opaque | Mask | Blend
    pub alpha_cutoff: f32,
    pub double_sided: bool,
}
```

Each optional texture is one `Option<TextureSource>`, the encoded png/jpg bytes plus their
extension, so a presence flag can never disagree with the bytes. `extract_gltf_material` reads
the PBR factors, the `KHR_materials_emissive_strength` multiplier, the `alphaMode`/
`alphaCutoff`/`doubleSided` flags, and five textures via `read_texture_bytes`, which pulls from
an embedded buffer view or an external file resolved next to the glTF (percent-decoding the
URI).

`extract_obj_material` reads `diffuse`, `emissive`, and `diffuse_texture`, plus the `Pm`/`Pr`
PBR keys out of tobj's unrecognized-parameter map. An OBJ material seeds metallic and
roughness to 0, matching what tinyobjloader reports when the `.mtl` omits them. Texture bytes
are carried as-is on both paths; decoding happens later, in
[image decoding](../image-decoding/). The [import pipeline](../import-pipeline/) bakes the
whole graph into a [`.smodel` container](../smodel-container/): the mesh, one `.smat` chunk
per material, the texture chunks, and one `.sanim` chunk per clip.

> [!NOTE]
> A glTF texture embedded as a `data:` URI is logged and skipped; the importer keeps the
> geometry without that texture. Embedded buffer-view images and external files both load.

## In the code

| What | File | Symbols |
|---|---|---|
| Extension dispatch | `geometry/src/translate.rs` | `translate_model` |
| glTF parse + node forest | `geometry/src/gltf_import.rs` | `import_gltf_model`, `build_parents`, `build_node_forest`, `append_primitive` |
| Skin + clip decode | `geometry/src/gltf_import.rs` | `build_skin_desc`, `decode_clips` |
| Morph compaction | `geometry/src/gltf_import.rs` | `finalize_morph`, `MORPH_DELTA_EPSILON_SQ` |
| OBJ parse + dedup | `geometry/src/obj_import.rs` | `import_obj_model`, `resolve_vertex`, `SlotMap` |
| Normal + tangent fallbacks | `geometry/src/picking.rs`; `geometry/src/types.rs` | `generate_normals`, `compute_tangents` |
| Material extraction | `geometry/src/gltf_import.rs`; `geometry/src/obj_import.rs` | `extract_gltf_material`, `read_texture_bytes`, `extract_obj_material` |
| Output graph types | `geometry/src/types.rs` | `ImportedModel`, `ImportedNode`, `ImportedMaterial`, `SkinPayload`, `MorphData` |

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the `Mesh`/`Submesh` the importers fill
- [Image decoding](../image-decoding/) — where the texture bytes get decoded
- [Import pipeline](../import-pipeline/) — what calls this and bakes the result
- [The .smodel container](../smodel-container/) — where the baked chunks land
- [Animation data model](../../animation/animation-data-model/) — the clip and track types
- [Error handling](../../core-and-conventions/error-handling/) — the error-as-value boundary
