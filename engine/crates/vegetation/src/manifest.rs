//! Canonical vegetation-world manifest and immutable cell directory.

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, UnitInterval, WorldBounds, WorldCellKey};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    ArtifactSectionCodec, ContentHash, CookDependency, CookPlatformProfile, CookVersionSet,
    CookWorkActual, CookWorkEstimate, Error, POINT_SCHEMA_COLUMNS, PlantTagId, PointColumnType,
    Result, VegetationCellSectionKind, point_schema_hash,
};

/// Current immutable vegetation-world base-manifest version.
pub const VEGETATION_BASE_MANIFEST_VERSION: u32 = 4;

const MANIFEST_MAGIC: &[u8; 8] = b"SVEGMAN4";

/// Stable schema identity for the complete manifest and cell-directory vocabulary.
#[must_use]
pub fn vegetation_base_manifest_schema_hash() -> ContentHash {
    ContentHash::of(
        b"saffron-anima/vegetation-base-manifest/schema/v4/world+map+versions+platform+graph+source-file+map-object-dependencies+seeds+point-columns+plant-tags+plants+cells+integrity+simulation+deterministic-work-estimates",
    )
}

/// One exact point column pinned into the manifest identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestPointColumn {
    /// Stable numeric column identity.
    pub id: u32,
    /// Canonical semantic name.
    pub name: String,
    /// Packed element shape.
    pub element_type: PointColumnType,
}

/// One stable, named seed domain used by world generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationSeedNamespace {
    /// Canonical namespace name.
    pub name: String,
    /// Stable namespace identity.
    pub namespace: u128,
}

/// One plant-family row required by cells in this generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationManifestPlant {
    /// Plant-family asset identity.
    pub family: Uuid,
    /// Canonically sorted unique family classification identities.
    pub tags: Vec<PlantTagId>,
    /// Exact canonical `.splant` source identity.
    pub source_hash: ContentHash,
    /// Exact validated `.splantc` artifact identity.
    pub artifact_hash: ContentHash,
    /// Canonical local-space minimum bounds in Q15.16 metres.
    pub local_bounds_min: [DecisionScalar; 3],
    /// Canonical local-space maximum bounds in Q15.16 metres.
    pub local_bounds_max: [DecisionScalar; 3],
    /// Compiled variation count.
    pub variation_count: u32,
    /// Compiled phenotype count.
    pub phenotype_count: u32,
    /// Species ecology rules and relations, baked by the cook so a tick never needs the asset
    /// catalog.
    pub ecology: crate::PlantEcologyDeclaration,
}

/// Spatial reason that one immutable cell reads another cell artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ManifestCellDependencyRole {
    /// Direct same-level neighbour data.
    Neighbour = 0,
    /// Finite support extending beyond the owner bounds.
    Halo = 1,
    /// Hierarchical ancestor summary.
    Ancestor = 2,
    /// Compiler-owned global-stage output.
    GlobalStage = 3,
}

impl ManifestCellDependencyRole {
    fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::Neighbour),
            1 => Ok(Self::Halo),
            2 => Ok(Self::Ancestor),
            3 => Ok(Self::GlobalStage),
            _ => Err(Error::ArtifactFormat {
                format: "vegetation base manifest",
                field: "cells.dependencies.role".to_owned(),
            }),
        }
    }
}

/// One exact inter-cell dependency in the immutable directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestCellDependency {
    /// Referenced cell key.
    pub cell: WorldCellKey,
    /// Exact referenced artifact identity.
    pub content_hash: ContentHash,
    /// Spatial ownership relation.
    pub role: ManifestCellDependencyRole,
    /// Required finite support in Q15.16 metres.
    pub halo: DecisionScalar,
}

/// Per-family population statistics for one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestSpeciesCount {
    /// Plant-family identity.
    pub family: Uuid,
    /// Accepted macro-point count.
    pub macro_count: u64,
    /// Micro-field sample count.
    pub micro_count: u64,
}

/// One independently resident `.svegcell` section recorded in the manifest directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestCellSection {
    /// Exact owned facet.
    pub kind: VegetationCellSectionKind,
    /// Section semantic version.
    pub version: u32,
    /// Storage codec.
    pub codec: ArtifactSectionCodec,
    /// Required payload alignment.
    pub alignment: u32,
    /// Stored bytes in the immutable artifact.
    pub stored_size: u64,
    /// Canonical decoded bytes.
    pub decoded_size: u64,
    /// Exact canonical section identity.
    pub content_hash: ContentHash,
}

/// One immutable `WorldCellKey → .svegcell` directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationManifestCell {
    /// Exact hierarchy cell.
    pub cell: WorldCellKey,
    /// Exact cell bounds, repeated for validated spatial queries.
    pub bounds: WorldBounds,
    /// Complete `.svegcell` byte identity.
    pub artifact_hash: ContentHash,
    /// Payload checksum covering alignment padding and stored sections.
    pub payload_hash: ContentHash,
    /// Exact neighbours, halos, ancestors, and global-stage cell inputs.
    pub dependencies: Vec<ManifestCellDependency>,
    /// Per-family macro/micro counts.
    pub species_counts: Vec<ManifestSpeciesCount>,
    /// Total accepted macro points.
    pub macro_count: u64,
    /// Total micro-field samples.
    pub micro_count: u64,
    /// Bytes needed when all cell facets are resident.
    pub resident_memory_bytes: u64,
    /// Complete stored artifact size.
    pub stored_bytes: u64,
    /// Preflight work prediction.
    pub estimate: CookWorkEstimate,
    /// Measured execution/cache statistics for the live job; excluded from canonical bytes.
    pub actual: CookWorkActual,
    /// Independently addressable facet directory.
    pub sections: Vec<ManifestCellSection>,
}

/// Complete immutable identity binding authored sources, contracts, and cooked base cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationBaseManifest {
    /// Manifest format version.
    pub version: u32,
    /// Owning world identity.
    pub world: Uuid,
    /// Vegetation-map identity.
    pub map: Uuid,
    /// Exact authored map-manifest identity.
    pub map_hash: ContentHash,
    /// Semantic versions participating in every cook key, including simulation compatibility.
    pub versions: CookVersionSet,
    /// Complete platform profile affecting derived bytes.
    pub platform: CookPlatformProfile,
    /// Exact canonical cook-graph identity.
    pub cook_graph_hash: ContentHash,
    /// Exact graph, plant, surface, map, and contract dependencies.
    pub dependencies: Vec<CookDependency>,
    /// Stable named world-seed domains.
    pub seed_namespaces: Vec<VegetationSeedNamespace>,
    /// Canonical point-column schema identity.
    pub point_schema_hash: ContentHash,
    /// Complete point-column vocabulary.
    pub point_columns: Vec<ManifestPointColumn>,
    /// Compiled plant-family table.
    pub plants: Vec<VegetationManifestPlant>,
    /// Content-addressed cell directory across hierarchy levels.
    pub cells: Vec<VegetationManifestCell>,
}

impl VegetationBaseManifest {
    /// Creates an empty manifest pinned to the current complete point vocabulary.
    #[must_use]
    pub fn current(
        world: Uuid,
        map: Uuid,
        map_hash: ContentHash,
        versions: CookVersionSet,
        platform: CookPlatformProfile,
        cook_graph_hash: ContentHash,
    ) -> Self {
        Self {
            version: VEGETATION_BASE_MANIFEST_VERSION,
            world,
            map,
            map_hash,
            versions,
            platform,
            cook_graph_hash,
            dependencies: Vec::new(),
            seed_namespaces: Vec::new(),
            point_schema_hash: ContentHash::new(point_schema_hash()),
            point_columns: current_point_columns(),
            plants: Vec::new(),
            cells: Vec::new(),
        }
    }

    /// Writes schedule-independent canonical bytes for the complete generation.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        validate_manifest(self)?;
        let mut writer = BinaryWriter::new();
        writer.bytes(MANIFEST_MAGIC);
        writer.u32(self.version);
        writer.bytes(&vegetation_base_manifest_schema_hash().bytes());
        writer.uuid(self.world);
        writer.uuid(self.map);
        writer.bytes(&self.map_hash.bytes());
        self.versions.encode(&mut writer);
        self.platform.encode(&mut writer)?;
        writer.bytes(&self.cook_graph_hash.bytes());

        let mut dependencies = self
            .dependencies
            .iter()
            .cloned()
            .map(|dependency| Ok((dependency.address.canonical_bytes()?, dependency)))
            .collect::<Result<Vec<_>>>()?;
        dependencies.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        reject_duplicate_keys(&dependencies, "dependencies.duplicateAddress")?;
        writer.length(dependencies.len())?;
        for (_, dependency) in dependencies {
            dependency.encode(&mut writer)?;
        }

        let mut seeds = self.seed_namespaces.clone();
        seeds.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.namespace.cmp(&right.namespace))
        });
        reject_duplicate_seeds(&seeds)?;
        writer.length(seeds.len())?;
        for seed in seeds {
            writer.string(&seed.name)?;
            writer.u128(seed.namespace);
        }

        writer.bytes(&self.point_schema_hash.bytes());
        writer.length(self.point_columns.len())?;
        for column in &self.point_columns {
            writer.u32(column.id);
            writer.u8(column.element_type as u8);
            writer.string(&column.name)?;
        }

        let mut plants = self.plants.clone();
        plants.sort_unstable_by_key(|plant| plant.family.value());
        reject_adjacent_by(
            &plants,
            |left, right| left.family == right.family,
            "plants.family",
        )?;
        writer.length(plants.len())?;
        for plant in plants {
            encode_plant(&mut writer, &plant)?;
        }

        let mut cells = self.cells.clone();
        cells.sort_unstable_by_key(|cell| cell.cell);
        reject_adjacent_by(&cells, |left, right| left.cell == right.cell, "cells.cell")?;
        writer.length(cells.len())?;
        for cell in cells {
            encode_cell(&mut writer, &cell)?;
        }
        Ok(writer.finish())
    }

    /// Strictly reads canonical manifest bytes and rejects reordered or malformed records.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let mut reader = BinaryReader::new(bytes, "vegetation base manifest");
        reader.expect(MANIFEST_MAGIC, "magic")?;
        let version = reader.u32()?;
        if version != VEGETATION_BASE_MANIFEST_VERSION {
            return Err(Error::FormatVersion {
                format: "vegetation base manifest",
                found: version,
                expected: VEGETATION_BASE_MANIFEST_VERSION,
            });
        }
        let schema = ContentHash::new(reader.array()?);
        if schema != vegetation_base_manifest_schema_hash() {
            return Err(Error::ArtifactSchema {
                format: "vegetation base manifest",
            });
        }
        let world = reader.uuid()?;
        let map = reader.uuid()?;
        let map_hash = ContentHash::new(reader.array()?);
        let versions = CookVersionSet::decode(&mut reader)?;
        let platform = CookPlatformProfile::decode(&mut reader)?;
        let cook_graph_hash = ContentHash::new(reader.array()?);

        let dependency_count = reader.count(44)?;
        let mut dependencies = Vec::with_capacity(dependency_count);
        for _ in 0..dependency_count {
            dependencies.push(CookDependency::decode(&mut reader)?);
        }

        let seed_count = reader.count(24)?;
        let mut seed_namespaces = Vec::with_capacity(seed_count);
        for _ in 0..seed_count {
            seed_namespaces.push(VegetationSeedNamespace {
                name: reader.string()?,
                namespace: reader.u128()?,
            });
        }

        let point_schema_hash = ContentHash::new(reader.array()?);
        let column_count = reader.count(13)?;
        let mut point_columns = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            point_columns.push(ManifestPointColumn {
                id: reader.u32()?,
                element_type: decode_point_column_type(reader.u8()?)?,
                name: reader.string()?,
            });
        }

        let plant_count = reader.count(146)?;
        let mut plants = Vec::with_capacity(plant_count);
        for _ in 0..plant_count {
            plants.push(decode_plant(&mut reader)?);
        }

        let cell_count = reader.count(212)?;
        let mut cells = Vec::with_capacity(cell_count);
        for _ in 0..cell_count {
            cells.push(decode_cell(&mut reader)?);
        }
        reader.complete()?;

        let manifest = Self {
            version,
            world,
            map,
            map_hash,
            versions,
            platform,
            cook_graph_hash,
            dependencies,
            seed_namespaces,
            point_schema_hash,
            point_columns,
            plants,
            cells,
        };
        if manifest.canonical_bytes()? != bytes {
            return Err(Error::ArtifactFormat {
                format: "vegetation base manifest",
                field: "nonCanonicalOrdering".to_owned(),
            });
        }
        Ok(manifest)
    }

    /// Exact identity used by saves and future network sessions.
    pub fn identity(&self) -> Result<ContentHash> {
        Ok(ContentHash::of(&self.canonical_bytes()?))
    }
}

fn current_point_columns() -> Vec<ManifestPointColumn> {
    POINT_SCHEMA_COLUMNS
        .iter()
        .map(|column| ManifestPointColumn {
            id: column.id.0,
            name: column.name.to_owned(),
            element_type: column.element_type,
        })
        .collect()
}

fn validate_manifest(manifest: &VegetationBaseManifest) -> Result<()> {
    if manifest.version != VEGETATION_BASE_MANIFEST_VERSION {
        return Err(Error::FormatVersion {
            format: "vegetation base manifest",
            found: manifest.version,
            expected: VEGETATION_BASE_MANIFEST_VERSION,
        });
    }
    if manifest.world.value() == 0
        || manifest.map.value() == 0
        || manifest.map_hash.is_zero()
        || manifest.cook_graph_hash.is_zero()
        || manifest.point_schema_hash != ContentHash::new(point_schema_hash())
        || manifest.point_columns != current_point_columns()
    {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "identityOrPointSchema".to_owned(),
        });
    }
    manifest.platform.identity()?;
    manifest.versions.validate()?;
    for dependency in &manifest.dependencies {
        dependency.validate()?;
    }
    if manifest
        .seed_namespaces
        .iter()
        .any(|seed| seed.name.is_empty() || seed.namespace == 0)
    {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "seedNamespaces".to_owned(),
        });
    }
    for plant in &manifest.plants {
        if plant.family.value() == 0
            || plant.tags.windows(2).any(|pair| pair[0] >= pair[1])
            || plant.source_hash.is_zero()
            || plant.artifact_hash.is_zero()
            || plant.variation_count == 0
            || plant.phenotype_count == 0
            || (0..3).any(|axis| plant.local_bounds_min[axis] >= plant.local_bounds_max[axis])
        {
            return Err(Error::ArtifactFormat {
                format: "vegetation base manifest",
                field: "plants".to_owned(),
            });
        }
    }
    for cell in &manifest.cells {
        validate_cell(cell)?;
    }
    Ok(())
}

fn validate_cell(cell: &VegetationManifestCell) -> Result<()> {
    if cell.bounds != cell.cell.bounds()
        || cell.artifact_hash.is_zero()
        || cell.payload_hash.is_zero()
        || cell.stored_bytes == 0
        || cell
            .dependencies
            .iter()
            .any(|dependency| dependency.content_hash.is_zero() || dependency.halo.bits() < 0)
    {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "cells.identityOrBounds".to_owned(),
        });
    }
    let macro_count = cell.species_counts.iter().try_fold(0_u64, |sum, species| {
        if species.family.value() == 0 {
            return Err(Error::ArtifactFormat {
                format: "vegetation base manifest",
                field: "cells.species.family".to_owned(),
            });
        }
        sum.checked_add(species.macro_count)
            .ok_or(Error::NumericOverflow)
    })?;
    let micro_count = cell.species_counts.iter().try_fold(0_u64, |sum, species| {
        sum.checked_add(species.micro_count)
            .ok_or(Error::NumericOverflow)
    })?;
    if macro_count != cell.macro_count
        || micro_count != cell.micro_count
        || !cell
            .sections
            .iter()
            .any(|section| section.kind == VegetationCellSectionKind::MacroPoints)
        || cell.sections.iter().any(|section| {
            section.version == 0
                || section.alignment == 0
                || !section.alignment.is_power_of_two()
                || section.content_hash.is_zero()
                || section.stored_size == 0
                || section.decoded_size == 0
        })
    {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "cells.countsOrSections".to_owned(),
        });
    }
    Ok(())
}

fn encode_plant(writer: &mut BinaryWriter, plant: &VegetationManifestPlant) -> Result<()> {
    writer.uuid(plant.family);
    writer.length(plant.tags.len())?;
    for tag in &plant.tags {
        writer.u64(tag.value());
    }
    writer.bytes(&plant.source_hash.bytes());
    writer.bytes(&plant.artifact_hash.bytes());
    for value in plant.local_bounds_min {
        writer.i32(value.bits());
    }
    for value in plant.local_bounds_max {
        writer.i32(value.bits());
    }
    writer.u32(plant.variation_count);
    writer.u32(plant.phenotype_count);
    for tick in plant.ecology.rules.stage_ticks {
        writer.u64(tick);
    }
    for value in [
        plant.ecology.rules.shade_tolerance,
        plant.ecology.rules.drought_tolerance,
        plant.ecology.rules.propagation_chance,
        plant.ecology.rules.regrowth_chance,
        plant.ecology.rules.deadfall_chance,
        plant.ecology.rules.root_demand,
    ] {
        writer.u16(value.bits());
    }
    writer.u32(plant.ecology.rules.spread_radius_m);
    writer.length(plant.ecology.relations.len())?;
    for relation in &plant.ecology.relations {
        writer.uuid(relation.family);
        writer.u32(relation.kind as u32);
        writer.u16(relation.strength.bits());
    }
    Ok(())
}

fn decode_plant(reader: &mut BinaryReader<'_>) -> Result<VegetationManifestPlant> {
    Ok(VegetationManifestPlant {
        family: reader.uuid()?,
        tags: {
            let count = reader.count(8)?;
            let mut tags = Vec::with_capacity(count);
            for _ in 0..count {
                tags.push(PlantTagId::new(reader.u64()?)?);
            }
            tags
        },
        source_hash: ContentHash::new(reader.array()?),
        artifact_hash: ContentHash::new(reader.array()?),
        local_bounds_min: [
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
        ],
        local_bounds_max: [
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
        ],
        variation_count: reader.u32()?,
        phenotype_count: reader.u32()?,
        ecology: crate::PlantEcologyDeclaration {
            rules: crate::EcologySpeciesRules {
                stage_ticks: [reader.u64()?, reader.u64()?, reader.u64()?, reader.u64()?],
                shade_tolerance: UnitInterval::from_bits(reader.u16()?),
                drought_tolerance: UnitInterval::from_bits(reader.u16()?),
                propagation_chance: UnitInterval::from_bits(reader.u16()?),
                regrowth_chance: UnitInterval::from_bits(reader.u16()?),
                deadfall_chance: UnitInterval::from_bits(reader.u16()?),
                root_demand: UnitInterval::from_bits(reader.u16()?),
                spread_radius_m: reader.u32()?,
            },
            relations: {
                let count = reader.count(22)?;
                let mut relations = Vec::with_capacity(count);
                for _ in 0..count {
                    relations.push(crate::PlantSpeciesRelation {
                        family: reader.uuid()?,
                        kind: crate::PlantRelationKind::try_from(reader.u32()?)?,
                        strength: UnitInterval::from_bits(reader.u16()?),
                    });
                }
                relations
            },
        },
    })
}

pub(crate) fn encode_cell(writer: &mut BinaryWriter, cell: &VegetationManifestCell) -> Result<()> {
    writer.cell(cell.cell);
    writer.bounds(cell.bounds);
    writer.bytes(&cell.artifact_hash.bytes());
    writer.bytes(&cell.payload_hash.bytes());

    let mut dependencies = cell.dependencies.clone();
    dependencies.sort_unstable_by_key(|dependency| (dependency.cell, dependency.role));
    reject_adjacent_by(
        &dependencies,
        |left, right| left.cell == right.cell && left.role == right.role,
        "cells.dependencies",
    )?;
    writer.length(dependencies.len())?;
    for dependency in dependencies {
        writer.cell(dependency.cell);
        writer.u8(dependency.role as u8);
        writer.i32(dependency.halo.bits());
        writer.bytes(&dependency.content_hash.bytes());
    }

    let mut species = cell.species_counts.clone();
    species.sort_unstable_by_key(|entry| entry.family.value());
    reject_adjacent_by(
        &species,
        |left, right| left.family == right.family,
        "cells.species",
    )?;
    writer.length(species.len())?;
    for entry in species {
        writer.uuid(entry.family);
        writer.u64(entry.macro_count);
        writer.u64(entry.micro_count);
    }

    writer.u64(cell.macro_count);
    writer.u64(cell.micro_count);
    writer.u64(cell.resident_memory_bytes);
    writer.u64(cell.stored_bytes);
    cell.estimate.encode(writer);

    let mut sections = cell.sections.clone();
    sections.sort_unstable_by_key(|section| section.kind);
    reject_adjacent_by(
        &sections,
        |left, right| left.kind == right.kind,
        "cells.sections",
    )?;
    writer.length(sections.len())?;
    for section in sections {
        writer.u16(section.kind as u16);
        writer.u32(section.version);
        writer.u8(section.codec as u8);
        writer.u32(section.alignment);
        writer.u64(section.stored_size);
        writer.u64(section.decoded_size);
        writer.bytes(&section.content_hash.bytes());
    }
    Ok(())
}

pub(crate) fn decode_cell(reader: &mut BinaryReader<'_>) -> Result<VegetationManifestCell> {
    let cell = reader.cell()?;
    let bounds = reader.bounds()?;
    let artifact_hash = ContentHash::new(reader.array()?);
    let payload_hash = ContentHash::new(reader.array()?);

    let dependency_count = reader.count(62)?;
    let mut dependencies = Vec::with_capacity(dependency_count);
    for _ in 0..dependency_count {
        dependencies.push(ManifestCellDependency {
            cell: reader.cell()?,
            role: ManifestCellDependencyRole::from_id(reader.u8()?)?,
            halo: DecisionScalar::from_bits(reader.i32()?),
            content_hash: ContentHash::new(reader.array()?),
        });
    }

    let species_count = reader.count(24)?;
    let mut species_counts = Vec::with_capacity(species_count);
    for _ in 0..species_count {
        species_counts.push(ManifestSpeciesCount {
            family: reader.uuid()?,
            macro_count: reader.u64()?,
            micro_count: reader.u64()?,
        });
    }

    let macro_count = reader.u64()?;
    let micro_count = reader.u64()?;
    let resident_memory_bytes = reader.u64()?;
    let stored_bytes = reader.u64()?;
    let estimate = CookWorkEstimate::decode(reader)?;

    let section_count = reader.count(59)?;
    let mut sections = Vec::with_capacity(section_count);
    for _ in 0..section_count {
        sections.push(ManifestCellSection {
            kind: VegetationCellSectionKind::from_id(reader.u16()?)?,
            version: reader.u32()?,
            codec: ArtifactSectionCodec::from_id("vegetation base manifest", reader.u8()?)?,
            alignment: reader.u32()?,
            stored_size: reader.u64()?,
            decoded_size: reader.u64()?,
            content_hash: ContentHash::new(reader.array()?),
        });
    }

    Ok(VegetationManifestCell {
        cell,
        bounds,
        artifact_hash,
        payload_hash,
        dependencies,
        species_counts,
        macro_count,
        micro_count,
        resident_memory_bytes,
        stored_bytes,
        estimate,
        actual: CookWorkActual::default(),
        sections,
    })
}

fn decode_point_column_type(id: u8) -> Result<PointColumnType> {
    match id {
        1 => Ok(PointColumnType::Id128),
        2 => Ok(PointColumnType::WorldCell),
        4 => Ok(PointColumnType::Orientation),
        5 => Ok(PointColumnType::FixedVec3),
        6 => Ok(PointColumnType::WorldBounds),
        7 => Ok(PointColumnType::AssetUuid),
        8 => Ok(PointColumnType::U32),
        9 => Ok(PointColumnType::U64),
        10 => Ok(PointColumnType::OptionalId128),
        11 => Ok(PointColumnType::Unit),
        12 => Ok(PointColumnType::SurfaceProjection),
        13 => Ok(PointColumnType::OptionalSurfaceAttachment),
        14 => Ok(PointColumnType::WorldPosition),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "pointColumns.type".to_owned(),
        }),
    }
}

fn reject_duplicate_keys<T>(entries: &[(Vec<u8>, T)], field: &str) -> Result<()> {
    reject_adjacent_by(entries, |left, right| left.0 == right.0, field)
}

fn reject_duplicate_seeds(seeds: &[VegetationSeedNamespace]) -> Result<()> {
    let mut names = std::collections::BTreeSet::new();
    let mut namespaces = std::collections::BTreeSet::new();
    if seeds
        .iter()
        .any(|seed| !names.insert(&seed.name) || !namespaces.insert(seed.namespace))
    {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: "seedNamespaces.duplicate".to_owned(),
        });
    }
    Ok(())
}

fn reject_adjacent_by<T>(
    entries: &[T],
    duplicate: impl Fn(&T, &T) -> bool,
    field: &str,
) -> Result<()> {
    if entries.windows(2).any(|pair| duplicate(&pair[0], &pair[1])) {
        return Err(Error::ArtifactFormat {
            format: "vegetation base manifest",
            field: field.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CookDependencyAddress, VegetationCellSectionKind};

    fn versions() -> CookVersionSet {
        CookVersionSet {
            schema: 1,
            compiler: 2,
            evaluator: 3,
            numeric: 4,
            simulation: 5,
        }
    }

    fn platform() -> CookPlatformProfile {
        CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["canonical-fixed".to_owned(), "thin-sheet".to_owned()],
        }
    }

    fn dependency(asset: u64, hash: u8) -> CookDependency {
        CookDependency {
            address: CookDependencyAddress::SourceAsset { asset: Uuid(asset) },
            content_hash: ContentHash::new([hash; 32]),
            bounds: None,
            halo: DecisionScalar::from_bits(0),
            ancestor_level: None,
        }
    }

    fn manifest() -> VegetationBaseManifest {
        let mut manifest = VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            ContentHash::new([3; 32]),
            versions(),
            platform(),
            ContentHash::new([4; 32]),
        );
        manifest.dependencies = vec![dependency(9, 5), dependency(4, 6)];
        manifest.seed_namespaces = vec![
            VegetationSeedNamespace {
                name: "understory".to_owned(),
                namespace: 12,
            },
            VegetationSeedNamespace {
                name: "canopy".to_owned(),
                namespace: 11,
            },
        ];
        manifest.plants.push(VegetationManifestPlant {
            family: Uuid(7),
            tags: vec![PlantTagId::new(17).unwrap(), PlantTagId::new(23).unwrap()],
            source_hash: ContentHash::new([7; 32]),
            artifact_hash: ContentHash::new([8; 32]),
            local_bounds_min: [
                DecisionScalar::from_bits(-10),
                DecisionScalar::from_bits(0),
                DecisionScalar::from_bits(-10),
            ],
            local_bounds_max: [
                DecisionScalar::from_bits(10),
                DecisionScalar::from_bits(100),
                DecisionScalar::from_bits(10),
            ],
            variation_count: 2,
            phenotype_count: 3,
            ecology: crate::PlantEcologyDeclaration::default(),
        });
        let cell = WorldCellKey::base(-3, 2, -1);
        manifest.cells.push(VegetationManifestCell {
            cell,
            bounds: cell.bounds(),
            artifact_hash: ContentHash::new([9; 32]),
            payload_hash: ContentHash::new([10; 32]),
            dependencies: vec![ManifestCellDependency {
                cell: WorldCellKey::base(-2, 2, -1),
                content_hash: ContentHash::new([11; 32]),
                role: ManifestCellDependencyRole::Neighbour,
                halo: DecisionScalar::from_bits(0),
            }],
            species_counts: vec![ManifestSpeciesCount {
                family: Uuid(7),
                macro_count: 4,
                micro_count: 8,
            }],
            macro_count: 4,
            micro_count: 8,
            resident_memory_bytes: 2048,
            stored_bytes: 1024,
            estimate: CookWorkEstimate {
                work_units: 10,
                peak_memory_bytes: 4096,
                input_bytes: 512,
                output_bytes: 1024,
            },
            actual: CookWorkActual {
                elapsed_micros: 20,
                peak_memory_bytes: 3072,
                input_bytes: 512,
                output_bytes: 1024,
                rejection_count: 2,
                cache_hit: false,
            },
            sections: vec![ManifestCellSection {
                kind: VegetationCellSectionKind::MacroPoints,
                version: 1,
                codec: ArtifactSectionCodec::Raw,
                alignment: 16,
                stored_size: 256,
                decoded_size: 256,
                content_hash: ContentHash::new([12; 32]),
            }],
        });
        manifest
    }

    #[test]
    fn manifest_identity_is_schedule_and_input_order_independent() {
        let a = manifest();
        let mut b = a.clone();
        b.dependencies.reverse();
        b.seed_namespaces.reverse();
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        let bytes = a.canonical_bytes().unwrap();
        let parsed = VegetationBaseManifest::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(parsed.canonical_bytes().unwrap(), bytes);
        assert_eq!(a.identity().unwrap(), ContentHash::of(&bytes));
    }

    #[test]
    fn manifest_rejects_noncanonical_family_tags() {
        let mut source = manifest();
        source.plants[0].tags.swap(0, 1);
        assert!(matches!(
            source.canonical_bytes(),
            Err(Error::ArtifactFormat { field, .. }) if field == "plants"
        ));

        source.plants[0].tags = vec![PlantTagId::new(17).unwrap(); 2];
        assert!(matches!(
            source.canonical_bytes(),
            Err(Error::ArtifactFormat { field, .. }) if field == "plants"
        ));
    }

    #[test]
    fn measured_work_does_not_change_manifest_identity() {
        let a = manifest();
        let mut b = a.clone();
        b.cells[0].actual.elapsed_micros = 999;
        b.cells[0].actual.cache_hit = true;
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        let parsed =
            VegetationBaseManifest::from_canonical_bytes(&a.canonical_bytes().unwrap()).unwrap();
        assert_eq!(parsed.cells[0].actual, CookWorkActual::default());
    }

    #[test]
    fn negative_cell_and_complete_directory_round_trip() {
        let source = manifest();
        let parsed =
            VegetationBaseManifest::from_canonical_bytes(&source.canonical_bytes().unwrap())
                .unwrap();
        assert_eq!(parsed.cells[0].cell, WorldCellKey::base(-3, 2, -1));
        assert_eq!(parsed.cells[0].sections, source.cells[0].sections);
    }

    #[test]
    fn exact_dependency_hash_changes_manifest_identity() {
        let a = manifest();
        let mut b = a.clone();
        b.dependencies[0].content_hash = ContentHash::new([99; 32]);
        assert_ne!(a.identity().unwrap(), b.identity().unwrap());
    }

    #[test]
    fn family_tags_change_manifest_identity() {
        let a = manifest();
        let mut b = a.clone();
        b.plants[0].tags.push(PlantTagId::new(29).unwrap());
        assert_ne!(a.identity().unwrap(), b.identity().unwrap());
    }

    #[test]
    fn corrupt_and_truncated_manifest_are_typed_failures() {
        let bytes = manifest().canonical_bytes().unwrap();
        assert!(matches!(
            VegetationBaseManifest::from_canonical_bytes(&bytes[..bytes.len() - 1]),
            Err(Error::ArtifactTruncated { .. })
        ));
        let mut old_version = bytes.clone();
        old_version[11] = (VEGETATION_BASE_MANIFEST_VERSION - 1) as u8;
        assert!(matches!(
            VegetationBaseManifest::from_canonical_bytes(&old_version),
            Err(Error::FormatVersion { .. })
        ));
        let mut corrupt = bytes;
        corrupt[12] ^= 1;
        assert!(matches!(
            VegetationBaseManifest::from_canonical_bytes(&corrupt),
            Err(Error::ArtifactSchema { .. })
        ));
    }
}
