//! Strict sectioned `.svegcell` and `.splantc` derived artifact containers.

mod cell;
mod container;
mod plant;
mod section;
mod texture;

pub use cell::{
    VegetationCellArtifactHeader, VegetationCellArtifactIndex, VegetationCellArtifactReader,
    VegetationCellSection, VegetationCellSectionDescriptor, VegetationCellSectionKind,
    write_vegetation_cell_artifact,
};
pub use container::{
    ArtifactSectionCodec, plant_compiled_artifact_schema_hash, vegetation_cell_artifact_schema_hash,
};
pub use plant::{
    PlantCompiledArtifactHeader, PlantCompiledArtifactIndex, PlantCompiledSection,
    PlantCompiledSectionDescriptor, PlantCompiledSectionKind, write_plant_compiled_artifact,
};
pub use texture::{
    PlantTextureContainer, PlantTextureFormat, read_plant_texture_container,
    write_plant_texture_container,
};

/// Current `.svegcell` container version.
pub const VEGETATION_CELL_ARTIFACT_VERSION: u32 = 1;
/// Current `.splantc` container version.
pub const PLANT_COMPILED_ARTIFACT_VERSION: u32 = 6;
/// Current `.svegcell` section payload version.
pub const VEGETATION_CELL_SECTION_VERSION: u32 = 1;
/// Current `.splantc` section payload version.
pub const PLANT_COMPILED_SECTION_VERSION: u32 = 1;
/// Current `.splantc` semantic part-table payload version.
pub const PLANT_PART_TABLE_SECTION_VERSION: u32 = 2;

/// Explicit stored and decoded byte budgets accepted while validating an artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactDecodeLimits {
    max_stored_section_bytes: u64,
    max_total_stored_bytes: u64,
    max_decoded_section_bytes: u64,
    max_total_decoded_bytes: u64,
}

impl ArtifactDecodeLimits {
    /// Creates one caller-owned validation and section-extraction budget.
    #[must_use]
    pub const fn new(
        max_stored_section_bytes: u64,
        max_total_stored_bytes: u64,
        max_decoded_section_bytes: u64,
        max_total_decoded_bytes: u64,
    ) -> Self {
        Self {
            max_stored_section_bytes,
            max_total_stored_bytes,
            max_decoded_section_bytes,
            max_total_decoded_bytes,
        }
    }
}

/// Project-wide artifact validation policy used by runtime and editor asset stores.
pub const VEGETATION_ARTIFACT_DECODE_LIMITS: ArtifactDecodeLimits = ArtifactDecodeLimits::new(
    16 * 1024 * 1024 * 1024,
    64 * 1024 * 1024 * 1024,
    16 * 1024 * 1024 * 1024,
    64 * 1024 * 1024 * 1024,
);
