//! Canonical persistent-state containers for runtime vegetation.

mod frame;
mod save;
mod state;
mod values;

#[cfg(test)]
mod tests;

use crate::binary::BinaryWriter;
use crate::{
    ContentHash, Error, Result, SaveStateEnvelope, VegetationBaseManifest, VegetationState,
    VegetationStateBinding,
};

pub(crate) use state::encode_state;

const STATE_FORMAT: &str = "vegetation persistent state";
const STATE_MAGIC: &[u8; 8] = b"SVEGST01";
const STATE_COMMIT: &[u8; 8] = b"SVEGSC01";
const STATE_VERSION: u32 = 1;
const SAVE_FORMAT: &str = "vegetation save state";
const SAVE_MAGIC: &[u8; 8] = b"SVEGSV01";
const SAVE_COMMIT: &[u8; 8] = b"SVEGVC01";
const SAVE_VERSION: u32 = 2;

fn state_schema_identity() -> ContentHash {
    ContentHash::of(
        b"saffron-anima/vegetation-state/schema/v1/manifest+cells+field-tiles+plant-deltas+disturbance-masks+applied-transactions+ecology",
    )
}

fn save_schema_identity() -> ContentHash {
    ContentHash::of(
        b"saffron-anima/vegetation-save/schema/v2/manifest+graph+versions+seed-namespaces+framed-snapshot+ordered-mutation-tail",
    )
}

impl VegetationStateBinding {
    /// Builds the exact persistence compatibility identity for one immutable base manifest.
    pub fn from_manifest(manifest: &VegetationBaseManifest) -> Result<Self> {
        let manifest_identity = manifest.identity()?;
        let mut seeds = manifest.seed_namespaces.clone();
        seeds.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.namespace.cmp(&right.namespace))
        });
        let mut writer = BinaryWriter::new();
        writer.bytes(b"saffron-anima/vegetation-seed-namespaces/v1\0");
        writer.length(seeds.len())?;
        for seed in seeds {
            writer.string(&seed.name)?;
            writer.u128(seed.namespace);
        }
        Ok(Self {
            manifest_identity,
            cook_graph_identity: manifest.cook_graph_hash,
            versions: manifest.versions,
            seed_namespaces_identity: ContentHash::of(&writer.finish()),
        })
    }

    fn validate(self) -> Result<()> {
        if self.manifest_identity.is_zero()
            || self.cook_graph_identity.is_zero()
            || self.seed_namespaces_identity.is_zero()
        {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.identity".to_owned(),
            });
        }
        self.versions.validate()
    }

    fn require_exact(self, expected: Self) -> Result<()> {
        if self.manifest_identity != expected.manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        if self.cook_graph_identity != expected.cook_graph_identity {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.cookGraphIdentity".to_owned(),
            });
        }
        if self.versions != expected.versions {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.versions".to_owned(),
            });
        }
        if self.seed_namespaces_identity != expected.seed_namespaces_identity {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.seedNamespacesIdentity".to_owned(),
            });
        }
        Ok(())
    }
}

impl VegetationState {
    /// Strictly decodes a canonical snapshot for the expected immutable manifest.
    pub fn from_canonical_bytes(bytes: &[u8], expected_manifest: [u8; 32]) -> Result<Self> {
        state::decode_state(bytes, expected_manifest)
    }

    /// Decodes a canonical snapshot against the generation it names, whichever that is.
    ///
    /// Durable state outlives the generation it was recorded against — a recook publishes a new
    /// one — so a reader that has to decide whether to bind or rebase reads it this way and
    /// compares afterwards.
    ///
    /// # Errors
    ///
    /// A vegetation error when the container is malformed or non-canonical.
    pub fn from_canonical_bytes_as_declared(bytes: &[u8]) -> Result<Self> {
        state::decode_state(bytes, state::declared_manifest_identity(bytes)?)
    }
}

impl SaveStateEnvelope {
    /// Writes the canonical, interruption-detecting snapshot-plus-tail container.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        save::encode_save(self)
    }

    /// Strictly decodes a save only when every deterministic binding matches.
    pub fn from_canonical_bytes(bytes: &[u8], expected: VegetationStateBinding) -> Result<Self> {
        save::decode_save(bytes, expected)
    }
}
