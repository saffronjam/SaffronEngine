+++
title = 'Animation data model'
weight = 1
math = true
+++

# Animation data model

An animation clip is a bundle of keyframe curves. Sampling every curve at one time yields a
*pose*, a local transform for every joint, and the scene composes that pose into world
matrices. This page covers the layer under playback: the clip/track keyframe model, the
decomposed pose types with their blend primitives, and the samplers that evaluate a curve.

Everything here is pure data and math with no playback state. The clip types live in
`saffron-geometry` beside the mesh formats; the pose types, samplers, and pose algebra live in
`saffron-animation`, an FFI-free crate whose only output toward rendering is a per-bone pose
override that scene composition consumes. The evaluator that advances clips over time is the
[playback runtime](../playback-runtime/).

## Clips, tracks, keyframes

A clip mirrors a [glTF 2.0](https://github.com/KhronosGroup/glTF/tree/main/specification/2.0)
animation losslessly. glTF models an animation as a set of *channels*, each pairing a *sampler*
(input times, output values, an interpolation mode) with a *target* (a node and one of its
animatable properties). `AnimTrack` is exactly one such channel:

```rust
pub struct AnimTrack {
    pub target: AnimTarget,     // Bone | Node — what the track drives
    pub index: i32,             // bone index for a Bone track; -1 for Node/Weights
    pub target_name: String,    // the glTF node name — the durable binding key
    pub path: AnimPath,         // Translation | Rotation | Scale | Weights
    pub interp: AnimInterp,     // Step | Linear | CubicSpline
    pub morph_count: u32,       // weights per keyframe for a Weights track, else 0
    pub times: Vec<f32>,        // sampler input — strictly increasing, seconds
    pub values: Vec<f32>,       // sampler output — flat floats
}
```

One track model carries all three channel kinds. `target` selects a skinned-mesh joint or a
plain scene-graph node, and `path = Weights` marks a morph-weight channel carrying
`morph_count` weights per keyframe. A `Bone` track binds by stable bone index (resolved by
name at import) plus the name itself; a `Node` or morph track binds by durable name alone and
keeps `index = -1`. The [node-TRS](../node-trs-animation/) and
[morph-target](../morph-targets/) pages cover the node and weight kinds.

Entity handles never appear in a track. They are a post-load cache, not stable across a
reload, so the durable name is the key that lets a clip survive a reimport that reorders
joints.

The `values` array is flat, and its stride follows the path: a `Vec3` per key for translation
and scale, a quaternion (`xyzw`) per key for rotation, `morph_count` floats per key for
weights. `CubicSpline` triples the stride, storing *in-tangent, value, out-tangent* per key.
An `AnimClip` is a name, the track list, and the duration (the maximum track end time).

The clip types sit in `saffron-geometry` next to `Vertex` and `Mesh` because Geometry owns the
engine's mesh and file formats: the glTF import fills the tracks, and the
[`.sanim` byte format](../../geometry-and-assets/sanim-format/) (a `SANM` chunk in the
`.smodel` container) persists them. The `AnimPath` / `AnimTarget` / `AnimInterp` discriminants
are pinned `u8` values shared with that format; each `from_u8` maps a byte back through an
explicit `match` and rejects anything out of range.

## Pose types

A pose holds the skeleton's transforms for one instant, kept decomposed with translation,
rotation, and scale as separate fields. That is the form clips sample into, and it blends
cleanly — rotations slerp, where a composed matrix does not.

```rust
pub struct JointPose {
    pub translation: Vec3,
    pub rotation: Quat,    // unit quaternion, glam xyzw order
    pub scale: Vec3,
}
```

`PoseBuffer` is the skeleton-sized container: its `local` vector holds one sampled `JointPose`
per joint, indexed 1:1 with `SkinnedMesh.bones`. The evaluator seeds it with each bone's rest
transform, samples the clip over it, and writes the result onto each driven bone as a
`PoseOverride` component that [world composition](../../scene-and-ecs/transform-and-matrices/)
prefers over the bone's authored `Transform`.

The pose therefore lives beside the scene, not in it. The authored bone `Transform`s keep the
rest pose and are never overwritten, so previewing a clip dirties nothing and needs no
snapshot/restore; removing the override reverts the bone to rest. Scrubbing a clip in Edit
mode relies on exactly this.

`PoseDelta` is the offset form of a pose pair: an additive translation, a delta quaternion
(`from * inverse(to)`), and a multiplicative scale ratio. `pose_diff` builds one from two
poses and `apply_delta` re-applies it scaled by a weight.

## Blend primitives

`blend_joint(base, over, weight)` mixes two poses per joint: translation and scale lerp
componentwise, rotation slerps and renormalizes. The runtime's cross-fade transition is this
blend under a `smoothstep01` alpha; its inertialize mode instead decays a captured `PoseDelta`
under `quintic_decay`, a quintic that reaches zero value, slope, and acceleration together at
the end of the window.

External pose producers write through the same pose seam rather than adding one. Foot IK
solves a leg chain and rewrites its joint rotations in the frame's final pose before the
`PoseOverride` write, and the ragdoll in `saffron-physics` blends its simulated bone
transforms into each bone's `PoseOverride` by an eased per-bone weight after the physics step.
[Foot IK and physics-ahead](../foot-ik-and-physics-ahead/) covers both producers.

## Sampling

`sample_track(track, t)` evaluates one T/R/S curve, returning a `Vec4`: `xyz` holds a
translation or scale, and all four lanes hold a normalized quaternion for rotation.
`locate_keys` binary-searches the strictly increasing times (`partition_point`) for the
segment bracketing `t`, clamping `t` to the first and last key — clips never extrapolate past
their ends. The bracket then interpolates per the track's mode:

- **Step** holds the earlier key's value across the segment.
- **Linear** lerps translation and scale componentwise. Rotation instead uses
  [slerp](https://en.wikipedia.org/wiki/Slerp) between the two quaternion keys and
  normalizes, which keeps constant angular velocity and a unit result.
- **CubicSpline** is a
  [cubic Hermite spline](https://en.wikipedia.org/wiki/Cubic_Hermite_spline). With
  $u = (t - t_0)/(t_1 - t_0)$ and $\Delta = t_1 - t_0$, the value is
  $$p(u) = h_{00}(u)\,p_0 + h_{10}(u)\,\Delta m_0 + h_{01}(u)\,p_1 + h_{11}(u)\,\Delta m_1$$
  with the standard Hermite basis. $m_0$ is the earlier key's out-tangent and $m_1$ the later
  key's in-tangent, both scaled by $\Delta$ as glTF requires; a rotation interpolates its four
  components this way and then normalizes.

An empty track returns the path's identity: a zero translation, an identity quaternion, or a
unit scale. Quaternions ride straight through the whole pipeline — glTF stores `[x, y, z, w]`
and glam's `Vec4` and `Quat` share that lane order, so `Quat::from_vec4` reads a sampled
rotation with no reorder.

`sample_weights(track, t, out)` is the N-wide twin for morph-weight tracks: the same bracket
and the same three modes, run per lane over `morph_count` scalars. Weights are independent
values, so there is no slerp and no normalization; an empty track leaves `out` as the caller
seeded it (the rest weights).

Clip-level sampling belongs to the runtime. `sample_clip_resolved` seeds a `PoseBuffer` with
the rest pose, walks the clip's bone tracks, and re-binds a track by `target_name` whenever
its stored index is stale. Only tracked joints are written, so a joint with no track keeps its
rest value, and a joint animated on one channel keeps the rest value on the others.

The crate's unit tests pin the math with worked numbers: linear endpoints and midpoints are
exact, a 0°→90° rotation samples to 45° at the midpoint, and an asymmetric-tangent cubic bends
its midpoint to $0.75$ where a lerp would give $0.5$.

## In the code

| What | File | Symbols |
|---|---|---|
| Clip + track types | `engine/crates/geometry/src/types.rs` | `AnimClip`, `AnimTrack`, `AnimPath`, `AnimInterp`, `AnimTarget` |
| Pose types | `engine/crates/animation/src/pose.rs` | `JointPose`, `PoseBuffer`, `PoseDelta` |
| Pose algebra | `engine/crates/animation/src/algebra.rs` | `blend_joint`, `pose_diff`, `apply_delta`, `smoothstep01`, `quintic_decay` |
| Track samplers | `engine/crates/animation/src/sample.rs` | `sample_track`, `sample_weights`, `locate_keys` |
| Clip-level sampling | `engine/crates/animation/src/runtime.rs` | `sample_clip_resolved`, `sample_into` |
| `.sanim` byte format | `engine/crates/geometry/src/sanim.rs` | `save_animation_to_buffer`, `load_animation_from_bytes` |
| Pose-override component | `engine/crates/scene/src/component.rs` | `PoseOverride` |

## Related

- [Playback runtime](../playback-runtime/) — the evaluator that advances clips and writes the overrides
- [Node-TRS animation](../node-trs-animation/) — the `Node` track kind on this same model
- [Morph targets](../morph-targets/) — the `Weights` track kind and the GPU deform it feeds
- [Foot IK and physics-ahead](../foot-ik-and-physics-ahead/) — the external producers on the pose seam
- [.sanim format](../../geometry-and-assets/sanim-format/) — how clips persist on disk
