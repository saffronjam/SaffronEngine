//! Macro-plant promotion: the transient entity view a sparse subset of plants gains.
//!
//! A promoted plant keeps its authoritative row in the macro SoA and additionally gains a scene
//! entity that renders, collides, and simulates for it — so falling, harvesting, and scripting
//! work on real hecs/Jolt objects without turning every plant into an entity. Exactly one owner
//! exists at every point the simulation can observe: promotion suppresses the plant's bulk
//! render instance and bulk collision batch in the same synchronization pass that creates the
//! entity, and demotion restores them in the pass that destroys it.
//!
//! Transitions are requested at any time and committed only at the fixed synchronization point
//! ([`VegetationPromotion::advance`]), so a request never lands mid-frame between the render and
//! collision views of the world.

use std::collections::BTreeMap;

use glam::{DVec3, Quat, Vec3};
use saffron_assets::AssetServer;
use saffron_core::Uuid;
use saffron_physics::World;
use saffron_scene::{
    Collider, Entity, IdComponent, MaterialSet, MaterialSlot, Mesh as MeshComponent, Motion,
    PlantOrigin, PlantVariant, PlantVitals, Rigidbody, Scene, Shape, Transform, quat_to_euler_zyx,
};
use saffron_spatial::{DecisionScalar, PlantId, QuantizedOrientation, UnitInterval, WorldPosition};
use saffron_vegetation::{
    ContentHash, MutationHeader, PlantCollisionProxy, PlantCollisionShape, PlantLifecycle,
    PromotionOriginState, VegetationMutation, VegetationMutationRecord, VegetationPlantSnapshot,
    VegetationWorld,
};

use crate::vegetation_family::PlantFamilyCache;

/// The authority id every promotion write-back is recorded under: the play-session simulation,
/// distinct from an editor or server authority.
const PROMOTION_AUTHORITY: u128 = 0x5361_6666_726f_6e5f_5072_6f6d_6f74_6501;

/// Where a plant sits in the promotion lifecycle.
///
/// `Bulk` is the resting state and is never stored; a plant with no tracked state is bulk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlantPromotionState {
    /// The macro SoA row is the only representation.
    #[default]
    Bulk,
    /// Promotion is requested and commits at the next synchronization point.
    Promoting,
    /// A live entity view owns render, collision, and simulation for the plant.
    Promoted {
        /// The transient entity's stable uuid.
        entity: Uuid,
    },
    /// Demotion is requested; the next synchronization point writes state back and destroys the
    /// entity.
    Demoting {
        /// The entity still owning the plant until the transition commits.
        entity: Uuid,
    },
}

/// Aggregate promotion counters for control and CLI inspection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationPromotionReport {
    /// Plants whose entity view is live.
    pub promoted: usize,
    /// Promotion requests waiting for the next synchronization point.
    pub promoting: usize,
    /// Demotion requests waiting for the next synchronization point.
    pub demoting: usize,
    /// Entity views created since the play world came up.
    pub promoted_total: u64,
    /// Entity views destroyed since the play world came up.
    pub demoted_total: u64,
    /// Plants felled: the rooted plant became a stump and a separate product entity spawned.
    pub felled_total: u64,
    /// Promotion requests rejected because the plant, its family, or its proxy set could not
    /// produce an entity view.
    pub failed_total: u64,
    /// Plants whose state has been written back through the reducer.
    pub flushed_total: u64,
}

/// A promotion request that could not be committed.
#[derive(Debug, thiserror::Error)]
pub enum VegetationPromotionError {
    /// The plant is not in a state that accepts the request.
    #[error("plant {plant} cannot be {requested} from its current promotion state")]
    State {
        /// Canonical plant identity.
        plant: String,
        /// The rejected request.
        requested: &'static str,
    },
    /// The vegetation authority rejected the suppression or the write-back.
    #[error(transparent)]
    Vegetation(#[from] saffron_vegetation::Error),
    /// A quantized world value could not be represented.
    #[error(transparent)]
    Spatial(#[from] saffron_spatial::Error),
}

/// Play-session promotion authority: the state machine plus the transient entities it owns.
#[derive(Default)]
pub struct VegetationPromotion {
    states: BTreeMap<PlantId, PlantPromotionState>,
    /// Plants queued for felling, in request order (a felling is an operation, not a state).
    felling: Vec<PlantId>,
    report: VegetationPromotionReport,
    flush_sequence: u64,
}

impl VegetationPromotion {
    /// Requests promotion, committed at the next synchronization point.
    ///
    /// # Errors
    ///
    /// [`VegetationPromotionError::State`] when the plant is already promoted or promoting.
    pub fn request_promotion(&mut self, plant: PlantId) -> Result<(), VegetationPromotionError> {
        match self.state(plant) {
            PlantPromotionState::Bulk => {
                self.states.insert(plant, PlantPromotionState::Promoting);
                Ok(())
            }
            // A demotion that has not committed yet is simply cancelled.
            PlantPromotionState::Demoting { entity } => {
                self.states
                    .insert(plant, PlantPromotionState::Promoted { entity });
                Ok(())
            }
            PlantPromotionState::Promoting | PlantPromotionState::Promoted { .. } => {
                Err(VegetationPromotionError::State {
                    plant: plant.to_string(),
                    requested: "promoted",
                })
            }
        }
    }

    /// Requests demotion, committed at the next synchronization point.
    ///
    /// # Errors
    ///
    /// [`VegetationPromotionError::State`] when the plant carries no live entity view.
    pub fn request_demotion(&mut self, plant: PlantId) -> Result<(), VegetationPromotionError> {
        match self.state(plant) {
            PlantPromotionState::Promoted { entity } => {
                self.states
                    .insert(plant, PlantPromotionState::Demoting { entity });
                Ok(())
            }
            // A promotion that has not committed yet leaves nothing to demote.
            PlantPromotionState::Promoting => {
                self.states.remove(&plant);
                Ok(())
            }
            PlantPromotionState::Bulk | PlantPromotionState::Demoting { .. } => {
                Err(VegetationPromotionError::State {
                    plant: plant.to_string(),
                    requested: "demoted",
                })
            }
        }
    }

    /// Requests felling, committed at the next synchronization point: the rooted plant becomes a
    /// stump in the macro SoA, and its above-ground mass becomes a separate product entity that
    /// falls under physics.
    ///
    /// The product is not the plant. It carries no [`PlantOrigin`], so no query, contact, or save
    /// can mistake a log for the rooted plant it came from, and the stump keeps the plant identity.
    ///
    /// # Errors
    ///
    /// [`VegetationPromotionError::State`] when a felling is already queued for the plant.
    pub fn request_felling(&mut self, plant: PlantId) -> Result<(), VegetationPromotionError> {
        if self.felling.contains(&plant) {
            return Err(VegetationPromotionError::State {
                plant: plant.to_string(),
                requested: "felled",
            });
        }
        self.felling.push(plant);
        Ok(())
    }

    /// The plant's current lifecycle state.
    #[must_use]
    pub fn state(&self, plant: PlantId) -> PlantPromotionState {
        self.states.get(&plant).copied().unwrap_or_default()
    }

    /// Every tracked plant and its state, in canonical identity order.
    pub fn states(&self) -> impl Iterator<Item = (PlantId, PlantPromotionState)> + '_ {
        self.states.iter().map(|(plant, state)| (*plant, *state))
    }

    /// The current aggregate counters.
    #[must_use]
    pub fn report(&self) -> VegetationPromotionReport {
        self.report
    }

    /// Commits every queued transition: pending promotions spawn their entity view and suppress
    /// the plant's bulk representation; pending demotions write the entity's state back through
    /// the reducer and destroy it. A promotion that cannot build a view returns to bulk.
    pub(crate) fn advance(
        &mut self,
        vegetation: &mut VegetationWorld,
        scene: &mut Scene,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        physics: Option<&mut World>,
    ) {
        let mut physics = physics;
        let pending: Vec<(PlantId, PlantPromotionState)> = self
            .states
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state,
                    PlantPromotionState::Promoting | PlantPromotionState::Demoting { .. }
                )
            })
            .map(|(plant, state)| (*plant, *state))
            .collect();

        for (plant, state) in pending {
            let outcome = match state {
                PlantPromotionState::Promoting => self.commit_promotion(
                    plant,
                    vegetation,
                    scene,
                    assets,
                    families,
                    physics.as_deref_mut(),
                ),
                PlantPromotionState::Demoting { entity } => {
                    self.commit_demotion(plant, entity, vegetation, scene, physics.as_deref_mut())
                }
                PlantPromotionState::Bulk | PlantPromotionState::Promoted { .. } => Ok(()),
            };
            if let Err(error) = outcome {
                tracing::warn!("vegetation promotion transition failed: {error}");
                self.states.remove(&plant);
                self.report.failed_total += 1;
            }
        }

        for plant in std::mem::take(&mut self.felling) {
            if let Err(error) = self.commit_felling(
                plant,
                vegetation,
                scene,
                assets,
                families,
                physics.as_deref_mut(),
            ) {
                tracing::warn!("vegetation felling failed: {error}");
                self.report.failed_total += 1;
            }
        }
        self.refresh_counts();
    }

    /// Writes every promoted plant's live entity state back through the reducer as one
    /// transaction, so a snapshot taken now is complete without waiting for anything to settle.
    /// The save barrier: a promoted plant's authoritative row carries its current transform and
    /// velocity, and the transient entity keeps living.
    ///
    /// Returns how many plants were written. The transaction id and every operation key derive
    /// from the record contents, so an identical flush is an exact idempotent replay while a
    /// changed one is a fresh transaction.
    ///
    /// # Errors
    ///
    /// Propagates the reducer's rejection, or a quantization failure of a live world value.
    pub fn flush_state(
        &mut self,
        scene: &Scene,
        vegetation: &mut VegetationWorld,
        physics: Option<&World>,
    ) -> Result<usize, VegetationPromotionError> {
        let mut states = Vec::new();
        for (plant, state) in self.states() {
            let PlantPromotionState::Promoted { entity } = state else {
                continue;
            };
            let Some(handle) = scene.find_entity_by_uuid(entity) else {
                continue;
            };
            let vitals = scene.component::<PlantVitals>(handle).ok();
            states.push((plant, origin_state(scene, handle, entity, physics)?, vitals));
        }
        if states.is_empty() {
            return Ok(0);
        }

        self.flush_sequence += 1;
        let logical_tick = self.flush_sequence;
        let mut digest = Vec::new();
        for (plant, state, _) in &states {
            digest.extend_from_slice(&plant.bytes());
            digest.extend_from_slice(&state.position.global_ticks()[0].to_be_bytes());
            digest.extend_from_slice(&state.position.global_ticks()[1].to_be_bytes());
            digest.extend_from_slice(&state.position.global_ticks()[2].to_be_bytes());
            for lane in state.orientation.bits() {
                digest.extend_from_slice(&lane.to_be_bytes());
            }
            for lane in state
                .scale
                .iter()
                .chain(&state.linear_velocity)
                .chain(&state.angular_velocity)
            {
                digest.extend_from_slice(&lane.bits().to_be_bytes());
            }
        }
        let transaction = leading_u128(ContentHash::of(&digest).bytes());

        let mut records: Vec<VegetationMutationRecord> = Vec::new();
        for (index, (plant, state, vitals)) in states.iter().enumerate() {
            let header = |salt: u128| MutationHeader {
                cell: state.position.cell(),
                transaction,
                authority: PROMOTION_AUTHORITY,
                logical_tick,
                idempotency_key: transaction ^ salt,
                base_revision: None,
            };
            let index = (index as u128) + 1;
            records.push(VegetationMutationRecord {
                header: header(index << 8),
                mutation: VegetationMutation::PromotionOriginState {
                    plant: *plant,
                    state: *state,
                },
            });
            if let Some(vitals) = vitals {
                records.push(VegetationMutationRecord {
                    header: header((index << 8) | 1),
                    mutation: vitals_mutation(*plant, *vitals),
                });
            }
        }
        vegetation.apply_confirmed_mutations(&records)?;
        self.report.flushed_total += states.len() as u64;
        Ok(states.len())
    }

    /// Demotes every promoted plant with a state write-back, for a play session that is ending
    /// while the vegetation authority is still live.
    pub(crate) fn demote_all(
        &mut self,
        vegetation: &mut VegetationWorld,
        scene: &mut Scene,
        physics: Option<&mut World>,
    ) {
        let mut physics = physics;
        for (plant, state) in self.states().collect::<Vec<_>>() {
            let entity = match state {
                PlantPromotionState::Promoted { entity }
                | PlantPromotionState::Demoting { entity } => entity,
                PlantPromotionState::Bulk | PlantPromotionState::Promoting => {
                    self.states.remove(&plant);
                    continue;
                }
            };
            if let Err(error) =
                self.commit_demotion(plant, entity, vegetation, scene, physics.as_deref_mut())
            {
                tracing::warn!("vegetation demotion at teardown failed: {error}");
                self.states.remove(&plant);
            }
        }
        self.refresh_counts();
    }

    /// Drops every entity view without a write-back, for a rebind whose source generation is
    /// gone: the plants those views described no longer exist in the bound manifest, so writing
    /// their state back would record it against a different world.
    pub(crate) fn abandon(&mut self, scene: &mut Scene, physics: Option<&mut World>) {
        let mut physics = physics;
        let abandoned = self.states.len();
        for (_, state) in std::mem::take(&mut self.states) {
            let entity = match state {
                PlantPromotionState::Promoted { entity }
                | PlantPromotionState::Demoting { entity } => entity,
                PlantPromotionState::Bulk | PlantPromotionState::Promoting => continue,
            };
            destroy_view(scene, entity, physics.as_deref_mut());
        }
        if abandoned > 0 {
            tracing::warn!(
                "vegetation promotion: dropped {abandoned} promoted plant view(s) — the bound \
                 cooked generation changed"
            );
        }
        self.report = VegetationPromotionReport::default();
        self.flush_sequence = 0;
    }

    /// Forgets every view after the play world itself was dropped (the entities and bodies died
    /// with it).
    pub(crate) fn reset(&mut self) {
        self.states.clear();
        self.felling.clear();
        self.report = VegetationPromotionReport::default();
        self.flush_sequence = 0;
    }

    fn commit_promotion(
        &mut self,
        plant: PlantId,
        vegetation: &mut VegetationWorld,
        scene: &mut Scene,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        physics: Option<&mut World>,
    ) -> Result<(), VegetationPromotionError> {
        let snapshot =
            vegetation
                .find_plant(plant)?
                .ok_or(saffron_vegetation::Error::PlantNotResident {
                    plant: plant.to_string(),
                })?;
        let family = families.get(snapshot.family, assets).ok_or(
            saffron_vegetation::Error::PlantNotResident {
                plant: plant.to_string(),
            },
        )?;

        let entity = spawn_view(scene, plant, &snapshot, &family)?;
        let uuid = scene
            .component::<IdComponent>(entity)
            .map(|id| id.id)
            .map_err(|_| saffron_vegetation::Error::PlantNotResident {
                plant: plant.to_string(),
            })?;
        scene.relink_hierarchy();
        scene.update_world_transforms();
        if let Some(world) = physics {
            let mut cook = |_: Uuid| Err("a promoted plant view uses analytic shapes".to_owned());
            if let Err(error) = world.add_entity_body(scene, entity, &mut cook) {
                tracing::warn!("vegetation promotion: plant {plant} view has no body: {error}");
            }
        }

        // Suppress the bulk representation last: if anything above failed, the plant never lost
        // its bulk owner.
        if let Err(error) = vegetation.promote_plant(plant) {
            scene.destroy_entity(entity);
            return Err(error.into());
        }
        self.states
            .insert(plant, PlantPromotionState::Promoted { entity: uuid });
        self.report.promoted_total += 1;
        Ok(())
    }

    fn commit_demotion(
        &mut self,
        plant: PlantId,
        entity: Uuid,
        vegetation: &mut VegetationWorld,
        scene: &mut Scene,
        physics: Option<&mut World>,
    ) -> Result<(), VegetationPromotionError> {
        if let Some(handle) = scene.find_entity_by_uuid(entity) {
            let state = origin_state(scene, handle, entity, physics.as_deref())?;
            let transaction =
                leading_u128(ContentHash::of(&demotion_digest(plant, &state)).bytes());
            let mut records = vec![VegetationMutationRecord {
                header: MutationHeader {
                    cell: state.position.cell(),
                    transaction,
                    authority: PROMOTION_AUTHORITY,
                    logical_tick: self.flush_sequence + 1,
                    idempotency_key: leading_u128(ContentHash::of(&plant.bytes()).bytes()),
                    base_revision: None,
                },
                mutation: VegetationMutation::PromotionOriginState { plant, state },
            }];
            // Whatever the view's vitals settled at is the plant's state now: damage, drying, and
            // growth all happened to the entity, and the macro row has to learn about them.
            if let Ok(vitals) = scene.component::<PlantVitals>(handle) {
                records.push(VegetationMutationRecord {
                    header: MutationHeader {
                        cell: state.position.cell(),
                        transaction,
                        authority: PROMOTION_AUTHORITY,
                        logical_tick: self.flush_sequence + 1,
                        idempotency_key: transaction ^ 0x5654, // "VT": the vitals record
                        base_revision: None,
                    },
                    mutation: vitals_mutation(plant, vitals),
                });
            }
            vegetation.apply_confirmed_mutations(&records)?;
            self.flush_sequence += 1;
            self.report.flushed_total += 1;
        }
        destroy_view(scene, entity, physics);
        vegetation.demote_plant(plant)?;
        self.states.remove(&plant);
        self.report.demoted_total += 1;
        Ok(())
    }

    fn commit_felling(
        &mut self,
        plant: PlantId,
        vegetation: &mut VegetationWorld,
        scene: &mut Scene,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        physics: Option<&mut World>,
    ) -> Result<(), VegetationPromotionError> {
        let mut physics = physics;
        let snapshot =
            vegetation
                .find_plant(plant)?
                .ok_or(saffron_vegetation::Error::PlantNotResident {
                    plant: plant.to_string(),
                })?;
        let family = families.get(snapshot.family, assets).ok_or(
            saffron_vegetation::Error::PlantNotResident {
                plant: plant.to_string(),
            },
        )?;

        // A promoted view has been standing in for the plant; the felling supersedes it, and its
        // state returns to the macro row before the plant becomes a stump.
        if let PlantPromotionState::Promoted { entity } | PlantPromotionState::Demoting { entity } =
            self.state(plant)
        {
            self.commit_demotion(plant, entity, vegetation, scene, physics.as_deref_mut())?;
        }

        // The rooted plant stays, as a stump under the same identity.
        let stump = VegetationMutationRecord {
            header: MutationHeader {
                cell: snapshot.position.cell(),
                transaction: leading_u128(
                    ContentHash::of(&felling_digest(plant, snapshot.ecology_tick)).bytes(),
                ),
                authority: PROMOTION_AUTHORITY,
                logical_tick: self.flush_sequence + 1,
                idempotency_key: leading_u128(
                    ContentHash::of(&felling_digest(plant, snapshot.ecology_tick)).bytes(),
                ),
                base_revision: None,
            },
            mutation: VegetationMutation::LifecycleTransition {
                plant,
                // Unconditional: the reducer's `from` precondition compares against the
                // persistent delta, while a cooked plant's lifecycle lives in the immutable base.
                // The authority has already read the effective row through `find_plant`.
                from: None,
                to: PlantLifecycle::Stump,
                ecology_tick: snapshot.ecology_tick,
            },
        };
        vegetation.apply_confirmed_mutations(&[stump])?;
        self.flush_sequence += 1;

        // The product is a plain dynamic entity: same mesh and materials, no plant identity.
        let product = spawn_product(scene, plant, &snapshot, &family)?;
        scene.relink_hierarchy();
        scene.update_world_transforms();
        if let Some(world) = physics {
            let mut cook = |_: Uuid| Err("a felled product uses analytic shapes".to_owned());
            if let Err(error) = world.add_entity_body(scene, product, &mut cook) {
                tracing::warn!("vegetation felling: product has no body: {error}");
            }
        }
        self.report.felled_total += 1;
        Ok(())
    }

    fn refresh_counts(&mut self) {
        self.report.promoted = self
            .states
            .values()
            .filter(|state| matches!(state, PlantPromotionState::Promoted { .. }))
            .count();
        self.report.promoting = self
            .states
            .values()
            .filter(|state| matches!(state, PlantPromotionState::Promoting))
            .count();
        self.report.demoting = self
            .states
            .values()
            .filter(|state| matches!(state, PlantPromotionState::Demoting { .. }))
            .count();
    }
}

/// Spawns the transient entity view: the same mesh, assembly combination, and materials the bulk
/// instance renders, plus one solid body from the family's largest collision proxy so the view
/// owns collision outright.
fn spawn_view(
    scene: &mut Scene,
    plant: PlantId,
    snapshot: &VegetationPlantSnapshot,
    family: &saffron_vegetation::PlantFamilyAsset,
) -> Result<Entity, VegetationPromotionError> {
    let hex = plant.canonical_hex();
    let entity = scene.create_entity(format!("Plant {}", &hex[..8]));
    let translation = snapshot
        .position
        .to_render_relative(WorldPosition::origin())?;
    let rotation = orientation_quat(snapshot.orientation);
    let scale = Vec3::new(
        snapshot.scale[0].to_f64() as f32,
        snapshot.scale[1].to_f64() as f32,
        snapshot.scale[2].to_f64() as f32,
    );
    let _ = scene.with_component_mut::<Transform, _>(entity, |transform| {
        transform.translation = translation;
        transform.rotation = quat_to_euler_zyx(rotation);
        transform.scale = scale;
    });

    let _ = scene.add_component(
        entity,
        PlantOrigin {
            plant,
            source_generation: snapshot.handle.generation.generation,
        },
    );
    // The view carries the plant's live biological state, so gameplay reads and writes it like any
    // other component and the demotion returns whatever it settled at.
    let _ = scene.add_component(
        entity,
        PlantVitals {
            lifecycle: snapshot.lifecycle as u32,
            health: snapshot.health.to_f64() as f32,
            moisture: snapshot.moisture.to_f64() as f32,
            fuel: snapshot.fuel.to_f64() as f32,
            ecology_tick: snapshot.ecology_tick,
        },
    );
    let _ = scene.add_component(
        entity,
        PlantVariant {
            variation: snapshot.variation,
            phenotype: snapshot.phenotype,
        },
    );
    let _ = scene.add_component(
        entity,
        MeshComponent {
            mesh: snapshot.family,
        },
    );
    if !family.material_slots.is_empty() {
        let _ = scene.add_component(
            entity,
            MaterialSet {
                slots: family
                    .material_slots
                    .iter()
                    .map(|material| MaterialSlot {
                        material: *material,
                        ..MaterialSlot::default()
                    })
                    .collect(),
            },
        );
    }
    if let Some((shape, half_extents, offset)) = primary_collider(&family.collision_proxies, scale)
    {
        let _ = scene.add_component(
            entity,
            Collider {
                shape,
                half_extents,
                offset,
                ..Collider::default()
            },
        );
        let _ = scene.add_component(
            entity,
            Rigidbody {
                motion: Motion::Dynamic,
                ..Rigidbody::default()
            },
        );
    }
    Ok(entity)
}

/// The family's largest analytic proxy as a single scene collider, pre-scaled by the plant's
/// per-axis scale (bodies are built scale-free from the collider). Convex-hull proxies have no
/// cooked hull geometry, so they never become the primary shape.
fn primary_collider(proxies: &[PlantCollisionProxy], scale: Vec3) -> Option<(Shape, Vec3, Vec3)> {
    let lateral = scale.x.max(scale.z);
    let mut best: Option<(f32, Shape, Vec3, Vec3)> = None;
    for proxy in proxies {
        let dimensions = Vec3::new(
            proxy.dimensions[0].to_f64() as f32,
            proxy.dimensions[1].to_f64() as f32,
            proxy.dimensions[2].to_f64() as f32,
        );
        let (shape, half_extents) = match proxy.shape {
            PlantCollisionShape::Box => (Shape::Box, dimensions * scale),
            PlantCollisionShape::Sphere => (
                Shape::Sphere,
                Vec3::new(dimensions.x * scale.max_element(), 0.0, 0.0),
            ),
            PlantCollisionShape::Capsule => (
                Shape::Capsule,
                Vec3::new(dimensions.x * lateral, dimensions.y * scale.y, 0.0),
            ),
            PlantCollisionShape::ConvexHull => continue,
        };
        let extent = match shape {
            Shape::Sphere => half_extents.x.powi(3),
            Shape::Capsule => half_extents.x * half_extents.x * half_extents.y,
            _ => half_extents.x * half_extents.y * half_extents.z,
        };
        let offset = Vec3::new(
            proxy.center[0].to_f64() as f32,
            proxy.center[1].to_f64() as f32,
            proxy.center[2].to_f64() as f32,
        ) * scale;
        if best.as_ref().is_none_or(|(current, ..)| extent > *current) {
            best = Some((extent, shape, half_extents, offset));
        }
    }
    best.map(|(_, shape, half_extents, offset)| (shape, half_extents, offset))
}

/// Reads the entity view's live world state as the quantized payload the reducer stores. Velocity
/// comes from the live body when one exists; without a physics world the plant simply comes to
/// rest where its transform sits.
fn origin_state(
    scene: &Scene,
    handle: Entity,
    entity: Uuid,
    physics: Option<&World>,
) -> Result<PromotionOriginState, VegetationPromotionError> {
    let matrix = scene.world_matrix(handle);
    let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
    let position = WorldPosition::from_render_relative(translation, WorldPosition::origin())?;
    let linear = physics.map_or(Vec3::ZERO, |world| world.body_linear_velocity(entity));
    let angular = physics.map_or(Vec3::ZERO, |world| world.body_angular_velocity(entity));
    Ok(PromotionOriginState {
        position,
        orientation: quantize_orientation(rotation)?,
        scale: quantize_vec3(scale)?,
        // Metres per fixed tick, the unit the reducer stores.
        linear_velocity: quantize_vec3(linear * saffron_physics::FIXED_STEP)?,
        // Turns per fixed tick.
        angular_velocity: quantize_vec3(
            angular * saffron_physics::FIXED_STEP / std::f32::consts::TAU,
        )?,
    })
}

/// Spawns the product a felling separates from its rooted plant: the same visual mass as a plain
/// dynamic entity, deliberately WITHOUT [`PlantOrigin`] — the log is not the tree, and nothing
/// downstream may resolve it as one.
fn spawn_product(
    scene: &mut Scene,
    plant: PlantId,
    snapshot: &VegetationPlantSnapshot,
    family: &saffron_vegetation::PlantFamilyAsset,
) -> Result<Entity, VegetationPromotionError> {
    let hex = plant.canonical_hex();
    let entity = scene.create_entity(format!("Felled {}", &hex[..8]));
    let translation = snapshot
        .position
        .to_render_relative(WorldPosition::origin())?;
    let rotation = orientation_quat(snapshot.orientation);
    let scale = Vec3::new(
        snapshot.scale[0].to_f64() as f32,
        snapshot.scale[1].to_f64() as f32,
        snapshot.scale[2].to_f64() as f32,
    );
    let _ = scene.with_component_mut::<Transform, _>(entity, |transform| {
        transform.translation = translation;
        transform.rotation = quat_to_euler_zyx(rotation);
        transform.scale = scale;
    });
    let _ = scene.add_component(
        entity,
        PlantVariant {
            variation: snapshot.variation,
            phenotype: snapshot.phenotype,
        },
    );
    let _ = scene.add_component(
        entity,
        MeshComponent {
            mesh: snapshot.family,
        },
    );
    if !family.material_slots.is_empty() {
        let _ = scene.add_component(
            entity,
            MaterialSet {
                slots: family
                    .material_slots
                    .iter()
                    .map(|material| MaterialSlot {
                        material: *material,
                        ..MaterialSlot::default()
                    })
                    .collect(),
            },
        );
    }
    if let Some((shape, half_extents, offset)) = primary_collider(&family.collision_proxies, scale)
    {
        let _ = scene.add_component(
            entity,
            Collider {
                shape,
                half_extents,
                offset,
                ..Collider::default()
            },
        );
        let _ = scene.add_component(
            entity,
            Rigidbody {
                motion: Motion::Dynamic,
                ..Rigidbody::default()
            },
        );
    }
    Ok(entity)
}

fn felling_digest(plant: PlantId, ecology_tick: u64) -> Vec<u8> {
    let mut digest = b"fell".to_vec();
    digest.extend_from_slice(&plant.bytes());
    digest.extend_from_slice(&ecology_tick.to_be_bytes());
    digest
}

/// The state-override mutation one view's settled vitals describe. Clamped into the closed unit
/// vocabulary the reducer stores, and the lifecycle falls back to the row's own value when the
/// discriminant is out of range.
fn vitals_mutation(plant: PlantId, vitals: PlantVitals) -> VegetationMutation {
    let unit = |value: f32| {
        UnitInterval::from_f64(f64::from(value.clamp(0.0, 1.0))).unwrap_or(UnitInterval::ZERO)
    };
    VegetationMutation::StateOverride {
        plant,
        lifecycle: PlantLifecycle::try_from(vitals.lifecycle).ok(),
        phenotype: None,
        health: Some(unit(vitals.health)),
        moisture: Some(unit(vitals.moisture)),
        fuel: Some(unit(vitals.fuel)),
        interaction_policy: None,
    }
}

/// Destroys the entity view and the body it owned, in the order that never leaves a live body
/// pointing at a dead entity.
fn destroy_view(scene: &mut Scene, entity: Uuid, physics: Option<&mut World>) {
    let Some(handle) = scene.find_entity_by_uuid(entity) else {
        return;
    };
    if let Some(world) = physics {
        world.remove_entity_bodies(handle);
    }
    scene.destroy_entity(handle);
}

fn demotion_digest(plant: PlantId, state: &PromotionOriginState) -> Vec<u8> {
    let mut digest = plant.bytes().to_vec();
    for axis in state.position.global_ticks() {
        digest.extend_from_slice(&axis.to_be_bytes());
    }
    for lane in state.orientation.bits() {
        digest.extend_from_slice(&lane.to_be_bytes());
    }
    for lane in state
        .scale
        .iter()
        .chain(&state.linear_velocity)
        .chain(&state.angular_velocity)
    {
        digest.extend_from_slice(&lane.bits().to_be_bytes());
    }
    digest
}

fn orientation_quat(orientation: QuantizedOrientation) -> Quat {
    let bits = orientation.bits();
    Quat::from_xyzw(
        f32::from(bits[0]) / 32_767.0,
        f32::from(bits[1]) / 32_767.0,
        f32::from(bits[2]) / 32_767.0,
        f32::from(bits[3]) / 32_767.0,
    )
    .normalize()
}

fn quantize_orientation(rotation: Quat) -> Result<QuantizedOrientation, saffron_spatial::Error> {
    let rotation = rotation.normalize();
    let lanes = [rotation.x, rotation.y, rotation.z, rotation.w].map(|lane| {
        (f64::from(lane) * f64::from(i16::MAX))
            .round_ties_even()
            .clamp(f64::from(i16::MIN + 1), f64::from(i16::MAX)) as i16
    });
    QuantizedOrientation::new(lanes)
}

fn quantize_vec3(value: Vec3) -> Result<[DecisionScalar; 3], saffron_spatial::Error> {
    let value = DVec3::new(f64::from(value.x), f64::from(value.y), f64::from(value.z));
    Ok([
        DecisionScalar::from_f64(value.x)?,
        DecisionScalar::from_f64(value.y)?,
        DecisionScalar::from_f64(value.z)?,
    ])
}

/// The leading 16 bytes of a content hash as a non-zero transaction/operation key.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading) | 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_core::Uuid;
    use saffron_spatial::DecisionScalar;
    use saffron_vegetation::{PlantCollisionProxy, PlantCollisionShape};

    fn plant(byte: u8) -> PlantId {
        PlantId::explicit([byte; 16]).expect("plant id")
    }

    fn scalar(value: f64) -> DecisionScalar {
        DecisionScalar::from_f64(value).expect("finite decision scalar")
    }

    fn snapshot_fixture(plant: PlantId) -> VegetationPlantSnapshot {
        VegetationPlantSnapshot {
            variation: 0,
            plant,
            ecology_tick: 12,
            handle: saffron_vegetation::VegetationPlantHandle {
                plant,
                generation: saffron_vegetation::VegetationCellGenerationId {
                    cell: saffron_spatial::WorldCellKey::base(0, 0, 0),
                    generation: 3,
                },
            },
            position: WorldPosition::origin(),
            orientation: QuantizedOrientation::identity(),
            scale: [scalar(1.0); 3],
            bounds: saffron_spatial::WorldBounds::new([0, 0, 0], [1, 1, 1]).expect("bounds"),
            family: Uuid(4),
            tags: Vec::new(),
            lifecycle: PlantLifecycle::Mature,
            phenotype: 1,
            interaction_policy: saffron_vegetation::InteractionPolicy::Harvestable,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ZERO,
            fuel: UnitInterval::ZERO,
            ignited: false,
            provenance: None,
        }
    }

    fn family_fixture() -> saffron_vegetation::PlantFamilyAsset {
        saffron_vegetation::PlantFamilyAsset {
            role: saffron_vegetation::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
            version: saffron_vegetation::PLANT_ASSET_VERSION,
            id: Uuid(4),
            name: "fixture".to_owned(),
            tags: Vec::new(),
            source: saffron_vegetation::PlantFamilySource::Native {
                graph: saffron_vegetation::BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
            parts: Vec::new(),
            dimensions: saffron_vegetation::PlantDimensions {
                height: scalar(4.0),
                trunk_radius: scalar(0.3),
                crown_radius: [scalar(1.0); 2],
                root_radius: [scalar(0.5); 2],
                local_bounds_min: [scalar(-1.0); 3],
                local_bounds_max: [scalar(1.0); 3],
            },
            material_slots: vec![Uuid(11)],
            spines: Vec::new(),
            mechanics: saffron_vegetation::MechanicalResponse {
                stiffness: scalar(1.0),
                damping: UnitInterval::from_bits(1000),
                drag: scalar(0.5),
                flutter: scalar(0.2),
                bend_limit: UnitInterval::from_bits(2000),
                damage_threshold: scalar(0.5),
                break_threshold: scalar(0.9),
            },
            variations: Vec::new(),
            phenotypes: Vec::new(),
            collision_proxies: vec![proxy(PlantCollisionShape::Capsule, 0.3, 2.0)],
            navigation_proxies: Vec::new(),
            interaction_policy: saffron_vegetation::InteractionPolicy::Harvestable,
            habitat: None,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        }
    }

    fn proxy(shape: PlantCollisionShape, radius: f64, height: f64) -> PlantCollisionProxy {
        PlantCollisionProxy {
            id: 1,
            shape,
            part: 0,
            center: [scalar(0.0), scalar(height), scalar(0.0)],
            dimensions: [scalar(radius), scalar(height), scalar(radius)],
            breakable: false,
        }
    }

    #[test]
    fn requests_move_through_the_lifecycle_and_reject_repeats() {
        let mut promotion = VegetationPromotion::default();
        let tree = plant(3);
        assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);

        promotion.request_promotion(tree).expect("promote request");
        assert_eq!(promotion.state(tree), PlantPromotionState::Promoting);
        // A second promotion of the same plant would mean two owners.
        assert!(promotion.request_promotion(tree).is_err());

        // Demoting an uncommitted promotion cancels it outright.
        promotion.request_demotion(tree).expect("cancel request");
        assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);
        assert!(promotion.request_demotion(tree).is_err());
    }

    #[test]
    fn demotion_of_a_live_view_is_cancelled_by_a_new_promotion() {
        let mut promotion = VegetationPromotion::default();
        let tree = plant(4);
        let entity = Uuid(77);
        promotion
            .states
            .insert(tree, PlantPromotionState::Promoted { entity });

        promotion.request_demotion(tree).expect("demote request");
        assert_eq!(
            promotion.state(tree),
            PlantPromotionState::Demoting { entity }
        );
        // Re-promoting before the transition commits keeps the same live entity.
        promotion.request_promotion(tree).expect("re-promote");
        assert_eq!(
            promotion.state(tree),
            PlantPromotionState::Promoted { entity }
        );
    }

    #[test]
    fn felling_requests_queue_once_and_products_carry_no_plant_identity() {
        let mut promotion = VegetationPromotion::default();
        let tree = plant(5);
        promotion.request_felling(tree).expect("felling request");
        // A second request would fell the same plant twice.
        assert!(promotion.request_felling(tree).is_err());
        // Felling is an operation, not a state: the plant is still bulk until it commits.
        assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);

        // The product entity a felling spawns is deliberately not the plant.
        let mut scene = Scene::new();
        let snapshot = snapshot_fixture(tree);
        let family = family_fixture();
        let product = spawn_product(&mut scene, tree, &snapshot, &family).expect("product");
        assert!(
            !scene.has_component::<PlantOrigin>(product),
            "a felled log must never resolve as the rooted plant it came from"
        );
        assert!(
            !scene.has_component::<PlantVitals>(product),
            "the product carries no plant biology — the stump keeps it"
        );
        // It is an ordinary dynamic entity that renders the same mass.
        assert!(scene.has_component::<MeshComponent>(product));
        assert!(scene.has_component::<Rigidbody>(product));
    }

    #[test]
    fn primary_collider_picks_the_largest_scaled_proxy() {
        let proxies = [
            proxy(PlantCollisionShape::Capsule, 0.2, 2.0),
            proxy(PlantCollisionShape::Box, 0.05, 0.05),
            proxy(PlantCollisionShape::ConvexHull, 5.0, 5.0),
        ];
        let (shape, half_extents, offset) =
            primary_collider(&proxies, Vec3::new(2.0, 1.0, 2.0)).expect("a primary shape");
        // The capsule wins; a convex hull has no cooked geometry so it never competes.
        assert_eq!(shape, Shape::Capsule);
        assert!((half_extents.x - 0.4).abs() < 1e-4);
        assert!((half_extents.y - 2.0).abs() < 1e-4);
        // The proxy centre scales per axis into the collider's local offset.
        assert!((offset.y - 2.0).abs() < 1e-4);
    }

    #[test]
    fn hull_only_families_get_no_primary_collider() {
        let proxies = [proxy(PlantCollisionShape::ConvexHull, 1.0, 1.0)];
        assert!(primary_collider(&proxies, Vec3::ONE).is_none());
    }

    #[test]
    fn orientation_round_trips_through_quantization() {
        let rotation = Quat::from_rotation_y(0.75).normalize();
        let quantized = quantize_orientation(rotation).expect("quantized orientation");
        let restored = orientation_quat(quantized);
        assert!(
            restored.dot(rotation).abs() > 0.9999,
            "quantization preserved the orientation ({restored:?} vs {rotation:?})"
        );
    }

    #[test]
    fn content_derived_keys_are_stable_and_non_zero() {
        let state = PromotionOriginState {
            position: WorldPosition::origin(),
            orientation: QuantizedOrientation::identity(),
            scale: [scalar(1.0); 3],
            linear_velocity: [scalar(0.5); 3],
            angular_velocity: [scalar(0.1); 3],
        };
        let first = leading_u128(ContentHash::of(&demotion_digest(plant(9), &state)).bytes());
        let again = leading_u128(ContentHash::of(&demotion_digest(plant(9), &state)).bytes());
        let other = leading_u128(ContentHash::of(&demotion_digest(plant(10), &state)).bytes());
        assert_eq!(first, again, "identical contents replay as one transaction");
        assert_ne!(first, other, "a different plant is a different transaction");
        assert_ne!(first, 0, "a zero transaction id is rejected by the reducer");
    }
}
