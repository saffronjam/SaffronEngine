//! Raster image decode onto the `image` crate.
//!
//! The output is always 4 channels, tightly packed `width * height * 4`: the 8-bit path
//! yields RGBA8, the float path linear RGBA f32 for `.hdr`-class sources.

use std::path::Path;

use crate::error::{Error, Result};
use crate::types::{DecodedImage, DecodedImageFloat};

/// Decode an image file into tightly packed RGBA8.
pub fn decode_image(path: impl AsRef<Path>) -> Result<DecodedImage> {
    let path = path.as_ref();
    let img = image::open(path)
        .map_err(|e| Error::Decode(format!("cannot decode image '{}': {e}", path.display())))?;
    Ok(to_rgba8(img))
}

/// Decode encoded image bytes into tightly packed RGBA8.
pub fn decode_image_from_memory(encoded: &[u8]) -> Result<DecodedImage> {
    let img = image::load_from_memory(encoded)
        .map_err(|e| Error::Decode(format!("cannot decode image from memory: {e}")))?;
    Ok(to_rgba8(img))
}

/// Decode an HDR image file into tightly packed linear RGBA f32. Detection is by content,
/// not extension: our float textures are stored under a `.hdr` name regardless of the real
/// container (a Radiance `.hdr` or an OpenEXR `.exr`), so `image::open`'s extension-based
/// dispatch would force the wrong decoder.
pub fn decode_image_hdr(path: impl AsRef<Path>) -> Result<DecodedImageFloat> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Decode(format!("cannot read HDR image '{}': {e}", path.display())))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| Error::Decode(format!("cannot decode HDR image '{}': {e}", path.display())))?;
    Ok(to_rgba32f(img))
}

/// Decode encoded HDR image bytes into tightly packed linear RGBA f32.
pub fn decode_image_from_memory_hdr(encoded: &[u8]) -> Result<DecodedImageFloat> {
    let img = image::load_from_memory(encoded)
        .map_err(|e| Error::Decode(format!("cannot decode HDR image from memory: {e}")))?;
    Ok(to_rgba32f(img))
}

/// Convert a decoded image to the tightly packed RGBA8 contract.
fn to_rgba8(img: image::DynamicImage) -> DecodedImage {
    let rgba = img.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    DecodedImage {
        rgba: rgba.into_raw(),
        width,
        height,
    }
}

/// Convert a decoded image to the tightly packed linear RGBA f32 contract.
fn to_rgba32f(img: image::DynamicImage) -> DecodedImageFloat {
    let rgba = img.to_rgba32f();
    let width = rgba.width();
    let height = rgba.height();
    DecodedImageFloat {
        rgba: rgba.into_raw(),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    /// Encode a small solid-color RGBA image to PNG bytes for the decode tests.
    fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let img = RgbaImage::from_pixel(width, height, Rgba(color));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode png");
        bytes
    }

    #[test]
    fn png_decodes_to_packed_rgba8() {
        let bytes = png_bytes(3, 2, [10, 20, 30, 255]);
        let decoded = decode_image_from_memory(&bytes).expect("decode png");
        assert_eq!(decoded.width, 3);
        assert_eq!(decoded.height, 2);
        assert_eq!(
            decoded.rgba.len() as u32,
            decoded.width * decoded.height * 4
        );
        assert_eq!(&decoded.rgba[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn rgb_source_is_promoted_to_four_channels() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([5, 6, 7]),
        ));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode rgb png");
        let decoded = decode_image_from_memory(&bytes).expect("decode rgb png");
        assert_eq!(decoded.rgba.len(), 2 * 2 * 4);
        assert_eq!(&decoded.rgba[0..4], &[5, 6, 7, 255]);
    }

    #[test]
    fn hdr_decodes_to_packed_rgba_f32() {
        // A Radiance .hdr encoded by the same crate, with an above-1.0 pixel; the
        // float path keeps real radiance (no sRGB clamp) and packs 4 floats per pixel.
        let pixels = [image::Rgb([2.0f32, 0.5, 0.25]), image::Rgb([0.1, 0.2, 0.3])];
        let mut bytes = Vec::new();
        let encoder = image::codecs::hdr::HdrEncoder::new(&mut bytes);
        encoder.encode(&pixels, 2, 1).expect("encode hdr");

        let decoded = decode_image_from_memory_hdr(&bytes).expect("decode hdr");
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(
            decoded.rgba.len() as u32,
            decoded.width * decoded.height * 4
        );
        assert!(decoded.rgba[0] > 1.5, "got {}", decoded.rgba[0]);
        assert_eq!(decoded.rgba[3], 1.0);
    }

    #[test]
    fn garbage_bytes_are_a_decode_error() {
        let err = decode_image_from_memory(&[0u8, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, Error::Decode(_)), "got {err}");
    }
}
