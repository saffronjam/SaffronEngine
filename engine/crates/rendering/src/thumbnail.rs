//! Screenshot/thumbnail read-back encode: the captured-framebuffer → RGB conversion and the PNG
//! encode.
//!
//! The PNG encode uses the `image` crate's [`image::codecs::png::PngEncoder`] rather
//! than an stb binding — the bytes are an internal preview artifact, not a hash-parity
//! wire contract, so any conforming RGB8 PNG encoder is correct.

use ash::vk;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;

/// Encoded PNG bytes plus the actual encoded pixel dimensions (so a reply reports the truthful
/// width/height rather than the requested size).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailPng {
    /// The encoded PNG bytes.
    pub bytes: Vec<u8>,
    /// The encoded image width.
    pub width: u32,
    /// The encoded image height.
    pub height: u32,
}

/// Which HDR→display mapping a thumbnail/screenshot PNG applies to an `RGBA16F` source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PngTransfer {
    /// The source is already tonemapped to display range: clamp `[0,1]×255`. Used for
    /// the post-processed offscreen capture.
    #[default]
    Clamp,
    /// The source is scene-linear HDR radiance: Reinhard + gamma. Used for an
    /// HDR-asset preview thumbnail.
    Tonemap,
}

/// Bytes per pixel for a captured framebuffer format: `RGBA16F` is 8, every other
/// supported capture format is 4.
pub fn format_pixel_bytes(format: vk::Format) -> u32 {
    if format == vk::Format::R16G16B16A16_SFLOAT {
        8
    } else {
        4
    }
}

/// Whether a format stores its first two 8-bit channels as B,G (the swapchain / shm
/// BGRA path) rather than R,G.
fn is_bgr(format: vk::Format) -> bool {
    format == vk::Format::B8G8R8A8_UNORM || format == vk::Format::B8G8R8A8_SRGB
}

/// Unpacks an IEEE-754 binary16 half (native u16) to f32. The inverse of
/// `upload::float_to_half`; the capture path reads back the half-float offscreen.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h & 0x8000) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x03ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign << 16 // signed zero
        } else {
            // Subnormal: normalize into a binary32 normal.
            let mut e = -1i32;
            let mut m = mant;
            loop {
                e += 1;
                m <<= 1;
                if m & 0x0400 != 0 {
                    break;
                }
            }
            let m = m & 0x03ff;
            (sign << 16) | (((127 - 15 - e) as u32) << 23) | (m << 13)
        }
    } else if exp == 0x1f {
        // Inf / NaN.
        (sign << 16) | 0x7f80_0000 | (mant << 13)
    } else {
        (sign << 16) | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// Encodes one HDR channel value to an 8-bit display byte per `transfer`: Reinhard +
/// gamma for `Tonemap`, a hard clamp for `Clamp`.
fn encode_hdr_channel(c: f32, transfer: PngTransfer) -> u8 {
    let mut v = if c < 0.0 { 0.0 } else { c };
    match transfer {
        PngTransfer::Tonemap => {
            v = v / (1.0 + v); // Reinhard: scene-linear radiance → [0,1)
            v = v.powf(1.0 / 2.2); // gamma encode for display
        }
        PngTransfer::Clamp => {
            if v > 1.0 {
                v = 1.0;
            }
        }
    }
    (v * 255.0 + 0.5) as u8
}

/// Converts a captured framebuffer to a tightly-packed 3-channel RGB buffer. 8-bit
/// sources reorder BGRA→RGB; `RGBA16F` sources unpack the half floats and apply
/// `transfer`. `pixels` must hold at least `width * height * format_pixel_bytes(format)`
/// bytes.
pub fn convert_to_rgb(
    pixels: &[u8],
    width: u32,
    height: u32,
    format: vk::Format,
    transfer: PngTransfer,
) -> Vec<u8> {
    let count = (width as usize) * (height as usize);
    let mut rgb = vec![0u8; count * 3];
    if format == vk::Format::R16G16B16A16_SFLOAT {
        for i in 0..count {
            let base = i * 8;
            let r = half_to_f32(u16::from_le_bytes([pixels[base], pixels[base + 1]]));
            let g = half_to_f32(u16::from_le_bytes([pixels[base + 2], pixels[base + 3]]));
            let b = half_to_f32(u16::from_le_bytes([pixels[base + 4], pixels[base + 5]]));
            rgb[i * 3] = encode_hdr_channel(r, transfer);
            rgb[i * 3 + 1] = encode_hdr_channel(g, transfer);
            rgb[i * 3 + 2] = encode_hdr_channel(b, transfer);
        }
    } else {
        let bgr = is_bgr(format);
        for i in 0..count {
            let (r_idx, b_idx) = if bgr {
                (i * 4 + 2, i * 4)
            } else {
                (i * 4, i * 4 + 2)
            };
            rgb[i * 3] = pixels[r_idx];
            rgb[i * 3 + 1] = pixels[i * 4 + 1];
            rgb[i * 3 + 2] = pixels[b_idx];
        }
    }
    rgb
}

/// Encodes a captured framebuffer to PNG bytes in memory (no file). Used for thumbnails
/// shipped over the JSON control protocol as base64.
///
/// # Errors
///
/// Returns [`crate::Error::ShaderLoad`] is never produced here; a PNG encode failure is
/// surfaced as [`std::io::Error`].
pub fn encode_to_png(
    pixels: &[u8],
    width: u32,
    height: u32,
    format: vk::Format,
    transfer: PngTransfer,
) -> std::io::Result<Vec<u8>> {
    let rgb = convert_to_rgb(pixels, width, height, format, transfer);
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(std::io::Error::other)?;
    Ok(out)
}

/// Writes a captured framebuffer to a PNG file (the `captureViewport` / window-capture
/// screenshot path). 8-bit sources reorder BGRA→RGB; the post-processed `RGBA16F`
/// offscreen is already display-range, so its halves are clamped.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] if the file cannot be written or the PNG
/// cannot be encoded.
pub fn write_png_file(
    pixels: &[u8],
    width: u32,
    height: u32,
    format: vk::Format,
    path: &std::path::Path,
) -> std::io::Result<()> {
    let bytes = encode_to_png(pixels, width, height, format, PngTransfer::Clamp)?;
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::float_to_half as float_to_half_for_test;

    #[test]
    fn pixel_bytes_match_the_format() {
        assert_eq!(format_pixel_bytes(vk::Format::R16G16B16A16_SFLOAT), 8);
        assert_eq!(format_pixel_bytes(vk::Format::B8G8R8A8_UNORM), 4);
        assert_eq!(format_pixel_bytes(vk::Format::R8G8B8A8_UNORM), 4);
    }

    #[test]
    fn bgra8_is_reordered_to_rgb() {
        // One BGRA pixel: B=10, G=20, R=30, A=255.
        let pixels = [10u8, 20, 30, 255];
        let rgb = convert_to_rgb(
            &pixels,
            1,
            1,
            vk::Format::B8G8R8A8_UNORM,
            PngTransfer::Clamp,
        );
        assert_eq!(rgb, vec![30, 20, 10], "BGRA → RGB reorders the channels");
    }

    #[test]
    fn rgba8_keeps_channel_order() {
        let pixels = [30u8, 20, 10, 255]; // RGBA
        let rgb = convert_to_rgb(
            &pixels,
            1,
            1,
            vk::Format::R8G8B8A8_UNORM,
            PngTransfer::Clamp,
        );
        assert_eq!(rgb, vec![30, 20, 10]);
    }

    #[test]
    fn half_to_f32_round_trips_known_values() {
        // Round-trip a set of finite values through float_to_half → half_to_f32.
        for &v in &[0.0f32, 1.0, 0.5, 2.0, 0.25, 100.0, -1.0, 0.1] {
            let h = float_to_half_for_test(v);
            let back = half_to_f32(h);
            // Half precision: expect close, not exact, for non-power-of-two values.
            let tol = v.abs() * 0.001 + 0.001;
            assert!((back - v).abs() <= tol, "half round-trip {v} → {back}");
        }
        // Exact half-representable values are exact.
        assert_eq!(half_to_f32(float_to_half_for_test(1.0)), 1.0);
        assert_eq!(half_to_f32(float_to_half_for_test(0.0)), 0.0);
        assert_eq!(half_to_f32(float_to_half_for_test(0.5)), 0.5);
    }

    #[test]
    fn hdr_clamp_maps_over_range_to_white() {
        // A 2-pixel RGBA16F row: pixel 0 = (0.5, 0.5, 0.5), pixel 1 = (3.0, 0.0, -1.0).
        let mut pixels = Vec::new();
        for &v in &[0.5f32, 0.5, 0.5, 1.0] {
            pixels.extend_from_slice(&float_to_half_for_test(v).to_le_bytes());
        }
        for &v in &[3.0f32, 0.0, -1.0, 1.0] {
            pixels.extend_from_slice(&float_to_half_for_test(v).to_le_bytes());
        }
        let rgb = convert_to_rgb(
            &pixels,
            2,
            1,
            vk::Format::R16G16B16A16_SFLOAT,
            PngTransfer::Clamp,
        );
        // pixel 0: 0.5 → ~128.
        assert!((rgb[0] as i32 - 128).abs() <= 1);
        // pixel 1: 3.0 clamps to 255; 0.0 → 0; -1.0 → 0.
        assert_eq!(rgb[3], 255, "over-range clamps to white");
        assert_eq!(rgb[4], 0);
        assert_eq!(rgb[5], 0, "negative clamps to black");
    }

    #[test]
    fn hdr_tonemap_compresses_radiance() {
        // A scene-linear radiance of 1.0 Reinhards to 0.5, then gamma to ~0.5^(1/2.2).
        let mut pixels = Vec::new();
        for &v in &[1.0f32, 1.0, 1.0, 1.0] {
            pixels.extend_from_slice(&float_to_half_for_test(v).to_le_bytes());
        }
        let rgb = convert_to_rgb(
            &pixels,
            1,
            1,
            vk::Format::R16G16B16A16_SFLOAT,
            PngTransfer::Tonemap,
        );
        // 1/(1+1) = 0.5; 0.5^(1/2.2) ≈ 0.7297 → ~186.
        let expected = (0.5f32.powf(1.0 / 2.2) * 255.0 + 0.5) as u8;
        assert_eq!(rgb[0], expected);
        assert!(
            rgb[0] < 255 && rgb[0] > 128,
            "tonemap compresses, not clamps"
        );
    }

    #[test]
    fn encode_to_png_produces_a_valid_png_signature() {
        // A 2x2 RGBA8 image.
        let pixels = vec![
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let png = encode_to_png(
            &pixels,
            2,
            2,
            vk::Format::R8G8B8A8_UNORM,
            PngTransfer::Clamp,
        )
        .expect("encode");
        // The PNG 8-byte magic signature.
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // Round-trip through the decoder to confirm the dimensions + a pixel.
        let decoded = image::load_from_memory(&png).expect("decode").to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }
}
