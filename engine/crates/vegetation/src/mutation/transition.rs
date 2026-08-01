//! The gameplay-visible transition a committed mutation produces.

use saffron_spatial::{UnitInterval, WorldCellKey};

use crate::{PlantId, PlantLifecycle};

use super::{VegetationMutation, VegetationMutationRecord, VegetationState};

/// What one committed mutation did to the world, in gameplay terms rather than storage terms.
///
/// Scripts, VFX, audio, quests, fire, and navigation all consume the same typed transition, so a
/// reducer commit is the single place a vegetation change becomes observable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VegetationTransitionKind {
    /// A plant took damage; `health` is the value it settled at.
    Damaged {
        /// Damage applied.
        amount: UnitInterval,
        /// Resulting persistent health.
        health: UnitInterval,
    },
    /// A plant was harvested into its harvested phenotype.
    Harvested { phenotype: u32 },
    /// A plant burned; `remaining_fuel` is what is left to burn.
    Burned {
        phenotype: u32,
        /// Fuel remaining after the event.
        remaining_fuel: UnitInterval,
    },
    /// A plant was removed from every downstream projection.
    Removed,
    /// A plant was added by an authority (a runtime planting or an authored anchor).
    Planted,
    /// A removed plant started a new biological lifecycle under the same identity.
    Regrew {
        /// Regrown lifecycle state.
        lifecycle: PlantLifecycle,
        phenotype: u32,
    },
    /// A plant's biological lifecycle advanced or was replaced.
    LifecycleChanged {
        /// Required prior state when the mutation declared one.
        from: Option<PlantLifecycle>,
        /// New lifecycle state.
        to: PlantLifecycle,
    },
    /// A plant was set alight.
    Ignited,
    /// A burning plant was put out.
    Extinguished,
    /// A plant's persistent water and combustible fuel changed.
    Wetted {
        moisture: UnitInterval,
        fuel: UnitInterval,
    },
    /// A plant's biological or interaction values were replaced wholesale.
    StateReplaced,
    /// A plant's exact transform changed (an authored override, or a promoted view's write-back).
    Moved,
    /// A signed disturbance-mask tile changed: trample and crush truth, never cosmetic bend.
    Disturbed {
        /// Disturbance class bits.
        categories: u32,
    },
}

/// One typed transition a committed mutation produced, addressed by cell and (where the mutation
/// names one) by plant. Emitted once per committed record; an idempotent replay emits nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationTransition {
    /// The transaction that committed it.
    pub transaction: u128,
    /// The cell whose persistent state changed.
    pub cell: WorldCellKey,
    /// The plant it names, absent for a cell-wide change (a field or disturbance tile).
    pub plant: Option<PlantId>,
    pub kind: VegetationTransitionKind,
}

/// The typed transition a just-applied record produced, read from the record plus the state it
/// settled at. `None` for a change with no gameplay-visible transition (a quantized field tile is
/// world truth an adapter re-reads, not an event).
pub(super) fn transition_for(
    state: &VegetationState,
    record: &VegetationMutationRecord,
) -> Option<VegetationTransition> {
    let cell = record.header.cell;
    let settled = |plant: &PlantId| {
        state
            .cells
            .get(&cell)
            .and_then(|cell| cell.plants.get(plant))
    };
    let (plant, kind) = match &record.mutation {
        VegetationMutation::FieldTilePatch { .. } | VegetationMutation::FieldTileClear { .. } => {
            return None;
        }
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            (Some(point.id), VegetationTransitionKind::Planted)
        }
        VegetationMutation::Tombstone { plant } => {
            (Some(*plant), VegetationTransitionKind::Removed)
        }
        VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. } => {
            (Some(*plant), VegetationTransitionKind::Moved)
        }
        VegetationMutation::StateOverride { plant, .. }
        | VegetationMutation::PlantDeltaRestore { plant, .. } => {
            (Some(*plant), VegetationTransitionKind::StateReplaced)
        }
        VegetationMutation::Damage { plant, amount, .. } => (
            Some(*plant),
            VegetationTransitionKind::Damaged {
                amount: *amount,
                health: settled(plant)
                    .and_then(|state| state.health)
                    .unwrap_or(UnitInterval::ZERO),
            },
        ),
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => (
            Some(*plant),
            VegetationTransitionKind::Wetted {
                moisture: *moisture,
                fuel: *fuel,
            },
        ),
        VegetationMutation::LifecycleTransition {
            plant, from, to, ..
        } => (
            Some(*plant),
            VegetationTransitionKind::LifecycleChanged {
                from: *from,
                to: *to,
            },
        ),
        VegetationMutation::Harvest { plant, phenotype } => (
            Some(*plant),
            VegetationTransitionKind::Harvested {
                phenotype: *phenotype,
            },
        ),
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => (
            Some(*plant),
            VegetationTransitionKind::Burned {
                phenotype: *phenotype,
                remaining_fuel: *remaining_fuel,
            },
        ),
        VegetationMutation::Ignite { plant } => (Some(*plant), VegetationTransitionKind::Ignited),
        VegetationMutation::Extinguish { plant } => {
            (Some(*plant), VegetationTransitionKind::Extinguished)
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ..
        } => (
            Some(*plant),
            VegetationTransitionKind::Regrew {
                lifecycle: *lifecycle,
                phenotype: *phenotype,
            },
        ),
        VegetationMutation::DisturbanceMask { categories, .. }
        | VegetationMutation::DisturbanceMaskClear { categories, .. } => (
            None,
            VegetationTransitionKind::Disturbed {
                categories: *categories,
            },
        ),
    };
    Some(VegetationTransition {
        transaction: record.header.transaction,
        cell,
        plant,
        kind,
    })
}
