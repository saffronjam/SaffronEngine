//! Immutable runtime cell generations, facet residency, and vegetation queries.

mod bvh;
mod events;
mod generation;
mod load;
mod network;
mod query;
mod residency;
mod simulation;
mod state;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use saffron_spatial::{GenerationSlot, ResidencyManager, WorldCellKey};

use crate::{
    ContentHash, Error, PlantId, PlantTagId, Result, VegetationBaseManifest,
    VegetationManifestCell, VegetationMutationRecord, VegetationState,
};

pub use bvh::VegetationQueryCost;
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
    /// Highest transport sequence accepted from a network envelope; zero before the first one, so
    /// a stream numbers from one and a retransmission never reduces twice.
    network_sequence: u64,
    /// Cells and facets this world is seated with, present only while it is in a network
    /// session. Absent is not the same as empty: a world outside a session accepts mutations for
    /// any cell, whereas a seated one refuses anything outside its declaration.
    network_interest: Option<crate::CellInterestSet>,
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
    /// The authority that last claimed a plant's *simulation* ownership — the one that moved it,
    /// returned promoted state for it, restored its whole delta, or removed it. Biological writes
    /// (damage, harvest, weather) claim nothing, so a script damaging a promoted plant does not
    /// take the plant away from the view simulating it.
    plant_authority: BTreeMap<PlantId, u128>,
    /// Bumped whenever persistent state is replaced wholesale — a save load, a snapshot import, or
    /// a network join. Every plant's ownership is then somebody else's answer, so a local view
    /// standing over one has to yield rather than write back into a world it never observed.
    authority_epoch: u64,
    /// Traversal work the query surface has performed since the last drain. Atomic because every
    /// query takes `&self` and the render adapter may ask from another thread.
    query_cost: QueryCostCounters,
}

#[derive(Debug, Default)]
struct QueryCostCounters {
    queries: AtomicU64,
    hits: AtomicU64,
    generations_visited: AtomicU64,
    nodes_visited: AtomicU64,
    rows_tested: AtomicU64,
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
            network_sequence: 0,
            network_interest: None,
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
            plant_authority: BTreeMap::new(),
            authority_epoch: 0,
            query_cost: QueryCostCounters::default(),
        })
    }

    /// Drains the traversal work the CPU query surface performed since the last call.
    pub fn take_query_cost(&self) -> VegetationQueryCost {
        let take = |counter: &AtomicU64| counter.swap(0, Ordering::Relaxed);
        VegetationQueryCost {
            queries: take(&self.query_cost.queries),
            hits: take(&self.query_cost.hits),
            generations_visited: take(&self.query_cost.generations_visited),
            nodes_visited: take(&self.query_cost.nodes_visited),
            rows_tested: take(&self.query_cost.rows_tested),
        }
    }

    fn record_query_cost(&self, cost: VegetationQueryCost) {
        let add = |counter: &AtomicU64, value: u64| {
            counter.fetch_add(value, Ordering::Relaxed);
        };
        add(&self.query_cost.queries, cost.queries);
        add(&self.query_cost.hits, cost.hits);
        add(
            &self.query_cost.generations_visited,
            cost.generations_visited,
        );
        add(&self.query_cost.nodes_visited, cost.nodes_visited);
        add(&self.query_cost.rows_tested, cost.rows_tested);
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
