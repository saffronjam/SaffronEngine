//! The `.sanim` (`SANM`) byte format: a 32-byte header, the clip name, then a
//! 24-byte record plus name/times/values per track.
//!
//! It mirrors the `.smesh` discipline: a `#[repr(C)]` Pod header and per-track record read
//! through `bytemuck`, so a `.smodel` `SANM` chunk and a standalone `.sanim` file decode the
//! same. The decode runs a bounded [`Cursor`] whose `take` returns [`Error::Truncated`] on
//! overrun, so a lying count can never drive a giant allocation.

use std::fs;
use std::path::Path;

use bytemuck::{Pod, Zeroable};

use crate::error::{Error, Result};
use crate::types::{AnimClip, AnimInterp, AnimPath, AnimTarget, AnimTrack};

/// The on-disk `.sanim` format version. One field, one accepted value.
pub const ANIM_FORMAT_VERSION: u32 = 2;

/// The four-byte tag at the head of every `.sanim` image.
const MAGIC: [u8; 4] = *b"SANM";

/// The 32-byte fixed header; the clip name follows, then the per-track sections.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct SANimHeader {
    /// `b"SANM"`.
    magic: [u8; 4],
    /// Format version; only [`ANIM_FORMAT_VERSION`] is accepted.
    version: u32,
    /// Number of tracks that follow the clip name.
    track_count: u32,
    /// Clip duration in seconds.
    duration: f32,
    /// Length in bytes of the clip name that follows the header.
    name_len: u32,
    /// Reserved, always 0.
    reserved: [u32; 3],
}

const _: () = assert!(
    size_of::<SANimHeader>() == 32,
    "SANimHeader must be exactly 32 bytes"
);

/// The 24-byte per-track record; the target name, then times, then values follow it.
///
/// `#[repr(C)]` Pod with explicit single-byte discriminants and a trailing `pad: u8`
/// so the field offsets and the 24-byte stride are fixed.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct SANimTrackRecord {
    /// Stable bone index for a `Bone` target, or `-1` for `Node`/`Weights`.
    index: i32,
    /// The [`AnimTarget`] discriminant byte.
    target: u8,
    /// The [`AnimPath`] discriminant byte.
    path: u8,
    /// The [`AnimInterp`] discriminant byte.
    interp: u8,
    /// Explicit pad to a 4-byte boundary, always 0.
    pad: u8,
    /// Weights-per-keyframe for a `Weights` track (the morph-target count); else 0.
    morph_count: u32,
    /// Length in bytes of the target name that follows the record.
    name_len: u32,
    /// Number of `f32` keyframe times that follow the target name.
    time_count: u32,
    /// Number of `f32` keyframe values that follow the times.
    value_count: u32,
}

const _: () = assert!(
    size_of::<SANimTrackRecord>() == 24,
    "SANimTrackRecord must be exactly 24 bytes"
);

/// A bounded forward cursor over a byte slice.
///
/// `take(n)` returns the next `n` bytes and advances, or [`Error::Truncated`] if the
/// slice has fewer than `n` bytes left — so every length read from the header/records
/// is checked against the real buffer before it can drive an allocation.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    /// Returns the next `n` bytes and advances the cursor, or [`Error::Truncated`].
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
}

/// Reads a tightly packed little-endian `f32` array from `bytes`.
///
/// `bytes.len()` is a multiple of 4 (the cursor took an exact `count * 4` span); any
/// trailing partial chunk is ignored by `chunks_exact`.
fn read_f32_le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Builds the `.sanim` byte image for `clip`.
///
/// Layout: the 32-byte header, the clip name bytes, then per track the 24-byte record
/// followed by the target name bytes, the `times` floats, and the `values` floats.
pub fn save_animation_to_buffer(clip: &AnimClip) -> Vec<u8> {
    let header = SANimHeader {
        magic: MAGIC,
        version: ANIM_FORMAT_VERSION,
        track_count: clip.tracks.len() as u32,
        duration: clip.duration,
        name_len: clip.name.len() as u32,
        reserved: [0, 0, 0],
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(bytemuck::bytes_of(&header));
    bytes.extend_from_slice(clip.name.as_bytes());
    for track in &clip.tracks {
        let record = SANimTrackRecord {
            index: track.index,
            target: track.target as u8,
            path: track.path as u8,
            interp: track.interp as u8,
            pad: 0,
            morph_count: track.morph_count,
            name_len: track.target_name.len() as u32,
            time_count: track.times.len() as u32,
            value_count: track.values.len() as u32,
        };
        bytes.extend_from_slice(bytemuck::bytes_of(&record));
        bytes.extend_from_slice(track.target_name.as_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&track.times));
        bytes.extend_from_slice(bytemuck::cast_slice(&track.values));
    }
    bytes
}

/// Decodes a `.sanim` byte image into an [`AnimClip`].
///
/// Validates the magic and version, then walks a bounded [`Cursor`]: the clip name,
/// and per track the record (mapping the `path`/`interp` bytes through
/// [`AnimPath::from_u8`]/[`AnimInterp::from_u8`]), the joint name, the times, and the
/// values. Any overrun is [`Error::Truncated`]; a bad discriminant is
/// [`Error::BadLayout`].
pub fn load_animation_from_bytes(bytes: &[u8]) -> Result<AnimClip> {
    let head = bytes
        .get(..size_of::<SANimHeader>())
        .ok_or(Error::Truncated)?;
    // `pod_read_unaligned`, not `from_bytes`: a `.smodel` `SANM` chunk slice may begin
    // at an unaligned offset, so the header read must not assume `u32` alignment.
    let header: SANimHeader = bytemuck::pod_read_unaligned(head);
    if header.magic != MAGIC {
        return Err(Error::BadMagic);
    }
    if header.version != ANIM_FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(header.version));
    }

    let mut cursor = Cursor::new(bytes, size_of::<SANimHeader>());

    let name_bytes = cursor.take(header.name_len as usize)?;
    let name = String::from_utf8_lossy(name_bytes).into_owned();

    let mut tracks = Vec::with_capacity(header.track_count as usize);
    for _ in 0..header.track_count {
        let record: SANimTrackRecord =
            bytemuck::pod_read_unaligned(cursor.take(size_of::<SANimTrackRecord>())?);

        let name_bytes = cursor.take(record.name_len as usize)?;
        let target_name = String::from_utf8_lossy(name_bytes).into_owned();

        // The float sections start at name-dependent offsets, so they are not
        // guaranteed `f32`-aligned in the byte image; read each as little-endian
        // (the frozen LE contract) so no alignment is assumed.
        let times_bytes = cursor.take(record.time_count as usize * size_of::<f32>())?;
        let times = read_f32_le(times_bytes);

        let values_bytes = cursor.take(record.value_count as usize * size_of::<f32>())?;
        let values = read_f32_le(values_bytes);

        tracks.push(AnimTrack {
            target: AnimTarget::from_u8(record.target)?,
            index: record.index,
            target_name,
            path: AnimPath::from_u8(record.path)?,
            interp: AnimInterp::from_u8(record.interp)?,
            morph_count: record.morph_count,
            times,
            values,
        });
    }

    Ok(AnimClip {
        name,
        duration: header.duration,
        tracks,
    })
}

/// Encodes `clip` and writes the `.sanim` image to `path`.
pub fn save_animation(clip: &AnimClip, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let bytes = save_animation_to_buffer(clip);
    fs::write(path, &bytes).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))
}

/// Reads a `.sanim` file and decodes its clip.
pub fn load_animation(path: impl AsRef<Path>) -> Result<AnimClip> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    load_animation_from_bytes(&bytes)
        .map_err(|e| Error::Import(format!("'{}': {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_clip() -> AnimClip {
        AnimClip {
            name: "Walk".to_string(),
            duration: 1.5,
            tracks: vec![
                AnimTrack {
                    target: AnimTarget::Bone,
                    index: 3,
                    target_name: "Hip".to_string(),
                    path: AnimPath::Rotation,
                    interp: AnimInterp::Linear,
                    morph_count: 0,
                    // Two quaternion keys (xyzw), 4 floats each.
                    times: vec![0.0, 1.5],
                    values: vec![0.0, 0.0, 0.0, 1.0, 0.1, 0.2, 0.3, 0.9],
                },
                AnimTrack {
                    target: AnimTarget::Bone,
                    index: 7,
                    target_name: "Foot".to_string(),
                    path: AnimPath::Translation,
                    interp: AnimInterp::Step,
                    morph_count: 0,
                    // Two translation keys (xyz), 3 floats each.
                    times: vec![0.0, 1.5],
                    values: vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
                },
            ],
        }
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let clip = sample_clip();
        let baked = save_animation_to_buffer(&clip);
        let loaded = load_animation_from_bytes(&baked).unwrap();

        assert_eq!(loaded.name, clip.name);
        assert_eq!(loaded.duration, clip.duration);
        assert_eq!(loaded.tracks.len(), clip.tracks.len());
        for (got, want) in loaded.tracks.iter().zip(&clip.tracks) {
            assert_eq!(got.target, want.target);
            assert_eq!(got.index, want.index);
            assert_eq!(got.target_name, want.target_name);
            assert_eq!(got.path, want.path);
            assert_eq!(got.interp, want.interp);
            assert_eq!(got.morph_count, want.morph_count);
            assert_eq!(got.times, want.times);
            assert_eq!(got.values, want.values);
        }
        assert_eq!(loaded, clip);
    }

    #[test]
    fn empty_clip_round_trips() {
        let clip = AnimClip::default();
        let baked = save_animation_to_buffer(&clip);
        // Header only: no name, no tracks.
        assert_eq!(baked.len(), 32);
        let loaded = load_animation_from_bytes(&baked).unwrap();
        assert_eq!(loaded, clip);
    }

    #[test]
    fn golden_bytes_header_and_record_are_frozen() {
        let clip = sample_clip();
        let baked = save_animation_to_buffer(&clip);

        let header: SANimHeader = bytemuck::pod_read_unaligned(&baked[..32]);
        assert_eq!(&header.magic, b"SANM");
        assert_eq!(header.version, 2);
        assert_eq!(header.track_count, 2);
        assert_eq!(header.duration, 1.5);
        assert_eq!(header.name_len, 4); // "Walk"
        assert_eq!(header.reserved, [0, 0, 0]);

        // The clip name follows the header verbatim.
        assert_eq!(&baked[32..36], b"Walk");

        // The first track record begins right after the name (24 bytes wide).
        let record: SANimTrackRecord = bytemuck::pod_read_unaligned(&baked[36..60]);
        assert_eq!(record.index, 3);
        assert_eq!(record.target, AnimTarget::Bone as u8);
        assert_eq!(record.target, 0);
        assert_eq!(record.path, AnimPath::Rotation as u8);
        assert_eq!(record.path, 1);
        assert_eq!(record.interp, AnimInterp::Linear as u8);
        assert_eq!(record.interp, 1);
        assert_eq!(record.pad, 0);
        assert_eq!(record.morph_count, 0);
        assert_eq!(record.name_len, 3); // "Hip"
        assert_eq!(record.time_count, 2);
        assert_eq!(record.value_count, 8);

        // The total length is exactly the sum of every section, no padding.
        let track0 = 24 + 3 + 2 * 4 + 8 * 4; // record + "Hip" + 2 times + 8 values
        let track1 = 24 + 4 + 2 * 4 + 6 * 4; // record + "Foot" + 2 times + 6 values
        assert_eq!(baked.len(), 32 + 4 + track0 + track1);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let clip = sample_clip();
        let mut baked = save_animation_to_buffer(&clip);
        baked[0] = b'X';
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::BadMagic)
        ));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let clip = sample_clip();
        // The superseded v1 record layout is rejected outright (no dual-version reader).
        let mut baked = save_animation_to_buffer(&clip);
        baked[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::UnsupportedVersion(1))
        ));
        // A future version is rejected too.
        let mut baked = save_animation_to_buffer(&clip);
        baked[4..8].copy_from_slice(&3u32.to_le_bytes());
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::UnsupportedVersion(3))
        ));
    }

    #[test]
    fn truncated_header_is_rejected() {
        let clip = sample_clip();
        let baked = save_animation_to_buffer(&clip);
        assert!(matches!(
            load_animation_from_bytes(&baked[..16]),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn huge_name_len_over_short_buffer_is_truncated() {
        // A header claiming a 4 GB clip name over a 32-byte buffer must reject without
        // allocating, not panic or blow up.
        let mut baked = save_animation_to_buffer(&AnimClip::default());
        baked[16..20].copy_from_slice(&u32::MAX.to_le_bytes()); // name_len
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn huge_track_count_over_short_buffer_is_truncated() {
        let mut baked = save_animation_to_buffer(&AnimClip::default());
        baked[8..12].copy_from_slice(&1_000_000u32.to_le_bytes()); // track_count
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn huge_time_count_over_short_buffer_is_truncated() {
        let clip = sample_clip();
        let mut baked = save_animation_to_buffer(&clip);
        // The first track record starts at 36; time_count is at record offset 16.
        baked[36 + 16..36 + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn out_of_range_target_byte_is_bad_layout() {
        let clip = sample_clip();
        let mut baked = save_animation_to_buffer(&clip);
        // The first record's `target` byte is at offset 36 + 4.
        baked[36 + 4] = 9;
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::BadLayout)
        ));
    }

    #[test]
    fn out_of_range_path_byte_is_bad_layout() {
        let clip = sample_clip();
        let mut baked = save_animation_to_buffer(&clip);
        // The first record's `path` byte is at offset 36 + 5.
        baked[36 + 5] = 9;
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::BadLayout)
        ));
    }

    #[test]
    fn out_of_range_interp_byte_is_bad_layout() {
        let clip = sample_clip();
        let mut baked = save_animation_to_buffer(&clip);
        // The first record's `interp` byte is at offset 36 + 6.
        baked[36 + 6] = 9;
        assert!(matches!(
            load_animation_from_bytes(&baked),
            Err(Error::BadLayout)
        ));
    }

    #[test]
    fn file_round_trip() {
        let clip = sample_clip();
        let path =
            std::env::temp_dir().join(format!("saffron-sanim-test-{}.sanim", std::process::id()));
        save_animation(&clip, &path).unwrap();
        let loaded = load_animation(&path).unwrap();
        assert_eq!(loaded, clip);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = std::env::temp_dir().join("saffron-sanim-does-not-exist.sanim");
        assert!(matches!(load_animation(&path), Err(Error::Io(_))));
    }
}
