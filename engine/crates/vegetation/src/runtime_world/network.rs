//! Seating this world in a network session, and keeping it verifiably in step.

use crate::{
    BaseManifestOffer, CellInterestSet, CheckpointReconciliation, Error, LateJoinGrant,
    LateJoinRequest, PeerBaseIdentity, Result, VegetationCheckpoint,
};

use super::VegetationWorld;

impl VegetationWorld {
    /// What this world offers a joining peer.
    ///
    /// # Errors
    ///
    /// A vegetation error when the bound manifest does not encode its identity.
    pub fn network_offer(&self) -> Result<BaseManifestOffer> {
        BaseManifestOffer::for_manifest(&self.manifest)
    }

    /// What this world compares an incoming offer against.
    ///
    /// # Errors
    ///
    /// A vegetation error when the bound manifest does not encode its identity.
    pub fn network_peer_identity(&self) -> Result<PeerBaseIdentity> {
        PeerBaseIdentity::for_manifest(&self.manifest)
    }

    /// The declaration this world is seated with, absent while it is in no session.
    #[must_use]
    pub const fn network_interest(&self) -> Option<&CellInterestSet> {
        self.network_interest.as_ref()
    }

    /// Replaces the seated declaration, or leaves the session when given `None`.
    ///
    /// Widening does not backfill: cells the world did not hold at the last grant stay absent
    /// until an authority sends them, which is what the next checkpoint comparison reports.
    pub fn declare_network_interest(&mut self, interest: Option<CellInterestSet>) {
        self.network_interest = interest;
    }

    /// Fingerprints the seated scope at the highest accepted sequence.
    ///
    /// # Errors
    ///
    /// [`Error::Network`] when this world is in no session, and any error the scoped snapshot or
    /// its fingerprint raises.
    pub fn network_checkpoint(&self) -> Result<VegetationCheckpoint> {
        let interest = self.network_interest.as_ref().ok_or_else(|| {
            Error::Network("this world is not seated in a network session".to_owned())
        })?;
        VegetationCheckpoint::of(&self.persistent, interest, self.network_sequence)
    }

    /// Seats one peer against this world's current state, as the authority.
    ///
    /// The grant consumes a stream position rather than reusing the last one: a seating envelope
    /// is an ordinary envelope, so it has to be distinguishable from the retransmission of
    /// whatever preceded it.
    ///
    /// # Errors
    ///
    /// [`Error::ManifestMismatch`] when the request is bound to a different immutable base,
    /// [`Error::NumericOverflow`] when the stream has run out of sequence numbers, and any error
    /// the scoped snapshot raises.
    pub fn issue_late_join(&mut self, request: &LateJoinRequest) -> Result<LateJoinGrant> {
        let sequence = self
            .network_sequence
            .checked_add(1)
            .ok_or(Error::NumericOverflow)?;
        let grant =
            LateJoinGrant::issue(request, &self.persistent, self.state_binding()?, sequence)?;
        self.network_sequence = sequence;
        Ok(grant)
    }

    /// Takes a grant, as the joining peer: adopts its scope, replaces state through the one
    /// receive path, and proves the result reproduces the authority's fingerprint.
    ///
    /// # Errors
    ///
    /// [`Error::ManifestMismatch`] when the grant is bound to a different immutable base,
    /// [`Error::Network`] when the seated state does not reproduce the granted checkpoint, and
    /// any reducer error the envelope raises.
    pub fn accept_late_join(&mut self, grant: &LateJoinGrant) -> Result<()> {
        if grant.binding != self.state_binding()? {
            return Err(Error::ManifestMismatch);
        }
        self.network_interest = Some(grant.interest.clone());
        self.network_sequence = 0;
        self.receive_network_envelope(&grant.envelope)?;
        match self.network_checkpoint()?.reconcile(grant.checkpoint) {
            CheckpointReconciliation::InSync => Ok(()),
            other => Err(Error::Network(format!(
                "late join did not reproduce the authority checkpoint: {other:?}"
            ))),
        }
    }
}
