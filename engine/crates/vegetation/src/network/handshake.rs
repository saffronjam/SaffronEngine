//! The exact base-manifest handshake a vegetation session opens with.

use crate::{ContentHash, CookVersionSet, VegetationBaseManifest, VegetationStateBinding};
use crate::{Result, network::VEGETATION_NETWORK_PROTOCOL_VERSION};

/// What an authority offers a joining peer: the world it is simulating, exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseManifestOffer {
    /// Wire contract this authority speaks.
    pub protocol_version: u32,
    /// The immutable generation, graph, version set, and seed namespaces the session runs on.
    pub binding: VegetationStateBinding,
    /// Identity of the 25-column macro-point schema the manifest was cooked against.
    pub point_schema_identity: ContentHash,
}

impl BaseManifestOffer {
    /// The offer an authority makes for one cooked base manifest.
    ///
    /// # Errors
    ///
    /// A vegetation error when the manifest does not encode its identity.
    pub fn for_manifest(manifest: &VegetationBaseManifest) -> Result<Self> {
        Ok(Self {
            protocol_version: VEGETATION_NETWORK_PROTOCOL_VERSION,
            binding: VegetationStateBinding::from_manifest(manifest)?,
            point_schema_identity: ContentHash::new(crate::point_schema_hash()),
        })
    }
}

/// Exactly why a peer refused an offer.
///
/// Each variant is a hard stop. There is no negotiation and no partial acceptance: a peer whose
/// cooked base differs in any of these dimensions would reduce the same mutation to a different
/// world, and a session that ran anyway would diverge silently instead of failing at join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestHandshakeRejection {
    #[error("peer speaks vegetation network protocol {local}, authority offered {offered}")]
    ProtocolVersion { offered: u32, local: u32 },
    #[error("peer is bound to a different immutable vegetation manifest")]
    ManifestIdentity,
    #[error("peer compiled a different canonical cook graph")]
    CookGraphIdentity,
    #[error("peer runs a different deterministic version set")]
    Versions {
        offered: CookVersionSet,
        local: CookVersionSet,
    },
    #[error("peer declares different named seed namespaces")]
    SeedNamespaces,
    #[error("peer cooked against a different macro-point schema")]
    PointSchema,
}

/// What a peer holds locally when it evaluates an offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerBaseIdentity {
    pub protocol_version: u32,
    pub binding: VegetationStateBinding,
    pub point_schema_identity: ContentHash,
}

impl PeerBaseIdentity {
    /// The identity a peer derives from the manifest it loaded.
    ///
    /// # Errors
    ///
    /// A vegetation error when the manifest does not encode its identity.
    pub fn for_manifest(manifest: &VegetationBaseManifest) -> Result<Self> {
        let offer = BaseManifestOffer::for_manifest(manifest)?;
        Ok(Self {
            protocol_version: offer.protocol_version,
            binding: offer.binding,
            point_schema_identity: offer.point_schema_identity,
        })
    }

    /// Accepts an offer only on an exact match, naming the first dimension that differs.
    ///
    /// # Errors
    ///
    /// [`ManifestHandshakeRejection`] for the dimension that differs, checked in the order a
    /// reader can act on: protocol, then generation, then graph, then version set, then seeds,
    /// then point schema.
    pub fn accept(
        self,
        offer: BaseManifestOffer,
    ) -> std::result::Result<VegetationStateBinding, ManifestHandshakeRejection> {
        if offer.protocol_version != self.protocol_version {
            return Err(ManifestHandshakeRejection::ProtocolVersion {
                offered: offer.protocol_version,
                local: self.protocol_version,
            });
        }
        if offer.binding.manifest_identity != self.binding.manifest_identity {
            return Err(ManifestHandshakeRejection::ManifestIdentity);
        }
        if offer.binding.cook_graph_identity != self.binding.cook_graph_identity {
            return Err(ManifestHandshakeRejection::CookGraphIdentity);
        }
        if offer.binding.versions != self.binding.versions {
            return Err(ManifestHandshakeRejection::Versions {
                offered: offer.binding.versions,
                local: self.binding.versions,
            });
        }
        if offer.binding.seed_namespaces_identity != self.binding.seed_namespaces_identity {
            return Err(ManifestHandshakeRejection::SeedNamespaces);
        }
        if offer.point_schema_identity != self.point_schema_identity {
            return Err(ManifestHandshakeRejection::PointSchema);
        }
        Ok(offer.binding)
    }
}
