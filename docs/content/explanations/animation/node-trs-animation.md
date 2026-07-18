+++
title = 'Node-TRS animation'
weight = 7
+++

# Node-TRS animation

Node-TRS animation plays a clip against plain scene-graph nodes, driving their translation,
rotation, and scale. A [glTF](https://github.com/KhronosGroup/glTF/tree/main/specification/2.0)
file that opens a door or orbits a moon animates nodes this way, with no skin involved; the
Khronos sample
[BoxAnimated](https://github.com/KhronosGroup/glTF-Sample-Assets/blob/main/Models/BoxAnimated/README.md)
is the canonical case. The engine drives these clips through the same player, sampler, and
pose-override seam as skeletal playback.

## One track model, two targets

A clip holds bone tracks, node tracks, and morph-weight tracks side by side. Each track declares
what it drives with an [`AnimTarget`](../animation-data-model/):

```rust
pub enum AnimTarget {
    Bone = 0, // a skinned-mesh joint, resolved to a bone index by name at import
    Node = 1, // a plain scene-graph node, bound by durable name at runtime
}
```

At import, `decode_clips` classifies each glTF channel. A channel whose target node is a skin
joint decodes as a `Bone` track carrying the joint's position in the skin; every other node
decodes as a `Node` track with `index = -1`, bound by the node's name. A morph-weights channel is
always a `Node` track with `AnimPath::Weights`.

The [sampler](../animation-data-model/) is shared: Step, Linear, and CubicSpline interpolation,
with a slerp for rotation. The two targets differ only in where the sampled local transform
lands.

## A live entity forest

glTF import decodes the document's nodes into a forest (`build_node_forest`), and a mesh-bearing
node carries its mesh in `ImportedNode.mesh`. `spawn_model` dispatches on shape. A skin takes the
rigged path, and a single identity-transform root with no clips collapses to one entity.
Everything else spawns one live `Transform` + `Relationship` entity per node, parented by uuid
under a container root: a multi-node forest, a non-identity root, or any model that carries
animation.

An animated single node never collapses, because the clip needs an `AnimationPlayer` and
`spawn_node_forest` puts the one player on the container root. Each node's local TRS survives as
a real entity's `Transform` rather than being baked into vertices, so a clip has something to
drive.

## Binding by name

`tick_animation` treats an `AnimationPlayer` without a `SkinnedMesh` as a node rig. Each frame,
`tick_node_rig` collects the clip's distinct node-track target names and resolves each to an
entity. `resolve_node_targets` caches the resolved `Entity` per name, keyed by the player
entity's stable id; a stale handle re-resolves through `find_named_descendant`, a first-match
pre-order walk scoped to the player's own subtree. The walk is never the global
`find_entity_by_uuid`: two instances of the same model repeat every node name, and a global scan
could bind one instance's clip to the other's nodes.

A resolved track samples into the entity's `PoseOverride`, the same runtime-only component a bone
track writes on a joint. `local_matrix` prefers the override over the authored `Transform`, so
the rest pose stays untouched and a stopped clip reverts the forest (`clear_node_overrides`).
Node rigs run the full [transition path](../playback-runtime/): cross-fade and inertialization
apply over the bound node entities exactly as over bones. A weights track on a node writes a
`MorphWeightOverride` instead; [morph targets](../morph-targets/) covers that channel.

## One playback surface

The node forest's player is an ordinary `AnimationPlayer`, so the transport commands drive it
unchanged; there is no node-specific playback verb:

```sh
sa list-clips <model>                 # {id, name, duration} per clip
sa play-animation <root> <clip> --loop
sa list-clip-bindings <root> <clip>   # channels resolved against the live forest
```

`list-clip-bindings` labels each channel with the bound entity's current name, resolved by a name
walk over the whole model forest (`model_root_of`), so a leaf selection still resolves every
channel. An unresolved channel falls back to the raw glTF node name, which the editor shows as
the broken-binding signal.

## In the code

| What | File | Symbols |
|---|---|---|
| Track target model | `geometry/src/types.rs` | `AnimTarget`, `AnimTrack` |
| glTF channel classification | `geometry/src/gltf_import.rs` | `decode_clips`, `build_node_forest` |
| Forest spawn + collapse rule | `assets/src/spawn.rs` | `spawn_model`, `spawn_node_forest`, `is_single_identity_root` |
| Name binding + node tick | `animation/src/runtime.rs` | `tick_node_rig`, `resolve_node_targets`, `find_named_descendant` |
| Override composition | `scene/src/hierarchy.rs`, `scene/src/component.rs` | `local_matrix`, `PoseOverride` |
| Binding inspection | `control/src/commands_animation.rs` | `channels_of`, `find_named_in_forest` |

## Related

- [Animation data model](../animation-data-model/) — the shared clip, track, and sampler types
- [Playback runtime](../playback-runtime/) — the player, transitions, and the override write seam
- [Morph targets](../morph-targets/) — weight channels bound through the same node names
- [Timeline](../timeline/) — the editor panel that drives the same player
