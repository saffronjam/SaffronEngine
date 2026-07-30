//! The KTX2 texture container one `.splantc` texture section carries.
//!
//! Every field is derived from the format, the base extent, and the level payloads, so the same
//! texels always encode to the same bytes — the artifact hash depends on it.

use crate::{Error, Result};

const FORMAT: &str = ".splantc texture";

/// `«KTX 20»\r\n\x1A\n`, the container's identifier.
const IDENTIFIER: [u8; 12] = [
    0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];
/// Identifier, format and extent words, the two byte-range indices, and the supercompression
/// scheme.
const HEADER_BYTES: usize = 80;
/// One level index entry: stored offset, stored length, decoded length.
const LEVEL_ENTRY_BYTES: usize = 24;
/// A basic data-format descriptor for an unpacked 8-bit RGBA format: the total-size word, the
/// 24-byte descriptor block, and one 16-byte sample per channel.
const DFD_BYTES: usize = 4 + 24 + 16 * 4;
/// Level payloads align to `lcm(texel block size, 4)`, which is 4 for every 8-bit RGBA format.
const LEVEL_ALIGNMENT: usize = 4;
/// Key/value entries, in the ascending key order the container requires.
///
/// `rd` states that level 0's first texel is the top-left one and rows run downwards, which is how
/// the cooker's row-major RGBA buffers are laid out; without it a viewer is free to flip the image.
const KEY_VALUES: [&[u8]; 2] = [b"KTXorientation\0rd\0", b"KTXwriter\0saffron-anima\0"];

/// The texel formats a `.splantc` texture container may declare, by `VkFormat` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum PlantTextureFormat {
    /// `VK_FORMAT_R8G8B8A8_UNORM`: linearly encoded 8-bit RGBA.
    Rgba8Unorm = 37,
    /// `VK_FORMAT_R8G8B8A8_SRGB`: sRGB-encoded colour with linear alpha.
    Rgba8Srgb = 43,
}

impl PlantTextureFormat {
    /// The `VkFormat` value the container declares.
    #[must_use]
    pub const fn vk_format(self) -> u32 {
        self as u32
    }

    /// Whether the texels are sRGB-encoded, which an upload has to match or the colour shifts.
    #[must_use]
    pub const fn is_srgb(self) -> bool {
        matches!(self, Self::Rgba8Srgb)
    }

    /// Bytes one texel occupies.
    #[must_use]
    pub const fn texel_bytes(self) -> u32 {
        4
    }

    fn from_vk_format(value: u32) -> Result<Self> {
        match value {
            37 => Ok(Self::Rgba8Unorm),
            43 => Ok(Self::Rgba8Srgb),
            _ => Err(Error::ArtifactFormat {
                format: FORMAT,
                field: "vkFormat".to_owned(),
            }),
        }
    }

    /// The transfer function the data-format descriptor records: sRGB or linear.
    const fn transfer_function(self) -> u8 {
        if self.is_srgb() { 2 } else { 1 }
    }
}

/// One mip-chained 2D texture, as the container holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantTextureContainer<'a> {
    /// Declared texel format.
    pub format: PlantTextureFormat,
    /// Level-0 width in texels.
    pub width: u32,
    /// Level-0 height in texels.
    pub height: u32,
    /// Level payloads, level 0 first.
    ///
    /// A level's extent is `max(1, base >> level)` rather than a stored field, so a payload whose
    /// length disagrees with its derived extent is refused instead of reinterpreted.
    pub levels: Vec<&'a [u8]>,
}

impl PlantTextureContainer<'_> {
    /// The extent of `level`, or `None` past the stored chain.
    #[must_use]
    pub fn level_extent(&self, level: usize) -> Option<(u32, u32)> {
        (level < self.levels.len()).then(|| level_extent(self.width, self.height, level))
    }

    /// Whether the stored chain reaches 1×1. A sampler that reaches a level the chain stops short
    /// of has nothing to read there.
    #[must_use]
    pub fn is_mip_complete(&self) -> bool {
        self.levels.len() == mip_chain_length(self.width, self.height)
    }
}

/// Encodes one texture as a KTX2 container.
///
/// # Errors
///
/// Returns [`Error::ArtifactFormat`] when the extent is zero, the chain is empty or longer than the
/// base extent allows, or a level payload does not match its derived extent.
pub fn write_plant_texture_container(texture: &PlantTextureContainer<'_>) -> Result<Vec<u8>> {
    let layout = Layout::of(texture.width, texture.height, texture.levels.len())?;
    for (level, payload) in texture.levels.iter().enumerate() {
        if payload.len() != level_bytes(texture.width, texture.height, level)? {
            return Err(Error::ArtifactFormat {
                format: FORMAT,
                field: format!("level{level}.byteLength"),
            });
        }
    }
    let mut bytes = Vec::with_capacity(layout.total);
    bytes.extend_from_slice(&IDENTIFIER);
    for word in [
        texture.format.vk_format(),
        1,
        texture.width,
        texture.height,
        0,
        0,
        1,
        word(texture.levels.len())?,
        0,
        word(layout.dfd_offset)?,
        word(DFD_BYTES)?,
        word(layout.kvd_offset)?,
        word(layout.kvd_bytes)?,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    // No supercompression global data: the section codec frames the payload at the artifact's one
    // pinned zstd profile, and a second scheme inside would be a second framing to keep pinned.
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    for (level, payload) in texture.levels.iter().enumerate() {
        let length = u64::try_from(payload.len()).map_err(|_| Error::NumericOverflow)?;
        let offset =
            u64::try_from(layout.level_offsets[level]).map_err(|_| Error::NumericOverflow)?;
        for value in [offset, length, length] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&data_format_descriptor(texture.format));
    for entry in KEY_VALUES {
        bytes.extend_from_slice(&word(entry.len())?.to_le_bytes());
        bytes.extend_from_slice(entry);
        let padded = align_up(bytes.len(), 4)?;
        pad_to(&mut bytes, padded)?;
    }
    // Smallest level first: a container read only as far as its first levels still yields a
    // complete low-resolution image, which is the order it is defined to be stored in.
    for level in (0..texture.levels.len()).rev() {
        pad_to(&mut bytes, layout.level_offsets[level])?;
        bytes.extend_from_slice(texture.levels[level]);
    }
    if bytes.len() != layout.total {
        return Err(Error::ArtifactFormat {
            format: FORMAT,
            field: "totalSize".to_owned(),
        });
    }
    Ok(bytes)
}

/// Decodes one KTX2 container, borrowing its level payloads.
///
/// Accepts exactly the encoding [`write_plant_texture_container`] produces: every index word,
/// descriptor byte, key/value entry, and padding byte is compared against the canonical layout for
/// the declared format and extent, so a container these bytes were not written as is rejected
/// rather than reinterpreted.
///
/// # Errors
///
/// Returns [`Error::ArtifactFormat`] or [`Error::ArtifactTruncated`] for any deviation.
pub fn read_plant_texture_container(bytes: &[u8]) -> Result<PlantTextureContainer<'_>> {
    let mut reader = LeReader::new(bytes);
    reader.expect(&IDENTIFIER, "identifier")?;
    let format = PlantTextureFormat::from_vk_format(reader.u32()?)?;
    let field = |name: &str| Error::ArtifactFormat {
        format: FORMAT,
        field: name.to_owned(),
    };
    if reader.u32()? != 1 {
        return Err(field("typeSize"));
    }
    let width = reader.u32()?;
    let height = reader.u32()?;
    if width == 0 || height == 0 {
        return Err(field("pixelExtent"));
    }
    if reader.u32()? != 0 {
        return Err(field("pixelDepth"));
    }
    if reader.u32()? != 0 {
        return Err(field("layerCount"));
    }
    if reader.u32()? != 1 {
        return Err(field("faceCount"));
    }
    let levels = usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)?;
    if reader.u32()? != 0 {
        return Err(field("supercompressionScheme"));
    }
    let layout = Layout::of(width, height, levels)?;
    if usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)? != layout.dfd_offset
        || usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)? != DFD_BYTES
    {
        return Err(field("dataFormatDescriptorIndex"));
    }
    if usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)? != layout.kvd_offset
        || usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)? != layout.kvd_bytes
    {
        return Err(field("keyValueIndex"));
    }
    if reader.u64()? != 0 || reader.u64()? != 0 {
        return Err(field("supercompressionGlobalDataIndex"));
    }
    let mut spans = Vec::with_capacity(levels);
    for level in 0..levels {
        let offset = usize::try_from(reader.u64()?).map_err(|_| Error::NumericOverflow)?;
        let stored = usize::try_from(reader.u64()?).map_err(|_| Error::NumericOverflow)?;
        let decoded = usize::try_from(reader.u64()?).map_err(|_| Error::NumericOverflow)?;
        let expected = level_bytes(width, height, level)?;
        if offset != layout.level_offsets[level] || stored != expected || decoded != expected {
            return Err(field(&format!("level{level}")));
        }
        spans.push(offset..offset + stored);
    }
    if reader.take(DFD_BYTES)? != data_format_descriptor(format) {
        return Err(field("dataFormatDescriptor"));
    }
    for entry in KEY_VALUES {
        if usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)? != entry.len() {
            return Err(field("keyValueLength"));
        }
        if reader.take(entry.len())? != entry {
            return Err(field("keyValue"));
        }
        reader.expect_zeroes(
            align_up(reader.offset, 4)? - reader.offset,
            "keyValuePadding",
        )?;
    }
    if bytes.len() != layout.total {
        return Err(Error::ArtifactFormat {
            format: FORMAT,
            field: "totalSize".to_owned(),
        });
    }
    let mut payloads = vec![&bytes[..0]; levels];
    for level in (0..levels).rev() {
        let span = spans[level].clone();
        reader.expect_zeroes(span.start - reader.offset, "levelPadding")?;
        payloads[level] = reader.take(span.end - span.start)?;
    }
    reader.complete()?;
    Ok(PlantTextureContainer {
        format,
        width,
        height,
        levels: payloads,
    })
}

/// Byte positions every part of a container lands at, for one format and extent.
struct Layout {
    dfd_offset: usize,
    kvd_offset: usize,
    kvd_bytes: usize,
    /// Stored offset per level, level 0 first.
    level_offsets: Vec<usize>,
    total: usize,
}

impl Layout {
    fn of(width: u32, height: u32, levels: usize) -> Result<Self> {
        if width == 0 || height == 0 || levels == 0 || levels > mip_chain_length(width, height) {
            return Err(Error::ArtifactFormat {
                format: FORMAT,
                field: "levelCount".to_owned(),
            });
        }
        let dfd_offset = HEADER_BYTES
            .checked_add(
                levels
                    .checked_mul(LEVEL_ENTRY_BYTES)
                    .ok_or(Error::NumericOverflow)?,
            )
            .ok_or(Error::NumericOverflow)?;
        let kvd_offset = dfd_offset
            .checked_add(DFD_BYTES)
            .ok_or(Error::NumericOverflow)?;
        let kvd_bytes = KEY_VALUES.iter().try_fold(0_usize, |total, entry| {
            let entry = align_up(4 + entry.len(), 4)?;
            total.checked_add(entry).ok_or(Error::NumericOverflow)
        })?;
        let mut cursor = kvd_offset
            .checked_add(kvd_bytes)
            .ok_or(Error::NumericOverflow)?;
        let mut level_offsets = vec![0_usize; levels];
        for level in (0..levels).rev() {
            cursor = align_up(cursor, LEVEL_ALIGNMENT)?;
            level_offsets[level] = cursor;
            cursor = cursor
                .checked_add(level_bytes(width, height, level)?)
                .ok_or(Error::NumericOverflow)?;
        }
        Ok(Self {
            dfd_offset,
            kvd_offset,
            kvd_bytes,
            level_offsets,
            total: cursor,
        })
    }
}

/// The basic data-format descriptor for an unpacked 8-bit RGBA format: one 8-bit sample per
/// channel over a single-texel block, with straight (non-premultiplied) alpha.
fn data_format_descriptor(format: PlantTextureFormat) -> [u8; DFD_BYTES] {
    let mut bytes = [0_u8; DFD_BYTES];
    let mut cursor = 0_usize;
    let mut push = |values: &[u8]| {
        bytes[cursor..cursor + values.len()].copy_from_slice(values);
        cursor += values.len();
    };
    let block_bytes = u16::try_from(DFD_BYTES - 4).expect("descriptor block fits");
    push(
        &u32::try_from(DFD_BYTES)
            .expect("descriptor fits")
            .to_le_bytes(),
    );
    // Khronos vendor, basic descriptor type, descriptor version 2.
    push(&0_u32.to_le_bytes());
    push(&2_u16.to_le_bytes());
    push(&block_bytes.to_le_bytes());
    // RGBSDA colour model, BT.709 primaries, straight alpha, one-texel block.
    push(&[1, 1, format.transfer_function(), 0]);
    push(&[0, 0, 0, 0]);
    push(&[format.texel_bytes() as u8, 0, 0, 0, 0, 0, 0, 0]);
    for (channel, bit_offset) in [(0_u8, 0_u16), (1, 8), (2, 16), (15, 24)] {
        // sRGB colour with linear alpha: the alpha sample carries the linear qualifier so a
        // consumer does not run the transfer function over coverage.
        let linear = if format.is_srgb() && channel == 15 {
            0x10
        } else {
            0
        };
        push(&bit_offset.to_le_bytes());
        push(&[7, channel | linear, 0, 0, 0, 0]);
        push(&0_u32.to_le_bytes());
        push(&255_u32.to_le_bytes());
    }
    debug_assert_eq!(cursor, DFD_BYTES);
    bytes
}

/// Levels a complete chain from `width` × `height` down to 1×1 carries.
fn mip_chain_length(width: u32, height: u32) -> usize {
    let mut levels = 1;
    let (mut width, mut height) = (width, height);
    while width > 1 || height > 1 {
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        levels += 1;
    }
    levels
}

fn level_extent(width: u32, height: u32, level: usize) -> (u32, u32) {
    let shift = u32::try_from(level).unwrap_or(u32::MAX).min(31);
    ((width >> shift).max(1), (height >> shift).max(1))
}

fn level_bytes(width: u32, height: u32, level: usize) -> Result<usize> {
    let (width, height) = level_extent(width, height, level);
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|texels| texels.checked_mul(4))
        .ok_or(Error::NumericOverflow)?;
    usize::try_from(bytes).map_err(|_| Error::NumericOverflow)
}

fn word(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::NumericOverflow)
}

fn align_up(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(Error::NumericOverflow)
}

fn pad_to(bytes: &mut Vec<u8>, offset: usize) -> Result<()> {
    if bytes.len() > offset {
        return Err(Error::ArtifactFormat {
            format: FORMAT,
            field: "padding".to_owned(),
        });
    }
    bytes.resize(offset, 0);
    Ok(())
}

/// The container's words are little-endian, unlike every other format in this crate.
struct LeReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> LeReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(Error::NumericOverflow)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(Error::ArtifactTruncated { format: FORMAT })?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn expect(&mut self, expected: &[u8], field: &str) -> Result<()> {
        if self.take(expected.len())? == expected {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: FORMAT,
                field: field.to_owned(),
            })
        }
    }

    fn expect_zeroes(&mut self, count: usize, field: &str) -> Result<()> {
        if self.take(count)?.iter().all(|byte| *byte == 0) {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: FORMAT,
                field: field.to_owned(),
            })
        }
    }

    fn complete(&self) -> Result<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::ArtifactFormat {
                format: FORMAT,
                field: "trailingBytes".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full chain from `width` × `height`, every level filled with its own byte so a reader that
    /// returned the wrong span is visible rather than plausible.
    fn chain(width: u32, height: u32) -> Vec<Vec<u8>> {
        (0..mip_chain_length(width, height))
            .map(|level| {
                let bytes = level_bytes(width, height, level).expect("level size");
                vec![u8::try_from(level + 1).expect("level tag"); bytes]
            })
            .collect()
    }

    fn container<'a>(levels: &'a [Vec<u8>], width: u32, height: u32) -> PlantTextureContainer<'a> {
        PlantTextureContainer {
            format: PlantTextureFormat::Rgba8Srgb,
            width,
            height,
            levels: levels.iter().map(Vec::as_slice).collect(),
        }
    }

    #[test]
    fn a_written_container_round_trips_its_format_extent_and_every_level() {
        let levels = chain(8, 4);
        let source = container(&levels, 8, 4);
        let bytes = write_plant_texture_container(&source).expect("encode");
        let decoded = read_plant_texture_container(&bytes).expect("decode");
        assert_eq!(decoded, source);
        assert!(decoded.is_mip_complete());
        assert_eq!(decoded.level_extent(0), Some((8, 4)));
        assert_eq!(decoded.level_extent(2), Some((2, 1)));
        assert_eq!(decoded.level_extent(3), Some((1, 1)));
        assert_eq!(decoded.level_extent(4), None);
        // The identifier and the declared format sit where the container defines them, so a viewer
        // that never heard of this cooker can open the payload.
        assert_eq!(&bytes[..12], &IDENTIFIER);
        assert_eq!(
            u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            43,
            "VK_FORMAT_R8G8B8A8_SRGB"
        );
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 4);
        let unorm = PlantTextureContainer {
            format: PlantTextureFormat::Rgba8Unorm,
            ..source
        };
        let unorm_bytes = write_plant_texture_container(&unorm).expect("encode");
        assert_eq!(
            read_plant_texture_container(&unorm_bytes)
                .expect("decode")
                .format,
            PlantTextureFormat::Rgba8Unorm,
            "the declared colour space survives the round trip"
        );
        assert_ne!(
            unorm_bytes, bytes,
            "the transfer function reaches the encoded bytes"
        );
    }

    #[test]
    fn levels_are_stored_smallest_first() {
        // The storage order the container defines: a reader that stops early still has a complete
        // small image. A writer that stored level 0 first would round-trip through this module and
        // hand every other KTX2 consumer levels it cannot use.
        let levels = chain(16, 16);
        let bytes = write_plant_texture_container(&container(&levels, 16, 16)).expect("encode");
        let offsets: Vec<u64> = (0..levels.len())
            .map(|level| {
                let at = HEADER_BYTES + level * LEVEL_ENTRY_BYTES;
                u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
            })
            .collect();
        assert!(
            offsets.windows(2).all(|pair| pair[0] > pair[1]),
            "level offsets must descend with level: {offsets:?}"
        );
        let smallest = *offsets.last().expect("a chain");
        assert!(
            offsets.iter().all(|offset| *offset >= smallest),
            "the last level is stored first"
        );
    }

    #[test]
    fn a_level_that_does_not_match_its_derived_extent_is_refused() {
        // Level extents derive from the base extent, so a short or long payload is the one way a
        // container could describe texels it does not carry.
        let mut levels = chain(4, 4);
        levels[1].push(0);
        assert!(write_plant_texture_container(&container(&levels, 4, 4)).is_err());
        let levels = chain(4, 4);
        assert!(
            write_plant_texture_container(&PlantTextureContainer {
                format: PlantTextureFormat::Rgba8Srgb,
                width: 0,
                height: 4,
                levels: levels.iter().map(Vec::as_slice).collect(),
            })
            .is_err(),
            "a zero extent has no texels to describe"
        );
        assert!(
            write_plant_texture_container(&PlantTextureContainer {
                format: PlantTextureFormat::Rgba8Srgb,
                width: 4,
                height: 4,
                levels: Vec::new(),
            })
            .is_err(),
            "an empty chain is not a texture"
        );
        let overlong: Vec<Vec<u8>> = chain(2, 2).into_iter().chain([vec![0; 4]]).collect();
        assert!(
            write_plant_texture_container(&container(&overlong, 2, 2)).is_err(),
            "a chain longer than the base extent allows is refused"
        );
    }

    #[test]
    fn a_truncated_chain_decodes_but_is_not_mip_complete() {
        // A short chain is a legal container and an unusable atlas: the consumer that needs every
        // level asks, rather than the codec guessing on its behalf.
        let full = chain(8, 8);
        let mut short = container(&full, 8, 8);
        short.levels.truncate(2);
        let bytes = write_plant_texture_container(&short).expect("encode");
        let decoded = read_plant_texture_container(&bytes).expect("decode");
        assert_eq!(decoded.levels.len(), 2);
        assert!(!decoded.is_mip_complete());
        assert!(container(&full, 8, 8).is_mip_complete());
    }

    #[test]
    fn every_framing_field_is_load_bearing() {
        // The crate's rule for a derived artifact: an unknown or corrupt payload is rejected, never
        // reinterpreted. Each mutation below is one field a lenient reader would skip over.
        let levels = chain(4, 4);
        let bytes = write_plant_texture_container(&container(&levels, 4, 4)).expect("encode");
        let level_index = HEADER_BYTES;
        let key_values = HEADER_BYTES + levels.len() * LEVEL_ENTRY_BYTES + DFD_BYTES;
        for (field, mutate) in [
            ("identifier", 0_usize),
            ("vkFormat", 12),
            ("typeSize", 16),
            ("pixelWidth", 20),
            ("pixelDepth", 28),
            ("layerCount", 32),
            ("faceCount", 36),
            ("levelCount", 40),
            ("supercompressionScheme", 44),
            ("dfdByteOffset", 48),
            ("kvdByteOffset", 56),
            ("sgdByteOffset", 64),
            ("levelOffset", level_index),
            ("levelByteLength", level_index + 8),
            ("levelUncompressedByteLength", level_index + 16),
            (
                "dataFormatDescriptor",
                HEADER_BYTES + levels.len() * LEVEL_ENTRY_BYTES + 8,
            ),
            ("keyValueLength", key_values),
            ("orientation", key_values + 4 + 15),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[mutate] ^= 0x01;
            assert!(
                read_plant_texture_container(&corrupt).is_err(),
                "{field} at byte {mutate} is not checked"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(
            read_plant_texture_container(&trailing).is_err(),
            "trailing bytes are accepted"
        );
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(read_plant_texture_container(&truncated).is_err());
        assert!(read_plant_texture_container(&[]).is_err());
        assert!(
            read_plant_texture_container(&bytes).is_ok(),
            "the unmutated container still decodes"
        );
    }
}
