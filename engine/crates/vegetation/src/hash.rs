//! Pinned SHA-256 used for vegetation identities and schema/manifests.

use sha2::{Digest, Sha256};

use crate::canonical::CanonicalSink;
use crate::{Error, Result};

/// Checked streaming SHA-256 state for canonical vegetation content.
#[derive(Clone)]
pub struct VegetationContentHasher {
    inner: Sha256,
    total_bytes: u64,
}

impl VegetationContentHasher {
    /// Creates an empty FIPS 180-4 SHA-256 stream.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Sha256::new(),
            total_bytes: 0,
        }
    }

    /// Adds one bounded byte fragment to the digest stream.
    pub fn update(&mut self, bytes: &[u8]) -> Result<()> {
        let additional = u64::try_from(bytes.len()).map_err(|_| Error::NumericOverflow)?;
        self.total_bytes = self
            .total_bytes
            .checked_add(additional)
            .filter(|length| *length <= u64::MAX / 8)
            .ok_or(Error::NumericOverflow)?;
        self.inner.update(bytes);
        Ok(())
    }

    /// Finishes the stream and returns the exact SHA-256 digest.
    pub fn finalize(self) -> Result<[u8; 32]> {
        Ok(self.inner.finalize().into())
    }
}

impl Default for VegetationContentHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl CanonicalSink for VegetationContentHasher {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.update(bytes)
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::{VegetationContentHasher, sha256};

    #[test]
    fn matches_fips_empty_vector() {
        assert_eq!(
            sha256(b""),
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ]
        );
    }

    #[test]
    fn matches_fips_abc_vector() {
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn fragmented_updates_match_block_boundaries() {
        for length in [55_usize, 56, 63, 64, 65] {
            let bytes = (0_u8..=u8::MAX).cycle().take(length).collect::<Vec<_>>();
            let expected = sha256(&bytes);
            for fragment_length in [1_usize, 7, 13, 64] {
                let mut hasher = VegetationContentHasher::new();
                for fragment in bytes.chunks(fragment_length) {
                    hasher.update(fragment).unwrap();
                }
                assert_eq!(hasher.finalize().unwrap(), expected);
            }
        }
    }
}
