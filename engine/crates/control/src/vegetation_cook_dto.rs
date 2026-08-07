//! Shared wire conversions for immutable vegetation cook artifacts and manifests.

use saffron_assets::VegetationCookStatistics;
use saffron_protocol::{
    FieldChannelDto, FieldChannelKindDto, Uuid as WireUuid, VegetationArtifactSectionCodecDto,
    VegetationBaseManifestDto, VegetationCellSectionDto, VegetationCellSectionKindDto,
    VegetationCookNodeAddressDto, VegetationCookPlatformProfileDto,
    VegetationCookRejectionTotalDto, VegetationCookStatisticsDto, VegetationCookVersionSetDto,
    VegetationCookWorkActualDto, VegetationCookWorkEstimateDto, VegetationGuid,
    VegetationManifestCellDependencyDto, VegetationManifestCellDependencyRoleDto,
    VegetationManifestCellDto, VegetationManifestCellSectionDto,
    VegetationManifestDependencyAddressDto, VegetationManifestDependencyDto,
    VegetationManifestPlantDto, VegetationManifestPointColumnDto, VegetationMapChunkKeyDto,
    VegetationMapChunkKindDto, VegetationMapTileKeyDto, VegetationPointColumnTypeDto,
    VegetationSeedNamespaceDto, VegetationSpeciesCountDto, WorldBoundsDto, WorldCellDto,
};
use saffron_spatial::{FieldChannel, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    ArtifactSectionCodec, CookDependency, CookDependencyAddress, CookNodeAddress,
    CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate, ManifestCellDependency,
    ManifestCellDependencyRole, ManifestCellSection, ManifestPointColumn, ManifestSpeciesCount,
    PointColumnType, VegetationBaseManifest, VegetationCellSectionDescriptor,
    VegetationCellSectionKind, VegetationManifestCell, VegetationManifestPlant,
    VegetationMapChunkKey, VegetationMapChunkKind, VegetationMapTileKey, VegetationSeedNamespace,
};

use crate::commands_vegetation::candidate_rejection_reason_dto;
use crate::error::Result;

pub(crate) fn world_bounds_dto(bounds: WorldBounds) -> WorldBoundsDto {
    WorldBoundsDto {
        min_ticks: bounds.min_ticks().map(|value| value.to_string()),
        max_ticks_exclusive: bounds.max_ticks_exclusive().map(|value| value.to_string()),
    }
}

pub(crate) fn world_cell_dto(cell: WorldCellKey) -> WorldCellDto {
    WorldCellDto {
        coordinates: cell.coordinates().map(|value| value.to_string()),
        level: cell.level(),
    }
}

fn cook_version_set_dto(versions: CookVersionSet) -> VegetationCookVersionSetDto {
    VegetationCookVersionSetDto {
        schema: versions.schema,
        compiler: versions.compiler,
        evaluator: versions.evaluator,
        numeric: versions.numeric,
        simulation: versions.simulation,
    }
}

fn cook_platform_profile_dto(
    platform: &CookPlatformProfile,
) -> Result<VegetationCookPlatformProfileDto> {
    let identity = platform.identity()?;
    let mut features = platform.features.clone();
    features.sort_unstable();
    features.dedup();
    Ok(VegetationCookPlatformProfileDto {
        target: platform.target.clone(),
        content_profile: platform.content_profile.clone(),
        toolchain: platform.toolchain.clone(),
        features,
        identity: identity.to_string(),
    })
}

fn cook_work_estimate_dto(estimate: CookWorkEstimate) -> VegetationCookWorkEstimateDto {
    VegetationCookWorkEstimateDto {
        work_units: estimate.work_units.to_string(),
        peak_memory_bytes: estimate.peak_memory_bytes.to_string(),
        input_bytes: estimate.input_bytes.to_string(),
        output_bytes: estimate.output_bytes.to_string(),
    }
}

fn cook_work_actual_dto(actual: CookWorkActual) -> VegetationCookWorkActualDto {
    VegetationCookWorkActualDto {
        elapsed_micros: actual.elapsed_micros.to_string(),
        peak_memory_bytes: actual.peak_memory_bytes.to_string(),
        input_bytes: actual.input_bytes.to_string(),
        output_bytes: actual.output_bytes.to_string(),
        rejection_count: actual.rejection_count.to_string(),
        cache_hit: actual.cache_hit,
    }
}

pub(crate) fn statistics_dto(statistics: &VegetationCookStatistics) -> VegetationCookStatisticsDto {
    VegetationCookStatisticsDto {
        nodes: statistics.nodes.to_string(),
        elapsed_micros: statistics.elapsed_micros.to_string(),
        peak_memory_bytes: statistics.peak_memory_bytes.to_string(),
        input_bytes: statistics.input_bytes.to_string(),
        output_bytes: statistics.output_bytes.to_string(),
        cache_hits: statistics.cache_hits.to_string(),
        cache_misses: statistics.cache_misses.to_string(),
        published_cells: statistics.published_cells.to_string(),
        rejections: statistics
            .rejections
            .iter()
            .map(|&(reason, count)| VegetationCookRejectionTotalDto {
                reason: candidate_rejection_reason_dto(reason),
                count: count.to_string(),
            })
            .collect(),
    }
}

pub(crate) fn cook_node_address_dto(address: &CookNodeAddress) -> VegetationCookNodeAddressDto {
    match address {
        CookNodeAddress::Plant { family } => VegetationCookNodeAddressDto::Plant {
            family: WireUuid(family.value()),
        },
        CookNodeAddress::GlobalStage {
            map,
            biome_instance,
            stage,
            owner,
        } => VegetationCookNodeAddressDto::GlobalStage {
            map: WireUuid(map.value()),
            biome_instance: vegetation_guid(*biome_instance),
            stage: stage.to_string(),
            owner: world_cell_dto(*owner),
        },
        CookNodeAddress::Cell { map, cell } => VegetationCookNodeAddressDto::Cell {
            map: WireUuid(map.value()),
            cell: world_cell_dto(*cell),
        },
    }
}

fn manifest_dependency_dto(dependency: &CookDependency) -> VegetationManifestDependencyDto {
    VegetationManifestDependencyDto {
        address: match &dependency.address {
            CookDependencyAddress::SourceAsset { asset } => {
                VegetationManifestDependencyAddressDto::SourceAsset {
                    asset: WireUuid(asset.value()),
                }
            }
            CookDependencyAddress::SourceFile { uri } => {
                VegetationManifestDependencyAddressDto::SourceFile { uri: uri.clone() }
            }
            CookDependencyAddress::MaterialCoverage { material } => {
                VegetationManifestDependencyAddressDto::MaterialCoverage {
                    material: WireUuid(material.value()),
                }
            }
            CookDependencyAddress::BiomeIr { map, instance } => {
                VegetationManifestDependencyAddressDto::BiomeIr {
                    map: WireUuid(map.value()),
                    instance: vegetation_guid(*instance),
                }
            }
            CookDependencyAddress::MapManifest { map } => {
                VegetationManifestDependencyAddressDto::MapManifest {
                    map: WireUuid(map.value()),
                }
            }
            CookDependencyAddress::MapObject { map, key } => {
                VegetationManifestDependencyAddressDto::MapObject {
                    map: WireUuid(map.value()),
                    key: vegetation_map_chunk_key_dto(*key),
                }
            }
            CookDependencyAddress::SurfaceProvider { provider, revision } => {
                VegetationManifestDependencyAddressDto::SurfaceProvider {
                    provider: provider.0.to_string(),
                    revision: revision.0.to_string(),
                }
            }
            CookDependencyAddress::SurfaceTile {
                provider,
                revision,
                channel,
                bounds,
            } => VegetationManifestDependencyAddressDto::SurfaceTile {
                provider: provider.0.to_string(),
                revision: revision.0.to_string(),
                channel: channel.map(field_channel_dto),
                bounds: world_bounds_dto(*bounds),
            },
            CookDependencyAddress::Contract { namespace } => {
                VegetationManifestDependencyAddressDto::Contract {
                    namespace: namespace.clone(),
                }
            }
            CookDependencyAddress::Node(node) => VegetationManifestDependencyAddressDto::Node {
                node: cook_node_address_dto(node),
            },
        },
        content_hash: dependency.content_hash.to_string(),
        bounds: dependency.bounds.map(world_bounds_dto),
        halo_bits: dependency.halo.bits(),
        ancestor_level: dependency.ancestor_level,
    }
}

fn cell_section_kind_dto(kind: VegetationCellSectionKind) -> VegetationCellSectionKindDto {
    match kind {
        VegetationCellSectionKind::MacroPoints => VegetationCellSectionKindDto::MacroPoints,
        VegetationCellSectionKind::MicroFields => VegetationCellSectionKindDto::MicroFields,
        VegetationCellSectionKind::Provenance => VegetationCellSectionKindDto::Provenance,
        VegetationCellSectionKind::RejectionDiagnostics => {
            VegetationCellSectionKindDto::RejectionDiagnostics
        }
        VegetationCellSectionKind::SurfaceAttachments => {
            VegetationCellSectionKindDto::SurfaceAttachments
        }
        VegetationCellSectionKind::SurfaceDependencies => {
            VegetationCellSectionKindDto::SurfaceDependencies
        }
        VegetationCellSectionKind::RenderReferences => {
            VegetationCellSectionKindDto::RenderReferences
        }
        VegetationCellSectionKind::RenderBounds => VegetationCellSectionKindDto::RenderBounds,
        VegetationCellSectionKind::CollisionInputs => VegetationCellSectionKindDto::CollisionInputs,
        VegetationCellSectionKind::NavigationContributions => {
            VegetationCellSectionKindDto::NavigationContributions
        }
        VegetationCellSectionKind::EcologyBoundary => VegetationCellSectionKindDto::EcologyBoundary,
        VegetationCellSectionKind::EcologyCheckpoint => {
            VegetationCellSectionKindDto::EcologyCheckpoint
        }
    }
}

fn artifact_section_codec_dto(codec: ArtifactSectionCodec) -> VegetationArtifactSectionCodecDto {
    match codec {
        ArtifactSectionCodec::Raw => VegetationArtifactSectionCodecDto::Raw,
        ArtifactSectionCodec::Zstd => VegetationArtifactSectionCodecDto::Zstd,
    }
}

pub(crate) fn cell_section_dto(
    section: &VegetationCellSectionDescriptor,
) -> VegetationCellSectionDto {
    VegetationCellSectionDto {
        kind: cell_section_kind_dto(section.kind),
        version: section.version,
        codec: artifact_section_codec_dto(section.codec),
        alignment: section.alignment,
        offset: section.offset.to_string(),
        stored_size: section.stored_size.to_string(),
        decoded_size: section.decoded_size.to_string(),
        content_hash: section.content_hash.to_string(),
    }
}

fn manifest_cell_section_dto(section: &ManifestCellSection) -> VegetationManifestCellSectionDto {
    VegetationManifestCellSectionDto {
        kind: cell_section_kind_dto(section.kind),
        version: section.version,
        codec: artifact_section_codec_dto(section.codec),
        alignment: section.alignment,
        stored_size: section.stored_size.to_string(),
        decoded_size: section.decoded_size.to_string(),
        content_hash: section.content_hash.to_string(),
    }
}

fn manifest_plant_dto(plant: &VegetationManifestPlant) -> VegetationManifestPlantDto {
    VegetationManifestPlantDto {
        family: WireUuid(plant.family.value()),
        tags: plant
            .tags
            .iter()
            .map(|tag| tag.value().to_string())
            .collect(),
        source_hash: plant.source_hash.to_string(),
        artifact_hash: plant.artifact_hash.to_string(),
        local_bounds_min_bits: plant.local_bounds_min.map(|value| value.bits()),
        local_bounds_max_bits: plant.local_bounds_max.map(|value| value.bits()),
        variation_count: plant.variation_count,
        phenotype_count: plant.phenotype_count,
    }
}

fn manifest_cell_dto(cell: &VegetationManifestCell) -> VegetationManifestCellDto {
    VegetationManifestCellDto {
        cell: world_cell_dto(cell.cell),
        bounds: world_bounds_dto(cell.bounds),
        artifact_hash: cell.artifact_hash.to_string(),
        payload_hash: cell.payload_hash.to_string(),
        dependencies: cell
            .dependencies
            .iter()
            .map(manifest_cell_dependency_dto)
            .collect(),
        species_counts: cell
            .species_counts
            .iter()
            .map(manifest_species_count_dto)
            .collect(),
        macro_count: cell.macro_count.to_string(),
        micro_count: cell.micro_count.to_string(),
        resident_memory_bytes: cell.resident_memory_bytes.to_string(),
        stored_bytes: cell.stored_bytes.to_string(),
        estimate: cook_work_estimate_dto(cell.estimate),
        actual: cook_work_actual_dto(cell.actual),
        sections: cell
            .sections
            .iter()
            .map(manifest_cell_section_dto)
            .collect(),
    }
}

fn manifest_point_column_dto(column: &ManifestPointColumn) -> VegetationManifestPointColumnDto {
    VegetationManifestPointColumnDto {
        id: column.id,
        name: column.name.clone(),
        element_type: point_column_type_dto(column.element_type),
    }
}

pub(crate) fn manifest_dto(manifest: &VegetationBaseManifest) -> Result<VegetationBaseManifestDto> {
    let identity = manifest.identity()?;
    Ok(VegetationBaseManifestDto {
        version: manifest.version,
        world: WireUuid(manifest.world.value()),
        map: WireUuid(manifest.map.value()),
        map_hash: manifest.map_hash.to_string(),
        versions: cook_version_set_dto(manifest.versions),
        platform: cook_platform_profile_dto(&manifest.platform)?,
        cook_graph_hash: manifest.cook_graph_hash.to_string(),
        dependencies: manifest
            .dependencies
            .iter()
            .map(manifest_dependency_dto)
            .collect(),
        seed_namespaces: manifest
            .seed_namespaces
            .iter()
            .map(seed_namespace_dto)
            .collect(),
        point_schema_hash: manifest.point_schema_hash.to_string(),
        point_columns: manifest
            .point_columns
            .iter()
            .map(manifest_point_column_dto)
            .collect(),
        plants: manifest.plants.iter().map(manifest_plant_dto).collect(),
        cells: manifest.cells.iter().map(manifest_cell_dto).collect(),
        identity: identity.to_string(),
    })
}

fn manifest_cell_dependency_dto(
    dependency: &ManifestCellDependency,
) -> VegetationManifestCellDependencyDto {
    VegetationManifestCellDependencyDto {
        cell: world_cell_dto(dependency.cell),
        content_hash: dependency.content_hash.to_string(),
        role: match dependency.role {
            ManifestCellDependencyRole::Neighbour => {
                VegetationManifestCellDependencyRoleDto::Neighbour
            }
            ManifestCellDependencyRole::Halo => VegetationManifestCellDependencyRoleDto::Halo,
            ManifestCellDependencyRole::Ancestor => {
                VegetationManifestCellDependencyRoleDto::Ancestor
            }
            ManifestCellDependencyRole::GlobalStage => {
                VegetationManifestCellDependencyRoleDto::GlobalStage
            }
        },
        halo_bits: dependency.halo.bits(),
    }
}

fn manifest_species_count_dto(count: &ManifestSpeciesCount) -> VegetationSpeciesCountDto {
    VegetationSpeciesCountDto {
        family: WireUuid(count.family.value()),
        macro_count: count.macro_count.to_string(),
        micro_count: count.micro_count.to_string(),
    }
}

fn seed_namespace_dto(seed: &VegetationSeedNamespace) -> VegetationSeedNamespaceDto {
    VegetationSeedNamespaceDto {
        name: seed.name.clone(),
        namespace: vegetation_guid(seed.namespace),
    }
}

fn point_column_type_dto(column: PointColumnType) -> VegetationPointColumnTypeDto {
    match column {
        PointColumnType::Id128 => VegetationPointColumnTypeDto::Id128,
        PointColumnType::WorldCell => VegetationPointColumnTypeDto::WorldCell,
        PointColumnType::Orientation => VegetationPointColumnTypeDto::Orientation,
        PointColumnType::FixedVec3 => VegetationPointColumnTypeDto::FixedVec3,
        PointColumnType::WorldBounds => VegetationPointColumnTypeDto::WorldBounds,
        PointColumnType::AssetUuid => VegetationPointColumnTypeDto::AssetUuid,
        PointColumnType::U32 => VegetationPointColumnTypeDto::U32,
        PointColumnType::U64 => VegetationPointColumnTypeDto::U64,
        PointColumnType::OptionalId128 => VegetationPointColumnTypeDto::OptionalId128,
        PointColumnType::Unit => VegetationPointColumnTypeDto::Unit,
        PointColumnType::SurfaceProjection => VegetationPointColumnTypeDto::SurfaceProjection,
        PointColumnType::OptionalSurfaceAttachment => {
            VegetationPointColumnTypeDto::OptionalSurfaceAttachment
        }
        PointColumnType::WorldPosition => VegetationPointColumnTypeDto::WorldPosition,
    }
}

fn vegetation_map_chunk_key_dto(key: VegetationMapChunkKey) -> VegetationMapChunkKeyDto {
    VegetationMapChunkKeyDto {
        layer: vegetation_guid(key.layer),
        tile: match key.tile {
            VegetationMapTileKey::Global => VegetationMapTileKeyDto::Global,
            VegetationMapTileKey::Cell(cell) => VegetationMapTileKeyDto::Cell {
                cell: world_cell_dto(cell),
            },
        },
        kind: match key.kind {
            VegetationMapChunkKind::Field => VegetationMapChunkKindDto::Field,
            VegetationMapChunkKind::AnchorOverride => VegetationMapChunkKindDto::AnchorOverride,
            VegetationMapChunkKind::GraphInstance => VegetationMapChunkKindDto::GraphInstance,
            VegetationMapChunkKind::LayerMetadata => VegetationMapChunkKindDto::LayerMetadata,
            VegetationMapChunkKind::EditorMetadata => VegetationMapChunkKindDto::EditorMetadata,
        },
    }
}

pub(crate) fn field_channel_dto(channel: FieldChannel) -> FieldChannelDto {
    let (kind, user) = match channel {
        FieldChannel::Altitude => (FieldChannelKindDto::Altitude, None),
        FieldChannel::Slope => (FieldChannelKindDto::Slope, None),
        FieldChannel::Curvature => (FieldChannelKindDto::Curvature, None),
        FieldChannel::Concavity => (FieldChannelKindDto::Concavity, None),
        FieldChannel::Drainage => (FieldChannelKindDto::Drainage, None),
        FieldChannel::Moisture => (FieldChannelKindDto::Moisture, None),
        FieldChannel::Temperature => (FieldChannelKindDto::Temperature, None),
        FieldChannel::Precipitation => (FieldChannelKindDto::Precipitation, None),
        FieldChannel::Sunlight => (FieldChannelKindDto::Sunlight, None),
        FieldChannel::Exposure => (FieldChannelKindDto::Exposure, None),
        FieldChannel::WaterDistance => (FieldChannelKindDto::WaterDistance, None),
        FieldChannel::WaterDepth => (FieldChannelKindDto::WaterDepth, None),
        FieldChannel::SignedBlocker => (FieldChannelKindDto::SignedBlocker, None),
        FieldChannel::SplineDistance => (FieldChannelKindDto::SplineDistance, None),
        FieldChannel::User(value) => (FieldChannelKindDto::User, Some(value.to_string())),
    };
    FieldChannelDto { kind, user }
}

fn vegetation_guid(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::DecisionScalar;
    use saffron_vegetation::{ContentHash, PlantTagId};

    use super::*;

    #[test]
    fn complete_manifest_conversion_preserves_canonical_identity_vocabulary() {
        let platform = CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["mesh-shaders".to_owned(), "bindless".to_owned()],
        };
        let mut manifest = VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            ContentHash::new([3; 32]),
            CookVersionSet::current(),
            platform,
            ContentHash::new([4; 32]),
        );
        manifest.plants.push(VegetationManifestPlant {
            family: Uuid(5),
            tags: vec![PlantTagId::new(7).unwrap(), PlantTagId::new(9).unwrap()],
            source_hash: ContentHash::new([6; 32]),
            artifact_hash: ContentHash::new([7; 32]),
            local_bounds_min: [DecisionScalar::from_bits(0); 3],
            local_bounds_max: [DecisionScalar::from_bits(1); 3],
            variation_count: 1,
            phenotype_count: 2,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        });

        let dto = manifest_dto(&manifest).unwrap();

        assert_eq!(dto.identity, manifest.identity().unwrap().to_string());
        assert_eq!(
            dto.platform.identity,
            manifest.platform.identity().unwrap().to_string()
        );
        assert_eq!(dto.plants[0].tags, ["7", "9"]);
        assert_eq!(dto.point_columns.len(), manifest.point_columns.len());
        assert_eq!(dto.cells.len(), manifest.cells.len());
    }
}
