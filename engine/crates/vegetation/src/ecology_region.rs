//! Dependency regions: the connected groups of cells that must be simulated together.
//!
//! Shade, competition, and seed spread cross cell borders, so a cell cannot be caught up alone —
//! its neighbours would still be at the old tick and it would read stale shade. A dependency
//! region is the transitive closure of "close enough to influence each other" under the largest
//! declared influence radius, and a tick advances a whole region or none of it.
//!
//! Publication is atomic per tick. Every cell in the region computes its result from immutable
//! tick-`N` state first, and only once all of them have succeeded is anything committed, in
//! canonical cell order. A cancelled or superseded catch-up therefore cannot leave half a tick
//! generation behind.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use saffron_spatial::WorldCellKey;

use crate::{
    EcologyCellSummary, EcologyPlantState, EcologyRelations, EcologySpeciesRules,
    EcologyTickInputs, EcologyWeather, Error, Result, VegetationMutation, advance_cell,
};

/// How far each cross-cell effect reaches, in cells.
///
/// Declared rather than inferred: a region is only correct if it is at least as wide as the
/// widest influence, so the radii live in one place and the region builder takes their maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologyInfluence {
    /// Canopy shade cast onto neighbours.
    pub shade_cells: u32,
    /// Crown and root competition for the same ground.
    pub competition_cells: u32,
    /// How far a spread seed may land.
    pub propagation_cells: u32,
    /// Ground-water sharing.
    pub moisture_cells: u32,
    /// Trample and clearing spillover.
    pub disturbance_cells: u32,
}

impl Default for EcologyInfluence {
    fn default() -> Self {
        Self {
            shade_cells: 1,
            competition_cells: 1,
            propagation_cells: 1,
            moisture_cells: 1,
            disturbance_cells: 1,
        }
    }
}

impl EcologyInfluence {
    /// The radius a dependency region must use: the widest declared influence, since a region
    /// narrower than any one effect would let that effect read stale neighbours.
    #[must_use]
    pub fn region_radius_cells(self) -> u32 {
        self.shade_cells
            .max(self.competition_cells)
            .max(self.propagation_cells)
            .max(self.moisture_cells)
            .max(self.disturbance_cells)
    }
}

/// One connected group of cells that advances together, in canonical key order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcologyRegion {
    cells: BTreeSet<WorldCellKey>,
}

impl EcologyRegion {
    /// The region's cells in canonical order.
    #[must_use]
    pub const fn cells(&self) -> &BTreeSet<WorldCellKey> {
        &self.cells
    }

    /// How many cells the region spans.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the region holds no cells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// A uniform-grid hash over cell keys, so a radius query costs its own neighbourhood rather than a
/// scan of every cell in the world.
///
/// Buckets are a `BTreeMap` and each bucket's cells stay sorted: a query's result is an input to a
/// tick, so a container whose iteration order varied would let bucket layout reach published bytes.
/// The bucket edge is the query radius, which bounds a query to at most four buckets per axis.
pub(crate) struct CellSpatialIndex {
    edge: i64,
    buckets: BTreeMap<(u8, [i64; 3]), Vec<WorldCellKey>>,
}

impl CellSpatialIndex {
    /// Indexes `cells` for queries at `radius`.
    pub(crate) fn build(cells: impl IntoIterator<Item = WorldCellKey>, radius: u32) -> Self {
        let edge = i64::from(radius).max(1);
        let mut buckets: BTreeMap<(u8, [i64; 3]), Vec<WorldCellKey>> = BTreeMap::new();
        for cell in cells {
            buckets
                .entry((cell.level(), bucket_of(cell.coordinates(), edge)))
                .or_default()
                .push(cell);
        }
        for bucket in buckets.values_mut() {
            bucket.sort_unstable();
        }
        Self { edge, buckets }
    }

    /// The indexed cells within `radius` of `cell` on every axis at the same level, excluding
    /// `cell` itself, in canonical order.
    pub(crate) fn within(&self, cell: WorldCellKey, radius: u32) -> Vec<WorldCellKey> {
        let radius = i64::from(radius);
        let coordinates = cell.coordinates();
        let low = bucket_of(
            coordinates.map(|value| value.saturating_sub(radius)),
            self.edge,
        );
        let high = bucket_of(
            coordinates.map(|value| value.saturating_add(radius)),
            self.edge,
        );
        let mut near = Vec::new();
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    let Some(bucket) = self.buckets.get(&(cell.level(), [x, y, z])) else {
                        continue;
                    };
                    near.extend(bucket.iter().copied().filter(|candidate| {
                        *candidate != cell && within(cell, *candidate, radius)
                    }));
                }
            }
        }
        near.sort_unstable();
        near
    }
}

/// The bucket a cell coordinate triple falls in, floored so negative coordinates bucket like
/// positive ones.
fn bucket_of(coordinates: [i64; 3], edge: i64) -> [i64; 3] {
    coordinates.map(|value| value.div_euclid(edge))
}

/// Groups `cells` into connected dependency regions under `radius`.
///
/// Two cells share a region when they are within `radius` on every axis, and the relation is
/// transitive: a chain of neighbours pulls the whole chain into one region, because advancing any
/// link needs the next one's tick-`N` state.
#[must_use]
pub fn dependency_regions(cells: &BTreeSet<WorldCellKey>, radius: u32) -> Vec<EcologyRegion> {
    let index = CellSpatialIndex::build(cells.iter().copied(), radius);
    let mut remaining: BTreeSet<WorldCellKey> = cells.clone();
    let mut regions = Vec::new();
    // Canonical order in, canonical order out: the region list cannot depend on iteration luck.
    while let Some(&seed) = remaining.iter().next() {
        remaining.remove(&seed);
        let mut region = BTreeSet::from([seed]);
        let mut frontier = VecDeque::from([seed]);
        while let Some(current) = frontier.pop_front() {
            for candidate in index.within(current, radius) {
                if remaining.remove(&candidate) {
                    region.insert(candidate);
                    frontier.push_back(candidate);
                }
            }
        }
        regions.push(EcologyRegion { cells: region });
    }
    regions
}

/// Whether two cells are within `radius` on every axis, at the same hierarchy level.
fn within(left: WorldCellKey, right: WorldCellKey, radius: i64) -> bool {
    left.level() == right.level()
        && left
            .coordinates()
            .iter()
            .zip(right.coordinates())
            .all(|(left, right)| left.abs_diff(right) <= radius.unsigned_abs())
}

/// How much work one catch-up call may do.
///
/// A budget delays readiness; it never drops or reorders a tick. Whatever it does not run stays
/// pending and runs next call, in the same order it would have. `workers` changes only how long the
/// call takes: regions are disjoint and a region tick is a pure function of immutable state, and the
/// results commit in canonical region order however many threads produced them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologyCatchUpBudget {
    /// Region ticks this call may advance.
    pub max_ticks: u32,
    /// Threads the pure per-region rule evaluation may spread across.
    pub workers: u32,
}

impl Default for EcologyCatchUpBudget {
    fn default() -> Self {
        Self {
            max_ticks: 8,
            workers: 1,
        }
    }
}

/// One catch-up call: how far to bring biology, and the rules to bring it with.
#[derive(Clone, Copy, Debug)]
pub struct EcologyCatchUp<'a> {
    /// World biological time to reach.
    pub target_tick: u64,
    /// How many ticks this call may execute.
    pub budget: EcologyCatchUpBudget,
    /// Declared cross-cell influence, which fixes how wide a dependency region is.
    pub influence: EcologyInfluence,
    /// Per-family rules, keyed by family value.
    pub rules: &'a BTreeMap<u64, EcologySpeciesRules>,
    pub relations: &'a EcologyRelations,
    /// Sampled weather for the ticks being run.
    pub weather: EcologyWeather,
}

/// What one catch-up call did, and what it still owes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcologyCatchUpReport {
    /// World biological time after the call.
    pub world_tick: u64,
    /// Dependency regions the world is partitioned into.
    pub regions: usize,
    /// Regions standing at world time, whose simulation facet is readable.
    pub regions_caught_up: usize,
    /// Regions that could not run because a cell they span is not resident.
    pub regions_awaiting_residency: usize,
    /// Ticks executed by this call.
    pub ticks_run: u64,
    /// Ticks owed by resident regions, which a following call runs. This is the actionable
    /// arrears: a caller that polls for outstanding work keys on it, because a region awaiting
    /// residency owes ticks no budget can spend.
    pub ticks_owed: u64,
    /// Ticks owed by regions a cell they span is not resident, which nothing can run until that
    /// ground loads.
    pub ticks_awaiting_residency: u64,
    /// Threads the call spread the pure per-region rule evaluation across: the configured worker
    /// count, bounded by how many regions were ever due in one round.
    pub workers: u32,
}

/// One region's committed result for one tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcologyRegionTick {
    /// The tick these results belong to.
    pub tick: u64,
    /// Per-cell mutations in canonical cell order, for the one reducer.
    pub mutations: Vec<(WorldCellKey, Vec<VegetationMutation>)>,
    /// Per-cell boundary summaries, published together.
    pub summaries: BTreeMap<WorldCellKey, EcologyCellSummary>,
}

/// The state a region tick reads: each cell's plants at tick `N` and the summaries from tick `N`.
#[derive(Clone, Debug, Default)]
pub struct EcologyRegionState {
    /// Plants per cell, each slice in canonical identity order.
    pub plants: BTreeMap<WorldCellKey, Vec<EcologyPlantState>>,
    /// Boundary summaries per cell from the completed tick.
    pub summaries: BTreeMap<WorldCellKey, EcologyCellSummary>,
}

/// The rules and environment one region tick runs under.
#[derive(Clone, Copy, Debug)]
pub struct EcologyTickRules<'a> {
    /// Vegetation-map identity, for random-domain separation.
    pub map: u128,
    /// Declared cross-cell influence, which fixes the halo a cell reads.
    pub influence: EcologyInfluence,
    /// Per-family rules, keyed by family value.
    pub rules: &'a BTreeMap<u64, EcologySpeciesRules>,
    pub relations: &'a EcologyRelations,
    /// Sampled weather for the tick.
    pub weather: EcologyWeather,
}

/// Advances one region by exactly one tick.
///
/// Every cell is computed from immutable tick-`N` state before anything is returned, so a failure
/// anywhere yields no partial generation: the caller either commits the whole tick or nothing.
///
/// # Errors
///
/// Propagates [`advance_cell`], and [`Error::Mutation`] when the region names a cell the state does
/// not carry — a region must be advanced with all of its cells present, or its members would read
/// missing neighbours as empty ground.
pub fn advance_region(
    region: &EcologyRegion,
    state: &EcologyRegionState,
    tick: u64,
    rules: &EcologyTickRules<'_>,
) -> Result<EcologyRegionTick> {
    let radius = rules.influence.region_radius_cells();
    let halo = CellSpatialIndex::build(state.summaries.keys().copied(), radius);
    let mut mutations = Vec::with_capacity(region.len());
    let mut summaries = BTreeMap::new();

    for &cell in region.cells() {
        let Some(plants) = state.plants.get(&cell) else {
            return Err(Error::Mutation(format!(
                "dependency region is missing cell {cell}, so its neighbours would read empty \
                 ground"
            )));
        };
        // The halo: every summary within the influence radius, from the completed tick. Reading
        // them from `state` rather than from this tick's results is what keeps the step
        // double-buffered across cells as well as within one.
        let neighbours: Vec<EcologyCellSummary> = halo
            .within(cell, radius)
            .into_iter()
            .filter_map(|neighbour| state.summaries.get(&neighbour).cloned())
            .collect();
        let output = advance_cell(&EcologyTickInputs {
            cell,
            tick,
            map: rules.map,
            plants,
            neighbours: &neighbours,
            rules: rules.rules,
            relations: rules.relations,
            weather: rules.weather,
        })?;
        mutations.push((cell, output.mutations));
        summaries.insert(cell, output.summary);
    }

    Ok(EcologyRegionTick {
        tick,
        mutations,
        summaries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_core::Uuid;
    use saffron_spatial::{PlantId, UnitInterval, WorldPosition};

    fn cell(x: i64, z: i64) -> WorldCellKey {
        WorldCellKey::base(x, 0, z)
    }

    fn plant_state(byte: u8, cell: WorldCellKey) -> EcologyPlantState {
        let ticks = cell.bounds().min_ticks();
        EcologyPlantState {
            plant: PlantId::explicit([byte; 16]).expect("plant id"),
            family: Uuid(7),
            position: WorldPosition::from_global_ticks([ticks[0] + 1, ticks[1] + 1, ticks[2] + 1])
                .expect("position"),
            lifecycle: crate::PlantLifecycle::Mature,
            ecology_tick: 50,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::from_bits(1_000),
            canopy: UnitInterval::from_bits(5_000),
        }
    }

    fn summary(tick: u64) -> EcologyCellSummary {
        EcologyCellSummary {
            tick,
            plants: 1,
            canopy: UnitInterval::from_bits(5_000),
            roots: UnitInterval::from_bits(3_000),
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::from_bits(1_000),
            families: vec![crate::EcologyFamilyPresence {
                family: 7,
                canopy: UnitInterval::from_bits(5_000),
                health: UnitInterval::ONE,
            }],
        }
    }

    fn region_state(cells: &[WorldCellKey]) -> EcologyRegionState {
        let mut state = EcologyRegionState::default();
        for (index, cell) in cells.iter().enumerate() {
            state
                .plants
                .insert(*cell, vec![plant_state(index as u8 + 1, *cell)]);
            state.summaries.insert(*cell, summary(0));
        }
        state
    }

    /// Cells influence each other transitively: a chain pulls the whole chain into one region,
    /// while a cell beyond the radius of every member forms its own.
    #[test]
    fn regions_are_the_transitive_closure_of_influence() {
        let cells = BTreeSet::from([cell(0, 0), cell(1, 0), cell(2, 0), cell(9, 0)]);
        let regions = dependency_regions(&cells, 1);
        assert_eq!(regions.len(), 2);
        // The chain 0-1-2 is one region even though 0 and 2 are two cells apart.
        assert_eq!(regions[0].len(), 3);
        assert!(regions[0].cells().contains(&cell(2, 0)));
        assert_eq!(regions[1].cells(), &BTreeSet::from([cell(9, 0)]));
    }

    /// A wider influence merges what a narrow one separates, which is why the region radius is the
    /// maximum of the declared radii rather than any single effect's.
    #[test]
    fn the_region_radius_is_the_widest_declared_influence() {
        let influence = EcologyInfluence {
            shade_cells: 1,
            competition_cells: 1,
            propagation_cells: 4,
            moisture_cells: 2,
            disturbance_cells: 1,
        };
        assert_eq!(influence.region_radius_cells(), 4);

        let cells = BTreeSet::from([cell(0, 0), cell(3, 0)]);
        assert_eq!(dependency_regions(&cells, 1).len(), 2);
        assert_eq!(
            dependency_regions(&cells, influence.region_radius_cells()).len(),
            1
        );
    }

    /// A region tick reads only the completed tick's summaries, so advancing a region gives the
    /// same answer however its cells are ordered internally — and it publishes every cell.
    #[test]
    fn a_region_tick_publishes_every_cell_from_the_completed_tick() {
        let cells = [cell(0, 0), cell(1, 0)];
        let state = region_state(&cells);
        let region = dependency_regions(&cells.iter().copied().collect(), 1)
            .pop()
            .expect("one region");
        let rules = BTreeMap::from([(7, EcologySpeciesRules::default())]);
        let weather = EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        };

        let first = advance_region(
            &region,
            &state,
            1,
            &EcologyTickRules {
                map: 0x5eed,
                influence: EcologyInfluence::default(),
                rules: &rules,
                relations: &EcologyRelations::new(),
                weather,
            },
        )
        .unwrap();
        assert_eq!(first.tick, 1);
        assert_eq!(first.summaries.len(), 2);
        assert_eq!(first.mutations.len(), 2);
        // Canonical cell order, so the committed generation is reproducible.
        assert_eq!(first.mutations[0].0, cell(0, 0));
        assert_eq!(first.mutations[1].0, cell(1, 0));

        let again = advance_region(
            &region,
            &state,
            1,
            &EcologyTickRules {
                map: 0x5eed,
                influence: EcologyInfluence::default(),
                rules: &rules,
                relations: &EcologyRelations::new(),
                weather,
            },
        )
        .unwrap();
        assert_eq!(first, again, "a region tick is reproducible");
    }

    /// A region advanced without one of its cells would have its neighbours read empty ground, so
    /// the tick is refused rather than publishing a wrong generation.
    #[test]
    fn a_region_missing_a_cell_is_refused() {
        let cells = [cell(0, 0), cell(1, 0)];
        let region = dependency_regions(&cells.iter().copied().collect(), 1)
            .pop()
            .expect("one region");
        // State for only one of the two cells.
        let state = region_state(&cells[..1]);
        let rules = BTreeMap::from([(7, EcologySpeciesRules::default())]);
        assert!(
            advance_region(
                &region,
                &state,
                1,
                &EcologyTickRules {
                    map: 0x5eed,
                    influence: EcologyInfluence::default(),
                    rules: &rules,
                    relations: &EcologyRelations::new(),
                    weather: EcologyWeather::default(),
                },
            )
            .is_err()
        );
    }

    /// Shade crosses a cell border, and it crosses exactly once: a plant is updated by its own
    /// cell's tick and by no other, and the surrounding cells' canopy is what changes its health.
    #[test]
    fn the_shade_seam_updates_each_plant_once() {
        let clearing = cell(0, 0);
        let forest = [cell(1, 0), cell(-1, 0), cell(0, 1), cell(0, -1)];
        let rules = BTreeMap::from([(7, EcologySpeciesRules::default())]);
        let weather = EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        };

        let health_of = |state: &EcologyRegionState, region: &EcologyRegion| {
            let tick = advance_region(
                region,
                state,
                1,
                &EcologyTickRules {
                    map: 0x5eed,
                    influence: EcologyInfluence::default(),
                    rules: &rules,
                    relations: &EcologyRelations::new(),
                    weather,
                },
            )
            .unwrap();
            // No plant may be touched by two cells' ticks.
            let mut seen = BTreeSet::new();
            for (_, mutations) in &tick.mutations {
                for mutation in mutations {
                    if let crate::VegetationMutation::LifecycleTransition { plant, .. } = mutation {
                        assert!(seen.insert(*plant), "{plant} was updated twice in one tick");
                    }
                }
            }
            tick.mutations
                .iter()
                .filter(|(cell, _)| *cell == clearing)
                .flat_map(|(_, mutations)| mutations)
                .find_map(|mutation| match mutation {
                    crate::VegetationMutation::StateOverride {
                        health: Some(health),
                        ..
                    } => Some(*health),
                    _ => None,
                })
        };

        // Alone, the plant stands in the open and holds full health.
        let open = region_state(&[clearing]);
        let alone = dependency_regions(&BTreeSet::from([clearing]), 1)
            .pop()
            .expect("one region");
        assert_eq!(
            health_of(&open, &alone),
            None,
            "an unshaded plant at full health has nothing to change"
        );

        // Ringed by dense canopy on the other side of every border, the same tick is a loss.
        let mut cells = vec![clearing];
        cells.extend_from_slice(&forest);
        let mut seam = region_state(&cells);
        for neighbour in forest {
            seam.summaries.insert(
                neighbour,
                EcologyCellSummary {
                    plants: 40,
                    canopy: UnitInterval::ONE,
                    ..summary(0)
                },
            );
        }
        let region = dependency_regions(&cells.iter().copied().collect(), 1)
            .pop()
            .expect("one region");
        assert_eq!(region.len(), 5);
        let under_shade = health_of(&seam, &region).expect("shade changed the plant's health");
        assert!(
            under_shade.bits() < UnitInterval::ONE.bits(),
            "the neighbours' canopy reached across the border: {}",
            under_shade.bits()
        );
    }

    /// Disjoint regions cannot see each other, so the order they are advanced in — and, with it,
    /// how many workers pick them up — cannot change the state that gets committed.
    #[test]
    fn disjoint_regions_commit_the_same_state_in_either_order() {
        let cells = [cell(0, 0), cell(1, 0), cell(40, 0), cell(41, 0)];
        let state = region_state(&cells);
        let regions = dependency_regions(&cells.iter().copied().collect(), 1);
        assert_eq!(regions.len(), 2);
        let rules = BTreeMap::from([(7, EcologySpeciesRules::default())]);
        let weather = EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        };

        let publish = |order: [&EcologyRegion; 2]| {
            let mut published = crate::EcologyState::new();
            published.advance_world_to(1).unwrap();
            for region in order {
                let tick = advance_region(
                    region,
                    &state,
                    1,
                    &EcologyTickRules {
                        map: 0x5eed,
                        influence: EcologyInfluence::default(),
                        rules: &rules,
                        relations: &EcologyRelations::new(),
                        weather,
                    },
                )
                .unwrap();
                published.publish_region_tick(1, &tick.summaries).unwrap();
            }
            published.checkpoint_identity()
        };

        assert_eq!(
            publish([&regions[0], &regions[1]]),
            publish([&regions[1], &regions[0]])
        );
    }

    /// The grid index answers exactly what a full scan answers. It is what turns region building
    /// and halo lookup from a walk of every cell in the world into a walk of a neighbourhood, so a
    /// disagreement would silently change which neighbours a tick reads.
    #[test]
    fn the_grid_index_answers_exactly_what_a_scan_answers() {
        let mut cells = BTreeSet::new();
        for x in [-97_i64, -8, -1, 0, 1, 2, 7, 40, 41, 1_000_000] {
            for z in [-13_i64, 0, 3, 4, 900] {
                cells.insert(cell(x, z));
            }
        }
        // A different level never neighbours a base cell, whatever the coordinates say.
        cells.insert(WorldCellKey::new(0, 0, 0, 2).expect("level-two cell"));

        for radius in [1_u32, 2, 5] {
            let index = CellSpatialIndex::build(cells.iter().copied(), radius);
            for &subject in &cells {
                let scanned: Vec<WorldCellKey> = cells
                    .iter()
                    .copied()
                    .filter(|candidate| {
                        *candidate != subject && within(subject, *candidate, i64::from(radius))
                    })
                    .collect();
                assert_eq!(
                    index.within(subject, radius),
                    scanned,
                    "radius {radius} around {subject}"
                );
            }
        }
    }

    /// The deterministic half of a tick is translation-invariant: the same forest advanced at the
    /// origin and a million cells away reaches the same health, moisture, fuel, and boundary
    /// summary. A rule that folded a cell coordinate or a cell-local offset into that arithmetic
    /// would separate the two.
    ///
    /// The stochastic channels are deliberately excluded — their random domain carries the owner
    /// cell, so two identical forests in different places are not clones of each other — so the
    /// rules here declare zero chance and the test compares what must not vary.
    #[test]
    fn the_deterministic_tick_is_translation_invariant() {
        let rules = BTreeMap::from([(
            7,
            EcologySpeciesRules {
                propagation_chance: UnitInterval::ZERO,
                deadfall_chance: UnitInterval::ZERO,
                regrowth_chance: UnitInterval::ZERO,
                ..EcologySpeciesRules::default()
            },
        )]);
        let weather = EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        };
        let outcome = |origin: i64| {
            let cells = [cell(origin, 0), cell(origin + 1, 0)];
            let state = region_state(&cells);
            let region = dependency_regions(&cells.iter().copied().collect(), 1)
                .pop()
                .expect("one region");
            let result = advance_region(
                &region,
                &state,
                1,
                &EcologyTickRules {
                    map: 0x5eed,
                    influence: EcologyInfluence::default(),
                    rules: &rules,
                    relations: &EcologyRelations::new(),
                    weather,
                },
            )
            .expect("the region ticks wherever it sits");
            let summaries: Vec<EcologyCellSummary> = result.summaries.into_values().collect();
            let mutations: Vec<Vec<VegetationMutation>> = result
                .mutations
                .into_iter()
                .map(|(_, mutations)| mutations)
                .collect();
            (summaries, mutations)
        };

        let (near_summaries, near_mutations) = outcome(0);
        let (far_summaries, far_mutations) = outcome(1_000_000);
        assert!(
            near_mutations.iter().any(|cell| !cell.is_empty()),
            "the tick changed something, so the comparison has something to compare",
        );
        assert_eq!(near_summaries, far_summaries);
        assert_eq!(near_mutations, far_mutations);
    }
}
