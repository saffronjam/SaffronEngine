//! Advancing biological time: dependency-region catch-up over the resident cells.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_spatial::{UnitInterval, WorldBounds, WorldCellKey};

use crate::ecology_region::CellSpatialIndex;
use crate::{
    ContentHash, EcologyCatchUp, EcologyCatchUpReport, EcologyInfluence, EcologyPlantState,
    EcologyRegion, EcologyRegionState, EcologyRegionTick, EcologyRelation, EcologyRelations,
    EcologySpeciesRules, EcologyTickRules, Error, MutationHeader, Result, VegetationMutationRecord,
    advance_region, dependency_regions,
};

use super::VegetationWorld;

/// Authority stamped on every mutation the ecology simulation commits.
const ECOLOGY_AUTHORITY: u128 = 0x5361_6666_726f_6e5f_4563_6f6c_6f67_7901;

/// Stack a region-tick worker runs on. A tick allocates per-cell plant rows and per-plant
/// mutations, so it wants more than the default thread stack of a small platform.
const ECOLOGY_WORKER_STACK_BYTES: usize = 4 * 1024 * 1024;

/// The world's dependency regions and which cell each belongs to.
///
/// Its cost is a closure over every planted cell in the world, and it only changes when the planted
/// set does — a cell streaming in or out redraws no region — so it is shared by `Arc` across the
/// residency snapshots taken above it.
pub(super) struct EcologyRegionClosure {
    planted_revision: u64,
    radius: u32,
    regions: Vec<EcologyRegion>,
    region_of: BTreeMap<WorldCellKey, usize>,
}

/// Where one dependency region stands: the cells it spans, the tick they share, and whether every
/// one of them is resident, which a tick requires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcologyRegionStanding {
    /// The cells that advance together, in canonical order.
    pub region: EcologyRegion,
    /// The tick every cell of the region stands at.
    pub tick: u64,
    /// Whether every cell it spans carries resident macro rows, so a tick may run.
    pub resident: bool,
}

/// The dependency regions plus which of them can currently run: the partition every ecology reader
/// shares for one revision of the ground.
pub(super) struct EcologyRegionPartition {
    pub(super) closure: Arc<EcologyRegionClosure>,
    ground_revision: u64,
    pub(super) resident: Vec<bool>,
}

impl EcologyRegionPartition {
    /// The world's dependency regions, in canonical order.
    fn regions(&self) -> &[EcologyRegion] {
        &self.closure.regions
    }

    /// The region index `cell` belongs to, absent for a cell that carries no plants.
    pub(super) fn region_of(&self, cell: WorldCellKey) -> Option<usize> {
        self.closure.region_of.get(&cell).copied()
    }
}

impl VegetationWorld {
    /// Advances biological time to `plan.target_tick` and catches the world's dependency regions
    /// up to it.
    ///
    /// World time moves first and unconditionally: biology has aged whether or not anything is
    /// loaded. Regions then execute the ticks they owe one round at a time — every region that
    /// owes a tick computes its next one, then the results commit in canonical region order — and a
    /// region only runs while every cell it spans is resident, because a region reading a neighbour
    /// that is not loaded would read stale ground and diverge from continuous simulation. Whatever
    /// the budget leaves undone stays owed, in order, for the next call; what residency leaves undone
    /// is reported separately, because no budget can spend it.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] with no worker, and propagates the tick rules and the reducer. Fails
    /// when a region's cells disagree about which tick they have reached — that means state was
    /// assembled from mismatched checkpoints.
    pub fn advance_ecology(&mut self, plan: &EcologyCatchUp<'_>) -> Result<EcologyCatchUpReport> {
        let workers = usize::try_from(plan.budget.workers).map_err(|_| Error::NumericOverflow)?;
        if workers == 0 {
            return Err(Error::Mutation(
                "ecology catch-up needs at least one worker".to_owned(),
            ));
        }
        self.persistent
            .ecology_mut()
            .advance_world_to(plan.target_tick)?;
        self.effective
            .ecology_mut()
            .advance_world_to(plan.target_tick)?;

        // One partition for the whole call: nothing inside it loads or unloads a cell, and a commit
        // that plants into empty ground redraws the regions for the next call rather than this one.
        let partition = self.ecology_partition(plan.influence);
        let radius = plan.influence.region_radius_cells();
        let mut ticks_run = 0_u64;
        let mut spread = 0_u32;
        let mut remaining = plan.budget.max_ticks;
        while remaining > 0 {
            let mut due = Vec::new();
            for (index, region) in partition.regions().iter().enumerate() {
                if !partition.resident[index] {
                    continue;
                }
                let reached = self.region_tick(region)?;
                if reached < plan.target_tick {
                    due.push((region, reached + 1));
                }
            }
            due.truncate(remaining as usize);
            if due.is_empty() {
                break;
            }
            let (results, used) = self.compute_region_ticks(&due, radius, plan, workers)?;
            spread = spread.max(used);
            for ((region, tick), result) in due.iter().zip(results) {
                self.commit_region_tick(region, *tick, result)?;
                ticks_run += 1;
            }
            remaining -= u32::try_from(due.len()).map_err(|_| Error::NumericOverflow)?;
        }

        let mut report = EcologyCatchUpReport {
            world_tick: plan.target_tick,
            regions: partition.regions().len(),
            ticks_run,
            workers: spread,
            ..EcologyCatchUpReport::default()
        };
        for (index, region) in partition.regions().iter().enumerate() {
            let reached = self.region_tick(region)?;
            let owed = plan.target_tick.saturating_sub(reached);
            if partition.resident[index] {
                report.ticks_owed += owed;
                if owed == 0 {
                    report.regions_caught_up += 1;
                }
            } else {
                report.ticks_awaiting_residency += owed;
                report.regions_awaiting_residency += 1;
            }
        }
        Ok(report)
    }

    /// Whether `cell` publishes a committed tick generation the physics and navigation facets may
    /// derive from.
    ///
    /// A cell whose region is resident and behind world time is mid-catch-up: its lifecycle state
    /// changes every executed tick, so a facet deriving from it would both show biology from a
    /// moment nobody was meant to observe and rebuild the whole cell once per tick. A cell whose
    /// region cannot run at all — some cell it spans is not resident — is *not* mid-catch-up: its
    /// last committed generation is the newest one that will exist until that ground loads, so it
    /// publishes. Gating that case on world time instead would delete collision and navigation for
    /// every loaded cell whose planted neighbours sit outside the streaming window.
    #[must_use]
    pub fn simulation_facet_is_settled(
        &self,
        cell: WorldCellKey,
        influence: EcologyInfluence,
    ) -> bool {
        let ecology = self.persistent.ecology();
        if ecology.cell_tick(cell) == ecology.clock().tick() {
            return true;
        }
        let partition = self.ecology_partition(influence);
        partition
            .region_of(cell)
            .is_none_or(|index| !partition.resident[index])
    }

    /// The dependency-region partition for `influence`.
    ///
    /// A cell streaming in or out only invalidates the residency half, which is a walk of the
    /// resident cells; the region closure behind it is a walk of every planted cell in the world and
    /// is carried forward until the planted set itself changes.
    fn ecology_partition(&self, influence: EcologyInfluence) -> Arc<EcologyRegionPartition> {
        let radius = influence.region_radius_cells();
        let cached = self
            .ecology_partition
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let reusable = cached.as_ref().filter(|partition| {
            partition.closure.radius == radius
                && partition.closure.planted_revision == self.ecology_planted_revision
        });
        let closure = match reusable {
            Some(current) if current.ground_revision == self.ecology_ground_revision => {
                return Arc::clone(current);
            }
            Some(current) => Arc::clone(&current.closure),
            None => Arc::new(self.build_region_closure(radius)),
        };
        let partition = Arc::new(EcologyRegionPartition {
            resident: self.region_residency(&closure),
            closure,
            ground_revision: self.ecology_ground_revision,
        });
        *self
            .ecology_partition
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&partition));
        partition
    }

    /// The transitive closure over every cell the cook planted plus every cell the simulation has
    /// planted into since: a seed that crossed a border into a cell the cook left empty is still a
    /// plant that ages.
    fn build_region_closure(&self, radius: u32) -> EcologyRegionClosure {
        let planted: BTreeSet<WorldCellKey> = self
            .manifest_cells
            .iter()
            .filter(|(_, cell)| cell.macro_count > 0)
            .map(|(key, _)| *key)
            .chain(self.persistent.cells().keys().copied())
            .collect();
        let regions = dependency_regions(&planted, radius);
        EcologyRegionClosure {
            planted_revision: self.ecology_planted_revision,
            radius,
            region_of: regions
                .iter()
                .enumerate()
                .flat_map(|(index, region)| region.cells().iter().map(move |cell| (*cell, index)))
                .collect(),
            regions,
        }
    }

    /// Which regions carry resident macro rows in every cell they span, counted from the resident
    /// cells rather than from the planted ones: the resident set is bounded by the streaming window
    /// and the planted set is the whole world.
    fn region_residency(&self, closure: &EcologyRegionClosure) -> Vec<bool> {
        let mut counts = vec![0_usize; closure.regions.len()];
        for cell in self.cells.keys() {
            if let Some(index) = closure.region_of.get(cell)
                && self.cell_has_resident_macro(*cell)
            {
                counts[*index] += 1;
            }
        }
        closure
            .regions
            .iter()
            .zip(counts)
            .map(|(region, resident)| resident == region.len())
            .collect()
    }

    /// Computes one tick for each due region, spread across `workers` threads, and reports how many
    /// threads it actually used.
    ///
    /// Every region reads immutable published state and returns an owned result, so the thread that
    /// produced a result cannot reach the answer: the results come back in the order the regions
    /// were given, which is canonical, and the caller commits them in that order.
    fn compute_region_ticks(
        &self,
        due: &[(&EcologyRegion, u64)],
        radius: u32,
        plan: &EcologyCatchUp<'_>,
        workers: usize,
    ) -> Result<(Vec<EcologyRegionTick>, u32)> {
        let halo = CellSpatialIndex::build(
            self.persistent.ecology().summaries().keys().copied(),
            radius,
        );
        let rules = EcologyTickRules {
            map: leading_u128(self.manifest_identity.bytes()),
            influence: plan.influence,
            rules: plan.rules,
            relations: plan.relations,
            weather: plan.weather,
        };
        let run = |(region, tick): &(&EcologyRegion, u64)| -> Result<EcologyRegionTick> {
            let state = self.region_state(region, &halo, radius)?;
            advance_region(region, &state, *tick, &rules)
        };

        let workers = workers.min(due.len());
        let spread = u32::try_from(workers).map_err(|_| Error::NumericOverflow)?;
        if workers <= 1 {
            return Ok((due.iter().map(run).collect::<Result<Vec<_>>>()?, 1));
        }
        let shards: Vec<Vec<(usize, &(&EcologyRegion, u64))>> = (0..workers)
            .map(|worker| {
                due.iter()
                    .enumerate()
                    .filter(|(index, _)| index % workers == worker)
                    .collect()
            })
            .collect();
        let run = &run;
        let mut computed =
            std::thread::scope(|scope| -> Result<Vec<(usize, EcologyRegionTick)>> {
                let mut handles = Vec::with_capacity(workers);
                for (worker, shard) in shards.into_iter().enumerate() {
                    handles.push(
                        std::thread::Builder::new()
                            .name(format!("vegetation-ecology-{worker}"))
                            .stack_size(ECOLOGY_WORKER_STACK_BYTES)
                            .spawn_scoped(
                                scope,
                                move || -> Result<Vec<(usize, EcologyRegionTick)>> {
                                    shard
                                        .into_iter()
                                        .map(|(index, entry)| run(entry).map(|tick| (index, tick)))
                                        .collect()
                                },
                            )
                            .map_err(|source| Error::GraphWorkerSpawn { source })?,
                    );
                }
                let mut joined = Vec::with_capacity(due.len());
                for handle in handles {
                    joined.extend(handle.join().map_err(|_| Error::GraphWorkerPanicked)??);
                }
                Ok(joined)
            })?;
        computed.sort_unstable_by_key(|(index, _)| *index);
        Ok((computed.into_iter().map(|(_, tick)| tick).collect(), spread))
    }

    /// Ecology rules per family, as the cook baked them into the manifest.
    ///
    /// A tick reads its species' rules from here rather than from the asset catalog: the manifest
    /// is the immutable thing the state is bound to, so the rules cannot drift from the state that
    /// was simulated under them.
    #[must_use]
    pub fn ecology_rules(&self) -> BTreeMap<u64, EcologySpeciesRules> {
        self.manifest
            .plants
            .iter()
            .map(|plant| (plant.family.value(), plant.ecology.rules))
            .collect()
    }

    /// Declared species relations, keyed by (subject family, other family).
    #[must_use]
    pub fn ecology_relations(&self) -> EcologyRelations {
        self.manifest
            .plants
            .iter()
            .flat_map(|plant| {
                plant.ecology.relations.iter().map(|relation| {
                    (
                        (plant.family.value(), relation.family.value()),
                        EcologyRelation {
                            kind: relation.kind,
                            strength: relation.strength,
                        },
                    )
                })
            })
            .collect()
    }

    /// Where every dependency region under `influence` stands, in canonical order.
    ///
    /// The one reader for a region's cells, tick, and residency, so an inspector and a catch-up
    /// cannot disagree about whether a region can run.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when a region's cells disagree about which tick they have reached, which
    /// means state was assembled from mismatched checkpoints.
    pub fn ecology_region_standings(
        &self,
        influence: EcologyInfluence,
    ) -> Result<Vec<EcologyRegionStanding>> {
        let partition = self.ecology_partition(influence);
        partition
            .regions()
            .iter()
            .enumerate()
            .map(|(index, region)| {
                Ok(EcologyRegionStanding {
                    tick: self.region_tick(region)?,
                    resident: partition.resident[index],
                    region: region.clone(),
                })
            })
            .collect()
    }

    /// Whether `cell` has a resident generation carrying macro rows to simulate.
    fn cell_has_resident_macro(&self, cell: WorldCellKey) -> bool {
        self.cells
            .get(&cell)
            .is_some_and(|entry| !entry.slot.read().macro_points.ids.is_empty())
    }

    /// The tick a region has been simulated to. Its cells advance together, so a disagreement is a
    /// corrupt checkpoint rather than something to paper over with a minimum.
    fn region_tick(&self, region: &EcologyRegion) -> Result<u64> {
        let ecology = self.persistent.ecology();
        let mut ticks = region.cells().iter().map(|cell| ecology.cell_tick(*cell));
        let first = ticks.next().unwrap_or_default();
        for tick in ticks {
            if tick != first {
                return Err(Error::Mutation(format!(
                    "dependency region spans cells at ticks {first} and {tick}; its cells must \
                     advance together"
                )));
            }
        }
        Ok(first)
    }

    /// Commits one region's computed tick: mutations through the one reducer, then the region's
    /// summaries published atomically.
    fn commit_region_tick(
        &mut self,
        region: &EcologyRegion,
        tick: u64,
        result: EcologyRegionTick,
    ) -> Result<()> {
        // Check the publication precondition before any mutation is committed, so the commit and
        // the tick generation cannot come apart: a refused tick leaves neither behind.
        let start = self.region_tick(region)?;
        if tick != start + 1 || tick > self.persistent.ecology().clock().tick() {
            return Err(Error::Mutation(format!(
                "ecology tick {tick} does not follow the region's tick {start} within world time \
                 {}",
                self.persistent.ecology().clock().tick()
            )));
        }

        let mut digest = b"saffron-anima/vegetation-ecology/transaction/v1".to_vec();
        digest.extend_from_slice(&tick.to_be_bytes());
        digest.extend_from_slice(&self.manifest_identity.bytes());
        for (cell, mutations) in &result.mutations {
            for coordinate in cell.coordinates() {
                digest.extend_from_slice(&coordinate.to_be_bytes());
            }
            digest.extend_from_slice(&(mutations.len() as u64).to_be_bytes());
        }
        let transaction = leading_u128(ContentHash::of(&digest).bytes());

        let mut records = Vec::new();
        let mut salt = 0_u128;
        for (cell, mutations) in result.mutations {
            for mutation in mutations {
                salt += 1;
                // A spread seed is owned by the cell it landed in, which is not always the cell
                // whose tick produced it.
                let cell = match &mutation {
                    crate::VegetationMutation::Planting(point)
                    | crate::VegetationMutation::AnchorAddition(point) => point.owner,
                    _ => cell,
                };
                records.push(VegetationMutationRecord {
                    header: MutationHeader {
                        cell,
                        transaction,
                        authority: ECOLOGY_AUTHORITY,
                        logical_tick: tick,
                        idempotency_key: transaction ^ (salt << 8),
                        base_revision: None,
                    },
                    mutation,
                });
            }
        }
        if !records.is_empty() {
            self.apply_confirmed_mutations(&records)?;
        }
        self.persistent
            .ecology_mut()
            .publish_region_tick(tick, &result.summaries)?;
        self.effective
            .ecology_mut()
            .publish_region_tick(tick, &result.summaries)
    }

    /// Reads a region's plants and boundary summaries out of the resident generations.
    ///
    /// Only the region and the halo `radius` reaches around it are collected: a tick filters the
    /// summaries by that radius anyway, so carrying the whole world's summaries in would cost every
    /// cell in the world per tick and change nothing about the answer.
    fn region_state(
        &self,
        region: &EcologyRegion,
        halo: &CellSpatialIndex,
        radius: u32,
    ) -> Result<EcologyRegionState> {
        let published = self.persistent.ecology().summaries();
        let mut summaries = BTreeMap::new();
        for &cell in region.cells() {
            for neighbour in std::iter::once(cell).chain(halo.within(cell, radius)) {
                if let Some(summary) = published.get(&neighbour) {
                    summaries.insert(neighbour, summary.clone());
                }
            }
        }
        let mut state = EcologyRegionState {
            summaries,
            ..EcologyRegionState::default()
        };
        for &cell in region.cells() {
            let Some(entry) = self.cells.get(&cell) else {
                return Err(Error::UnknownRuntimeCell { cell });
            };
            let generation = entry.slot.read();
            let rows = generation.macro_points.row_count()?;
            let mut plants = Vec::with_capacity(rows);
            for row in 0..rows {
                let point = generation.macro_points.point(row)?;
                plants.push(EcologyPlantState {
                    plant: point.id,
                    family: point.family,
                    position: point.position,
                    lifecycle: point.lifecycle,
                    ecology_tick: point.ecology_tick,
                    health: point.health,
                    moisture: point.moisture,
                    fuel: point.fuel,
                    canopy: canopy_share(point.bounds, cell),
                });
            }
            plants.sort_unstable_by_key(|plant| plant.plant);
            state.plants.insert(cell, plants);
        }
        Ok(state)
    }
}

/// The leading 16 bytes of a content hash, as the transaction and map identities want a `u128`.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading)
}

/// A plant's share of its cell's ground, from the conservative bounds the cook produced: shade is
/// an area effect, so the shade a cell casts is the sum of its plants' footprints against the
/// cell's own footprint. Integer ticks throughout, because a canopy figure feeds simulated results
/// and may not vary with floating-point rounding.
pub(super) fn canopy_share(plant: WorldBounds, cell: WorldCellKey) -> UnitInterval {
    let footprint = |bounds: WorldBounds| -> i128 {
        let minimum = bounds.min_ticks();
        let maximum = bounds.max_ticks_exclusive();
        (maximum[0] - minimum[0]).max(0) * (maximum[2] - minimum[2]).max(0)
    };
    let ground = footprint(cell.bounds());
    if ground <= 0 {
        return UnitInterval::ZERO;
    }
    let share = footprint(plant) * i128::from(UnitInterval::ONE.bits()) / ground;
    UnitInterval::from_bits(u16::try_from(share.clamp(0, i128::from(u16::MAX))).unwrap_or(u16::MAX))
}
