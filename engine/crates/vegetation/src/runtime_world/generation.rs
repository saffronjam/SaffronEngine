//! Immutable published cell generations and the plant snapshots readers see.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, ResidencyFacet, ResidencyMask, UnitInterval, WorldBounds, WorldCellKey,
    WorldPosition,
};

use crate::{
    ContentHash, DisturbanceTileKey, Error, InteractionPolicy, MicroFieldTile, PlantFlags, PlantId,
    PlantLifecycle, PlantPoint, PlantPointColumns, PlantTagId, ProvenanceHandle, ProvenanceRecord,
    ProvenanceTable, QuantizedOrientation, Result, VegetationCellFacet, VegetationCellSectionKind,
    VegetationCellState,
};

use super::bvh::MacroBvh;
use super::query::VegetationQueryFilter;

/// Stable identity of one completely published runtime cell generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationCellGenerationId {
    pub cell: WorldCellKey,
    /// Monotonic generation within the cell publication slot.
    pub generation: u64,
}

/// Opaque generation-tagged reference to a stable plant identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationPlantHandle {
    /// Stable public identity.
    pub plant: PlantId,
    /// Cell generation that produced the handle.
    pub generation: VegetationCellGenerationId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlantSlot {
    pub(super) row: u32,
    pub(super) generation: u64,
}

/// Immutable query result; no internal row or slot escapes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationPlantSnapshot {
    pub plant: PlantId,
    /// Monotonic biological tick.
    pub ecology_tick: u64,
    /// Generation-tagged lookup handle.
    pub handle: VegetationPlantHandle,
    pub position: WorldPosition,
    pub orientation: QuantizedOrientation,
    /// Q15.16 local scale.
    pub scale: [DecisionScalar; 3],
    /// Conservative world bounds.
    pub bounds: WorldBounds,
    pub family: Uuid,
    /// Canonically ordered family tags.
    pub tags: Vec<PlantTagId>,
    pub lifecycle: PlantLifecycle,
    /// Family variation, which selects the individual's geometry.
    pub variation: u32,
    pub phenotype: u32,
    pub interaction_policy: InteractionPolicy,
    pub health: UnitInterval,
    pub moisture: UnitInterval,
    pub fuel: UnitInterval,
    /// Whether the plant is alight.
    pub ignited: bool,
    /// Compact accepted-point provenance when the editing facet is resident.
    pub provenance: Option<ProvenanceRecord>,
}

/// Complete immutable data visible to readers for one cell generation.
#[derive(Clone, Debug)]
pub struct VegetationCellGeneration {
    pub(super) id: VegetationCellGenerationId,
    pub(super) manifest_identity: ContentHash,
    pub(super) resident: ResidencyMask,
    pub(super) facets: BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
    pub(super) macro_points: PlantPointColumns,
    pub(super) slots: BTreeMap<PlantId, PlantSlot>,
    pub(super) bvh: MacroBvh,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
}

impl VegetationCellGeneration {
    pub(super) fn empty(
        cell: WorldCellKey,
        manifest_identity: ContentHash,
        family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    ) -> Result<Self> {
        Self::build(
            VegetationCellGenerationId {
                cell,
                generation: 0,
            },
            manifest_identity,
            ResidencyMask::NONE,
            BTreeMap::new(),
            PlantPointColumns::default(),
            family_tags,
            BTreeMap::new(),
        )
    }

    pub(super) fn build(
        id: VegetationCellGenerationId,
        manifest_identity: ContentHash,
        resident: ResidencyMask,
        facets: BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
        macro_points: PlantPointColumns,
        family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
        disturbance_masks: BTreeMap<DisturbanceTileKey, Vec<i16>>,
    ) -> Result<Self> {
        let rows = macro_points.row_count()?;
        let mut slots = BTreeMap::new();
        let mut bounds = Vec::new();
        bounds
            .try_reserve_exact(rows)
            .map_err(|source| Error::MemoryReservation {
                resource: "vegetation runtime BVH bounds",
                source,
            })?;
        for row in 0..rows {
            let id_value = macro_points.ids[row];
            let row = u32::try_from(row).map_err(|_| Error::NumericOverflow)?;
            if slots
                .insert(
                    id_value,
                    PlantSlot {
                        row,
                        generation: id.generation,
                    },
                )
                .is_some()
            {
                return Err(Error::DuplicatePlantId(id_value.to_string()));
            }
            bounds.push(macro_points.bounds[row as usize]);
        }
        let bvh = MacroBvh::build(&bounds)?;
        Ok(Self {
            id,
            manifest_identity,
            resident,
            facets,
            macro_points,
            slots,
            bvh,
            family_tags,
            disturbance_masks,
        })
    }

    /// Complete generation identity.
    #[must_use]
    pub const fn id(&self) -> VegetationCellGenerationId {
        self.id
    }

    /// Exact immutable base manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> ContentHash {
        self.manifest_identity
    }

    /// Logical facets present in this complete generation.
    #[must_use]
    pub const fn resident_facets(&self) -> ResidencyMask {
        self.resident
    }

    /// Effective macro SoA after persistent deltas.
    #[must_use]
    pub fn macro_points(&self) -> &PlantPointColumns {
        &self.macro_points
    }

    /// Quantized micro tiles when a facet requiring them is resident.
    pub fn micro_fields(&self) -> Option<&[MicroFieldTile]> {
        match self.facets.get(&VegetationCellSectionKind::MicroFields) {
            Some(VegetationCellFacet::MicroFields(tiles)) => Some(tiles),
            _ => None,
        }
    }

    /// Persistent disturbance masks overlaid on quantized micro fields.
    #[must_use]
    pub fn disturbance_masks(&self) -> &BTreeMap<DisturbanceTileKey, Vec<i16>> {
        &self.disturbance_masks
    }

    /// Per-plant navigation contribution rows when the navigation facet is resident.
    pub fn navigation_contributions(&self) -> Option<&[crate::VegetationNavigationContribution]> {
        match self
            .facets
            .get(&VegetationCellSectionKind::NavigationContributions)
        {
            Some(VegetationCellFacet::NavigationContributions(rows)) => Some(rows),
            _ => None,
        }
    }

    /// Per-plant collision derivation rows when the physics facet is resident.
    pub fn collision_inputs(&self) -> Option<&[crate::VegetationCollisionInput]> {
        match self.facets.get(&VegetationCellSectionKind::CollisionInputs) {
            Some(VegetationCellFacet::CollisionInputs(rows)) => Some(rows),
            _ => None,
        }
    }

    /// Editor provenance when the editing facet is resident.
    pub fn provenance(&self) -> Option<&ProvenanceTable> {
        match self.facets.get(&VegetationCellSectionKind::Provenance) {
            Some(VegetationCellFacet::Provenance(table)) => Some(table),
            _ => None,
        }
    }

    /// Requires one validated typed artifact facet for renderer, physics, nav, or tooling adapters.
    pub fn require_facet(&self, kind: VegetationCellSectionKind) -> Result<&VegetationCellFacet> {
        self.facets.get(&kind).ok_or(Error::FacetNotResident {
            cell: self.id.cell,
            facet: section_name(kind),
        })
    }

    fn point(&self, slot: PlantSlot) -> Result<PlantPoint> {
        if slot.generation != self.id.generation {
            return Err(Error::StaleGeneration {
                cell: self.id.cell,
                expected: slot.generation,
                current: self.id.generation,
            });
        }
        self.macro_points.point(slot.row as usize)
    }

    pub(super) fn snapshot(&self, slot: PlantSlot) -> Result<VegetationPlantSnapshot> {
        let point = self.point(slot)?;
        let tags = self
            .family_tags
            .get(&point.family.value())
            .cloned()
            .unwrap_or_default();
        let provenance = self
            .provenance()
            .and_then(|table| table.get(ProvenanceHandle(point.provenance)))
            .cloned();
        Ok(VegetationPlantSnapshot {
            plant: point.id,
            ecology_tick: point.ecology_tick,
            handle: VegetationPlantHandle {
                plant: point.id,
                generation: self.id,
            },
            position: point.position,
            orientation: point.orientation,
            scale: point.scale,
            bounds: point.bounds,
            family: point.family,
            tags,
            lifecycle: point.lifecycle,
            variation: point.variation,
            phenotype: point.phenotype,
            interaction_policy: point.interaction_policy,
            health: point.health,
            moisture: point.moisture,
            fuel: point.fuel,
            ignited: point.flags.contains(PlantFlags::IGNITED),
            provenance,
        })
    }

    pub(super) fn matching_snapshot(
        &self,
        row: u32,
        filter: &VegetationQueryFilter,
    ) -> Result<Option<VegetationPlantSnapshot>> {
        let point = self.macro_points.point(row as usize)?;
        let tags = self
            .family_tags
            .get(&point.family.value())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if !filter.matches(&point, tags) {
            return Ok(None);
        }
        self.snapshot(PlantSlot {
            row,
            generation: self.id.generation,
        })
        .map(Some)
    }
}

pub(super) fn required_sections(facets: ResidencyMask) -> BTreeSet<VegetationCellSectionKind> {
    let mut result = BTreeSet::new();
    for facet in facets.iter() {
        result.extend(match facet {
            ResidencyFacet::Render => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::MicroFields,
                VegetationCellSectionKind::RenderReferences,
                VegetationCellSectionKind::RenderBounds,
            ][..],
            ResidencyFacet::Physics => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::CollisionInputs,
            ],
            ResidencyFacet::Simulation => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::MicroFields,
                VegetationCellSectionKind::EcologyBoundary,
                VegetationCellSectionKind::EcologyCheckpoint,
            ],
            ResidencyFacet::Editing => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::Provenance,
                VegetationCellSectionKind::RejectionDiagnostics,
                VegetationCellSectionKind::SurfaceAttachments,
                VegetationCellSectionKind::SurfaceDependencies,
            ],
            ResidencyFacet::Navigation => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::NavigationContributions,
            ],
            ResidencyFacet::Network => &[
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::EcologyBoundary,
                VegetationCellSectionKind::EcologyCheckpoint,
            ],
        });
    }
    result
}

fn section_name(kind: VegetationCellSectionKind) -> &'static str {
    match kind {
        VegetationCellSectionKind::MacroPoints => "macro-points",
        VegetationCellSectionKind::MicroFields => "micro-fields",
        VegetationCellSectionKind::Provenance => "provenance",
        VegetationCellSectionKind::RejectionDiagnostics => "rejection-diagnostics",
        VegetationCellSectionKind::SurfaceAttachments => "surface-attachments",
        VegetationCellSectionKind::SurfaceDependencies => "surface-dependencies",
        VegetationCellSectionKind::RenderReferences => "render-references",
        VegetationCellSectionKind::RenderBounds => "render-bounds",
        VegetationCellSectionKind::CollisionInputs => "collision-inputs",
        VegetationCellSectionKind::NavigationContributions => "navigation-contributions",
        VegetationCellSectionKind::EcologyBoundary => "ecology-boundary",
        VegetationCellSectionKind::EcologyCheckpoint => "ecology-checkpoint",
    }
}

pub(super) fn macro_columns(
    facets: &BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
) -> Result<&PlantPointColumns> {
    macro_columns_optional(facets).ok_or_else(|| Error::ArtifactFormat {
        format: ".svegcell",
        field: "macro points are required by every logical runtime facet".to_owned(),
    })
}

pub(super) fn macro_columns_optional(
    facets: &BTreeMap<VegetationCellSectionKind, VegetationCellFacet>,
) -> Option<&PlantPointColumns> {
    match facets.get(&VegetationCellSectionKind::MacroPoints) {
        Some(VegetationCellFacet::MacroPoints(columns)) => Some(columns),
        _ => None,
    }
}

pub(super) fn effective_macro_points(
    base: &PlantPointColumns,
    state: Option<&VegetationCellState>,
) -> Result<PlantPointColumns> {
    let mut points = BTreeMap::new();
    for row in 0..base.row_count()? {
        let point = base.point(row)?;
        points.insert(point.id, point);
    }
    if let Some(state) = state {
        for (plant, delta) in &state.plants {
            if let Some(addition) = &delta.addition
                && points.insert(*plant, addition.clone()).is_some()
            {
                return Err(Error::DuplicatePlantId(plant.to_string()));
            }
            if delta.tombstoned {
                points.remove(plant);
                continue;
            }
            let Some(point) = points.get_mut(plant) else {
                continue;
            };
            if let Some((position, orientation, scale)) = delta.transform {
                let previous = point.position.global_ticks();
                let next = position.global_ticks();
                let offset = std::array::from_fn(|axis| next[axis] - previous[axis]);
                point.position = position;
                point.orientation = orientation;
                point.scale = scale;
                point.bounds = translate_bounds(point.bounds, offset)?;
                point.flags = point.flags.union(PlantFlags::TRANSFORM_OVERRIDE);
            }
            if let Some(lifecycle) = delta.lifecycle {
                point.lifecycle = lifecycle;
            }
            if let Some(phenotype) = delta.phenotype {
                point.phenotype = phenotype;
            }
            if let Some(ecology_tick) = delta.ecology_tick {
                point.ecology_tick = ecology_tick;
            }
            if let Some(health) = delta.health {
                point.health = health;
            }
            if let Some(moisture) = delta.moisture {
                point.moisture = moisture;
            }
            if let Some(fuel) = delta.fuel {
                point.fuel = fuel;
            }
            if let Some(interaction_policy) = delta.interaction_policy {
                point.interaction_policy = interaction_policy;
            }
            point.flags = if delta.ignited {
                point.flags.union(PlantFlags::IGNITED)
            } else {
                point.flags.difference(PlantFlags::IGNITED)
            };
            if delta.lifecycle.is_some()
                || delta.phenotype.is_some()
                || delta.ecology_tick.is_some()
                || delta.health.is_some()
                || delta.moisture.is_some()
                || delta.fuel.is_some()
                || delta.interaction_policy.is_some()
            {
                point.flags = point.flags.union(PlantFlags::STATE_OVERRIDE);
            }
        }
    }
    PlantPointColumns::from_points(points.into_values().collect())
}

fn translate_bounds(bounds: WorldBounds, offset: [i128; 3]) -> Result<WorldBounds> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    WorldBounds::new(
        [
            minimum[0]
                .checked_add(offset[0])
                .ok_or(Error::NumericOverflow)?,
            minimum[1]
                .checked_add(offset[1])
                .ok_or(Error::NumericOverflow)?,
            minimum[2]
                .checked_add(offset[2])
                .ok_or(Error::NumericOverflow)?,
        ],
        [
            maximum[0]
                .checked_add(offset[0])
                .ok_or(Error::NumericOverflow)?,
            maximum[1]
                .checked_add(offset[1])
                .ok_or(Error::NumericOverflow)?,
            maximum[2]
                .checked_add(offset[2])
                .ok_or(Error::NumericOverflow)?,
        ],
    )
    .map_err(Into::into)
}
