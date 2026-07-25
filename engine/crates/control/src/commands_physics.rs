//! The 12 physics-domain control commands: world state + body listing, impulse,
//! contact-event draining, collider/bone auto-fit, kinematic bones, character movement,
//! ray/sphere queries, and the ragdoll surface.
//!
//! This is the one domain that touches the **nullable** [`EngineContext::physics`] field
//! — the live play world, `None` in Edit. Every world-querying handler guards the null
//! and returns an inactive/empty result so the editor can poll unconditionally;
//! mutation/query-on-world handlers return the typed "no physics world" error. The
//! collider/bone-fit + kinematic/ragdoll-config handlers reach `sceneEdit` instead (they
//! configure authored components and work in Edit).

use saffron_physics::{ContactKind, MotionType, World};
use saffron_protocol::{
    ApplyImpulseParams, ApplyImpulseResult, ContactEventDto, DrainContactsParams,
    DrainContactsResult, EnableRagdollParams, FitColliderParams, FitColliderResult,
    GetRagdollParams, KinematicBonesResult, MoveCharacterParams, MoveCharacterResult,
    PhysicsBodiesResult, PhysicsBodyDto, PhysicsStateResult, RagdollResult, RaycastParams,
    RaycastResult, SetKinematicBonesParams, SetRagdollParams, ShapecastParams, Uuid,
};
use saffron_scene::{
    CharacterController, Collider, Entity, KinematicBones, Scene, Shape, SkinnedMesh,
};

use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::{entity_uuid, fit_collider, from_vec3, resolve_entity, to_vec3};

/// The wire spelling of a Jolt motion type.
fn motion_name(motion: MotionType) -> &'static str {
    match motion {
        MotionType::Static => "static",
        MotionType::Kinematic => "kinematic",
        MotionType::Dynamic => "dynamic",
    }
}

/// The wire spelling of a collider shape.
fn collider_shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Box => "box",
        Shape::Sphere => "sphere",
        Shape::Capsule => "capsule",
        Shape::ConvexHull => "convexhull",
        Shape::Mesh => "mesh",
    }
}

/// The rig's ragdoll reply: live presence/active/mean-weight from the world, plus the
/// authored `BonePhysicsComponent` bone count.
fn ragdoll_result_for(world: &World, scene: &Scene, rig: Entity) -> RagdollResult {
    let uuid = entity_uuid(scene, rig);
    let state = world.ragdoll_state(saffron_core::Uuid(uuid));
    let bones = scene
        .with_component::<saffron_scene::BonePhysicsComponent, _>(rig, |bp| {
            i32::try_from(bp.bones.len()).unwrap_or(i32::MAX)
        })
        .unwrap_or(0);
    RagdollResult {
        present: state.present,
        active: state.active,
        body_weight: state.body_weight,
        bones,
    }
}

/// The "no physics world — enter play first" error every world-mutating handler returns
/// when `ctx.physics` is `None`.
fn no_world() -> Error {
    Error::command("no physics world — enter play first")
}

/// Registers the 12 physics-domain commands onto `reg`.
pub fn register_physics_commands(reg: &mut CommandRegistry) {
    reg.register::<saffron_protocol::EmptyParams, PhysicsStateResult>(
        "physics-state",
        "physics-state — summary of the live physics world (active, body + dynamic counts)",
        |ctx, _params| {
            // No world in Edit — report inactive, never an error, so the editor polls
            // unconditionally.
            let Some(world) = ctx.physics.as_deref() else {
                return Ok(PhysicsStateResult {
                    active: false,
                    body_count: 0,
                    dynamic_count: 0,
                });
            };
            let stats = world.stats();
            Ok(PhysicsStateResult {
                active: stats.active,
                body_count: stats.body_count,
                dynamic_count: stats.dynamic_count,
            })
        },
    );

    reg.register::<saffron_protocol::EmptyParams, PhysicsBodiesResult>(
        "physics-bodies",
        "physics-bodies — every live body's entity, motion, active state, and world position",
        |ctx, _params| {
            let Some(world) = ctx.physics.as_deref() else {
                return Ok(PhysicsBodiesResult { bodies: Vec::new() });
            };
            let bodies = world
                .list_bodies()
                .into_iter()
                .map(|body| PhysicsBodyDto {
                    target: body.target.map(target_dto),
                    motion: motion_name(body.motion).to_owned(),
                    active: body.active,
                    position: to_vec3(body.position),
                })
                .collect();
            Ok(PhysicsBodiesResult { bodies })
        },
    );

    reg.register::<ApplyImpulseParams, ApplyImpulseResult>(
        "apply-impulse",
        "apply-impulse {entity, impulse:{x,y,z}} — push a Dynamic rigidbody (returns its new velocity)",
        |ctx, params| {
            if ctx.physics.is_none() {
                return Err(no_world());
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            let uuid = saffron_core::Uuid(entity_uuid(ctx.scene_edit.active_scene(), entity));
            let world = ctx.physics.as_deref_mut().ok_or_else(no_world)?;
            world.apply_impulse(uuid, from_vec3(params.impulse));
            let velocity = world.body_linear_velocity(uuid);
            Ok(ApplyImpulseResult {
                velocity: to_vec3(velocity),
            })
        },
    );

    reg.register::<FitColliderParams, FitColliderResult>(
        "fit-collider",
        "fit-collider {entity} — re-fit a Collider's shape to the entity's mesh AABB",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if !ctx
                .scene_edit
                .active_scene()
                .has_component::<Collider>(entity)
            {
                return Err(Error::command("entity has no Collider"));
            }
            if !fit_collider(ctx, entity) {
                return Err(Error::command("no resolvable mesh to fit the collider to"));
            }
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            let collider = scene
                .component::<Collider>(entity)
                .map_err(|_| Error::command("entity has no Collider"))?;
            Ok(FitColliderResult {
                entity: Uuid(entity_uuid(scene, entity)),
                shape: collider_shape_name(collider.shape).to_owned(),
                half_extents: to_vec3(collider.half_extents),
                offset: to_vec3(collider.offset),
            })
        },
    );

    reg.register::<DrainContactsParams, DrainContactsResult>(
        "drain-contacts",
        "drain-contacts {since} — contact/trigger events with seq > since (non-blocking)",
        |ctx, params| {
            let Some(world) = ctx.physics.as_deref() else {
                // Edit: no world, an empty drain (the editor polls unconditionally).
                return Ok(DrainContactsResult {
                    events: Vec::new(),
                    high_water_seq: 0,
                    oldest_seq: 0,
                    overflowed: false,
                });
            };
            let since = params.since.map_or(0, |s| s.max(0) as u64);
            let drain = world.drain_contacts(since);
            let events = drain
                .events
                .into_iter()
                .map(|event| ContactEventDto {
                    seq: event.seq as i64,
                    kind: match event.kind {
                        ContactKind::Begin => "begin",
                        ContactKind::End => "end",
                    }
                    .to_owned(),
                    target_a: event.target_a.map(target_dto),
                    target_b: event.target_b.map(target_dto),
                    sensor: event.sensor,
                    point: to_vec3(event.point),
                    normal: to_vec3(event.normal),
                    tick: event.tick,
                })
                .collect();
            Ok(DrainContactsResult {
                events,
                high_water_seq: drain.high_water_seq as i64,
                oldest_seq: drain.oldest_seq as i64,
                overflowed: drain.overflowed,
            })
        },
    );

    reg.register::<SetKinematicBonesParams, KinematicBonesResult>(
        "set-kinematic-bones",
        "set-kinematic-bones {entity, enabled?} — toggle a rig's kinematic-bone physics",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            // The editor selects a model by its container root; the rig (the SkinnedMesh entity)
            // lives on a descendant. Resolve to it so the kinematic bones bind the actual rig,
            // and reject a model with no rig rather than attaching to a bare container.
            let Some(rig) = scene.model_rig_entity(entity) else {
                return Err(Error::command("entity has no rig for kinematic bones"));
            };
            if !scene.has_component::<KinematicBones>(rig) {
                let _ = scene.add_component(rig, KinematicBones::default());
            }
            if let Some(enabled) = params.enabled {
                let _ = scene.with_component_mut::<KinematicBones, _>(rig, |k| k.enabled = enabled);
            }
            let enabled = scene
                .with_component::<KinematicBones, _>(rig, |k| k.enabled)
                .unwrap_or(false);
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            let bone_count = scene
                .with_component::<SkinnedMesh, _>(rig, |s| {
                    i32::try_from(s.bones.len()).unwrap_or(i32::MAX)
                })
                .unwrap_or(0);
            Ok(KinematicBonesResult {
                entity: Uuid(entity_uuid(scene, rig)),
                enabled,
                bone_count,
            })
        },
    );

    reg.register::<MoveCharacterParams, MoveCharacterResult>(
        "move-character",
        "move-character {entity, velocity:{x,y,z}, jump?} — set a character's desired walk velocity",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            if !scene.has_component::<CharacterController>(entity) {
                return Err(Error::command("entity has no CharacterController"));
            }
            // The sweep consumes these on the next physics step (the inert seam); y is gravity's.
            let _ = scene.with_component_mut::<CharacterController, _>(entity, |c| {
                c.desired_velocity = saffron_geometry::glam::Vec3::new(
                    params.velocity.x,
                    0.0,
                    params.velocity.z,
                );
                if params.jump.unwrap_or(false) {
                    c.vertical_velocity = 5.0; // a fixed jump impulse
                }
            });
            let position = scene.world_translation(entity);
            let on_ground = scene
                .with_component::<CharacterController, _>(entity, |c| c.on_ground)
                .unwrap_or(false);
            Ok(MoveCharacterResult {
                position: to_vec3(position),
                on_ground,
            })
        },
    );

    reg.register::<RaycastParams, RaycastResult>(
        "raycast",
        "raycast {origin:{x,y,z}, dir:{x,y,z}, maxDist=1000} — closest physics hit (entity/point/normal/distance)",
        |ctx, params| {
            let world = ctx.physics.as_deref().ok_or_else(no_world)?;
            let hit = world.raycast(
                from_vec3(params.origin),
                from_vec3(params.dir),
                params.max_dist.unwrap_or(1000.0),
            );
            Ok(ray_hit_result(hit))
        },
    );

    reg.register::<ShapecastParams, RaycastResult>(
        "shapecast",
        "shapecast {origin:{x,y,z}, dir:{x,y,z}, radius, maxDist=1000} — closest sphere-sweep hit",
        |ctx, params| {
            let world = ctx.physics.as_deref().ok_or_else(no_world)?;
            let hit = world.sphere_cast(
                from_vec3(params.origin),
                from_vec3(params.dir),
                params.radius,
                params.max_dist.unwrap_or(1000.0),
            );
            Ok(ray_hit_result(hit))
        },
    );

    reg.register::<EnableRagdollParams, RagdollResult>(
        "enable-ragdoll",
        "enable-ragdoll {entity, enabled?} — go limp (true) or restore animation (false) on a rig",
        |ctx, params| {
            if ctx.physics.is_none() {
                return Err(no_world());
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            // Selecting the model root resolves to its rig descendant (SkinnedMesh + BonePhysics).
            let rig = ctx.scene_edit.active_scene().animatable_descendant(entity);
            let uuid = saffron_core::Uuid(entity_uuid(ctx.scene_edit.active_scene(), rig));
            if params.enabled.unwrap_or(true) {
                // The borrow has to split scene (read) from the world (mut); take a snapshot scene
                // reference for enable_ragdoll, then re-borrow for the reply.
                let world = ctx.physics.as_deref_mut().ok_or_else(no_world)?;
                world
                    .enable_ragdoll(ctx.scene_edit.active_scene(), rig)
                    .map_err(|e| Error::command(e.to_string()))?;
            } else {
                ctx.physics
                    .as_deref_mut()
                    .ok_or_else(no_world)?
                    .disable_ragdoll(uuid);
            }
            let world = ctx.physics.as_deref().ok_or_else(no_world)?;
            Ok(ragdoll_result_for(
                world,
                ctx.scene_edit.active_scene(),
                rig,
            ))
        },
    );

    reg.register::<SetRagdollParams, RagdollResult>(
        "set-ragdoll",
        "set-ragdoll {entity, active?, bodyWeight?, bone?, weight?} — drive a rig's active-ragdoll blend",
        |ctx, params| {
            if ctx.physics.is_none() {
                return Err(no_world());
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            let rig = ctx.scene_edit.active_scene().animatable_descendant(entity);
            let uuid = saffron_core::Uuid(entity_uuid(ctx.scene_edit.active_scene(), rig));
            // Auto-create the ragdoll on first drive so a hit reaction "just works".
            if !ctx.physics.as_deref().ok_or_else(no_world)?.has_ragdoll(uuid) {
                let world = ctx.physics.as_deref_mut().ok_or_else(no_world)?;
                world
                    .enable_ragdoll(ctx.scene_edit.active_scene(), rig)
                    .map_err(|e| Error::command(e.to_string()))?;
            }
            ctx.physics
                .as_deref_mut()
                .ok_or_else(no_world)?
                .set_ragdoll_blend(
                    uuid,
                    params.active,
                    params.body_weight,
                    params.bone,
                    params.weight,
                )
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.animation_version += 1;
            let world = ctx.physics.as_deref().ok_or_else(no_world)?;
            Ok(ragdoll_result_for(world, ctx.scene_edit.active_scene(), rig))
        },
    );

    reg.register::<GetRagdollParams, RagdollResult>(
        "get-ragdoll",
        "get-ragdoll {entity} — the rig's ragdoll presence, active flag, and blend weight",
        |ctx, params| {
            if ctx.physics.is_none() {
                return Err(no_world());
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            let rig = ctx.scene_edit.active_scene().animatable_descendant(entity);
            let world = ctx.physics.as_deref().ok_or_else(no_world)?;
            Ok(ragdoll_result_for(
                world,
                ctx.scene_edit.active_scene(),
                rig,
            ))
        },
    );
}

/// Maps a physics ray/sphere hit to the wire `RaycastResult` (shared by both queries).
fn ray_hit_result(hit: saffron_physics::RayHit) -> RaycastResult {
    RaycastResult {
        hit: hit.hit,
        target: hit.target.map(target_dto),
        point: to_vec3(hit.point),
        normal: to_vec3(hit.normal),
        distance: hit.distance,
    }
}

/// Maps a physics tagged target onto the wire DTO.
fn target_dto(target: saffron_physics::WorldHitTarget) -> saffron_protocol::WorldHitTargetDto {
    match target {
        saffron_physics::WorldHitTarget::SceneEntity(uuid) => {
            saffron_protocol::WorldHitTargetDto::SceneEntity { id: Uuid(uuid.0) }
        }
        saffron_physics::WorldHitTarget::Vegetation(plant) => {
            saffron_protocol::WorldHitTargetDto::Vegetation {
                plant: saffron_protocol::PlantId(plant.canonical_hex()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::registry::{CommandRegistry, register_builtin_commands};
    use crate::test_support::{StubRenderer, with_stub};

    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    /// With no live world (Edit), `physics-state` reports inactive and `physics-bodies`
    /// returns an empty list — never an error, so the editor can poll unconditionally.
    #[test]
    fn null_world_query_commands_degrade_gracefully() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let state = reg.dispatch(ctx, &json!({ "cmd": "physics-state" }));
            assert_eq!(state["ok"], json!(true));
            assert_eq!(state["result"]["active"], json!(false));
            assert_eq!(state["result"]["bodyCount"], json!(0));
            assert_eq!(state["result"]["dynamicCount"], json!(0));

            let bodies = reg.dispatch(ctx, &json!({ "cmd": "physics-bodies" }));
            assert_eq!(bodies["ok"], json!(true));
            assert_eq!(bodies["result"]["bodies"], json!([]));

            let drain = reg.dispatch(
                ctx,
                &json!({ "cmd": "drain-contacts", "params": { "since": 0 } }),
            );
            assert_eq!(drain["ok"], json!(true));
            assert_eq!(drain["result"]["events"], json!([]));
            assert_eq!(drain["result"]["overflowed"], json!(false));
        });
    }

    /// With no live world, the mutation/query-on-world commands return the typed
    /// "no physics world" error rather than degrading.
    #[test]
    fn null_world_mutation_commands_error() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let raycast = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "raycast",
                    "params": { "origin": { "x": 0, "y": 0, "z": 0 }, "dir": { "x": 0, "y": -1, "z": 0 } }
                }),
            );
            assert_eq!(raycast["ok"], json!(false));
            assert_eq!(
                raycast["error"]["message"],
                json!("no physics world — enter play first")
            );

            let impulse = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "apply-impulse",
                    "params": { "entity": "1", "impulse": { "x": 0, "y": 1, "z": 0 } }
                }),
            );
            assert_eq!(impulse["ok"], json!(false));
            assert_eq!(
                impulse["error"]["message"],
                json!("no physics world — enter play first")
            );
        });
    }

    /// `fit-collider` errors when the entity carries no Collider, and `raycast` defaults
    /// a missing `maxDist` without a deserialize error.
    #[test]
    fn fit_collider_without_collider_errors() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let entity = ctx.scene_edit.active_scene().create_entity("bare");
            let uuid = crate::selector::entity_uuid(ctx.scene_edit.active_scene(), entity);
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "fit-collider", "params": { "entity": uuid.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(false));
            assert_eq!(reply["error"]["message"], json!("entity has no Collider"));
        });
    }
}
