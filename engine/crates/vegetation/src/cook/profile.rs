//! The semantic versions and platform profile every cook identity folds in.

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{Error, Result};

use super::ContentHash;

/// Semantic versions that participate in every cook identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CookVersionSet {
    /// Cook-graph and artifact schema version.
    pub schema: u32,
    /// Source-normalization/compiler semantic version.
    pub compiler: u32,
    /// Biome evaluator semantic version.
    pub evaluator: u32,
    /// Authoritative numeric contract version.
    pub numeric: u32,
    /// Persistent simulation/save compatibility version.
    pub simulation: u32,
}

impl CookVersionSet {
    /// Returns the exact semantic contract set produced by this build.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            schema: 2,
            compiler: 5,
            evaluator: 5,
            numeric: 1,
            simulation: 1,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if [
            self.schema,
            self.compiler,
            self.evaluator,
            self.numeric,
            self.simulation,
        ]
        .contains(&0)
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "versions".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) {
        writer.u32(self.schema);
        writer.u32(self.compiler);
        writer.u32(self.evaluator);
        writer.u32(self.numeric);
        writer.u32(self.simulation);
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let value = Self {
            schema: reader.u32()?,
            compiler: reader.u32()?,
            evaluator: reader.u32()?,
            numeric: reader.u32()?,
            simulation: reader.u32()?,
        };
        value.validate()?;
        Ok(value)
    }
}

/// Complete platform profile that can affect derived artifact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookPlatformProfile {
    /// Rust target triple.
    pub target: String,
    /// Logical content profile, such as `portable-vulkan`.
    pub content_profile: String,
    /// Exact compiler/toolchain identity.
    pub toolchain: String,
    /// Canonical feature vocabulary selected for the artifact.
    pub features: Vec<String>,
}

impl CookPlatformProfile {
    /// Validates and returns the canonical profile identity.
    pub fn identity(&self) -> Result<ContentHash> {
        let mut writer = BinaryWriter::new();
        self.encode(&mut writer)?;
        Ok(ContentHash::of(&writer.finish()))
    }

    pub(crate) fn encode(&self, writer: &mut BinaryWriter) -> Result<()> {
        if self.target.is_empty() || self.content_profile.is_empty() || self.toolchain.is_empty() {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "platformProfile".to_owned(),
            });
        }
        let mut features = self.features.clone();
        features.sort_unstable();
        features.dedup();
        if features.iter().any(String::is_empty) {
            return Err(Error::ArtifactFormat {
                format: "vegetation cook",
                field: "platformProfile.features".to_owned(),
            });
        }
        writer.string(&self.target)?;
        writer.string(&self.content_profile)?;
        writer.string(&self.toolchain)?;
        writer.length(features.len())?;
        for feature in features {
            writer.string(&feature)?;
        }
        Ok(())
    }

    pub(crate) fn decode(reader: &mut BinaryReader<'_>) -> Result<Self> {
        let target = reader.string()?;
        let content_profile = reader.string()?;
        let toolchain = reader.string()?;
        let count = reader.count(8)?;
        let mut features = Vec::with_capacity(count);
        for _ in 0..count {
            features.push(reader.string()?);
        }
        let profile = Self {
            target,
            content_profile,
            toolchain,
            features,
        };
        profile.identity()?;
        Ok(profile)
    }
}
