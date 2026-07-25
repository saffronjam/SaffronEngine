//! CPU reference for the canonical raster and ray coverage decision.

use saffron_material::AlphaClassification;

use crate::CoverageSourceKind;

/// Result shared by alpha-to-coverage, blended, and stochastic coverage consumers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanonicalCoverageSample {
    /// Filtered coverage probability.
    pub alpha: f32,
    /// Whether the current point survives binary classification.
    pub covered: bool,
}

/// Returns the object-anchored stochastic threshold used by `coverage.slang`.
#[must_use]
pub fn spatial_coverage_threshold(
    anchor_position: [f32; 3],
    uv: [f32; 2],
    source_extent: [u32; 2],
    salt: u64,
    temporal_phase: u32,
) -> f32 {
    let object_cell = anchor_position.map(|value| (value * 4096.0).floor() as i32 as u32);
    let extent = source_extent.map(|value| value.max(1));
    let texel = [
        ((uv[0] - uv[0].floor()) * extent[0] as f32).floor() as i32 as u32,
        ((uv[1] - uv[1].floor()) * extent[1] as f32).floor() as i32 as u32,
    ];
    let mut hash = coverage_mix(object_cell[0] ^ salt as u32);
    hash = coverage_mix(hash ^ object_cell[1]);
    hash = coverage_mix(hash ^ object_cell[2]);
    hash = coverage_mix(hash ^ texel[0] ^ texel[1].wrapping_mul(0x9e37_79b9));
    hash = coverage_mix(hash ^ (salt >> 32) as u32 ^ temporal_phase.wrapping_mul(0x85eb_ca6b));
    (hash as f32 + 0.5) * (1.0 / 4_294_967_296.0)
}

/// Classifies one already-sampled canonical coverage value.
///
/// `alpha_width` is the raster derivative footprint and is zero for ray/CPU classification.
/// Canonical probability textures bypass authored-cutoff reconstruction. Alpha-to-coverage and
/// transmissive consumers retain any nonzero probability; masked consumers use the stable spatial
/// threshold.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn classify_canonical_coverage(
    sampled: f32,
    uv: [f32; 2],
    anchor_position: [f32; 3],
    source_kind: CoverageSourceKind,
    classification: AlphaClassification,
    base_color_alpha: f32,
    source_extent: [u32; 2],
    salt: u64,
    temporal_phase: u32,
    reference_cutoff: f32,
    alpha_width: f32,
    canonical_probability: bool,
    alpha_to_coverage: bool,
) -> CanonicalCoverageSample {
    if source_kind == CoverageSourceKind::ModeledGeometry {
        return CanonicalCoverageSample {
            alpha: 1.0,
            covered: true,
        };
    }
    if classification == AlphaClassification::Opaque {
        return CanonicalCoverageSample {
            alpha: 1.0,
            covered: true,
        };
    }
    let (sampled, alpha_width) = if source_kind == CoverageSourceKind::AlbedoAlpha {
        (
            sampled * base_color_alpha,
            alpha_width * base_color_alpha.abs(),
        )
    } else {
        (sampled, alpha_width)
    };
    let probability = if canonical_probability {
        sampled.clamp(0.0, 1.0)
    } else {
        ((sampled - reference_cutoff) / alpha_width.max(1e-5) + 0.5).clamp(0.0, 1.0)
    };
    let covered = if classification == AlphaClassification::Transmissive || alpha_to_coverage {
        probability > 0.0
    } else {
        probability
            >= spatial_coverage_threshold(anchor_position, uv, source_extent, salt, temporal_phase)
    };
    CanonicalCoverageSample {
        alpha: probability,
        covered,
    }
}

fn coverage_mix(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;
    use std::sync::Arc;

    use super::*;
    use crate::compute_dispatch::{ComputeBuffer, run_compute};
    use crate::{Device, SurfaceSource, validation_issue_count};

    const COVERAGE_FIXTURE_COUNT: usize = 8;

    fn noncanonical_albedo_fixture() -> CanonicalCoverageSample {
        classify_canonical_coverage(
            0.625,
            [0.125, 0.625],
            [0.25, -0.5, 1.0],
            CoverageSourceKind::AlbedoAlpha,
            AlphaClassification::Transmissive,
            0.5,
            [64, 32],
            0x1122_3344_5566_7788,
            7,
            0.25,
            0.5,
            false,
            false,
        )
    }

    fn coverage_fixture(index: usize) -> CanonicalCoverageSample {
        match index {
            0 => noncanonical_albedo_fixture(),
            1 | 2 => classify_canonical_coverage(
                0.25,
                [0.2, 0.3],
                [1.0, 2.0, 3.0],
                CoverageSourceKind::Texture,
                AlphaClassification::Masked,
                0.5,
                [64, 64],
                7,
                2,
                0.5,
                0.0,
                true,
                index == 2,
            ),
            3 => classify_canonical_coverage(
                0.0,
                [0.2, 0.3],
                [1.0, 2.0, 3.0],
                CoverageSourceKind::Texture,
                AlphaClassification::Transmissive,
                1.0,
                [64, 64],
                7,
                2,
                0.5,
                0.0,
                true,
                false,
            ),
            4 => classify_canonical_coverage(
                0.0,
                [0.0; 2],
                [0.0; 3],
                CoverageSourceKind::ModeledGeometry,
                AlphaClassification::Masked,
                0.0,
                [1; 2],
                1,
                0,
                0.5,
                0.0,
                true,
                false,
            ),
            5 => classify_canonical_coverage(
                0.0,
                [0.0; 2],
                [0.0; 3],
                CoverageSourceKind::Texture,
                AlphaClassification::Opaque,
                0.0,
                [1; 2],
                1,
                0,
                0.5,
                0.0,
                true,
                false,
            ),
            6 => classify_canonical_coverage(
                0.75,
                [0.2, 0.3],
                [1.0, 2.0, 3.0],
                CoverageSourceKind::Texture,
                AlphaClassification::Masked,
                1.0,
                [64, 64],
                7,
                2,
                0.5,
                0.0,
                false,
                false,
            ),
            7 => classify_canonical_coverage(
                0.5,
                [0.2, 0.3],
                [1.0, 2.0, 3.0],
                CoverageSourceKind::AlbedoAlpha,
                AlphaClassification::Transmissive,
                0.5,
                [64, 64],
                7,
                2,
                0.5,
                0.0,
                true,
                false,
            ),
            _ => unreachable!("coverage fixture index is bounded by COVERAGE_FIXTURE_COUNT"),
        }
    }

    #[test]
    fn threshold_is_object_anchored_and_phase_deterministic() {
        let args = (
            [0.125, -0.25, 0.5],
            [0.2, 0.7],
            [512, 256],
            0x1122_3344_5566_7788,
        );
        let first = spatial_coverage_threshold(args.0, args.1, args.2, args.3, 3);
        assert_eq!(
            first,
            spatial_coverage_threshold(args.0, args.1, args.2, args.3, 3)
        );
        assert_ne!(
            first,
            spatial_coverage_threshold(args.0, args.1, args.2, args.3, 4)
        );
        assert!((0.0..=1.0).contains(&first));
    }

    #[test]
    fn masked_a2c_and_transmissive_share_probability() {
        let classify = |classification, alpha_to_coverage| {
            classify_canonical_coverage(
                0.25,
                [0.2, 0.3],
                [1.0, 2.0, 3.0],
                CoverageSourceKind::Texture,
                classification,
                0.5,
                [64, 64],
                7,
                2,
                0.5,
                0.0,
                true,
                alpha_to_coverage,
            )
        };
        assert_eq!(classify(AlphaClassification::Masked, true).alpha, 0.25);
        assert!(classify(AlphaClassification::Masked, true).covered);
        assert_eq!(
            classify(AlphaClassification::Transmissive, false),
            CanonicalCoverageSample {
                alpha: 0.25,
                covered: true
            }
        );
    }

    #[test]
    fn albedo_factor_and_modeled_geometry_are_canonical_sources() {
        let albedo = classify_canonical_coverage(
            0.5,
            [0.0; 2],
            [0.0; 3],
            CoverageSourceKind::AlbedoAlpha,
            AlphaClassification::Transmissive,
            0.5,
            [1; 2],
            1,
            0,
            0.5,
            0.0,
            true,
            false,
        );
        assert_eq!(albedo.alpha, 0.25);

        let modeled = classify_canonical_coverage(
            0.0,
            [0.0; 2],
            [0.0; 3],
            CoverageSourceKind::ModeledGeometry,
            AlphaClassification::Masked,
            0.0,
            [1; 2],
            1,
            0,
            0.5,
            0.0,
            true,
            false,
        );
        assert_eq!(
            modeled,
            CanonicalCoverageSample {
                alpha: 1.0,
                covered: true,
            }
        );
    }

    #[test]
    fn noncanonical_albedo_factor_precedes_cutoff_reconstruction() {
        assert_eq!(
            noncanonical_albedo_fixture(),
            CanonicalCoverageSample {
                alpha: 0.75,
                covered: true,
            }
        );
    }

    #[test]
    fn rust_and_slang_coverage_matrix_match_on_gpu() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Arc::new(device),
            Err(error) => {
                eprintln!("skipping: no Vulkan device obtainable ({error})");
                return;
            }
        };
        let expected = (0..COVERAGE_FIXTURE_COUNT)
            .flat_map(|index| {
                let sample = coverage_fixture(index);
                [sample.alpha.to_bits(), u32::from(sample.covered)]
            })
            .collect::<Vec<_>>();
        let before = validation_issue_count();
        let buffers = run_compute(
            Arc::clone(&device),
            "coverage_test",
            vec![ComputeBuffer::zeroed(
                COVERAGE_FIXTURE_COUNT * 2 * size_of::<u32>(),
            )],
            [1, 1, 1],
        )
        .expect("coverage fixture GPU dispatch");
        let actual = buffers[0]
            .chunks_exact(size_of::<u32>())
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
