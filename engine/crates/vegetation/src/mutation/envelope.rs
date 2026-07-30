//! Save, editor-journal, and network envelopes over the same reduced state.

use crate::{ContentHash, CookVersionSet, Error, Result};

use super::{VegetationMutationRecord, VegetationState, reduce_mutations};

/// Editor-only journal envelope retaining gesture grouping and exact inverse/preimage operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorJournalEnvelope {
    /// Editor gesture identity.
    pub gesture: u128,
    /// Forward records passed to the reducer.
    pub forward: Vec<VegetationMutationRecord>,
    /// Inverse records computed from captured preimages at gesture creation.
    pub inverse: Vec<VegetationMutationRecord>,
}

/// Exact compatibility identity required by a persistent vegetation state container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationStateBinding {
    /// Exact immutable world-manifest identity.
    pub manifest_identity: ContentHash,
    /// Exact canonical graph compiled into the manifest.
    pub cook_graph_identity: ContentHash,
    /// Complete schema/compiler/evaluator/numeric/simulation contract.
    pub versions: CookVersionSet,
    /// Canonical identity of every named seed namespace.
    pub seed_namespaces_identity: ContentHash,
}

/// Compact save envelope: canonical snapshot plus a bounded mutation tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveStateEnvelope {
    /// Exact immutable-generation and deterministic-simulation contract.
    pub binding: VegetationStateBinding,
    pub snapshot: VegetationState,
    /// Mutations after the snapshot boundary.
    pub tail: Vec<VegetationMutationRecord>,
}

impl SaveStateEnvelope {
    /// Reduces the tail over a clone of the snapshot.
    pub fn reduced_state(&self) -> Result<VegetationState> {
        let manifest_identity = self.binding.manifest_identity.bytes();
        if self.snapshot.manifest_identity != manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        let mut state = self.snapshot.clone();
        reduce_mutations(&mut state, manifest_identity, &self.tail)?;
        Ok(state)
    }

    /// Reduces the tail and returns a new envelope with an empty tail.
    pub fn compact(self) -> Result<Self> {
        let snapshot = self.reduced_state()?;
        Ok(Self {
            binding: self.binding,
            snapshot,
            tail: Vec::new(),
        })
    }
}

/// Future network envelope with transport sequence separate from simulation logical ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkMutationEnvelope {
    /// Monotonic transport sequence.
    pub sequence: u64,
    /// Exact manifest understood by sender and receiver.
    pub manifest_identity: [u8; 32],
    /// Sequenced idempotent operations.
    pub operations: Vec<VegetationMutationRecord>,
    /// Optional authoritative snapshot for joining or correction.
    pub snapshot: Option<VegetationState>,
}
