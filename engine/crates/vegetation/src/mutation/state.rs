//! Complete persistent vegetation state bound to one immutable cooked-base manifest.

use std::collections::BTreeMap;

use saffron_spatial::WorldCellKey;

use crate::Result;

use super::VegetationCellState;

/// Complete persistent vegetation state bound to one immutable cooked-base manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationState {
    pub(super) manifest_identity: [u8; 32],
    pub(super) cells: BTreeMap<WorldCellKey, VegetationCellState>,
    pub(super) applied_transactions: BTreeMap<u128, [u8; 32]>,
    /// How far biology has advanced, under which rule set, with the boundary summaries a
    /// dependency-region catch-up reads.
    ecology: crate::EcologyState,
}

impl VegetationState {
    /// Creates empty persistent state for an exact base manifest.
    #[must_use]
    pub fn new(manifest_identity: [u8; 32]) -> Self {
        Self {
            manifest_identity,
            cells: BTreeMap::new(),
            applied_transactions: BTreeMap::new(),
            ecology: crate::EcologyState::new(),
        }
    }

    /// Exact immutable base-manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> [u8; 32] {
        self.manifest_identity
    }

    /// Cell states in canonical key order.
    #[must_use]
    pub fn cells(&self) -> &BTreeMap<WorldCellKey, VegetationCellState> {
        &self.cells
    }

    pub(crate) fn applied_transactions(&self) -> &BTreeMap<u128, [u8; 32]> {
        &self.applied_transactions
    }

    pub(crate) fn from_canonical_parts(
        manifest_identity: [u8; 32],
        cells: BTreeMap<WorldCellKey, VegetationCellState>,
        applied_transactions: BTreeMap<u128, [u8; 32]>,
        ecology: crate::EcologyState,
    ) -> Self {
        Self {
            manifest_identity,
            cells,
            applied_transactions,
            ecology,
        }
    }

    /// The persisted ecology state.
    #[must_use]
    pub const fn ecology(&self) -> &crate::EcologyState {
        &self.ecology
    }

    /// The persisted ecology state, for the simulation that advances it.
    pub const fn ecology_mut(&mut self) -> &mut crate::EcologyState {
        &mut self.ecology
    }

    /// Writes the canonical, interruption-detecting snapshot container.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        crate::state_codec::encode_state(self)
    }

    /// Binds this state to a different immutable base, carrying every delta across.
    ///
    /// A recook publishes a new base and re-keys the generation, but it does not re-key the world:
    /// a plant's identity derives from authoring ancestry rather than from the cook that placed it,
    /// and a delta is addressed by plant and cell. So nothing is dropped here — a delta the new
    /// base has no plant or no cell for is inert until, and unless, that ground comes back, exactly
    /// like the tombstone of a plant that is already gone. Deciding for the author which of their
    /// accumulated world is worth keeping is how a recook silently destroys it.
    ///
    /// # Errors
    ///
    /// A vegetation error when the manifest does not encode.
    pub fn rebase(&self, manifest: &crate::VegetationBaseManifest) -> Result<Self> {
        Ok(Self {
            manifest_identity: manifest.identity()?.bytes(),
            cells: self.cells.clone(),
            applied_transactions: self.applied_transactions.clone(),
            ecology: self.ecology.clone(),
        })
    }
}
