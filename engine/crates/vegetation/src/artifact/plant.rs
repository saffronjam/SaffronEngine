//! The `.splantc` container: one compiled plant family's sections, all of which must be present.

use std::borrow::Cow;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval};

use super::container::{
    ArtifactFlavor, RawDescriptor, RawSection, decoded_budget, exact_decode_limits, read_container,
    section_bytes, write_container,
};
use super::{
    ArtifactDecodeLimits, PLANT_COMPILED_SECTION_VERSION, PLANT_PART_TABLE_SECTION_VERSION,
};
use crate::binary::{BinaryReader, BinaryWriter};
use crate::{ArtifactSectionCodec, ContentHash, Error, MechanicalResponse, PlantTagId, Result};

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
    /// Material slots, the family atlas layout, coverage, and texture derivation inputs.
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
    /// Family-space signed distance fields as SDST bytes
    /// (`saffron_geometry::sdf_set_to_bytes`), one per family.
    DistanceField = 16,
    /// The family atlas's mip chain as a KTX2 container
    /// (`write_plant_texture_container`), addressed by the layout in
    /// [`Self::MaterialsCoverage`]; empty when the family packs no atlas.
    TextureContainer = 17,
}

impl PlantCompiledSectionKind {
    /// Complete known vocabulary in canonical TOC order.
    pub const ALL: [Self; 17] = [
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
        Self::DistanceField,
        Self::TextureContainer,
    ];

    pub(super) fn from_id(id: u16) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| *kind as u16 == id)
            .ok_or(Error::ArtifactUnknownSection {
                format: ".splantc",
                section: id,
            })
    }
}

pub(super) const fn plant_section_version(kind: PlantCompiledSectionKind) -> u32 {
    match kind {
        PlantCompiledSectionKind::PartTable => PLANT_PART_TABLE_SECTION_VERSION,
        _ => PLANT_COMPILED_SECTION_VERSION,
    }
}

/// One canonical uncompressed plant-section payload supplied to the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompiledSection {
    /// Exact owned facet.
    pub kind: PlantCompiledSectionKind,
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

/// Canonical header inputs for one `.splantc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantCompiledArtifactHeader {
    /// Plant-family source asset.
    pub family: Uuid,
    /// Hash of versions, platform, and every exact input dependency.
    pub cook_key: ContentHash,
    pub platform_profile: ContentHash,
}

/// Exact location and identity of one plant artifact section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantCompiledSectionDescriptor {
    /// Exact owned facet.
    pub kind: PlantCompiledSectionKind,
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

/// Validated random-access index over immutable `.splantc` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCompiledArtifactIndex {
    /// Plant-family source asset.
    pub family: Uuid,
    /// Complete cook-input identity.
    pub cook_key: ContentHash,
    pub platform_profile: ContentHash,
    /// Payload digest covering alignment padding and every stored section.
    pub payload_hash: ContentHash,
    /// Sections in canonical kind order.
    pub sections: Vec<PlantCompiledSectionDescriptor>,
    decode_limits: ArtifactDecodeLimits,
}

impl PlantCompiledArtifactIndex {
    /// Strictly validates a complete artifact and indexes its independent sections.
    pub fn open(bytes: &[u8], limits: ArtifactDecodeLimits) -> Result<Self> {
        let raw = read_container(bytes, ArtifactFlavor::Plant, limits)?;
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
            decode_limits: limits,
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
            self.decode_limits,
        )
    }

    fn part_table<'a>(&self, bytes: &'a [u8]) -> Result<Cow<'a, [u8]>> {
        self.section(bytes, PlantCompiledSectionKind::PartTable)?
            .ok_or_else(|| Error::ArtifactFormat {
                format: ".splantc",
                field: "sections.partTable".to_owned(),
            })
    }

    /// Reads the canonical sorted family-tag prefix from the semantic part table.
    pub fn family_tags(&self, bytes: &[u8]) -> Result<Vec<PlantTagId>> {
        let section = self.part_table(bytes)?;
        let mut reader = part_table_reader(section.as_ref())?;
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

    /// Reads the family's authored wind and bend response from the semantic part table.
    pub fn mechanical_response(&self, bytes: &[u8]) -> Result<MechanicalResponse> {
        let section = self.part_table(bytes)?;
        let mut reader = part_table_reader(section.as_ref())?;
        let tag_count = reader.count(8)?;
        for _ in 0..tag_count {
            reader.u64()?;
        }
        let part_count = reader.count(8)?;
        for _ in 0..part_count {
            reader.u128()?;
            if reader.u8()? != 0 {
                reader.u128()?;
            }
            reader.u8()?;
            reader.u32()?;
            let source_count = reader.count(16)?;
            for _ in 0..source_count {
                reader.u128()?;
            }
        }
        // `PlantDimensions`: height, trunk radius, crown radius (2), root radius (2),
        // and the conservative local bounds (3 + 3).
        for _ in 0..12 {
            reader.i32()?;
        }
        let response = MechanicalResponse {
            stiffness: DecisionScalar::from_bits(reader.i32()?),
            drag: DecisionScalar::from_bits(reader.i32()?),
            flutter: DecisionScalar::from_bits(reader.i32()?),
            damage_threshold: DecisionScalar::from_bits(reader.i32()?),
            break_threshold: DecisionScalar::from_bits(reader.i32()?),
            damping: UnitInterval::from_bits(reader.u16()?),
            bend_limit: UnitInterval::from_bits(reader.u16()?),
        };
        if response.stiffness.bits() < 0 || response.drag.bits() < 0 || response.flutter.bits() < 0
        {
            return Err(Error::ArtifactFormat {
                format: ".splantc part table",
                field: "mechanics".to_owned(),
            });
        }
        Ok(response)
    }
}

fn part_table_reader(section: &[u8]) -> Result<BinaryReader<'_>> {
    let mut reader = BinaryReader::new(section, ".splantc part table");
    let domain_length = reader.length()?;
    if reader.take(domain_length)? != b"saffron-anima/splantc/part-table/v2" {
        return Err(Error::ArtifactFormat {
            format: ".splantc part table",
            field: "domain".to_owned(),
        });
    }
    Ok(reader)
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
    let decoded = decoded_budget(&raw)?;
    let mut identity = BinaryWriter::new();
    identity.uuid(header.family);
    let bytes = write_container(
        ArtifactFlavor::Plant,
        &identity.finish(),
        header.cook_key,
        header.platform_profile,
        raw,
    )?;
    PlantCompiledArtifactIndex::open(&bytes, exact_decode_limits(&bytes, decoded)?)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VEGETATION_ARTIFACT_DECODE_LIMITS;

    #[test]
    fn plant_container_uses_the_same_strict_toc_contract() {
        let texels = [vec![7_u8; 16], vec![9_u8; 4]];
        let texture = crate::write_plant_texture_container(&crate::PlantTextureContainer {
            format: crate::PlantTextureFormat::Rgba8Srgb,
            width: 2,
            height: 2,
            levels: texels.iter().map(Vec::as_slice).collect(),
        })
        .unwrap();
        let sections = PlantCompiledSectionKind::ALL
            .into_iter()
            .map(|kind| {
                let bytes = match kind {
                    PlantCompiledSectionKind::Validation => vec![kind as u8; 64 * 1024],
                    PlantCompiledSectionKind::TextureContainer => texture.clone(),
                    _ => vec![kind as u8],
                };
                PlantCompiledSection::new(kind, bytes)
            })
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
        let index =
            PlantCompiledArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
        assert_eq!(index.family, Uuid(77));
        let part_table = index
            .section(&bytes, PlantCompiledSectionKind::PartTable)
            .unwrap();
        assert_eq!(
            part_table.as_deref(),
            Some([PlantCompiledSectionKind::PartTable as u8].as_slice())
        );
        let validation = index
            .sections
            .iter()
            .find(|section| section.kind == PlantCompiledSectionKind::Validation)
            .unwrap();
        assert_eq!(validation.codec, ArtifactSectionCodec::Zstd);
        let decoded = index
            .section(&bytes, PlantCompiledSectionKind::Validation)
            .unwrap()
            .unwrap();
        let expected = sections
            .iter()
            .find(|section| section.kind == PlantCompiledSectionKind::Validation)
            .unwrap();
        assert_eq!(decoded.as_ref(), expected.bytes);

        // The texture section travels through the same codec, hash, and alignment discipline as
        // every other facet, and what comes back out is the container that went in.
        let stored = index
            .section(&bytes, PlantCompiledSectionKind::TextureContainer)
            .unwrap()
            .unwrap();
        let read = crate::read_plant_texture_container(stored.as_ref()).unwrap();
        assert_eq!(read.format, crate::PlantTextureFormat::Rgba8Srgb);
        assert_eq!((read.width, read.height), (2, 2));
        assert_eq!(read.levels, [texels[0].as_slice(), texels[1].as_slice()]);
        assert!(read.is_mip_complete());
    }
}
