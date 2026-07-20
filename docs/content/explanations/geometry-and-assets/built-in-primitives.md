+++
title = 'Built-in primitives'
weight = 11
+++

# Built-in primitives

A built-in primitive is geometry the engine generates itself: a cube, a plane, or a UV sphere,
referenced by a reserved mesh id instead of a catalog row. Every other mesh reaches a scene
through an imported file. A primitive comes from a function, so "add a cube" needs no project
loaded and writes nothing to the asset catalog.

## Reserved ids, not catalog rows

Ids below `1024` are reserved (`RESERVED_BELOW` in `saffron-core`); `Uuid::new` mints ids only at
or above it, so a reserved constant can never collide with a minted asset id. The default
material holds id `1` and the asset-preview floor mesh id `2`; the primitives take the next three
slots:

```rust
pub const BUILTIN_CUBE_MESH_ID: Uuid = Uuid(3);
pub const BUILTIN_PLANE_MESH_ID: Uuid = Uuid(4);
pub const BUILTIN_SPHERE_MESH_ID: Uuid = Uuid(5);

pub enum BuiltinMesh { Cube, Plane, Sphere }
```

A primitive entity carries an ordinary [`Mesh`](../../scene-and-ecs/built-in-components/)
component whose `mesh` field holds one of these ids, plus a `MaterialSet` with one default slot;
slot material `0` resolves to the built-in default material. The id serializes into scene JSON as
the decimal string `"3"`, like any minted id. `BuiltinMesh::from_reserved_id` is the single
reserved-id → primitive decoder, and the editor mirrors it as its `BUILTIN_MESHES` list.

## Generated geometry

`saffron_geometry::{cube, plane, uv_sphere}` return the same interleaved `Mesh` every importer
produces: position / normal / uv0 / tangent vertices, 32-bit indices, one submesh (see
[vertex layout](../mesh-and-vertex-layout/)). `compute_tangents` fills the tangents with
[Lengyel's method](https://terathon.com/blog/tangent-space.html), UV-aligned with the ±1
bitangent handedness in `w`, so a primitive carries the same per-vertex frame as an imported
mesh.

| Primitive | Shape | Vertices | Triangles |
|---|---|---|---|
| Cube | edge 1 (`±0.5`), hard normals, 4 vertices per face | 24 | 12 |
| Plane | 1×1 on XZ, facing +Y | 4 | 2 |
| Sphere | radius 1, 32 rings × 48 sectors, normals equal positions | 1617 | 3072 |

All three are origin-centered and unit-scaled; the entity's `Transform` sizes them. Triangles
wind counter-clockwise as seen from outside, matching the renderer's
`FrontFace::COUNTER_CLOCKWISE` state and its back-face cull for solid materials.

The dense preview sphere (`preview_displacement_sphere`, 192 rings × 288 sectors under reserved
id `7`) is a separate reserved mesh, not a spawnable primitive. The interactive material and
texture previews shade on it so a displacement map moves real vertices into a true silhouette.
The HDRI previews render their probe balls on the built-in sphere itself, varying only per-slot
material overrides.

## Seeded on demand

`AssetServer::load_mesh_asset` resolves a mesh id cache-first. A reserved primitive id has no
catalog row: on a cache miss, `seed_builtin_mesh` generates the geometry, uploads it, and caches
it under the reserved id without consulting the catalog. A project load clears the GPU caches,
and the next resolve re-seeds the primitive, so no eager bookkeeping tracks which primitives a
scene uses.

No signed-distance field is baked for a primitive (the preview floor mesh follows the same rule),
and no file is read, so a primitive spawns even with no project open. A failed upload
negative-caches under the reserved id; `clear_asset_caches` drops the marker and the next resolve
retries.

## Spawning and picking

The `add-entity` control command's `cube` / `plane` / `sphere` presets attach the `Mesh` +
`MaterialSet` pair directly, and the editor's Create menu drives the same presets through the
typed client:

```sh
sa add-entity cube
# Cube  id=13980406960281953981
```

The Inspector's mesh picker lists the three primitives in a fixed "Built-in" group above the
catalog rows (mesh fields only), and its trigger shows a "Built-in" chip when the selected id is
reserved. A primitive is choosable anywhere a mesh is, yet it never appears in the Assets grid:
`list-assets` reads only the catalog.

## In the code

| What | File | Symbols |
|---|---|---|
| Geometry generators | `geometry/src/primitives.rs` | `cube`, `plane`, `uv_sphere`, `push_quad` |
| Tangent computation | `geometry/src/types.rs` | `compute_tangents`, `Vertex` |
| Reserved-range mint | `core/src/uuid.rs` | `RESERVED_BELOW`, `Uuid::new` |
| Reserved ids + enum | `assets/src/lib.rs` | `BUILTIN_CUBE_MESH_ID`, `BuiltinMesh`, `from_reserved_id` |
| On-demand seed + resolve | `assets/src/load.rs` | `load_mesh_asset`, `seed_builtin_mesh` |
| Spawn presets | `control/src/commands_scene.rs` | the `add-entity` handler |
| Preset wire enum | `protocol/src/dto.rs` | `AddEntityPreset` |
| Picker group + chip | `editor/src/components/AssetPicker.tsx` | `BUILTIN_MESHES`, `BuiltinChip` |
| Create menu | `editor/src/app/CreateMenu.tsx` | `CREATE_PRESETS`, `CreateMenu` |

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the interleaved `Vertex` the generators fill
- [Mesh upload](../gpu-mesh-upload/) — how the seeded geometry reaches the GPU
- [Asset catalog](../asset-server-and-catalog/) — the cache a primitive rides without a row
- [Components](../../scene-and-ecs/built-in-components/) — the `Mesh` + `MaterialSet` a primitive carries
