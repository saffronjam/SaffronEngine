+++
title = 'ECS architecture'
weight = 1
+++

# ECS architecture

An entity-component-system (ECS) is a data-oriented architecture: game state is plain data in tight
storage, entities are identifiers that group components, and logic runs as functions over that data.
The layout suits a renderer that walks thousands of objects per frame.

The scene crate builds its world on [`hecs`](https://docs.rs/hecs/0.11/hecs/) for storage, but that
choice stays wrapped. `Scene` owns a private `hecs::World`, and downstream operations go through
methods on `Scene`. Other crates depend on the scene access contract rather than the storage API.

## The world is a struct

`Scene` holds the `hecs::World`, the scene-wide [environment](../asset-catalog-in-scene/), and an
optional shared handle to the project's [asset catalog](../asset-catalog-in-scene/), so the
registry-driven inspector can resolve mesh and material ids to names. `Entity` is a copyable handle
that wraps the storage's generational id, so it never dangles against a relocated `Scene`; a handle
that outlives its entity is caught by `valid`.

```rust
pub struct Scene {
    world: hecs::World,
    pub environment: SceneEnvironment,
    pub catalog: Option<Arc<AssetCatalog>>,  // borrowed; set per-frame, never serialized
    revision: SceneRevision,
    journal: VecDeque<SceneMutation>,
}

pub struct Entity(hecs::Entity);
```

`Entity::NULL` is the sentinel non-entity, used by the runtime caches that store a flat
`Vec<Entity>` where an unresolved slot needs a value rather than an `Option`. `valid` reports
`false` for it.

## Operations are methods on the scene

Component access is a set of generic methods on `Scene`, each bounded on `crate::Component` (the
re-exported storage trait, so callers never name the ECS):

```rust
impl Scene {
    pub fn add_component<C: Component>(&mut self, entity: Entity, c: C) -> Result<()>;
    pub fn has_component<C: Component>(&self, entity: Entity) -> bool;
    pub fn remove_component<C: Component>(&mut self, entity: Entity);
    pub fn with_component<C: Component, R>(&self, e: Entity, f: impl FnOnce(&C) -> R) -> Result<R>;
    pub fn with_component_mut<C, R>(&mut self, e: Entity, f: impl FnOnce(&mut C) -> R) -> Result<R>;
    pub fn component<C: Component + Copy>(&self, entity: Entity) -> Result<C>;
}
```

Reads are scoped to a borrow (`with_component` runs a closure against the component rather than
handing out a long-lived reference into the storage), and `component` is the convenience copy-out
for a small `Copy` component. A stale handle or a missing component is a typed
[`Error`](../scene-serialization/), never a panic.

`create_entity` mints a fresh entity already carrying the standard authored set: an `IdComponent`
with a new [`Uuid`](../scene-serialization/), a `Name`, a default `Transform`, a root
`Relationship`, and a `ComponentOrder`. `destroy_entity` removes it and its whole subtree.

Every successful mutation also enters the [scene mutation journal](../scene-mutation-journal/).
Derived renderer and tool state can consume revisioned changes or rebuild from a snapshot when its
cursor falls outside retained history.

## Iteration: for_each over a query

The one iteration primitive is `for_each`, generic over a `hecs` query tuple of component
references. It runs a callback for each entity that carries all of them:

```rust
scene.for_each::<(&Transform, &mut Camera), _>(|entity, (transform, camera)| {
    update_camera(entity, transform, camera);
});
```

The query tuple spells the access exactly: `for_each::<&Transform, _>` reads, `for_each::<(&Transform,
&mut Camera), _>` reads one and mutates the other. A system is just a function that calls `for_each`
with the components it cares about — `render_scene` walks `(&Transform, &Mesh)` to gather
renderables; `primary_camera` walks `(&Transform, &Camera)` to find the first primary camera and
inverts its world matrix into a view. Mutable query members produce journal updates for every matched
entity.

## Why this shape

`hecs` already provides archetype storage, queries, and generational handles. Exposing it directly
would bind every downstream crate to one ECS and scatter `hecs::` across the tree. Keeping the world
field private and the access a fixed method surface lets the same world feed the renderer, the
serializer, and the editor, with none of them owning a privileged ECS API, and keeps the backend
swap to this one crate. Per-component behavior lives in the [component registry](../component-registry/),
which is data, not a trait hierarchy.

## In the code

| What | File | Symbols |
|---|---|---|
| World + handle | `scene/src/scene.rs` | `Scene`, `Entity`, `Scene::valid` |
| Component access | `scene/src/scene.rs` | `add_component`, `has_component`, `remove_component`, `with_component`, `with_component_mut`, `component` |
| Lifecycle | `scene/src/scene.rs` | `create_entity`, `spawn_with_id`, `destroy_entity` |
| Iteration | `scene/src/scene.rs` | `for_each` |
| Mutation tracking | `scene/src/journal.rs` | `SceneMutation`, `SceneJournalCursor`, `SceneJournalRead` |
| Re-exported storage traits | `scene/src/scene.rs` | `Component`, `Query` |
| Camera resolve | `scene/src/hierarchy.rs` | `primary_camera`, `CameraView`, `camera_projection` |
| A system that walks it | `assets/src/render_scene.rs` | `render_scene` |

## Related
- [Component registry](../component-registry/) — per-component behavior as data, not methods
- [Components](../built-in-components/) — the value structs `for_each` iterates
- [Scene mutation journal](../scene-mutation-journal/) — incremental derived-state synchronization
- [Go-flavored design](../../core-and-conventions/go-flavored-design/) — why structs + free-standing methods
