//! Deterministic persistent vegetation mutations and their distinct transport envelopes.

mod encode;
mod envelope;
mod journal;
mod reduce;
mod state;
#[cfg(test)]
mod tests;
mod transition;
mod types;

pub(crate) use encode::{mutation_tag, record_order_key};
pub use envelope::{NetworkMutationEnvelope, SaveStateEnvelope, VegetationStateBinding};
pub use journal::EditorJournalEnvelope;
pub use reduce::{MutationReduction, reduce_mutations};
pub use state::VegetationState;
pub use transition::{VegetationTransition, VegetationTransitionKind};
pub use types::{
    DisturbanceTileKey, FieldTileKey, FieldTileState, MutationHeader, PlantPersistentState,
    PromotionOriginState, VegetationCellState, VegetationMutation, VegetationMutationRecord,
};
