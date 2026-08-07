+++
title = 'Scene hierarchy'
weight = 8
math = true
+++

# Scene hierarchy

The scene hierarchy turns ECS entities into a forest of parented transforms. Each entity stores one
durable parent identifier, while resolved handles and world matrices remain runtime caches. This
keeps saved references stable even though `hecs` handles change when a scene is loaded or copied.

## Durable links and runtime caches

`Relationship` contains the parent's `Uuid`, a resolved `parent_handle`, and a `children` vector. A
parent value of `0` marks a root. Only the UUID is serialized:

```json
{
  "Relationship": {
    "parent": "142883415"
  }
}
```

`Scene::relink_hierarchy` rebuilds both handle caches after loading or changing structure. It first
maps every entity UUID to its live handle, then resolves parent links and fills each parent's child
list. Missing relationships become roots. Dangling links, self-parenting, and cycles are reset to
root with a warning, so every subsequent traversal sees a valid forest.

The same pass resolves `SkinnedMesh::bone_handles` from the skin's ordered bone UUIDs. Neither
relationship handles nor bone handles cross a save/load boundary.

## World transforms

`Transform` stores local translation, Euler rotation, and scale. `update_world_transforms` starts at
every root and follows the cached child lists. For an entity $e$ with parent $p$, it composes:

$$
W_e = W_p L_e
$$

where $L_e$ is the entity's local matrix. The pass carries a full `Mat4`, so parent rotation and
non-uniform scale remain part of the child's world transform. `WorldTransform` is unregistered and
therefore absent from scene documents.

The [scene mutation journal](../scene-mutation-journal/) records local, hierarchy, and published
world revisions. Composition skips a clean subtree and propagates a changed parent revision through
descendants once. Each published matrix retains its preceding value and revision for temporal
consumers.

Animation can supply a runtime `PoseOverride`. `local_matrix` uses that quaternion-based TRS while
the override exists, otherwise it uses the authored `Transform`. Consumers read the cached result
through `world_matrix`, `world_translation`, and `world_rotation`; `world_matrix` composes the parent
chain directly if the cache is missing.

## Reparenting

`set_parent(child, new_parent, keep_world)` is the structural mutation entry point. It rejects stale
handles, self-parenting, and any new parent whose ancestry already contains the child. Passing
`None` detaches the child to a root.

The editor reparents with `keep_world = true`. Before changing the link, the scene composes the
child's exact world matrix. It then derives a new local matrix under the target parent:

$$
L'_e = W_{newParent}^{-1} W_e
$$

`set_local_from_matrix` decomposes the result into translation, rotation, and scale. The rotation
uses the engine's stable ZYX extraction before storage in `Transform`. A transform cannot represent
shear, so decomposition drops shear introduced by a rotated, non-uniformly scaled parent.

Destroying an entity destroys its subtree. `destroy_entity` collects descendants before despawning
anything, removes the root of that subtree from its parent's child cache, then despawns the collected
handles.

## Model forests and skeletons

An imported [glTF model](https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html) may place meshes,
an animation player, and joints on different entities under a container root. Hierarchy helpers
therefore resolve a model as a subtree rather than assuming its root carries every component.

- `model_mesh_entities` returns every mesh-bearing entity in the subtree.
- `model_rig_entity` finds the first entity with `SkinnedMesh`.
- `model_morph_entity` finds the entity with `MorphComponent`.
- `model_player` resolves the model root, then its animation authority.
- `model_root_of` walks upward from a picked child to its `ModelInstance` root.

Skeleton joints are ordinary entities tagged with `Bone`. After world transforms are updated,
`joint_matrices` constructs the skinning palette in imported joint order:

$$
J_i = W_{bone_i} B_i^{-1}
$$

Here $B_i^{-1}$ is the imported inverse-bind matrix. Animation, reparenting, and physics all affect
skinning through the same hierarchy composition.

## In the code

| What | File | Symbols |
|---|---|---|
| Relationship data | `scene/src/component.rs` | `Relationship`, `WorldTransform`, `SkinnedMesh` |
| Cache rebuild and transforms | `scene/src/hierarchy.rs` | `relink_hierarchy`, `update_world_transforms`, `world_matrix` |
| Reparenting | `scene/src/hierarchy.rs` | `set_parent`, `set_local_from_matrix`, `compose_world_matrix` |
| Subtree destruction | `scene/src/scene.rs` | `destroy_entity`, `subtree_entities` |
| Model subtree resolution | `scene/src/hierarchy.rs` | `model_mesh_entities`, `model_rig_entity`, `model_player`, `model_root_of` |
| Relationship serialization | `scene/src/serde.rs` | `SceneSerialize for Relationship` |

## Related

- [Transform and matrices](../transform-and-matrices/) — local TRS conventions and matrix composition.
- [Scene serialization](../scene-serialization/) — document structure and UUID preservation.
- [Scene mutation journal](../scene-mutation-journal/) — dirty propagation and transform revisions.
- [Animation](../../animation/) — runtime pose overrides and joint palettes.
- [Picking](../picking/) — resolving a hit child to its model root.
