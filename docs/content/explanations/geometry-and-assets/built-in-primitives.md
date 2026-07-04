+++
title = 'Built-in primitives'
weight = 11
+++

# Built-in primitives

A built-in primitive is native geometry the engine generates itself — a cube, a plane, or a
sphere — that an entity references like any mesh but that is never a project asset. Dropping a
primitive into a scene needs no project loaded and adds no rows to the asset catalog.

Every other mesh comes from importing a file; a primitive comes from a function. Keeping the
three shapes native means "add a cube" is a one-component spawn, not an import that bakes a
model, a mesh, and a material into the catalog and leaves them there.

## Reserved ids, not catalog rows

UUIDs below `1024` are reserved (`RESERVED_BELOW`); `Uuid::new` only ever mints ids at or above
it, so a small constant can never collide with a real asset. The default material (`1`) and the
asset-preview floor (`2`) already use this range; the primitives extend it:

```rust
pub const BUILTIN_CUBE_MESH_ID:   Uuid = Uuid(3);
pub const BUILTIN_PLANE_MESH_ID:  Uuid = Uuid(4);
pub const BUILTIN_SPHERE_MESH_ID: Uuid = Uuid(5);

pub enum BuiltinMesh { Cube, Plane, Sphere }
```

A primitive entity carries a normal [`Mesh`](../../scene-and-ecs/built-in-components/) component
whose `mesh` is one of these reserved ids, plus a default `MaterialSet` slot. Nothing about the
component, the scene JSON, or the draw path is special — the id serializes as the decimal string
`"3"`, and `BuiltinMesh::from_reserved_id` is the single `< 1024 → primitive` decoder (mirrored
by a constant on the editor side).

## Seeded on demand

The geometry lives in one place: `saffron_geometry::{cube, plane, uv_sphere}` produce the same
position / normal / uv0 `Mesh` every importer produces (tangents are derived per-fragment, so the
[vertex layout](../mesh-and-vertex-layout/) carries none). Winding is counter-clockwise as seen
from outside, matching the renderer's `FrontFace::COUNTER_CLOCKWISE` back-face cull. The material
thumbnail preview renders on this same `uv_sphere`, so there is exactly one sphere in the engine.

`AssetServer::load_mesh_asset` resolves a mesh id cache-first. A reserved primitive id misses the
catalog by design, so on a cache miss the loader generates its geometry, uploads it, and caches
it under the reserved id — no catalog interaction. Because the resolve is cache-first and the
seed happens on miss, a primitive re-seeds automatically after a project-load cache clear and
needs no eager bookkeeping. No SDF is baked (like the preview floor), so a primitive spawns even
with no project open.

## In the editor

The Inspector's mesh picker lists the three primitives in a fixed **Built-in** group above the
catalog rows, and the trigger shows a **Built-in** chip when the selected mesh is a reserved id.
Primitives are choosable everywhere a mesh is, but they never appear in the Assets grid because
`list-assets` stays catalog-only. The `add-entity` control command grows a `cube` / `plane` /
`sphere` preset that spawns the native `Mesh` + `MaterialSet` directly.

## In the code

| What | File | Symbols |
|---|---|---|
| Geometry generators | `geometry/src/primitives.rs` | `cube`, `plane`, `uv_sphere` |
| Reserved ids + enum | `assets/src/lib.rs` | `BUILTIN_*_MESH_ID`, `BuiltinMesh` |
| On-demand seed + resolve | `assets/src/load.rs` | `load_mesh_asset`, `seed_builtin_mesh` |
| Native spawn preset | `control/src/commands_scene.rs` | `add-entity` (`AddEntityPreset`) |
| Picker group + chip | `editor/src/components/AssetPicker.tsx` | `BUILTIN_MESHES`, `BuiltinChip` |

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the `Mesh` the generators build
- [Mesh upload](../gpu-mesh-upload/) — how the seeded geometry reaches the GPU
- [Asset server & catalog](../asset-server-and-catalog/) — the cache a primitive rides without a row
- [Built-in components](../../scene-and-ecs/built-in-components/) — the `Mesh` + `MaterialSet` a primitive carries
