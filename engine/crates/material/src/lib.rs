//! Material surface, thin-sheet, and canonical coverage value contracts.

#![deny(unsafe_code)]

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval};

/// Material validation failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Physical sheet thickness must be positive.
    #[error("thin-sheet thickness must be positive")]
    NonPositiveThickness,
    /// Coverage textures must have a nonzero extent.
    #[error("coverage source extent must be nonzero")]
    ZeroCoverageExtent,
    /// Spatial coverage hashing requires a nonzero salt.
    #[error("coverage spatial hash salt must be nonzero")]
    ZeroSpatialHashSalt,
    /// OMM thresholds must progress from transparent to opaque.
    #[error("opacity-micromap thresholds are inverted")]
    InvertedOpacityThresholds,
    /// Absorption channels must lie in canonical normalized Q15.16 range.
    #[error("thin-sheet absorption lies outside canonical normalized range")]
    AbsorptionOutOfRange,
    /// Transmission channels must lie in canonical normalized Q15.16 range.
    #[error("thin-sheet transmission lies outside canonical normalized range")]
    TransmissionOutOfRange,
    /// Reflected and transmitted energy exceeds the material limit.
    #[error("thin-sheet reflected and transmitted energy exceeds its limit")]
    EnergyLimitExceeded,
    /// Coverage mip hashes do not describe the complete source chain.
    #[error("coverage mip hash count is {actual}; expected {expected}")]
    CoverageMipCount {
        /// Exact count required by the source extent.
        expected: usize,
        /// Supplied hash count.
        actual: usize,
    },
    /// A dedicated coverage texture must carry a nonzero asset identity.
    #[error("dedicated coverage texture identity must be nonzero")]
    NullCoverageTexture,
}

/// Material validation result.
pub type Result<T> = std::result::Result<T, Error>;

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
    pub fn validate(&self) -> Result<()> {
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
#[repr(u32)]
pub enum AlphaClassification {
    /// Every sample is fully covered.
    Opaque = 0,
    /// Binary/stochastic coverage from the canonical alpha source.
    #[default]
    Masked = 1,
    /// True partial transmission, sorted/weighted as transparent geometry.
    Transmissive = 2,
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
    pub fn validate(&self) -> Result<()> {
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
        if self.thickness.bits() <= 0 {
            return Err(Error::NonPositiveThickness);
        }
        if self.coverage.source_extent.contains(&0) {
            return Err(Error::ZeroCoverageExtent);
        }
        if self.coverage.spatial_hash_salt == 0 {
            return Err(Error::ZeroSpatialHashSalt);
        }
        if self.opacity_micromap.transparent_threshold > self.opacity_micromap.opaque_threshold {
            return Err(Error::InvertedOpacityThresholds);
        }
        if !absorption_in_range {
            return Err(Error::AbsorptionOutOfRange);
        }
        if !transmission_in_range {
            return Err(Error::TransmissionOutOfRange);
        }
        if front_energy > energy_limit || back_energy > energy_limit {
            return Err(Error::EnergyLimitExceeded);
        }
        if !self.coverage.mip_hashes.is_empty()
            && self.coverage.mip_hashes.len() != expected_mips as usize
        {
            return Err(Error::CoverageMipCount {
                expected: expected_mips as usize,
                actual: self.coverage.mip_hashes.len(),
            });
        }
        if matches!(self.coverage_source, CoverageSource::Texture(id) if id.value() == 0) {
            return Err(Error::NullCoverageTexture);
        }
        Ok(())
    }
}

/// The Beer–Lambert extinction coefficient every aggregate march applies, in reciprocal metres:
/// fully dense porous matter transmits about 5% of the light entering one metre of it.
///
/// One number shared by the cook that derives a voxel's occupancy and the shader that marches
/// through it (`sdfExtinctionStep` in `sdf.slang`). A voxel injected against one coefficient and
/// sampled against another would change brightness at the triangle↔voxel transition, which is
/// exactly the artifact the shared constant rules out.
pub const AGGREGATE_EXTINCTION_PER_METER: f32 = 3.0;

/// The energy that survives `distance_m` metres of aggregate matter at `occupancy` density — the
/// Rust counterpart of the shader's extinction step.
#[must_use]
pub fn aggregate_transmittance(occupancy: f32, distance_m: f32) -> f32 {
    (-AGGREGATE_EXTINCTION_PER_METER * occupancy.max(0.0) * distance_m.max(0.0)).exp()
}

/// The occupancy density an aggregate voxel needs so that marching through `thickness_m` of it
/// transmits `transmission` — the energy the triangle stack it replaces would have let through.
///
/// This is the inverse of [`aggregate_transmittance`], and it is what keeps a plant's indirect
/// irradiance, sky visibility, and reflection response continuous as it crosses from triangles to
/// its aggregate voxel: both sides describe the same optical depth rather than two authored
/// guesses. Opaque matter (`transmission <= 0`) is solid; a vanishing thickness is solid too,
/// since no finite density can absorb across no distance.
#[must_use]
pub fn parity_occupancy(transmission: f32, thickness_m: f32) -> f32 {
    let transmission = transmission.clamp(0.0, 1.0);
    let thickness = thickness_m.max(0.0);
    if transmission <= f32::EPSILON || thickness <= f32::EPSILON {
        return 1.0;
    }
    let optical_depth = -transmission.ln();
    (optical_depth / (AGGREGATE_EXTINCTION_PER_METER * thickness)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_classification_gpu_values_are_canonical() {
        assert_eq!(AlphaClassification::Opaque as u32, 0);
        assert_eq!(AlphaClassification::Masked as u32, 1);
        assert_eq!(AlphaClassification::Transmissive as u32, 2);
    }

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

    /// The transition invariant: a voxel whose occupancy came from the triangle stack's measured
    /// transmission transmits that same energy when marched. Without this, a plant changes
    /// brightness at the moment it becomes an aggregate.
    #[test]
    fn voxel_occupancy_round_trips_the_triangle_stack_transmission() {
        // Cascade voxel edges, finest to coarsest, against transmissions the coefficient can
        // express across them.
        for &extent in &[0.25_f32, 0.5, 1.0, 4.0] {
            for &transmission in &[0.9_f32, 0.6, 0.35, 0.1] {
                let occupancy = parity_occupancy(transmission, extent);
                // Anything that saturated is outside the representable range by construction.
                if occupancy >= 1.0 {
                    continue;
                }
                let marched = aggregate_transmittance(occupancy, extent);
                assert!(
                    (marched - transmission).abs() < 1.0e-3,
                    "transmission {transmission} across a {extent} m voxel marched as {marched} (occupancy {occupancy})"
                );
            }
        }
    }

    /// Matter too dense to describe at the shared coefficient saturates rather than reporting a
    /// density above one, and degenerate inputs read as solid.
    #[test]
    fn parity_occupancy_saturates_and_treats_degenerate_input_as_solid() {
        // 2% transmission across a 1 cm voxel needs far more extinction than the coefficient
        // allows, so it saturates solid rather than leaking light.
        assert_eq!(parity_occupancy(0.02, 0.01), 1.0);
        assert_eq!(parity_occupancy(0.0, 1.0), 1.0);
        assert_eq!(parity_occupancy(0.5, 0.0), 1.0);
        // Fully transmitting matter is empty.
        assert_eq!(parity_occupancy(1.0, 1.0), 0.0);
    }

    /// The Rust constant and the shader's `sdfExtinctionStep` must be the same number: a voxel
    /// injected against one coefficient and marched against another shifts brightness exactly at
    /// the transition this parity exists to hide.
    #[test]
    fn the_extinction_coefficient_matches_the_shader() {
        let source = include_str!("../../../assets/shaders/sdf.slang");
        let needle = format!("exp(-{AGGREGATE_EXTINCTION_PER_METER:.1} * max(occupancy, 0.0)");
        assert!(
            source.contains(&needle),
            "sdf.slang's extinction step does not use {AGGREGATE_EXTINCTION_PER_METER} per metre"
        );
    }
}
