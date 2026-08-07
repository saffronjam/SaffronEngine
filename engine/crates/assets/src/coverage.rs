//! Canonical coverage-image derivation for masked and thin-sheet materials.

/// One tightly packed RGBA8 level of a canonical coverage-preserving mip chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageMip {
    /// Level width in texels.
    pub width: u32,
    /// Level height in texels.
    pub height: u32,
    /// Row-major RGBA8 texels.
    pub rgba: Vec<u8>,
}

/// Builds a full alpha-area-preserving mip chain from decoded RGBA8 pixels.
///
/// Alpha is an exact box integral over each destination texel's normalized source
/// footprint. RGB is filtered premultiplied by alpha and unpremultiplied afterward,
/// which prevents transparent-edge color fringes. The integer overlap weights make
/// odd extents deterministic and keep the result independent of a GPU blitter.
#[must_use]
pub fn coverage_preserving_mips(
    rgba: &[u8],
    width: u32,
    height: u32,
    reference_cutoff_bits: u16,
) -> Vec<CoverageMip> {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || rgba.len() != expected {
        return Vec::new();
    }
    let mut canonical = rgba.to_vec();
    let cutoff = u8::try_from((u32::from(reference_cutoff_bits) * 255 + 32_767) / 65_535)
        .expect("normalized cutoff maps to u8");
    for pixel in canonical.chunks_exact_mut(4) {
        pixel[3] = remap_reference_cutoff(pixel[3], cutoff);
    }
    let mut levels = vec![CoverageMip {
        width,
        height,
        rgba: canonical,
    }];
    while levels
        .last()
        .is_some_and(|level| level.width > 1 || level.height > 1)
    {
        let previous = levels.last().expect("base coverage mip exists");
        levels.push(downsample(previous));
    }
    levels
}

fn remap_reference_cutoff(alpha: u8, cutoff: u8) -> u8 {
    match (alpha, cutoff) {
        (0, _) => 0,
        (255, _) => 255,
        (_, 0) => 128 + rounded_div(u64::from(alpha) * 127, 255) as u8,
        (_, 255) => rounded_div(u64::from(alpha) * 128, 255) as u8,
        (alpha, cutoff) if alpha <= cutoff => {
            rounded_div(u64::from(alpha) * 128, u64::from(cutoff)) as u8
        }
        (alpha, cutoff) => {
            128 + rounded_div(u64::from(alpha - cutoff) * 127, u64::from(255 - cutoff)) as u8
        }
    }
}

fn downsample(source: &CoverageMip) -> CoverageMip {
    let width = (source.width / 2).max(1);
    let height = (source.height / 2).max(1);
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let mut alpha_weight = 0_u64;
            let mut total_weight = 0_u64;
            let mut premultiplied = [0_u64; 3];
            for sy in 0..source.height {
                let wy = interval_overlap(y, height, sy, source.height);
                if wy == 0 {
                    continue;
                }
                for sx in 0..source.width {
                    let wx = interval_overlap(x, width, sx, source.width);
                    if wx == 0 {
                        continue;
                    }
                    let weight = u64::from(wx) * u64::from(wy);
                    let at = (sy as usize * source.width as usize + sx as usize) * 4;
                    let alpha = u64::from(source.rgba[at + 3]);
                    total_weight += weight;
                    alpha_weight += alpha * weight;
                    for (channel, accumulated) in premultiplied.iter_mut().enumerate() {
                        *accumulated += u64::from(source.rgba[at + channel]) * alpha * weight;
                    }
                }
            }
            let out = (y as usize * width as usize + x as usize) * 4;
            rgba[out + 3] = rounded_div(alpha_weight, total_weight) as u8;
            if alpha_weight != 0 {
                for (channel, &accumulated) in premultiplied.iter().enumerate() {
                    rgba[out + channel] = rounded_div(accumulated, alpha_weight) as u8;
                }
            }
        }
    }
    CoverageMip {
        width,
        height,
        rgba,
    }
}

fn interval_overlap(dst: u32, dst_extent: u32, src: u32, src_extent: u32) -> u32 {
    let dst_start = dst * src_extent;
    let dst_end = (dst + 1) * src_extent;
    let src_start = src * dst_extent;
    let src_end = (src + 1) * dst_extent;
    dst_end
        .min(src_end)
        .saturating_sub(dst_start.max(src_start))
}

fn rounded_div(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator / 2) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(level: &CoverageMip) -> Vec<u8> {
        level.rgba.chunks_exact(4).map(|pixel| pixel[3]).collect()
    }

    #[test]
    fn binary_checkerboard_keeps_half_coverage_to_one_texel() {
        let mut rgba = Vec::new();
        for index in 0..16 {
            rgba.extend_from_slice(&[40, 180, 70, if index % 2 == 0 { 255 } else { 0 }]);
        }
        let levels = coverage_preserving_mips(&rgba, 4, 4, 32_768);
        assert_eq!(
            levels
                .iter()
                .map(|mip| (mip.width, mip.height))
                .collect::<Vec<_>>(),
            [(4, 4), (2, 2), (1, 1)]
        );
        assert_eq!(alpha(&levels[1]), [128; 4]);
        assert_eq!(alpha(&levels[2]), [128]);
        assert_eq!(&levels[2].rgba[..3], &[40, 180, 70]);
    }

    #[test]
    fn odd_extents_use_exact_normalized_footprints() {
        let mut rgba = vec![255_u8; 7 * 3 * 4];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel[3] = (index * 11) as u8;
        }
        let levels = coverage_preserving_mips(&rgba, 7, 3, 32_768);
        assert_eq!(
            levels
                .iter()
                .map(|mip| (mip.width, mip.height))
                .collect::<Vec<_>>(),
            [(7, 3), (3, 1), (1, 1)]
        );
        let source_mean = rgba
            .chunks_exact(4)
            .map(|pixel| u64::from(pixel[3]))
            .sum::<u64>()
            / 21;
        assert!(u64::from(levels.last().unwrap().rgba[3]).abs_diff(source_mean) <= 1);
    }

    #[test]
    fn invalid_image_has_no_chain() {
        assert!(coverage_preserving_mips(&[], 0, 1, 32_768).is_empty());
        assert!(coverage_preserving_mips(&[0; 3], 1, 1, 32_768).is_empty());
    }

    #[test]
    fn authored_reference_cutoff_maps_to_half_coverage() {
        for cutoff in [32_u8, 128, 224] {
            let bits = u16::try_from((u32::from(cutoff) * 65_535 + 127) / 255).unwrap();
            let rgba = [1, 2, 3, cutoff];
            let levels = coverage_preserving_mips(&rgba, 1, 1, bits);
            assert!(levels[0].rgba[3].abs_diff(128) <= 1);
        }
    }
}
