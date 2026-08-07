//! Late join: what a peer asks for, and the one grant that seats it in the stream.

use crate::{
    ContentHash, NetworkMutationEnvelope, Result, VegetationState, VegetationStateBinding,
};

use super::{
    BaseManifestOffer, CellInterestSet, ManifestHandshakeRejection, PeerBaseIdentity,
    VEGETATION_NETWORK_PROTOCOL_VERSION, VegetationCheckpoint,
};

/// What a joining peer sends after accepting an offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LateJoinRequest {
    /// Wire contract the peer speaks.
    pub protocol_version: u32,
    /// The binding the peer accepted, echoed so the authority can refuse a stale accept.
    pub binding: VegetationStateBinding,
    /// The cells and facets the peer wants seated with.
    pub interest: CellInterestSet,
}

impl LateJoinRequest {
    /// The request a peer forms from an offer it has already accepted.
    ///
    /// # Errors
    ///
    /// [`ManifestHandshakeRejection`] when the offer does not match the peer's own base exactly.
    pub fn accept(
        peer: PeerBaseIdentity,
        offer: BaseManifestOffer,
        interest: CellInterestSet,
    ) -> std::result::Result<Self, ManifestHandshakeRejection> {
        Ok(Self {
            protocol_version: VEGETATION_NETWORK_PROTOCOL_VERSION,
            binding: peer.accept(offer)?,
            interest,
        })
    }
}

/// Everything a late joiner needs to be exactly in sync at one sequence.
///
/// The snapshot travels inside a [`NetworkMutationEnvelope`] rather than beside one, so a joining
/// peer and a peer being corrected after divergence take the identical receive path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LateJoinGrant {
    /// The binding the session runs on.
    pub binding: VegetationStateBinding,
    /// The interest the authority seated this peer with. It echoes the request unless the
    /// authority narrowed it, and it is the scope every later checkpoint is taken over.
    pub interest: CellInterestSet,
    /// The seating envelope: the scoped authoritative snapshot at `checkpoint.sequence`.
    pub envelope: NetworkMutationEnvelope,
    /// The fingerprint the peer must reproduce once it has applied the envelope.
    pub checkpoint: VegetationCheckpoint,
}

impl LateJoinGrant {
    /// Seats one peer against the authority's current state at `sequence`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::ManifestMismatch`] when the request is bound to a different immutable base,
    /// and any error the scoped snapshot or its fingerprint raises.
    pub fn issue(
        request: &LateJoinRequest,
        authority: &VegetationState,
        binding: VegetationStateBinding,
        sequence: u64,
    ) -> Result<Self> {
        if request.protocol_version != VEGETATION_NETWORK_PROTOCOL_VERSION
            || request.binding.manifest_identity != binding.manifest_identity
            || request.binding.cook_graph_identity != binding.cook_graph_identity
            || request.binding.versions != binding.versions
            || request.binding.seed_namespaces_identity != binding.seed_namespaces_identity
            || authority.manifest_identity() != binding.manifest_identity.bytes()
        {
            return Err(crate::Error::ManifestMismatch);
        }
        let snapshot = authority.scope_to_interest(&request.interest)?;
        Ok(Self {
            binding,
            interest: request.interest.clone(),
            envelope: NetworkMutationEnvelope {
                sequence,
                manifest_identity: snapshot.manifest_identity(),
                operations: Vec::new(),
                snapshot: Some(snapshot),
            },
            checkpoint: VegetationCheckpoint::of(authority, &request.interest, sequence)?,
        })
    }

    /// Identity of the scope this grant seats the peer with.
    ///
    /// # Errors
    ///
    /// [`crate::Error::NumericOverflow`] when the declaration exceeds the encodable length.
    pub fn interest_identity(&self) -> Result<ContentHash> {
        self.interest.identity()
    }
}
