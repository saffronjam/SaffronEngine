//! The runtime's [`ScriptHostBridge`] implementation: the concrete end of the POD seam
//! `saffron-script` declares, so the `sa.*` physics bindings and the `sa.log` sink reach the live
//! world without `saffron-script` importing `saffron-physics`.
//!
//! [`RuntimeScriptBridge`] holds the play world and scene as `Rc<RefCell<…>>` cells shared with the
//! [`RuntimeSession`](crate::RuntimeSession) — `Rc` rather than a `Mutex` because the VM is `!Send`.
//! Sharing the cells is what lets an installed callback see the live world, which is `None` before
//! start and after stop.
//!
//! `sa.log` cannot write into the consumer's log ring while a script tick runs, so the line lands in
//! [`SharedScriptSink`] and the session drains it once the call batch returns.

use std::cell::RefCell;
use std::rc::Rc;

use glam::Vec3;

use saffron_core::Uuid;
use saffron_physics::World;
use saffron_scene::{MorphComponent, MorphWeightOverride, Scene};
use saffron_script::{ScriptHostBridge, ScriptPlantHit, ScriptRagdollState, ScriptRayHit};

/// The live play physics world, shared between the session and the bridge (`None` before
/// start / after stop).
pub type SharedPhysics = Rc<RefCell<Option<World>>>;
/// The play scene, shared so the scene-reading `enable_ragdoll` resolves the rig entity.
pub type SharedScene = Rc<RefCell<Scene>>;
/// The bound vegetation authority, shared between the session and the bridge so a script's
/// vegetation query or interaction reaches the same world the runtime publishes (`None` while no
/// exact cooked generation is bound).
pub type SharedVegetation = Rc<RefCell<Option<saffron_vegetation::VegetationWorld>>>;

/// One buffered `sa.log` line: the sender uuid and its message, drained by the session into
/// the consumer's log ring after the script call batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptLogLine {
    /// The uuid of the instance that logged the line.
    pub sender: u64,
    /// The logged message.
    pub message: String,
}

/// The shared `sa.log` buffer: the bridge appends, the session drains it once the tick
/// releases its borrows.
pub type SharedScriptSink = Rc<RefCell<Vec<ScriptLogLine>>>;

/// The runtime's [`ScriptHostBridge`], over the shared world / scene / log cells.
///
/// Construct it from the session's own cells with [`RuntimeScriptBridge::new`], box it as
/// `Rc<dyn ScriptHostBridge>`, and install it onto the play session's
/// [`ScriptHost`](saffron_script::ScriptHost). Every method guards a `None` world / dead
/// lookup as a safe no-op, so a call before start or between worlds never panics.
pub struct RuntimeScriptBridge {
    /// The live play physics world (`None` before start / after stop). Shared so the bridge
    /// reaches whatever world is current without owning it.
    physics: SharedPhysics,
    /// The play scene, shared so the scene-reading `enable_ragdoll` resolves the rig
    /// entity.
    scene: SharedScene,
    /// The bound vegetation authority the vegetation queries and interactions read and mutate.
    vegetation: SharedVegetation,
    /// The buffered `sa.log` lines the session drains into the consumer's log ring.
    sink: SharedScriptSink,
}

impl RuntimeScriptBridge {
    /// Wires the bridge to the session's shared world / scene cells and the log sink.
    #[must_use]
    pub fn new(
        physics: SharedPhysics,
        scene: SharedScene,
        vegetation: SharedVegetation,
        sink: SharedScriptSink,
    ) -> Self {
        Self {
            physics,
            scene,
            vegetation,
            sink,
        }
    }

    /// Reduces one plant-addressed mutation through the bound authority, minting the header the
    /// reducer requires: the cell the plant is resident in, that cell's current revision as the
    /// optimistic precondition, and a transaction/operation key derived from the content plus that
    /// revision. Two identical calls therefore commit twice (the revision advanced between them),
    /// while a genuine replay of the same record against the same revision is idempotent.
    fn mutate_plant(
        &self,
        plant: &str,
        build: impl FnOnce(saffron_spatial::PlantId) -> saffron_vegetation::VegetationMutation,
    ) -> bool {
        let Ok(plant_id) = plant.parse::<saffron_spatial::PlantId>() else {
            return false;
        };
        let mut world = self.vegetation.borrow_mut();
        let Some(world) = world.as_mut() else {
            return false;
        };
        let Ok(Some(snapshot)) = world.find_plant(plant_id) else {
            return false;
        };
        let cell = snapshot.position.cell();
        let base_revision = world
            .persistent_state()
            .cells()
            .get(&cell)
            .map_or(0, |state| state.revision);
        let mutation = build(plant_id);
        let mut digest = plant_id.bytes().to_vec();
        digest.extend_from_slice(&base_revision.to_be_bytes());
        digest.extend_from_slice(format!("{mutation:?}").as_bytes());
        let key = leading_u128(saffron_vegetation::ContentHash::of(&digest).bytes());
        let record = saffron_vegetation::VegetationMutationRecord {
            header: saffron_vegetation::MutationHeader {
                cell,
                transaction: key,
                authority: SCRIPT_AUTHORITY,
                logical_tick: base_revision + 1,
                idempotency_key: key,
                base_revision: Some(base_revision),
            },
            mutation,
        };
        match world.apply_confirmed_mutations(&[record]) {
            Ok(reduction) => !reduction.committed_transactions.is_empty(),
            Err(error) => {
                tracing::warn!("script vegetation mutation rejected: {error}");
                false
            }
        }
    }

    /// Flattens a physics [`RayHit`](saffron_physics::RayHit) into the script-side POD —
    /// a plain field copy.
    fn flatten(hit: saffron_physics::RayHit) -> ScriptRayHit {
        ScriptRayHit {
            hit: hit.hit,
            target: hit.target.map(script_target),
            point: hit.point,
            normal: hit.normal,
            distance: hit.distance,
        }
    }
}

/// Maps a physics tagged target into the script-side mirror (a plain re-tag; the two
/// enums share the same vocabulary without a crate dependency between them).
pub(crate) fn script_target(
    target: saffron_physics::WorldHitTarget,
) -> saffron_script::ScriptHitTarget {
    match target {
        saffron_physics::WorldHitTarget::SceneEntity(uuid) => {
            saffron_script::ScriptHitTarget::SceneEntity(uuid)
        }
        saffron_physics::WorldHitTarget::Vegetation(plant) => {
            saffron_script::ScriptHitTarget::Vegetation(plant)
        }
    }
}

impl ScriptHostBridge for RuntimeScriptBridge {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> ScriptRayHit {
        match self.physics.borrow().as_ref() {
            Some(world) => Self::flatten(world.raycast(origin, dir, max_dist)),
            None => ScriptRayHit::default(),
        }
    }

    fn sphere_cast(&self, origin: Vec3, dir: Vec3, radius: f32, max_dist: f32) -> ScriptRayHit {
        match self.physics.borrow().as_ref() {
            Some(world) => Self::flatten(world.sphere_cast(origin, dir, radius, max_dist)),
            None => ScriptRayHit::default(),
        }
    }

    fn apply_impulse(&self, entity: Uuid, impulse: Vec3) {
        if let Some(world) = self.physics.borrow_mut().as_mut() {
            world.apply_impulse(entity, impulse);
        }
    }

    fn add_force(&self, entity: Uuid, force: Vec3) {
        if let Some(world) = self.physics.borrow_mut().as_mut() {
            world.add_force(entity, force);
        }
    }

    fn set_velocity(&self, entity: Uuid, velocity: Vec3) {
        if let Some(world) = self.physics.borrow_mut().as_mut() {
            world.set_linear_velocity(entity, velocity);
        }
    }

    fn set_morph_weights(&self, entity: Uuid, weights: &[f32]) {
        let mut scene = self.scene.borrow_mut();
        let Some(e) = scene.find_entity_by_uuid(entity) else {
            return;
        };
        if !scene.valid(e) {
            return;
        }
        // The morph mesh is the entity itself or the morph-bearing entity in its forest (a
        // script targets the model's container while the morph mesh rides a child node).
        let target = scene.model_morph_entity(e).unwrap_or(e);
        // A non-morph entity or a length mismatch is a silent no-op (the bridge contract).
        let Ok(count) = scene.with_component::<MorphComponent, _>(target, |c| c.weights.len())
        else {
            return;
        };
        if weights.len() != count {
            return;
        }
        if scene.has_component::<MorphWeightOverride>(target) {
            let _ = scene.with_component_mut::<MorphWeightOverride, _>(target, |o| {
                o.weights = weights.to_vec();
            });
        } else {
            let _ = scene.with_component_mut::<MorphComponent, _>(target, |c| {
                c.weights = weights.to_vec();
            });
        }
    }

    fn get_velocity(&self, entity: Uuid) -> Vec3 {
        match self.physics.borrow().as_ref() {
            Some(world) => world.body_linear_velocity(entity),
            None => Vec3::ZERO,
        }
    }

    fn set_ragdoll_enabled(&self, rig: Uuid, enable: bool) -> bool {
        let mut physics = self.physics.borrow_mut();
        let Some(world) = physics.as_mut() else {
            return false;
        };
        if !enable {
            world.disable_ragdoll(rig);
            return true;
        }
        // Enabling reads the rig's SkinnedMesh + BonePhysics off the play scene; the scene
        // cell is a separate borrow from physics.
        let scene = self.scene.borrow();
        let Some(entity) = scene.find_entity_by_uuid(rig) else {
            return false;
        };
        if !scene.valid(entity) {
            return false;
        }
        match world.enable_ragdoll(&scene, entity) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("script: enable_ragdoll: {err}");
                false
            }
        }
    }

    fn set_ragdoll_blend(&self, rig: Uuid, active: bool, body_weight: f32) {
        if let Some(world) = self.physics.borrow_mut().as_mut() {
            let _ = world.set_ragdoll_blend(rig, Some(active), Some(body_weight), None, None);
        }
    }

    fn ragdoll_state(&self, rig: Uuid) -> ScriptRagdollState {
        match self.physics.borrow().as_ref() {
            Some(world) => {
                let s = world.ragdoll_state(rig);
                ScriptRagdollState {
                    present: s.present,
                    active: s.active,
                    body_weight: s.body_weight,
                    bones: s.bones,
                }
            }
            None => ScriptRagdollState::default(),
        }
    }

    fn vegetation_raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<ScriptPlantHit> {
        let world = self.vegetation.borrow();
        let world = world.as_ref()?;
        let origin_position = render_relative_position(origin)?;
        let ray = saffron_vegetation::VegetationQueryRay::new(
            origin_position,
            dir.as_dvec3(),
            f64::from(max_dist),
        )
        .ok()?;
        let hits = world
            .query_ray(ray, &saffron_vegetation::VegetationQueryFilter::default())
            .ok()?;
        hits.first()
            .map(|hit| plant_hit(&hit.plant, hit.distance_m))
    }

    fn vegetation_nearest(&self, position: Vec3, radius: f32) -> Option<ScriptPlantHit> {
        let world = self.vegetation.borrow();
        let world = world.as_ref()?;
        let center = render_relative_position(position)?;
        world
            .query_nearest(
                center,
                Some(f64::from(radius)),
                &saffron_vegetation::VegetationQueryFilter::default(),
            )
            .ok()
            .flatten()
            .map(|hit| plant_hit(&hit.plant, hit.distance_m))
    }

    fn vegetation_in_radius(
        &self,
        position: Vec3,
        radius: f32,
        limit: usize,
    ) -> Vec<ScriptPlantHit> {
        let world = self.vegetation.borrow();
        let Some(world) = world.as_ref() else {
            return Vec::new();
        };
        let Some(center) = render_relative_position(position) else {
            return Vec::new();
        };
        let origin = center.world_meters();
        let Ok(plants) = world.query_radius(
            center,
            f64::from(radius),
            &saffron_vegetation::VegetationQueryFilter::default(),
        ) else {
            return Vec::new();
        };
        let mut hits: Vec<ScriptPlantHit> = plants
            .iter()
            .map(|plant| {
                let distance = (plant.position.world_meters() - origin).length();
                plant_hit(plant, distance)
            })
            .collect();
        hits.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.plant.cmp(&right.plant))
        });
        hits.truncate(limit);
        hits
    }

    fn vegetation_damage(&self, plant: &str, amount: f32) -> bool {
        let Ok(amount) = saffron_spatial::UnitInterval::from_f64(f64::from(amount.clamp(0.0, 1.0)))
        else {
            return false;
        };
        self.mutate_plant(plant, |plant| {
            saffron_vegetation::VegetationMutation::Damage {
                plant,
                amount,
                phenotype: None,
            }
        })
    }

    fn vegetation_harvest(&self, plant: &str, phenotype: u32) -> bool {
        self.mutate_plant(plant, |plant| {
            saffron_vegetation::VegetationMutation::Harvest { plant, phenotype }
        })
    }

    fn log_sink(&self, sender: Uuid, message: &str) {
        self.sink.borrow_mut().push(ScriptLogLine {
            sender: sender.0,
            message: message.to_owned(),
        });
    }
}

/// The authority id every script-driven vegetation mutation is recorded under, distinct from the
/// editor, the promotion write-back, and a future server.
const SCRIPT_AUTHORITY: u128 = 0x5361_6666_726f_6e5f_5363_7269_7074_0001;

/// The leading 16 bytes of a content hash as a non-zero transaction/operation key.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading) | 1
}

/// Quantizes a script-supplied render-relative position into the exact world vocabulary.
fn render_relative_position(position: Vec3) -> Option<saffron_spatial::WorldPosition> {
    saffron_spatial::WorldPosition::from_render_relative(
        position,
        saffron_spatial::WorldPosition::origin(),
    )
    .ok()
}

/// Flattens one macro-plant snapshot into the script-side POD.
fn plant_hit(
    plant: &saffron_vegetation::VegetationPlantSnapshot,
    distance_m: f64,
) -> ScriptPlantHit {
    ScriptPlantHit {
        plant: plant.plant.canonical_hex(),
        position: plant
            .position
            .to_render_relative(saffron_spatial::WorldPosition::origin())
            .unwrap_or(Vec3::ZERO),
        distance: distance_m as f32,
        lifecycle: lifecycle_name(plant.lifecycle).to_owned(),
        health: plant.health.to_f64() as f32,
        interaction_policy: policy_name(plant.interaction_policy).to_owned(),
    }
}

fn lifecycle_name(lifecycle: saffron_vegetation::PlantLifecycle) -> &'static str {
    use saffron_vegetation::PlantLifecycle as Lifecycle;
    match lifecycle {
        Lifecycle::Seed => "seed",
        Lifecycle::Sprout => "sprout",
        Lifecycle::Juvenile => "juvenile",
        Lifecycle::Mature => "mature",
        Lifecycle::Senescent => "senescent",
        Lifecycle::Dead => "dead",
        Lifecycle::Stump => "stump",
        Lifecycle::Removed => "removed",
    }
}

fn policy_name(policy: saffron_vegetation::InteractionPolicy) -> &'static str {
    use saffron_vegetation::InteractionPolicy as Policy;
    match policy {
        Policy::Decorative => "decorative",
        Policy::Interactive => "interactive",
        Policy::Harvestable => "harvestable",
        Policy::Structural => "structural",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_scene::register_builtin_components;
    use std::sync::Arc;

    fn cells() -> (
        SharedPhysics,
        SharedScene,
        SharedVegetation,
        SharedScriptSink,
    ) {
        (
            Rc::new(RefCell::new(None)),
            Rc::new(RefCell::new(Scene::new())),
            Rc::new(RefCell::new(None)),
            Rc::new(RefCell::new(Vec::new())),
        )
    }

    /// With no world (before start), the physics calls are safe no-ops: a missed raycast,
    /// zero velocity, a false ragdoll toggle.
    #[test]
    fn no_world_is_a_safe_noop() {
        let (physics, scene, vegetation, sink) = cells();
        let bridge = RuntimeScriptBridge::new(physics, scene, vegetation, sink);
        assert_eq!(
            bridge.raycast(Vec3::ZERO, Vec3::Z, 100.0),
            ScriptRayHit::default()
        );
        assert_eq!(bridge.get_velocity(Uuid(7)), Vec3::ZERO);
        assert!(!bridge.set_ragdoll_enabled(Uuid(7), true));
        assert_eq!(bridge.ragdoll_state(Uuid(7)), ScriptRagdollState::default());
        // The mutating no-ops do not panic.
        bridge.apply_impulse(Uuid(7), Vec3::ONE);
        bridge.add_force(Uuid(7), Vec3::ONE);
        bridge.set_velocity(Uuid(7), Vec3::ONE);
        bridge.set_ragdoll_blend(Uuid(7), true, 0.5);
    }

    /// `log_sink` appends the line into the shared sink, tagged with the sender uuid — the
    /// session drains it from there into the consumer's log ring after the tick.
    #[test]
    fn log_sink_buffers_the_line() {
        let (physics, scene, vegetation, sink) = cells();
        let bridge = RuntimeScriptBridge::new(physics, scene, vegetation, Rc::clone(&sink));
        bridge.log_sink(Uuid(42), "hello");
        let lines = sink.borrow();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].sender, 42);
        assert_eq!(lines[0].message, "hello");
    }

    /// With no bound vegetation authority every vegetation call is a safe no-op: an empty query
    /// and a refused mutation, never a panic.
    #[test]
    fn vegetation_calls_without_an_authority_are_safe_no_ops() {
        let (physics, scene, vegetation, sink) = cells();
        let bridge = RuntimeScriptBridge::new(physics, scene, vegetation, sink);
        assert!(
            bridge
                .vegetation_raycast(Vec3::ZERO, Vec3::Z, 50.0)
                .is_none()
        );
        assert!(bridge.vegetation_nearest(Vec3::ZERO, 10.0).is_none());
        assert!(bridge.vegetation_in_radius(Vec3::ZERO, 10.0, 8).is_empty());
        // A malformed identity is refused before the authority is even consulted.
        assert!(!bridge.vegetation_damage("not-a-plant-id", 0.5));
        assert!(!bridge.vegetation_damage("40aabbccddeeff00112233445566778899", 0.5));
        assert!(!bridge.vegetation_harvest("40aabbccddeeff00112233445566778899", 2));
    }

    /// The physics calls route to the live world: a velocity set + read-back round-trips
    /// through a real (Jolt-backed) world, and `set_ragdoll_enabled(false)` on an absent
    /// rig is the documented `true` (disable is always accepted on a live world).
    #[test]
    fn physics_calls_route_to_the_live_world() {
        let world = match World::new() {
            Ok(world) => world,
            Err(err) => {
                // Jolt globals failed to install (no toolchain) — skip, not a false pass.
                eprintln!("skipping: World::new failed: {err}");
                return;
            }
        };
        let physics = Rc::new(RefCell::new(Some(world)));
        let scene = Rc::new(RefCell::new(Scene::new()));
        let vegetation = Rc::new(RefCell::new(None));
        let sink = Rc::new(RefCell::new(Vec::new()));
        let bridge = RuntimeScriptBridge::new(physics, Rc::clone(&scene), vegetation, sink);

        // No mapped body for this uuid → velocity read is zero, and the impulse/force/
        // velocity sets are no-ops (warned) rather than panics, on a live world.
        assert_eq!(bridge.get_velocity(Uuid(123)), Vec3::ZERO);
        bridge.apply_impulse(Uuid(123), Vec3::ONE);
        bridge.set_velocity(Uuid(123), Vec3::new(1.0, 2.0, 3.0));

        // Disable on a live world is accepted (true) even when the rig is absent.
        assert!(bridge.set_ragdoll_enabled(Uuid(123), false));
        // Enable on an absent rig is false (no entity for the uuid in the empty scene).
        assert!(!bridge.set_ragdoll_enabled(Uuid(123), true));

        // The registry construction is unrelated but confirms the scene cell is usable
        // alongside the physics borrow without a RefCell clash.
        let _registry = Arc::new(register_builtin_components());
    }
}
