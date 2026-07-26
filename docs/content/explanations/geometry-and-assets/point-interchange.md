+++
title = 'Point interchange'
weight = 22
math = true
+++

# Point interchange

Plant placement often starts somewhere else: a Houdini scatter, a layout published as instanced
points. Interchange brings those points in as authored anchors and writes them back out through the
canonical point schema, so an imported point is an ordinary authored plant rather than a second kind
of object the runtime has to know about.

## Nothing rides along unread

A source attribute the canonical point vocabulary cannot express is reported, never kept. Carrying it
as an opaque blob would make it a second truth: something the file says about a plant that no engine
system reads, that no validation checks, and that the next export would faithfully echo back as if it
meant something.

```sh
sa vegetation-import-points '{"map":"Forest","layer":"7","path":"/scatter/oaks.geo",
  "prototypes":[{"name":"oak","family":"1024"}],"expectedGeneration":"12"}'
#   anchors=2048  tiles=9  prototypes=1  unsupported=["Cd","v"]
```

The attributes that do map are position, orientation, scale, a stable id, and the prototype name.

| Source | Reads | Prototype is | Stable id is |
|---|---|---|---|
| Houdini JSON `.geo` | `P`, `orient`/`rot`, `scale`/`pscale`, `id`, `name`/`variant` | the `name` string | the `id` attribute, else the point index |
| USD `.usda` | `PointInstancer` `positions`, `orientations`, `scales`, `protoIndices`, `ids`, `invisibleIds` | the last component of each prototype path | the `ids` array, else the array position |
| glTF `.gltf`/`.glb` | `EXT_mesh_gpu_instancing` `TRANSLATION`, `ROTATION`, `SCALE`, `_ID` | the instanced node's name | the `_ID` attribute, else the instance index |

Houdini spells uniform scale `pscale` and non-uniform scale `scale`; both may be present, and the
uniform one multiplies the other. A glTF instance transform is in its node's local space, so it
composes with the node's own transform and its whole ancestor chain: a scatter parented under a scaled
group is scaled.

Two USD conventions are easy to get wrong and are handled explicitly. A `quatf` orientation is WXYZ,
not the XYZW every other seam here uses. And `invisibleIds` masks by the instancer's own `ids` rather
than by array position, so an instancer that reorders its arrays keeps masking the same instances.

A masked instance is not a plant, so it becomes no anchor. Its identity lives in the source, which is
what lets unmasking bring back the same plant instead of a new one — and because identities are content
-derived rather than slot-derived, masking one instance cannot renumber or disturb another's GPU slot.

Reading USD needs no USD runtime: the text form states the arrays directly, and the subset a plant
scatter uses is small and stable.

## Identity is what makes a round trip safe

Each instance's identity comes from the authored layer and the source's own stable id. Re-importing an
updated scatter therefore re-addresses the same plants instead of duplicating them, and an authored
override keyed to one of those identities still finds its plant.

Two instances claiming one identity are refused rather than merged: silently collapsing them would
lose a plant an artist can see in the file. A sparse mask deactivates instead of deleting, so an
inactive instance keeps its identity and returns when the mask changes.

$$\text{id} = \operatorname{SHA256}(\text{layer} \mathbin\Vert \text{stable id})_{[0..16)}$$

## Bounds come from the family

An instance's position quantizes to world ticks and its quaternion to signed normalized lanes. Its
bounds do not come from the source at all — they come from the plant family's own dimensions scaled by
the instance, because a content-creation tool's idea of a bounding box is not the engine's and a wrong
one would break culling rather than merely look odd.

Anchors land in the tile of their own position, one authored chunk per tile, exactly as a brush
gesture's anchors do. An import replaces the layer's anchors for the tiles it touches and leaves every
other authored row in those chunks alone.

```sh
sa vegetation-export-points '{"map":"Forest","layer":"7","path":"/scatter/oaks-back.geo"}'
#   instances=2048  prototypes=1
```

The destination extension picks the writer, `.geo` or `.usda`, and an extension with no writer is
refused by name rather than guessed at. Export writes the prototypes under their families' catalog
names and every instance under the stable id it arrived with, so the file that comes back addresses the
same plants. Mutable `.svegcell` state is
never exported as authoring truth: the round trip is over authored anchors.

## In the code

| What | File | Symbols |
|---|---|---|
| Vocabulary and anchor mapping | `vegetation/src/interchange.rs` | `PointInterchange`, `interchange_to_anchors`, `anchors_to_interchange` |
| Stable identity | `vegetation/src/interchange.rs` | `interchange_plant_id` |
| Houdini point clouds | `vegetation/src/interchange.rs` | `read_houdini_points`, `write_houdini_points` |
| USD point instancers | `vegetation/src/interchange_usd.rs` | `read_usd_point_instancers`, `write_usd_point_instancer` |
| glTF instancing | `geometry/src/gltf_instancing.rs` | `read_gltf_instancing`, `GltfInstanceSet` |
| glTF to the vocabulary | `vegetation/src/interchange.rs` | `gltf_instancing_to_interchange` |
| Control surface | `control/src/commands_asset.rs` | `register_interchange_commands` |

## Related

- [Vegetation assets](../vegetation-assets/) — the authored map, its layers, and its anchor chunks
- [Botanical graph](../botanical-graph/) — the plant families an imported point places
- [Vegetation cooking](../vegetation-cooking/) — the cooker every authored anchor normalizes through
