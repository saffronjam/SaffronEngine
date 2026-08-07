//! Batched vegetation collision-facet residency.
//!
//! A physics-facet-resident cell materializes simplified Jolt proxies for its macro plants: one
//! batched create per published generation, one batched remove when that generation is superseded or
//! leaves residency, every body registered under `WorldHitTarget::Vegetation(PlantId)`. The
//! interaction policy picks the body class — `Decorative` never collides and micro/grass never
//! reaches these rows at all, `Interactive` is a query-only sensor, `Structural` and `Harvestable`
//! are solid near-field statics.
//!
//! Bodies are generation-tagged, and a republished cell's old bodies are removed in the same
//! synchronization pass that creates the new generation's, so exactly one collision owner exists per
//! plant at every point the simulation can observe.
//!
//! A cell mid-catch-up carries no bodies. Several ticks of lifecycle changes are still to run, and
//! materializing a batch per intermediate tick would both publish state nobody should observe and
//! rebuild the whole cell's proxies every frame of the catch-up. A cell whose region cannot run at
//! all still carries bodies, from its last committed generation: it is not going to change until the
//! ground it depends on loads.

use std::collections::BTreeMap;

use glam::{DQuat, DVec3, Quat, Vec3};
use saffron_assets::AssetServer;
use saffron_physics::{INVALID_BODY_ID, StaticTargetBodyCreate, World, WorldHitTarget};
use saffron_scene::Shape;
use saffron_spatial::{ResidencyFacet, WorldCellKey};
use saffron_vegetation::{
    EcologyInfluence, InteractionPolicy, PlantCollisionProxy, PlantCollisionShape,
    VegetationCollisionInput, VegetationWorld,
};

use crate::vegetation_family::PlantFamilyCache;

/// Surface friction every derived vegetation proxy body carries.
const VEGETATION_BODY_FRICTION: f32 = 0.6;

/// Aggregate collision-facet counters for control/CLI inspection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationCollisionReport {
    /// Cells currently carrying collision bodies.
    pub resident_cells: usize,
    /// Live proxy bodies across every resident cell.
    pub resident_bodies: usize,
    /// Bodies created since the physics world came up.
    pub created_total: u64,
    /// Bodies removed since the physics world came up.
    pub removed_total: u64,
    /// Proxy rows skipped because the family declares a convex-hull proxy, which has no cooked
    /// hull geometry to build from.
    pub hull_skipped_total: u64,
    /// Distinct families whose `.splant` asset failed to load; their plants carry no bodies.
    pub failed_families: usize,
}

struct ResidentCellCollision {
    generation: u64,
    bulk_revision: u64,
    bodies: Vec<u32>,
}

/// Play-session owner of the vegetation collision facet: diffs physics-resident cell
/// generations against the live body set at one fixed synchronization point per frame.
#[derive(Default)]
pub(crate) struct VegetationCollisionResidency {
    cells: BTreeMap<WorldCellKey, ResidentCellCollision>,
    report: VegetationCollisionReport,
}

impl VegetationCollisionResidency {
    /// The fixed synchronization point: removes every superseded generation's bodies, then
    /// materializes bodies for newly resident generations whose biology is settled, in one pass.
    pub(crate) fn advance(
        &mut self,
        vegetation: &VegetationWorld,
        physics: &mut World,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        influence: EcologyInfluence,
    ) {
        let mut desired = BTreeMap::new();
        for (cell, generation) in vegetation.resident_cells() {
            if generation
                .resident_facets()
                .contains(ResidencyFacet::Physics)
                && vegetation.simulation_facet_is_settled(cell, influence)
            {
                desired.insert(cell, generation);
            }
        }

        // Superseded generations go first so a republished cell never has two collision owners.
        // A promotion or demotion moves the cell's bulk-suppression revision, which retires the
        // batch exactly like a republication: the promoted entity owns collision from then on.
        let stale: Vec<WorldCellKey> = self
            .cells
            .iter()
            .filter(|(cell, entry)| {
                desired.get(*cell).map(|generation| {
                    (
                        generation.id().generation,
                        vegetation.cell_bulk_revision(**cell),
                    )
                }) != Some((entry.generation, entry.bulk_revision))
            })
            .map(|(cell, _)| *cell)
            .collect();
        for cell in stale {
            let entry = self.cells.remove(&cell).expect("stale key was snapshotted");
            self.report.removed_total += entry.bodies.len() as u64;
            physics.remove_bodies(&entry.bodies);
        }

        for (cell, generation) in desired {
            if self.cells.contains_key(&cell) {
                continue;
            }
            let mut creates = Vec::new();
            for row in generation.collision_inputs().unwrap_or(&[]) {
                if vegetation.is_bulk_suppressed(row.plant) {
                    continue;
                }
                self.append_plant_bodies(row, assets, families, &mut creates);
            }
            let bodies: Vec<u32> = physics
                .add_static_target_bodies(&creates)
                .into_iter()
                .filter(|&id| id != INVALID_BODY_ID)
                .collect();
            self.report.created_total += bodies.len() as u64;
            self.cells.insert(
                cell,
                ResidentCellCollision {
                    generation: generation.id().generation,
                    bulk_revision: vegetation.cell_bulk_revision(cell),
                    bodies,
                },
            );
        }

        self.report.resident_cells = self.cells.len();
        self.report.resident_bodies = self.cells.values().map(|entry| entry.bodies.len()).sum();
        self.report.failed_families = families.failed_families();
    }

    /// Removes every resident body from a still-live physics world (the vegetation authority
    /// went away while play continues).
    pub(crate) fn remove_all(&mut self, physics: &mut World) {
        for (_, entry) in std::mem::take(&mut self.cells) {
            self.report.removed_total += entry.bodies.len() as u64;
            physics.remove_bodies(&entry.bodies);
        }
        self.report.resident_cells = 0;
        self.report.resident_bodies = 0;
    }

    /// Forgets every tracked body and counter after the physics world itself was dropped (the
    /// bodies died with it).
    pub(crate) fn reset(&mut self) {
        self.cells.clear();
        self.report = VegetationCollisionReport::default();
    }

    /// The current aggregate counters.
    pub(crate) fn report(&self) -> VegetationCollisionReport {
        self.report
    }

    fn append_plant_bodies(
        &mut self,
        row: &VegetationCollisionInput,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        out: &mut Vec<StaticTargetBodyCreate>,
    ) {
        let Some(sensor) = body_class(row.interaction_policy) else {
            return;
        };
        let Some(family) = families.get(row.family, assets) else {
            return;
        };
        self.report.hull_skipped_total +=
            derive_proxy_bodies(row, sensor, &family.collision_proxies, out);
    }
}

/// The body class an interaction policy selects: `None` never collides, `Some(true)` is a
/// query-only sensor, `Some(false)` a solid near-field static.
fn body_class(policy: InteractionPolicy) -> Option<bool> {
    match policy {
        InteractionPolicy::Decorative => None,
        InteractionPolicy::Interactive => Some(true),
        InteractionPolicy::Structural | InteractionPolicy::Harvestable => Some(false),
    }
}

/// Composes one plant's proxy set into world-space body rows and returns how many convex-hull
/// proxies were skipped (they carry no cooked hull geometry to build from).
fn derive_proxy_bodies(
    row: &VegetationCollisionInput,
    sensor: bool,
    proxies: &[PlantCollisionProxy],
    out: &mut Vec<StaticTargetBodyCreate>,
) -> u64 {
    let bits = row.orientation.bits();
    let rotation_d = DQuat::from_xyzw(
        f64::from(bits[0]) / 32_767.0,
        f64::from(bits[1]) / 32_767.0,
        f64::from(bits[2]) / 32_767.0,
        f64::from(bits[3]) / 32_767.0,
    )
    .normalize();
    let scale = DVec3::new(
        row.scale[0].to_f64(),
        row.scale[1].to_f64(),
        row.scale[2].to_f64(),
    );
    let base = row.position.world_meters();
    let rotation = Quat::from_xyzw(
        rotation_d.x as f32,
        rotation_d.y as f32,
        rotation_d.z as f32,
        rotation_d.w as f32,
    );

    let mut hull_skipped = 0_u64;
    for proxy in proxies {
        let center = DVec3::new(
            proxy.center[0].to_f64(),
            proxy.center[1].to_f64(),
            proxy.center[2].to_f64(),
        ) * scale;
        let position = base + rotation_d * center;
        let dimensions = DVec3::new(
            proxy.dimensions[0].to_f64(),
            proxy.dimensions[1].to_f64(),
            proxy.dimensions[2].to_f64(),
        );
        let (shape, half_extents) = match proxy.shape {
            PlantCollisionShape::Box => (Shape::Box, (dimensions * scale).as_vec3()),
            // The quantized per-axis scale meets an isotropic shape: the conservative envelope
            // scales by the largest axis.
            PlantCollisionShape::Sphere => (
                Shape::Sphere,
                Vec3::new((dimensions.x * scale.max_element()) as f32, 0.0, 0.0),
            ),
            PlantCollisionShape::Capsule => (
                Shape::Capsule,
                Vec3::new(
                    (dimensions.x * scale.x.max(scale.z)) as f32,
                    (dimensions.y * scale.y) as f32,
                    0.0,
                ),
            ),
            PlantCollisionShape::ConvexHull => {
                hull_skipped += 1;
                continue;
            }
        };
        out.push(StaticTargetBodyCreate {
            target: WorldHitTarget::Vegetation(row.plant),
            shape,
            half_extents,
            position: position.as_vec3(),
            rotation,
            sensor,
            friction: VEGETATION_BODY_FRICTION,
        });
    }
    hull_skipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_core::Uuid;
    use saffron_spatial::{
        DecisionScalar, PlantId, QuantizedOrientation, WorldBounds, WorldPosition,
    };
    use saffron_vegetation::PlantLifecycle;

    fn scalar(value: f64) -> DecisionScalar {
        DecisionScalar::from_f64(value).expect("finite decision scalar")
    }

    fn row(policy: InteractionPolicy) -> VegetationCollisionInput {
        VegetationCollisionInput {
            plant: PlantId::explicit([21; 16]).expect("plant id"),
            family: Uuid(7),
            position: WorldPosition::from_world_meters(glam::DVec3::new(10.0, 0.0, -4.0))
                .expect("position quantizes"),
            orientation: QuantizedOrientation::identity(),
            scale: [scalar(2.0), scalar(1.0), scalar(2.0)],
            bounds: WorldBounds::new([0, 0, 0], [1, 1, 1]).expect("bounds"),
            interaction_policy: policy,
            lifecycle: PlantLifecycle::Mature,
        }
    }

    fn proxy(shape: PlantCollisionShape) -> PlantCollisionProxy {
        PlantCollisionProxy {
            id: 1,
            shape,
            part: 0,
            center: [scalar(0.0), scalar(1.5), scalar(0.0)],
            dimensions: [scalar(0.3), scalar(1.5), scalar(0.3)],
            breakable: false,
        }
    }

    #[test]
    fn policies_select_the_body_class() {
        assert_eq!(body_class(InteractionPolicy::Decorative), None);
        assert_eq!(body_class(InteractionPolicy::Interactive), Some(true));
        assert_eq!(body_class(InteractionPolicy::Structural), Some(false));
        assert_eq!(body_class(InteractionPolicy::Harvestable), Some(false));
    }

    #[test]
    fn proxies_compose_world_transform_and_scale() {
        let row = row(InteractionPolicy::Structural);
        let mut out = Vec::new();
        let skipped = derive_proxy_bodies(
            &row,
            false,
            &[
                proxy(PlantCollisionShape::Capsule),
                proxy(PlantCollisionShape::Box),
            ],
            &mut out,
        );
        assert_eq!(skipped, 0);
        assert_eq!(out.len(), 2);

        let capsule = &out[0];
        assert_eq!(capsule.target, WorldHitTarget::Vegetation(row.plant));
        assert_eq!(capsule.shape, Shape::Capsule);
        assert!(!capsule.sensor);
        // The identity-rotated centre offset scales per axis: (0, 1.5·1, 0) over (10, 0, -4).
        assert!((capsule.position - Vec3::new(10.0, 1.5, -4.0)).length() < 1e-4);
        // Capsule radius scales by the larger lateral axis (2.0), half-height by y (1.0).
        assert!((capsule.half_extents.x - 0.6).abs() < 1e-4);
        assert!((capsule.half_extents.y - 1.5).abs() < 1e-4);

        let boxed = &out[1];
        assert_eq!(boxed.shape, Shape::Box);
        // Box half-extents scale component-wise: (0.3·2, 1.5·1, 0.3·2).
        assert!((boxed.half_extents - Vec3::new(0.6, 1.5, 0.6)).length() < 1e-4);
    }

    #[test]
    fn hull_proxies_are_counted_and_skipped() {
        let row = row(InteractionPolicy::Harvestable);
        let mut out = Vec::new();
        let skipped = derive_proxy_bodies(
            &row,
            false,
            &[
                proxy(PlantCollisionShape::ConvexHull),
                proxy(PlantCollisionShape::Sphere),
            ],
            &mut out,
        );
        assert_eq!(skipped, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].shape, Shape::Sphere);
        // The isotropic sphere radius scales by the largest axis (2.0).
        assert!((out[0].half_extents.x - 0.6).abs() < 1e-4);
    }
}
