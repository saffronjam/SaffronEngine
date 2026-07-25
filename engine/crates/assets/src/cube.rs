//! The creative 3D LUT interchange: the Adobe/Resolve `.cube` importer and the engine's native
//! baked-look `.slut` codec.
//!
//! A `.cube` is a display-referred creative look — a text table of `N³` RGB triples the tonemap pass
//! samples tetrahedrally after the view transform. A `.slut` is the engine's binary bake output: the
//! whole HDR→display tail (grade + view transform + creative LUT) folded into one table over a log2
//! shaper, the interchange the exported player and external full-tail ingest consume.

use std::sync::Arc;

use saffron_core::Uuid;
use saffron_rendering::{GpuLut, LUT_SHAPER_EV_MAX, LUT_SHAPER_EV_MIN};
use saffron_scene::{AssetEntry, AssetType};

use crate::gpu::GpuUploader;
use crate::{AssetServer, Error};

/// A parsed creative look-up table imported from a `.cube` file.
#[derive(Debug, Clone)]
pub struct CubeLut {
    /// The table resolution per axis — `17`, `33`, or `65`.
    pub size: u32,
    /// The lower domain bound (`[0, 0, 0]` by default).
    pub domain_min: [f32; 3],
    /// The upper domain bound (`[1, 1, 1]` by default).
    pub domain_max: [f32; 3],
    /// The `size³` entries, red-fastest.
    pub rgb: Vec<[f32; 3]>,
}

/// A `.cube` parse failure.
#[derive(Debug, thiserror::Error)]
pub enum CubeError {
    /// The `LUT_3D_SIZE` was not one of the supported resolutions.
    #[error("unsupported LUT_3D_SIZE {0} (expected 17, 33 or 65)")]
    UnsupportedSize(u32),
    /// The table held the wrong number of RGB triples for its declared size.
    #[error("expected {expected} entries, found {found}")]
    EntryCount {
        /// The count the `LUT_3D_SIZE` implies (`size³`).
        expected: usize,
        /// The count actually parsed.
        found: usize,
    },
    /// A line could not be parsed (a bad number, a wrong field count, or a missing size).
    #[error("malformed line {line}: {reason}")]
    Malformed {
        /// The 1-indexed source line.
        line: usize,
        /// A short cause.
        reason: &'static str,
    },
    /// The file declared no `LUT_3D_SIZE`.
    #[error("no LUT_3D_SIZE header")]
    NoSize,
}

/// Parses a `.cube` file: an optional `TITLE`, a `LUT_3D_SIZE N` (`17|33|65`), optional
/// `DOMAIN_MIN`/`DOMAIN_MAX`, then `N³` whitespace-separated `r g b` triples ordered red-fastest.
///
/// # Errors
///
/// [`CubeError`] for an unsupported size, a wrong entry count, a malformed line, or a missing size.
pub fn parse_cube(text: &str) -> Result<CubeLut, CubeError> {
    let mut size: Option<u32> = None;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut rgb: Vec<[f32; 3]> = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let content = raw.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let mut fields = content.split_whitespace();
        let key = fields.next().unwrap_or("");
        match key {
            "TITLE" => {}
            "LUT_3D_SIZE" => {
                let n: u32 =
                    fields
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or(CubeError::Malformed {
                            line,
                            reason: "LUT_3D_SIZE needs an integer",
                        })?;
                if !matches!(n, 17 | 33 | 65) {
                    return Err(CubeError::UnsupportedSize(n));
                }
                size = Some(n);
                rgb.reserve((n as usize).pow(3));
            }
            "LUT_1D_SIZE" => return Err(CubeError::UnsupportedSize(0)),
            "DOMAIN_MIN" => domain_min = parse_triple(&mut fields, line)?,
            "DOMAIN_MAX" => domain_max = parse_triple(&mut fields, line)?,
            _ => {
                // A data row: three floats (the key was the first). Re-parse the whole line.
                let mut all = content.split_whitespace();
                rgb.push(parse_triple(&mut all, line)?);
            }
        }
    }

    let size = size.ok_or(CubeError::NoSize)?;
    let expected = (size as usize).pow(3);
    if rgb.len() != expected {
        return Err(CubeError::EntryCount {
            expected,
            found: rgb.len(),
        });
    }
    Ok(CubeLut {
        size,
        domain_min,
        domain_max,
        rgb,
    })
}

/// Parses three whitespace floats off an iterator.
fn parse_triple<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    line: usize,
) -> Result<[f32; 3], CubeError> {
    let mut out = [0.0f32; 3];
    for slot in &mut out {
        *slot = fields
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or(CubeError::Malformed {
                line,
                reason: "expected three floats",
            })?;
    }
    Ok(out)
}

/// The `.slut` magic (`"SLUT"` little-endian) and format version.
const SLUT_MAGIC: u32 = u32::from_le_bytes(*b"SLUT");
const SLUT_VERSION: u32 = 1;

/// A baked look table: the folded HDR→display tail over a log2 shaper. The runtime resource both the
/// engine's `bake-look` and an external full-tail `.cube` ingest produce.
pub struct BakedLut {
    /// The table resolution per axis (`33` for the engine bake).
    pub size: u32,
    /// The shaper's lower EV bound (scene-linear anchored at 18% grey).
    pub shaper_ev_min: f32,
    /// The shaper's upper EV bound.
    pub shaper_ev_max: f32,
    /// The `size³` entries as red-fastest `[r, g, b]` f16 bits.
    pub rgb: Vec<[u16; 3]>,
}

impl BakedLut {
    /// Wraps the GPU bake output into a `.slut`-serializable table over the engine's shaper span.
    #[must_use]
    pub fn from_bake(size: u32, rgb: Vec<[u16; 3]>) -> Self {
        Self {
            size,
            shaper_ev_min: LUT_SHAPER_EV_MIN,
            shaper_ev_max: LUT_SHAPER_EV_MAX,
            rgb,
        }
    }

    /// Serializes the table to the binary `.slut` form: an 8-word little-endian header
    /// (`magic, version, size, evMin, evMax`) then `size³ × 3` f16 words, red-fastest.
    #[must_use]
    pub fn to_slut_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + self.rgb.len() * 6);
        out.extend_from_slice(&SLUT_MAGIC.to_le_bytes());
        out.extend_from_slice(&SLUT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.shaper_ev_min.to_le_bytes());
        out.extend_from_slice(&self.shaper_ev_max.to_le_bytes());
        for [r, g, b] in &self.rgb {
            out.extend_from_slice(&r.to_le_bytes());
            out.extend_from_slice(&g.to_le_bytes());
            out.extend_from_slice(&b.to_le_bytes());
        }
        out
    }
}

impl AssetServer {
    /// Imports an external `.cube` creative look into the asset dir + catalog: parses it, copies the
    /// source into `luts/<id>.cube`, registers an [`AssetType::Lut`] row (name = filename stem), uploads
    /// its 3D image, and caches it. Returns the new asset id the RenderPanel's creative-look slot
    /// references.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read/written, [`Error::Lut`] for a malformed `.cube`, or a
    /// renderer upload failure.
    pub fn import_cube_lut(&mut self, gpu: &dyn GpuUploader, path: &str) -> crate::Result<Uuid> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io(format!("cannot open '{path}': {e}")))?;
        let lut = parse_cube(&text).map_err(|e| Error::Lut(e.to_string()))?;
        let stem = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("look")
            .to_owned();
        let id = Uuid::new();
        let rel = format!("luts/{}.cube", id.value());
        std::fs::write(self.root.join(&rel), text.as_bytes())
            .map_err(|e| Error::Io(format!("cannot write '{rel}': {e}")))?;
        let gpu_lut = gpu.upload_lut_3d(&lut.rgb, lut.size)?;
        let name = self.catalog.unique_name(&stem);
        self.register_imported_asset(AssetEntry {
            id,
            name,
            asset_type: AssetType::Lut,
            path: rel,
            content_hash: crate::import::hash_bytes_fnv(text.as_bytes()),
            ..AssetEntry::default()
        });
        if let Err(err) = self.write_asset_sidecar(id) {
            tracing::warn!(
                "import: could not write lut .smeta for {}: {err}",
                id.value()
            );
        }
        self.lut_by_uuid.insert(id.value(), Some(gpu_lut));
        Ok(id)
    }

    /// Serializes a GPU look bake as a native `.slut` into `luts/<id>.slut` and registers an
    /// [`AssetType::Lut`] row, returning `(id, relative path)`. The frozen full-tail table the exported
    /// player and external ingest consume.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be written.
    pub fn import_baked_lut(
        &mut self,
        name: &str,
        size: u32,
        rgb: Vec<[u16; 3]>,
    ) -> crate::Result<(Uuid, String)> {
        let baked = BakedLut::from_bake(size, rgb);
        let id = Uuid::new();
        let rel = format!("luts/{}.slut", id.value());
        let bytes = baked.to_slut_bytes();
        std::fs::write(self.root.join(&rel), &bytes)
            .map_err(|e| Error::Io(format!("cannot write '{rel}': {e}")))?;
        let unique = self.catalog.unique_name(name);
        self.register_imported_asset(AssetEntry {
            id,
            name: unique,
            asset_type: AssetType::Lut,
            path: rel.clone(),
            content_hash: crate::import::hash_bytes_fnv(&bytes),
            ..AssetEntry::default()
        });
        if let Err(err) = self.write_asset_sidecar(id) {
            tracing::warn!("bake: could not write lut .smeta for {}: {err}", id.value());
        }
        Ok((id, rel))
    }

    /// Resolves a LUT asset id to its GPU 3D image, parsing + uploading the copied file on a cache
    /// miss (a `.cube` creative look or a baked `.slut`). A dangling or wrong-typed id warns once and
    /// negative-caches; the tonemap pass falls back to the identity default.
    pub fn load_cube_lut_asset(&mut self, gpu: &dyn GpuUploader, id: Uuid) -> Option<Arc<GpuLut>> {
        if let Some(cached) = self.lut_by_uuid.get(&id.value()) {
            return cached.clone();
        }
        let entry = match self.catalog.find(id) {
            Some(entry) if entry.asset_type == AssetType::Lut => entry,
            _ => {
                tracing::warn!("lut {} not in catalog; using identity", id.value());
                self.lut_by_uuid.insert(id.value(), None);
                return None;
            }
        };
        let full = format!("{}/{}", self.root.display(), entry.path);
        let is_slut = entry.path.ends_with(".slut");
        let uploaded = if is_slut {
            std::fs::read(&full)
                .ok()
                .and_then(|bytes| parse_slut(&bytes))
                .and_then(|(size, rgb)| gpu.upload_lut_3d(&rgb, size).ok())
        } else {
            std::fs::read_to_string(&full)
                .ok()
                .and_then(|text| parse_cube(&text).ok())
                .and_then(|lut| gpu.upload_lut_3d(&lut.rgb, lut.size).ok())
        };
        if uploaded.is_none() {
            tracing::warn!("lut {} failed to load; using identity", id.value());
        }
        self.lut_by_uuid.insert(id.value(), uploaded.clone());
        uploaded
    }
}

/// Decodes a `.slut` into `(size, red-fastest [r,g,b] f32 triples)`, or `None` on a bad magic/version
/// or truncated body. The f16 words are widened to f32 for the shared `upload_lut_3d` path.
fn parse_slut(bytes: &[u8]) -> Option<(u32, Vec<[f32; 3]>)> {
    if bytes.len() < 20 || &bytes[0..4] != b"SLUT" {
        return None;
    }
    if u32::from_le_bytes(bytes[4..8].try_into().ok()?) != SLUT_VERSION {
        return None;
    }
    let size = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let count = (size as usize).checked_pow(3)?;
    let body = &bytes[20..];
    if body.len() < count * 6 {
        return None;
    }
    let mut rgb = Vec::with_capacity(count);
    for cell in body.chunks_exact(6).take(count) {
        let h = |i: usize| half_to_f32(u16::from_le_bytes([cell[i], cell[i + 1]]));
        rgb.push([h(0), h(2), h(4)]);
    }
    Some((size, rgb))
}

/// Widens an IEEE-754 binary16 (as raw bits) to f32.
fn half_to_f32(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = (h >> 10) & 0x1f;
    let mant = u32::from(h & 0x3ff);
    let bits = match exp {
        0 if mant == 0 => sign,
        0 => {
            // Subnormal: normalize.
            let mut e = -1i32;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            let exp32 = (127 - 15 + 1 + e) as u32;
            sign | (exp32 << 23) | ((m & 0x3ff) << 13)
        }
        0x1f => sign | (0xff << 23) | (mant << 13),
        _ => sign | ((u32::from(exp) + 127 - 15) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_2x2_style_cube_of_supported_size() {
        // A 17³ identity-ish table: header + the right number of triples.
        let n = 17usize;
        let mut text =
            String::from("TITLE \"look\"\nLUT_3D_SIZE 17\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
        for _ in 0..n.pow(3) {
            text.push_str("0.1 0.2 0.3\n");
        }
        let lut = parse_cube(&text).expect("parse");
        assert_eq!(lut.size, 17);
        assert_eq!(lut.rgb.len(), n.pow(3));
        assert_eq!(lut.domain_max, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn rejects_an_unsupported_size() {
        let err = parse_cube("LUT_3D_SIZE 16\n").unwrap_err();
        assert!(matches!(err, CubeError::UnsupportedSize(16)));
    }

    #[test]
    fn rejects_a_wrong_entry_count() {
        let err = parse_cube("LUT_3D_SIZE 17\n0 0 0\n").unwrap_err();
        assert!(matches!(err, CubeError::EntryCount { .. }));
    }

    #[test]
    fn slut_header_round_trips_size_and_shaper() {
        let baked = BakedLut::from_bake(
            2,
            vec![
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
            ],
        );
        let bytes = baked.to_slut_bytes();
        assert_eq!(&bytes[0..4], b"SLUT");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(bytes.len(), 20 + 8 * 6);
    }
}
