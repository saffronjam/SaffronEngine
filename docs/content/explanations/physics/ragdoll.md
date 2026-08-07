+++
title = 'Ragdoll'
weight = 8
+++

# Ragdoll

A ragdoll gives each skeleton bone a simulated body and constrains neighboring bodies at their
joints. Physics then writes those body transforms back as bone-local poses, so gravity and contacts
can drive the rendered skeleton.

## Per-bone description

`BonePhysicsComponent.bones` runs parallel to `SkinnedMesh.bones`. Each `BonePhysics` entry defines
the capsule size, mass, parent-joint type, swing and twist limits, and motor settings for the bone at
the same index.

Import creates this array from the rest skeleton. For each bone, it uses the greatest distance to a
direct child as the capsule length. Half-height is half that distance, or `0.05` for a leaf; radius
is 30% of half-height with a `0.03` minimum. Imported entries have mass `1.0` and a `SwingTwist`
joint.

The generic component field commands can edit individual array entries. A valid ragdoll requires a
nonempty `SkinnedMesh`, a `BonePhysicsComponent`, and equal bone counts. `World::enable_ragdoll`
returns typed errors when those requirements fail or Jolt cannot create the ragdoll.

## Jolt construction

Anima builds a [`Jolt Ragdoll`](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_ragdoll.html)
from one `BonePart` per skeleton bone. It derives the parent index from the scene hierarchy and
seeds every part with the bone's composed world position and rotation. Each part is a dynamic
capsule on the `Moving` layer.

The bridge clamps capsule radius and half-height to `0.03` and mass to `0.01`. A non-root part gets
one constraint at the child bone's world-space joint origin:

| `Joint` | Jolt constraint | Limits |
|---|---|---|
| `Fixed` | `FixedConstraint` | Rigid relative transform |
| `Hinge` | `HingeConstraint` | Symmetric angle from `swingTwistLimits.x` |
| `SwingTwist` | `SwingTwistConstraint` | Two cone half-angles and a symmetric twist angle |
| `Free` | `PointConstraint` | Shared point with free relative rotation |

Each limit value at or below `0.001` uses `0.7` radians. The bridge calls
`RagdollSettings::Stabilize` and `CalculateBodyIndexToConstraintIndex` before it creates and
activates the ragdoll.

`enable_ragdoll` initializes every pose weight to `1` and leaves the constraint motors off. The
result is a passive ragdoll: joint limits and contacts constrain the bodies, while gravity supplies
their motion.

## Bone pose write-back

After physics steps, `World::write_ragdoll_poses` reads every part's world transform. It converts a
child part into bone-local space with `inverse(parent_part_world) * part_world`. The root instead
uses the rig entity's composed world transform as its parent frame.

The resulting translation, rotation, and scale enter the bone's `PoseOverride`. A passive ragdoll
starts with full physics weight, so these values replace the animation pose for the tick. Scene
hierarchy composition and skinning consume the override through the same local-pose path as skeletal
animation.

```text
Jolt part world transforms
  -> parent-relative bone transforms
  -> PoseOverride per bone
  -> scene world transforms
  -> skinning joint matrices
```

## Lifetime and control

Ragdolls belong to the physics world created for the play scene. Stopping play drops that world and
the duplicated play scene together, leaving authored bone transforms unchanged.

`enable-ragdoll` resolves a selected model root to its animatable rig descendant. Enabling rebuilds
the rig's ragdoll from its current pose; disabling removes it from Jolt. On the next animation tick,
an active clip writes its bone overrides again, while a rig without a clip has its overrides
cleared.

For example, this enables a passive ragdoll during play:

```console
$ sa enable-ragdoll --entity 42
ragdoll=present  active=no  bodyWeight=1.00  bones=64
```

The equivalent Lua entity methods are `enable_ragdoll()` and `disable_ragdoll()`. Motor drive and
partial pose weights use the active-ragdoll controls.

## In the code

| What | File | Symbols |
|---|---|---|
| Ragdoll creation, pose write-back, and removal | `engine/crates/physics/src/world/` | `World::enable_ragdoll`, `World::write_ragdoll_poses`, `World::disable_ragdoll`, `joint_raw` |
| Jolt settings and constraints | `engine/crates/physics-sys/src/lib.rs`, `shim/jolt_bridge.cpp` | `add_ragdoll`, `BonePart`, `build_joint_constraint`, `ragdoll_part_transform` |
| Bone physics schema | `engine/crates/scene/src/component.rs` | `BonePhysics`, `BonePhysicsComponent`, `Joint` |
| Import fitting | `engine/crates/assets/src/spawn.rs` | `autofit_bone_physics` |
| Runtime tick order | `engine/crates/runtime/src/session.rs` | `RuntimeSession::step` |
| Control protocol | `engine/crates/control/src/commands_physics.rs`, `engine/crates/protocol/src/dto/` | `register_physics_commands`, `EnableRagdollParams`, `RagdollResult` |

## Related

- [Active ragdoll](../active-ragdoll/) explains motor targets and per-bone animation blending.
- [Animation data model](../../animation/animation-data-model/) explains `PoseOverride` and joint matrices.
- [Kinematic bones](../kinematic-bones/) covers the opposite direction, where animation drives physics bodies.
