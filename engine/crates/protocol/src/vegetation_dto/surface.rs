use crate::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Two-sided normal behavior for a thin foliage sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ThinSheetNormalBehaviorDto {
    Preserve,
    FaceForwardBack,
    Symmetric,
}

/// Canonical alpha/coverage source shared by raster, voxel, and ray tracing derivations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CoverageSourceDto {
    AlbedoAlpha,
    Texture { texture: Uuid },
    ModeledGeometry,
}

/// Conservative classification of the canonical coverage source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AlphaClassificationDto {
    Opaque,
    Masked,
    Transmissive,
}

/// Coverage-preserving mip derivation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CoverageMipMetadataDto {
    pub reference_cutoff: u16,
    pub source_extent: [u32; 2],
    pub spatial_hash_salt: String,
    pub classification: AlphaClassificationDto,
    pub mip_hashes: Vec<String>,
}

/// Aggregate voxel material moments derived from canonical coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VoxelMaterialMomentsDto {
    pub occupancy: u16,
    pub albedo_mean_bits: [i32; 3],
    pub roughness_mean: u16,
    pub transmission_mean_bits: [i32; 3],
    pub thickness_mean_bits: i32,
    pub normal_second_moments_bits: [i32; 6],
}

/// Optional opacity-micromap derivation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct OpacityMicromapDerivationDto {
    pub enabled: bool,
    pub max_subdivision: u8,
    pub transparent_threshold: u16,
    pub opaque_threshold: u16,
}

/// Complete energy-conserving thin-sheet foliage response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ThinSheetFoliageParametersDto {
    pub front_albedo_response: u16,
    pub back_albedo_response: u16,
    pub thickness_bits: i32,
    pub absorption_color_bits: [i32; 3],
    pub transmission_color_bits: [i32; 3],
    pub roughness: u16,
    pub normal_behavior: ThinSheetNormalBehaviorDto,
    pub coverage_source: CoverageSourceDto,
    pub coverage: CoverageMipMetadataDto,
    pub voxel_moments: VoxelMaterialMomentsDto,
    pub opacity_micromap: OpacityMicromapDerivationDto,
    pub energy_limit: u16,
}

/// Exactly one material surface family and its complete typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "model",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum MaterialSurfaceDto {
    Standard,
    ThinSheetFoliage {
        parameters: ThinSheetFoliageParametersDto,
    },
}
