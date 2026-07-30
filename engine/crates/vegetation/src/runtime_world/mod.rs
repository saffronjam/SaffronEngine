//! Immutable runtime cell generations, facet residency, and vegetation queries.

mod bvh;
mod events;
mod generation;
mod load;
mod query;
mod residency;
mod simulation;
mod state;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use saffron_spatial::{GenerationSlot, ResidencyManager, WorldCellKey};

use crate::{
    ContentHash, Error, PlantId, PlantTagId, Result, VegetationBaseManifest,
    VegetationManifestCell, VegetationMutationRecord, VegetationState,
};

pub use events::{VEGETATION_EVENT_RING_CAP, VegetationEvent, VegetationEventDrain};
pub use generation::{
    VegetationCellGeneration, VegetationCellGenerationId, VegetationPlantHandle,
    VegetationPlantSnapshot,
};
pub use load::{StagedVegetationCellGeneration, VegetationCellLoad};
pub use query::{
    VegetationCombustionSample, VegetationMicroHit, VegetationNearestHit, VegetationQueryFilter,
    VegetationQueryRay, VegetationRayHit,
};
pub use residency::{
    VegetationCellLoadRequest, VegetationResidencyBudgets, VegetationResidencyReport,
};

pub use simulation::EcologyRegionStanding;

use simulation::EcologyRegionPartition;

struct RuntimeCell {
    slot: Arc<GenerationSlot<VegetationCellGeneration>>,
    /// Monotonic counter bumped whenever a plant this cell owns enters or leaves bulk
    /// suppression, so render and collision adapters re-derive the cell.
    bulk_revision: u64,
}

/// The sole authoritative owner of runtime vegetation cell generations and persistent deltas.
pub struct VegetationWorld {
    manifest: VegetationBaseManifest,
    manifest_identity: ContentHash,
    manifest_cells: BTreeMap<WorldCellKey, VegetationManifestCell>,
    family_tags: Arc<BTreeMap<u64, Vec<PlantTagId>>>,
    persistent: VegetationState,
    effective: VegetationState,
    predictions: BTreeMap<u128, Vec<VegetationMutationRecord>>,
    residency: ResidencyManager,
    budgets: VegetationResidencyBudgets,
    residency_revision: u64,
    /// Monotonic counter bumped whenever the ground an ecology tick reads changes: a cell
    /// generation published or unloaded, or persistent state that planted into a cell the cook
    /// left empty. Distinct from `residency_revision`, which is the staging token guard and would
    /// discard in-flight cell loads if it moved on a publication.
    ecology_ground_revision: u64,
    /// Monotonic counter bumped whenever the set of cells carrying plants changes, which is the only
    /// thing that redraws the dependency-region partition. Streaming moves the ground revision
    /// without moving this one.
    ecology_planted_revision: u64,
    /// Memoized dependency-region partition, keyed by the revisions and radius it was built at, so
    /// neither the per-frame catch-up poll nor the per-cell facet readiness reader pays a closure
    /// over every planted cell in the world.
    ecology_partition: std::sync::RwLock<Option<Arc<EcologyRegionPartition>>>,
    cells: BTreeMap<WorldCellKey, RuntimeCell>,
    event_ring: VecDeque<VegetationEvent>,
    event_seq: u64,
    /// Plants whose bulk representation is suppressed because a promoted entity owns them.
    /// Keyed by identity rather than held in an immutable published generation, so a cell
    /// unload, republication, or reload never loses or duplicates the suppression.
    bulk_suppressed: BTreeSet<PlantId>,
}

impl VegetationWorld {
    /// Binds a new runtime world to one exact, validated immutable manifest.
    pub fn new(
        manifest: VegetationBaseManifest,
        budgets: VegetationResidencyBudgets,
    ) -> Result<Self> {
        let canonical = manifest.canonical_bytes()?;
        let manifest_identity = ContentHash::of(&canonical);
        let manifest_cells = manifest
            .cells
            .iter()
            .cloned()
            .map(|cell| (cell.cell, cell))
            .collect();
        let family_tags = Arc::new(
            manifest
                .plants
                .iter()
                .map(|plant| (plant.family.value(), plant.tags.clone()))
                .collect(),
        );
        let persistent = VegetationState::new(manifest_identity.bytes());
        Ok(Self {
            effective: persistent.clone(),
            persistent,
            predictions: BTreeMap::new(),
            manifest,
            manifest_identity,
            manifest_cells,
            family_tags,
            residency: ResidencyManager::new(),
            budgets,
            residency_revision: 0,
            ecology_ground_revision: 0,
            ecology_planted_revision: 0,
            ecology_partition: std::sync::RwLock::new(None),
            cells: BTreeMap::new(),
            event_ring: VecDeque::new(),
            event_seq: 0,
            bulk_suppressed: BTreeSet::new(),
        })
    }

    /// Bound immutable manifest.
    #[must_use]
    pub fn manifest(&self) -> &VegetationBaseManifest {
        &self.manifest
    }

    /// Exact bound manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> ContentHash {
        self.manifest_identity
    }

    /// Revision of the ground an ecology tick reads: cell residency and the set of cells carrying
    /// plants. A catch-up that found nothing to run stays a no-op until this moves, so a caller
    /// polling for ecology work compares it against the revision its last catch-up observed.
    #[must_use]
    pub const fn ecology_ground_revision(&self) -> u64 {
        self.ecology_ground_revision
    }

    fn bump_ecology_ground_revision(&mut self) {
        self.ecology_ground_revision = self.ecology_ground_revision.wrapping_add(1);
    }

    /// Records that the set of cells carrying plants changed, which redraws the dependency regions
    /// and is therefore also a change to the ground.
    fn bump_ecology_planted_revision(&mut self) {
        self.ecology_planted_revision = self.ecology_planted_revision.wrapping_add(1);
        self.bump_ecology_ground_revision();
    }

    /// Returns the current complete generation while keeping it alive for the reader.
    pub fn cell_snapshot(&self, cell: WorldCellKey) -> Option<Arc<VegetationCellGeneration>> {
        self.cells.get(&cell).map(|entry| entry.slot.read())
    }

    /// Iterates every resident cell's current published generation, keeping each alive
    /// for the reader — the render adapter's snapshot walk.
    pub fn resident_cells(
        &self,
    ) -> impl Iterator<Item = (WorldCellKey, Arc<VegetationCellGeneration>)> + '_ {
        self.cells
            .iter()
            .map(|(cell, entry)| (*cell, entry.slot.read()))
    }

    fn resident_macro_generations(
        &self,
    ) -> impl Iterator<Item = Arc<VegetationCellGeneration>> + '_ {
        self.cells.values().filter_map(|entry| {
            let generation = entry.slot.read();
            (!generation.macro_points.ids.is_empty()).then_some(generation)
        })
    }

    fn ensure_runtime_cell(&mut self, cell: WorldCellKey) -> Result<()> {
        if self.cells.contains_key(&cell) {
            return Ok(());
        }
        let empty = Arc::new(VegetationCellGeneration::empty(
            cell,
            self.manifest_identity,
            self.family_tags.clone(),
        )?);
        self.cells.insert(
            cell,
            RuntimeCell {
                slot: Arc::new(GenerationSlot::new(cell, empty)),
                bulk_revision: 0,
            },
        );
        Ok(())
    }

    fn require_manifest_cell(&self, cell: WorldCellKey) -> Result<&VegetationManifestCell> {
        self.manifest_cells
            .get(&cell)
            .ok_or(Error::UnknownRuntimeCell { cell })
    }
}
