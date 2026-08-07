//! Periodic checkpoint hashes: how a peer discovers it has diverged, and by how much.

use crate::binary::BinaryWriter;
use crate::{ContentHash, Result, VegetationState};

use super::CellInterestSet;

/// One agreed-state fingerprint at an exact transport sequence.
///
/// Both sides compute this over the *same* interest scope, so a peer holding a subset of the
/// world reconciles against the authority's projection of that subset rather than against a
/// world-wide digest it could never reproduce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCheckpoint {
    /// Transport sequence this fingerprint describes; zero is the state at join.
    pub sequence: u64,
    /// Completed ecology tick, so a divergence report separates biology from mutations.
    pub ecology_tick: u64,
    /// Exact immutable base-manifest identity.
    pub manifest_identity: [u8; 32],
    /// Identity of the interest scope the fingerprint was taken over.
    pub interest_identity: ContentHash,
    /// Fingerprint of the scoped persistent state.
    pub state_identity: ContentHash,
}

impl VegetationCheckpoint {
    /// Fingerprints the part of `state` that `interest` covers, at `sequence`.
    ///
    /// # Errors
    ///
    /// A vegetation error when the scoped state does not encode canonically.
    pub fn of(state: &VegetationState, interest: &CellInterestSet, sequence: u64) -> Result<Self> {
        let scoped = state.scope_to_interest(interest)?;
        let mut preimage = BinaryWriter::new();
        preimage.bytes(b"saffron-anima/vegetation-network/checkpoint/v1\0");
        preimage.u64(sequence);
        preimage.bytes(&scoped.canonical_bytes()?);
        Ok(Self {
            sequence,
            ecology_tick: scoped.ecology().clock().tick(),
            manifest_identity: scoped.manifest_identity(),
            interest_identity: interest.identity()?,
            state_identity: ContentHash::of(&preimage.finish()),
        })
    }

    /// Compares a received authority checkpoint against this local one.
    #[must_use]
    pub fn reconcile(self, authority: Self) -> CheckpointReconciliation {
        if self.manifest_identity != authority.manifest_identity {
            return CheckpointReconciliation::ManifestMismatch;
        }
        if self.interest_identity != authority.interest_identity {
            return CheckpointReconciliation::InterestMismatch;
        }
        match self.sequence.cmp(&authority.sequence) {
            std::cmp::Ordering::Less => CheckpointReconciliation::Behind {
                by: authority.sequence - self.sequence,
            },
            std::cmp::Ordering::Greater => CheckpointReconciliation::Ahead {
                by: self.sequence - authority.sequence,
            },
            std::cmp::Ordering::Equal if self.state_identity == authority.state_identity => {
                CheckpointReconciliation::InSync
            }
            std::cmp::Ordering::Equal => CheckpointReconciliation::Diverged,
        }
    }
}

/// What a checkpoint comparison concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointReconciliation {
    /// Same sequence, same scope, same bytes.
    InSync,
    /// Same scope, same sequence, different bytes: only an authoritative snapshot recovers.
    Diverged,
    /// The peer has not yet applied this many operations.
    Behind { by: u64 },
    /// The peer applied operations the authority has not acknowledged.
    Ahead { by: u64 },
    /// The two sides scoped over different interest declarations, so the digests are not
    /// comparable at all.
    InterestMismatch,
    /// The two sides are bound to different immutable base manifests.
    ManifestMismatch,
}

impl CheckpointReconciliation {
    /// Whether the peer must be corrected with an authoritative snapshot rather than operations.
    #[must_use]
    pub const fn requires_snapshot(self) -> bool {
        matches!(
            self,
            Self::Diverged | Self::Ahead { .. } | Self::InterestMismatch | Self::ManifestMismatch
        )
    }
}
