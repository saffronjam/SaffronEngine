//! Deterministic multi-source facet residency and generation-safe publication.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::num::NonZeroUsize;
use std::sync::{Arc, RwLock};

use glam::DVec3;

use crate::{Error, Result, WorldCellKey, WorldPosition};

/// Number of independently reference-counted residency facets.
pub const FACET_COUNT: usize = 6;

/// Default maximum number of facet-cell claims one source update may materialize.
pub const DEFAULT_SOURCE_CLAIM_BUDGET: usize = 1 << 20;

/// One independently resident use of a spatial cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ResidencyFacet {
    /// Render data.
    Render = 0,
    /// Physics data.
    Physics = 1,
    /// Simulation data.
    Simulation = 2,
    /// Editor-authoring data.
    Editing = 3,
    /// Navigation contribution data.
    Navigation = 4,
    /// Network-interest data.
    Network = 5,
}

impl ResidencyFacet {
    /// Every facet in canonical order.
    pub const ALL: [Self; FACET_COUNT] = [
        Self::Render,
        Self::Physics,
        Self::Simulation,
        Self::Editing,
        Self::Navigation,
        Self::Network,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

/// A compact set of residency facets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResidencyMask(u8);

impl ResidencyMask {
    /// No facets.
    pub const NONE: Self = Self(0);
    /// Every defined facet.
    pub const ALL: Self = Self((1 << FACET_COUNT) - 1);

    /// A mask containing one facet.
    #[must_use]
    pub const fn one(facet: ResidencyFacet) -> Self {
        Self(1 << facet as u8)
    }

    /// Adds a facet.
    #[must_use]
    pub const fn with(self, facet: ResidencyFacet) -> Self {
        Self(self.0 | (1 << facet as u8))
    }

    /// Whether the facet is present.
    #[must_use]
    pub const fn contains(self, facet: ResidencyFacet) -> bool {
        self.0 & (1 << facet as u8) != 0
    }

    /// The packed facet bits, for hashing a source's demand into its revision.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Facets in canonical order.
    pub fn iter(self) -> impl Iterator<Item = ResidencyFacet> {
        ResidencyFacet::ALL
            .into_iter()
            .filter(move |facet| self.contains(*facet))
    }
}

/// Stable source identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpatialSourceId(pub u64);

/// Load and cleanup radii for one hierarchy level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLevel {
    /// Hierarchy level.
    pub level: u8,
    /// Inclusive load radius in cells.
    pub load_radius_cells: u32,
    /// Inclusive retention radius in cells.
    pub cleanup_radius_cells: u32,
}

/// A predicted source of facet residency demand.
#[derive(Clone, Debug, PartialEq)]
pub struct SpatialSource {
    /// Stable source identity.
    pub id: SpatialSourceId,
    /// Monotonic source revision.
    pub revision: u64,
    /// Exact current position.
    pub position: WorldPosition,
    /// World velocity in metres per second.
    pub velocity_mps: DVec3,
    /// Prediction horizon in seconds.
    pub prediction_seconds: f64,
    /// Per-level load and cleanup radii.
    pub levels: Vec<SourceLevel>,
    /// Requested facets.
    pub facets: ResidencyMask,
    /// Higher values schedule first; output remains identical.
    pub priority: i32,
}

impl SpatialSource {
    fn validate(&self) -> Result<()> {
        if !self.velocity_mps.is_finite()
            || !self.prediction_seconds.is_finite()
            || self.prediction_seconds < 0.0
            || self.levels.is_empty()
            || self.facets == ResidencyMask::NONE
            || self.levels.iter().any(|level| {
                level.cleanup_radius_cells < level.load_radius_cells
                    || WorldCellKey::new(0, 0, 0, level.level).is_err()
            })
        {
            return Err(Error::InvalidSpatialSource);
        }
        let mut levels: Vec<u8> = self.levels.iter().map(|level| level.level).collect();
        levels.sort_unstable();
        levels.dedup();
        if levels.len() != self.levels.len() {
            return Err(Error::InvalidSpatialSource);
        }
        Ok(())
    }

    fn predicted_position(&self) -> Result<WorldPosition> {
        let delta = self.velocity_mps * self.prediction_seconds;
        self.position.offset_meters(delta)
    }
}

/// One cell's resolved reference counts and scheduling priority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidencySnapshot {
    /// Cell identity.
    pub cell: WorldCellKey,
    /// Reference count per [`ResidencyFacet::ALL`] entry.
    pub reference_counts: [u32; FACET_COUNT],
    /// Highest contributing source priority.
    pub priority: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Claim {
    cell: WorldCellKey,
    facet: ResidencyFacet,
}

/// Deterministically resolves multiple predicted sources into per-facet cell reference counts.
pub struct ResidencyManager {
    sources: BTreeMap<SpatialSourceId, SpatialSource>,
    claims: BTreeMap<SpatialSourceId, BTreeSet<Claim>>,
    source_claim_budget: NonZeroUsize,
}

impl Default for ResidencyManager {
    fn default() -> Self {
        Self::with_source_claim_budget(
            NonZeroUsize::new(DEFAULT_SOURCE_CLAIM_BUDGET).expect("default budget is non-zero"),
        )
    }
}

impl ResidencyManager {
    /// Creates an empty manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a manager with an explicit upper bound on materialized facet-cell claims per source.
    #[must_use]
    pub fn with_source_claim_budget(source_claim_budget: NonZeroUsize) -> Self {
        Self {
            sources: BTreeMap::new(),
            claims: BTreeMap::new(),
            source_claim_budget,
        }
    }

    /// Adds or replaces one source, applying load and cleanup hysteresis exactly.
    pub fn update_source(&mut self, source: SpatialSource) -> Result<()> {
        source.validate()?;
        let center = source.predicted_position()?;
        let previous = self.claims.get(&source.id).cloned().unwrap_or_default();
        let mut next = BTreeSet::new();
        for level in &source.levels {
            let center_cell = center.cell().ancestor(level.level)?;
            let load = cells_in_cube(
                center_cell,
                level.load_radius_cells,
                self.source_claim_budget.get(),
            )?;
            let cleanup = cells_in_cube(
                center_cell,
                level.cleanup_radius_cells,
                self.source_claim_budget.get(),
            )?;
            for facet in source.facets.iter() {
                next.extend(load.iter().copied().map(|cell| Claim { cell, facet }));
                next.extend(previous.iter().copied().filter(|claim| {
                    claim.facet == facet
                        && claim.cell.level() == level.level
                        && cleanup.contains(&claim.cell)
                }));
            }
            if next.len() > self.source_claim_budget.get() {
                return Err(Error::ResidencyBudgetExceeded);
            }
        }
        self.claims.insert(source.id, next);
        self.sources.insert(source.id, source);
        self.validate_counts()
    }

    /// Removes a source and all its references.
    pub fn remove_source(&mut self, id: SpatialSourceId) -> bool {
        let source = self.sources.remove(&id).is_some();
        self.claims.remove(&id);
        source
    }

    /// Sources in stable identity order.
    #[must_use]
    pub fn sources(&self) -> Vec<SpatialSource> {
        self.sources.values().cloned().collect()
    }

    /// Resolved non-empty cell snapshots in canonical key order.
    pub fn snapshots(&self) -> Result<Vec<ResidencySnapshot>> {
        let mut result: BTreeMap<WorldCellKey, ResidencySnapshot> = BTreeMap::new();
        for (source_id, claims) in &self.claims {
            let source = &self.sources[source_id];
            for claim in claims {
                let entry = result.entry(claim.cell).or_insert(ResidencySnapshot {
                    cell: claim.cell,
                    reference_counts: [0; FACET_COUNT],
                    priority: i32::MIN,
                });
                entry.reference_counts[claim.facet.index()] = entry.reference_counts
                    [claim.facet.index()]
                .checked_add(1)
                .ok_or(Error::ResidencyOverflow)?;
                entry.priority = entry.priority.max(source.priority);
            }
        }
        Ok(result.into_values().collect())
    }

    fn validate_counts(&self) -> Result<()> {
        let _ = self.snapshots()?;
        Ok(())
    }
}

fn cells_in_cube(
    center: WorldCellKey,
    radius: u32,
    cell_budget: usize,
) -> Result<BTreeSet<WorldCellKey>> {
    let diameter = u128::from(radius)
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or(Error::ResidencyBudgetExceeded)?;
    let cardinality = diameter
        .checked_pow(3)
        .ok_or(Error::ResidencyBudgetExceeded)?;
    if cardinality > cell_budget as u128 {
        return Err(Error::ResidencyBudgetExceeded);
    }
    let radius = i64::from(radius);
    let mut cells = BTreeSet::new();
    for z in -radius..=radius {
        for y in -radius..=radius {
            for x in -radius..=radius {
                cells.insert(center.neighbour([x, y, z])?);
            }
        }
    }
    Ok(cells)
}

/// A generation identity attached to cancelable asynchronous work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationToken {
    /// Target cell.
    pub cell: WorldCellKey,
    /// Source revision that requested the work.
    pub source_revision: u64,
    /// Monotonic cell generation.
    pub generation: u64,
}

struct GenerationState<T> {
    current: GenerationToken,
    published: Arc<T>,
}

/// One cell's atomic whole-value publication slot.
pub struct GenerationSlot<T> {
    cell: WorldCellKey,
    state: RwLock<GenerationState<T>>,
}

impl<T> GenerationSlot<T> {
    /// Creates a slot with its complete initial value.
    #[must_use]
    pub fn new(cell: WorldCellKey, initial: Arc<T>) -> Self {
        Self {
            cell,
            state: RwLock::new(GenerationState {
                current: GenerationToken {
                    cell,
                    source_revision: 0,
                    generation: 0,
                },
                published: initial,
            }),
        }
    }

    /// Begins a newer generation and invalidates every older token.
    pub fn begin(&self, source_revision: u64) -> Result<GenerationToken> {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let generation = state
            .current
            .generation
            .checked_add(1)
            .ok_or(Error::GenerationExhausted)?;
        state.current = GenerationToken {
            cell: self.cell,
            source_revision,
            generation,
        };
        Ok(state.current)
    }

    /// Invalidates the token if it is still current.
    pub fn cancel(&self, token: GenerationToken) -> Result<bool> {
        if token.cell != self.cell {
            return Err(Error::GenerationCellMismatch);
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.current != token {
            return Ok(false);
        }
        state.current.generation = state
            .current
            .generation
            .checked_add(1)
            .ok_or(Error::GenerationExhausted)?;
        Ok(true)
    }

    /// Publishes a complete staged value only when the token remains current.
    pub fn try_publish(&self, token: GenerationToken, staged: Arc<T>) -> Result<bool> {
        if token.cell != self.cell {
            return Err(Error::GenerationCellMismatch);
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.current != token {
            return Ok(false);
        }
        state.published = staged;
        Ok(true)
    }

    /// Clones the complete published generation. Readers never observe partial replacement.
    #[must_use]
    pub fn read(&self) -> Arc<T> {
        Arc::clone(
            &self
                .state
                .read()
                .unwrap_or_else(|poison| poison.into_inner())
                .published,
        )
    }

    /// Whether a token remains current.
    #[must_use]
    pub fn is_current(&self, token: GenerationToken) -> bool {
        token.cell == self.cell
            && self
                .state
                .read()
                .unwrap_or_else(|poison| poison.into_inner())
                .current
                == token
    }
}

/// A total deterministic priority key. Higher priority pops first; every tie uses stable identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobPriorityKey {
    /// Source priority.
    pub priority: i32,
    /// Target cell.
    pub cell: WorldCellKey,
    /// Target facet.
    pub facet: ResidencyFacet,
    /// Generation.
    pub generation: u64,
    /// Stable logical job identity assigned by the producer.
    pub job_id: u128,
}

impl Ord for JobPriorityKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.cell.cmp(&self.cell))
            .then_with(|| other.facet.cmp(&self.facet))
            .then_with(|| other.generation.cmp(&self.generation))
            .then_with(|| other.job_id.cmp(&self.job_id))
    }
}

impl PartialOrd for JobPriorityKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct JobEntry<T> {
    key: JobPriorityKey,
    payload: T,
}

impl<T> PartialEq for JobEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<T> Eq for JobEntry<T> {}

impl<T> Ord for JobEntry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl<T> PartialOrd for JobEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A deterministic job queue whose insertion/worker order cannot change priority order.
pub struct SpatialJobQueue<T> {
    heap: BinaryHeap<JobEntry<T>>,
}

impl<T> Default for SpatialJobQueue<T> {
    fn default() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }
}

impl<T> SpatialJobQueue<T> {
    /// Pushes a job with a complete total key.
    pub fn push(&mut self, key: JobPriorityKey, payload: T) {
        self.heap.push(JobEntry { key, payload });
    }

    /// Pops the next job.
    pub fn pop(&mut self) -> Option<(JobPriorityKey, T)> {
        self.heap.pop().map(|entry| (entry.key, entry.payload))
    }

    /// Number of queued jobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: u64, position: WorldPosition) -> SpatialSource {
        SpatialSource {
            id: SpatialSourceId(id),
            revision: 1,
            position,
            velocity_mps: DVec3::ZERO,
            prediction_seconds: 0.0,
            levels: vec![SourceLevel {
                level: 0,
                load_radius_cells: 0,
                cleanup_radius_cells: 1,
            }],
            facets: ResidencyMask::one(ResidencyFacet::Render),
            priority: id as i32,
        }
    }

    #[test]
    fn source_order_cannot_change_resolved_bytes() {
        let origin = WorldPosition::origin();
        let moved =
            WorldPosition::from_render_relative(glam::Vec3::new(64.0, 0.0, 0.0), origin).unwrap();
        let mut a = ResidencyManager::new();
        a.update_source(source(1, origin)).unwrap();
        a.update_source(source(2, moved)).unwrap();
        let mut b = ResidencyManager::new();
        b.update_source(source(2, moved)).unwrap();
        b.update_source(source(1, origin)).unwrap();
        assert_eq!(a.snapshots().unwrap(), b.snapshots().unwrap());
    }

    #[test]
    fn cleanup_radius_retains_then_releases_cells() {
        let origin = WorldPosition::origin();
        let mut manager = ResidencyManager::new();
        manager.update_source(source(1, origin)).unwrap();
        let one_cell =
            WorldPosition::from_render_relative(glam::Vec3::new(64.0, 0.0, 0.0), origin).unwrap();
        manager.update_source(source(1, one_cell)).unwrap();
        assert_eq!(manager.snapshots().unwrap().len(), 2);
        let three_cells =
            WorldPosition::from_render_relative(glam::Vec3::new(192.0, 0.0, 0.0), origin).unwrap();
        manager.update_source(source(1, three_cells)).unwrap();
        assert_eq!(manager.snapshots().unwrap().len(), 1);
    }

    #[test]
    fn late_generation_is_rejected_without_partial_publication() {
        let slot = GenerationSlot::new(WorldCellKey::base(0, 0, 0), Arc::new(vec![1, 2, 3]));
        let old = slot.begin(10).unwrap();
        let current = slot.begin(11).unwrap();
        assert!(!slot.try_publish(old, Arc::new(vec![4])).unwrap());
        assert_eq!(&*slot.read(), &[1, 2, 3]);
        assert!(slot.try_publish(current, Arc::new(vec![5, 6])).unwrap());
        assert_eq!(&*slot.read(), &[5, 6]);
    }

    #[test]
    fn canceled_generation_never_publishes() {
        let slot = GenerationSlot::new(WorldCellKey::base(2, -3, 5), Arc::new(vec![1, 2]));
        let token = slot.begin(4).unwrap();
        assert!(slot.cancel(token).unwrap());
        assert!(!slot.try_publish(token, Arc::new(vec![9, 9])).unwrap());
        assert_eq!(&*slot.read(), &[1, 2]);
    }

    #[test]
    fn readers_observe_only_complete_generations() {
        let slot = Arc::new(GenerationSlot::new(
            WorldCellKey::base(0, 0, 0),
            Arc::new(vec![1_u8; 4096]),
        ));
        let token = slot.begin(1).unwrap();
        let writer = Arc::clone(&slot);
        let handle = std::thread::spawn(move || {
            writer
                .try_publish(token, Arc::new(vec![2_u8; 4096]))
                .unwrap()
        });
        for _ in 0..4096 {
            let value = slot.read();
            assert!(value.iter().all(|byte| *byte == 1) || value.iter().all(|byte| *byte == 2));
        }
        assert!(handle.join().unwrap());
        assert!(slot.read().iter().all(|byte| *byte == 2));
    }

    #[test]
    fn generation_identity_never_wraps() {
        let slot = GenerationSlot::new(WorldCellKey::base(0, 0, 0), Arc::new(0_u8));
        slot.state.write().unwrap().current.generation = u64::MAX;
        assert_eq!(slot.begin(1), Err(Error::GenerationExhausted));
    }

    #[test]
    fn source_claim_budget_rejects_unbounded_materialization() {
        let mut manager = ResidencyManager::with_source_claim_budget(NonZeroUsize::new(8).unwrap());
        let mut value = source(1, WorldPosition::origin());
        value.levels[0].load_radius_cells = 1;
        value.levels[0].cleanup_radius_cells = 1;
        assert_eq!(
            manager.update_source(value),
            Err(Error::ResidencyBudgetExceeded)
        );
        assert!(manager.sources().is_empty());
        assert!(manager.snapshots().unwrap().is_empty());
    }

    #[test]
    fn shuffled_queue_insertion_has_one_pop_order() {
        let cell = WorldCellKey::base(0, 0, 0);
        let keys = [
            JobPriorityKey {
                priority: 1,
                cell,
                facet: ResidencyFacet::Render,
                generation: 1,
                job_id: 9,
            },
            JobPriorityKey {
                priority: 2,
                cell,
                facet: ResidencyFacet::Render,
                generation: 1,
                job_id: 7,
            },
            JobPriorityKey {
                priority: 2,
                cell,
                facet: ResidencyFacet::Render,
                generation: 1,
                job_id: 3,
            },
        ];
        let mut first = SpatialJobQueue::default();
        let mut second = SpatialJobQueue::default();
        for key in keys {
            first.push(key, key.job_id);
        }
        for key in keys.into_iter().rev() {
            second.push(key, key.job_id);
        }
        let drain = |queue: &mut SpatialJobQueue<u128>| {
            std::iter::from_fn(|| queue.pop().map(|(_, payload)| payload)).collect::<Vec<_>>()
        };
        assert_eq!(drain(&mut first), drain(&mut second));
        assert_eq!(drain(&mut SpatialJobQueue::default()), Vec::<u128>::new());
    }

    #[test]
    fn worker_count_cannot_change_canonical_result_bytes() {
        let cell = WorldCellKey::base(-4, 7, 2);
        let keys: Vec<JobPriorityKey> = (0_u128..128)
            .map(|job_id| JobPriorityKey {
                priority: (job_id % 7) as i32,
                cell,
                facet: ResidencyFacet::Simulation,
                generation: 3,
                job_id,
            })
            .collect();
        let evaluate = |workers: usize| {
            let mut queue = SpatialJobQueue::default();
            for key in keys.iter().rev() {
                queue.push(*key, key.job_id.to_be_bytes());
            }
            let mut outputs = vec![Vec::<[u8; 16]>::new(); workers];
            let mut worker = 0;
            while let Some((_, bytes)) = queue.pop() {
                outputs[worker].push(bytes);
                worker = (worker + 1) % workers;
            }
            let mut merged: Vec<[u8; 16]> = outputs.into_iter().flatten().collect();
            merged.sort_unstable();
            merged
        };
        assert_eq!(evaluate(1), evaluate(2));
        assert_eq!(evaluate(1), evaluate(17));
    }
}
