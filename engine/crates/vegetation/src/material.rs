//! Thin-sheet foliage and canonical coverage derivation contracts.

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval};

/// Material surface response family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceModel {
    /// Conventional opaque/masked/translucent PBR surface.
    #[default]
    Standard,
    /// Energy-conserving two-sided thin foliage sheet.
    ThinSheetFoliage,
}

/// Exactly one material surface response and its complete typed parameters.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MaterialSurface {
    /// Conventional PBR surface.
    #[default]
    Standard,
    /// Energy-conserving two-sided thin foliage sheet.
    ThinSheetFoliage(ThinSheetFoliageParameters),
}

impl MaterialSurface {
    /// Surface-model selector emitted in `.smat`.
    #[must_use]
    pub fn model(&self) -> SurfaceModel {
        match self {
            Self::Standard => SurfaceModel::Standard,
            Self::ThinSheetFoliage(_) => SurfaceModel::ThinSheetFoliage,
        }
    }

    /// Validates the selected response and its parameters as one inseparable union.
    pub fn validate(&self) -> crate::Result<()> {
        match self {
            Self::Standard => Ok(()),
            Self::ThinSheetFoliage(parameters) => parameters.validate(),
        }
    }
}

impl SurfaceModel {
    /// Canonical `.smat` wire spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::ThinSheetFoliage => "thin-sheet-foliage",
        }
    }

    /// Parses the canonical wire spelling.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "thin-sheet-foliage" => Some(Self::ThinSheetFoliage),
            _ => None,
        }
    }
}

/// How normals behave across the two faces of a thin leaf/frond.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThinSheetNormalBehavior {
    /// Preserve geometric/tangent normal orientation.
    Preserve,
    /// Flip the back face toward its observer while preserving tangent detail.
    #[default]
    FaceForwardBack,
    /// Use a symmetric two-sided lobe.
    Symmetric,
}

impl ThinSheetNormalBehavior {
    /// Canonical wire spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::FaceForwardBack => "face-forward-back",
            Self::Symmetric => "symmetric",
        }
    }

    /// Parses the canonical wire spelling.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "preserve" => Some(Self::Preserve),
            "face-forward-back" => Some(Self::FaceForwardBack),
            "symmetric" => Some(Self::Symmetric),
            _ => None,
        }
    }
}

/// Canonical alpha/coverage source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoverageSource {
    /// Alpha from the albedo/base-color source.
    #[default]
    AlbedoAlpha,
    /// Alpha from a dedicated catalog texture.
    Texture(Uuid),
    /// Fully modeled geometry has unit coverage.
    ModeledGeometry,
}

/// Conservative alpha classification used by raster, voxel, and RT derivations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaClassification {
    /// Every sample is fully covered.
    Opaque,
    /// Binary/stochastic coverage from the canonical alpha source.
    #[default]
    Masked,
    /// True partial transmission, sorted/weighted as transparent geometry.
    Transmissive,
}

/// Coverage-preserving mip derivation metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageMipMetadata {
    /// Alpha cutoff whose covered area is preserved across mips.
    pub reference_cutoff: UnitInterval,
    /// Source width/height used to anchor spatial hashing.
    pub source_extent: [u32; 2],
    /// Stable object-space hash salt.
    pub spatial_hash_salt: u64,
    /// Classification shared by depth/color/shadow/picking/RT.
    pub classification: AlphaClassification,
    /// SHA-256 of each canonical coverage mip, base first.
    pub mip_hashes: Vec<[u8; 32]>,
}

/// Aggregate voxel material moments derived from the same canonical coverage source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoxelMaterialMoments {
    /// Occupancy/coverage density.
    pub occupancy: UnitInterval,
    /// Mean albedo RGB in Q15.16.
    pub albedo_mean: [DecisionScalar; 3],
    /// Mean roughness.
    pub roughness_mean: UnitInterval,
    /// Mean transmitted energy RGB in Q15.16.
    pub transmission_mean: [DecisionScalar; 3],
    /// Mean thickness in Q15.16 metres.
    pub thickness_mean: DecisionScalar,
    /// Normal second moments in canonical XX/YY/ZZ/XY/XZ/YZ order.
    pub normal_second_moments: [DecisionScalar; 6],
}

/// Optional opacity-micromap derivation fields; correctness never depends on OMM support.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpacityMicromapDerivation {
    /// Allow an optional OMM artifact to be generated.
    pub enabled: bool,
    /// Maximum subdivision level.
    pub max_subdivision: u8,
    /// Alpha below this value is transparent.
    pub transparent_threshold: UnitInterval,
    /// Alpha above this value is opaque.
    pub opaque_threshold: UnitInterval,
}

/// Complete thin-sheet foliage material contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinSheetFoliageParameters {
    /// Front-face albedo response multiplier.
    pub front_albedo_response: UnitInterval,
    /// Back-face albedo response multiplier.
    pub back_albedo_response: UnitInterval,
    /// Physical sheet thickness in Q15.16 metres.
    pub thickness: DecisionScalar,
    /// Beer-Lambert absorption RGB.
    pub absorption_color: [DecisionScalar; 3],
    /// Transmitted-light tint RGB.
    pub transmission_color: [DecisionScalar; 3],
    /// Thin-sheet roughness.
    pub roughness: UnitInterval,
    /// Two-sided normal policy.
    pub normal_behavior: ThinSheetNormalBehavior,
    /// Canonical coverage source.
    pub coverage_source: CoverageSource,
    /// Coverage mip/hash/classification metadata.
    pub coverage: CoverageMipMetadata,
    /// Aggregate voxel material moments.
    pub voxel_moments: VoxelMaterialMoments,
    /// Optional OMM derivation metadata.
    pub opacity_micromap: OpacityMicromapDerivation,
    /// Maximum reflected + transmitted energy in canonical normalized units.
    pub energy_limit: UnitInterval,
}

impl Default for ThinSheetFoliageParameters {
    fn default() -> Self {
        Self {
            front_albedo_response: UnitInterval::from_bits(30_000),
            back_albedo_response: UnitInterval::from_bits(30_000),
            thickness: DecisionScalar::from_bits(655),
            absorption_color: [DecisionScalar::from_bits(0); 3],
            transmission_color: [DecisionScalar::from_bits(30_000); 3],
            roughness: UnitInterval::from_bits(32_768),
            normal_behavior: ThinSheetNormalBehavior::FaceForwardBack,
            coverage_source: CoverageSource::AlbedoAlpha,
            coverage: CoverageMipMetadata {
                reference_cutoff: UnitInterval::from_bits(32_768),
                source_extent: [1, 1],
                spatial_hash_salt: 1,
                classification: AlphaClassification::Masked,
                mip_hashes: Vec::new(),
            },
            voxel_moments: VoxelMaterialMoments::default(),
            opacity_micromap: OpacityMicromapDerivation::default(),
            energy_limit: UnitInterval::ONE,
        }
    }
}

impl ThinSheetFoliageParameters {
    /// Validates physical and derivation constraints.
    pub fn validate(&self) -> crate::Result<()> {
        let transmission = self
            .transmission_color
            .iter()
            .map(|value| value.bits())
            .max()
            .unwrap_or(0);
        let absorption_in_range = self
            .absorption_color
            .iter()
            .all(|value| (0..=65_535).contains(&value.bits()));
        let transmission_in_range = self
            .transmission_color
            .iter()
            .all(|value| (0..=65_535).contains(&value.bits()));
        let energy_limit = i64::from(self.energy_limit.bits());
        let front_energy = i64::from(self.front_albedo_response.bits()) + i64::from(transmission);
        let back_energy = i64::from(self.back_albedo_response.bits()) + i64::from(transmission);
        let expected_mips = u32::BITS
            - self
                .coverage
                .source_extent
                .into_iter()
                .max()
                .unwrap_or(1)
                .leading_zeros();
        if self.thickness.bits() <= 0
            || self.coverage.source_extent.contains(&0)
            || self.coverage.spatial_hash_salt == 0
            || self.opacity_micromap.transparent_threshold > self.opacity_micromap.opaque_threshold
            || !absorption_in_range
            || !transmission_in_range
            || front_energy > energy_limit
            || back_energy > energy_limit
            || (!self.coverage.mip_hashes.is_empty()
                && self.coverage.mip_hashes.len() != expected_mips as usize)
            || matches!(self.coverage_source, CoverageSource::Texture(id) if id.value() == 0)
        {
            return Err(crate::Error::InvalidFormat {
                format: ".smat",
                field: "thinSheetFoliage".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_model_wire_is_single_and_canonical() {
        assert_eq!(
            SurfaceModel::ThinSheetFoliage.as_wire(),
            "thin-sheet-foliage"
        );
        assert_eq!(
            SurfaceModel::from_wire("thin-sheet-foliage"),
            Some(SurfaceModel::ThinSheetFoliage)
        );
        assert_eq!(SurfaceModel::from_wire("unknown"), None);
    }

    #[test]
    fn omm_thresholds_cannot_invert() {
        let mut params = ThinSheetFoliageParameters::default();
        params.opacity_micromap.transparent_threshold = UnitInterval::ONE;
        params.opacity_micromap.opaque_threshold = UnitInterval::ZERO;
        assert!(params.validate().is_err());
    }
}
