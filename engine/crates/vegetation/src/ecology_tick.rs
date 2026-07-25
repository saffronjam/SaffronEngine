//! The fixed-tick ecology rules: one deterministic step over a cell's macro plants.
//!
//! A tick is a pure function. It reads immutable tick-`N` state — the cell's own plants plus its
//! neighbours' boundary summaries — and returns the owned changes for tick `N+1` as typed
//! mutations, never mutating anything in place. That is what makes the same result reachable by
//! continuous simulation and by unload-then-catch-up: nothing in the step depends on when it ran,
//! which worker ran it, or what order the cells were visited in.
//!
//! Every stochastic decision is a counter-based sample keyed by (map, species, plant, rule
//! channel) at sample index `tick`, so a plant's coin flips are the same whichever thread asks and
//! however many neighbours were loaded. No rule reads wall-clock time, iteration order, or a
//! running accumulator.
//!
//! These are game-world rules, not a botanical model. They are deliberately simple and legible:
//! stage advancement by biological age, suitability from shade and water, health integrating
//! suitability, mortality at zero health, deadfall and regrowth, and seed spread from mature
//! plants. [`ECOLOGY_SIMULATION_VERSION`](crate::ECOLOGY_SIMULATION_VERSION) names this rule set;
//! changing any threshold here changes results and must bump it.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{RandomDomain, RandomStream, UnitInterval, WorldCellKey, WorldPosition};

use crate::{
    EcologyCellSummary, Error, PlantId, PlantIdNamespace, PlantLifecycle, Result,
    VegetationMutation,
};

/// Random channels, one per stochastic rule, so adding a rule cannot perturb another's stream.
mod channel {
    /// Whether a mature plant spreads a seed this tick.
    pub const PROPAGATION: u32 = 1;
    /// Where that seed lands within the parent's spread radius.
    pub const SEED_PLACEMENT: u32 = 2;
    /// Whether a stump regrows this tick.
    pub const REGROWTH: u32 = 3;
    /// Whether a dead plant falls this tick.
    pub const DEADFALL: u32 = 4;
}

/// One plant's simulated biology at the tick being read. A compact row, never a promoted entity:
/// ecology runs over the macro SoA whether or not anything is promoted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcologyPlantState {
    /// Stable identity.
    pub plant: PlantId,
    /// Compiled family.
    pub family: Uuid,
    /// Exact position, which seed placement is measured from.
    pub position: WorldPosition,
    /// Biological lifecycle stage.
    pub lifecycle: PlantLifecycle,
    /// Biological age in ticks.
    pub ecology_tick: u64,
    /// Persistent health.
    pub health: UnitInterval,
    /// Persistent moisture.
    pub moisture: UnitInterval,
    /// Persistent combustible fuel.
    pub fuel: UnitInterval,
    /// This plant's own canopy occupancy, its share of the local shade budget.
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
    /// Below-ground share of the cell a mature plant of this family claims, the root budget it
    /// competes for separately from the canopy.
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
    /// What the relation does.
    pub kind: crate::PlantRelationKind,
    /// How strongly it applies.
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

/// Health gained by a fully suitable tick, and lost by a fully unsuitable one.
const HEALTH_STEP: u16 = 3_000;

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
    /// Declared species relations.
    pub relations: &'a EcologyRelations,
    /// Sampled weather for the tick.
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

/// Advances one cell by exactly one tick.
///
/// # Errors
///
/// [`Error::Mutation`] when the plants are not in canonical identity order, since a cell's result
/// must not depend on the order its rows happened to arrive in, or when a derived value leaves the
/// closed unit range.
pub fn advance_cell(inputs: &EcologyTickInputs<'_>) -> Result<EcologyTickOutput> {
    let mut previous: Option<PlantId> = None;
    for state in inputs.plants {
        if previous.is_some_and(|previous| previous >= state.plant) {
            return Err(Error::Mutation(
                "ecology tick input plants must be in canonical identity order".to_owned(),
            ));
        }
        previous = Some(state.plant);
    }

    // Shade is the local canopy the cell carries plus what its neighbours contribute. It is read
    // once for the whole tick, from immutable state, so no plant sees a partially updated world.
    let local_canopy: u32 = inputs
        .plants
        .iter()
        .filter(|state| live(state.lifecycle))
        .map(|state| u32::from(state.canopy.bits()))
        .sum();
    let neighbour_canopy: u32 = inputs
        .neighbours
        .iter()
        .map(|summary| u32::from(summary.canopy.bits()))
        .sum();
    let ambient_shade = saturating_unit(local_canopy + neighbour_canopy / 4);

    // Roots compete for the same water on the same footprint. The budget has the same shape as the
    // canopy's: what this cell demands, plus a quarter of what each neighbour demands.
    let local_roots: u32 = inputs
        .plants
        .iter()
        .filter(|state| live(state.lifecycle))
        .map(|state| {
            u32::from(
                inputs
                    .rules
                    .get(&state.family.value())
                    .copied()
                    .unwrap_or_default()
                    .root_demand
                    .bits(),
            )
        })
        .sum();
    let neighbour_roots: u32 = inputs
        .neighbours
        .iter()
        .map(|summary| u32::from(summary.roots.bits()))
        .sum();
    let root_pressure = saturating_unit(local_roots + neighbour_roots / 4);

    // Who else is growing here, locally and across every border, so a relation rule can find them.
    let mut present: BTreeMap<u64, (u32, u32, u32)> = BTreeMap::new();
    for state in inputs.plants.iter().filter(|state| live(state.lifecycle)) {
        let entry = present.entry(state.family.value()).or_default();
        entry.0 += u32::from(state.canopy.bits());
        entry.1 += u32::from(state.health.bits());
        entry.2 += 1;
    }
    for presence in inputs
        .neighbours
        .iter()
        .flat_map(|summary| summary.families.iter())
    {
        let entry = present.entry(presence.family).or_default();
        entry.0 += u32::from(presence.canopy.bits()) / 4;
        entry.1 += u32::from(presence.health.bits());
        entry.2 += 1;
    }
    let neighbourhood: Vec<(u64, UnitInterval, UnitInterval)> = present
        .iter()
        .map(|(family, (canopy, health, count))| {
            (
                *family,
                saturating_unit(*canopy),
                saturating_unit(health / count.max(&1)),
            )
        })
        .collect();

    let dormant = inputs.weather.warmth <= DORMANCY_WARMTH;

    let mut mutations = Vec::new();
    let mut live_plants = 0_u32;
    let mut canopy_total = 0_u32;
    let mut root_total = 0_u32;
    let mut by_family: BTreeMap<u64, (u32, u32, u32)> = BTreeMap::new();
    let mut health_total = 0_u32;
    let mut moisture_total = 0_u32;
    let mut fuel_total = 0_u32;

    for state in inputs.plants {
        let rules = inputs
            .rules
            .get(&state.family.value())
            .copied()
            .unwrap_or_default();
        let age = state.ecology_tick.saturating_add(1);

        // Water: what fell, tempered by the species' drought tolerance, settles the plant's
        // moisture toward the supply rather than snapping to it.
        let moisture = approach(state.moisture, inputs.weather.water, HEALTH_STEP);
        // Fuel accumulates with dryness and biomass; a wet plant burns poorly.
        let fuel = if live(state.lifecycle) {
            approach(state.fuel, invert(moisture), HEALTH_STEP / 2)
        } else {
            // Dead matter keeps drying out.
            approach(state.fuel, UnitInterval::ONE, HEALTH_STEP / 2)
        };

        let mut lifecycle = state.lifecycle;
        let mut health = state.health;

        // What the neighbours mean for this species: relief from the shade of a canopy it is
        // adapted to, a suitability bonus or penalty, and whether a successor is still waiting.
        let mut shade_relief = 0_u32;
        let mut bonus = 0_i32;
        let mut waiting = false;
        for (other, canopy, other_health) in &neighbourhood {
            if *other == state.family.value() {
                continue;
            }
            let Some(relation) = inputs.relations.get(&(state.family.value(), *other)) else {
                continue;
            };
            let weight = scale(*canopy, relation.strength);
            match relation.kind {
                crate::PlantRelationKind::Companion => bonus += i32::from(weight.bits()),
                crate::PlantRelationKind::Antagonist => bonus -= i32::from(weight.bits()),
                crate::PlantRelationKind::Understory => shade_relief += u32::from(weight.bits()),
                crate::PlantRelationKind::Successor => {
                    // A successor establishes under the canopy and holds there until it fails.
                    shade_relief += u32::from(weight.bits());
                    if other_health.bits() >= UnitInterval::ONE.bits() / 2 {
                        waiting = true;
                    } else {
                        bonus += i32::from(weight.bits());
                    }
                }
            }
        }

        if live(state.lifecycle) {
            // Suitability: enough light for the species, and enough water. The worse of the two
            // governs — a well-watered plant in full shade is still a shaded plant. Root pressure
            // takes its cut of the water before the species' drought tolerance sees it.
            let perceived_shade = UnitInterval::from_bits(
                ambient_shade
                    .bits()
                    .saturating_sub(u16::try_from(shade_relief).unwrap_or(u16::MAX)),
            );
            let light = suitability(invert(perceived_shade), rules.shade_tolerance);
            let water = suitability(
                scale(moisture, invert(root_pressure)),
                rules.drought_tolerance,
            );
            let suitable = saturating_unit(
                (i32::from(light.min(water).bits()) + bonus)
                    .clamp(0, i32::from(UnitInterval::ONE.bits())) as u32,
            );
            health = if dormant {
                // Dormancy neither grows nor kills; the plant idles.
                health
            } else if suitable.bits() >= UnitInterval::ONE.bits() / 2 {
                approach(health, UnitInterval::ONE, HEALTH_STEP)
            } else {
                approach(health, UnitInterval::ZERO, HEALTH_STEP)
            };

            lifecycle = if health == UnitInterval::ZERO {
                // Mortality: a plant that ran out of health dies standing.
                PlantLifecycle::Dead
            } else if dormant
                || (waiting
                    && !matches!(
                        state.lifecycle,
                        PlantLifecycle::Mature | PlantLifecycle::Senescent
                    ))
            {
                // A successor's seedlings hold their stage while the canopy above them is healthy.
                state.lifecycle
            } else {
                stage_for(age, rules.stage_ticks, state.lifecycle)
            };
        }

        // Age advances every tick, whether or not the stage moved, so the record is the same
        // shape either way. The reducer's `from` precondition compares against the persistent
        // delta, which a cooked plant does not populate, so the transition is unconditional.
        mutations.push(VegetationMutation::LifecycleTransition {
            plant: state.plant,
            from: None,
            to: lifecycle,
            ecology_tick: age,
        });

        if health != state.health {
            mutations.push(VegetationMutation::StateOverride {
                plant: state.plant,
                lifecycle: None,
                phenotype: None,
                health: Some(health),
                moisture: None,
                fuel: None,
                interaction_policy: None,
            });
        }
        if moisture != state.moisture || fuel != state.fuel {
            mutations.push(VegetationMutation::MoistureFuel {
                plant: state.plant,
                moisture,
                fuel,
            });
        }

        let stream = |channel: u32| {
            RandomStream::new(RandomDomain {
                map: inputs.map,
                node_guid: 0,
                node_semantic_revision: crate::ECOLOGY_SIMULATION_VERSION,
                seed_namespace: 0,
                cell: inputs.cell,
                candidate: plant_key(state.plant),
                ancestor: 0,
                species: state.family.value().into(),
                channel,
            })
        };

        match lifecycle {
            // A mature plant may spread one seed per tick, placed inside its spread radius.
            PlantLifecycle::Mature if !dormant => {
                live_plants += 1;
                if stream(channel::PROPAGATION).chance(inputs.tick, 0, rules.propagation_chance) {
                    let placement = stream(channel::SEED_PLACEMENT);
                    if let Some(seed) = seed_point(state, &rules, inputs.tick, placement, age)? {
                        mutations.push(VegetationMutation::Planting(seed));
                    }
                }
            }
            // Dead matter eventually falls, leaving a stump.
            PlantLifecycle::Dead => {
                if stream(channel::DEADFALL).chance(inputs.tick, 0, rules.deadfall_chance) {
                    mutations.push(VegetationMutation::LifecycleTransition {
                        plant: state.plant,
                        from: None,
                        to: PlantLifecycle::Stump,
                        ecology_tick: age,
                    });
                }
            }
            // A stump may regrow under its own identity.
            PlantLifecycle::Stump if !dormant => {
                if stream(channel::REGROWTH).chance(inputs.tick, 0, rules.regrowth_chance) {
                    mutations.push(VegetationMutation::Regrow {
                        plant: state.plant,
                        lifecycle: PlantLifecycle::Sprout,
                        phenotype: 0,
                        ecology_tick: age,
                    });
                }
            }
            _ => {
                if live(lifecycle) {
                    live_plants += 1;
                }
            }
        }

        if live(lifecycle) {
            canopy_total += u32::from(state.canopy.bits());
            root_total += u32::from(rules.root_demand.bits());
            health_total += u32::from(health.bits());
            moisture_total += u32::from(moisture.bits());
            fuel_total += u32::from(fuel.bits());
            let entry = by_family.entry(state.family.value()).or_default();
            entry.0 += u32::from(state.canopy.bits());
            entry.1 += u32::from(health.bits());
            entry.2 += 1;
        }
    }

    let mean = |total: u32| {
        total
            .checked_div(live_plants)
            .map_or(UnitInterval::ZERO, saturating_unit)
    };
    Ok(EcologyTickOutput {
        mutations,
        summary: EcologyCellSummary {
            tick: inputs.tick,
            plants: live_plants,
            canopy: saturating_unit(canopy_total),
            roots: saturating_unit(root_total),
            health: mean(health_total),
            moisture: mean(moisture_total),
            fuel: mean(fuel_total),
            families: by_family
                .into_iter()
                .map(
                    |(family, (canopy, health, count))| crate::EcologyFamilyPresence {
                        family,
                        canopy: saturating_unit(canopy),
                        health: saturating_unit(health / count.max(1)),
                    },
                )
                .collect(),
        },
    })
}

/// Whether a lifecycle stage is a living plant that grows, shades, and competes.
const fn live(lifecycle: PlantLifecycle) -> bool {
    matches!(
        lifecycle,
        PlantLifecycle::Seed
            | PlantLifecycle::Sprout
            | PlantLifecycle::Juvenile
            | PlantLifecycle::Mature
            | PlantLifecycle::Senescent
    )
}

/// The stage a plant of `age` belongs in, never moving backwards from `current`.
fn stage_for(age: u64, thresholds: [u64; 4], current: PlantLifecycle) -> PlantLifecycle {
    let staged = if age >= thresholds[3] {
        PlantLifecycle::Senescent
    } else if age >= thresholds[2] {
        PlantLifecycle::Mature
    } else if age >= thresholds[1] {
        PlantLifecycle::Juvenile
    } else if age >= thresholds[0] {
        PlantLifecycle::Sprout
    } else {
        PlantLifecycle::Seed
    };
    // Growth is monotonic: a stage reached is never un-reached by a rule change or a stale age.
    if stage_rank(staged) > stage_rank(current) {
        staged
    } else {
        current
    }
}

const fn stage_rank(lifecycle: PlantLifecycle) -> u8 {
    match lifecycle {
        PlantLifecycle::Seed => 0,
        PlantLifecycle::Sprout => 1,
        PlantLifecycle::Juvenile => 2,
        PlantLifecycle::Mature => 3,
        PlantLifecycle::Senescent => 4,
        PlantLifecycle::Dead => 5,
        PlantLifecycle::Stump => 6,
        PlantLifecycle::Removed => 7,
    }
}

/// How suitable a condition is for a species: at or above its tolerance is fully suitable, and
/// below it falls off linearly to nothing.
fn suitability(condition: UnitInterval, tolerance: UnitInterval) -> UnitInterval {
    if condition.bits() >= tolerance.bits() {
        return UnitInterval::ONE;
    }
    if tolerance.bits() == 0 {
        return UnitInterval::ONE;
    }
    let scaled = u32::from(condition.bits()) * u32::from(UnitInterval::ONE.bits())
        / u32::from(tolerance.bits());
    saturating_unit(scaled)
}

/// Moves `value` toward `target` by at most `step`, never overshooting.
fn approach(value: UnitInterval, target: UnitInterval, step: u16) -> UnitInterval {
    let current = i32::from(value.bits());
    let goal = i32::from(target.bits());
    let step = i32::from(step);
    let next = if goal > current {
        (current + step).min(goal)
    } else {
        (current - step).max(goal)
    };
    UnitInterval::from_bits(next.clamp(0, i32::from(UnitInterval::ONE.bits())) as u16)
}

/// `value` scaled by `factor`, both closed unit values.
fn scale(value: UnitInterval, factor: UnitInterval) -> UnitInterval {
    UnitInterval::from_bits(
        (u32::from(value.bits()) * u32::from(factor.bits()) / u32::from(UnitInterval::ONE.bits()))
            as u16,
    )
}

fn invert(value: UnitInterval) -> UnitInterval {
    UnitInterval::from_bits(UnitInterval::ONE.bits() - value.bits())
}

fn saturating_unit(value: u32) -> UnitInterval {
    UnitInterval::from_bits(value.min(u32::from(UnitInterval::ONE.bits())) as u16)
}

/// The leading 64 bits of a plant identity, as its random-domain candidate key.
fn plant_key(plant: PlantId) -> u64 {
    let bytes = plant.bytes();
    let mut key = [0_u8; 8];
    key.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(key)
}

/// A seed spread from `parent`: a runtime-namespace plant at an offset inside the spread radius.
///
/// Returns `None` when the species does not spread or the offset leaves the representable world.
fn seed_point(
    parent: &EcologyPlantState,
    rules: &EcologySpeciesRules,
    tick: u64,
    placement: RandomStream,
    age: u64,
) -> Result<Option<crate::PlantPoint>> {
    if rules.spread_radius_m == 0 {
        return Ok(None);
    }
    let radius_ticks =
        i128::from(rules.spread_radius_m) * i128::from(saffron_spatial::LOCAL_TICKS_PER_METER);
    // Two lanes of the same sample: an offset in the ground plane, never in height.
    let span = radius_ticks * 2 + 1;
    let offset_x = i128::from(placement.lane(tick, 0)) % span - radius_ticks;
    let offset_z = i128::from(placement.lane(tick, 1)) % span - radius_ticks;
    let ticks = parent.position.global_ticks();
    let Ok(position) =
        WorldPosition::from_global_ticks([ticks[0] + offset_x, ticks[1], ticks[2] + offset_z])
    else {
        return Ok(None);
    };

    // The seed's identity is minted by the simulation authority, so it carries the runtime
    // namespace rather than pretending to be a cooked plant.
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&plant_key(parent.plant).to_be_bytes());
    bytes[8..].copy_from_slice(&tick.to_be_bytes());
    let plant = PlantId::from_payload(PlantIdNamespace::Runtime, bytes);
    let bounds = saffron_spatial::WorldBounds::new(
        position.global_ticks().map(|tick| tick - 1),
        position.global_ticks().map(|tick| tick + 2),
    )?;
    Ok(Some(crate::PlantPoint {
        id: plant,
        owner: position.cell(),
        position,
        // A simulated row carries no orientation, so a seed stands upright.
        orientation: saffron_spatial::QuantizedOrientation::identity(),
        scale: [saffron_spatial::DecisionScalar::from_integer(1)?; 3],
        bounds,
        family: parent.family,
        variation: 0,
        lifecycle: PlantLifecycle::Seed,
        phenotype: 0,
        representation_class: 0,
        deterministic_key: u128::from_be_bytes(plant.bytes()),
        candidate: 0,
        parent: Some(parent.plant),
        colony: None,
        ecology_tick: age,
        health: UnitInterval::ONE,
        moisture: parent.moisture,
        fuel: UnitInterval::ZERO,
        phenology: UnitInterval::ZERO,
        flags: crate::PlantFlags::RUNTIME,
        interaction_policy: crate::InteractionPolicy::Decorative,
        provenance: 0,
        attachment: None,
        surface_projection: [saffron_spatial::DecisionScalar::from_bits(0); 3],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: u128 = 0x5eed;

    fn cell() -> WorldCellKey {
        WorldCellKey::base(0, 0, 0)
    }

    fn plant(byte: u8) -> PlantId {
        PlantId::explicit([byte; 16]).expect("plant id")
    }

    fn rules() -> BTreeMap<u64, EcologySpeciesRules> {
        BTreeMap::from([(7, EcologySpeciesRules::default())])
    }

    fn state(byte: u8, lifecycle: PlantLifecycle, age: u64) -> EcologyPlantState {
        EcologyPlantState {
            plant: plant(byte),
            family: Uuid(7),
            position: WorldPosition::from_world_meters(saffron_geometry::glam::DVec3::new(
                f64::from(byte),
                0.0,
                4.0,
            ))
            .expect("position"),
            lifecycle,
            ecology_tick: age,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::from_bits(1_000),
            canopy: UnitInterval::from_bits(4_000),
        }
    }

    fn growing_weather() -> EcologyWeather {
        EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        }
    }

    fn inputs<'a>(
        plants: &'a [EcologyPlantState],
        neighbours: &'a [EcologyCellSummary],
        rules: &'a BTreeMap<u64, EcologySpeciesRules>,
        tick: u64,
        weather: EcologyWeather,
    ) -> EcologyTickInputs<'a> {
        EcologyTickInputs {
            cell: cell(),
            tick,
            map: MAP,
            plants,
            neighbours,
            rules,
            relations: &EMPTY_RELATIONS,
            weather,
        }
    }

    /// No declared relations: the tests that need them build their own.
    static EMPTY_RELATIONS: std::sync::LazyLock<EcologyRelations> =
        std::sync::LazyLock::new(EcologyRelations::new);

    /// The step is a pure function of its inputs: the same tick run twice is byte-identical, and
    /// running it against a differently *ordered* neighbour list changes nothing either, because
    /// catch-up visits neighbours in its own order.
    #[test]
    fn a_tick_is_reproducible_and_neighbour_order_independent() {
        let plants = [
            state(1, PlantLifecycle::Mature, 100),
            state(2, PlantLifecycle::Juvenile, 20),
        ];
        let rules = rules();
        let near = EcologyCellSummary {
            tick: 4,
            plants: 3,
            canopy: UnitInterval::from_bits(6_000),
            roots: UnitInterval::from_bits(3_000),
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(20_000),
            fuel: UnitInterval::from_bits(5_000),
            families: Vec::new(),
        };
        let far = EcologyCellSummary {
            canopy: UnitInterval::from_bits(2_000),
            ..near.clone()
        };

        let forward = [near.clone(), far.clone()];
        let reversed = [far, near];
        let first = advance_cell(&inputs(&plants, &forward, &rules, 5, growing_weather())).unwrap();
        let again = advance_cell(&inputs(&plants, &forward, &rules, 5, growing_weather())).unwrap();
        assert_eq!(first, again, "the same tick twice is the same result");

        let swapped =
            advance_cell(&inputs(&plants, &reversed, &rules, 5, growing_weather())).unwrap();
        assert_eq!(
            first, swapped,
            "neighbour order cannot change a cell's result"
        );
    }

    /// Plants must arrive in canonical identity order: a cell's result cannot depend on the order
    /// its rows happened to be gathered in, so an unordered input is refused rather than silently
    /// producing a different answer.
    #[test]
    fn unordered_plants_are_refused() {
        let rules = rules();
        let ordered = [
            state(1, PlantLifecycle::Mature, 100),
            state(2, PlantLifecycle::Mature, 100),
        ];
        let reversed = [ordered[1], ordered[0]];
        assert!(advance_cell(&inputs(&ordered, &[], &rules, 1, growing_weather())).is_ok());
        assert!(advance_cell(&inputs(&reversed, &[], &rules, 1, growing_weather())).is_err());
    }

    /// Age advances every tick and stages only move forward, so a stale age or a changed threshold
    /// cannot un-grow a plant.
    #[test]
    fn stages_advance_with_age_and_never_regress() {
        assert_eq!(
            stage_for(0, [4, 16, 48, 240], PlantLifecycle::Seed),
            PlantLifecycle::Seed
        );
        assert_eq!(
            stage_for(5, [4, 16, 48, 240], PlantLifecycle::Seed),
            PlantLifecycle::Sprout
        );
        assert_eq!(
            stage_for(50, [4, 16, 48, 240], PlantLifecycle::Sprout),
            PlantLifecycle::Mature
        );
        // A younger age against an already-mature plant leaves it mature.
        assert_eq!(
            stage_for(1, [4, 16, 48, 240], PlantLifecycle::Mature),
            PlantLifecycle::Mature
        );

        // Every tick emits the plant's advanced age, whether or not the stage moved.
        let plants = [state(1, PlantLifecycle::Juvenile, 47)];
        let rules = rules();
        let output = advance_cell(&inputs(&plants, &[], &rules, 9, growing_weather())).unwrap();
        let advanced = output.mutations.iter().find_map(|mutation| match mutation {
            VegetationMutation::LifecycleTransition {
                to, ecology_tick, ..
            } => Some((*to, *ecology_tick)),
            _ => None,
        });
        assert_eq!(advanced, Some((PlantLifecycle::Mature, 48)));
    }

    /// A dormant tick idles: age still advances, but health does not move and no stage change,
    /// seed, or regrowth fires. A dormant winter is not a stopped clock.
    #[test]
    fn a_dormant_tick_advances_age_without_growing() {
        let mut row = state(1, PlantLifecycle::Juvenile, 47);
        row.health = UnitInterval::from_bits(20_000);
        let plants = [row];
        let rules = rules();
        let dormant = EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(1_000),
        };
        let output = advance_cell(&inputs(&plants, &[], &rules, 3, dormant)).unwrap();

        let mut saw_age = false;
        for mutation in &output.mutations {
            match mutation {
                VegetationMutation::LifecycleTransition {
                    to, ecology_tick, ..
                } => {
                    // The stage held even though the age crossed the mature threshold.
                    assert_eq!(*to, PlantLifecycle::Juvenile);
                    assert_eq!(*ecology_tick, 48);
                    saw_age = true;
                }
                // Health must not move in a dormant tick.
                VegetationMutation::StateOverride { health, .. } => {
                    panic!("dormancy changed health to {health:?}")
                }
                VegetationMutation::Planting(_) | VegetationMutation::Regrow { .. } => {
                    panic!("dormancy fired a growth rule")
                }
                _ => {}
            }
        }
        assert!(saw_age, "a dormant tick still advances biological age");
    }

    /// Deep shade starves a plant: health falls tick by tick and the plant dies at zero rather
    /// than lingering at full health forever.
    #[test]
    fn shade_starves_a_plant_to_death() {
        let rules = rules();
        // A dense neighbourhood: enough canopy that the species' shade tolerance is not met.
        let shade = EcologyCellSummary {
            tick: 0,
            plants: 40,
            canopy: UnitInterval::ONE,
            roots: UnitInterval::from_bits(3_000),
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::ZERO,
            families: Vec::new(),
        };
        let mut row = state(1, PlantLifecycle::Juvenile, 20);
        let mut lifecycle = row.lifecycle;
        let mut health = row.health;
        for tick in 1..=40_u64 {
            row.health = health;
            row.lifecycle = lifecycle;
            let plants = [row];
            let output = advance_cell(&inputs(
                &plants,
                &[shade.clone(), shade.clone(), shade.clone(), shade.clone()],
                &rules,
                tick,
                growing_weather(),
            ))
            .unwrap();
            for mutation in &output.mutations {
                match mutation {
                    VegetationMutation::StateOverride {
                        health: Some(next), ..
                    } => health = *next,
                    VegetationMutation::LifecycleTransition { to, .. } => lifecycle = *to,
                    _ => {}
                }
            }
            row.ecology_tick = tick;
            if lifecycle == PlantLifecycle::Dead {
                break;
            }
        }
        assert_eq!(
            health,
            UnitInterval::ZERO,
            "shade drained the plant's health"
        );
        assert_eq!(
            lifecycle,
            PlantLifecycle::Dead,
            "a plant at zero health dies"
        );
    }

    /// A spread seed is a runtime-namespace plant owned by the cell it lands in, parented to the
    /// plant that spread it — never a forged cooked identity.
    #[test]
    fn a_spread_seed_is_a_runtime_plant_parented_to_its_source() {
        let rules = BTreeMap::from([(
            7,
            EcologySpeciesRules {
                // Certain propagation, so the test is about the seed and not the coin flip.
                propagation_chance: UnitInterval::ONE,
                ..EcologySpeciesRules::default()
            },
        )]);
        let plants = [state(1, PlantLifecycle::Mature, 100)];
        let output = advance_cell(&inputs(&plants, &[], &rules, 11, growing_weather())).unwrap();
        let seed = output
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                VegetationMutation::Planting(point) => Some(point),
                _ => None,
            })
            .expect("a mature plant spread a seed");
        assert_eq!(seed.id.namespace().unwrap(), PlantIdNamespace::Runtime);
        assert_eq!(seed.parent, Some(plant(1)));
        assert_eq!(seed.lifecycle, PlantLifecycle::Seed);
        assert_eq!(seed.family, Uuid(7));
        // It is owned by whichever cell it landed in, and its bounds enclose its position.
        assert_eq!(seed.owner, seed.position.cell());
        assert!(seed.bounds.contains(seed.position));
    }

    /// The phase's central claim: running ticks one at a time is the same as being unloaded and
    /// caught up in a batch. Both routes advance the same cell through the same tick range and must
    /// reach an identical state and checkpoint identity.
    #[test]
    fn catch_up_equals_continuous_simulation() {
        let rules = rules();
        let start = [
            state(1, PlantLifecycle::Mature, 100),
            state(2, PlantLifecycle::Sprout, 5),
            state(3, PlantLifecycle::Dead, 400),
        ];

        // Continuous: tick by tick, each step reading the previous step's rows.
        let mut continuous_rows = start;
        let mut continuous_state = crate::EcologyState::new();
        for tick in 1..=12_u64 {
            continuous_state.advance_world_to(tick).unwrap();
            let output = advance_cell(&inputs(
                &continuous_rows,
                &[],
                &rules,
                tick,
                growing_weather(),
            ))
            .unwrap();
            continuous_rows = apply(&continuous_rows, &output.mutations, tick);
            continuous_state
                .publish_region_tick(tick, &BTreeMap::from([(cell(), output.summary)]))
                .unwrap();
        }

        // Catch-up: the cell was unloaded at tick 0 and is advanced through the range the clock
        // itself hands out.
        let mut caught_rows = start;
        let mut caught_state = crate::EcologyState::new();
        caught_state.advance_world_to(12).unwrap();
        for tick in caught_state.clock().ticks_from(0) {
            let output =
                advance_cell(&inputs(&caught_rows, &[], &rules, tick, growing_weather())).unwrap();
            caught_rows = apply(&caught_rows, &output.mutations, tick);
            caught_state
                .publish_region_tick(tick, &BTreeMap::from([(cell(), output.summary)]))
                .unwrap();
        }

        assert_eq!(
            continuous_rows, caught_rows,
            "the plants ended in the same state"
        );
        assert_eq!(
            continuous_state.checkpoint_identity(),
            caught_state.checkpoint_identity(),
            "both routes reached the same checkpoint"
        );
    }

    /// Folds a tick's mutations back into the rows, the way the reducer would, so a multi-tick
    /// test can step without a full `VegetationState`.
    fn apply(
        rows: &[EcologyPlantState; 3],
        mutations: &[VegetationMutation],
        tick: u64,
    ) -> [EcologyPlantState; 3] {
        let mut next = *rows;
        for mutation in mutations {
            match mutation {
                VegetationMutation::LifecycleTransition { plant, to, .. } => {
                    if let Some(row) = next.iter_mut().find(|row| row.plant == *plant) {
                        row.lifecycle = *to;
                        row.ecology_tick = tick;
                    }
                }
                VegetationMutation::StateOverride {
                    plant,
                    health: Some(health),
                    ..
                } => {
                    if let Some(row) = next.iter_mut().find(|row| row.plant == *plant) {
                        row.health = *health;
                    }
                }
                VegetationMutation::MoistureFuel {
                    plant,
                    moisture,
                    fuel,
                } => {
                    if let Some(row) = next.iter_mut().find(|row| row.plant == *plant) {
                        row.moisture = *moisture;
                        row.fuel = *fuel;
                    }
                }
                // A spread seed becomes its own row in the real reducer; this fixture tracks the
                // three starting plants, and seeds have their own test.
                _ => {}
            }
        }
        next
    }

    /// The summary a cell publishes describes only its live plants, and it is what neighbours read.
    #[test]
    fn the_published_summary_counts_live_plants_only() {
        let rules = rules();
        let plants = [
            state(1, PlantLifecycle::Mature, 100),
            state(2, PlantLifecycle::Dead, 300),
            state(3, PlantLifecycle::Juvenile, 20),
        ];
        let output = advance_cell(&inputs(&plants, &[], &rules, 7, growing_weather())).unwrap();
        assert_eq!(output.summary.tick, 7);
        assert_eq!(
            output.summary.plants, 2,
            "the dead plant is not a live plant"
        );
        assert!(output.summary.canopy.bits() > 0);
    }

    /// A neighbour cell packed with roots takes its share of the water before this plant's drought
    /// tolerance sees any, so the same tick that grows an uncrowded plant costs a crowded one.
    #[test]
    fn root_competition_takes_its_share_of_the_water() {
        let rules = BTreeMap::from([(
            7,
            EcologySpeciesRules {
                // A thirsty species, so the loss shows in one tick.
                drought_tolerance: UnitInterval::from_bits(40_000),
                ..EcologySpeciesRules::default()
            },
        )]);
        let mut row = state(1, PlantLifecycle::Mature, 100);
        row.health = UnitInterval::from_bits(40_000);
        let plants = [row];

        let health_after = |roots: u16| {
            let neighbour = EcologyCellSummary {
                tick: 0,
                plants: 30,
                canopy: UnitInterval::ZERO,
                roots: UnitInterval::from_bits(roots),
                health: UnitInterval::ONE,
                moisture: UnitInterval::from_bits(30_000),
                fuel: UnitInterval::ZERO,
                families: Vec::new(),
            };
            let output = advance_cell(&inputs(
                &plants,
                &[
                    neighbour.clone(),
                    neighbour.clone(),
                    neighbour.clone(),
                    neighbour,
                ],
                &rules,
                1,
                growing_weather(),
            ))
            .unwrap();
            output.summary.health
        };

        assert!(
            health_after(UnitInterval::ONE.bits()) < health_after(0),
            "crowded roots cost the plant health an open one gains"
        );
    }

    /// The same neighbour helps or hurts depending on the declared relation, and nothing at all
    /// without one.
    #[test]
    fn a_companion_lifts_where_an_antagonist_suppresses() {
        // A species that needs full light, so one neighbour's canopy already puts it near the
        // line a relation can push it across.
        let rules = BTreeMap::from([(
            7,
            EcologySpeciesRules {
                shade_tolerance: UnitInterval::ONE,
                ..EcologySpeciesRules::default()
            },
        )]);
        let mut row = state(1, PlantLifecycle::Mature, 100);
        row.health = UnitInterval::from_bits(30_000);
        let plants = [row];
        let neighbour = EcologyCellSummary {
            tick: 0,
            plants: 4,
            canopy: UnitInterval::ONE,
            roots: UnitInterval::ZERO,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::ZERO,
            families: vec![crate::EcologyFamilyPresence {
                family: 9,
                canopy: UnitInterval::ONE,
                health: UnitInterval::ONE,
            }],
        };

        let health_after = |kind: Option<crate::PlantRelationKind>| {
            let relations: EcologyRelations = kind
                .map(|kind| {
                    BTreeMap::from([(
                        (7, 9),
                        EcologyRelation {
                            kind,
                            strength: UnitInterval::ONE,
                        },
                    )])
                })
                .unwrap_or_default();
            advance_cell(&EcologyTickInputs {
                cell: cell(),
                tick: 1,
                map: MAP,
                plants: &plants,
                neighbours: std::slice::from_ref(&neighbour),
                rules: &rules,
                relations: &relations,
                weather: growing_weather(),
            })
            .unwrap()
            .summary
            .health
        };

        let alone = health_after(None);
        let helped = health_after(Some(crate::PlantRelationKind::Companion));
        let hurt = health_after(Some(crate::PlantRelationKind::Antagonist));
        assert!(hurt < alone, "an antagonist costs the plant health");
        assert!(alone <= helped, "a companion never costs it health");
        assert!(
            alone > UnitInterval::from_bits(30_000),
            "the plant grows unaided"
        );
        assert!(hurt < helped, "the two relations pull opposite ways");
    }

    /// A successor waits under a healthy canopy: its seedlings age but hold their stage, and they
    /// resume advancing once the canopy above them is failing.
    #[test]
    fn a_successor_holds_its_stage_until_the_canopy_fails() {
        let rules = rules();
        let relations: EcologyRelations = BTreeMap::from([(
            (7, 9),
            EcologyRelation {
                kind: crate::PlantRelationKind::Successor,
                strength: UnitInterval::ONE,
            },
        )]);
        // Old enough to be Mature by age alone.
        let plants = [state(1, PlantLifecycle::Sprout, 100)];

        let stage_after = |canopy_health: u16| {
            let neighbour = EcologyCellSummary {
                tick: 0,
                plants: 4,
                canopy: UnitInterval::from_bits(4_000),
                roots: UnitInterval::ZERO,
                health: UnitInterval::from_bits(canopy_health),
                moisture: UnitInterval::from_bits(30_000),
                fuel: UnitInterval::ZERO,
                families: vec![crate::EcologyFamilyPresence {
                    family: 9,
                    canopy: UnitInterval::from_bits(40_000),
                    health: UnitInterval::from_bits(canopy_health),
                }],
            };
            let output = advance_cell(&EcologyTickInputs {
                cell: cell(),
                tick: 1,
                map: MAP,
                plants: &plants,
                neighbours: std::slice::from_ref(&neighbour),
                rules: &rules,
                relations: &relations,
                weather: growing_weather(),
            })
            .unwrap();
            output
                .mutations
                .iter()
                .find_map(|mutation| match mutation {
                    VegetationMutation::LifecycleTransition { to, .. } => Some(*to),
                    _ => None,
                })
                .expect("every tick records the stage it settled on")
        };

        assert_eq!(
            stage_after(UnitInterval::ONE.bits()),
            PlantLifecycle::Sprout,
            "the canopy above is healthy, so the successor holds"
        );
        assert_eq!(
            stage_after(1_000),
            PlantLifecycle::Mature,
            "the canopy is failing, so the successor takes its place"
        );
    }

    /// An understory species survives shade that would starve it without the relation, because the
    /// canopy it lives under is the shade it is adapted to.
    #[test]
    fn an_understory_species_tolerates_the_canopy_it_lives_under() {
        let rules = rules();
        let mut row = state(1, PlantLifecycle::Mature, 100);
        row.health = UnitInterval::from_bits(20_000);
        let plants = [row];
        let neighbour = EcologyCellSummary {
            tick: 0,
            plants: 40,
            canopy: UnitInterval::ONE,
            roots: UnitInterval::ZERO,
            health: UnitInterval::ONE,
            moisture: UnitInterval::from_bits(30_000),
            fuel: UnitInterval::ZERO,
            families: vec![crate::EcologyFamilyPresence {
                family: 9,
                canopy: UnitInterval::ONE,
                health: UnitInterval::ONE,
            }],
        };

        let health_after = |relations: EcologyRelations| {
            advance_cell(&EcologyTickInputs {
                cell: cell(),
                tick: 1,
                map: MAP,
                plants: &plants,
                neighbours: &[
                    neighbour.clone(),
                    neighbour.clone(),
                    neighbour.clone(),
                    neighbour.clone(),
                ],
                rules: &rules,
                relations: &relations,
                weather: growing_weather(),
            })
            .unwrap()
            .summary
            .health
        };

        let exposed = health_after(EcologyRelations::new());
        let adapted = health_after(BTreeMap::from([(
            (7, 9),
            EcologyRelation {
                kind: crate::PlantRelationKind::Understory,
                strength: UnitInterval::ONE,
            },
        )]));
        assert!(
            exposed < adapted,
            "the same deep shade starves the exposed plant and suits the understory one"
        );
    }
}
