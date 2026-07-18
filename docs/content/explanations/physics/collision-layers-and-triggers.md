+++
title = 'Collision layers, sensors, and contact events'
weight = 4
+++

# Collision layers, sensors, and contact events

Collision filtering decides which shapes may meet, while contact events tell gameplay when an
accepted pair starts or stops touching. The engine combines five object layers, sensor bodies, and
a bounded sequence-numbered event ring so scripts and control clients can observe contacts without
entering the physics worker threads.

## Object layers and filtering

[Jolt's collision-filtering model](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/) assigns each body
one object layer, maps object layers onto broad-phase layers, and applies a pair filter before the
narrow phase. Anima uses these five object layers:

| Layer | Selection |
|---|---|
| `Static` | A collider without a rigidbody, or a rigidbody whose motion is `Static` |
| `Moving` | A non-static rigidbody with `collisionLayer = 0`; unknown values also select this layer |
| `Character` | A non-static rigidbody with `collisionLayer = 1`, and the query layer used by `CharacterVirtual` |
| `Debris` | A non-static rigidbody with `collisionLayer = 2` |
| `Sensor` | Any collider with `isSensor = true` |

`resolve_object_layer` applies that selection in a fixed order. The sensor flag wins first, a
non-static rigidbody selects one of the three moving slots, and every other collider is `Static`.
The rigidbody still controls the Jolt motion type when its collider is a sensor.

The object-layer matrix is symmetric. `yes` marks an eligible pair; broad-phase overlap and shape
tests must also succeed before a contact occurs.

| A / B | `Static` | `Moving` | `Character` | `Debris` | `Sensor` |
|---|---:|---:|---:|---:|---:|
| `Static` | no | yes | yes | yes | yes |
| `Moving` | yes | yes | yes | yes | yes |
| `Character` | yes | yes | yes | yes | yes |
| `Debris` | yes | yes | yes | no | yes |
| `Sensor` | yes | yes | yes | yes | no |

`layers_collide` holds the Rust copy of the matrix, and `layers_collide_impl` supplies the same
policy to Jolt's `ObjectLayerPairFilter`. At the coarser broad phase, only `Static` maps to
`NonMoving`; all other object layers map to `Moving`. Static objects query the moving tree, while
the other layers can query both trees.

## Sensors report without solving

`Collider.is_sensor` becomes Jolt's `BodyCreationSettings::mIsSensor`. A sensor reports accepted
contacts through the listener but does not resolve penetration, so a moving body can pass through
the volume. The pair matrix must still accept Sensor-versus-solid pairs; rejecting a pair there
would suppress both physical response and the event. Sensor-versus-Sensor pairs are filtered out.

The object layer and sensor flag have separate jobs. The layer determines whether collision work
may reach the contact stage. The sensor flag tells Jolt not to apply an impulse when it does.

## Contact callbacks become a ring

Jolt calls its [`ContactListener`](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_contact_listener.html)
from multiple worker threads during `PhysicsSystem::Update`. `ContactListenerImpl` therefore stores
raw body IDs, a representative point and normal, and the transition kind in a mutex-protected
buffer. `OnContactPersisted` is ignored, so the engine records only `Begin` and `End` transitions.

After the fixed substeps finish, `World::drain_into_ring` drains that buffer on the simulation
thread. It maps each Jolt `BodyID` to an entity UUID, marks the event as a sensor overlap when either
body is a sensor, assigns the next `seq` and the current physics `tick`, then appends it to the ring.
An `End` event has zero point and normal because Jolt's removal callback supplies only the body pair.

The ring retains 256 events. `World::drain_contacts(since)` snapshots retained events with
`seq > since` and returns three cursor fields:

| Field | Meaning |
|---|---|
| `highWaterSeq` | Highest sequence number assigned in this play session |
| `oldestSeq` | Oldest event still retained, or `0` when the ring is empty |
| `overflowed` | The supplied cursor predates an event evicted from the ring |

A cursor belongs to one play session. Creating or stopping the physics world clears the ring and
resets its sequence counter. In Edit, `drain-contacts` returns an empty result with zero cursors.

For example, a client that has processed sequence 41 asks only for newer transitions:

```console
$ sa drain-contacts --since 41
  #42     begin  sensor  310 <-> 901
  #43     end    sensor  310 <-> 901
  high=43  oldest=1  overflowed=no  (2 events)
```

Passing `--since 43` on the next call yields no events unless another transition has entered the
ring. A client that receives `overflowed=yes` processes the retained tail and advances to
`highWaterSeq`.

## Script dispatch

`RuntimeSession::step` keeps a cursor separate from every control client. After physics steps, it
drains new events and calls `ScriptHost::dispatch_contact` before `on_update`. Each transition is
offered to the scripts on entity A and then entity B, with the opposite entity passed as `other`.

Sensor `Begin` and `End` events call `on_trigger_enter(other)` and `on_trigger_exit(other)`. A solid
`Begin` calls `on_contact(other, point, normal)` with world-space vectors. Solid `End` remains
visible in the contact ring but has no script handler. Missing handlers are successful no-ops; a
handler error enters the runtime error sink and stops that tick's dispatch.

## In the code

| What | File | Symbols |
|---|---|---|
| Layer selection and Rust matrix | `engine/crates/physics/src/world.rs`, `src/types.rs` | `resolve_object_layer`, `ObjectLayer`, `layers_collide` |
| Jolt layer filters and listener | `engine/crates/physics-sys/shim/jolt_bridge.h`, `jolt_bridge.cpp` | `BroadPhaseLayerImpl`, `ObjectVsBroadPhaseImpl`, `ObjectLayerPairImpl`, `ContactListenerImpl` |
| Contact event ring | `engine/crates/physics/src/world.rs`, `src/types.rs` | `World::drain_into_ring`, `World::drain_contacts`, `ContactEvent`, `ContactDrain`, `CONTACT_RING_CAP` |
| Control protocol | `engine/crates/control/src/commands_physics.rs`, `engine/crates/protocol/src/dto.rs` | `register_physics_commands`, `DrainContactsParams`, `DrainContactsResult`, `ContactEventDto` |
| Script consumption | `engine/crates/runtime/src/session.rs`, `engine/crates/script/src/runtime.rs` | `RuntimeSession::step`, `ScriptHost::dispatch_contact`, `ContactInfo` |

## Related

- [Rigidbody and collider](../rigidbody-and-collider/) explains how components become Jolt bodies.
- [Character controller](../character-controller/) uses the `Character` layer for sweep queries.
- [Script components and the play runtime](../../scripting/script-components-and-runtime/) defines the contact handler surface.
