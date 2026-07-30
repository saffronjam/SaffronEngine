//! One immutable cook dependency and the work accounting recorded beside it.

use saffron_spatial::{DecisionScalar, MAX_HIERARCHY_LEVEL, WorldBounds};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{Error, Result};

use super::{ContentHash, CookDependencyAddress};

/// Exact content and spatial support of one immutable cook dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookDependency {
    pub address: CookDependencyAddress,
    /// Exact canonical source identity.
    pub content_hash: ContentHash,
    /// Exact source coverage when spatially bounded.
    pub bounds: Option<WorldBounds>,
    /// Composed finite support read beyond the output bounds.
    pub halo: DecisionScalar,
    /// Ancestor level required by propagating/global work.
    pub ancestor_level: Option<u8>,
}

impl CookDependency {
    pub(crate) fn validate(&self) -> Result<()> {
        self.address.validate()?;
        if self.content_hash.is_zero()
            || self.halo.bits() < 0
            || self
                .ancestor_level
                .is_some_and(|level| level > MAX_HIERARCHY_LEVEL)
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook graph",
                field: "dependencies.contentOrSupport".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        self.address.encode(writer)?;
        writer.bytes(&self.content_hash.bytes());
        match self.bounds {
            Some(bounds) => {
                writer.bool(true);
                writer.bounds(bounds);
            }
            None => writer.bool(false),
        }
        writer.i32(self.halo.bits());
        match self.ancestor_level {
            Some(level) => {
                writer.bool(true);
                writer.u8(level);
            }
            None => writer.bool(false),
        }
        Ok(())
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        Ok(Self {
            address: CookDependencyAddress::decode(reader)?,
            content_hash: ContentHash::new(reader.array()?),
            bounds: if reader.bool()? {
                Some(reader.bounds()?)
            } else {
                None
            },
            halo: DecisionScalar::from_bits(reader.i32()?),
            ancestor_level: if reader.bool()? {
                Some(reader.u8()?)
            } else {
                None
            },
        })
    }
}

/// Predicted bounded work for one cook node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CookWorkEstimate {
    /// Stable abstract work units.
    pub work_units: u64,
    /// Peak resident bytes admitted before execution.
    pub peak_memory_bytes: u64,
    /// Canonical input bytes read.
    pub input_bytes: u64,
    /// Canonical output bytes expected.
    pub output_bytes: u64,
}

impl CookWorkEstimate {
    pub(crate) fn encode(&self, writer: &mut BinaryWriter) {
        writer.u64(self.work_units);
        writer.u64(self.peak_memory_bytes);
        writer.u64(self.input_bytes);
        writer.u64(self.output_bytes);
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        Ok(Self {
            work_units: reader.u64()?,
            peak_memory_bytes: reader.u64()?,
            input_bytes: reader.u64()?,
            output_bytes: reader.u64()?,
        })
    }
}

/// Measured execution and cache result for one cook node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CookWorkActual {
    /// Wall-clock execution duration.
    pub elapsed_micros: u64,
    /// Measured peak resident memory.
    pub peak_memory_bytes: u64,
    /// Actual canonical input bytes read.
    pub input_bytes: u64,
    /// Actual canonical output bytes published.
    pub output_bytes: u64,
    /// Typed rejection total produced by this node.
    pub rejection_count: u64,
    /// Whether the node was satisfied by an already validated artifact.
    pub cache_hit: bool,
}

pub(crate) fn canonical_dependencies(
    dependencies: &[CookDependency],
) -> Result<Vec<(Vec<u8>, CookDependency)>> {
    let mut dependencies = dependencies
        .iter()
        .cloned()
        .map(|dependency| {
            dependency.validate()?;
            Ok((dependency.address.canonical_bytes()?, dependency))
        })
        .collect::<Result<Vec<_>>>()?;
    dependencies.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if dependencies.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(Error::ArtifactFormat {
            format: "vegetation cook graph",
            field: "dependencies.duplicateAddress".to_owned(),
        });
    }
    Ok(dependencies)
}
