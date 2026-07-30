use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{UnitInterval, WorldCellKey, WorldPosition};

use crate::{EcologyCellSummary, PlantId, PlantLifecycle, VegetationMutation};

/// One plant's simulated biology at the tick being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologyPlantState {
    pub plant: PlantId,
    pub family: Uuid,
    /// Exact position, which seed placement is measured from.
    pub position: WorldPosition,
    pub lifecycle: PlantLifecycle,
    /// Biological age in ticks.
    pub ecology_tick: u64,
    pub health: UnitInterval,
    pub moisture: UnitInterval,
    pub fuel: UnitInterval,
    /// This plant's share of the local shade budget.
    pub canopy: UnitInterval,
}

/// A species' rules, resolved from its family asset once per tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologySpeciesRules {
    /// Ticks of biological age at which the plant enters sprout, juvenile, mature, and senescent.
    pub stage_ticks: [u64; 4],
    /// Shade the species tolerates before growth suffers.
    pub shade_tolerance: UnitInterval,
    /// Dryness the species tolerates before growth suffers.
    pub drought_tolerance: UnitInterval,
    /// Chance per tick that a mature plant spreads one seed.
    pub propagation_chance: UnitInterval,
    /// Metres a spread seed may land from its parent.
    pub spread_radius_m: u32,
    /// Chance per tick that a stump regrows.
    pub regrowth_chance: UnitInterval,
    /// Chance per tick that a dead plant falls to a stump.
    pub deadfall_chance: UnitInterval,
    /// Below-ground share of the cell a mature plant claims, competed for separately from canopy.
    pub root_demand: UnitInterval,
}

impl Default for EcologySpeciesRules {
    fn default() -> Self {
        Self {
            stage_ticks: [4, 16, 48, 240],
            shade_tolerance: UnitInterval::from_bits(20_000),
            drought_tolerance: UnitInterval::from_bits(16_000),
            propagation_chance: UnitInterval::from_bits(600),
            spread_radius_m: 6,
            regrowth_chance: UnitInterval::from_bits(400),
            deadfall_chance: UnitInterval::from_bits(2_000),
            root_demand: UnitInterval::from_bits(3_000),
        }
    }
}

/// How one species responds to another growing nearby, resolved from the compiled family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologyRelation {
    pub kind: crate::PlantRelationKind,
    pub strength: UnitInterval,
}

/// Declared relations, keyed by (subject family, other family).
pub type EcologyRelations = BTreeMap<(u64, u64), EcologyRelation>;

/// Sampled environment for the tick. Weather owns these values; vegetation only consumes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcologyWeather {
    /// Water reaching the ground this tick.
    pub water: UnitInterval,
    /// Warmth available for growth; below [`DORMANCY_WARMTH`] the tick is dormant.
    pub warmth: UnitInterval,
}

/// Warmth below which growth pauses. Age still advances — a dormant winter is not a stopped clock.
pub const DORMANCY_WARMTH: UnitInterval = UnitInterval::from_bits(8_000);

/// Everything one cell's tick reads. All of it is immutable tick-`N` state.
#[derive(Clone, Copy, Debug)]
pub struct EcologyTickInputs<'a> {
    /// The cell being advanced.
    pub cell: WorldCellKey,
    /// The tick being produced (the successor of the completed one).
    pub tick: u64,
    /// Vegetation-map identity, for random-domain separation.
    pub map: u128,
    /// The cell's plants at tick `N`, in canonical identity order.
    pub plants: &'a [EcologyPlantState],
    /// Neighbour boundary summaries at tick `N`.
    pub neighbours: &'a [EcologyCellSummary],
    /// Per-family rules, keyed by family value.
    pub rules: &'a BTreeMap<u64, EcologySpeciesRules>,
    pub relations: &'a EcologyRelations,
    pub weather: EcologyWeather,
}

/// One cell's owned result for the tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcologyTickOutput {
    /// Typed mutations, in canonical plant order, for the one reducer to apply.
    pub mutations: Vec<VegetationMutation>,
    /// The boundary summary neighbours read next tick.
    pub summary: EcologyCellSummary,
}
