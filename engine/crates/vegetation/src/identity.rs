//! Stable opaque plant identity and namespace collision auditing.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use saffron_core::Uuid;
use saffron_spatial::WorldCellKey;

use crate::hash::sha256;
use crate::{Error, Result};

const ID_DOMAIN: &[u8] = b"saffron-anima/plant-id/v1\0";
const NAMESPACE_MASK: u8 = 0b1100_0000;
const PAYLOAD_MASK: u8 = 0b0011_1111;

/// The non-overlapping authority that minted a plant identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum PlantIdNamespace {
    /// Deterministically cooked procedural plant, bound to an exact base manifest.
    Procedural = 0,
    /// Explicit authored plant carrying a stored GUID.
    Explicit = 1,
    /// Runtime plant minted by the simulation/network authority.
    Runtime = 2,
}

/// A stable opaque 128-bit plant identity.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlantId([u8; 16]);

impl PlantId {
    /// Constructs a procedural identity from the complete canonical identity vocabulary.
    #[must_use]
    pub fn procedural(input: ProceduralPlantIdentity) -> Self {
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
        Self::from_payload(
            PlantIdNamespace::Procedural,
            digest[..16].try_into().unwrap(),
        )
    }

    /// Constructs an explicit authored identity from its stored 128-bit GUID payload.
    pub fn explicit(stored_guid: [u8; 16]) -> Result<Self> {
        if stored_guid == [0; 16] {
            return Err(Error::InvalidPlantId);
        }
        Ok(Self::from_payload(PlantIdNamespace::Explicit, stored_guid))
    }

    /// Constructs an authority-issued runtime identity from its 128-bit authority payload.
    pub fn runtime(authority_id: [u8; 16]) -> Result<Self> {
        if authority_id == [0; 16] {
            return Err(Error::InvalidPlantId);
        }
        Ok(Self::from_payload(PlantIdNamespace::Runtime, authority_id))
    }

    fn from_payload(namespace: PlantIdNamespace, mut payload: [u8; 16]) -> Self {
        payload[0] = (payload[0] & PAYLOAD_MASK) | ((namespace as u8) << 6);
        Self(payload)
    }

    /// Returns the identity namespace encoded in the high two bits.
    pub fn namespace(self) -> Result<PlantIdNamespace> {
        match (self.0[0] & NAMESPACE_MASK) >> 6 {
            0 => Ok(PlantIdNamespace::Procedural),
            1 => Ok(PlantIdNamespace::Explicit),
            2 => Ok(PlantIdNamespace::Runtime),
            _ => Err(Error::InvalidPlantId),
        }
    }

    /// The canonical 16-byte representation.
    #[must_use]
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    /// Rebuilds an identity from its canonical bytes and validates the namespace tag.
    pub fn from_canonical_bytes(bytes: [u8; 16]) -> Result<Self> {
        let id = Self(bytes);
        id.namespace()?;
        Ok(id)
    }

    /// The canonical lowercase 32-digit hexadecimal representation.
    #[must_use]
    pub fn canonical_hex(self) -> String {
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            use std::fmt::Write as _;
            write!(&mut text, "{byte:02x}").unwrap();
        }
        text
    }
}

impl fmt::Debug for PlantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlantId")
            .field(&self.to_string())
            .finish()
    }
}

impl fmt::Display for PlantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical_hex())
    }
}

impl FromStr for PlantId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InvalidPlantId);
        }
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(Error::InvalidPlantId);
        }
        let mut bytes = [0_u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| Error::InvalidPlantId)?;
        }
        let id = Self(bytes);
        id.namespace()?;
        Ok(id)
    }
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
        let id = PlantId::procedural(input());
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

        let retained_before = PlantId::procedural(retained);
        assert_ne!(
            PlantId::procedural(unrelated_before),
            PlantId::procedural(unrelated_after)
        );
        assert_eq!(retained_before, PlantId::procedural(retained));
    }

    #[test]
    fn render_origin_rebasing_does_not_change_plant_identity() {
        let world = WorldPosition::from_global_ticks([1_000_000, -250_000, 3_000_000]).unwrap();
        let origins = [
            WorldPosition::origin(),
            WorldPosition::from_global_ticks([750_000, -500_000, 2_500_000]).unwrap(),
        ];
        let expected = PlantId::procedural(ProceduralPlantIdentity {
            owner: world.cell(),
            ..input()
        });

        for origin in origins {
            let relative = world.to_render_relative(origin).unwrap();
            let reconstructed = WorldPosition::from_render_relative(relative, origin).unwrap();
            assert_eq!(reconstructed, world);
            assert_eq!(
                PlantId::procedural(ProceduralPlantIdentity {
                    owner: reconstructed.cell(),
                    ..input()
                }),
                expected
            );
        }
    }

    #[test]
    fn collision_table_rejects_every_duplicate() {
        let id = PlantId::procedural(input());
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
