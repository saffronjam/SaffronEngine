//! Strict sectioned `.svegcell` and `.splantc` derived artifact containers.

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};

use saffron_core::Uuid;
use saffron_spatial::WorldCellKey;

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    ContentHash, Error, PlantTagId, Result, VegetationContentHasher, vegetation_content_hash,
};

/// Current `.svegcell` container version.
pub const VEGETATION_CELL_ARTIFACT_VERSION: u32 = 1;
/// Current `.splantc` container version.
pub const PLANT_COMPILED_ARTIFACT_VERSION: u32 = 2;
/// Current `.svegcell` section payload version.
pub const VEGETATION_CELL_SECTION_VERSION: u32 = 1;
/// Current `.splantc` section payload version.
pub const PLANT_COMPILED_SECTION_VERSION: u32 = 1;
/// Current `.splantc` semantic part-table payload version.
pub const PLANT_PART_TABLE_SECTION_VERSION: u32 = 2;

const CELL_MAGIC: &[u8; 8] = b"SVEGCEL1";
const PLANT_MAGIC: &[u8; 8] = b"SPLANTC2";
const COMMON_HEADER_BYTES: usize = 8 + 4 + 32 + 2 + 32 + 32 + 4 + 32;
const TOC_ENTRY_BYTES: usize = 2 + 4 + 1 + 4 + 8 + 8 + 8 + 32;
const MAX_SECTION_ALIGNMENT: u32 = 4096;
const MAX_SECTION_STORED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SECTION_DECODED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ARTIFACT_DECODED_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const ZSTD_COMPRESSION_LEVEL: i32 = 10;
const ZSTD_WINDOW_LOG: u32 = 27;

/// Stable schema identity of the `.svegcell` header and TOC vocabulary.
#[must_use]
pub fn vegetation_cell_artifact_schema_hash() -> ContentHash {
    ContentHash::of(b"saffron-anima/svegcell/schema/v1/strict-toc+cell+cook+platform+stored-payload-hash+raw-or-zstd-checksummed-sections+decoded-size-and-content-hash")
}

/// Stable schema identity of the `.splantc` header and TOC vocabulary.
#[must_use]
pub fn plant_compiled_artifact_schema_hash() -> ContentHash {
    ContentHash::of(b"saffron-anima/splantc/schema/v2/strict-toc+all-15-sections+family+cook+platform+stored-payload-hash+raw-or-zstd-checksummed-sections+decoded-size-and-content-hash+family-tags+portable-triangle-voxel-hierarchy+deformation+pages+ray-tracing")
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

/// Final section ownership vocabulary inside one `.splantc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum PlantCompiledSectionKind {
    /// Canonical normalized source and import transform record.
    SourceNormalization = 1,
    /// Semantic part/prototype/variation table.
    PartTable = 2,
    /// Private normalized geometry and assembly data.
    Geometry = 3,
    /// Material slots, atlases, coverage, and texture derivation inputs.
    MaterialsCoverage = 4,
    /// Structural skeleton/spines and deformation weights.
    SkeletonWeights = 5,
    /// Lifecycle/phenotype compatibility and mappings.
    Phenotypes = 6,
    /// Collision and breakage derivation data.
    Collision = 7,
    /// Navigation proxy derivation data.
    Navigation = 8,
    /// Exact source license, attribution, and dependency provenance.
    Provenance = 9,
    /// Portable triangle-cluster hierarchy.
    TriangleHierarchy = 10,
    /// Aggregate voxel hierarchy and material moments.
    VoxelHierarchy = 11,
    /// Structural deformation modes and swept bounds.
    Deformation = 12,
    /// Content page directory and guaranteed roots.
    PageDirectory = 13,
    /// Ray-tracing/opacity-micromap derivation metadata.
    RayTracing = 14,
    /// Validation statistics and normalized-source diagnostics.
    Validation = 15,
}

impl PlantCompiledSectionKind {
    /// Complete known vocabulary in canonical TOC order.
    pub const ALL: [Self; 15] = [
        Self::SourceNormalization,
        Self::PartTable,
        Self::Geometry,
        Self::MaterialsCoverage,
        Self::SkeletonWeights,
        Self::Phenotypes,
        Self::Collision,
        Self::Navigation,
        Self::Provenance,
        Self::TriangleHierarchy,
        Self::VoxelHierarchy,
        Self::Deformation,
        Self::PageDirectory,
        Self::RayTracing,
        Self::Validation,
    ];

    fn from_id(id: u16) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| *kind as u16 == id)
            .ok_or(Error::ArtifactUnknownSection {
                format: ".splantc",
                section: id,
            })
    }
}

const fn plant_section_version(kind: PlantCompiledSectionKind) -> u32 {
    match kind {
        PlantCompiledSectionKind::PartTable => PLANT_PART_TABLE_SECTION_VERSION,
        _ => PLANT_COMPILED_SECTION_VERSION,
    }
}

/// One canonical uncompressed cell-section payload supplied to the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationCellSection {
    /// Exact owned facet.
    pub kind: VegetationCellSectionKind,
    /// Section semantic version.
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

/// One canonical uncompressed plant-section payload supplied to the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompiledSection {
    /// Exact owned facet.
    pub kind: PlantCompiledSectionKind,
    /// Section semantic version.
    pub version: u32,
    /// Power-of-two payload alignment.
    pub alignment: u32,
    /// Canonical decoded payload bytes.
    pub bytes: Vec<u8>,
}

impl PlantCompiledSection {
    /// Creates a section at the exact version owned by its typed payload.
    #[must_use]
    pub fn new(kind: PlantCompiledSectionKind, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            version: plant_section_version(kind),
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
    /// Complete platform-profile identity.
    pub platform_profile: ContentHash,
}

/// Canonical header inputs for one `.splantc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantCompiledArtifactHeader {
    /// Plant-family source asset.
    pub family: Uuid,
    /// Hash of versions, platform, and every exact input dependency.
    pub cook_key: ContentHash,
    /// Complete platform-profile identity.
    pub platform_profile: ContentHash,
}

/// Exact location and identity of one cell artifact section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationCellSectionDescriptor {
    /// Exact owned facet.
    pub kind: VegetationCellSectionKind,
    /// Section semantic version.
    pub version: u32,
    /// Storage codec.
    pub codec: ArtifactSectionCodec,
    /// Power-of-two payload alignment.
    pub alignment: u32,
    /// Absolute byte offset in the artifact.
    pub offset: u64,
    /// Stored byte count.
    pub stored_size: u64,
    /// Decoded canonical byte count.
    pub decoded_size: u64,
    /// SHA-256 identity of decoded canonical bytes.
    pub content_hash: ContentHash,
}

/// Exact location and identity of one plant artifact section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantCompiledSectionDescriptor {
    /// Exact owned facet.
    pub kind: PlantCompiledSectionKind,
    /// Section semantic version.
    pub version: u32,
    /// Storage codec.
    pub codec: ArtifactSectionCodec,
    /// Power-of-two payload alignment.
    pub alignment: u32,
    /// Absolute byte offset in the artifact.
    pub offset: u64,
    /// Stored byte count.
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

impl From<PlantCompiledSectionDescriptor> for RawDescriptor {
    fn from(section: PlantCompiledSectionDescriptor) -> Self {
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
    /// Complete platform-profile identity.
    pub platform_profile: ContentHash,
    /// Payload digest covering alignment padding and every stored section.
    pub payload_hash: ContentHash,
    /// Sections in canonical kind order.
    pub sections: Vec<VegetationCellSectionDescriptor>,
}

/// Bounded-memory validated reader over independently resident `.svegcell` facets.
pub struct VegetationCellArtifactReader<R> {
    source: R,
    index: VegetationCellArtifactIndex,
    artifact_hash: ContentHash,
}

impl<R: Read + Seek> VegetationCellArtifactReader<R> {
    /// Streams and validates the complete container without retaining unrelated section bytes.
    pub fn open(mut source: R) -> Result<Self> {
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
        let sections = read_container_toc(&toc_bytes, ArtifactFlavor::Cell)?;
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
            let stored_size =
                usize::try_from(descriptor.stored_size).map_err(|_| Error::NumericOverflow)?;
            let mut stored = Vec::new();
            stored
                .try_reserve_exact(stored_size)
                .map_err(|source| Error::MemoryReservation {
                    resource: "vegetation artifact stored section",
                    source,
                })?;
            stored.resize(stored_size, 0);
            read_exact_artifact(&mut source, &mut stored, format)?;
            payload_hasher.update(&stored)?;
            artifact_hasher.update(&stored)?;
            decode_stored_section(&stored, *descriptor, format)?;
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
        let index = cell_index_from_raw(RawIndex {
            identity: header.identity,
            cook_key: header.cook_key,
            platform_profile: header.platform_profile,
            payload_hash: header.payload_hash,
            sections,
        })?;
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
        let decoded = decode_stored_section(&bytes, descriptor.into(), ".svegcell")?;
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
    pub fn open(bytes: &[u8]) -> Result<Self> {
        cell_index_from_raw(read_container(bytes, ArtifactFlavor::Cell)?)
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
        )
    }
}

fn cell_index_from_raw(raw: RawIndex) -> Result<VegetationCellArtifactIndex> {
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
    })
}

/// Validated random-access index over immutable `.splantc` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompiledArtifactIndex {
    /// Plant-family source asset.
    pub family: Uuid,
    /// Complete cook-input identity.
    pub cook_key: ContentHash,
    /// Complete platform-profile identity.
    pub platform_profile: ContentHash,
    /// Payload digest covering alignment padding and every stored section.
    pub payload_hash: ContentHash,
    /// Sections in canonical kind order.
    pub sections: Vec<PlantCompiledSectionDescriptor>,
}

impl PlantCompiledArtifactIndex {
    /// Strictly validates a complete artifact and indexes its independent sections.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        let raw = read_container(bytes, ArtifactFlavor::Plant)?;
        let mut reader = BinaryReader::new(&raw.identity, ".splantc");
        let family = reader.uuid()?;
        reader.complete()?;
        let sections = raw
            .sections
            .into_iter()
            .map(|section| {
                Ok(PlantCompiledSectionDescriptor {
                    kind: PlantCompiledSectionKind::from_id(section.kind)?,
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
        for required in PlantCompiledSectionKind::ALL {
            if !sections.iter().any(|section| section.kind == required) {
                return Err(Error::ArtifactFormat {
                    format: ".splantc",
                    field: format!("sections.{required:?}"),
                });
            }
        }
        Ok(Self {
            family,
            cook_key: raw.cook_key,
            platform_profile: raw.platform_profile,
            payload_hash: raw.payload_hash,
            sections,
        })
    }

    /// Returns one validated decoded section without decoding unrelated facets.
    pub fn section<'a>(
        &self,
        bytes: &'a [u8],
        kind: PlantCompiledSectionKind,
    ) -> Result<Option<Cow<'a, [u8]>>> {
        section_bytes(
            bytes,
            self.sections
                .iter()
                .find(|section| section.kind == kind)
                .copied()
                .map(Into::into),
            ".splantc",
        )
    }

    /// Reads the canonical sorted family-tag prefix from the semantic part table.
    pub fn family_tags(&self, bytes: &[u8]) -> Result<Vec<PlantTagId>> {
        let section = self
            .section(bytes, PlantCompiledSectionKind::PartTable)?
            .ok_or_else(|| Error::ArtifactFormat {
                format: ".splantc",
                field: "sections.partTable".to_owned(),
            })?;
        let mut reader = BinaryReader::new(section.as_ref(), ".splantc part table");
        let domain_length = reader.length()?;
        if reader.take(domain_length)? != b"saffron-anima/splantc/part-table/v2" {
            return Err(Error::ArtifactFormat {
                format: ".splantc part table",
                field: "domain".to_owned(),
            });
        }
        let count = reader.count(8)?;
        let mut tags = Vec::with_capacity(count);
        for _ in 0..count {
            tags.push(PlantTagId::new(reader.u64()?)?);
        }
        if tags.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::ArtifactFormat {
                format: ".splantc part table",
                field: "familyTags".to_owned(),
            });
        }
        Ok(tags)
    }
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
    let bytes = write_container(
        ArtifactFlavor::Cell,
        &header.cell.canonical_bytes(),
        header.cook_key,
        header.platform_profile,
        &raw,
    )?;
    VegetationCellArtifactIndex::open(&bytes)?;
    Ok(bytes)
}

/// Writes and validates one canonical sectioned `.splantc`.
pub fn write_plant_compiled_artifact(
    header: PlantCompiledArtifactHeader,
    sections: &[PlantCompiledSection],
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
    let mut identity = BinaryWriter::new();
    identity.uuid(header.family);
    let bytes = write_container(
        ArtifactFlavor::Plant,
        &identity.finish(),
        header.cook_key,
        header.platform_profile,
        &raw,
    )?;
    PlantCompiledArtifactIndex::open(&bytes)?;
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum ArtifactFlavor {
    Cell,
    Plant,
}

impl ArtifactFlavor {
    fn format(self) -> &'static str {
        match self {
            Self::Cell => ".svegcell",
            Self::Plant => ".splantc",
        }
    }

    fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::Cell => CELL_MAGIC,
            Self::Plant => PLANT_MAGIC,
        }
    }

    fn version(self) -> u32 {
        match self {
            Self::Cell => VEGETATION_CELL_ARTIFACT_VERSION,
            Self::Plant => PLANT_COMPILED_ARTIFACT_VERSION,
        }
    }

    fn schema(self) -> ContentHash {
        match self {
            Self::Cell => vegetation_cell_artifact_schema_hash(),
            Self::Plant => plant_compiled_artifact_schema_hash(),
        }
    }

    fn section_version(self, kind: u16) -> Result<u32> {
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

    fn identity_size(self) -> usize {
        match self {
            Self::Cell => 25,
            Self::Plant => 8,
        }
    }

    fn maximum_sections(self) -> usize {
        match self {
            Self::Cell => VegetationCellSectionKind::ALL.len(),
            Self::Plant => PlantCompiledSectionKind::ALL.len(),
        }
    }

    fn validate_kind(self, kind: u16) -> Result<()> {
        match self {
            Self::Cell => VegetationCellSectionKind::from_id(kind).map(drop),
            Self::Plant => PlantCompiledSectionKind::from_id(kind).map(drop),
        }
    }
}

#[derive(Clone)]
struct RawSection {
    kind: u16,
    version: u32,
    alignment: u32,
    bytes: Vec<u8>,
}

struct StoredSection {
    descriptor: RawDescriptor,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
struct RawDescriptor {
    kind: u16,
    version: u32,
    codec: ArtifactSectionCodec,
    alignment: u32,
    offset: u64,
    stored_size: u64,
    decoded_size: u64,
    content_hash: ContentHash,
}

struct RawIndex {
    identity: Vec<u8>,
    cook_key: ContentHash,
    platform_profile: ContentHash,
    payload_hash: ContentHash,
    sections: Vec<RawDescriptor>,
}

struct RawHeader {
    identity: Vec<u8>,
    cook_key: ContentHash,
    platform_profile: ContentHash,
    payload_hash: ContentHash,
    section_count: usize,
}

fn write_container(
    flavor: ArtifactFlavor,
    identity: &[u8],
    cook_key: ContentHash,
    platform_profile: ContentHash,
    sections: &[RawSection],
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
    let mut sections = sections.to_vec();
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
        let decoded_size = u64::try_from(section.bytes.len()).map_err(|_| Error::NumericOverflow)?;
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
        stored_sections.push(StoredSection {
            descriptor,
            bytes: stored,
        });
        cursor = cursor
            .checked_add(
                stored_sections
                    .last()
                    .ok_or(Error::NumericOverflow)?
                    .bytes
                    .len(),
            )
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

fn read_container(bytes: &[u8], flavor: ArtifactFlavor) -> Result<RawIndex> {
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
        decode_section(bytes, *descriptor, format)?;
    }
    Ok(RawIndex {
        identity: header.identity,
        cook_key: header.cook_key,
        platform_profile: header.platform_profile,
        payload_hash: header.payload_hash,
        sections,
    })
}

fn read_container_header(bytes: &[u8], flavor: ArtifactFlavor) -> Result<RawHeader> {
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

fn read_container_toc(bytes: &[u8], flavor: ArtifactFlavor) -> Result<Vec<RawDescriptor>> {
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
    validate_decode_limits(format, &sections)?;
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
    validate_section_size(format, section.kind, "decoded", section.bytes.len() as u64)?;
    Ok(())
}

fn validate_descriptor(format: &'static str, descriptor: RawDescriptor) -> Result<()> {
    validate_alignment(format, descriptor.kind, descriptor.alignment)?;
    if descriptor.content_hash.is_zero()
        || descriptor.codec == ArtifactSectionCodec::Raw
            && descriptor.stored_size != descriptor.decoded_size
    {
        return Err(Error::ArtifactFormat {
            format,
            field: format!("section{}.sizeOrHash", descriptor.kind),
        });
    }
    validate_section_size(
        format,
        descriptor.kind,
        "stored",
        descriptor.stored_size,
    )?;
    validate_section_size(
        format,
        descriptor.kind,
        "decoded",
        descriptor.decoded_size,
    )?;
    Ok(())
}

fn validate_section_size(
    format: &'static str,
    section: u16,
    size_kind: &'static str,
    requested: u64,
) -> Result<()> {
    let limit = match size_kind {
        "stored" => MAX_SECTION_STORED_BYTES,
        "decoded" => MAX_SECTION_DECODED_BYTES,
        _ => return Err(Error::NumericOverflow),
    };
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

fn validate_decode_limits(format: &'static str, sections: &[RawDescriptor]) -> Result<()> {
    let mut total = 0_u64;
    for descriptor in sections {
        total = total
            .checked_add(descriptor.decoded_size)
            .ok_or(Error::NumericOverflow)?;
    }
    if total > MAX_ARTIFACT_DECODED_BYTES {
        return Err(Error::ArtifactDecodedLimit {
            format,
            requested: total,
            limit: MAX_ARTIFACT_DECODED_BYTES,
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

fn validate_spans(
    bytes: &[u8],
    format: &'static str,
    payload_start: usize,
    sections: &[RawDescriptor],
) -> Result<()> {
    let mut cursor = payload_start;
    for descriptor in sections {
        let offset = usize::try_from(descriptor.offset).map_err(|_| Error::NumericOverflow)?;
        let size = usize::try_from(descriptor.stored_size).map_err(|_| Error::NumericOverflow)?;
        if offset < cursor {
            return Err(Error::ArtifactOverlappingSection {
                format,
                section: descriptor.kind,
            });
        }
        if offset % descriptor.alignment as usize != 0 {
            return Err(Error::ArtifactMisalignedSection {
                format,
                section: descriptor.kind,
            });
        }
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
        if cursor > bytes.len() {
            return Err(Error::ArtifactTruncated { format });
        }
    }
    if cursor != bytes.len() {
        return Err(Error::ArtifactFormat {
            format,
            field: "trailingBytes".to_owned(),
        });
    }
    Ok(())
}

fn validate_span_layout(
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

fn stream_region(
    source: &mut impl Read,
    mut remaining: u64,
    require_zero: bool,
    payload_hasher: &mut VegetationContentHasher,
    artifact_hasher: &mut VegetationContentHasher,
    format: &'static str,
) -> Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let length = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| Error::NumericOverflow)?;
        read_exact_artifact(source, &mut buffer[..length], format)?;
        let bytes = &buffer[..length];
        if require_zero && bytes.iter().any(|byte| *byte != 0) {
            return Err(Error::ArtifactFormat {
                format,
                field: "alignmentPadding".to_owned(),
            });
        }
        payload_hasher.update(bytes)?;
        artifact_hasher.update(bytes)?;
        remaining = remaining
            .checked_sub(u64::try_from(length).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(())
}

fn read_exact_artifact(
    source: &mut impl Read,
    bytes: &mut [u8],
    format: &'static str,
) -> Result<()> {
    source.read_exact(bytes).map_err(|source| {
        if source.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::ArtifactTruncated { format }
        } else {
            Error::ArtifactIo { format, source }
        }
    })
}

fn section_bytes<'a>(
    bytes: &'a [u8],
    descriptor: Option<RawDescriptor>,
    format: &'static str,
) -> Result<Option<&'a [u8]>> {
    descriptor
        .map(|descriptor| descriptor_slice(bytes, descriptor, format))
        .transpose()
}

fn descriptor_slice<'a>(
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

/// Returns the full artifact's final content-addressed identity.
#[must_use]
pub fn artifact_content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::new(vegetation_content_hash(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_TOC_START: usize = COMMON_HEADER_BYTES + 25;
    const DESCRIPTOR_VERSION: usize = 2;
    const DESCRIPTOR_CODEC: usize = 6;
    const DESCRIPTOR_ALIGNMENT: usize = 7;
    const DESCRIPTOR_OFFSET: usize = 11;
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
            VegetationCellSection::raw(VegetationCellSectionKind::MacroPoints, vec![1, 2, 3]),
            VegetationCellSection::raw(VegetationCellSectionKind::MicroFields, vec![4, 5]),
            VegetationCellSection::raw(VegetationCellSectionKind::RenderBounds, vec![6; 33]),
        ]
    }

    #[test]
    fn negative_cell_and_sections_round_trip() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let index = VegetationCellArtifactIndex::open(&bytes).unwrap();
        assert_eq!(index.cell, WorldCellKey::base(-7, 3, -2));
        assert_eq!(
            index
                .section(&bytes, VegetationCellSectionKind::MicroFields)
                .unwrap(),
            Some([4_u8, 5].as_slice())
        );
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
        let expected_index = VegetationCellArtifactIndex::open(&bytes).unwrap();
        let mut reader = VegetationCellArtifactReader::open(std::io::Cursor::new(bytes)).unwrap();
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
            VegetationCellArtifactReader::open(std::io::Cursor::new(corrupt)),
            Err(Error::ArtifactHashMismatch { .. })
        ));
        assert!(matches!(
            VegetationCellArtifactReader::open(std::io::Cursor::new(
                bytes[..bytes.len() - 1].to_vec()
            )),
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
    fn one_section_change_preserves_unrelated_section_identity() {
        let first = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut changed = cell_sections();
        changed[1].bytes.push(9);
        let second = write_vegetation_cell_artifact(cell_header(), &changed).unwrap();
        let first = VegetationCellArtifactIndex::open(&first).unwrap();
        let second = VegetationCellArtifactIndex::open(&second).unwrap();
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
            VegetationCellArtifactIndex::open(&corrupt),
            Err(Error::ArtifactHashMismatch { .. })
        ));
        assert!(matches!(
            VegetationCellArtifactIndex::open(&bytes[..bytes.len() - 1]),
            Err(Error::ArtifactTruncated { .. })
        ));
    }

    #[test]
    fn header_version_and_schema_are_strictly_typed() {
        let bytes = write_vegetation_cell_artifact(cell_header(), &cell_sections()).unwrap();
        let mut version = bytes.clone();
        version[8..12].copy_from_slice(&2_u32.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&version),
            Err(Error::FormatVersion { found: 2, .. })
        ));
        let mut schema = bytes;
        schema[12] ^= 1;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&schema),
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
            VegetationCellArtifactIndex::open(&unknown_section),
            Err(Error::ArtifactUnknownSection { .. })
        ));

        let mut unknown_codec = bytes.clone();
        unknown_codec[CELL_TOC_START + DESCRIPTOR_CODEC] = u8::MAX;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&unknown_codec),
            Err(Error::ArtifactUnknownCodec { .. })
        ));

        let mut unsupported_version = bytes.clone();
        unsupported_version
            [CELL_TOC_START + DESCRIPTOR_VERSION..CELL_TOC_START + DESCRIPTOR_VERSION + 4]
            .copy_from_slice(&2_u32.to_be_bytes());
        assert!(matches!(
            VegetationCellArtifactIndex::open(&unsupported_version),
            Err(Error::FormatVersion { found: 2, .. })
        ));

        let mut duplicate = bytes;
        let second = CELL_TOC_START + TOC_ENTRY_BYTES;
        let first_kind = duplicate[CELL_TOC_START..CELL_TOC_START + 2].to_vec();
        duplicate[second..second + 2].copy_from_slice(&first_kind);
        assert!(matches!(
            VegetationCellArtifactIndex::open(&duplicate),
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
            VegetationCellArtifactIndex::open(&invalid_alignment),
            Err(Error::ArtifactMisalignedSection { .. })
        ));

        let mut overlapping = bytes.clone();
        let first_offset = overlapping
            [CELL_TOC_START + DESCRIPTOR_OFFSET..CELL_TOC_START + DESCRIPTOR_OFFSET + 8]
            .to_vec();
        overlapping[second + DESCRIPTOR_OFFSET..second + DESCRIPTOR_OFFSET + 8]
            .copy_from_slice(&first_offset);
        assert!(matches!(
            VegetationCellArtifactIndex::open(&overlapping),
            Err(Error::ArtifactOverlappingSection { .. })
        ));

        let mut section_hash = bytes;
        section_hash[CELL_TOC_START + DESCRIPTOR_HASH] ^= 1;
        assert!(matches!(
            VegetationCellArtifactIndex::open(&section_hash),
            Err(Error::ArtifactHashMismatch { subject, .. }) if subject == "section 1"
        ));
    }

    #[test]
    fn plant_container_uses_the_same_strict_toc_contract() {
        let sections = PlantCompiledSectionKind::ALL
            .into_iter()
            .map(|kind| PlantCompiledSection::raw(kind, vec![kind as u8]))
            .collect::<Vec<_>>();
        let bytes = write_plant_compiled_artifact(
            PlantCompiledArtifactHeader {
                family: Uuid(77),
                cook_key: ContentHash::new([3; 32]),
                platform_profile: ContentHash::new([4; 32]),
            },
            &sections,
        )
        .unwrap();
        let index = PlantCompiledArtifactIndex::open(&bytes).unwrap();
        assert_eq!(index.family, Uuid(77));
        assert_eq!(
            index
                .section(&bytes, PlantCompiledSectionKind::PartTable)
                .unwrap(),
            Some([PlantCompiledSectionKind::PartTable as u8].as_slice())
        );
    }
}
