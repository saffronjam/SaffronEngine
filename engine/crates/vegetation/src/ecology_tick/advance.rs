use std::collections::BTreeMap;

use saffron_spatial::{RandomDomain, RandomStream, UnitInterval};

use super::math::{HEALTH_STEP, approach, invert, saturating_unit, scale, suitability};
use super::model::{DORMANCY_WARMTH, EcologyTickInputs, EcologyTickOutput};
use super::seed::{plant_key, seed_point};
use super::stage::{live, stage_for};
use crate::{Error, PlantId, PlantLifecycle, Result, VegetationMutation};

/// Random channels, one per stochastic rule, so adding a rule cannot perturb another's stream.
mod channel {
    pub const PROPAGATION: u32 = 1;
    pub const SEED_PLACEMENT: u32 = 2;
    pub const REGROWTH: u32 = 3;
    pub const DEADFALL: u32 = 4;
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

    // Read once for the whole tick from immutable state, so no plant sees a partially updated
    // world. Neighbour contributions count at a quarter weight.
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

        let moisture = approach(state.moisture, inputs.weather.water, HEALTH_STEP);
        let fuel = if live(state.lifecycle) {
            approach(state.fuel, invert(moisture), HEALTH_STEP / 2)
        } else {
            approach(state.fuel, UnitInterval::ONE, HEALTH_STEP / 2)
        };

        let mut lifecycle = state.lifecycle;
        let mut health = state.health;

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
            // The worse of light and water governs, and root pressure takes its cut of the water
            // before the species' drought tolerance sees it.
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
                health
            } else if suitable.bits() >= UnitInterval::ONE.bits() / 2 {
                approach(health, UnitInterval::ONE, HEALTH_STEP)
            } else {
                approach(health, UnitInterval::ZERO, HEALTH_STEP)
            };

            lifecycle = if health == UnitInterval::ZERO {
                PlantLifecycle::Dead
            } else if dormant
                || (waiting
                    && !matches!(
                        state.lifecycle,
                        PlantLifecycle::Mature | PlantLifecycle::Senescent
                    ))
            {
                state.lifecycle
            } else {
                stage_for(age, rules.stage_ticks, state.lifecycle)
            };
        }

        // Age advances every tick, whether or not the stage moved. The reducer's `from`
        // precondition compares against the persistent delta, which a cooked plant does not
        // populate, so the transition is unconditional.
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
            PlantLifecycle::Mature if !dormant => {
                live_plants += 1;
                if stream(channel::PROPAGATION).chance(inputs.tick, 0, rules.propagation_chance) {
                    let placement = stream(channel::SEED_PLACEMENT);
                    if let Some(seed) = seed_point(state, &rules, inputs.tick, placement, age)? {
                        mutations.push(VegetationMutation::Planting(seed));
                    }
                }
            }
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
        summary: crate::EcologyCellSummary {
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
