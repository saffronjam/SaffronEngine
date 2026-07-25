//! Stable opaque plant identity and namespace collision auditing.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::WorldCellKey;

use crate::hash::sha256;
use crate::{Error, Result};

const ID_DOMAIN: &[u8] = b"saffron-anima/plant-id/v1\0";

/// A stable non-zero plant-family classification identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PlantTagId(u64);

impl PlantTagId {
    /// Constructs a validated plant-family tag identity.
    pub fn new(value: u64) -> Result<Self> {
        if value == 0 {
            return Err(Error::InvalidPlantTagId);
        }
        Ok(Self(value))
    }

    /// Returns the canonical numeric identity.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for PlantTagId {
    type Error = Error;

    fn try_from(value: u64) -> Result<Self> {
        Self::new(value)
    }
}

pub use saffron_spatial::{PlantId, PlantIdNamespace};

/// Derives a procedural plant identity from the complete canonical identity
/// vocabulary: the domain-separated SHA-256 of every determinism input, stamped with
/// the [`PlantIdNamespace::Procedural`] tag.
#[must_use]
pub fn derive_procedural_plant_id(input: ProceduralPlantIdentity) -> PlantId {
    let mut preimage = Vec::with_capacity(ID_DOMAIN.len() + 93);
    preimage.extend_from_slice(ID_DOMAIN);
    preimage.push(PlantIdNamespace::Procedural as u8);
    preimage.extend_from_slice(&input.map.value().to_be_bytes());
    preimage.extend_from_slice(&input.layer_guid.to_be_bytes());
    preimage.extend_from_slice(&input.node_address.to_be_bytes());
    preimage.extend_from_slice(&input.node_semantic_revision.to_be_bytes());
    preimage.extend_from_slice(&input.candidate.to_be_bytes());
    preimage.extend_from_slice(&input.ancestor.to_be_bytes());
    preimage.extend_from_slice(&input.seed_namespace.to_be_bytes());
    preimage.extend_from_slice(&input.owner.canonical_bytes());
    preimage.extend_from_slice(&input.family.value().to_be_bytes());
    let digest = sha256(&preimage);
    PlantId::from_payload(
        PlantIdNamespace::Procedural,
        digest[..16].try_into().unwrap(),
    )
}

/// Complete inputs to a deterministic cooked plant identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProceduralPlantIdentity {
    /// Vegetation-map identity.
    pub map: Uuid,
    /// Stable authored layer identity.
    pub layer_guid: u128,
    /// Fully qualified root-biome/module-path/node execution address.
    pub node_address: u128,
    /// Revision changed only when the node's semantic meaning changes.
    pub node_semantic_revision: u32,
    /// Stable candidate identity within the node/cell.
    pub candidate: u64,
    /// Ancestor candidate used by hierarchical refinement.
    pub ancestor: u64,
    /// Named seed namespace owned by the biome/module.
    pub seed_namespace: u128,
    /// Canonical owner cell.
    pub owner: WorldCellKey,
    /// Plant-family identity.
    pub family: Uuid,
}

/// A cooker-local hard collision audit over all accepted plant identities.
#[derive(Clone, Debug, Default)]
pub struct PlantIdCollisionTable {
    ids: BTreeMap<PlantId, [u8; 32]>,
}

impl PlantIdCollisionTable {
    /// Inserts one accepted identity and its full source fingerprint.
    pub fn insert(&mut self, id: PlantId, source_fingerprint: [u8; 32]) -> Result<()> {
        if self.ids.insert(id, source_fingerprint).is_some() {
            return Err(Error::DuplicatePlantId(id.to_string()));
        }
        Ok(())
    }

    /// Number of audited accepted identities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no identity has been audited.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use saffron_spatial::WorldPosition;

    use super::*;

    fn input() -> ProceduralPlantIdentity {
        ProceduralPlantIdentity {
            map: Uuid(17),
            layer_guid: 23,
            node_address: 29,
            node_semantic_revision: 3,
            candidate: 31,
            ancestor: 37,
            seed_namespace: 41,
            owner: WorldCellKey::base(-5, 7, -11),
            family: Uuid(43),
        }
    }

    #[test]
    fn procedural_id_golden_is_pinned_and_round_trips() {
        let id = derive_procedural_plant_id(input());
        assert_eq!(id.to_string(), "1d8b921f8252f69b6c1c88f9da096eea");
        assert_eq!(id.to_string().parse::<PlantId>().unwrap(), id);
        assert_eq!(id.namespace().unwrap(), PlantIdNamespace::Procedural);
    }

    #[test]
    fn namespaces_do_not_overlap() {
        let payload = [0x55; 16];
        let explicit = PlantId::explicit(payload).unwrap();
        let runtime = PlantId::runtime(payload).unwrap();
        assert_ne!(explicit, runtime);
        assert_eq!(explicit.namespace().unwrap(), PlantIdNamespace::Explicit);
        assert_eq!(runtime.namespace().unwrap(), PlantIdNamespace::Runtime);
    }

    #[test]
    fn plant_tag_identity_rejects_zero() {
        assert!(matches!(PlantTagId::new(0), Err(Error::InvalidPlantTagId)));
        assert_eq!(PlantTagId::new(41).unwrap().value(), 41);
    }

    #[test]
    fn unrelated_node_revision_does_not_change_retained_node_identity() {
        let retained = input();
        let unrelated_before = ProceduralPlantIdentity {
            node_address: 101,
            node_semantic_revision: 1,
            ..retained
        };
        let unrelated_after = ProceduralPlantIdentity {
            node_semantic_revision: 2,
            ..unrelated_before
        };

        let retained_before = derive_procedural_plant_id(retained);
        assert_ne!(
            derive_procedural_plant_id(unrelated_before),
            derive_procedural_plant_id(unrelated_after)
        );
        assert_eq!(retained_before, derive_procedural_plant_id(retained));
    }

    #[test]
    fn render_origin_rebasing_does_not_change_plant_identity() {
        let world = WorldPosition::from_global_ticks([1_000_000, -250_000, 3_000_000]).unwrap();
        let origins = [
            WorldPosition::origin(),
            WorldPosition::from_global_ticks([750_000, -500_000, 2_500_000]).unwrap(),
        ];
        let expected = derive_procedural_plant_id(ProceduralPlantIdentity {
            owner: world.cell(),
            ..input()
        });

        for origin in origins {
            let relative = world.to_render_relative(origin).unwrap();
            let reconstructed = WorldPosition::from_render_relative(relative, origin).unwrap();
            assert_eq!(reconstructed, world);
            assert_eq!(
                derive_procedural_plant_id(ProceduralPlantIdentity {
                    owner: reconstructed.cell(),
                    ..input()
                }),
                expected
            );
        }
    }

    #[test]
    fn collision_table_rejects_every_duplicate() {
        let id = derive_procedural_plant_id(input());
        let mut table = PlantIdCollisionTable::default();
        table.insert(id, [1; 32]).unwrap();
        assert!(matches!(
            table.insert(id, [1; 32]),
            Err(Error::DuplicatePlantId(_))
        ));
    }

    #[test]
    fn wire_form_rejects_uppercase_and_noncanonical_lengths() {
        assert!(
            "1D8B921F8252F69B6C1C88F9DA096EEA"
                .parse::<PlantId>()
                .is_err()
        );
        assert!("1b03".parse::<PlantId>().is_err());
    }
}
