use std::collections::BTreeMap;

use saffron_assets::PlantValidationOutcome;
use saffron_protocol::{
    AssetTypeDto, FieldChannelDto, FieldChannelKindDto, PlantCompileDiagnosticCodeDto,
    PlantCompileDiagnosticDto, PlantCompileStatisticsDto, PlantId, PlantReimportConflictReasonDto,
    PlantSemanticDestinationDto, PlantSourceHashUpdateDto, PlantSourceLocatorDto,
    PlantSourceReferenceDto, PlantSourceRoleDto, PlantSourceSelectorDto, ReimportConflictEntryDto,
    Uuid as WireUuid, VegetationGuid, VegetationMapChunkKeyDto, VegetationMapChunkKindDto,
    VegetationMapTileKeyDto, VegetationSourceProvenanceDto, VegetationValidationIssueDto,
    VegetationValidationSeverityDto, VegetationValidationSummaryDto, WorldBoundsDto, WorldCellDto,
};
use saffron_scene::AssetType;
use saffron_vegetation::{
    PlantCompileDiagnostic, PlantCompileDiagnosticCode, PlantCompileDiagnosticSeverity,
    PlantFamilyAsset, PlantFamilySource, PlantReimportConflict, PlantReimportConflictReason,
    PlantSemanticDestination, PlantSourceLocator, PlantSourceReference, PlantSourceRole,
    PlantSourceSelector, SourceProvenance, VegetationMapChunkKind, VegetationMapTileKey,
};

use super::*;
use crate::error::{Error, Result};

/// The `base64` standard encoder, used by `preview-render` and the thumbnail commands to
/// ship PNG bytes inside a JSON string.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// The wire `AssetTypeDto` for a catalog kind.
pub(crate) fn asset_type_dto(asset_type: AssetType) -> AssetTypeDto {
    match asset_type {
        AssetType::Texture => AssetTypeDto::Texture,
        AssetType::Other => AssetTypeDto::Other,
        AssetType::Animation => AssetTypeDto::Animation,
        AssetType::Material => AssetTypeDto::Material,
        AssetType::Model => AssetTypeDto::Model,
        AssetType::Mesh => AssetTypeDto::Mesh,
        AssetType::Lut => AssetTypeDto::Lut,
        AssetType::Environment => AssetTypeDto::Environment,
        AssetType::Plant => AssetTypeDto::Plant,
        AssetType::Biome => AssetTypeDto::Biome,
        AssetType::VegetationMap => AssetTypeDto::VegetationMap,
    }
}

pub(crate) fn vegetation_guid(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

pub(crate) fn plant_id(value: saffron_vegetation::PlantId) -> PlantId {
    PlantId(value.to_string())
}

pub(crate) fn world_bounds_dto(bounds: saffron_spatial::WorldBounds) -> WorldBoundsDto {
    WorldBoundsDto {
        min_ticks: bounds.min_ticks().map(|value| value.to_string()),
        max_ticks_exclusive: bounds.max_ticks_exclusive().map(|value| value.to_string()),
    }
}

pub(crate) fn world_cell_dto(cell: saffron_spatial::WorldCellKey) -> WorldCellDto {
    WorldCellDto {
        coordinates: cell.coordinates().map(|value| value.to_string()),
        level: cell.level(),
    }
}

pub(crate) fn field_channel_dto(channel: saffron_spatial::FieldChannel) -> FieldChannelDto {
    use saffron_spatial::FieldChannel;
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

pub(crate) fn source_provenance_from_dto(
    provenance: &VegetationSourceProvenanceDto,
) -> SourceProvenance {
    SourceProvenance {
        source: provenance.source.clone(),
        source_uri: provenance.source_uri.clone(),
        license_id: provenance.license_id.clone(),
        license_uri: provenance.license_uri.clone(),
        author: provenance.author.clone(),
        attribution: provenance.attribution.clone(),
        requires_attribution: provenance.requires_attribution,
    }
}

pub(crate) fn source_provenance_dto(
    provenance: &SourceProvenance,
) -> VegetationSourceProvenanceDto {
    VegetationSourceProvenanceDto {
        source: provenance.source.clone(),
        source_uri: provenance.source_uri.clone(),
        license_id: provenance.license_id.clone(),
        license_uri: provenance.license_uri.clone(),
        author: provenance.author.clone(),
        attribution: provenance.attribution.clone(),
        requires_attribution: provenance.requires_attribution,
    }
}

pub(crate) fn plant_source_selector_from_dto(
    selector: &PlantSourceSelectorDto,
) -> Result<PlantSourceSelector> {
    let guid = |value: &saffron_protocol::VegetationGuid| -> Result<u128> {
        u128::from_str_radix(&value.0, 16)
            .map_err(|_| Error::command("selector identity must be 32 hex digits"))
    };
    Ok(match selector {
        PlantSourceSelectorDto::Whole => PlantSourceSelector::Whole,
        PlantSourceSelectorDto::Element { id, path } => PlantSourceSelector::Element {
            id: guid(id)?,
            path: path.clone(),
        },
        PlantSourceSelectorDto::Submesh { element, index } => PlantSourceSelector::Submesh {
            element: guid(element)?,
            index: *index,
        },
    })
}

pub(crate) fn plant_source_selector_dto(selector: &PlantSourceSelector) -> PlantSourceSelectorDto {
    match selector {
        PlantSourceSelector::Whole => PlantSourceSelectorDto::Whole,
        PlantSourceSelector::Element { id, path } => PlantSourceSelectorDto::Element {
            id: vegetation_guid(*id),
            path: path.clone(),
        },
        PlantSourceSelector::Submesh { element, index } => PlantSourceSelectorDto::Submesh {
            element: vegetation_guid(*element),
            index: *index,
        },
    }
}

pub(crate) fn plant_source_selector_text(selector: &PlantSourceSelector) -> String {
    match selector {
        PlantSourceSelector::Whole => "whole".to_owned(),
        PlantSourceSelector::Element { id, path } => format!("element:{id:032x}:{path}"),
        PlantSourceSelector::Submesh { element, index } => {
            format!("submesh:{element:032x}:{index}")
        }
    }
}

pub(crate) fn plant_semantic_destination_dto(
    destination: PlantSemanticDestination,
) -> PlantSemanticDestinationDto {
    match destination {
        PlantSemanticDestination::Part(id) => PlantSemanticDestinationDto::Part {
            id: vegetation_guid(id),
        },
        PlantSemanticDestination::Spine(id) => PlantSemanticDestinationDto::Spine {
            id: vegetation_guid(id),
        },
        PlantSemanticDestination::MaterialSlot(slot) => {
            PlantSemanticDestinationDto::MaterialSlot { slot }
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            PlantSemanticDestinationDto::CollisionProxy {
                id: vegetation_guid(id),
            }
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            PlantSemanticDestinationDto::NavigationProxy {
                id: vegetation_guid(id),
            }
        }
        PlantSemanticDestination::Phenotype(id) => PlantSemanticDestinationDto::Phenotype { id },
    }
}

pub(crate) fn plant_reimport_conflict_dto(
    conflict: &PlantReimportConflict,
) -> ReimportConflictEntryDto {
    ReimportConflictEntryDto {
        target: vegetation_guid(conflict.target),
        source: vegetation_guid(conflict.source),
        selector: plant_source_selector_dto(&conflict.selector),
        destination: plant_semantic_destination_dto(conflict.destination),
        reason: match conflict.reason {
            PlantReimportConflictReason::MissingSource => {
                PlantReimportConflictReasonDto::MissingSource
            }
            PlantReimportConflictReason::MissingElement => {
                PlantReimportConflictReasonDto::MissingElement
            }
        },
    }
}

pub(crate) fn plant_diagnostic_code_dto(
    code: PlantCompileDiagnosticCode,
) -> PlantCompileDiagnosticCodeDto {
    match code {
        PlantCompileDiagnosticCode::MissingSource => PlantCompileDiagnosticCodeDto::MissingSource,
        PlantCompileDiagnosticCode::DuplicateSource => {
            PlantCompileDiagnosticCodeDto::DuplicateSource
        }
        PlantCompileDiagnosticCode::EmptySelection => PlantCompileDiagnosticCodeDto::EmptySelection,
        PlantCompileDiagnosticCode::InvalidGeometry => {
            PlantCompileDiagnosticCodeDto::InvalidGeometry
        }
        PlantCompileDiagnosticCode::MissingMaterial => {
            PlantCompileDiagnosticCodeDto::MissingMaterial
        }
        PlantCompileDiagnosticCode::InvalidMaterial => {
            PlantCompileDiagnosticCodeDto::InvalidMaterial
        }
        PlantCompileDiagnosticCode::InvalidSkeleton => {
            PlantCompileDiagnosticCodeDto::InvalidSkeleton
        }
        PlantCompileDiagnosticCode::MissingCoverageUv => {
            PlantCompileDiagnosticCodeDto::MissingCoverageUv
        }
        PlantCompileDiagnosticCode::InvalidLeafOrientation => {
            PlantCompileDiagnosticCodeDto::InvalidLeafOrientation
        }
        PlantCompileDiagnosticCode::BoundsMismatch => PlantCompileDiagnosticCodeDto::BoundsMismatch,
        PlantCompileDiagnosticCode::LimitExceeded => PlantCompileDiagnosticCodeDto::LimitExceeded,
        PlantCompileDiagnosticCode::SourceChanged => PlantCompileDiagnosticCodeDto::SourceChanged,
        PlantCompileDiagnosticCode::OrphanedEdit => PlantCompileDiagnosticCodeDto::OrphanedEdit,
    }
}

pub(crate) fn plant_diagnostic_code_text(code: PlantCompileDiagnosticCode) -> &'static str {
    match code {
        PlantCompileDiagnosticCode::MissingSource => "missing-source",
        PlantCompileDiagnosticCode::DuplicateSource => "duplicate-source",
        PlantCompileDiagnosticCode::EmptySelection => "empty-selection",
        PlantCompileDiagnosticCode::InvalidGeometry => "invalid-geometry",
        PlantCompileDiagnosticCode::MissingMaterial => "missing-material",
        PlantCompileDiagnosticCode::InvalidMaterial => "invalid-material",
        PlantCompileDiagnosticCode::InvalidSkeleton => "invalid-skeleton",
        PlantCompileDiagnosticCode::MissingCoverageUv => "missing-coverage-uv",
        PlantCompileDiagnosticCode::InvalidLeafOrientation => "invalid-leaf-orientation",
        PlantCompileDiagnosticCode::BoundsMismatch => "bounds-mismatch",
        PlantCompileDiagnosticCode::LimitExceeded => "limit-exceeded",
        PlantCompileDiagnosticCode::SourceChanged => "source-changed",
        PlantCompileDiagnosticCode::OrphanedEdit => "orphaned-edit",
    }
}

pub(crate) fn validation_severity_dto(
    severity: PlantCompileDiagnosticSeverity,
) -> VegetationValidationSeverityDto {
    match severity {
        PlantCompileDiagnosticSeverity::Info => VegetationValidationSeverityDto::Info,
        PlantCompileDiagnosticSeverity::Warning => VegetationValidationSeverityDto::Warning,
        PlantCompileDiagnosticSeverity::Error => VegetationValidationSeverityDto::Error,
    }
}

pub(crate) fn plant_compile_diagnostic_dto(
    diagnostic: &PlantCompileDiagnostic,
) -> PlantCompileDiagnosticDto {
    PlantCompileDiagnosticDto {
        severity: validation_severity_dto(diagnostic.severity),
        code: plant_diagnostic_code_dto(diagnostic.code),
        source: diagnostic.source.map(vegetation_guid),
        source_selector: diagnostic.selector.as_ref().map(plant_source_selector_dto),
        path: diagnostic.path.clone(),
        message: diagnostic.message.clone(),
    }
}

pub(crate) fn plant_validation_summary(
    outcome: &PlantValidationOutcome,
) -> VegetationValidationSummaryDto {
    let mut issues = outcome
        .compile
        .diagnostics
        .iter()
        .map(|diagnostic| VegetationValidationIssueDto {
            severity: validation_severity_dto(diagnostic.severity),
            code: plant_diagnostic_code_text(diagnostic.code).to_owned(),
            path: diagnostic.path.clone(),
            message: diagnostic.message.clone(),
            source_selector: diagnostic.selector.as_ref().map(plant_source_selector_text),
        })
        .collect::<Vec<_>>();
    issues.extend(outcome.compile.conflicts.conflicts.iter().map(|conflict| {
        VegetationValidationIssueDto {
            severity: VegetationValidationSeverityDto::Error,
            code: match conflict.reason {
                PlantReimportConflictReason::MissingSource => "reimport-missing-source",
                PlantReimportConflictReason::MissingElement => "reimport-missing-element",
            }
            .to_owned(),
            path: format!("source.semanticTargets.{:032x}", conflict.target),
            message: "the authored semantic target cannot be preserved during reimport".to_owned(),
            source_selector: Some(plant_source_selector_text(&conflict.selector)),
        }
    }));
    VegetationValidationSummaryDto {
        valid: outcome.compile.publishable(),
        issues,
    }
}

pub(crate) fn plant_compile_statistics_dto(
    statistics: saffron_vegetation::PlantCompileStatistics,
) -> PlantCompileStatisticsDto {
    PlantCompileStatisticsDto {
        sources: statistics.sources.to_string(),
        meshes: statistics.meshes.to_string(),
        vertices: statistics.vertices.to_string(),
        indices: statistics.indices.to_string(),
        joints: statistics.joints.to_string(),
        materials: statistics.materials.to_string(),
        rejected: statistics.rejected.to_string(),
    }
}

pub(crate) fn plant_source_reference_dto(
    source: &PlantSourceReference,
    updates: &BTreeMap<u128, [u8; 32]>,
) -> PlantSourceReferenceDto {
    PlantSourceReferenceDto {
        id: vegetation_guid(source.id),
        locator: match &source.locator {
            PlantSourceLocator::Asset(asset) => PlantSourceLocatorDto::Asset {
                asset: WireUuid(asset.value()),
            },
            PlantSourceLocator::File(uri) => PlantSourceLocatorDto::File { uri: uri.clone() },
        },
        role: match source.role {
            PlantSourceRole::Geometry => PlantSourceRoleDto::Geometry,
            PlantSourceRole::Material => PlantSourceRoleDto::Material,
            PlantSourceRole::Skeleton => PlantSourceRoleDto::Skeleton,
            PlantSourceRole::Collision => PlantSourceRoleDto::Collision,
            PlantSourceRole::Navigation => PlantSourceRoleDto::Navigation,
        },
        selector: plant_source_selector_dto(&source.selector),
        content_hash: coverage_hash_text(updates.get(&source.id).unwrap_or(&source.content_hash)),
        provenance: source_provenance_dto(&source.provenance),
    }
}

pub(crate) fn plant_sources_dto(
    asset: &PlantFamilyAsset,
    outcome: &PlantValidationOutcome,
) -> Vec<PlantSourceReferenceDto> {
    let updates = outcome
        .compile
        .source_updates
        .iter()
        .map(|update| (update.source, update.current))
        .collect::<BTreeMap<_, _>>();
    match &asset.source {
        PlantFamilySource::Imported(recipe) => recipe
            .sources
            .iter()
            .map(|source| plant_source_reference_dto(source, &updates))
            .collect(),
        PlantFamilySource::Native { .. } => Vec::new(),
    }
}

pub(crate) fn plant_source_updates_dto(
    outcome: &PlantValidationOutcome,
) -> Vec<PlantSourceHashUpdateDto> {
    outcome
        .compile
        .source_updates
        .iter()
        .map(|update| PlantSourceHashUpdateDto {
            source: vegetation_guid(update.source),
            previous: coverage_hash_text(&update.previous),
            current: coverage_hash_text(&update.current),
        })
        .collect()
}

pub(crate) fn vegetation_map_chunk_key_dto(
    key: saffron_vegetation::VegetationMapChunkKey,
) -> VegetationMapChunkKeyDto {
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

pub(crate) fn authored_field_tile_dto(
    tile: &saffron_vegetation::AuthoredFieldTile,
) -> saffron_protocol::AuthoredFieldTileDto {
    saffron_protocol::AuthoredFieldTileDto {
        channel: field_channel_dto(tile.channel),
        layer: vegetation_guid(tile.layer),
        dimensions: tile.dimensions,
        quantum_bits: tile.quantum_bits,
        values: tile.values.clone(),
    }
}
