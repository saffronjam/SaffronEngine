//! The vegetation query surface: bounds, radius, ray, nearest, and combustion sampling.

use std::collections::BTreeSet;

use glam::DVec3;
use saffron_core::Uuid;
use saffron_spatial::{UnitInterval, WorldBounds, WorldCellKey, WorldPosition};

use crate::{Error, InteractionPolicy, PlantId, PlantLifecycle, PlantPoint, PlantTagId, Result};

use super::VegetationWorld;
use super::bvh::distance_squared_to_bounds;
use super::generation::{VegetationPlantHandle, VegetationPlantSnapshot};
use super::simulation::canopy_share;

/// What a volume holds, for a system that needs to know whether it will burn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationCombustionSample {
    /// Plants matched by the sample.
    pub plants: u32,
    /// How many of them are alight.
    pub ignited: u32,
    /// Mean combustible fuel.
    pub fuel: UnitInterval,
    /// Mean persistent moisture.
    pub moisture: UnitInterval,
    /// Mean health.
    pub health: UnitInterval,
    /// Ground covered by the matched plants, as a share of one cell.
    pub occupancy: UnitInterval,
}

/// Closed filters shared by every vegetation macro query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationQueryFilter {
    /// Allowed families; empty accepts every family.
    pub families: Vec<Uuid>,
    /// Every listed tag must be present on the family.
    pub required_tags: BTreeSet<PlantTagId>,
    /// Allowed lifecycle values; empty accepts every lifecycle.
    pub lifecycles: BTreeSet<PlantLifecycle>,
    /// Allowed interaction policies; empty accepts every policy.
    pub interaction_policies: Vec<InteractionPolicy>,
}

impl VegetationQueryFilter {
    pub(super) fn matches(&self, point: &PlantPoint, tags: &[PlantTagId]) -> bool {
        (self.families.is_empty() || self.families.contains(&point.family))
            && self.required_tags.iter().all(|tag| tags.contains(tag))
            && (self.lifecycles.is_empty() || self.lifecycles.contains(&point.lifecycle))
            && (self.interaction_policies.is_empty()
                || self
                    .interaction_policies
                    .contains(&point.interaction_policy))
    }
}

/// One nonpersistent micro-field paint-feedback hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VegetationMicroHit {
    /// World-space hit position in metres.
    pub position: saffron_geometry::glam::DVec3,
    /// Metric distance from the ray origin.
    pub distance_m: f64,
    /// The field's plant family.
    pub family: Uuid,
    /// The owning cell.
    pub cell: WorldCellKey,
}

/// Finite world-space ray used only by the vegetation query surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VegetationQueryRay {
    /// Exact quantized origin.
    pub origin: WorldPosition,
    /// Normalized world-space direction.
    pub direction: DVec3,
    /// Inclusive maximum distance in metres.
    pub max_distance_m: f64,
}

impl VegetationQueryRay {
    /// Validates and normalizes a finite non-zero direction.
    pub fn new(origin: WorldPosition, direction: DVec3, max_distance_m: f64) -> Result<Self> {
        if !direction.is_finite()
            || direction.length_squared() == 0.0
            || !max_distance_m.is_finite()
            || max_distance_m < 0.0
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "ray".to_owned(),
            });
        }
        Ok(Self {
            origin,
            direction: direction.normalize(),
            max_distance_m,
        })
    }
}

/// One bounds-level vegetation ray hit.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationRayHit {
    pub plant: VegetationPlantSnapshot,
    /// Entry distance into its conservative bounds.
    pub distance_m: f64,
}

/// One nearest-plant result.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationNearestHit {
    pub plant: VegetationPlantSnapshot,
    /// Euclidean point-to-bounds distance.
    pub distance_m: f64,
}

impl VegetationWorld {
    pub fn resolve_handle(&self, handle: VegetationPlantHandle) -> Result<VegetationPlantSnapshot> {
        let generation =
            self.cell_snapshot(handle.generation.cell)
                .ok_or(Error::PlantNotResident {
                    plant: handle.plant.to_string(),
                })?;
        if generation.id.generation != handle.generation.generation {
            return Err(Error::StaleGeneration {
                cell: handle.generation.cell,
                expected: handle.generation.generation,
                current: generation.id.generation,
            });
        }
        let slot = generation
            .slots
            .get(&handle.plant)
            .copied()
            .ok_or_else(|| Error::PlantNotResident {
                plant: handle.plant.to_string(),
            })?;
        generation.snapshot(slot)
    }

    /// Looks up one stable identity across CPU-resident macro generations.
    pub fn find_plant(&self, plant: PlantId) -> Result<Option<VegetationPlantSnapshot>> {
        for generation in self.resident_macro_generations() {
            if let Some(slot) = generation.slots.get(&plant).copied() {
                return generation.snapshot(slot).map(Some);
            }
        }
        Ok(None)
    }

    /// Exact-bounds query over every CPU-resident macro cell.
    pub fn query_bounds(
        &self,
        bounds: WorldBounds,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationPlantSnapshot>> {
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.query_bounds(bounds) {
                if let Some(snapshot) = generation.matching_snapshot(row, filter)? {
                    results.push(snapshot);
                }
            }
        }
        results.sort_unstable_by_key(|snapshot| snapshot.plant);
        Ok(results)
    }

    /// Radius query over conservative bounds, independent of render or collision visibility.
    pub fn query_radius(
        &self,
        center: WorldPosition,
        radius_m: f64,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationPlantSnapshot>> {
        if !radius_m.is_finite() || radius_m < 0.0 {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "radius".to_owned(),
            });
        }
        let center_m = center.world_meters();
        let extent = DVec3::splat(radius_m);
        let candidate_bounds =
            WorldBounds::from_world_meters(center_m - extent, center_m + extent)?;
        let radius_squared = radius_m * radius_m;
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.query_bounds(candidate_bounds) {
                let point_bounds = generation.macro_points.bounds[row as usize];
                if distance_squared_to_bounds(center_m, point_bounds) <= radius_squared
                    && let Some(snapshot) = generation.matching_snapshot(row, filter)?
                {
                    results.push(snapshot);
                }
            }
        }
        results.sort_unstable_by_key(|snapshot| snapshot.plant);
        Ok(results)
    }

    /// Bounds ray query sorted by distance and stable identity.
    pub fn query_ray(
        &self,
        ray: VegetationQueryRay,
        filter: &VegetationQueryFilter,
    ) -> Result<Vec<VegetationRayHit>> {
        let origin = ray.origin.world_meters();
        let mut results = Vec::new();
        for generation in self.resident_macro_generations() {
            for (row, distance_m) in
                generation
                    .bvh
                    .query_ray(origin, ray.direction, ray.max_distance_m)
            {
                if let Some(plant) = generation.matching_snapshot(row, filter)? {
                    results.push(VegetationRayHit { plant, distance_m });
                }
            }
        }
        results.sort_by(|left, right| {
            left.distance_m
                .total_cmp(&right.distance_m)
                .then_with(|| left.plant.plant.cmp(&right.plant.plant))
        });
        Ok(results)
    }

    /// Nearest micro-field ground hit along a ray: the first resident cell whose
    /// floor-plane crossing lands on a texel with nonzero density. The hit is
    /// nonpersistent paint feedback — micro blades have no identity.
    pub fn query_micro_ray(&self, ray: VegetationQueryRay) -> Option<VegetationMicroHit> {
        let origin = ray.origin.world_meters();
        let mut nearest: Option<VegetationMicroHit> = None;
        for (cell, generation) in self.resident_cells() {
            let Some(tiles) = generation.micro_fields() else {
                continue;
            };
            let bounds = cell.bounds();
            let tick = 1.0 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
            let min = bounds.min_ticks().map(|value| value as f64 * tick);
            let max = bounds
                .max_ticks_exclusive()
                .map(|value| value as f64 * tick);
            if ray.direction.y.abs() < 1e-9 {
                continue;
            }
            let t = (min[1] - origin.y) / ray.direction.y;
            if t < 0.0 || t > ray.max_distance_m {
                continue;
            }
            let point = origin + ray.direction * t;
            if point.x < min[0] || point.x >= max[0] || point.z < min[2] || point.z >= max[2] {
                continue;
            }
            for tile in tiles {
                let dims = tile.dimensions;
                let texel_x =
                    ((point.x - min[0]) / (max[0] - min[0]) * f64::from(dims[0])).floor() as u32;
                let texel_z =
                    ((point.z - min[2]) / (max[2] - min[2]) * f64::from(dims[2])).floor() as u32;
                let texel_x = texel_x.min(dims[0].saturating_sub(1));
                let texel_z = texel_z.min(dims[2].saturating_sub(1));
                let index = (texel_x + dims[0] * dims[1] * texel_z) as usize;
                if tile.density.get(index).is_none_or(|density| *density == 0) {
                    continue;
                }
                if nearest
                    .as_ref()
                    .is_none_or(|current| t < current.distance_m)
                {
                    nearest = Some(VegetationMicroHit {
                        position: point,
                        distance_m: t,
                        family: tile.family,
                        cell,
                    });
                }
            }
        }
        nearest
    }

    /// Nearest matching plant within an optional finite maximum distance.
    pub fn query_nearest(
        &self,
        position: WorldPosition,
        max_distance_m: Option<f64>,
        filter: &VegetationQueryFilter,
    ) -> Result<Option<VegetationNearestHit>> {
        if max_distance_m.is_some_and(|distance| !distance.is_finite() || distance < 0.0) {
            return Err(Error::ArtifactFormat {
                format: "vegetation runtime query",
                field: "nearest.maxDistance".to_owned(),
            });
        }
        let point = position.world_meters();
        let mut best: Option<VegetationNearestHit> = None;
        for generation in self.resident_macro_generations() {
            for row in generation.bvh.rows_by_nearness(point) {
                let distance_m =
                    distance_squared_to_bounds(point, generation.macro_points.bounds[row as usize])
                        .sqrt();
                if max_distance_m.is_some_and(|maximum| distance_m > maximum) {
                    continue;
                }
                let Some(plant) = generation.matching_snapshot(row, filter)? else {
                    continue;
                };
                let replace = best.as_ref().is_none_or(|current| {
                    distance_m < current.distance_m
                        || (distance_m == current.distance_m && plant.plant < current.plant.plant)
                });
                if replace {
                    best = Some(VegetationNearestHit { plant, distance_m });
                }
            }
        }
        Ok(best)
    }

    /// Samples the combustible state of a volume: what is growing there, how much of it is alight,
    /// and how wet it is.
    ///
    /// This is the seam a fire system reads. Vegetation owns fuel, moisture, health, occupancy, and
    /// the persistent record of what is burning; heat propagation and smoke belong to the system
    /// that calls this and answers with [`VegetationMutation::Ignite`],
    /// [`VegetationMutation::Extinguish`], [`VegetationMutation::Burn`], and
    /// [`VegetationMutation::MoistureFuel`].
    ///
    /// # Errors
    ///
    /// Propagates [`Self::query_bounds`].
    pub fn combustion_sample(
        &self,
        bounds: WorldBounds,
        filter: &VegetationQueryFilter,
    ) -> Result<VegetationCombustionSample> {
        let plants = self.query_bounds(bounds, filter)?;
        if plants.is_empty() {
            return Ok(VegetationCombustionSample::default());
        }
        let mean = |total: u64| {
            UnitInterval::from_bits(u16::try_from(total / plants.len() as u64).unwrap_or(u16::MAX))
        };
        let sum = |select: fn(&VegetationPlantSnapshot) -> UnitInterval| {
            plants
                .iter()
                .map(|plant| u64::from(select(plant).bits()))
                .sum::<u64>()
        };
        let occupancy = plants
            .iter()
            .map(|plant| u64::from(canopy_share(plant.bounds, plant.position.cell()).bits()))
            .sum::<u64>();
        Ok(VegetationCombustionSample {
            plants: plants.len() as u32,
            ignited: plants.iter().filter(|plant| plant.ignited).count() as u32,
            fuel: mean(sum(|plant| plant.fuel)),
            moisture: mean(sum(|plant| plant.moisture)),
            health: mean(sum(|plant| plant.health)),
            occupancy: UnitInterval::from_bits(
                u16::try_from(occupancy).unwrap_or(UnitInterval::ONE.bits()),
            ),
        })
    }
}
