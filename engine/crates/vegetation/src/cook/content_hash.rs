//! The exact SHA-256 identity of canonical vegetation content.

use std::fmt;
use std::str::FromStr;

use crate::{Error, Result, vegetation_content_hash};

/// Exact SHA-256 identity of canonical vegetation content.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Wraps an exact SHA-256 digest.
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Hashes canonical bytes with the vegetation SHA-256 implementation.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(vegetation_content_hash(bytes))
    }

    /// Returns the exact digest bytes.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Whether this is the reserved missing identity.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl From<[u8; 32]> for ContentHash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

impl From<ContentHash> for [u8; 32] {
    fn from(value: ContentHash) -> Self {
        value.0
    }
}

impl FromStr for ContentHash {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            });
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let text = std::str::from_utf8(pair).map_err(|_| Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            })?;
            bytes[index] = u8::from_str_radix(text, 16).map_err(|_| Error::ArtifactFormat {
                format: "content hash",
                field: "hex".to_owned(),
            })?;
        }
        let hash = Self(bytes);
        if hash.to_string() != value {
            return Err(Error::ArtifactFormat {
                format: "content hash",
                field: "canonicalHex".to_owned(),
            });
        }
        Ok(hash)
    }
}
