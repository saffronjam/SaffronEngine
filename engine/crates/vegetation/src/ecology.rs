//! The ecology clock and the persisted state that makes catch-up provably equal to continuous
//! simulation.
//!
//! Biological age is its own axis. The calendar and time of day drive appearance and seasonal
//! rates, but they are presentation inputs: moving them backwards previews a different season, it
//! does not un-grow a tree, revive a dead one, or re-emit an event. That asymmetry is enforced
//! here rather than left to callers — [`EcologyClock`] only moves forward, and the persisted state
//! records the rule-set version it was produced under so a changed rule set is a loud mismatch
//! instead of a silent re-simulation.

use std::collections::BTreeMap;

use saffron_spatial::{UnitInterval, WorldCellKey};

use crate::{ContentHash, Error, Result};

/// Identity of the rule set and numeric contract a simulated result was produced under.
///
/// Bump it whenever a rule, a coefficient, an evaluation order, or a numeric convention changes.
/// State carrying an older version is not silently advanced against new rules: the results would
/// no longer match the continuous simulation the state claims to be a checkpoint of.
pub const ECOLOGY_SIMULATION_VERSION: u32 = 2;

/// A monotonic biological clock, counted in fixed ecology ticks.
///
/// The clock is world biological time: how far the world has aged, not how far any one cell has
/// been simulated. It is deliberately not derived from the calendar, so a project can rewind time
/// of day to preview autumn without rewinding growth. Time may jump — a save loaded a hundred
/// ticks later moves the clock a hundred ticks in one step — and the cells behind it are brought
/// forward by catch-up, one executed tick at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EcologyClock {
    tick: u64,
}

impl EcologyClock {
    /// A clock at `tick`.
    #[must_use]
    pub const fn at(tick: u64) -> Self {
        Self { tick }
    }

    /// World biological time.
    #[must_use]
    pub const fn tick(self) -> u64 {
        self.tick
    }

    /// The ticks that must run to bring `from` up to the clock, empty when already there.
    #[must_use]
    pub const fn ticks_from(self, from: u64) -> std::ops::RangeInclusive<u64> {
        (from + 1)..=self.tick
    }

    /// Moves world time to `target`.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when `target` is behind the clock. A caller that wants to preview an
    /// earlier season changes the calendar, not this.
    pub fn advance_to(&mut self, target: u64) -> Result<()> {
        if target < self.tick {
            return Err(Error::Mutation(format!(
                "ecology clock cannot move backwards: at tick {}, asked for {target}",
                self.tick
            )));
        }
        self.tick = target;
        Ok(())
    }
}

/// One cell's boundary summary: what a neighbouring cell needs to know about it without reading
/// its plants.
///
/// Shade, competition, and propagation all cross cell borders, so a cell cannot be advanced from
/// its own state alone. These summaries are the immutable per-tick facts neighbours read, which is
/// what lets a dependency region advance from checkpoints instead of from live neighbours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcologyCellSummary {
    /// Tick the summary describes.
    pub tick: u64,
    /// Live plants the cell held at that tick.
    pub plants: u32,
    /// Summed canopy occupancy, the shade a neighbour receives.
    pub canopy: UnitInterval,
    /// Summed below-ground demand, the root competition a neighbour meets.
    pub roots: UnitInterval,
    /// Mean health across live plants.
    pub health: UnitInterval,
    /// Mean moisture across live plants.
    pub moisture: UnitInterval,
    /// Mean combustible fuel across live plants.
    pub fuel: UnitInterval,
    /// Which species are present and how they are faring, in canonical family order. Companion,
    /// antagonist, successor, and understory rules all need to know *which* neighbour is there,
    /// not just how much shade it casts.
    pub families: Vec<EcologyFamilyPresence>,
}

/// One species' presence in a cell at a tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcologyFamilyPresence {
    /// The family.
    pub family: u64,
    /// Its summed canopy occupancy.
    pub canopy: UnitInterval,
    /// Mean health across its live plants, which is what a successor waits on.
    pub health: UnitInterval,
}

/// The persisted ecology state: how far biology has advanced, under which rules, and the boundary
/// summaries a catch-up reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcologyState {
    version: u32,
    clock: EcologyClock,
    summaries: BTreeMap<WorldCellKey, EcologyCellSummary>,
}

impl Default for EcologyState {
    fn default() -> Self {
        Self::new()
    }
}

impl EcologyState {
    /// Empty state at tick zero under the current rule set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: ECOLOGY_SIMULATION_VERSION,
            clock: EcologyClock::default(),
            summaries: BTreeMap::new(),
        }
    }

    /// Rebuilds state decoded from a snapshot.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when the snapshot was produced by a different rule set, or when a
    /// summary describes a tick the clock has not reached.
    pub fn from_parts(
        version: u32,
        clock: EcologyClock,
        summaries: BTreeMap<WorldCellKey, EcologyCellSummary>,
    ) -> Result<Self> {
        if version != ECOLOGY_SIMULATION_VERSION {
            return Err(Error::Mutation(format!(
                "ecology state was simulated under rule set {version}, this build runs \
                 {ECOLOGY_SIMULATION_VERSION}"
            )));
        }
        if let Some((cell, summary)) = summaries
            .iter()
            .find(|(_, summary)| summary.tick > clock.tick())
        {
            return Err(Error::Mutation(format!(
                "cell {cell} summarises tick {} beyond world time {}",
                summary.tick,
                clock.tick()
            )));
        }
        Ok(Self {
            version,
            clock,
            summaries,
        })
    }

    /// The rule set this state was produced under.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The biological clock.
    #[must_use]
    pub const fn clock(&self) -> EcologyClock {
        self.clock
    }

    /// Boundary summaries in canonical cell order.
    #[must_use]
    pub const fn summaries(&self) -> &BTreeMap<WorldCellKey, EcologyCellSummary> {
        &self.summaries
    }

    /// Moves world biological time forward.
    ///
    /// # Errors
    ///
    /// Propagates [`EcologyClock::advance_to`].
    pub fn advance_world_to(&mut self, target: u64) -> Result<()> {
        self.clock.advance_to(target)
    }

    /// The tick `cell` has been simulated to. A cell with no summary has not been simulated.
    #[must_use]
    pub fn cell_tick(&self, cell: WorldCellKey) -> u64 {
        self.summaries.get(&cell).map_or(0, |summary| summary.tick)
    }

    /// Whether `cell` has caught up to world time, which is what makes its simulation facet
    /// readable.
    #[must_use]
    pub fn is_caught_up(&self, cell: WorldCellKey) -> bool {
        self.cell_tick(cell) == self.clock.tick()
    }

    /// Publishes one dependency region's completed tick across every cell it touched.
    ///
    /// Every summary is checked before any is stored, so a region either gains a whole tick
    /// generation or none of it. A cell may only step to its own successor tick, and never past
    /// world time.
    ///
    /// # Errors
    ///
    /// [`Error::Mutation`] when a summary is not the successor of that cell's tick, when the
    /// summaries disagree on which tick they belong to, or when the tick is ahead of the clock.
    pub fn publish_region_tick(
        &mut self,
        tick: u64,
        summaries: &BTreeMap<WorldCellKey, EcologyCellSummary>,
    ) -> Result<()> {
        if tick > self.clock.tick() {
            return Err(Error::Mutation(format!(
                "ecology tick {tick} is ahead of world time {}",
                self.clock.tick()
            )));
        }
        for (cell, summary) in summaries {
            if summary.tick != tick {
                return Err(Error::Mutation(format!(
                    "cell {cell} summarises tick {} in the publication of tick {tick}",
                    summary.tick
                )));
            }
            let current = self.cell_tick(*cell);
            if tick != current + 1 {
                return Err(Error::Mutation(format!(
                    "cell {cell} is at tick {current}, so tick {tick} is not its successor"
                )));
            }
        }
        for (cell, summary) in summaries {
            self.summaries.insert(*cell, summary.clone());
        }
        Ok(())
    }

    /// The checkpoint identity: a content hash over the rule set, the completed tick, and every
    /// boundary summary in canonical cell order.
    ///
    /// Two runs that reached the same tick by different routes — continuous simulation, or unload
    /// then dependency-region catch-up — produce the same identity exactly when their committed
    /// state agrees. That equality is the phase's central claim, so it has one canonical
    /// encoding rather than a comparison rule per caller.
    #[must_use]
    pub fn checkpoint_identity(&self) -> ContentHash {
        let mut bytes = b"saffron-anima/vegetation-ecology/checkpoint/v1".to_vec();
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.clock.tick().to_be_bytes());
        bytes.extend_from_slice(&(self.summaries.len() as u64).to_be_bytes());
        for (cell, summary) in &self.summaries {
            for coordinate in cell.coordinates() {
                bytes.extend_from_slice(&coordinate.to_be_bytes());
            }
            bytes.extend_from_slice(&cell.level().to_be_bytes());
            bytes.extend_from_slice(&summary.tick.to_be_bytes());
            bytes.extend_from_slice(&summary.plants.to_be_bytes());
            for value in [
                summary.canopy,
                summary.roots,
                summary.health,
                summary.moisture,
                summary.fuel,
            ] {
                bytes.extend_from_slice(&value.canonical_bytes());
            }
            bytes.extend_from_slice(&(summary.families.len() as u64).to_be_bytes());
            for presence in &summary.families {
                bytes.extend_from_slice(&presence.family.to_be_bytes());
                bytes.extend_from_slice(&presence.canopy.canonical_bytes());
                bytes.extend_from_slice(&presence.health.canonical_bytes());
            }
        }
        ContentHash::of(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: i64) -> WorldCellKey {
        WorldCellKey::base(x, 0, 0)
    }

    fn summary(tick: u64, plants: u32) -> EcologyCellSummary {
        EcologyCellSummary {
            tick,
            plants,
            canopy: UnitInterval::from_bits(20_000),
            roots: UnitInterval::from_bits(6_000),
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::from_bits(40_000),
            families: vec![EcologyFamilyPresence {
                family: 7,
                canopy: UnitInterval::from_bits(20_000),
                health: UnitInterval::ONE,
            }],
        }
    }

    #[test]
    fn world_time_moves_forward_and_may_jump() {
        let mut clock = EcologyClock::default();
        assert_eq!(clock.ticks_from(0), 1..=0);
        // A save loaded much later jumps world time; the cells behind it catch up by executing
        // every tick in between.
        clock.advance_to(100).unwrap();
        assert_eq!(clock.tick(), 100);
        assert_eq!(clock.ticks_from(97).count(), 3);

        // A calendar rewind asks for an earlier tick: refused, not silently accepted.
        assert!(clock.advance_to(99).is_err());
        // Advancing to where it already is changes nothing.
        clock.advance_to(100).unwrap();
        assert_eq!(clock.tick(), 100);
    }

    #[test]
    fn a_region_publishes_a_whole_tick_or_none_of_it() {
        let mut state = EcologyState::new();
        state.advance_world_to(2).unwrap();

        // Two cells at tick 0; a publication naming one of them at tick 2 skips a generation, so
        // the whole publication is refused and neither cell moves.
        let skipping = BTreeMap::from([(cell(0), summary(1, 4)), (cell(1), summary(2, 4))]);
        assert!(state.publish_region_tick(1, &skipping).is_err());
        assert_eq!(state.cell_tick(cell(0)), 0);
        assert_eq!(state.cell_tick(cell(1)), 0);

        let tick_one = BTreeMap::from([(cell(0), summary(1, 4)), (cell(1), summary(1, 7))]);
        state.publish_region_tick(1, &tick_one).unwrap();
        assert_eq!(state.cell_tick(cell(1)), 1);
        // World time is at 2, so a cell at 1 is not yet readable as simulated.
        assert!(!state.is_caught_up(cell(0)));

        // Replaying the same tick would double-apply it.
        assert!(state.publish_region_tick(1, &tick_one).is_err());
        // And nothing may run ahead of world time.
        let tick_three = BTreeMap::from([(cell(0), summary(3, 4))]);
        assert!(state.publish_region_tick(3, &tick_three).is_err());

        let tick_two = BTreeMap::from([(cell(0), summary(2, 4)), (cell(1), summary(2, 7))]);
        state.publish_region_tick(2, &tick_two).unwrap();
        assert!(state.is_caught_up(cell(0)) && state.is_caught_up(cell(1)));
    }

    #[test]
    fn the_checkpoint_identity_pins_rules_tick_and_every_summary() {
        let mut continuous = EcologyState::new();
        continuous.advance_world_to(1).unwrap();
        continuous
            .publish_region_tick(
                1,
                &BTreeMap::from([(cell(0), summary(1, 4)), (cell(1), summary(1, 7))]),
            )
            .unwrap();

        // The same tick reached by a world-time jump and a catch-up: the identity is about the
        // committed state, not the route taken to it.
        let mut caught_up = EcologyState::new();
        caught_up.advance_world_to(1).unwrap();
        caught_up
            .publish_region_tick(
                1,
                &BTreeMap::from([(cell(1), summary(1, 7)), (cell(0), summary(1, 4))]),
            )
            .unwrap();
        assert_eq!(
            continuous.checkpoint_identity(),
            caught_up.checkpoint_identity()
        );

        // One differing plant count is a different checkpoint.
        let mut diverged = EcologyState::new();
        diverged.advance_world_to(1).unwrap();
        diverged
            .publish_region_tick(
                1,
                &BTreeMap::from([(cell(0), summary(1, 4)), (cell(1), summary(1, 8))]),
            )
            .unwrap();
        assert_ne!(
            continuous.checkpoint_identity(),
            diverged.checkpoint_identity()
        );
    }

    #[test]
    fn state_from_a_different_rule_set_is_refused() {
        let summaries = BTreeMap::new();
        assert!(
            EcologyState::from_parts(
                ECOLOGY_SIMULATION_VERSION + 1,
                EcologyClock::at(5),
                summaries
            )
            .is_err()
        );
    }

    #[test]
    fn a_summary_beyond_the_completed_tick_is_refused() {
        let mut summaries = BTreeMap::new();
        summaries.insert(cell(0), summary(9, 1));
        assert!(
            EcologyState::from_parts(
                ECOLOGY_SIMULATION_VERSION,
                EcologyClock::at(4),
                summaries.clone()
            )
            .is_err()
        );
        // At or below the clock it decodes.
        assert!(
            EcologyState::from_parts(ECOLOGY_SIMULATION_VERSION, EcologyClock::at(9), summaries)
                .is_ok()
        );
    }
}
