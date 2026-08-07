//! The fixed-tick ecology rules: one deterministic step over a cell's macro plants.
//!
//! A tick is a pure function of immutable tick-`N` state — the cell's own plants plus its
//! neighbours' boundary summaries — returning tick `N+1` as typed mutations. Every stochastic
//! decision is a counter-based sample keyed by (map, species, plant, rule channel) at sample index
//! `tick`, so continuous simulation and unload-then-catch-up reach the same result whichever
//! thread ran the step and however many neighbours were loaded.
//!
//! [`ECOLOGY_SIMULATION_VERSION`](crate::ECOLOGY_SIMULATION_VERSION) names this rule set; changing
//! any threshold here changes results and must bump it.

mod advance;
mod math;
mod model;
mod seed;
mod stage;

#[cfg(test)]
mod tests;

pub use advance::advance_cell;
pub use model::{
    DORMANCY_WARMTH, EcologyPlantState, EcologyRelation, EcologyRelations, EcologySpeciesRules,
    EcologyTickInputs, EcologyTickOutput, EcologyWeather,
};
