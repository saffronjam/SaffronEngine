//! Conservative opacity-micromap derivation from a coverage alpha plane.
//!
//! An opacity micromap subdivides each triangle and labels the micro-triangles opaque,
//! transparent, or unknown, so ray traversal can skip the coverage classifier wherever the
//! answer is already settled. The traversal treats *unknown* as non-opaque and runs the
//! classifier there, which is what makes unknown the safe verdict: a micromap may only remove
//! work, never change an answer.
//!
//! So the derivation never guesses. A micro-triangle is labelled opaque or transparent only
//! when every point of its texture footprint provably classifies that way; anything the bound
//! cannot settle is unknown. The bound comes from a min/max pyramid over the alpha plane, and
//! the footprint is the micro-triangle's UV bounding box — a superset of the triangle, so the
//! result stays conservative while the query stays cheap.

use saffron_material::{AlphaClassification, OpacityMicromapDerivation};

/// Bytes per micro-triangle state in the 4-state format, expressed as bits.
const STATE_BITS: usize = 2;

/// The largest subdivision level the format allows.
pub const MAX_SUBDIVISION_LEVEL: u8 = 12;

/// Texels one micro-triangle should cover. Bilinear reconstruction reads a 2x2 neighbourhood,
/// so subdividing finer than that cannot sharpen the bound — it only multiplies the work.
const TEXELS_PER_MICRO_TRIANGLE: f64 = 4.0;

/// The four states a micro-triangle can carry, in the encoding the format stores.
///
/// The numeric values are the format's, not ours: traversal maps 0 to *ignored*, 1 to
/// *opaque*, and both unknown states to *non-opaque*, which is what re-runs the classifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MicroState {
    /// Provably below the coverage threshold everywhere: the ray passes through.
    Transparent = 0,
    /// Provably above it everywhere: the ray commits without a classifier call.
    Opaque = 1,
    /// Not provable, leaning transparent. Traversal runs the classifier.
    UnknownTransparent = 2,
    /// Not provable, leaning opaque. Traversal runs the classifier.
    UnknownOpaque = 3,
}

impl MicroState {
    /// Whether traversal must still consult the coverage classifier here.
    #[must_use]
    pub fn is_unknown(self) -> bool {
        matches!(self, Self::UnknownTransparent | Self::UnknownOpaque)
    }
}

/// Per-triangle micromap descriptor, matching `VkMicromapTriangleEXT`'s field order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicromapTriangle {
    /// Byte offset of this triangle's state block within the packed data.
    pub data_offset: u32,
    /// Subdivision level, so `4^level` micro-triangles.
    pub subdivision_level: u16,
    /// Format selector; only the 4-state encoding is derived here.
    pub format: u16,
}

/// One `(count, subdivision_level, format)` usage row, matching `VkMicromapUsageEXT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicromapUsage {
    /// Triangles carrying this level and format.
    pub count: u32,
    /// The subdivision level they share.
    pub subdivision_level: u32,
    /// The format they share.
    pub format: u32,
}

/// How many micro-triangles the derivation settled versus left to the classifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MicromapClasses {
    /// Provably opaque.
    pub opaque: u64,
    /// Provably transparent.
    pub transparent: u64,
    /// Unresolved; the classifier still runs.
    pub unknown: u64,
}

/// A derived micromap: the per-triangle index stream, the blocks it points into, the packed
/// state bits, and the usage histogram the build needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpacityMicromapBuild {
    /// One entry per geometry triangle. Negative values are the format's special indices for
    /// a uniform triangle, which needs no block at all.
    pub indices: Vec<i32>,
    /// Blocks referenced by the non-negative indices.
    pub blocks: Vec<MicromapTriangle>,
    /// Packed micro-triangle states, two bits each, LSB first within a byte.
    pub data: Vec<u8>,
    /// Usage rows grouped by `(level, format)`, ascending.
    pub usage: Vec<MicromapUsage>,
    /// What the derivation settled.
    pub classes: MicromapClasses,
}

impl OpacityMicromapBuild {
    /// The state stored for micro-triangle `micro` of `block`, unpacking the two-bit encoding.
    #[must_use]
    pub fn state(&self, block: usize, micro: u32) -> Option<MicroState> {
        let block = self.blocks.get(block)?;
        let bit = micro as usize * STATE_BITS;
        let byte = *self.data.get(block.data_offset as usize + bit / 8)?;
        Some(match (byte >> (bit % 8)) & 0b11 {
            0 => MicroState::Transparent,
            1 => MicroState::Opaque,
            2 => MicroState::UnknownTransparent,
            _ => MicroState::UnknownOpaque,
        })
    }

    /// Whether anything here is worth building — a derivation that settled nothing carries no
    /// blocks and would only cost memory.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// The coverage rule a surface's ray classification follows, reduced to what a conservative
/// bound needs: the alpha interval that provably classifies covered, and the one that provably
/// does not.
#[derive(Clone, Copy, Debug)]
pub struct CoverageRule {
    /// Alpha at or above which coverage is certain for every hash and anchor.
    pub always_covered_at_or_above: f32,
    /// Alpha at or below which coverage is impossible.
    pub never_covered_at_or_below: f32,
}

impl CoverageRule {
    /// The rule for a surface, given how its ray path classifies coverage.
    ///
    /// `canonical_probability` marks a source whose alpha *is* a coverage probability, which
    /// the masked path compares against an object-anchored spatial hash rather than a constant.
    /// A hash can land anywhere in `(0, 1]`, so only saturated alpha is provable there — the
    /// authored cutoff cannot be honoured without changing what gets drawn.
    ///
    /// With a constant cutoff the ray path collapses to a step at that cutoff, and the guard
    /// band absorbs the difference between the shader's float arithmetic and ours.
    #[must_use]
    pub fn new(
        classification: AlphaClassification,
        canonical_probability: bool,
        reference_cutoff: f32,
    ) -> Self {
        /// Alpha margin absorbing the shader's f32 rounding of the coverage ramp.
        const GUARD: f32 = 1e-4;
        match classification {
            AlphaClassification::Opaque => Self {
                always_covered_at_or_above: 0.0,
                never_covered_at_or_below: -1.0,
            },
            _ if canonical_probability => Self {
                always_covered_at_or_above: 1.0,
                never_covered_at_or_below: 0.0,
            },
            AlphaClassification::Transmissive => Self {
                always_covered_at_or_above: reference_cutoff,
                never_covered_at_or_below: reference_cutoff - GUARD,
            },
            AlphaClassification::Masked => Self {
                always_covered_at_or_above: reference_cutoff + GUARD,
                never_covered_at_or_below: reference_cutoff - GUARD,
            },
        }
    }
}

/// A min/max pyramid over an alpha plane: level `n` holds the extremes of each `2^n` block, so
/// bounding a rectangle costs a handful of taps instead of scanning its texels.
pub struct AlphaBounds {
    levels: Vec<(u32, u32, Vec<u8>, Vec<u8>)>,
}

impl AlphaBounds {
    /// Builds the pyramid from a tightly packed alpha plane.
    #[must_use]
    pub fn new(alpha: &[u8], width: u32, height: u32) -> Self {
        let mut levels = Vec::new();
        if width == 0 || height == 0 || alpha.len() < (width as usize * height as usize) {
            return Self { levels };
        }
        levels.push((width, height, alpha.to_vec(), alpha.to_vec()));
        let (mut w, mut h) = (width, height);
        while w > 1 || h > 1 {
            let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
            let (_, _, ref pmin, ref pmax) = levels[levels.len() - 1];
            let mut nmin = vec![u8::MAX; (nw * nh) as usize];
            let mut nmax = vec![u8::MIN; (nw * nh) as usize];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) as usize;
                    let dst = ((y / 2) * nw + (x / 2)) as usize;
                    nmin[dst] = nmin[dst].min(pmin[src]);
                    nmax[dst] = nmax[dst].max(pmax[src]);
                }
            }
            levels.push((nw, nh, nmin, nmax));
            w = nw;
            h = nh;
        }
        Self { levels }
    }

    /// The `(min, max)` alpha over an inclusive texel rectangle, as normalized floats.
    ///
    /// Walks up to the level where the rectangle spans at most two texels per axis, then reads
    /// the (at most) four covering entries — the standard pyramid query, and the reason the
    /// derivation cost tracks the texture rather than the mesh.
    #[must_use]
    pub fn range(&self, x0: i64, y0: i64, x1: i64, y1: i64) -> (f32, f32) {
        let Some(&(width, height, _, _)) = self.levels.first() else {
            return (0.0, 1.0);
        };
        let clamp = |v: i64, hi: u32| v.clamp(0, i64::from(hi) - 1) as u32;
        let (x0, y0) = (clamp(x0, width), clamp(y0, height));
        let (x1, y1) = (clamp(x1, width), clamp(y1, height));
        let span = (x1 - x0).max(y1 - y0);
        let level = (u32::BITS - span.leading_zeros()).min(self.levels.len() as u32 - 1) as usize;
        let (lw, lh, ref lmin, ref lmax) = self.levels[level];
        let (mut lo, mut hi) = (u8::MAX, u8::MIN);
        for y in (y0 >> level).min(lh - 1)..=(y1 >> level).min(lh - 1) {
            for x in (x0 >> level).min(lw - 1)..=(x1 >> level).min(lw - 1) {
                let i = (y * lw + x) as usize;
                lo = lo.min(lmin[i]);
                hi = hi.max(lmax[i]);
            }
        }
        (f32::from(lo) / 255.0, f32::from(hi) / 255.0)
    }
}

/// Maps barycentric coordinates to a micro-triangle index along the format's space-filling
/// curve.
///
/// Transliterated from the reference implementation published with the extension rather than
/// re-derived: the curve is a specific hierarchical ordering, and an ordering that merely looks
/// plausible would put every state in the wrong slot.
#[must_use]
pub fn barycentrics_to_index(u: f32, v: f32, level: u32) -> u32 {
    let u = u.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let scale = (1u32 << level) as f32;
    let fu = u * scale;
    let fv = v * scale;
    let mut iu = fu as u32;
    let mut iv = fv as u32;
    let uf = fu - iu as f32;
    let vf = fv - iv as f32;
    if iu >= (1 << level) {
        iu = (1 << level) - 1;
    }
    if iv >= (1 << level) {
        iv = (1 << level) - 1;
    }
    let iuv = iu + iv;
    if iuv >= (1 << level) {
        iu -= iuv - (1 << level) + 1;
    }
    let mut iw = !(iu + iv);
    if uf + vf >= 1.0 && iuv < (1 << level) - 1 {
        iw = iw.wrapping_sub(1);
    }
    let mut b0 = !(iu ^ iw);
    b0 &= (1 << level) - 1;
    let t = (iu ^ iv) & b0;
    let mut f = t;
    f ^= f >> 1;
    f ^= f >> 2;
    f ^= f >> 4;
    f ^= f >> 8;
    let mut b1 = ((f ^ iu) & !b0) | t;
    b0 = (b0 | (b0 << 8)) & 0x00ff_00ff;
    b0 = (b0 | (b0 << 4)) & 0x0f0f_0f0f;
    b0 = (b0 | (b0 << 2)) & 0x3333_3333;
    b0 = (b0 | (b0 << 1)) & 0x5555_5555;
    b1 = (b1 | (b1 << 8)) & 0x00ff_00ff;
    b1 = (b1 | (b1 << 4)) & 0x0f0f_0f0f;
    b1 = (b1 | (b1 << 2)) & 0x3333_3333;
    b1 = (b1 | (b1 << 1)) & 0x5555_5555;
    b0 | (b1 << 1)
}

/// The barycentric corners of micro-triangle `index` at `level`, found by inverting the curve
/// over the level's micro-triangles.
///
/// The published reference gives the forward mapping only, so this searches it. The search is
/// bounded by `4^level` and runs once per triangle, and the alternative — a hand-derived
/// inverse — is the kind of thing that is wrong in a way no test notices.
#[must_use]
pub fn micro_triangle_corners(index: u32, level: u32) -> [(f32, f32); 3] {
    let n = 1u32 << level;
    let step = 1.0 / n as f32;
    for row in 0..n {
        for col in 0..(n - row) {
            // The upright sub-triangle of this cell, then the inverted one where it exists.
            let u = col as f32 * step;
            let v = row as f32 * step;
            let upright = [(u, v), (u + step, v), (u, v + step)];
            let centre = centroid(&upright);
            if barycentrics_to_index(centre.0, centre.1, level) == index {
                return upright;
            }
            if col + row + 1 < n {
                let inverted = [(u + step, v), (u + step, v + step), (u, v + step)];
                let centre = centroid(&inverted);
                if barycentrics_to_index(centre.0, centre.1, level) == index {
                    return inverted;
                }
            }
        }
    }
    [(0.0, 0.0), (0.0, 0.0), (0.0, 0.0)]
}

fn centroid(corners: &[(f32, f32); 3]) -> (f32, f32) {
    (
        (corners[0].0 + corners[1].0 + corners[2].0) / 3.0,
        (corners[0].1 + corners[1].1 + corners[2].1) / 3.0,
    )
}

/// The subdivision level for a triangle covering `area_texels`, capped by the policy and the
/// format.
#[must_use]
pub fn subdivision_level(area_texels: f64, policy: &OpacityMicromapDerivation) -> u32 {
    let cap = u32::from(policy.max_subdivision.min(MAX_SUBDIVISION_LEVEL));
    if area_texels <= TEXELS_PER_MICRO_TRIANGLE || cap == 0 {
        return 0;
    }
    let wanted = (area_texels / TEXELS_PER_MICRO_TRIANGLE).log(4.0).ceil();
    (wanted.max(0.0) as u32).min(cap)
}

/// Classifies one micro-triangle from the alpha bound over its UV footprint.
fn classify(
    corners: &[(f32, f32); 3],
    uv: &[[f32; 2]; 3],
    bounds: &AlphaBounds,
    width: u32,
    height: u32,
    rule: &CoverageRule,
    base_alpha: f32,
) -> MicroState {
    let mut min_u = f32::MAX;
    let mut max_u = f32::MIN;
    let mut min_v = f32::MAX;
    let mut max_v = f32::MIN;
    for &(b1, b2) in corners {
        let b0 = 1.0 - b1 - b2;
        let u = uv[0][0] * b0 + uv[1][0] * b1 + uv[2][0] * b2;
        let v = uv[0][1] * b0 + uv[1][1] * b1 + uv[2][1] * b2;
        min_u = min_u.min(u);
        max_u = max_u.max(u);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    // Texel space, dilated by one past the bilinear support so the bound covers every point a
    // sampler could read inside the footprint.
    let to_texel = |value: f32, extent: u32| f64::from(value) * f64::from(extent) - 0.5;
    let x0 = to_texel(min_u, width).floor() as i64 - 1;
    let x1 = to_texel(max_u, width).ceil() as i64 + 1;
    let y0 = to_texel(min_v, height).floor() as i64 - 1;
    let y1 = to_texel(max_v, height).ceil() as i64 + 1;
    let (lo, hi) = bounds.range(x0, y0, x1, y1);
    // The material's base alpha multiplies an albedo-alpha source before classification, so it
    // scales the bound too.
    let (lo, hi) = (lo * base_alpha, hi * base_alpha);
    if lo >= rule.always_covered_at_or_above {
        MicroState::Opaque
    } else if hi <= rule.never_covered_at_or_below {
        MicroState::Transparent
    } else if (lo + hi) * 0.5 >= rule.always_covered_at_or_above {
        MicroState::UnknownOpaque
    } else {
        MicroState::UnknownTransparent
    }
}

/// The coverage plane a derivation reads, with the rule that interprets it.
#[derive(Clone, Copy, Debug)]
pub struct CoverageSourcePlane<'a> {
    /// Tightly packed alpha, exactly the plane the ray path samples.
    pub alpha: &'a [u8],
    /// Plane width in texels.
    pub width: u32,
    /// Plane height in texels.
    pub height: u32,
    /// How the ray path turns a sampled alpha into a coverage verdict.
    pub rule: CoverageRule,
    /// The material's base alpha, which multiplies an albedo-alpha source.
    pub base_alpha: f32,
}

/// Derives a micromap for an indexed triangle mesh against a coverage alpha plane.
///
/// Returns an empty build when the policy is off, the plane is absent, or there is no geometry
/// — in which case no micromap should be attached at all.
#[must_use]
pub fn derive_opacity_micromap(
    indices: &[u32],
    uvs: &[[f32; 2]],
    source: CoverageSourcePlane<'_>,
    policy: &OpacityMicromapDerivation,
) -> OpacityMicromapBuild {
    let CoverageSourcePlane {
        alpha,
        width,
        height,
        rule,
        base_alpha,
    } = source;
    if !policy.enabled || width == 0 || height == 0 || indices.len() < 3 {
        return OpacityMicromapBuild::default();
    }
    let bounds = AlphaBounds::new(alpha, width, height);
    let mut build = OpacityMicromapBuild::default();
    let mut usage: std::collections::BTreeMap<(u32, u32), u32> = std::collections::BTreeMap::new();

    for triangle in indices.chunks_exact(3) {
        let Some(uv) = triangle_uvs(triangle, uvs) else {
            // A triangle without coordinates cannot be bounded, so it stays fully unknown and
            // the classifier keeps deciding it.
            build.indices.push(-4);
            continue;
        };
        let area_texels = uv_area_texels(&uv, width, height);
        let level = subdivision_level(area_texels, policy);
        let count = 1usize << (2 * level);
        let mut states = Vec::with_capacity(count);
        for index in 0..count as u32 {
            let corners = micro_triangle_corners(index, level);
            states.push(classify(
                &corners, &uv, &bounds, width, height, &rule, base_alpha,
            ));
        }
        for state in &states {
            match state {
                MicroState::Opaque => build.classes.opaque += 1,
                MicroState::Transparent => build.classes.transparent += 1,
                _ => build.classes.unknown += 1,
            }
        }
        // A uniform triangle needs no block: the format carries the verdict in the index.
        if let Some(&first) = states.first()
            && states.iter().all(|state| *state == first)
        {
            build.indices.push(match first {
                MicroState::Transparent => -1,
                MicroState::Opaque => -2,
                MicroState::UnknownTransparent => -3,
                MicroState::UnknownOpaque => -4,
            });
            continue;
        }
        let data_offset = u32::try_from(build.data.len()).unwrap_or(u32::MAX);
        let bytes = (count * STATE_BITS).div_ceil(8);
        build.data.resize(build.data.len() + bytes, 0);
        for (index, state) in states.iter().enumerate() {
            let bit = index * STATE_BITS;
            build.data[data_offset as usize + bit / 8] |= (*state as u8) << (bit % 8);
        }
        build.indices.push(build.blocks.len() as i32);
        build.blocks.push(MicromapTriangle {
            data_offset,
            subdivision_level: level as u16,
            format: OPACITY_FORMAT_4_STATE,
        });
        *usage
            .entry((level, u32::from(OPACITY_FORMAT_4_STATE)))
            .or_default() += 1;
    }

    build.usage = usage
        .into_iter()
        .map(|((subdivision_level, format), count)| MicromapUsage {
            count,
            subdivision_level,
            format,
        })
        .collect();
    build
}

/// The 4-state opacity format selector, matching `VK_OPACITY_MICROMAP_FORMAT_4_STATE_EXT`.
pub const OPACITY_FORMAT_4_STATE: u16 = 2;

fn triangle_uvs(triangle: &[u32], uvs: &[[f32; 2]]) -> Option<[[f32; 2]; 3]> {
    Some([
        *uvs.get(triangle[0] as usize)?,
        *uvs.get(triangle[1] as usize)?,
        *uvs.get(triangle[2] as usize)?,
    ])
}

fn uv_area_texels(uv: &[[f32; 2]; 3], width: u32, height: u32) -> f64 {
    let ax = f64::from(uv[1][0] - uv[0][0]) * f64::from(width);
    let ay = f64::from(uv[1][1] - uv[0][1]) * f64::from(height);
    let bx = f64::from(uv[2][0] - uv[0][0]) * f64::from(width);
    let by = f64::from(uv[2][1] - uv[0][1]) * f64::from(height);
    (ax * by - ay * bx).abs() * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_material::SurfaceUnit as UnitInterval;

    fn policy(level: u8) -> OpacityMicromapDerivation {
        OpacityMicromapDerivation {
            enabled: true,
            max_subdivision: level,
            transparent_threshold: UnitInterval::from_bits(0),
            opaque_threshold: UnitInterval::from_bits(u16::MAX),
        }
    }

    /// A plane split down the middle: left fully transparent, right fully opaque.
    fn split_plane(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .map(|i| if i % width < width / 2 { 0 } else { 255 })
            .collect()
    }

    #[test]
    fn the_curve_round_trips_every_micro_triangle() {
        // The index is derived from barycentrics; the corner search inverts it. If either is
        // wrong the states land in the wrong slots, which no image test would localize.
        for level in 0..=6 {
            let count = 1u32 << (2 * level);
            for index in 0..count {
                let corners = micro_triangle_corners(index, level);
                let centre = centroid(&corners);
                assert_eq!(
                    barycentrics_to_index(centre.0, centre.1, level),
                    index,
                    "level {level} index {index}"
                );
            }
        }
    }

    #[test]
    fn a_uniform_triangle_uses_a_special_index_and_no_block() {
        let alpha = vec![255u8; 64 * 64];
        let build = derive_opacity_micromap(
            &[0, 1, 2],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            CoverageSourcePlane {
                alpha: &alpha,
                width: 64,
                height: 64,
                rule: CoverageRule::new(AlphaClassification::Masked, false, 0.5),
                base_alpha: 1.0,
            },
            &policy(3),
        );
        assert_eq!(build.indices, vec![-2], "fully opaque is special index -2");
        assert!(build.blocks.is_empty());
        assert!(build.data.is_empty());
        assert_eq!(build.classes.unknown, 0);
    }

    #[test]
    fn a_mixed_triangle_emits_a_block_with_settled_and_unknown_states() {
        let build = derive_opacity_micromap(
            &[0, 1, 2],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            CoverageSourcePlane {
                alpha: &split_plane(64, 64),
                width: 64,
                height: 64,
                rule: CoverageRule::new(AlphaClassification::Masked, false, 0.5),
                base_alpha: 1.0,
            },
            &policy(3),
        );
        assert_eq!(build.indices.len(), 1);
        assert!(build.indices[0] >= 0, "a mixed triangle references a block");
        assert_eq!(build.blocks.len(), 1);
        assert!(!build.data.is_empty());
        // Both halves of the split must be settled, and the seam between them must not be.
        assert!(build.classes.opaque > 0);
        assert!(build.classes.transparent > 0);
        assert!(build.classes.unknown > 0);
        assert_eq!(
            build.usage,
            vec![MicromapUsage {
                count: 1,
                subdivision_level: 3,
                format: u32::from(OPACITY_FORMAT_4_STATE),
            }]
        );
    }

    #[test]
    fn a_stochastic_source_only_settles_saturated_alpha() {
        // A canonical-probability source is compared against a spatial hash, not a constant, so
        // only 0 and 1 are provable. This is the thin-sheet case, and honouring the authored
        // thresholds here would change what gets drawn.
        let rule = CoverageRule::new(AlphaClassification::Masked, true, 0.5);
        assert_eq!(rule.always_covered_at_or_above, 1.0);
        assert_eq!(rule.never_covered_at_or_below, 0.0);
        // A plane of 0.6 alpha settles nothing under that rule.
        let build = derive_opacity_micromap(
            &[0, 1, 2],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            CoverageSourcePlane {
                alpha: &vec![153u8; 32 * 32],
                width: 32,
                height: 32,
                rule,
                base_alpha: 1.0,
            },
            &policy(2),
        );
        assert_eq!(build.classes.opaque, 0);
        assert_eq!(build.classes.transparent, 0);
        assert!(build.classes.unknown > 0);
    }

    #[test]
    fn the_alpha_bound_contains_every_texel_it_spans() {
        let plane = split_plane(32, 32);
        let bounds = AlphaBounds::new(&plane, 32, 32);
        for (x0, y0, x1, y1) in [(0i64, 0i64, 31i64, 31i64), (0, 0, 5, 5), (20, 4, 27, 19)] {
            let (lo, hi) = bounds.range(x0, y0, x1, y1);
            let mut actual_lo = f32::MAX;
            let mut actual_hi = f32::MIN;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let value = f32::from(plane[(y * 32 + x) as usize]) / 255.0;
                    actual_lo = actual_lo.min(value);
                    actual_hi = actual_hi.max(value);
                }
            }
            assert!(lo <= actual_lo, "bound min {lo} exceeds actual {actual_lo}");
            assert!(hi >= actual_hi, "bound max {hi} below actual {actual_hi}");
        }
    }

    #[test]
    fn a_disabled_policy_derives_nothing() {
        let mut off = policy(3);
        off.enabled = false;
        let build = derive_opacity_micromap(
            &[0, 1, 2],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            CoverageSourcePlane {
                alpha: &vec![255u8; 16 * 16],
                width: 16,
                height: 16,
                rule: CoverageRule::new(AlphaClassification::Masked, false, 0.5),
                base_alpha: 1.0,
            },
            &off,
        );
        assert!(build.is_empty());
    }

    #[test]
    fn subdivision_tracks_texel_density_under_the_policy_cap() {
        let p = policy(12);
        assert_eq!(subdivision_level(1.0, &p), 0, "a tiny triangle needs none");
        assert!(subdivision_level(4096.0, &p) > subdivision_level(64.0, &p));
        assert_eq!(subdivision_level(1e9, &policy(2)), 2, "the cap binds");
    }
}
