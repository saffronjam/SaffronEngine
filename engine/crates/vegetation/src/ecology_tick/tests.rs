use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{UnitInterval, WorldCellKey, WorldPosition};

use super::stage::stage_for;
use super::{
    EcologyPlantState, EcologyRelation, EcologyRelations, EcologySpeciesRules, EcologyTickInputs,
    EcologyWeather, advance_cell,
};
use crate::{EcologyCellSummary, PlantId, PlantIdNamespace, PlantLifecycle, VegetationMutation};

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

static EMPTY_RELATIONS: std::sync::LazyLock<EcologyRelations> =
    std::sync::LazyLock::new(EcologyRelations::new);

/// The step is a pure function of its inputs, and catch-up visits neighbours in its own order.
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

    let swapped = advance_cell(&inputs(&plants, &reversed, &rules, 5, growing_weather())).unwrap();
    assert_eq!(
        first, swapped,
        "neighbour order cannot change a cell's result"
    );
}

/// Plants must arrive in canonical identity order, so gather order cannot change the answer.
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

/// Age advances every tick and stages only move forward.
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
    assert_eq!(
        stage_for(1, [4, 16, 48, 240], PlantLifecycle::Mature),
        PlantLifecycle::Mature
    );

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

/// A dormant tick idles: age advances, but health, stages, seeds, and regrowth all hold.
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
                assert_eq!(*to, PlantLifecycle::Juvenile);
                assert_eq!(*ecology_tick, 48);
                saw_age = true;
            }
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

/// Deep shade drains health tick by tick and the plant dies at zero.
#[test]
fn shade_starves_a_plant_to_death() {
    let rules = rules();
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

/// A spread seed is a runtime-namespace plant owned by the cell it lands in, never a forged
/// cooked identity.
#[test]
fn a_spread_seed_is_a_runtime_plant_parented_to_its_source() {
    let rules = BTreeMap::from([(
        7,
        EcologySpeciesRules {
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
    assert_eq!(seed.owner, seed.position.cell());
    assert!(seed.bounds.contains(seed.position));
}

/// Running ticks one at a time is the same as being unloaded and caught up in a batch: both
/// routes must reach an identical state and checkpoint identity.
#[test]
fn catch_up_equals_continuous_simulation() {
    let rules = rules();
    let start = [
        state(1, PlantLifecycle::Mature, 100),
        state(2, PlantLifecycle::Sprout, 5),
        state(3, PlantLifecycle::Dead, 400),
    ];

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

    let mut caught_rows = start;
    let mut caught_state = crate::EcologyState::new();
    caught_state.advance_world_to(12).unwrap();
    for tick in 1..=caught_state.clock().tick() {
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

/// Folds a tick's mutations back into the rows, the way the reducer would, so a multi-tick test
/// can step without a full `VegetationState`. Spread seeds have their own test and are ignored.
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
            _ => {}
        }
    }
    next
}

/// The summary a cell publishes describes only its live plants.
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
/// tolerance sees any.
#[test]
fn root_competition_takes_its_share_of_the_water() {
    let rules = BTreeMap::from([(
        7,
        EcologySpeciesRules {
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

/// The same neighbour helps or hurts depending on the declared relation, and does nothing without
/// one.
#[test]
fn a_companion_lifts_where_an_antagonist_suppresses() {
    // A species that needs full light, so one neighbour's canopy already puts it near the line a
    // relation can push it across.
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

/// A successor's seedlings age but hold their stage under a healthy canopy, and resume advancing
/// once the canopy above them is failing.
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

/// An understory species survives shade that would starve it without the relation.
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
