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

    /// A micromap may remove work but never change an answer: for every micro-triangle the
    /// derivation calls settled, the classifier must agree at every point inside it, under
    /// every spatial-hash salt, anchor and temporal phase — the hash makes coverage depend on
    /// where the plant stands rather than on alpha alone. Unknown states are not asserted:
    /// they mean the classifier still runs, so whatever it decides is correct.
    ///
    /// Both coverage rules are checked. A plain masked surface reconstructs the authored cutoff;
    /// a thin-sheet surface's alpha *is* a coverage probability, which the classifier compares
    /// against the spatial hash instead — a different branch, and the one the plant cooker takes.
    #[test]
    fn settled_micro_triangles_agree_with_the_classifier_under_every_hash() {
        use saffron_geometry::{
            CoverageRule, CoverageSourcePlane, MicroState, derive_opacity_micromap,
            micro_triangle_corners,
        };
        use saffron_material::{AlphaClassification, OpacityMicromapDerivation, SurfaceUnit};

        const EXTENT: u32 = 64;
        // A full-range gradient: micro-triangles tile every alpha level, so a derivation that
        // settles at the wrong threshold has somewhere to be caught.
        let gradient: Vec<u8> = (0..EXTENT * EXTENT)
            .map(|i| ((i % EXTENT) * 255 / (EXTENT - 1)) as u8)
            .collect();
        // The authored cutout shape: a hard diagonal, placed to cross the probe triangle so both
        // verdicts occur, and with no soft ramp for a rounding error to hide in.
        let cutout: Vec<u8> = (0..EXTENT * EXTENT)
            .map(|i| {
                let (x, y) = (i % EXTENT, i / EXTENT);
                if x + y < EXTENT / 2 { 255 } else { 0 }
            })
            .collect();

        let uvs = [[0.0_f32, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let cutoff = 0.5_f32;
        // `(alpha plane, canonical probability, subdivision cap)`. The cap is the cooked one for
        // the thin-sheet case: the level a full-plane triangle picks there is the finest the
        // derivation ever runs at, and the bound is tightest exactly there.
        let cases: [(&[u8], bool, u8); 2] = [(&gradient, false, 4), (&cutout, true, 5)];

        for (alpha, canonical_probability, max_subdivision) in cases {
            let policy = OpacityMicromapDerivation {
                enabled: true,
                max_subdivision,
                transparent_threshold: SurfaceUnit::from_bits(0),
                opaque_threshold: SurfaceUnit::from_bits(u16::MAX),
            };
            let rule =
                CoverageRule::new(AlphaClassification::Masked, canonical_probability, cutoff);
            let build = derive_opacity_micromap(
                &[0, 1, 2],
                &uvs,
                CoverageSourcePlane {
                    alpha,
                    width: EXTENT,
                    height: EXTENT,
                    rule,
                    base_alpha: 1.0,
                },
                &policy,
            );

            // The derivation must have settled something, or this proves nothing at all.
            assert!(
                build.classes.opaque > 0,
                "no opaque micro-triangles to check (canonical {canonical_probability})"
            );
            assert!(
                build.classes.transparent > 0,
                "no transparent micro-triangles to check (canonical {canonical_probability})"
            );

            // Bilinear, clamp-to-edge: what the sampler the ray path binds actually returns, and
            // the reason the derivation dilates its texel footprint past the tap's own support.
            let sample = |uv: [f32; 2]| -> f32 {
                let texel = |v: f32, extent: u32| v * extent as f32 - 0.5;
                let (tx, ty) = (texel(uv[0], EXTENT), texel(uv[1], EXTENT));
                let (x0, y0) = (tx.floor(), ty.floor());
                let (fx, fy) = (tx - x0, ty - y0);
                let at = |x: f32, y: f32| {
                    let x = (x as i64).clamp(0, i64::from(EXTENT) - 1) as u32;
                    let y = (y as i64).clamp(0, i64::from(EXTENT) - 1) as u32;
                    f32::from(alpha[(y * EXTENT + x) as usize]) / 255.0
                };
                let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
                let bottom = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
                top * (1.0 - fy) + bottom * fy
            };

            let block = build.indices[0];
            assert!(block >= 0, "the edge must produce a mixed block");
            let level = u32::from(build.blocks[block as usize].subdivision_level);
            let mut checked = 0_u32;
            for micro in 0..1u32 << (2 * level) {
                let Some(state) = build.state(block as usize, micro) else {
                    continue;
                };
                if state.is_unknown() {
                    continue;
                }
                let corners = micro_triangle_corners(micro, level);
                // Corners, edge midpoints and the centroid: a ray hits anywhere in the
                // micro-triangle including its boundary, and the boundary is where the bound is
                // tightest — sampling only the interior would never test the dilation.
                let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
                let probes = [
                    corners[0],
                    corners[1],
                    corners[2],
                    mid(corners[0], corners[1]),
                    mid(corners[1], corners[2]),
                    mid(corners[2], corners[0]),
                    (
                        (corners[0].0 + corners[1].0 + corners[2].0) / 3.0,
                        (corners[0].1 + corners[1].1 + corners[2].1) / 3.0,
                    ),
                ];
                for (b1, b2) in probes {
                    let b0 = 1.0 - b1 - b2;
                    let uv = [
                        uvs[0][0] * b0 + uvs[1][0] * b1 + uvs[2][0] * b2,
                        uvs[0][1] * b0 + uvs[1][1] * b1 + uvs[2][1] * b2,
                    ];
                    for salt in [0_u64, 7, 0x1122_3344_5566_7788, u64::MAX] {
                        for phase in [0_u32, 3, 91] {
                            for anchor in [[0.0_f32; 3], [12.5, -3.25, 400.0], [-1e3, 7.0, 1e3]] {
                                let verdict = classify_canonical_coverage(
                                    sample(uv),
                                    uv,
                                    anchor,
                                    CoverageSourceKind::Texture,
                                    AlphaClassification::Masked,
                                    1.0,
                                    [EXTENT, EXTENT],
                                    salt,
                                    phase,
                                    cutoff,
                                    0.0,
                                    canonical_probability,
                                    false,
                                );
                                assert_eq!(
                                    verdict.covered,
                                    state == MicroState::Opaque,
                                    "micro {micro} declared {state:?} but the classifier \
                                     disagreed (canonical {canonical_probability}, salt {salt}, \
                                     phase {phase}, anchor {anchor:?})"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
            assert!(checked > 0, "no settled micro-triangle was sampled");
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
