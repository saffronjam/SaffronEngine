//! The strict container both derived artifacts share: a fixed header, a sorted TOC of
//! independently addressable sections, and the aligned payload their offsets point into.

use std::borrow::Cow;

use super::plant::plant_section_version;
use super::section::{decode_stored_section, encode_section, validate_decoded_section};
use super::{ArtifactDecodeLimits, PlantCompiledSectionKind, VegetationCellSectionKind};
use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    ContentHash, Error, PLANT_COMPILED_ARTIFACT_VERSION, Result, VEGETATION_CELL_ARTIFACT_VERSION,
    VEGETATION_CELL_SECTION_VERSION,
};

pub(super) const CELL_MAGIC: &[u8; 8] = b"SVEGCEL1";
pub(super) const PLANT_MAGIC: &[u8; 8] = b"SPLANTC2";
pub(super) const COMMON_HEADER_BYTES: usize = 8 + 4 + 32 + 2 + 32 + 32 + 4 + 32;
pub(super) const TOC_ENTRY_BYTES: usize = 2 + 4 + 1 + 4 + 8 + 8 + 8 + 32;
pub(super) const MAX_SECTION_ALIGNMENT: u32 = 4096;

/// Stable schema identity of the `.svegcell` header and TOC vocabulary.
#[must_use]
pub fn vegetation_cell_artifact_schema_hash() -> ContentHash {
    ContentHash::of(b"saffron-anima/svegcell/schema/v1/strict-toc+cell+cook+platform+stored-payload-hash+raw-or-zstd-checksummed-sections+decoded-size-and-content-hash")
}

/// Stable schema identity of the `.splantc` header and TOC vocabulary.
#[must_use]
pub fn plant_compiled_artifact_schema_hash() -> ContentHash {
    ContentHash::of(b"saffron-anima/splantc/schema/v4/strict-toc+all-17-sections+family-atlas-layout+ktx2-texture-container+family+cook+platform+stored-payload-hash+raw-or-zstd-checksummed-sections+decoded-size-and-content-hash+family-tags+portable-triangle-voxel-hierarchy+deformation+pages+ray-tracing")
}

/// Exact section-storage codec recorded in a derived artifact TOC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ArtifactSectionCodec {
    /// Canonical bytes stored directly.
    #[default]
    Raw = 0,
    /// One deterministic checksummed Zstandard frame without a dictionary.
    Zstd = 1,
}

impl ArtifactSectionCodec {
    pub(crate) fn from_id(format: &'static str, id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::Raw),
            1 => Ok(Self::Zstd),
            codec => Err(Error::ArtifactUnknownCodec { format, codec }),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ArtifactFlavor {
    Cell,
    Plant,
}

impl ArtifactFlavor {
    pub(super) fn format(self) -> &'static str {
        match self {
            Self::Cell => ".svegcell",
            Self::Plant => ".splantc",
        }
    }

    pub(super) fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::Cell => CELL_MAGIC,
            Self::Plant => PLANT_MAGIC,
        }
    }

    pub(super) fn version(self) -> u32 {
        match self {
            Self::Cell => VEGETATION_CELL_ARTIFACT_VERSION,
            Self::Plant => PLANT_COMPILED_ARTIFACT_VERSION,
        }
    }

    pub(super) fn schema(self) -> ContentHash {
        match self {
            Self::Cell => vegetation_cell_artifact_schema_hash(),
            Self::Plant => plant_compiled_artifact_schema_hash(),
        }
    }

    pub(super) fn section_version(self, kind: u16) -> Result<u32> {
        match self {
            Self::Cell => {
                VegetationCellSectionKind::from_id(kind)?;
                Ok(VEGETATION_CELL_SECTION_VERSION)
            }
            Self::Plant => Ok(plant_section_version(PlantCompiledSectionKind::from_id(
                kind,
            )?)),
        }
    }

    pub(super) fn identity_size(self) -> usize {
        match self {
            Self::Cell => 25,
            Self::Plant => 8,
        }
    }

    pub(super) fn maximum_sections(self) -> usize {
        match self {
            Self::Cell => VegetationCellSectionKind::ALL.len(),
            Self::Plant => PlantCompiledSectionKind::ALL.len(),
        }
    }

    pub(super) fn validate_kind(self, kind: u16) -> Result<()> {
        match self {
            Self::Cell => VegetationCellSectionKind::from_id(kind).map(drop),
            Self::Plant => PlantCompiledSectionKind::from_id(kind).map(drop),
        }
    }
}

pub(super) struct RawSection {
    pub(super) kind: u16,
    pub(super) version: u32,
    pub(super) alignment: u32,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct StoredSection {
    pub(super) descriptor: RawDescriptor,
    pub(super) bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
pub(super) struct RawDescriptor {
    pub(super) kind: u16,
    pub(super) version: u32,
    pub(super) codec: ArtifactSectionCodec,
    pub(super) alignment: u32,
    pub(super) offset: u64,
    pub(super) stored_size: u64,
    pub(super) decoded_size: u64,
    pub(super) content_hash: ContentHash,
}

pub(super) struct RawIndex {
    pub(super) identity: Vec<u8>,
    pub(super) cook_key: ContentHash,
    pub(super) platform_profile: ContentHash,
    pub(super) payload_hash: ContentHash,
    pub(super) sections: Vec<RawDescriptor>,
}

pub(super) struct RawHeader {
    pub(super) identity: Vec<u8>,
    pub(super) cook_key: ContentHash,
    pub(super) platform_profile: ContentHash,
    pub(super) payload_hash: ContentHash,
    pub(super) section_count: usize,
}

/// The largest and the summed decoded section size, measured before the payloads move into the
/// writer.
pub(super) fn decoded_budget(sections: &[RawSection]) -> Result<(u64, u64)> {
    let mut largest = 0_u64;
    let mut total = 0_u64;
    for section in sections {
        let decoded = u64::try_from(section.bytes.len()).map_err(|_| Error::NumericOverflow)?;
        largest = largest.max(decoded);
        total = total.checked_add(decoded).ok_or(Error::NumericOverflow)?;
    }
    Ok((largest, total))
}

/// The limits that accept exactly the artifact just written and nothing larger.
pub(super) fn exact_decode_limits(
    bytes: &[u8],
    decoded: (u64, u64),
) -> Result<ArtifactDecodeLimits> {
    let stored_bytes = u64::try_from(bytes.len()).map_err(|_| Error::NumericOverflow)?;
    Ok(ArtifactDecodeLimits::new(
        stored_bytes,
        stored_bytes,
        decoded.0,
        decoded.1,
    ))
}

pub(super) fn write_container(
    flavor: ArtifactFlavor,
    identity: &[u8],
    cook_key: ContentHash,
    platform_profile: ContentHash,
    mut sections: Vec<RawSection>,
) -> Result<Vec<u8>> {
    let format = flavor.format();
    if identity.len() != flavor.identity_size()
        || cook_key.is_zero()
        || platform_profile.is_zero()
        || sections.is_empty()
        || sections.len() > flavor.maximum_sections()
    {
        return Err(Error::ArtifactFormat {
            format,
            field: "header".to_owned(),
        });
    }
    sections.sort_unstable_by_key(|section| section.kind);
    for section in &sections {
        flavor.validate_kind(section.kind)?;
        validate_section_header(flavor, section)?;
    }
    for pair in sections.windows(2) {
        if pair[0].kind == pair[1].kind {
            return Err(Error::ArtifactDuplicateSection {
                format,
                section: pair[0].kind,
            });
        }
    }
    let toc_size = sections
        .len()
        .checked_mul(TOC_ENTRY_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let payload_start = COMMON_HEADER_BYTES
        .checked_add(identity.len())
        .and_then(|value| value.checked_add(toc_size))
        .ok_or(Error::NumericOverflow)?;
    let mut cursor = payload_start;
    let mut stored_sections = Vec::with_capacity(sections.len());
    for section in &sections {
        cursor = align_up(cursor, section.alignment)?;
        let (codec, stored) = encode_section(format, section.kind, &section.bytes)?;
        let stored_size = u64::try_from(stored.len()).map_err(|_| Error::NumericOverflow)?;
        let decoded_size =
            u64::try_from(section.bytes.len()).map_err(|_| Error::NumericOverflow)?;
        let descriptor = RawDescriptor {
            kind: section.kind,
            version: section.version,
            codec,
            alignment: section.alignment,
            offset: u64::try_from(cursor).map_err(|_| Error::NumericOverflow)?,
            stored_size,
            decoded_size,
            content_hash: ContentHash::of(&section.bytes),
        };
        validate_descriptor(format, descriptor)?;
        let stored_length = stored.len();
        stored_sections.push(StoredSection {
            descriptor,
            bytes: stored,
        });
        cursor = cursor
            .checked_add(stored_length)
            .ok_or(Error::NumericOverflow)?;
    }
    let mut payload = BinaryWriter::with_capacity(cursor.saturating_sub(payload_start));
    for section in &stored_sections {
        let absolute = payload_start
            .checked_add(payload.len())
            .ok_or(Error::NumericOverflow)?;
        let offset =
            usize::try_from(section.descriptor.offset).map_err(|_| Error::NumericOverflow)?;
        payload.zeroes(offset.checked_sub(absolute).ok_or(Error::NumericOverflow)?)?;
        payload.bytes(&section.bytes);
    }
    let payload = payload.finish();
    let payload_hash = ContentHash::of(&payload);
    let mut writer = BinaryWriter::with_capacity(cursor);
    writer.bytes(flavor.magic());
    writer.u32(flavor.version());
    writer.bytes(&flavor.schema().bytes());
    writer.u16(u16::try_from(identity.len()).map_err(|_| Error::NumericOverflow)?);
    writer.bytes(identity);
    writer.bytes(&cook_key.bytes());
    writer.bytes(&platform_profile.bytes());
    writer.u32(u32::try_from(stored_sections.len()).map_err(|_| Error::NumericOverflow)?);
    writer.bytes(&payload_hash.bytes());
    for section in &stored_sections {
        write_descriptor(&mut writer, section.descriptor);
    }
    writer.bytes(&payload);
    Ok(writer.finish())
}

pub(super) fn read_container(
    bytes: &[u8],
    flavor: ArtifactFlavor,
    limits: ArtifactDecodeLimits,
) -> Result<RawIndex> {
    let format = flavor.format();
    let header_size = COMMON_HEADER_BYTES
        .checked_add(flavor.identity_size())
        .ok_or(Error::NumericOverflow)?;
    let header = read_container_header(
        bytes
            .get(..header_size)
            .ok_or(Error::ArtifactTruncated { format })?,
        flavor,
    )?;
    let toc_size = header
        .section_count
        .checked_mul(TOC_ENTRY_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let payload_start = header_size
        .checked_add(toc_size)
        .ok_or(Error::NumericOverflow)?;
    let sections = read_container_toc(
        bytes
            .get(header_size..payload_start)
            .ok_or(Error::ArtifactTruncated { format })?,
        flavor,
        limits,
    )?;
    validate_spans(bytes, format, payload_start, &sections)?;
    let payload = bytes
        .get(payload_start..)
        .ok_or(Error::ArtifactTruncated { format })?;
    if ContentHash::of(payload) != header.payload_hash {
        return Err(Error::ArtifactHashMismatch {
            format,
            subject: "payload".to_owned(),
        });
    }
    for descriptor in &sections {
        validate_decoded_section(
            std::io::Cursor::new(descriptor_slice(bytes, *descriptor, format)?),
            *descriptor,
            format,
        )?;
    }
    Ok(RawIndex {
        identity: header.identity,
        cook_key: header.cook_key,
        platform_profile: header.platform_profile,
        payload_hash: header.payload_hash,
        sections,
    })
}

pub(super) fn read_container_header(bytes: &[u8], flavor: ArtifactFlavor) -> Result<RawHeader> {
    let format = flavor.format();
    let mut reader = BinaryReader::new(bytes, format);
    reader.expect(flavor.magic(), "magic")?;
    let version = reader.u32()?;
    if version != flavor.version() {
        return Err(Error::FormatVersion {
            format,
            found: version,
            expected: flavor.version(),
        });
    }
    if ContentHash::new(reader.array()?) != flavor.schema() {
        return Err(Error::ArtifactSchema { format });
    }
    let identity_size = usize::from(reader.u16()?);
    if identity_size != flavor.identity_size() {
        return Err(Error::ArtifactFormat {
            format,
            field: "identitySize".to_owned(),
        });
    }
    let identity = reader.take(identity_size)?.to_vec();
    let cook_key = ContentHash::new(reader.array()?);
    let platform_profile = ContentHash::new(reader.array()?);
    if cook_key.is_zero() || platform_profile.is_zero() {
        return Err(Error::ArtifactFormat {
            format,
            field: "contentIdentity".to_owned(),
        });
    }
    let section_count = usize::try_from(reader.u32()?).map_err(|_| Error::NumericOverflow)?;
    if section_count == 0 || section_count > flavor.maximum_sections() {
        return Err(Error::ArtifactFormat {
            format,
            field: "sectionCount".to_owned(),
        });
    }
    let payload_hash = ContentHash::new(reader.array()?);
    reader.complete()?;
    Ok(RawHeader {
        identity,
        cook_key,
        platform_profile,
        payload_hash,
        section_count,
    })
}

pub(super) fn read_container_toc(
    bytes: &[u8],
    flavor: ArtifactFlavor,
    limits: ArtifactDecodeLimits,
) -> Result<Vec<RawDescriptor>> {
    let format = flavor.format();
    if !bytes.len().is_multiple_of(TOC_ENTRY_BYTES) {
        return Err(Error::ArtifactFormat {
            format,
            field: "tocSize".to_owned(),
        });
    }
    let section_count = bytes.len() / TOC_ENTRY_BYTES;
    let mut sections = Vec::with_capacity(section_count);
    let mut reader = BinaryReader::new(bytes, format);
    for _ in 0..section_count {
        let descriptor = read_descriptor(&mut reader, format)?;
        flavor.validate_kind(descriptor.kind)?;
        validate_descriptor(format, descriptor)?;
        let expected_version = flavor.section_version(descriptor.kind)?;
        if descriptor.version != expected_version {
            return Err(Error::FormatVersion {
                format,
                found: descriptor.version,
                expected: expected_version,
            });
        }
        sections.push(descriptor);
    }
    reader.complete()?;
    for pair in sections.windows(2) {
        if pair[0].kind == pair[1].kind {
            return Err(Error::ArtifactDuplicateSection {
                format,
                section: pair[0].kind,
            });
        }
        if pair[0].kind > pair[1].kind {
            return Err(Error::ArtifactFormat {
                format,
                field: "tocOrder".to_owned(),
            });
        }
    }
    validate_decode_limits(format, &sections, limits)?;
    Ok(sections)
}

fn validate_section_header(flavor: ArtifactFlavor, section: &RawSection) -> Result<()> {
    let format = flavor.format();
    let expected_version = flavor.section_version(section.kind)?;
    if section.version != expected_version {
        return Err(Error::FormatVersion {
            format,
            found: section.version,
            expected: expected_version,
        });
    }
    validate_alignment(format, section.kind, section.alignment)?;
    Ok(())
}

fn validate_descriptor(format: &'static str, descriptor: RawDescriptor) -> Result<()> {
    validate_alignment(format, descriptor.kind, descriptor.alignment)?;
    if descriptor.content_hash.is_zero()
        || descriptor.codec == ArtifactSectionCodec::Raw
            && descriptor.stored_size != descriptor.decoded_size
        || descriptor.codec == ArtifactSectionCodec::Zstd && descriptor.stored_size == 0
    {
        return Err(Error::ArtifactFormat {
            format,
            field: format!("section{}.sizeOrHash", descriptor.kind),
        });
    }
    Ok(())
}

pub(super) fn validate_section_size(
    format: &'static str,
    section: u16,
    size_kind: &'static str,
    requested: u64,
    limit: u64,
) -> Result<()> {
    if requested > limit {
        return Err(Error::ArtifactSectionLimit {
            format,
            section,
            size_kind,
            requested,
            limit,
        });
    }
    Ok(())
}

pub(super) fn validate_decode_limits(
    format: &'static str,
    sections: &[RawDescriptor],
    limits: ArtifactDecodeLimits,
) -> Result<()> {
    let mut total_stored = 0_u64;
    let mut total_decoded = 0_u64;
    for descriptor in sections {
        validate_section_size(
            format,
            descriptor.kind,
            "stored",
            descriptor.stored_size,
            limits.max_stored_section_bytes,
        )?;
        validate_section_size(
            format,
            descriptor.kind,
            "decoded",
            descriptor.decoded_size,
            limits.max_decoded_section_bytes,
        )?;
        total_stored = total_stored
            .checked_add(descriptor.stored_size)
            .ok_or(Error::NumericOverflow)?;
        total_decoded = total_decoded
            .checked_add(descriptor.decoded_size)
            .ok_or(Error::NumericOverflow)?;
    }
    if total_stored > limits.max_total_stored_bytes {
        return Err(Error::ArtifactTotalLimit {
            format,
            size_kind: "stored",
            requested: total_stored,
            limit: limits.max_total_stored_bytes,
        });
    }
    if total_decoded > limits.max_total_decoded_bytes {
        return Err(Error::ArtifactTotalLimit {
            format,
            size_kind: "decoded",
            requested: total_decoded,
            limit: limits.max_total_decoded_bytes,
        });
    }
    Ok(())
}

fn validate_alignment(format: &'static str, section: u16, alignment: u32) -> Result<()> {
    if alignment == 0 || alignment > MAX_SECTION_ALIGNMENT || !alignment.is_power_of_two() {
        return Err(Error::ArtifactMisalignedSection { format, section });
    }
    Ok(())
}

/// The streaming reader proves alignment padding is zero as it hashes it; an in-memory container
/// has the whole payload already, so it walks the gaps after the layout check.
pub(super) fn validate_spans(
    bytes: &[u8],
    format: &'static str,
    payload_start: usize,
    sections: &[RawDescriptor],
) -> Result<()> {
    let length = u64::try_from(bytes.len()).map_err(|_| Error::NumericOverflow)?;
    validate_span_layout(length, format, payload_start, sections)?;
    let mut cursor = payload_start;
    for descriptor in sections {
        let offset = usize::try_from(descriptor.offset).map_err(|_| Error::NumericOverflow)?;
        let size = usize::try_from(descriptor.stored_size).map_err(|_| Error::NumericOverflow)?;
        let gap = bytes
            .get(cursor..offset)
            .ok_or(Error::ArtifactTruncated { format })?;
        if gap.iter().any(|byte| *byte != 0) {
            return Err(Error::ArtifactFormat {
                format,
                field: "alignmentPadding".to_owned(),
            });
        }
        cursor = offset.checked_add(size).ok_or(Error::NumericOverflow)?;
    }
    Ok(())
}

pub(super) fn validate_span_layout(
    artifact_length: u64,
    format: &'static str,
    payload_start: usize,
    sections: &[RawDescriptor],
) -> Result<()> {
    let mut cursor = u64::try_from(payload_start).map_err(|_| Error::NumericOverflow)?;
    for descriptor in sections {
        if descriptor.offset < cursor {
            return Err(Error::ArtifactOverlappingSection {
                format,
                section: descriptor.kind,
            });
        }
        if descriptor.offset % u64::from(descriptor.alignment) != 0 {
            return Err(Error::ArtifactMisalignedSection {
                format,
                section: descriptor.kind,
            });
        }
        cursor = descriptor
            .offset
            .checked_add(descriptor.stored_size)
            .ok_or(Error::NumericOverflow)?;
        if cursor > artifact_length {
            return Err(Error::ArtifactTruncated { format });
        }
    }
    if cursor != artifact_length {
        return Err(Error::ArtifactFormat {
            format,
            field: "trailingBytes".to_owned(),
        });
    }
    Ok(())
}

fn decode_section<'a>(
    bytes: &'a [u8],
    descriptor: RawDescriptor,
    format: &'static str,
    limits: ArtifactDecodeLimits,
) -> Result<Cow<'a, [u8]>> {
    decode_stored_section(
        descriptor_slice(bytes, descriptor, format)?,
        descriptor,
        format,
        limits,
    )
}

pub(super) fn section_bytes<'a>(
    bytes: &'a [u8],
    descriptor: Option<RawDescriptor>,
    format: &'static str,
    limits: ArtifactDecodeLimits,
) -> Result<Option<Cow<'a, [u8]>>> {
    descriptor
        .map(|descriptor| decode_section(bytes, descriptor, format, limits))
        .transpose()
}

pub(super) fn descriptor_slice<'a>(
    bytes: &'a [u8],
    descriptor: RawDescriptor,
    format: &'static str,
) -> Result<&'a [u8]> {
    let begin = usize::try_from(descriptor.offset).map_err(|_| Error::NumericOverflow)?;
    let size = usize::try_from(descriptor.stored_size).map_err(|_| Error::NumericOverflow)?;
    let end = begin.checked_add(size).ok_or(Error::NumericOverflow)?;
    bytes
        .get(begin..end)
        .ok_or(Error::ArtifactTruncated { format })
}

fn write_descriptor(writer: &mut BinaryWriter, descriptor: RawDescriptor) {
    writer.u16(descriptor.kind);
    writer.u32(descriptor.version);
    writer.u8(descriptor.codec as u8);
    writer.u32(descriptor.alignment);
    writer.u64(descriptor.offset);
    writer.u64(descriptor.stored_size);
    writer.u64(descriptor.decoded_size);
    writer.bytes(&descriptor.content_hash.bytes());
}

fn read_descriptor(reader: &mut BinaryReader<'_>, format: &'static str) -> Result<RawDescriptor> {
    Ok(RawDescriptor {
        kind: reader.u16()?,
        version: reader.u32()?,
        codec: ArtifactSectionCodec::from_id(format, reader.u8()?)?,
        alignment: reader.u32()?,
        offset: reader.u64()?,
        stored_size: reader.u64()?,
        decoded_size: reader.u64()?,
        content_hash: ContentHash::new(reader.array()?),
    })
}

fn align_up(value: usize, alignment: u32) -> Result<usize> {
    let alignment = usize::try_from(alignment).map_err(|_| Error::NumericOverflow)?;
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(Error::NumericOverflow)
}
