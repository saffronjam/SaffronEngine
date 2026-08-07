use saffron_spatial::{RandomStream, UnitInterval, WorldPosition};

use super::model::{EcologyPlantState, EcologySpeciesRules};
use crate::{PlantId, PlantIdNamespace, PlantLifecycle, Result};

/// The leading 64 bits of a plant identity, as its random-domain candidate key.
pub(super) fn plant_key(plant: PlantId) -> u64 {
    let bytes = plant.bytes();
    let mut key = [0_u8; 8];
    key.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(key)
}

/// A seed spread from `parent`: a runtime-namespace plant at an offset inside the spread radius.
/// `None` when the species does not spread or the offset leaves the representable world.
pub(super) fn seed_point(
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

    // Minted by the simulation authority, so it carries the runtime namespace rather than
    // pretending to be a cooked plant.
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
