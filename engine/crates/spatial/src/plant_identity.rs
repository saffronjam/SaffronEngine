//! Stable opaque plant identity: the world-crossing 128-bit id physics hit targets,
//! vegetation state, and the wire protocol all name a macro plant by.

use std::fmt;
use std::str::FromStr;

use crate::{Error, Result};

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

/// A stable opaque 128-bit plant identity. The high two bits of the first byte carry
/// the [`PlantIdNamespace`]; the derivation of each namespace's payload belongs to its
/// authority (the vegetation cooker for procedural ids).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlantId([u8; 16]);

impl PlantId {
    /// Decodes the canonical identity bytes and validates the encoded namespace.
    pub fn from_bytes(bytes: [u8; 16]) -> Result<Self> {
        Self::from_canonical_bytes(bytes)
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

    /// Stamps `namespace` over a derived 128-bit payload (a hash digest slice or an
    /// authority GUID); the payload's masked bits are preserved.
    #[must_use]
    pub fn from_payload(namespace: PlantIdNamespace, mut payload: [u8; 16]) -> Self {
        payload[0] = (payload[0] & PAYLOAD_MASK) | ((namespace as u8) << 6);
        Self(payload)
    }

    /// The identity namespace encoded in the high two bits.
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
        Self::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_round_trip_and_invalid_tags_reject() {
        let explicit = PlantId::explicit([7; 16]).expect("explicit id");
        assert_eq!(explicit.namespace().unwrap(), PlantIdNamespace::Explicit);
        let runtime = PlantId::runtime([9; 16]).expect("runtime id");
        assert_eq!(runtime.namespace().unwrap(), PlantIdNamespace::Runtime);
        let hex = runtime.canonical_hex();
        assert_eq!(hex.parse::<PlantId>().unwrap(), runtime);
        // Namespace tag 3 is unassigned.
        let mut bad = [0_u8; 16];
        bad[0] = 0b1100_0000;
        assert!(PlantId::from_bytes(bad).is_err());
    }
}
