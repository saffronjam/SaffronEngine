//! Vegetation's navigation contribution seam.
//!
//! Navigation is not implemented here and never will be. This module publishes only what vegetation
//! *contributes* to whatever consumes it: cell-addressed obstacle and traversal-cost payloads, plus
//! the world bounds that changed since the consumer last looked. There is no foliage-private navmesh,
//! no tile builder, and no pathfinding.
//!
//! Each plant declares exactly one contribution, derived from its interaction policy and its family's
//! navigation proxies. A promoted plant contributes a *dynamic* obstacle, because it is moving and a
//! static tile rebuild would be stale before it finished.
//!
//! A cell mid-catch-up publishes nothing. Its lifecycle state changes every tick, and a consumer
//! that rebuilt a tile per intermediate tick would spend the whole catch-up rebuilding ground the
//! player never saw. A cell whose region cannot run at all keeps publishing its last committed
//! generation, because that is the newest one there will be until the ground it depends on loads.

use std::collections::BTreeMap;

use glam::DVec3;
use saffron_assets::AssetServer;
use saffron_core::Uuid;
use saffron_spatial::{PlantId, ResidencyFacet, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    EcologyInfluence, InteractionPolicy, PlantNavigationProxy, VegetationNavigationContribution,
    VegetationWorld,
};

use crate::vegetation_family::PlantFamilyCache;

/// What one plant contributes to navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationContributionKind {
    /// A traversal-cost field: passable, at a multiplier.
    Cost,
    /// An immovable simplified obstacle.
    StaticObstacle,
    /// An obstacle that is moving, so a consumer treats it as dynamic until it settles.
    DynamicObstacle,
}

/// One plant's published navigation contribution.
#[derive(Clone, Debug, PartialEq)]
pub struct NavigationContribution {
    /// The contributing plant.
    pub plant: PlantId,
    /// Which of the four declarations this is (the fourth, no effect, publishes nothing).
    pub kind: NavigationContributionKind,
    /// Conservative world bounds the contribution occupies.
    pub bounds: WorldBounds,
    /// World-space footprint polygon in metres, X/Z pairs in authored order.
    pub footprint: Vec<[f64; 2]>,
    /// Obstacle height in metres.
    pub height_m: f64,
    /// Traversal-cost multiplier in 0..1, where one is neutral.
    pub cost: f64,
}

/// Aggregate navigation-seam counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VegetationNavigationReport {
    /// Cells currently publishing contributions.
    pub resident_cells: usize,
    /// Published contributions across every resident cell.
    pub contributions: usize,
    /// Contributions that are obstacles rather than cost fields.
    pub obstacles: usize,
    /// Obstacles published as dynamic because a promoted entity owns the plant.
    pub dynamic_obstacles: usize,
    /// World regions changed since the consumer last drained them.
    pub pending_dirty_regions: usize,
}

struct NavCell {
    generation: u64,
    bulk_revision: u64,
    /// Whether each contributing plant was derived as promoted, in canonical identity order. A
    /// bulk-revision move re-derives exactly the plants whose bit changed — a promotion changes one
    /// plant's declaration, and dirtying the rest would ask a consumer to rebuild ground nothing
    /// moved on.
    promoted: BTreeMap<PlantId, bool>,
    /// Every published row, in canonical plant order.
    contributions: Vec<NavigationContribution>,
}

impl NavCell {
    /// Re-derives the plants whose bulk suppression moved and returns the world bounds they cover.
    fn resuppress(
        &mut self,
        rows: &[VegetationNavigationContribution],
        bulk_revision: u64,
        suppressed: &dyn Fn(PlantId) -> bool,
        proxies: &mut dyn FnMut(Uuid) -> Option<Vec<PlantNavigationProxy>>,
    ) -> Vec<WorldBounds> {
        self.bulk_revision = bulk_revision;
        let mut dirty = Vec::new();
        for row in rows {
            let promoted = suppressed(row.plant);
            if self.promoted.get(&row.plant) == Some(&promoted) {
                continue;
            }
            self.promoted.insert(row.plant, promoted);
            let Some(proxies) = proxies(row.family) else {
                continue;
            };
            let derived = derive_contributions(row, promoted, &proxies);
            dirty.extend(
                self.contributions
                    .iter()
                    .filter(|existing| existing.plant == row.plant)
                    .map(|existing| existing.bounds),
            );
            self.contributions
                .retain(|existing| existing.plant != row.plant);
            dirty.extend(derived.iter().map(|contribution| contribution.bounds));
            self.contributions.extend(derived);
        }
        // A stable sort by identity restores canonical order and keeps each plant's proxies in the
        // order its family authored them.
        self.contributions
            .sort_by_key(|contribution| contribution.plant);
        dirty
    }
}

/// Publishes vegetation's navigation contributions per resident cell and accumulates the dirty
/// world bounds a consumer must rebuild.
#[derive(Default)]
pub struct VegetationNavigationSeam {
    cells: BTreeMap<WorldCellKey, NavCell>,
    dirty: Vec<WorldBounds>,
    report: VegetationNavigationReport,
}

impl VegetationNavigationSeam {
    /// Reconciles every navigation-resident cell against the world, marking the affected bounds
    /// dirty. Runs at the same fixed synchronization point as collision residency, so a consumer
    /// never reads a half-updated cell.
    ///
    /// A republished generation retires the whole cell: every row it carried may have moved. A
    /// promotion or demotion moves only the cell's bulk-suppression revision, and then only the
    /// plants whose suppression actually flipped are re-derived — promoting one tree must not ask
    /// a consumer to rebuild the tiles under every other plant in the cell.
    pub(crate) fn advance(
        &mut self,
        vegetation: &VegetationWorld,
        assets: &AssetServer,
        families: &mut PlantFamilyCache,
        influence: EcologyInfluence,
    ) {
        let mut desired = BTreeMap::new();
        for (cell, generation) in vegetation.resident_cells() {
            if generation
                .resident_facets()
                .contains(ResidencyFacet::Navigation)
                && vegetation.simulation_facet_is_settled(cell, influence)
            {
                desired.insert(cell, generation);
            }
        }

        let stale: Vec<WorldCellKey> = self
            .cells
            .iter()
            .filter(|(cell, entry)| {
                desired
                    .get(*cell)
                    .map(|generation| generation.id().generation)
                    != Some(entry.generation)
            })
            .map(|(cell, _)| *cell)
            .collect();
        for cell in stale {
            let entry = self.cells.remove(&cell).expect("stale key was snapshotted");
            // What a retired contribution covered has to be rebuilt whether or not the cell
            // returns, so its bounds are dirty either way.
            self.mark_dirty(entry.contributions.iter().map(|row| row.bounds));
        }

        for (cell, generation) in desired {
            let bulk_revision = vegetation.cell_bulk_revision(cell);
            let rows = generation.navigation_contributions().unwrap_or(&[]);
            let mut resolve = |family: Uuid| {
                families
                    .get(family, assets)
                    .map(|family| family.navigation_proxies.clone())
            };
            let dirty = match self.cells.get_mut(&cell) {
                Some(entry) if entry.bulk_revision == bulk_revision => continue,
                Some(entry) => entry.resuppress(
                    rows,
                    bulk_revision,
                    &|plant| vegetation.is_bulk_suppressed(plant),
                    &mut resolve,
                ),
                None => {
                    let mut promoted = BTreeMap::new();
                    let mut contributions = Vec::new();
                    for row in rows {
                        let suppressed = vegetation.is_bulk_suppressed(row.plant);
                        promoted.insert(row.plant, suppressed);
                        if let Some(proxies) = resolve(row.family) {
                            contributions.extend(derive_contributions(row, suppressed, &proxies));
                        }
                    }
                    let dirty = contributions.iter().map(|row| row.bounds).collect();
                    self.cells.insert(
                        cell,
                        NavCell {
                            generation: generation.id().generation,
                            bulk_revision,
                            promoted,
                            contributions,
                        },
                    );
                    dirty
                }
            };
            self.mark_dirty(dirty.into_iter());
        }

        self.refresh_counts();
    }

    /// Every published contribution, cell by cell in canonical order.
    pub fn cells(&self) -> impl Iterator<Item = (WorldCellKey, &[NavigationContribution])> + '_ {
        self.cells
            .iter()
            .map(|(cell, entry)| (*cell, entry.contributions.as_slice()))
    }

    /// The world regions changed since the last drain, coalesced into non-overlapping-enough
    /// boxes for a tile rebuild. Draining clears them: one consumer owns the rebuild.
    pub fn take_dirty_regions(&mut self) -> Vec<WorldBounds> {
        let regions = std::mem::take(&mut self.dirty);
        self.report.pending_dirty_regions = 0;
        regions
    }

    /// The world regions changed since the last drain, without consuming them.
    pub fn dirty_regions(&self) -> &[WorldBounds] {
        &self.dirty
    }

    /// The current aggregate counters.
    #[must_use]
    pub fn report(&self) -> VegetationNavigationReport {
        self.report
    }

    /// Drops every published contribution, marking what they covered dirty — the vegetation
    /// authority went away, so a consumer must rebuild those regions without them.
    pub(crate) fn clear(&mut self) {
        for (_, entry) in std::mem::take(&mut self.cells) {
            self.dirty
                .extend(entry.contributions.iter().map(|row| row.bounds));
        }
        self.refresh_counts();
    }

    fn mark_dirty(&mut self, bounds: impl Iterator<Item = WorldBounds>) {
        for region in bounds {
            // Coalesce into an existing region it touches rather than growing an unbounded list.
            match self
                .dirty
                .iter_mut()
                .find(|existing| overlaps(**existing, region))
            {
                Some(existing) => *existing = existing.union(region),
                None => self.dirty.push(region),
            }
        }
    }

    fn refresh_counts(&mut self) {
        self.report.resident_cells = self.cells.len();
        self.report.contributions = self
            .cells
            .values()
            .map(|entry| entry.contributions.len())
            .sum();
        self.report.obstacles = self
            .cells
            .values()
            .flat_map(|entry| &entry.contributions)
            .filter(|row| row.kind != NavigationContributionKind::Cost)
            .count();
        self.report.dynamic_obstacles = self
            .cells
            .values()
            .flat_map(|entry| &entry.contributions)
            .filter(|row| row.kind == NavigationContributionKind::DynamicObstacle)
            .count();
        self.report.pending_dirty_regions = self.dirty.len();
    }
}

/// Whether two half-open boxes share any volume.
fn overlaps(left: WorldBounds, right: WorldBounds) -> bool {
    (0..3).all(|axis| {
        left.min_ticks()[axis] < right.max_ticks_exclusive()[axis]
            && right.min_ticks()[axis] < left.max_ticks_exclusive()[axis]
    })
}

/// The contribution a plant's policy and family proxies declare. A plant with no navigation proxy,
/// or a decorative one, contributes nothing at all — the fourth declaration.
fn derive_contributions(
    row: &VegetationNavigationContribution,
    promoted: bool,
    proxies: &[PlantNavigationProxy],
) -> Vec<NavigationContribution> {
    let kind = match row.interaction_policy {
        InteractionPolicy::Decorative => return Vec::new(),
        InteractionPolicy::Interactive => NavigationContributionKind::Cost,
        // A promoted plant is moving under physics, so a static tile rebuild would be stale before
        // it finished; the consumer gets a dynamic obstacle until the plant settles and demotes.
        InteractionPolicy::Structural | InteractionPolicy::Harvestable if promoted => {
            NavigationContributionKind::DynamicObstacle
        }
        InteractionPolicy::Structural | InteractionPolicy::Harvestable => {
            NavigationContributionKind::StaticObstacle
        }
    };
    let tick = 1.0 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
    let origin = DVec3::new(
        row.bounds.min_ticks()[0] as f64 * tick,
        row.bounds.min_ticks()[1] as f64 * tick,
        row.bounds.min_ticks()[2] as f64 * tick,
    );
    proxies
        .iter()
        .map(|proxy| NavigationContribution {
            plant: row.plant,
            kind,
            bounds: row.bounds,
            footprint: proxy
                .footprint
                .iter()
                .map(|point| [origin.x + point[0].to_f64(), origin.z + point[1].to_f64()])
                .collect(),
            height_m: proxy.height.to_f64(),
            cost: proxy.cost.to_f64(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_core::Uuid;
    use saffron_spatial::{DecisionScalar, UnitInterval};
    use saffron_vegetation::PlantLifecycle;

    fn scalar(value: f64) -> DecisionScalar {
        DecisionScalar::from_f64(value).expect("finite decision scalar")
    }

    fn bounds(min: i128) -> WorldBounds {
        WorldBounds::new([min, 0, min], [min + 1024, 2048, min + 1024]).expect("bounds")
    }

    fn plant(byte: u8) -> PlantId {
        PlantId::explicit([byte; 16]).expect("plant id")
    }

    fn row(policy: InteractionPolicy, min: i128) -> VegetationNavigationContribution {
        row_for(plant(31), policy, min)
    }

    fn row_for(
        plant: PlantId,
        policy: InteractionPolicy,
        min: i128,
    ) -> VegetationNavigationContribution {
        VegetationNavigationContribution {
            plant,
            family: Uuid(9),
            bounds: bounds(min),
            interaction_policy: policy,
            lifecycle: PlantLifecycle::Mature,
        }
    }

    /// Builds the cell entry the seam's own build path produces, for the tests that then move one
    /// plant's suppression.
    fn nav_cell(rows: &[VegetationNavigationContribution]) -> NavCell {
        let mut cell = NavCell {
            generation: 1,
            bulk_revision: 0,
            promoted: rows.iter().map(|row| (row.plant, false)).collect(),
            contributions: Vec::new(),
        };
        for row in rows {
            cell.contributions
                .extend(derive_contributions(row, false, &[proxy()]));
        }
        cell
    }

    fn proxy() -> PlantNavigationProxy {
        PlantNavigationProxy {
            id: 1,
            footprint: vec![
                [scalar(0.0), scalar(0.0)],
                [scalar(0.5), scalar(0.0)],
                [scalar(0.5), scalar(0.5)],
            ],
            height: scalar(4.0),
            cost: UnitInterval::from_bits(30_000),
        }
    }

    #[test]
    fn policies_declare_one_contribution_each() {
        // Decorative plants contribute nothing — grass never becomes a nav object.
        assert!(
            derive_contributions(&row(InteractionPolicy::Decorative, 0), false, &[proxy()])
                .is_empty()
        );
        // A plant with no authored nav proxy also contributes nothing.
        assert!(
            derive_contributions(&row(InteractionPolicy::Structural, 0), false, &[]).is_empty()
        );

        let cost = derive_contributions(&row(InteractionPolicy::Interactive, 0), false, &[proxy()]);
        assert_eq!(cost[0].kind, NavigationContributionKind::Cost);
        assert!((cost[0].height_m - 4.0).abs() < 1e-6);
        assert!(cost[0].cost > 0.0 && cost[0].cost < 1.0);

        let solid = derive_contributions(&row(InteractionPolicy::Structural, 0), false, &[proxy()]);
        assert_eq!(solid[0].kind, NavigationContributionKind::StaticObstacle);
        // The same plant while promoted is dynamic: it is moving, so a static rebuild would lie.
        let promoted =
            derive_contributions(&row(InteractionPolicy::Structural, 0), true, &[proxy()]);
        assert_eq!(
            promoted[0].kind,
            NavigationContributionKind::DynamicObstacle
        );
    }

    /// Promoting one plant dirties one plant's ground. The cell's other contributions keep the rows
    /// they already published, and the ground under them is never handed back for a rebuild — a
    /// whole-cell re-publication would coalesce into a region spanning every plant in the cell.
    #[test]
    fn one_promotion_dirties_only_that_plant() {
        let rows = [
            row_for(plant(1), InteractionPolicy::Structural, 0),
            row_for(plant(2), InteractionPolicy::Structural, 8_192),
            row_for(plant(3), InteractionPolicy::Structural, 16_384),
        ];
        let mut cell = nav_cell(&rows);
        let before = cell.contributions.clone();

        let promoted = plant(2);
        let dirty = cell.resuppress(&rows, 1, &|id| id == promoted, &mut |_| Some(vec![proxy()]));

        // Only the promoted plant's own bounds came back, and they came back once for what it
        // covered and once for what it now covers — both the same box.
        assert!(
            dirty.iter().all(|region| *region == bounds(8_192)),
            "the promotion dirtied ground no plant moved on: {dirty:?}"
        );
        assert!(
            !dirty.is_empty(),
            "the promoted plant's ground must rebuild"
        );

        // The untouched plants kept the exact rows they published.
        let kept: Vec<&NavigationContribution> = cell
            .contributions
            .iter()
            .filter(|row| row.plant != promoted)
            .collect();
        let expected: Vec<&NavigationContribution> =
            before.iter().filter(|row| row.plant != promoted).collect();
        assert_eq!(kept, expected);
        assert_eq!(
            cell.contributions
                .iter()
                .filter(|row| row.plant == promoted)
                .map(|row| row.kind)
                .collect::<Vec<_>>(),
            vec![NavigationContributionKind::DynamicObstacle]
        );
        // Canonical identity order survives the splice.
        assert!(
            cell.contributions
                .windows(2)
                .all(|pair| pair[0].plant <= pair[1].plant)
        );

        // A revision that moved without any suppression moving re-derives nothing at all.
        assert!(
            cell.resuppress(&rows, 2, &|id| id == promoted, &mut |_| Some(vec![proxy()]))
                .is_empty()
        );
    }

    #[test]
    fn dirty_regions_coalesce_and_drain_once() {
        let mut seam = VegetationNavigationSeam::default();
        // Two overlapping regions merge; a distant one stays separate.
        seam.mark_dirty([bounds(0), bounds(512), bounds(100_000)].into_iter());
        assert_eq!(seam.dirty_regions().len(), 2);

        let drained = seam.take_dirty_regions();
        assert_eq!(drained.len(), 2);
        assert!(
            seam.take_dirty_regions().is_empty(),
            "a drained region is owned by its consumer and never re-delivered"
        );
    }
}
