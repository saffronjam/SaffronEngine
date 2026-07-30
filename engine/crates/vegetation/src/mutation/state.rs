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
}
