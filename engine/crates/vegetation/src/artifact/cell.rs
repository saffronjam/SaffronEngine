//! The `.svegcell` container: one hierarchy cell's independently resident facets.

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};

use saffron_spatial::WorldCellKey;

use super::container::{
    ArtifactFlavor, COMMON_HEADER_BYTES, RawDescriptor, RawIndex, RawSection, TOC_ENTRY_BYTES,
    decoded_budget, exact_decode_limits, read_container, read_container_header, read_container_toc,
    section_bytes, validate_span_layout, write_container,
};
use super::section::{
    ArtifactHashingReader, decode_stored_section, read_exact_artifact, stream_region,
    validate_decoded_section,
};
use super::{ArtifactDecodeLimits, VEGETATION_CELL_SECTION_VERSION};
use crate::{ArtifactSectionCodec, ContentHash, Error, Result, VegetationContentHasher};

/// Independently addressable facet inside one `.svegcell`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum VegetationCellSectionKind {
    /// Canonical schema-hashed macro point columns.
    MacroPoints = 1,
    /// Quantized micro density and attribute tiles.
    MicroFields = 2,
    /// Compact and expanded accepted-point provenance.
    Provenance = 3,
    /// Rejected candidates and named diagnostic streams.
    RejectionDiagnostics = 4,
    /// Stable point-to-surface attachments and projection coordinates.
    SurfaceAttachments = 5,
    /// Provider/tile revisions and replay-query dependencies.
    SurfaceDependencies = 6,
    /// Plant-family/variation/phenotype representation references.
    RenderReferences = 7,
    /// Conservative static and deformation-aware render bounds.
    RenderBounds = 8,
    /// Collision broadphase and proxy derivation inputs.
    CollisionInputs = 9,
    /// Navigation obstacle and traversal-cost contributions.
    NavigationContributions = 10,
    /// Cross-cell ecology boundary summaries.
    EcologyBoundary = 11,
    /// Deterministic ecology checkpoint seed data.
    EcologyCheckpoint = 12,
}

impl VegetationCellSectionKind {
    /// Complete known vocabulary in canonical TOC order.
    pub const ALL: [Self; 12] = [
        Self::MacroPoints,
        Self::MicroFields,
        Self::Provenance,
        Self::RejectionDiagnostics,
        Self::SurfaceAttachments,
        Self::SurfaceDependencies,
        Self::RenderReferences,
        Self::RenderBounds,
        Self::CollisionInputs,
        Self::NavigationContributions,
        Self::EcologyBoundary,
        Self::EcologyCheckpoint,
    ];

    pub(crate) fn from_id(id: u16) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| *kind as u16 == id)
            .ok_or(Error::ArtifactUnknownSection {
                format: ".svegcell",
                section: id,
            })
    }
}

/// One canonical uncompressed cell-section payload supplied to the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationCellSection {
    /// Exact owned facet.
    pub kind: VegetationCellSectionKind,
    pub version: u32,
    /// Power-of-two payload alignment.
    pub alignment: u32,
    /// Canonical decoded payload bytes.
    pub bytes: Vec<u8>,
}

impl VegetationCellSection {
    /// Creates a version-one section with 16-byte payload alignment.
    #[must_use]
    pub fn new(kind: VegetationCellSectionKind, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            version: VEGETATION_CELL_SECTION_VERSION,
            alignment: 16,
            bytes,
        }
    }
}

/// Canonical header inputs for one `.svegcell`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCellArtifactHeader {
    /// Exact hierarchy cell stored by the artifact.
    pub cell: WorldCellKey,
    /// Hash of versions, platform, and every exact input dependency.
    pub cook_key: ContentHash,
    pub platform_profile: ContentHash,
}

/// Exact location and identity of one cell artifact section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCellSectionDescriptor {
    /// Exact owned facet.
    pub kind: VegetationCellSectionKind,
    pub version: u32,
    pub codec: ArtifactSectionCodec,
    /// Power-of-two payload alignment.
    pub alignment: u32,
    /// Absolute byte offset in the artifact.
    pub offset: u64,
    pub stored_size: u64,
    /// Decoded canonical byte count.
    pub decoded_size: u64,
    /// SHA-256 identity of decoded canonical bytes.
    pub content_hash: ContentHash,
}

impl From<VegetationCellSectionDescriptor> for RawDescriptor {
    fn from(section: VegetationCellSectionDescriptor) -> Self {
        Self {
            kind: section.kind as u16,
            version: section.version,
            codec: section.codec,
            alignment: section.alignment,
            offset: section.offset,
            stored_size: section.stored_size,
            decoded_size: section.decoded_size,
            content_hash: section.content_hash,
        }
    }
}

/// Validated random-access index over immutable `.svegcell` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationCellArtifactIndex {
    /// Exact hierarchy cell.
    pub cell: WorldCellKey,
    /// Complete cook-input identity.
    pub cook_key: ContentHash,
    pub platform_profile: ContentHash,
    /// Payload digest covering alignment padding and every stored section.
    pub payload_hash: ContentHash,
    /// Sections in canonical kind order.
    pub sections: Vec<VegetationCellSectionDescriptor>,
    decode_limits: ArtifactDecodeLimits,
}

/// Bounded-memory validated reader over independently resident `.svegcell` facets.
pub struct VegetationCellArtifactReader<R> {
    source: R,
    index: VegetationCellArtifactIndex,
    artifact_hash: ContentHash,
}

impl<R: Read + Seek> VegetationCellArtifactReader<R> {
    /// Streams and validates the complete container without retaining unrelated section bytes.
    pub fn open(mut source: R, limits: ArtifactDecodeLimits) -> Result<Self> {
        let format = ArtifactFlavor::Cell.format();
        let length = source
            .seek(SeekFrom::End(0))
            .map_err(|source| Error::ArtifactIo { format, source })?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|source| Error::ArtifactIo { format, source })?;
        let header_size = COMMON_HEADER_BYTES
            .checked_add(ArtifactFlavor::Cell.identity_size())
            .ok_or(Error::NumericOverflow)?;
        let mut header_bytes = vec![0_u8; header_size];
        read_exact_artifact(&mut source, &mut header_bytes, format)?;
        let header = read_container_header(&header_bytes, ArtifactFlavor::Cell)?;
        let toc_size = header
            .section_count
            .checked_mul(TOC_ENTRY_BYTES)
            .ok_or(Error::NumericOverflow)?;
        let mut toc_bytes = vec![0_u8; toc_size];
        read_exact_artifact(&mut source, &mut toc_bytes, format)?;
        let sections = read_container_toc(&toc_bytes, ArtifactFlavor::Cell, limits)?;
        let payload_start = header_size
            .checked_add(toc_size)
            .ok_or(Error::NumericOverflow)?;
        validate_span_layout(length, format, payload_start, &sections)?;

        let mut artifact_hasher = VegetationContentHasher::new();
        artifact_hasher.update(&header_bytes)?;
        artifact_hasher.update(&toc_bytes)?;
        let mut payload_hasher = VegetationContentHasher::new();
        let mut cursor = u64::try_from(payload_start).map_err(|_| Error::NumericOverflow)?;
        for descriptor in &sections {
            stream_region(
                &mut source,
                descriptor
                    .offset
                    .checked_sub(cursor)
                    .ok_or(Error::NumericOverflow)?,
                true,
                &mut payload_hasher,
                &mut artifact_hasher,
                format,
            )?;
            let hashing_source = ArtifactHashingReader {
                source: source.by_ref().take(descriptor.stored_size),
                payload_hasher: &mut payload_hasher,
                artifact_hasher: &mut artifact_hasher,
            };
            validate_decoded_section(hashing_source, *descriptor, format)?;
            cursor = descriptor
                .offset
                .checked_add(descriptor.stored_size)
                .ok_or(Error::NumericOverflow)?;
        }
        if cursor != length {
            return Err(Error::ArtifactFormat {
                format,
                field: "trailingBytes".to_owned(),
            });
        }
        if ContentHash::new(payload_hasher.finalize()?) != header.payload_hash {
            return Err(Error::ArtifactHashMismatch {
                format,
                subject: "payload".to_owned(),
            });
        }
        let artifact_hash = ContentHash::new(artifact_hasher.finalize()?);
        let index = cell_index_from_raw(
            RawIndex {
                identity: header.identity,
                cook_key: header.cook_key,
                platform_profile: header.platform_profile,
                payload_hash: header.payload_hash,
                sections,
            },
            limits,
        )?;
        Ok(Self {
            source,
            index,
            artifact_hash,
        })
    }

    /// Validated header and section directory.
    #[must_use]
    pub fn index(&self) -> &VegetationCellArtifactIndex {
        &self.index
    }

    /// SHA-256 identity of the complete streamed artifact.
    #[must_use]
    pub const fn artifact_hash(&self) -> ContentHash {
        self.artifact_hash
    }

    /// Reads and revalidates exactly one decoded facet without retaining other payloads.
    pub fn read_section(&mut self, kind: VegetationCellSectionKind) -> Result<Option<Vec<u8>>> {
        let Some(descriptor) = self
            .index
            .sections
            .iter()
            .find(|section| section.kind == kind)
            .copied()
        else {
            return Ok(None);
        };
        self.source
            .seek(SeekFrom::Start(descriptor.offset))
            .map_err(|source| Error::ArtifactIo {
                format: ".svegcell",
                source,
            })?;
        let size = usize::try_from(descriptor.stored_size).map_err(|_| Error::NumericOverflow)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|source| Error::MemoryReservation {
                resource: "vegetation cell section",
                source,
            })?;
        bytes.resize(size, 0);
        read_exact_artifact(&mut self.source, &mut bytes, ".svegcell")?;
        let decoded = decode_stored_section(
            &bytes,
            descriptor.into(),
            ".svegcell",
            self.index.decode_limits,
        )?;
        Ok(Some(match decoded {
            Cow::Borrowed(_) => bytes,
            Cow::Owned(decoded) => decoded,
        }))
    }

    /// Returns the underlying stream after validation and any facet reads.
    pub fn into_inner(self) -> R {
        self.source
    }
}

impl VegetationCellArtifactIndex {
    /// Strictly validates a complete artifact and indexes its independent sections.
    pub fn open(bytes: &[u8], limits: ArtifactDecodeLimits) -> Result<Self> {
        cell_index_from_raw(read_container(bytes, ArtifactFlavor::Cell, limits)?, limits)
    }

    /// Returns one validated decoded section without decoding unrelated facets.
    pub fn section<'a>(
        &self,
        bytes: &'a [u8],
        kind: VegetationCellSectionKind,
    ) -> Result<Option<Cow<'a, [u8]>>> {
        section_bytes(
            bytes,
            self.sections
                .iter()
                .find(|section| section.kind == kind)
                .copied()
                .map(Into::into),
            ".svegcell",
            self.decode_limits,
        )
    }
}

fn cell_index_from_raw(
    raw: RawIndex,
    decode_limits: ArtifactDecodeLimits,
) -> Result<VegetationCellArtifactIndex> {
    let cell = WorldCellKey::from_canonical_bytes(raw.identity.try_into().map_err(|_| {
        Error::ArtifactFormat {
            format: ".svegcell",
            field: "cell".to_owned(),
        }
    })?)?;
    let sections = raw
        .sections
        .into_iter()
        .map(|section| {
            Ok(VegetationCellSectionDescriptor {
                kind: VegetationCellSectionKind::from_id(section.kind)?,
                version: section.version,
                codec: section.codec,
                alignment: section.alignment,
                offset: section.offset,
                stored_size: section.stored_size,
                decoded_size: section.decoded_size,
                content_hash: section.content_hash,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if !sections
        .iter()
        .any(|section| section.kind == VegetationCellSectionKind::MacroPoints)
    {
        return Err(Error::ArtifactFormat {
            format: ".svegcell",
            field: "sections.macroPoints".to_owned(),
        });
    }
    Ok(VegetationCellArtifactIndex {
        cell,
        cook_key: raw.cook_key,
        platform_profile: raw.platform_profile,
        payload_hash: raw.payload_hash,
        sections,
        decode_limits,
    })
}

/// Writes and validates one canonical sectioned `.svegcell`.
pub fn write_vegetation_cell_artifact(
    header: VegetationCellArtifactHeader,
    sections: &[VegetationCellSection],
) -> Result<Vec<u8>> {
    let raw = sections
        .iter()
        .map(|section| RawSection {
            kind: section.kind as u16,
            version: section.version,
            alignment: section.alignment,
            bytes: section.bytes.clone(),
        })
        .collect::<Vec<_>>();
    let decoded = decoded_budget(&raw)?;
    let bytes = write_container(
        ArtifactFlavor::Cell,
        &header.cell.canonical_bytes(),
        header.cook_key,
        header.platform_profile,
        raw,
    )?;
    VegetationCellArtifactIndex::open(&bytes, exact_decode_limits(&bytes, decoded)?)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VEGETATION_ARTIFACT_DECODE_LIMITS;
    use crate::artifact::container::validate_decode_limits;

    const CELL_TOC_START: usize = COMMON_HEADER_BYTES + 25;
    const DESCRIPTOR_VERSION: usize = 2;
    const DESCRIPTOR_CODEC: usize = 6;
    const DESCRIPTOR_ALIGNMENT: usize = 7;
    const DESCRIPTOR_OFFSET: usize = 11;
    const DESCRIPTOR_DECODED_SIZE: usize = 27;
    const DESCRIPTOR_HASH: usize = 35;

    fn cell_header() -> VegetationCellArtifactHeader {
        VegetationCellArtifactHeader {
            cell: WorldCellKey::base(-7, 3, -2),
            cook_key: ContentHash::new([1; 32]),
            platform_profile: ContentHash::new([2; 32]),
        }
    }

    fn cell_sections() -> Vec<VegetationCellSection> {
        vec![
            VegetationCellSection::new(VegetationCellSectionKind::MacroPoints, vec![1, 2, 3]),
            VegetationCellSection::new(VegetationCellSectionKind::MicroFields, vec![4, 5]),
            VegetationCellSection::new(VegetationCellSectionKind::RenderBounds, vec![6; 33]),
        ]
    }

    #[test]
    fn negative_cell_and_sections_round_trip() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let index =
            VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        assert_eq!(index.cell, WorldCellKey::base(-7, 3, -2));
        let micro_fields = index
            .section(&bytes, VegetationCellSectionKind::MicroFields)
            .unwrap();
        assert_eq!(micro_fields.as_deref(), Some([4_u8, 5].as_slice()));
        assert_eq!(
            index
                .section(&bytes, VegetationCellSectionKind::Provenance)
                .unwrap(),
            None
        );
    }

    #[test]
    fn streaming_reader_validates_once_and_reads_only_the_requested_facet() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let expected_hash = ContentHash::of(&bytes);
        let expected_index =
            VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        assert_eq!(
            expected_index
                .sections
                .iter()
                .find(|section| section.kind == VegetationCellSectionKind::RenderBounds)
                .unwrap()
                .codec,
            ArtifactSectionCodec::Zstd
        );
        let mut reader = VegetationCellArtifactReader::open(
            std::io::Cursor::new(bytes),
            VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .unwrap();
        assert_eq!(reader.artifact_hash(), expected_hash);
        assert_eq!(reader.index(), &expected_index);
        assert_eq!(
            reader
                .read_section(VegetationCellSectionKind::RenderBounds)
                .unwrap(),
            Some(vec![6; 33])
        );
        assert_eq!(
            reader
                .read_section(VegetationCellSectionKind::Provenance)
                .unwrap(),
            None
        );
    }

    #[test]
    fn streaming_reader_rejects_payload_corruption_and_truncation() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(matches!(
            VegetationCellArtifactReader::open(
                std::io::Cursor::new(corrupt),
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::ArtifactCodec { .. })
        ));
        assert!(matches!(
            VegetationCellArtifactReader::open(
                std::io::Cursor::new(bytes[..bytes.len() - 1].to_vec()),
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::ArtifactTruncated { .. })
        ));
    }

    #[test]
    fn section_and_input_order_do_not_change_bytes() {
        let sections = cell_sections();
        let mut reversed = sections.clone();
        reversed.reverse();
        assert_eq!(
            write_vegetation_cell_artifact(cell_header(), &sections).unwrap(),
            write_vegetation_cell_artifact(cell_header(), &reversed).unwrap()
        );
    }

    #[test]
    fn canonical_codec_uses_zstd_only_when_it_is_smaller() {
        let sections = vec![
            VegetationCellSection::new(VegetationCellSectionKind::MacroPoints, vec![1, 2, 3]),
            VegetationCellSection::new(
                VegetationCellSectionKind::MicroFields,
                vec![0x5a; 64 * 1024],
            ),
        ];
        let first = write_vegetation_cell_artifact(cell_header(), &sections).unwrap();
        let second = write_vegetation_cell_artifact(cell_header(), &sections).unwrap();
        assert_eq!(first, second);

        let index =
            VegetationCellArtifactIndex::open(&first, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        let raw = index
            .sections
            .iter()
            .find(|section| section.kind == VegetationCellSectionKind::MacroPoints)
            .unwrap();
        assert_eq!(raw.codec, ArtifactSectionCodec::Raw);
        assert_eq!(raw.stored_size, raw.decoded_size);
        let compressed = index
            .sections
            .iter()
            .find(|section| section.kind == VegetationCellSectionKind::MicroFields)
            .unwrap();
        assert_eq!(compressed.codec, ArtifactSectionCodec::Zstd);
        assert!(compressed.stored_size < compressed.decoded_size);
        assert_eq!(compressed.content_hash, ContentHash::of(&sections[1].bytes));
        let stored_begin = usize::try_from(compressed.offset).unwrap();
        let stored_end = stored_begin + usize::try_from(compressed.stored_size).unwrap();
        let stored = &first[stored_begin..stored_end];
        assert_eq!(&stored[..4], [0x28, 0xb5, 0x2f, 0xfd]);
        assert_ne!(stored[4] & 0x04, 0);
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(stored).unwrap(),
            Some(compressed.decoded_size)
        );
        let decoded = index
            .section(&first, VegetationCellSectionKind::MicroFields)
            .unwrap()
            .unwrap();
        assert_eq!(decoded.as_ref(), sections[1].bytes);
    }

    #[test]
    fn zstd_checksum_corruption_and_truncation_are_typed_failures() {
        let sections = vec![VegetationCellSection::new(
            VegetationCellSectionKind::MacroPoints,
            vec![0xa5; 64 * 1024],
        )];
        let mut bytes = write_vegetation_cell_artifact(cell_header(), &sections).unwrap();
        let index =
            VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        let descriptor = index.sections[0];
        assert_eq!(descriptor.codec, ArtifactSectionCodec::Zstd);

        let stored_begin = usize::try_from(descriptor.offset).unwrap();
        let stored_size = usize::try_from(descriptor.stored_size).unwrap();
        let stored_end = stored_begin + stored_size;
        let mut truncated = bytes[stored_begin..stored_end].to_vec();
        truncated.pop();
        let truncated_descriptor = RawDescriptor {
            stored_size: descriptor.stored_size - 1,
            ..descriptor.into()
        };
        assert!(matches!(
            decode_stored_section(
                &truncated,
                truncated_descriptor,
                ".svegcell",
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::ArtifactCodec { .. })
        ));

        bytes[stored_end - 1] ^= 1;
        let payload_start = CELL_TOC_START + index.sections.len() * TOC_ENTRY_BYTES;
        let payload_hash = ContentHash::of(&bytes[payload_start..]);
        bytes[CELL_TOC_START - 32..CELL_TOC_START].copy_from_slice(&payload_hash.bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactCodec { .. })
        ));
    }

    #[test]
    fn decoded_size_limits_are_validated_from_the_toc() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut oversized = bytes;
        let section_limits = ArtifactDecodeLimits::new(u64::MAX, u64::MAX, 2, u64::MAX);
        oversized[CELL_TOC_START + DESCRIPTOR_DECODED_SIZE
            ..CELL_TOC_START + DESCRIPTOR_DECODED_SIZE + 8]
            .copy_from_slice(&3_u64.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&oversized, section_limits),
            Err(Error::ArtifactSectionLimit {
                size_kind: "decoded",
                ..
            })
        ));

        let descriptor = RawDescriptor {
            kind: 1,
            version: 1,
            codec: ArtifactSectionCodec::Zstd,
            alignment: 16,
            offset: 0,
            stored_size: 1,
            decoded_size: 10,
            content_hash: ContentHash::new([1; 32]),
        };
        let total_limits = ArtifactDecodeLimits::new(u64::MAX, u64::MAX, 10, 40);
        assert!(matches!(
            validate_decode_limits("test artifact", &[descriptor; 5], total_limits),
            Err(Error::ArtifactTotalLimit {
                size_kind: "decoded",
                ..
            })
        ));
    }

    #[test]
    fn one_section_change_preserves_unrelated_section_identity() {
        let first = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut changed = cell_sections();
        changed[1].bytes.push(9);
        let second = write_vegetation_cell_artifact(cell_header(), &changed).unwrap();
        let first =
            VegetationCellArtifactIndex::open(&first, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        let second =
            VegetationCellArtifactIndex::open(&second, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        let hash = |index: &VegetationCellArtifactIndex, kind| {
            index
                .sections
                .iter()
                .find(|section| section.kind == kind)
                .unwrap()
                .content_hash
        };
        assert_ne!(
            hash(&first, VegetationCellSectionKind::MicroFields),
            hash(&second, VegetationCellSectionKind::MicroFields)
        );
        assert_eq!(
            hash(&first, VegetationCellSectionKind::MacroPoints),
            hash(&second, VegetationCellSectionKind::MacroPoints)
        );
    }

    #[test]
    fn corruption_and_truncation_are_typed_failures() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut corrupt = bytes.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&corrupt, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactHashMismatch { .. })
        ));
        assert!(matches!(
            VegetationCellArtifactIndex::open(
                &bytes[..bytes.len() - 1],
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::ArtifactTruncated { .. })
        ));
    }

    #[test]
    fn header_version_and_schema_are_strictly_typed() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut version = bytes.clone();
        version[8..12].copy_from_slice(&2_u32.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&version, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::FormatVersion { found: 2, .. })
        ));
        let mut schema = bytes;
        schema[12] ^= 1;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&schema, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactSchema { .. })
        ));
    }

    #[test]
    fn toc_rejects_unknown_duplicate_and_unsupported_entries() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut unknown_section = bytes.clone();
        unknown_section[CELL_TOC_START..CELL_TOC_START + 2]
            .copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&unknown_section, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactUnknownSection { .. })
        ));

        let mut unknown_codec = bytes.clone();
        unknown_codec[CELL_TOC_START + DESCRIPTOR_CODEC] = u8::MAX;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&unknown_codec, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactUnknownCodec { .. })
        ));

        let mut unsupported_version = bytes.clone();
        unsupported_version
            [CELL_TOC_START + DESCRIPTOR_VERSION..CELL_TOC_START + DESCRIPTOR_VERSION + 4]
            .copy_from_slice(&2_u32.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(
                &unsupported_version,
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::FormatVersion { found: 2, .. })
        ));

        let mut duplicate = bytes;
        let second = CELL_TOC_START + TOC_ENTRY_BYTES;
        let first_kind = duplicate[CELL_TOC_START..CELL_TOC_START + 2].to_vec();
        duplicate[second..second + 2].copy_from_slice(&first_kind);
        assert!(matches!(
            VegetationCellArtifactIndex::open(&duplicate, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactDuplicateSection { .. })
        ));
    }

    #[test]
    fn toc_rejects_invalid_alignment_overlap_and_section_hash() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let second = CELL_TOC_START + TOC_ENTRY_BYTES;

        let mut invalid_alignment = bytes.clone();
        invalid_alignment[second + DESCRIPTOR_ALIGNMENT..second + DESCRIPTOR_ALIGNMENT + 4]
            .copy_from_slice(&3_u32.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(
                &invalid_alignment,
                VEGETATION_ARTIFACT_DECODE_LIMITS
            ),
            Err(Error::ArtifactMisalignedSection { .. })
        ));

        let mut overlapping = bytes.clone();
        let first_offset = overlapping
            [CELL_TOC_START + DESCRIPTOR_OFFSET..CELL_TOC_START + DESCRIPTOR_OFFSET + 8]
            .to_vec();
        overlapping[second + DESCRIPTOR_OFFSET..second + DESCRIPTOR_OFFSET + 8]
            .copy_from_slice(&first_offset);
        assert!(matches!(
            VegetationCellArtifactIndex::open(&overlapping, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactOverlappingSection { .. })
        ));

        let mut section_hash = bytes;
        section_hash[CELL_TOC_START + DESCRIPTOR_HASH] ^= 1;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&section_hash, VEGETATION_ARTIFACT_DECODE_LIMITS),
            Err(Error::ArtifactHashMismatch { subject, .. }) if subject == "section 1"
        ));
    }
}
