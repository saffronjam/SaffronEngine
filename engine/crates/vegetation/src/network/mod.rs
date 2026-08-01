//! The vegetation session contract a networked authority and its peers agree on.
//!
//! Four values carry the whole agreement, and none of them names a transport: the exact base
//! manifest a session binds to, the per-cell facet interest a peer declares, the sequenced
//! envelope operations travel in, and the periodic checkpoint both sides fingerprint to notice
//! divergence. Only persistent macro state crosses — micro fields, wind, and bend are derived
//! from the immutable base plus that state, so a receiver reconstructs them locally rather than
//! receiving them.

mod checkpoint;
mod handshake;
mod interest;
mod session;

#[cfg(test)]
mod tests;

/// The vegetation session wire contract this build speaks.
///
/// Bumped whenever the handshake, interest, envelope, or checkpoint encoding changes meaning. A
/// mismatch is refused at join: there is no negotiation, because two peers reducing the same
/// operation under different contracts diverge silently instead of failing.
pub const VEGETATION_NETWORK_PROTOCOL_VERSION: u32 = 1;

pub use checkpoint::{CheckpointReconciliation, VegetationCheckpoint};
pub use handshake::{BaseManifestOffer, ManifestHandshakeRejection, PeerBaseIdentity};
pub use interest::{CellInterestKey, CellInterestSet};
pub use session::{LateJoinGrant, LateJoinRequest};
