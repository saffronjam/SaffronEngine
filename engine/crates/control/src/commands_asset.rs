//! The 51 asset/project-domain control commands: project lifecycle (get/new/open/save/
//! load/reload + create-script), model + texture import + instantiation, the catalog
//! (list/scan/clean/delete-unused), sub-asset extraction, model info + references + the
//! asset model, the asset-preview enter/exit + active-view switch, asset/folder management
//! (rename/move/create-folder/delete), usages + metadata + assignment, the material system
//! (create/assign/import/list/get/update/preview/set-graph/instance/override/compile/cook),
//! scene save+load, screenshot, and thumbnails (get/view/cache).
//!
//! The highest-coupling domain: a handler holds `&mut` to [`AssetServer`][saffron_assets::AssetServer], the
//! [`SceneEditContext`], and the renderer at once through the disjoint
//! [`EngineContext`] fields. Heavy lifting (the importers, the node-graph→Slang codegen,
//! the preview render) lives in `saffron-assets`; these handlers stay thin
//! orchestration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use saffron_assets::{
    AssetServer, BUILTIN_SPHERE_MESH_ID, BuiltinMesh, ContainerMetadata, MaterialAsset,
    PREVIEW_MATERIAL_ID, PlantRecookOptions, PlantRecookOutcome, PlantValidationOutcome,
    ProjectHost, ProjectInfo, analyze_clean, asset_bytes, asset_type_name, build_dependency_graph,
    clear_extraction, colorspace_for_role_explicit, colorspace_name, create_project_script,
    default_display_name, default_material_asset, delete_unused, exposed_parameter,
    extract_sub_asset, import_material_folder, import_vegetation_asset, load_biome_asset,
    load_catalog_material_asset, load_catalog_material_asset_raw, load_plant_family_asset,
    load_vegetation_map_snapshot, lower_graph_to_params, model_render_aabb, pbr_exposed_parameters,
    pick_scene_surface, portable_vegetation_platform_profile, recook_plant_family, reimport_model,
    request_thumbnail, save_material_asset, texture_role_from_hint, texture_role_name,
    update_material_asset, valid_project_name, validate_plant_family_sources,
    vegetation_cook_versions, viewport_ray,
};
use saffron_core::{HeightMode, Uuid};
use saffron_geometry::glam::{Vec2, Vec3 as MathVec3};
use saffron_protocol::{
    AlphaClassificationDto, AnimationClipDto, AppManifest, AssetAttributionDto,
    AssetCapabilitiesDto, AssetEntryDto, AssetList, AssetMetadataDto, AssetMetadataParams,
    AssetModelResult, AssetPlacementParams, AssetPlacementPhaseDto, AssetPlacementResult, AssetRef,
    AssetReferencesParams, AssetReferencesResult, AssetSelector, AssetSlotDto, AssetTypeDto,
    AssetUsageDto, AssetUsagesParams, AssetUsagesResult, AssignAssetParams, AssignAssetResult,
    BiomeAssetSummaryDto, BiomeRoleDto, BoneDto, BootStageDto, CleanAssetsParams,
    CleanCandidateDto, CleanReport, ClearExtractionParams, ControlDiagnosticDto,
    CoverageMipMetadataDto, CoverageSourceDto, CreateAssetFolderParams, CreateScriptParams,
    CreateScriptResult, DeleteAssetFolderParams, DeleteAssetParams, DeleteAssetResult,
    DeleteUnusedParams, DeleteUnusedResult, EmptyParams, EntityRef, ExportAppParams,
    ExportAppResult, ExposedParamDto, ExtractSubAssetParams, FieldBlendOperatorDto,
    FieldChannelDto, FieldChannelKindDto, GetAssetModelParams, ImportModelParams,
    ImportModelResult, ImportTextureParams, ImportTextureResult, ImportVegetationAssetParams,
    ImportVegetationAssetResult, InclusionOperatorDto, InstantiateModelParams,
    InteractionPolicyDto, LayerCoordinateSpaceDto, MaterialAssignParams, MaterialAssignResult,
    MaterialCompileParams, MaterialCompileResult, MaterialCookResult, MaterialCreateInstanceParams,
    MaterialCreateParams, MaterialCreateResult, MaterialGetParams, MaterialGetResult,
    MaterialImportParams, MaterialImportResultDto, MaterialListResult, MaterialRefDto,
    MaterialSchemaParams, MaterialSchemaResult, MaterialSetGraphParams, MaterialSetGraphResult,
    MaterialSetOverrideParams, MaterialSetOverrideResult, MaterialSurfaceDto, MaterialUpdateParams,
    MaterialUpdateResult, ModelInfoParams, ModelInfoResult, ModelSubAssetDto, MoveAssetParams,
    NewProjectParams, OpacityMicromapDerivationDto, OptionalPathParams, PathParams, PathResult,
    PlacementTransformDto, PlantAssetSummaryDto, PlantCompileDiagnosticCodeDto,
    PlantCompileDiagnosticDto, PlantCompileStatisticsDto, PlantId, PlantRecookParams,
    PlantRecookResult, PlantReimportConflictReasonDto, PlantSemanticDestinationDto,
    PlantSourceHashUpdateDto, PlantSourceKindDto, PlantSourceLocatorDto, PlantSourceReferenceDto,
    PlantSourceRoleDto, PlantSourceSelectorDto, PlantStateOverrideDto, PlantTransformOverrideDto,
    PlantValidateParams, PlantValidationResult, PlayStateResult, PreviewRenderParams,
    PreviewRenderResult, ProjectInfoDto, ProjectPhaseDto, ProjectStatusDto, ProjectStoresDto,
    QuitResult, ReimportConflictDiagnosticDto, ReimportConflictEntryDto, ReimportModelParams,
    ReimportModelResult, RenameAssetFolderParams, RenameAssetParams, ScanAssetsResult,
    ScreenshotParams, ScreenshotResult, ScreenshotTargetDto, SetActiveViewParams,
    SetActiveViewResult, SpeciesWeightDto, ThinSheetFoliageParametersDto,
    ThinSheetNormalBehaviorDto, ThumbnailCacheParams, ThumbnailCacheResult, ThumbnailFormatDto,
    ThumbnailParams, ThumbnailResult, Uuid as WireUuid, Vec3, Vec4, VegetationAssetSummaryDto,
    VegetationAssetSummaryParams, VegetationAssetSummaryResult,
    VegetationCandidateRejectionReasonDto, VegetationCookNodeAddressDto,
    VegetationCookRejectionTotalDto, VegetationCookStatisticsDto, VegetationGuid,
    VegetationLayerDto, VegetationLayerOperatorDto, VegetationManifestDependencyAddressDto,
    VegetationManifestDependencyDto, VegetationMapChunkKeyDto, VegetationMapChunkKindDto,
    VegetationMapSummaryDto, VegetationMapTileKeyDto, VegetationSourceProvenanceDto,
    VegetationValidationIssueDto, VegetationValidationSeverityDto, VegetationValidationSummaryDto,
    VoxelMaterialMomentsDto, WorldBoundsDto, WorldCellDto,
};
use saffron_rendering::ViewId;
use saffron_scene::{
    AnimationPlayer, AssetEntry, AssetType, Attribution, Colorspace, DirectionalLight, Entity,
    IdComponent, MaterialSet, MaterialSlot, Mesh, Name, PreviewGhost, Scene, SkinnedMesh, SkyMode,
    TextureRole, Transform, VegetationField,
};
use saffron_sceneedit::{
    BootStage, NewProjectSpec, OrbitState, PlacementPreview, PlayState, ProjectLoadRequest,
    ProjectPhase, SceneEditCamera, SceneEditContext,
};
use saffron_vegetation::{
    AlphaClassification, BiomeRole, CandidateRejectionReason, ContentHash, CookDependency,
    CookDependencyAddress, CookGraph, CookNodeAddress, CoverageMipMetadata, CoverageSource,
    FieldBlendOperator, InclusionOperator, LayerCoordinateSpace, MaterialSurface,
    OpacityMicromapDerivation, PlantCompileDiagnostic, PlantCompileDiagnosticCode,
    PlantCompileDiagnosticSeverity, PlantCompileLimits, PlantFamilyAsset, PlantFamilySource,
    PlantReimportConflict, PlantReimportConflictReason, PlantSemanticDestination,
    PlantSourceLocator, PlantSourceReference, PlantSourceRole, PlantSourceSelector,
    SourceProvenance, ThinSheetFoliageParameters, ThinSheetNormalBehavior, VegetationBaseManifest,
    VegetationCellArtifactIndex, VegetationCellSectionKind, VegetationLayer,
    VegetationLayerOperator, VegetationMapChunkKind, VegetationMapTileKey, VoxelMaterialMoments,
    vegetation_rejection_totals,
};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, ControlRenderer, EngineContext};
use crate::selector::{entity_ref_dto, entity_uuid, resolve_entity};

/// The `base64` standard encoder, used by `preview-render` and the thumbnail commands to
/// ship PNG bytes inside a JSON string.
fn base64_encode(bytes: &[u8]) -> String {
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
fn asset_type_dto(asset_type: AssetType) -> AssetTypeDto {
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

fn vegetation_guid(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

fn plant_id(value: saffron_vegetation::PlantId) -> PlantId {
    PlantId(value.to_string())
}

fn world_bounds_dto(bounds: saffron_spatial::WorldBounds) -> WorldBoundsDto {
    WorldBoundsDto {
        min_ticks: bounds.min_ticks().map(|value| value.to_string()),
        max_ticks_exclusive: bounds.max_ticks_exclusive().map(|value| value.to_string()),
    }
}

fn world_cell_dto(cell: saffron_spatial::WorldCellKey) -> WorldCellDto {
    WorldCellDto {
        coordinates: cell.coordinates().map(|value| value.to_string()),
        level: cell.level(),
    }
}

fn field_channel_dto(channel: saffron_spatial::FieldChannel) -> FieldChannelDto {
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

fn plant_source_selector_text(selector: &PlantSourceSelector) -> String {
    match selector {
        PlantSourceSelector::Whole => "whole".to_owned(),
        PlantSourceSelector::Element { id, path } => format!("element:{id:032x}:{path}"),
        PlantSourceSelector::Submesh { element, index } => {
            format!("submesh:{element:032x}:{index}")
        }
    }
}

fn plant_semantic_destination_dto(
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

fn plant_reimport_conflict_dto(conflict: &PlantReimportConflict) -> ReimportConflictEntryDto {
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

fn plant_diagnostic_code_dto(code: PlantCompileDiagnosticCode) -> PlantCompileDiagnosticCodeDto {
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

fn plant_diagnostic_code_text(code: PlantCompileDiagnosticCode) -> &'static str {
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

fn validation_severity_dto(
    severity: PlantCompileDiagnosticSeverity,
) -> VegetationValidationSeverityDto {
    match severity {
        PlantCompileDiagnosticSeverity::Info => VegetationValidationSeverityDto::Info,
        PlantCompileDiagnosticSeverity::Warning => VegetationValidationSeverityDto::Warning,
        PlantCompileDiagnosticSeverity::Error => VegetationValidationSeverityDto::Error,
    }
}

fn plant_compile_diagnostic_dto(diagnostic: &PlantCompileDiagnostic) -> PlantCompileDiagnosticDto {
    PlantCompileDiagnosticDto {
        severity: validation_severity_dto(diagnostic.severity),
        code: plant_diagnostic_code_dto(diagnostic.code),
        source: diagnostic.source.map(vegetation_guid),
        source_selector: diagnostic.selector.as_ref().map(plant_source_selector_dto),
        path: diagnostic.path.clone(),
        message: diagnostic.message.clone(),
    }
}

fn plant_validation_summary(outcome: &PlantValidationOutcome) -> VegetationValidationSummaryDto {
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

fn plant_compile_statistics_dto(
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

fn plant_source_reference_dto(
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

fn plant_sources_dto(
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

fn plant_source_updates_dto(outcome: &PlantValidationOutcome) -> Vec<PlantSourceHashUpdateDto> {
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

fn vegetation_map_chunk_key_dto(
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

fn authored_field_tile_dto(
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

fn plant_point_dto(point: &saffron_vegetation::PlantPoint) -> saffron_protocol::PlantPointDto {
    saffron_protocol::PlantPointDto {
        id: plant_id(point.id),
        owner: world_cell_dto(point.owner),
        local_position: point.position.local().ticks(),
        orientation: point.orientation.bits(),
        scale_bits: point.scale.map(|scale| scale.bits()),
        bounds: world_bounds_dto(point.bounds),
        family: WireUuid(point.family.value()),
        variation: point.variation,
        lifecycle: crate::commands_vegetation_runtime::lifecycle_dto(point.lifecycle),
        phenotype: point.phenotype,
        representation_class: point.representation_class,
        deterministic_key: vegetation_guid(point.deterministic_key),
        candidate: point.candidate.to_string(),
        parent: point.parent.map(plant_id),
        colony: point.colony.map(plant_id),
        ecology_tick: point.ecology_tick.to_string(),
        health: point.health.bits(),
        moisture: point.moisture.bits(),
        fuel: point.fuel.bits(),
        phenology: point.phenology.bits(),
        flags: point.flags.bits(),
        interaction_policy: interaction_policy_dto(point.interaction_policy),
        provenance: point.provenance,
        attachment: point
            .attachment
            .map(|attachment| saffron_protocol::SurfaceAttachmentDto {
                provider: attachment.provider.0.to_string(),
                primitive: attachment.primitive.0.to_string(),
                barycentric: attachment.barycentric.map(|value| value.bits()),
                revision: attachment.revision.0.to_string(),
            }),
        surface_projection_bits: point.surface_projection.map(|value| value.bits()),
    }
}

/// Serializes one authored chunk for the wire. Only the brush payload kinds cross:
/// layer metadata is served by `vegetation-asset-summary`, and graph-instance /
/// editor-metadata chunks have no wire payload shape.
fn vegetation_map_chunk_dto(
    chunk: &saffron_vegetation::VegetationMapChunk,
) -> Result<saffron_protocol::VegetationMapChunkDto> {
    let payload = match &chunk.payload {
        saffron_vegetation::VegetationMapChunkPayload::Field(field) => {
            saffron_protocol::VegetationMapChunkPayloadDto::Field {
                fields: field.fields.iter().map(authored_field_tile_dto).collect(),
                blockers: field.blockers.iter().map(authored_field_tile_dto).collect(),
            }
        }
        saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(anchors) => {
            saffron_protocol::VegetationMapChunkPayloadDto::AnchorOverride {
                explicit_plants: anchors
                    .explicit_plants
                    .iter()
                    .map(|anchor| saffron_protocol::ExplicitPlantAnchorDto {
                        id: plant_id(anchor.id),
                        layer: vegetation_guid(anchor.layer),
                        family: WireUuid(anchor.family.value()),
                        point: plant_point_dto(&anchor.point),
                    })
                    .collect(),
                pins: anchors.pins.iter().copied().map(plant_id).collect(),
                transform_overrides: anchors
                    .transform_overrides
                    .iter()
                    .map(|value| PlantTransformOverrideDto {
                        plant: plant_id(value.plant),
                        global_ticks: value
                            .position
                            .global_ticks()
                            .map(|component| component.to_string()),
                        scale_bits: value.scale.map(|component| component.bits()),
                    })
                    .collect(),
                state_overrides: anchors
                    .state_overrides
                    .iter()
                    .map(|value| PlantStateOverrideDto {
                        plant: plant_id(value.plant),
                        health: value.health.map(|item| item.bits()),
                        moisture: value.moisture.map(|item| item.bits()),
                        fuel: value.fuel.map(|item| item.bits()),
                        interaction_policy: value.interaction_policy.map(interaction_policy_dto),
                    })
                    .collect(),
            }
        }
        _ => {
            return Err(Error::command(
                "chunk read serves field and anchor-override chunks",
            ));
        }
    };
    Ok(saffron_protocol::VegetationMapChunkDto {
        key: vegetation_map_chunk_key_dto(chunk.key),
        revision: chunk.revision.to_string(),
        payload,
    })
}

fn cook_node_address_dto(address: &CookNodeAddress) -> VegetationCookNodeAddressDto {
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

fn manifest_dependencies_dto(
    dependencies: &[CookDependency],
) -> Vec<VegetationManifestDependencyDto> {
    dependencies.iter().map(manifest_dependency_dto).collect()
}

fn validation_error(
    code: &str,
    path: &str,
    message: impl Into<String>,
) -> VegetationValidationIssueDto {
    VegetationValidationIssueDto {
        severity: VegetationValidationSeverityDto::Error,
        code: code.to_owned(),
        path: path.to_owned(),
        message: message.into(),
        source_selector: None,
    }
}

fn source_asset_dependency(
    assets: &AssetServer,
    id: Uuid,
) -> std::result::Result<VegetationManifestDependencyDto, String> {
    let entry = assets
        .catalog()
        .find(id)
        .ok_or_else(|| format!("catalog asset {} is missing", id.value()))?;
    let bytes = std::fs::read(assets.root.join(&entry.path)).map_err(|error| error.to_string())?;
    Ok(VegetationManifestDependencyDto {
        address: VegetationManifestDependencyAddressDto::SourceAsset {
            asset: WireUuid(id.value()),
        },
        content_hash: ContentHash::of(&bytes).to_string(),
        bounds: None,
        halo_bits: 0,
        ancestor_level: None,
    })
}

fn biome_summary_metadata(
    assets: &AssetServer,
    root: &saffron_vegetation::BiomeAsset,
) -> (
    VegetationValidationSummaryDto,
    Vec<VegetationManifestDependencyDto>,
) {
    fn visit(
        assets: &AssetServer,
        biome: &saffron_vegetation::BiomeAsset,
        active: &mut BTreeSet<u64>,
        complete: &mut BTreeSet<u64>,
        dependencies: &mut BTreeMap<u64, VegetationManifestDependencyDto>,
        issues: &mut Vec<VegetationValidationIssueDto>,
    ) {
        if complete.contains(&biome.id.value()) {
            return;
        }
        if !active.insert(biome.id.value()) {
            issues.push(validation_error(
                "biome-module-cycle",
                "modules",
                format!("biome module cycle reaches asset {}", biome.id.value()),
            ));
            return;
        }
        match source_asset_dependency(assets, biome.id) {
            Ok(dependency) => {
                dependencies.insert(biome.id.value(), dependency);
            }
            Err(message) => issues.push(validation_error(
                "missing-source-asset",
                "dependencies",
                message,
            )),
        }
        for palette in &biome.palette {
            match source_asset_dependency(assets, palette.plant) {
                Ok(dependency) => {
                    dependencies.insert(palette.plant.value(), dependency);
                }
                Err(message) => issues.push(validation_error(
                    "missing-plant-dependency",
                    "palette",
                    message,
                )),
            }
        }
        for module in &biome.modules {
            match load_biome_asset(assets, module.biome) {
                Ok(module_asset) => visit(
                    assets,
                    &module_asset,
                    active,
                    complete,
                    dependencies,
                    issues,
                ),
                Err(error) => issues.push(validation_error(
                    "invalid-biome-module",
                    "modules",
                    error.to_string(),
                )),
            }
        }
        active.remove(&biome.id.value());
        complete.insert(biome.id.value());
    }

    let mut active = BTreeSet::new();
    let mut complete = BTreeSet::new();
    let mut dependencies = BTreeMap::new();
    let mut issues = Vec::new();
    visit(
        assets,
        root,
        &mut active,
        &mut complete,
        &mut dependencies,
        &mut issues,
    );
    (
        VegetationValidationSummaryDto {
            valid: issues.is_empty(),
            issues,
        },
        dependencies.into_values().collect(),
    )
}

fn map_authored_metadata(
    assets: &AssetServer,
    map: &saffron_vegetation::VegetationMapSnapshot,
) -> (
    VegetationValidationSummaryDto,
    Vec<VegetationManifestDependencyDto>,
) {
    let mut issues = Vec::new();
    let mut dependencies = Vec::with_capacity(map.inventory.len().saturating_add(1));
    let manifest_hash = assets
        .catalog()
        .find(map.id)
        .ok_or_else(|| format!("catalog asset {} is missing", map.id.value()))
        .and_then(|entry| {
            std::fs::read(assets.root.join(&entry.path)).map_err(|error| error.to_string())
        });
    match manifest_hash {
        Ok(bytes) => dependencies.push(VegetationManifestDependencyDto {
            address: VegetationManifestDependencyAddressDto::MapManifest {
                map: WireUuid(map.id.value()),
            },
            content_hash: ContentHash::of(&bytes).to_string(),
            bounds: Some(world_bounds_dto(map.bounds)),
            halo_bits: 0,
            ancestor_level: None,
        }),
        Err(message) => issues.push(validation_error("missing-map-manifest", "root", message)),
    }
    dependencies.extend(
        map.inventory
            .iter()
            .map(|reference| VegetationManifestDependencyDto {
                address: VegetationManifestDependencyAddressDto::MapObject {
                    map: WireUuid(map.id.value()),
                    key: vegetation_map_chunk_key_dto(reference.key),
                },
                content_hash: coverage_hash_text(&reference.content_hash),
                bounds: None,
                halo_bits: 0,
                ancestor_level: None,
            }),
    );

    let mut source_assets = map
        .biome_instances
        .iter()
        .map(|instance| instance.biome.value())
        .collect::<BTreeSet<_>>();
    for layer in &map.layers {
        if let VegetationLayerOperator::SpeciesWeights(weights) = &layer.operator {
            source_assets.extend(weights.iter().map(|weight| weight.family.value()));
        }
    }
    for chunk in &map.chunks {
        if let saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(payload) =
            &chunk.payload
        {
            source_assets.extend(
                payload
                    .explicit_plants
                    .iter()
                    .map(|plant| plant.family.value()),
            );
        }
    }
    for source in source_assets {
        match source_asset_dependency(assets, Uuid(source)) {
            Ok(dependency) => dependencies.push(dependency),
            Err(message) => issues.push(validation_error(
                "missing-map-source-asset",
                "dependencies",
                message,
            )),
        }
    }
    (
        VegetationValidationSummaryDto {
            valid: issues.is_empty(),
            issues,
        },
        dependencies,
    )
}

fn rejection_reason_index(reason: CandidateRejectionReason) -> usize {
    match reason {
        CandidateRejectionReason::SurfaceMiss => 0,
        CandidateRejectionReason::Threshold => 1,
        CandidateRejectionReason::WeightedElimination => 2,
        CandidateRejectionReason::PriorityExclusion => 3,
        CandidateRejectionReason::Competition => 4,
        CandidateRejectionReason::ForeignOwner => 5,
        CandidateRejectionReason::NoSpecies => 6,
    }
}

pub(crate) fn rejection_reason_dto(
    reason: CandidateRejectionReason,
) -> VegetationCandidateRejectionReasonDto {
    match reason {
        CandidateRejectionReason::SurfaceMiss => VegetationCandidateRejectionReasonDto::SurfaceMiss,
        CandidateRejectionReason::Threshold => VegetationCandidateRejectionReasonDto::Threshold,
        CandidateRejectionReason::WeightedElimination => {
            VegetationCandidateRejectionReasonDto::WeightedElimination
        }
        CandidateRejectionReason::PriorityExclusion => {
            VegetationCandidateRejectionReasonDto::PriorityExclusion
        }
        CandidateRejectionReason::Competition => VegetationCandidateRejectionReasonDto::Competition,
        CandidateRejectionReason::ForeignOwner => {
            VegetationCandidateRejectionReasonDto::ForeignOwner
        }
        CandidateRejectionReason::NoSpecies => VegetationCandidateRejectionReasonDto::NoSpecies,
    }
}

fn current_map_cook_metadata(
    assets: &AssetServer,
    map: Uuid,
) -> Result<
    Option<(
        VegetationBaseManifest,
        Vec<VegetationManifestDependencyDto>,
        VegetationCookStatisticsDto,
    )>,
> {
    let store = assets.vegetation_artifact_store();
    let Some(manifest_bytes) = store.read_current_manifest(map).map_err(Error::from)? else {
        return Ok(None);
    };
    let manifest = VegetationBaseManifest::from_canonical_bytes(&manifest_bytes)?;
    let graph_bytes = store
        .read_cook_graph(manifest.cook_graph_hash)
        .map_err(Error::from)?;
    let graph = CookGraph::from_canonical_bytes(&graph_bytes)?;
    let mut rejection_totals = [0_u64; 7];
    for node in &graph.nodes {
        if !matches!(node.address, CookNodeAddress::Cell { .. }) {
            continue;
        }
        let artifact = store.read_cell(node.output_hash).map_err(Error::from)?;
        let index = VegetationCellArtifactIndex::open(
            &artifact,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )?;
        let diagnostics = index
            .section(&artifact, VegetationCellSectionKind::RejectionDiagnostics)?
            .ok_or_else(|| {
                Error::command("vegetation cell has no rejection diagnostics section")
            })?;
        for (reason, count) in vegetation_rejection_totals(diagnostics.as_ref())? {
            let total = &mut rejection_totals[rejection_reason_index(reason)];
            *total = total.saturating_add(count);
        }
    }
    let rejection_reasons = [
        CandidateRejectionReason::SurfaceMiss,
        CandidateRejectionReason::Threshold,
        CandidateRejectionReason::WeightedElimination,
        CandidateRejectionReason::PriorityExclusion,
        CandidateRejectionReason::Competition,
        CandidateRejectionReason::ForeignOwner,
        CandidateRejectionReason::NoSpecies,
    ];
    let nodes = u64::try_from(graph.nodes.len()).unwrap_or(u64::MAX);
    let statistics = VegetationCookStatisticsDto {
        nodes: nodes.to_string(),
        elapsed_micros: graph
            .nodes
            .iter()
            .fold(0_u64, |total, node| {
                total.saturating_add(node.actual.elapsed_micros)
            })
            .to_string(),
        peak_memory_bytes: graph
            .nodes
            .iter()
            .map(|node| node.actual.peak_memory_bytes)
            .max()
            .unwrap_or(0)
            .to_string(),
        input_bytes: graph
            .nodes
            .iter()
            .fold(0_u64, |total, node| {
                total.saturating_add(node.actual.input_bytes)
            })
            .to_string(),
        output_bytes: graph
            .nodes
            .iter()
            .fold(0_u64, |total, node| {
                total.saturating_add(node.actual.output_bytes)
            })
            .to_string(),
        cache_hits: graph
            .nodes
            .iter()
            .filter(|node| node.actual.cache_hit)
            .count()
            .to_string(),
        cache_misses: graph
            .nodes
            .iter()
            .filter(|node| !node.actual.cache_hit)
            .count()
            .to_string(),
        published_cells: graph
            .nodes
            .iter()
            .filter(|node| {
                matches!(node.address, CookNodeAddress::Cell { .. }) && !node.actual.cache_hit
            })
            .count()
            .to_string(),
        rejections: rejection_reasons
            .into_iter()
            .zip(rejection_totals)
            .filter(|(_, count)| *count != 0)
            .map(|(reason, count)| VegetationCookRejectionTotalDto {
                reason: rejection_reason_dto(reason),
                count: count.to_string(),
            })
            .collect(),
    };
    let dependencies = manifest_dependencies_dto(&manifest.dependencies);
    Ok(Some((manifest, dependencies, statistics)))
}

/// Layers whose authored chunks differ from what `manifest` consumed (all layers
/// carrying authored content when no manifest exists) — a recook would change the
/// cooked output for these.
fn dirty_map_layers(
    map: Uuid,
    inventory: &[saffron_vegetation::VegetationMapChunkReference],
    manifest: Option<&VegetationBaseManifest>,
) -> Vec<VegetationGuid> {
    let layer_owned = |kind: VegetationMapChunkKind| {
        matches!(
            kind,
            VegetationMapChunkKind::LayerMetadata
                | VegetationMapChunkKind::Field
                | VegetationMapChunkKind::AnchorOverride
        )
    };
    let mut dirty = std::collections::BTreeSet::new();
    let Some(manifest) = manifest else {
        for reference in inventory {
            if layer_owned(reference.key.kind) {
                dirty.insert(reference.key.layer);
            }
        }
        return dirty.into_iter().map(vegetation_guid).collect();
    };
    let consumed = manifest
        .dependencies
        .iter()
        .filter_map(|dependency| match &dependency.address {
            CookDependencyAddress::MapObject {
                map: dependency_map,
                key,
            } if *dependency_map == map => Some((*key, dependency.content_hash)),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for reference in inventory {
        if layer_owned(reference.key.kind)
            && consumed.get(&reference.key).map(|hash| hash.bytes()) != Some(reference.content_hash)
        {
            dirty.insert(reference.key.layer);
        }
    }
    for key in consumed.keys() {
        if layer_owned(key.kind) && !inventory.iter().any(|reference| reference.key == *key) {
            dirty.insert(key.layer);
        }
    }
    dirty.into_iter().map(vegetation_guid).collect()
}

fn field_blend_dto(operator: FieldBlendOperator) -> FieldBlendOperatorDto {
    match operator {
        FieldBlendOperator::Replace => FieldBlendOperatorDto::Replace,
        FieldBlendOperator::Add => FieldBlendOperatorDto::Add,
        FieldBlendOperator::Multiply => FieldBlendOperatorDto::Multiply,
        FieldBlendOperator::Minimum => FieldBlendOperatorDto::Minimum,
        FieldBlendOperator::Maximum => FieldBlendOperatorDto::Maximum,
    }
}

fn inclusion_dto(operator: InclusionOperator) -> InclusionOperatorDto {
    match operator {
        InclusionOperator::Include => InclusionOperatorDto::Include,
        InclusionOperator::Exclude => InclusionOperatorDto::Exclude,
    }
}

fn interaction_policy_dto(policy: saffron_vegetation::InteractionPolicy) -> InteractionPolicyDto {
    match policy {
        saffron_vegetation::InteractionPolicy::Decorative => InteractionPolicyDto::Decorative,
        saffron_vegetation::InteractionPolicy::Interactive => InteractionPolicyDto::Interactive,
        saffron_vegetation::InteractionPolicy::Harvestable => InteractionPolicyDto::Harvestable,
        saffron_vegetation::InteractionPolicy::Structural => InteractionPolicyDto::Structural,
    }
}

fn coverage_hash_text(hash: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
    }
    text
}

fn parse_coverage_hash(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::command(
            "coverage mip hash must be 64 lowercase hexadecimal digits",
        ));
    }
    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::command("coverage mip hash is invalid"))?;
    }
    Ok(hash)
}

fn material_surface_dto(surface: &MaterialSurface) -> MaterialSurfaceDto {
    let MaterialSurface::ThinSheetFoliage(parameters) = surface else {
        return MaterialSurfaceDto::Standard;
    };
    MaterialSurfaceDto::ThinSheetFoliage {
        parameters: ThinSheetFoliageParametersDto {
            front_albedo_response: parameters.front_albedo_response.bits(),
            back_albedo_response: parameters.back_albedo_response.bits(),
            thickness_bits: parameters.thickness.bits(),
            absorption_color_bits: parameters.absorption_color.map(|value| value.bits()),
            transmission_color_bits: parameters.transmission_color.map(|value| value.bits()),
            roughness: parameters.roughness.bits(),
            normal_behavior: match parameters.normal_behavior {
                ThinSheetNormalBehavior::Preserve => ThinSheetNormalBehaviorDto::Preserve,
                ThinSheetNormalBehavior::FaceForwardBack => {
                    ThinSheetNormalBehaviorDto::FaceForwardBack
                }
                ThinSheetNormalBehavior::Symmetric => ThinSheetNormalBehaviorDto::Symmetric,
            },
            coverage_source: match parameters.coverage_source {
                CoverageSource::AlbedoAlpha => CoverageSourceDto::AlbedoAlpha,
                CoverageSource::Texture(texture) => CoverageSourceDto::Texture {
                    texture: WireUuid(texture.value()),
                },
                CoverageSource::ModeledGeometry => CoverageSourceDto::ModeledGeometry,
            },
            coverage: CoverageMipMetadataDto {
                reference_cutoff: parameters.coverage.reference_cutoff.bits(),
                source_extent: parameters.coverage.source_extent,
                spatial_hash_salt: parameters.coverage.spatial_hash_salt.to_string(),
                classification: match parameters.coverage.classification {
                    AlphaClassification::Opaque => AlphaClassificationDto::Opaque,
                    AlphaClassification::Masked => AlphaClassificationDto::Masked,
                    AlphaClassification::Transmissive => AlphaClassificationDto::Transmissive,
                },
                mip_hashes: parameters
                    .coverage
                    .mip_hashes
                    .iter()
                    .map(coverage_hash_text)
                    .collect(),
            },
            voxel_moments: VoxelMaterialMomentsDto {
                occupancy: parameters.voxel_moments.occupancy.bits(),
                albedo_mean_bits: parameters
                    .voxel_moments
                    .albedo_mean
                    .map(|value| value.bits()),
                roughness_mean: parameters.voxel_moments.roughness_mean.bits(),
                transmission_mean_bits: parameters
                    .voxel_moments
                    .transmission_mean
                    .map(|value| value.bits()),
                thickness_mean_bits: parameters.voxel_moments.thickness_mean.bits(),
                normal_second_moments_bits: parameters
                    .voxel_moments
                    .normal_second_moments
                    .map(|value| value.bits()),
            },
            opacity_micromap: OpacityMicromapDerivationDto {
                enabled: parameters.opacity_micromap.enabled,
                max_subdivision: parameters.opacity_micromap.max_subdivision,
                transparent_threshold: parameters.opacity_micromap.transparent_threshold.bits(),
                opaque_threshold: parameters.opacity_micromap.opaque_threshold.bits(),
            },
            energy_limit: parameters.energy_limit.bits(),
        },
    }
}

fn material_surface_from_dto(surface: MaterialSurfaceDto) -> Result<MaterialSurface> {
    let surface = match surface {
        MaterialSurfaceDto::Standard => MaterialSurface::Standard,
        MaterialSurfaceDto::ThinSheetFoliage { parameters } => {
            let coverage_source = match parameters.coverage_source {
                CoverageSourceDto::AlbedoAlpha => CoverageSource::AlbedoAlpha,
                CoverageSourceDto::Texture { texture } => CoverageSource::Texture(Uuid(texture.0)),
                CoverageSourceDto::ModeledGeometry => CoverageSource::ModeledGeometry,
            };
            let coverage = CoverageMipMetadata {
                reference_cutoff: saffron_spatial::UnitInterval::from_bits(
                    parameters.coverage.reference_cutoff,
                ),
                source_extent: parameters.coverage.source_extent,
                spatial_hash_salt: parameters
                    .coverage
                    .spatial_hash_salt
                    .parse::<u64>()
                    .map_err(|_| Error::command("coverage spatial hash salt is invalid"))?,
                classification: match parameters.coverage.classification {
                    AlphaClassificationDto::Opaque => AlphaClassification::Opaque,
                    AlphaClassificationDto::Masked => AlphaClassification::Masked,
                    AlphaClassificationDto::Transmissive => AlphaClassification::Transmissive,
                },
                mip_hashes: parameters
                    .coverage
                    .mip_hashes
                    .iter()
                    .map(|hash| parse_coverage_hash(hash))
                    .collect::<Result<Vec<_>>>()?,
            };
            MaterialSurface::ThinSheetFoliage(ThinSheetFoliageParameters {
                front_albedo_response: saffron_spatial::UnitInterval::from_bits(
                    parameters.front_albedo_response,
                ),
                back_albedo_response: saffron_spatial::UnitInterval::from_bits(
                    parameters.back_albedo_response,
                ),
                thickness: saffron_spatial::DecisionScalar::from_bits(parameters.thickness_bits),
                absorption_color: parameters
                    .absorption_color_bits
                    .map(saffron_spatial::DecisionScalar::from_bits),
                transmission_color: parameters
                    .transmission_color_bits
                    .map(saffron_spatial::DecisionScalar::from_bits),
                roughness: saffron_spatial::UnitInterval::from_bits(parameters.roughness),
                normal_behavior: match parameters.normal_behavior {
                    ThinSheetNormalBehaviorDto::Preserve => ThinSheetNormalBehavior::Preserve,
                    ThinSheetNormalBehaviorDto::FaceForwardBack => {
                        ThinSheetNormalBehavior::FaceForwardBack
                    }
                    ThinSheetNormalBehaviorDto::Symmetric => ThinSheetNormalBehavior::Symmetric,
                },
                coverage_source,
                coverage,
                voxel_moments: VoxelMaterialMoments {
                    occupancy: saffron_spatial::UnitInterval::from_bits(
                        parameters.voxel_moments.occupancy,
                    ),
                    albedo_mean: parameters
                        .voxel_moments
                        .albedo_mean_bits
                        .map(saffron_spatial::DecisionScalar::from_bits),
                    roughness_mean: saffron_spatial::UnitInterval::from_bits(
                        parameters.voxel_moments.roughness_mean,
                    ),
                    transmission_mean: parameters
                        .voxel_moments
                        .transmission_mean_bits
                        .map(saffron_spatial::DecisionScalar::from_bits),
                    thickness_mean: saffron_spatial::DecisionScalar::from_bits(
                        parameters.voxel_moments.thickness_mean_bits,
                    ),
                    normal_second_moments: parameters
                        .voxel_moments
                        .normal_second_moments_bits
                        .map(saffron_spatial::DecisionScalar::from_bits),
                },
                opacity_micromap: OpacityMicromapDerivation {
                    enabled: parameters.opacity_micromap.enabled,
                    max_subdivision: parameters.opacity_micromap.max_subdivision,
                    transparent_threshold: saffron_spatial::UnitInterval::from_bits(
                        parameters.opacity_micromap.transparent_threshold,
                    ),
                    opaque_threshold: saffron_spatial::UnitInterval::from_bits(
                        parameters.opacity_micromap.opaque_threshold,
                    ),
                },
                energy_limit: saffron_spatial::UnitInterval::from_bits(parameters.energy_limit),
            })
        }
    };
    surface
        .validate()
        .map_err(|error| Error::command(error.to_string()))?;
    Ok(surface)
}

fn vegetation_layer_dto(layer: &VegetationLayer) -> VegetationLayerDto {
    let operator = match &layer.operator {
        VegetationLayerOperator::ScalarField(field) => VegetationLayerOperatorDto::ScalarField {
            channel: field_channel_dto(field.channel),
            tile_set: vegetation_guid(field.tile_set),
            blend: field_blend_dto(field.blend),
            weight: field.weight.bits(),
        },
        VegetationLayerOperator::VectorField {
            channel,
            tile_set,
            value,
            blend,
        } => VegetationLayerOperatorDto::VectorField {
            channel: field_channel_dto(*channel),
            tile_set: vegetation_guid(*tile_set),
            value_bits: [value.x.bits(), value.y.bits(), value.z.bits()],
            blend: field_blend_dto(*blend),
        },
        VegetationLayerOperator::SpeciesWeights(weights) => {
            VegetationLayerOperatorDto::SpeciesWeights {
                weights: weights
                    .iter()
                    .map(|weight| SpeciesWeightDto {
                        family: WireUuid(weight.family.value()),
                        weight: weight.weight.bits(),
                    })
                    .collect(),
            }
        }
        VegetationLayerOperator::Density(field) => VegetationLayerOperatorDto::Density {
            channel: field_channel_dto(field.channel),
            tile_set: vegetation_guid(field.tile_set),
            blend: field_blend_dto(field.blend),
            weight: field.weight.bits(),
        },
        VegetationLayerOperator::Mask {
            tile_set,
            operation,
        } => VegetationLayerOperatorDto::Mask {
            tile_set: vegetation_guid(*tile_set),
            operation: inclusion_dto(*operation),
        },
        VegetationLayerOperator::Volume(volume) => VegetationLayerOperatorDto::Volume {
            bounds: world_bounds_dto(volume.bounds),
            operation: inclusion_dto(volume.operation),
            falloff_bits: volume.falloff.bits(),
        },
        VegetationLayerOperator::Spline(spline) => VegetationLayerOperatorDto::Spline {
            spline: vegetation_guid(spline.spline),
            points: spline
                .points
                .iter()
                .map(|point| point.global_ticks().map(|value| value.to_string()))
                .collect(),
            radius_bits: spline.radius.bits(),
            operation: inclusion_dto(spline.operation),
        },
        VegetationLayerOperator::Anchors(plants) => VegetationLayerOperatorDto::Anchors {
            plants: plants.iter().copied().map(plant_id).collect(),
        },
        VegetationLayerOperator::Pins(plants) => VegetationLayerOperatorDto::Pins {
            plants: plants.iter().copied().map(plant_id).collect(),
        },
        VegetationLayerOperator::TransformOverrides(overrides) => {
            VegetationLayerOperatorDto::TransformOverrides {
                overrides: overrides
                    .iter()
                    .map(|value| PlantTransformOverrideDto {
                        plant: plant_id(value.plant),
                        global_ticks: value
                            .position
                            .global_ticks()
                            .map(|component| component.to_string()),
                        scale_bits: value.scale.map(|component| component.bits()),
                    })
                    .collect(),
            }
        }
        VegetationLayerOperator::StateOverrides(overrides) => {
            VegetationLayerOperatorDto::StateOverrides {
                overrides: overrides
                    .iter()
                    .map(|value| PlantStateOverrideDto {
                        plant: plant_id(value.plant),
                        health: value.health.map(|item| item.bits()),
                        moisture: value.moisture.map(|item| item.bits()),
                        fuel: value.fuel.map(|item| item.bits()),
                        interaction_policy: value.interaction_policy.map(interaction_policy_dto),
                    })
                    .collect(),
            }
        }
        VegetationLayerOperator::Blocker {
            tile_set,
            categories,
        } => VegetationLayerOperatorDto::Blocker {
            tile_set: vegetation_guid(*tile_set),
            categories: *categories,
        },
    };
    VegetationLayerDto {
        id: vegetation_guid(layer.id),
        name: layer.name.clone(),
        coordinate_space: match layer.coordinate_space {
            LayerCoordinateSpace::World => LayerCoordinateSpaceDto::World,
            LayerCoordinateSpace::Surface => LayerCoordinateSpaceDto::Surface,
            LayerCoordinateSpace::OwnerLocal => LayerCoordinateSpaceDto::OwnerLocal,
        },
        bounds: world_bounds_dto(layer.bounds),
        operator,
        dependencies: layer
            .dependencies
            .iter()
            .copied()
            .map(vegetation_guid)
            .collect(),
        order: layer.order,
        locked: layer.locked,
        muted: layer.muted,
        revision: layer.revision.to_string(),
    }
}

/// Reads an id-or-name selector value as its string form, treating any non-string as empty.
fn selector_string(selector: &AssetSelector) -> String {
    selector.name().unwrap_or_default().to_owned()
}

/// Wraps a folder path as an optional, mapping an empty path to `None`.
fn optional_folder(folder: &str) -> Option<String> {
    if folder.is_empty() {
        None
    } else {
        Some(folder.to_owned())
    }
}

/// The uuid an id-or-name selector resolves to: an unsigned number, a non-negative signed
/// number, or a whole-string decimal parse.
fn selector_id(selector: &AssetSelector) -> u64 {
    selector.id().unwrap_or(0)
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to a catalog entry id,
/// by id or name.
pub(crate) fn resolve_asset(ctx: &EngineContext<'_>, selector: &AssetSelector) -> Result<Uuid> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for entry in &ctx.assets.catalog().entries {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(entry.id);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to its index in the
/// catalog `entries`.
fn resolve_asset_index(ctx: &EngineContext<'_>, selector: &AssetSelector) -> Result<usize> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for (i, entry) in ctx.assets.catalog().entries.iter().enumerate() {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(i);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

fn preview_asset_placement(
    ctx: &mut EngineContext<'_>,
    params: AssetPlacementParams,
) -> Result<AssetPlacementResult> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        return Err(Error::command(
            "asset placement is only available in Edit mode",
        ));
    }
    if ctx.scene_edit.preview_active_view {
        return Err(Error::command(
            "asset placement targets the scene view, not the asset preview",
        ));
    }
    let selector = params
        .asset
        .as_ref()
        .ok_or_else(|| Error::command("missing 'asset'"))?;
    let asset = resolve_asset(ctx, selector)?;
    let entry = ctx
        .assets
        .catalog()
        .find(asset)
        .ok_or_else(|| Error::command(format!("no asset '{}'", asset.value())))?;
    if entry.asset_type != AssetType::Model {
        return Err(Error::command(format!(
            "asset {} is not a model",
            asset.value()
        )));
    }

    let u = params.u.unwrap_or(0.5).clamp(0.0, 1.0);
    let v = params.v.unwrap_or(0.5).clamp(0.0, 1.0);
    let ndc = Vec2::new(u * 2.0 - 1.0, v * 2.0 - 1.0);
    let viewport = (
        ctx.renderer.viewport_width(),
        ctx.renderer.viewport_height(),
    );
    let cam = ctx.scene_edit.render_camera_view();
    let name = entry.name.clone();

    // Reuse the existing ghost if it already previews this asset; otherwise (re)instantiate one
    // into the authored scene and tag its whole subtree so it is excluded from save / pick /
    // outliner while it renders.
    let reuse = ctx
        .scene_edit
        .placement_preview
        .as_ref()
        .filter(|p| p.asset == asset)
        .map(|p| p.root)
        .filter(|&root| ctx.scene_edit.scene.has_component::<IdComponent>(root));
    let root = match reuse {
        Some(root) => root,
        None => {
            clear_placement_ghost(ctx);
            let root = ctx
                .assets
                .instantiate_model(&mut ctx.scene_edit.scene, asset, &name)
                .map_err(|e| Error::command(e.to_string()))?;
            for node in ctx.scene_edit.scene.subtree_entities(root) {
                let _ = ctx
                    .scene_edit
                    .scene
                    .add_component(node, PreviewGhost::default());
            }
            ctx.scene_edit.placement_preview = Some(PlacementPreview {
                asset,
                root,
                rest_bounds: None,
            });
            root
        }
    };

    let mut placement = None;
    {
        let scene = &mut ctx.scene_edit.scene;
        let assets = &mut ctx.assets;
        let preview_slot = &mut ctx.scene_edit.placement_preview;
        let renderer = &mut ctx.renderer;
        renderer.with_gpu_uploader(&mut |gpu| {
            scene.update_world_transforms();
            // Measure the rest-pose bounds once (before any placement transform skews them);
            // a later resolve (Phase 2 async upload) fills them in if the mesh wasn't ready.
            if let Some(preview) = preview_slot.as_mut()
                && preview.rest_bounds.is_none()
            {
                preview.rest_bounds = model_render_aabb(gpu, scene, assets, root);
            }
            let bounds = preview_slot.as_ref().and_then(|p| p.rest_bounds);
            placement = Some(compute_asset_placement(
                gpu, viewport, scene, assets, bounds, &cam, ndc,
            ));
        });
    }
    let transform = match placement.ok_or_else(|| Error::command("upload seam unavailable"))? {
        Ok(transform) => transform,
        Err(reason) => {
            return Ok(AssetPlacementResult {
                active: true,
                valid: false,
                transform: None,
                entity: None,
                reason: Some(reason),
            });
        }
    };
    apply_transform(&mut ctx.scene_edit.scene, root, transform);

    Ok(AssetPlacementResult {
        active: true,
        valid: true,
        transform: Some(placement_transform_dto(&transform)),
        entity: None,
        reason: None,
    })
}

/// Destroys the current placement ghost subtree (if any) and clears the preview slot.
fn clear_placement_ghost(ctx: &mut EngineContext<'_>) {
    if let Some(preview) = ctx.scene_edit.placement_preview.take() {
        ctx.scene_edit.scene.destroy_entity(preview.root);
    }
}

fn commit_asset_placement(ctx: &mut EngineContext<'_>) -> Result<AssetPlacementResult> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        clear_placement_ghost(ctx);
        return Err(Error::command(
            "asset placement is only available in Edit mode",
        ));
    }
    if ctx.scene_edit.preview_active_view {
        clear_placement_ghost(ctx);
        return Err(Error::command(
            "asset placement targets the scene view, not the asset preview",
        ));
    }
    let Some(preview) = ctx.scene_edit.placement_preview.take() else {
        return Ok(AssetPlacementResult {
            active: false,
            valid: false,
            transform: None,
            entity: None,
            reason: Some("no active placement preview".to_owned()),
        });
    };
    // The ghost already sits in the scene at the placement transform with its geometry uploaded;
    // committing is just dropping the tag from its subtree so it persists and selects.
    let root = preview.root;
    for node in ctx.scene_edit.scene.subtree_entities(root) {
        ctx.scene_edit.scene.remove_component::<PreviewGhost>(node);
    }
    let transform = ctx
        .scene_edit
        .scene
        .with_component::<Transform, _>(root, |t| *t)
        .unwrap_or_default();
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.set_selection(root);
    let entity = {
        let scene = &mut ctx.scene_edit.scene;
        entity_ref_dto(scene, root)
    };
    Ok(AssetPlacementResult {
        active: false,
        valid: true,
        transform: Some(placement_transform_dto(&transform)),
        entity: Some(entity),
        reason: None,
    })
}

/// Computes the placement transform that drops a model with the given rest-pose world AABB
/// `rest_bounds` onto the surface (or ground plane) under the cursor. The placement ray skips
/// [`PreviewGhost`]-tagged geometry, so it sees only the authored scene, never the ghost itself.
fn compute_asset_placement(
    gpu: &dyn saffron_assets::GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    rest_bounds: Option<(MathVec3, MathVec3)>,
    cam: &saffron_scene::CameraView,
    ndc: Vec2,
) -> std::result::Result<Transform, String> {
    if viewport.0 == 0 || viewport.1 == 0 {
        return Err("viewport has zero size".to_owned());
    }
    let ray = viewport_ray(viewport, cam, ndc);
    let target = pick_scene_surface(gpu, viewport, scene, assets, cam, ndc)
        .map_err(|error| error.to_string())?
        .map(|hit| {
            hit.surface
                .position
                .to_render_relative(saffron_spatial::WorldPosition::origin())
        })
        .transpose()
        .map_err(|error| error.to_string())?
        .or_else(|| ground_plane_hit(ray))
        .ok_or_else(|| "placement ray did not hit the scene or ground plane".to_owned())?;
    let (min, max) = rest_bounds.ok_or_else(|| "model has no renderable bounds".to_owned())?;
    let bottom_center = MathVec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
    Ok(Transform {
        translation: target - bottom_center,
        rotation: MathVec3::ZERO,
        scale: MathVec3::ONE,
    })
}

fn ground_plane_hit(ray: saffron_geometry::Ray) -> Option<MathVec3> {
    if ray.dir.y.abs() < 0.0001 {
        return None;
    }
    let t = -ray.origin.y / ray.dir.y;
    (t >= 0.0).then_some(ray.origin + ray.dir * t)
}

fn apply_transform(scene: &mut Scene, entity: Entity, transform: Transform) {
    let _ = scene.with_component_mut::<Transform, _>(entity, |t| *t = transform);
}

fn placement_transform_dto(transform: &Transform) -> PlacementTransformDto {
    PlacementTransformDto {
        translation: vec3(transform.translation),
        rotation: vec3(transform.rotation),
        scale: vec3(transform.scale),
    }
}

/// Parses the opaque `stores` sidecar block into the wire DTO, defaulting when absent.
fn stores_dto_from_value(value: &serde_json::Value) -> ProjectStoresDto {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Maps an `import-texture` colorspace hint to a `Colorspace`; `auto`/absent → heuristic.
fn colorspace_from_str(value: Option<&str>) -> Option<Colorspace> {
    match value.map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("srgb") => Some(Colorspace::Srgb),
        Some("linear") => Some(Colorspace::Linear),
        Some("hdr") => Some(Colorspace::Hdr),
        _ => None,
    }
}

/// Converts a wire attribution DTO into the catalog's `Attribution`.
fn attribution_from_dto(dto: AssetAttributionDto) -> Attribution {
    Attribution {
        license_id: dto.license_id,
        requires_attribution: dto.requires_attribution,
        license_url: dto.license_url,
        author: dto.author,
        source_url: dto.source_url,
        store_id: dto.store_id,
    }
}

/// Creation time (seconds since the Unix epoch) of the file backing a catalog entry, preferring
/// the filesystem birth time and falling back to the modified time; `0` if neither is available.
fn asset_created_at(root: &Path, rel_path: &str) -> i64 {
    let metadata = match std::fs::metadata(root.join(rel_path)) {
        Ok(metadata) => metadata,
        Err(_) => return 0,
    };
    metadata
        .created()
        .or_else(|_| metadata.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The wire DTO for one catalog entry.
fn asset_dto(root: &Path, entry: &AssetEntry) -> AssetEntryDto {
    let is_texture = entry.asset_type == AssetType::Texture;
    // Resolve the effective upload space (never surface `Auto`), matching the loader/sidecar.
    let colorspace = is_texture.then(|| {
        let cs = if entry.colorspace != Colorspace::Auto {
            entry.colorspace
        } else if entry.hdr {
            Colorspace::Hdr
        } else if entry.linear {
            Colorspace::Linear
        } else {
            Colorspace::Srgb
        };
        colorspace_name(cs).to_owned()
    });
    let role = (is_texture && entry.role != TextureRole::Unknown)
        .then(|| texture_role_name(entry.role).to_owned());
    AssetEntryDto {
        id: WireUuid(entry.id.value()),
        name: entry.name.clone(),
        r#type: asset_type_dto(entry.asset_type),
        path: entry.path.clone(),
        folder: optional_folder(&entry.folder),
        container: (entry.container.value() != 0).then(|| WireUuid(entry.container.value())),
        duration: (entry.asset_type == AssetType::Animation).then_some(entry.duration),
        rigged: entry.rigged.then_some(true),
        colorspace,
        role,
        created_at: asset_created_at(root, &entry.path),
        attribution: entry.attribution.as_ref().map(attribution_to_dto),
    }
}

/// Converts catalog `Attribution` into the wire DTO.
fn attribution_to_dto(attribution: &Attribution) -> AssetAttributionDto {
    AssetAttributionDto {
        license_id: attribution.license_id.clone(),
        requires_attribution: attribution.requires_attribution,
        license_url: attribution.license_url.clone(),
        author: attribution.author.clone(),
        source_url: attribution.source_url.clone(),
        store_id: attribution.store_id.clone(),
    }
}

/// The compact `{ id, name, folder? }` reply for a catalog entry.
fn asset_ref(entry: &AssetEntry) -> AssetRef {
    AssetRef {
        id: WireUuid(entry.id.value()),
        name: entry.name.clone(),
        folder: optional_folder(&entry.folder),
    }
}

/// Rewrites the durable `.smeta` sidecar for each id after a folder mutation touched many
/// rows. Best-effort: an IO failure is logged, not surfaced, so the command still succeeds.
fn write_asset_sidecars(assets: &AssetServer, ids: &[Uuid], label: &str) {
    for &id in ids {
        if let Err(err) = assets.write_asset_sidecar(id) {
            tracing::warn!("{label}: could not write .smeta for {}: {err}", id.value());
        }
    }
}

/// The full catalog as an [`AssetList`].
fn asset_list_dto(root: &Path, catalog: &saffron_scene::AssetCatalog) -> AssetList {
    AssetList {
        assets: catalog
            .entries
            .iter()
            .map(|entry| asset_dto(root, entry))
            .collect(),
        folders: catalog.folders.clone(),
    }
}

/// Whether a folder path is well-formed: non-empty, no leading/trailing `/`, no `\`, and no
/// empty `//` segment.
fn valid_folder_path(folder: &str) -> bool {
    if folder.is_empty()
        || folder.starts_with('/')
        || folder.ends_with('/')
        || folder.contains('\\')
        || folder.contains("//")
    {
        return false;
    }
    true
}

/// Whether the catalog already carries `folder`.
fn has_folder(catalog: &saffron_scene::AssetCatalog, folder: &str) -> bool {
    catalog.folders.iter().any(|existing| existing == folder)
}

/// Whether `candidate` is a strict descendant folder of `folder`.
fn is_folder_descendant(candidate: &str, folder: &str) -> bool {
    candidate.len() > folder.len()
        && candidate.starts_with(folder)
        && candidate.as_bytes()[folder.len()] == b'/'
}

/// Re-roots `value` from the `from` folder prefix onto `to`.
fn replace_folder_prefix(value: &str, from: &str, to: &str) -> String {
    if value == from {
        return to.to_owned();
    }
    if is_folder_descendant(value, from) {
        return format!("{to}{}", &value[from.len()..]);
    }
    value.to_owned()
}

/// The entity's `Name`, or empty.
fn entity_name(scene: &Scene, entity: Entity) -> String {
    scene
        .with_component::<Name, _>(entity, |n| n.name.clone())
        .unwrap_or_default()
}

/// The entity's `IdComponent` uuid as an optional wire uuid.
fn entity_id(scene: &Scene, entity: Entity) -> Option<WireUuid> {
    let id = entity_uuid(scene, entity);
    (id != 0).then_some(WireUuid(id))
}

/// One `(entity, slot)` reference, collected during a scene scan and resolved to a usage DTO
/// after — so the scan's `&mut` scene borrow does not overlap the per-entity name/id reads.
type Reference = (Entity, &'static str);

/// Collects every `(entity, slot)` reference to `asset` in the scene (the scan half of
/// [`collect_asset_usages`] / [`clear_asset_usages`]): mesh slots + material albedo /
/// metallic-roughness slots, and the vegetation-map field. The environment sky-texture hit is
/// the boolean second tuple.
fn scan_asset_references(scene: &mut Scene, asset: Uuid) -> (Vec<Reference>, bool) {
    let mut refs = Vec::new();
    scene.for_each::<(&Mesh,), _>(|entity, (mesh,)| {
        if mesh.mesh.value() == asset.value() {
            refs.push((entity, "mesh"));
        }
    });
    scene.for_each::<(&MaterialSet,), _>(|entity, (set,)| {
        if set
            .slots
            .iter()
            .any(|s| s.material.value() == asset.value())
        {
            refs.push((entity, "material"));
        }
    });
    scene.for_each::<(&VegetationField,), _>(|entity, (field,)| {
        if field.map.value() == asset.value() {
            refs.push((entity, "vegetationField.map"));
        }
    });
    let sky = scene.environment.sky_texture.value() == asset.value();
    (refs, sky)
}

/// One collected `(entity, slot)` reference as a usage DTO (the name/id read after the scan).
fn usage_dto(scene: &Scene, entity: Entity, slot: &str) -> AssetUsageDto {
    AssetUsageDto {
        entity: entity_id(scene, entity),
        entity_name: Some(entity_name(scene, entity)),
        slot: slot.to_owned(),
    }
}

/// Every place `asset` is referenced in the active scene: `Mesh` slots, `MaterialSet` slot
/// material references, and the environment sky texture.
fn collect_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
    let (refs, sky) = scan_asset_references(scene, asset);
    let mut usages: Vec<AssetUsageDto> = refs
        .iter()
        .map(|&(entity, slot)| usage_dto(scene, entity, slot))
        .collect();
    if sky {
        usages.push(AssetUsageDto {
            entity: None,
            entity_name: None,
            slot: "environment.skyTexture".to_owned(),
        });
    }
    usages
}

/// Clears every reference to `asset` in the scene and returns the cleared usages (the
/// `delete-asset` cascade). The DTOs are built (name/id read) before the slot is zeroed.
fn clear_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
    let (refs, sky) = scan_asset_references(scene, asset);
    let mut cleared: Vec<AssetUsageDto> = refs
        .iter()
        .map(|&(entity, slot)| usage_dto(scene, entity, slot))
        .collect();
    for &(entity, slot) in &refs {
        match slot {
            "mesh" => {
                let _ = scene.with_component_mut::<Mesh, _>(entity, |m| m.mesh = Uuid(0));
            }
            "vegetationField.map" => {
                let _ = scene.with_component_mut::<VegetationField, _>(entity, |field| {
                    field.map = Uuid(0);
                    field.enabled = false;
                });
            }
            // "material": clear every slot that referenced the deleted material to the
            // built-in default.
            _ => {
                let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
                    for s in &mut set.slots {
                        if s.material.value() == asset.value() {
                            s.material = Uuid(0);
                        }
                    }
                });
            }
        }
    }
    if sky {
        cleared.push(AssetUsageDto {
            entity: None,
            entity_name: None,
            slot: "environment.skyTexture".to_owned(),
        });
        scene.environment.sky_texture = Uuid(0);
    }
    cleared
}

/// The current project's identity, read from the editor.
fn current_project_info(ctx: &EngineContext<'_>) -> ProjectInfo {
    ProjectInfo {
        loaded: ctx.scene_edit.project_ready(),
        root: ctx.scene_edit.project_root.clone(),
        path: ctx.scene_edit.project_path.clone(),
        name: ctx.scene_edit.project_name.clone(),
        display_name: ctx.scene_edit.project_display_name.clone(),
    }
}

/// Writes a [`ProjectInfo`] back onto the editor, also resetting the scene path.
fn apply_project_info(ctx: &mut EngineContext<'_>, project: &ProjectInfo) {
    ctx.scene_edit.project_phase = if project.loaded {
        ProjectPhase::Ready
    } else {
        ProjectPhase::Unloaded
    };
    ctx.scene_edit.project_root = project.root.clone();
    ctx.scene_edit.project_path = project.path.clone();
    ctx.scene_edit.project_name = project.name.clone();
    ctx.scene_edit.project_display_name = project.display_name.clone();
    ctx.scene_edit.scene_path = project.path.clone();
}

/// Brings the host's project up from the editor-set environment at startup by seeding the loader
/// inbox — the same non-blocking path the lifecycle commands use, so there is one project
/// bring-up code path: `SAFFRON_PROJECT` selects a project to open (or create when the name is
/// valid and unborn), else `SAFFRON_SCRATCH_PROJECT` makes a deterministic per-shell scratch
/// project, else a `project.json` in the working directory is opened. With none of those set the
/// host waits for the editor's project picker (phase stays `Unloaded`).
pub fn bootstrap_project_from_env(scene_edit: &mut SceneEditContext) {
    let request = if let Some(selected) = std::env::var_os("SAFFRON_PROJECT")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
    {
        let create_new =
            valid_project_name(&selected) && !saffron_assets::project_json_path(&selected).exists();
        if create_new {
            ProjectLoadRequest::New(NewProjectSpec {
                name: selected,
                display_name: String::new(),
                root: String::new(),
            })
        } else {
            ProjectLoadRequest::Open(selected)
        }
    } else if std::env::var_os("SAFFRON_SCRATCH_PROJECT").is_some() {
        ProjectLoadRequest::New(NewProjectSpec {
            name: saffron_assets::scratch_project_name(),
            display_name: "Scratch Project".to_owned(),
            root: String::new(),
        })
    } else if Path::new("project.json").exists() {
        ProjectLoadRequest::Open("project.json".to_owned())
    } else {
        return; // Nothing to bring up — wait for the editor's project picker.
    };

    scene_edit.project_load_inbox = Some(request);
    scene_edit.project_phase = ProjectPhase::Loading;
}

/// The wire DTO for a [`ProjectInfo`].
fn project_dto(project: &ProjectInfo) -> ProjectInfoDto {
    ProjectInfoDto {
        loaded: project.loaded,
        root: project.root.clone(),
        path: project.path.clone(),
        name: project.name.clone(),
        display_name: project.display_name.clone(),
    }
}

/// The wire DTO for the live project-load phase + progress.
fn project_status_dto(ctx: &EngineContext<'_>) -> ProjectStatusDto {
    let sc = &ctx.scene_edit;
    let p = &sc.project_load;
    ProjectStatusDto {
        phase: match sc.project_phase {
            ProjectPhase::Unloaded => ProjectPhaseDto::Unloaded,
            ProjectPhase::Loading => ProjectPhaseDto::Loading,
            ProjectPhase::Ready => ProjectPhaseDto::Ready,
            ProjectPhase::Failed => ProjectPhaseDto::Failed,
        },
        stage: boot_stage_dto(p.stage),
        done: i32::try_from(p.done).unwrap_or(i32::MAX),
        total: i32::try_from(p.total).unwrap_or(i32::MAX),
        label: p.label.clone(),
        current_item: p.current_item.clone(),
        error: p.error.clone(),
        version: i64::try_from(p.version).unwrap_or(i64::MAX),
        name: sc.project_name.clone(),
        path: sc.project_path.clone(),
    }
}

/// Maps a [`BootStage`] onto its wire enum.
fn boot_stage_dto(stage: BootStage) -> BootStageDto {
    match stage {
        BootStage::Manifest => BootStageDto::Manifest,
        BootStage::Catalog => BootStageDto::Catalog,
        BootStage::Scene => BootStageDto::Scene,
        BootStage::Install => BootStageDto::Install,
        BootStage::Assets => BootStageDto::Assets,
        BootStage::Skybox => BootStageDto::Skybox,
        BootStage::Accel => BootStageDto::Accel,
        BootStage::Ready => BootStageDto::Ready,
        BootStage::Failed => BootStageDto::Failed,
    }
}

/// The "no project loaded" guard.
fn require_project_loaded(ctx: &EngineContext<'_>) -> Result<()> {
    if ctx.scene_edit.project_ready() {
        Ok(())
    } else {
        Err(Error::command("no project loaded"))
    }
}

/// The [`ProjectHost`] adapter over the renderer seam: the project lifecycle commands hand
/// it to [`AssetServer::create_project`][saffron_assets::AssetServer::create_project] /
/// `load_project` / `save_project`, which need the
/// GPU-idle + render-settings serde the renderer owns. Wraps `&mut dyn ControlRenderer`,
/// so it borrows only the renderer field of the [`EngineContext`] — disjoint from `assets`
/// and `scene_edit`.
pub(crate) struct RendererProjectHost<'a> {
    pub(crate) renderer: &'a mut dyn ControlRenderer,
}

impl ProjectHost for RendererProjectHost<'_> {
    fn wait_gpu_idle(&mut self) {
        self.renderer.wait_gpu_idle();
    }

    fn render_settings_to_json(&self) -> Value {
        self.renderer.render_settings_to_json()
    }

    fn apply_render_settings(&mut self, settings: &Value) {
        self.renderer.apply_render_settings(settings);
    }
}

/// Cooks the loaded project into a standalone app folder at `params.output_dir`: pre-bakes every
/// material's mesh SPIR-V (so the shipped player never needs `slangc`), then stages the player
/// binary + the project data (`project.json`, `assets/`, `src/`) + the engine `shaders/` + an
/// `app.json` manifest. macOS uses a native `.app` bundle with its Vulkan runtime in
/// `Contents/Frameworks`; other platforms use a flat directory. Side-effecting (writes to disk);
/// returns the staged path and any non-fatal warnings.
fn export_app(ctx: &mut EngineContext<'_>, params: &ExportAppParams) -> Result<ExportAppResult> {
    if params.output_dir.trim().is_empty() {
        return Err(Error::command("missing 'outputDir'"));
    }
    let project_root = ctx.scene_edit.project_root.clone();
    if project_root.is_empty() {
        return Err(Error::command("no project loaded to export"));
    }
    let project_root = PathBuf::from(&project_root);
    let mut warnings: Vec<String> = Vec::new();

    // 1. Pre-bake every material's mesh shader into the project assets (the player loads only the
    //    baked `.spv`; it never invokes `slangc`). Mirrors the `material-cook` command's loop.
    let material_ids: Vec<Uuid> = ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|e| e.asset_type == AssetType::Material)
        .map(|e| e.id)
        .collect();
    for id in material_ids {
        let Ok(raw) = load_catalog_material_asset_raw(ctx.assets, id) else {
            continue;
        };
        if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
            continue; // a factor-only material has no node graph to bake.
        }
        let mut probe = raw.clone();
        if lower_graph_to_params(&raw.graph, &mut probe) {
            continue; // the graph lowers to plain params — no codegen shader needed.
        }
        if let Err(err) = ctx.assets.compile_material_mesh_shader(&raw.graph, id) {
            warnings.push(format!("material {id}: shader bake failed: {err}"));
        }
    }

    // 2. Stage the platform-native application layout. The player binary + engine shaders sit
    //    beside the running host binary in the build tree.
    let layout = ExportLayout::for_output(Path::new(&params.output_dir));
    std::fs::create_dir_all(&layout.resources).map_err(|e| {
        Error::command(format!(
            "create output dir '{}': {e}",
            layout.root.display()
        ))
    })?;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| Error::command("cannot resolve the engine binary directory"))?;

    copy_file(
        &project_root.join("project.json"),
        &layout.resources.join("project.json"),
    )
    .map_err(|e| Error::command(format!("copy project.json: {e}")))?;
    // The authored vegetation sources stay behind: a runtime binds a cooked generation from the
    // artifact store, and the project loader drops a catalog row whose file is absent.
    copy_dir_filtered(
        &project_root.join("assets"),
        &layout.resources.join("assets"),
        &|path| !is_authored_vegetation(path),
    )
    .map_err(|e| Error::command(format!("copy assets/: {e}")))?;
    let src = project_root.join("src");
    if src.is_dir() {
        copy_dir_recursive(&src, &layout.resources.join("src"))
            .map_err(|e| Error::command(format!("copy src/: {e}")))?;
    }
    let shaders = exe_dir.join("shaders");
    if shaders.is_dir() {
        copy_dir_recursive(&shaders, &layout.resources.join("shaders"))
            .map_err(|e| Error::command(format!("copy shaders/: {e}")))?;
    } else {
        warnings.push(format!("engine shaders not found at {}", shaders.display()));
    }
    let player = exe_dir.join("saffron-player");
    if player.is_file() {
        copy_file(&player, &layout.executable)
            .map_err(|e| Error::command(format!("copy saffron-player: {e}")))?;
    } else {
        warnings.push(format!(
            "saffron-player binary not found at {} (build it before export)",
            player.display()
        ));
    }
    // The cooked vegetation closure. The store lives beside `assets/` rather than inside it, so the
    // asset copy above never carries it — and without it a player binds no manifest and the world
    // comes up bare.
    let maps: Vec<saffron_core::Uuid> = ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.asset_type == AssetType::VegetationMap)
        .map(|entry| entry.id)
        .collect();
    let declared_maps = maps.len();
    let store = ctx.assets.vegetation_artifact_store();
    let closure = saffron_assets::vegetation_export_closure(&store, maps)
        .map_err(|error| Error::command(format!("vegetation export closure: {error}")))?;
    let store_root = store.root().to_path_buf();
    let packaged_root = layout.resources.join("cache").join("vegetation");
    for file in &closure.files {
        copy_file(
            &store_root.join(&file.relative),
            &packaged_root.join(&file.relative),
        )
        .map_err(|error| Error::command(format!("copy {}: {error}", file.relative.display())))?;
    }
    for map in &closure.maps {
        if map.missing > 0 {
            warnings.push(format!(
                "vegetation map {} names {} artifact(s) the store does not hold; recook before shipping",
                map.map.value(),
                map.missing
            ));
        }
    }
    // A project with no vegetation map ships no vegetation and that is not a warning; a project that
    // HAS one and never cooked it is.
    if declared_maps > 0 && closure.maps.is_empty() {
        warnings.push(format!(
            "{declared_maps} vegetation map(s) have no cooked generation; the package ships no vegetation"
        ));
    }

    // License attribution: every packaged plant source that says it requires attribution, written
    // where a shipped build can show it. A licence obligation that lives only in the editor is an
    // obligation the shipped product breaks.
    let mut attributions: Vec<String> = Vec::new();
    for entry in ctx
        .assets
        .catalog()
        .entries
        .iter()
        .filter(|entry| entry.asset_type == AssetType::Plant)
        .map(|entry| entry.id)
        .collect::<Vec<_>>()
    {
        let Ok(plant) = load_plant_family_asset(ctx.assets, entry) else {
            continue;
        };
        let saffron_vegetation::PlantFamilySource::Imported(recipe) = &plant.source else {
            continue;
        };
        for source in &recipe.sources {
            let provenance = &source.provenance;
            if !provenance.requires_attribution {
                continue;
            }
            let line = format!(
                "{} — {} ({}) — {}",
                plant.name, provenance.attribution, provenance.license_id, provenance.source_uri
            );
            if !attributions.contains(&line) {
                attributions.push(line);
            }
        }
    }
    attributions.sort();
    if !attributions.is_empty() {
        let text = format!("{}\n", attributions.join("\n"));
        std::fs::write(layout.resources.join("ATTRIBUTION.txt"), text)
            .map_err(|error| Error::command(format!("write ATTRIBUTION.txt: {error}")))?;
    }

    let app_json = serde_json::to_string_pretty(&params.app)
        .map_err(|e| Error::command(format!("serialize app.json: {e}")))?;
    std::fs::write(layout.resources.join("app.json"), app_json)
        .map_err(|e| Error::command(format!("write app.json: {e}")))?;
    stage_platform_runtime(&layout, &params.app, &mut warnings)?;

    Ok(ExportAppResult {
        path: layout.root.to_string_lossy().into_owned(),
        warnings,
        vegetation: closure
            .maps
            .iter()
            .map(|map| saffron_protocol::ExportVegetationMapDto {
                map: WireUuid(map.map.value()),
                manifest_identity: map.manifest_identity.clone(),
                plants: map.plants.to_string(),
                cells: map.cells.to_string(),
                missing: map.missing.to_string(),
                baseline: map.baseline,
                macro_plants: map.macro_plants.to_string(),
                facets: map
                    .facet_bytes
                    .iter()
                    .map(|facet| saffron_protocol::ExportVegetationFacetDto {
                        facet: format!("{:?}", facet.kind),
                        cells: facet.cells.to_string(),
                        bytes: facet.bytes.to_string(),
                    })
                    .collect(),
            })
            .collect(),
        vegetation_bytes: closure.total_bytes.to_string(),
        attributions: attributions.len() as u32,
    })
}

/// The platform-native output paths for one exported application.
struct ExportLayout {
    root: PathBuf,
    resources: PathBuf,
    executable: PathBuf,
}

impl ExportLayout {
    fn for_output(output: &Path) -> Self {
        #[cfg(target_os = "macos")]
        {
            let root = if output.extension().is_some_and(|ext| ext == "app") {
                output.to_path_buf()
            } else {
                PathBuf::from(format!("{}.app", output.display()))
            };
            let contents = root.join("Contents");
            Self {
                resources: contents.join("Resources"),
                executable: contents.join("MacOS").join("saffron-player"),
                root,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self {
            root: output.to_path_buf(),
            resources: output.to_path_buf(),
            executable: output.join("saffron-player"),
        }
    }
}

/// Stages the Linux C++ runtime beside the player, where its `$ORIGIN` rpath resolves it.
#[cfg(not(target_os = "macos"))]
fn stage_platform_runtime(
    layout: &ExportLayout,
    _app: &AppManifest,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for lib in ["libc++.so.1", "libc++abi.so.1"] {
        match find_runtime_lib(lib) {
            Some(src) => copy_file(&src, &layout.root.join(lib))
                .map_err(|e| Error::command(format!("copy {lib}: {e}")))?,
            None => warnings.push(format!(
                "{lib} not found on the host; the exported app needs it beside saffron-player"
            )),
        }
    }
    Ok(())
}

/// Stages a self-contained macOS application bundle with MoltenVK, metadata, its license, and
/// ad-hoc signatures. The player loads the bundled MoltenVK dynamic library directly.
#[cfg(target_os = "macos")]
fn stage_platform_runtime(
    layout: &ExportLayout,
    app: &AppManifest,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let contents = layout.root.join("Contents");
    let frameworks = contents.join("Frameworks");
    let moltenvk = find_macos_runtime_lib("libMoltenVK.dylib")
        .ok_or_else(|| Error::command("macOS Vulkan driver libMoltenVK.dylib not found"))?;
    let bundled_moltenvk = frameworks.join("libMoltenVK.dylib");
    copy_file(&moltenvk, &bundled_moltenvk)
        .map_err(|e| Error::command(format!("copy libMoltenVK.dylib: {e}")))?;

    std::fs::write(contents.join("Info.plist"), macos_info_plist(app))
        .map_err(|e| Error::command(format!("write Info.plist: {e}")))?;
    stage_macos_runtime_licenses(&layout.resources, warnings)?;

    for code in [&bundled_moltenvk, &layout.executable] {
        ad_hoc_sign(code)?;
    }
    ad_hoc_sign(&layout.root)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn find_macos_runtime_lib(name: &str) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(sdk) = std::env::var_os("VULKAN_SDK") {
        dirs.push(PathBuf::from(sdk).join("lib"));
    }
    dirs.extend(
        ["/opt/homebrew/lib", "/usr/local/lib"]
            .into_iter()
            .map(PathBuf::from),
    );
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(target_os = "macos")]
fn macos_info_plist(app: &AppManifest) -> String {
    let title = xml_escape(&app.title);
    let identifier = bundle_identifier_component(&app.title);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>{title}</string>
  <key>CFBundleExecutable</key><string>saffron-player</string>
  <key>CFBundleIdentifier</key><string>com.saffron.anima.{identifier}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>{title}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{}</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn bundle_identifier_component(title: &str) -> String {
    let mut component = String::new();
    let mut separator = false;
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            component.push(ch);
            separator = false;
        } else if !component.is_empty() && !separator {
            component.push('-');
            separator = true;
        }
    }
    while component.ends_with('-') {
        component.pop();
    }
    if component.is_empty() {
        "app".to_owned()
    } else {
        component
    }
}

#[cfg(target_os = "macos")]
fn stage_macos_runtime_licenses(resources: &Path, warnings: &mut Vec<String>) -> Result<()> {
    let licenses = resources.join("licenses");
    for (name, candidates) in [(
        "MoltenVK-LICENSE.txt",
        [
            "/opt/homebrew/opt/molten-vk/LICENSE",
            "/usr/local/opt/molten-vk/LICENSE",
        ],
    )] {
        if let Some(source) = candidates
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        {
            copy_file(&source, &licenses.join(name))
                .map_err(|e| Error::command(format!("copy {name}: {e}")))?;
        } else {
            warnings.push(format!("license file for {name} not found"));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ad_hoc_sign(path: &Path) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--timestamp=none"])
        .arg(path)
        .output()
        .map_err(|e| Error::command(format!("run codesign for '{}': {e}", path.display())))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::command(format!(
            "codesign '{}': {}",
            path.display(),
            stderr.trim()
        )))
    }
}

/// Resolves a shared library by SONAME from the usual Linux library directories (honoring a
/// `LD_LIBRARY_PATH` override first), returning the first match — for bundling the C++ runtime
/// into a standalone export.
#[cfg(not(target_os = "macos"))]
fn find_runtime_lib(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("LD_LIBRARY_PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    dirs.extend(["/usr/lib64", "/usr/lib", "/lib64", "/lib"].map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|p| p.exists())
}

/// Copies one file, creating the destination's parent directory first.
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Recursively copies a directory tree (files + subdirectories) into `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir_filtered(src, dst, &|_| true)
}

/// Authored vegetation sources a runtime never reads: it binds a cooked generation from the artifact
/// store and streams the cells that generation names.
const AUTHORED_VEGETATION: [&str; 3] = ["splant", "sbiome", "svegmap"];

/// Whether one path is an authored vegetation source or its sidecar package.
///
/// Excluding these from an exported package is safe because the project loader treats the filesystem
/// as the source of truth and drops a catalog row whose file is absent, and because vegetation binds
/// by identity through the artifact store rather than through the catalog.
fn is_authored_vegetation(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    AUTHORED_VEGETATION.iter().any(|extension| {
        name.ends_with(&format!(".{extension}")) || name.ends_with(&format!(".{extension}.data"))
    })
}

/// Copies a tree, skipping whatever the filter rejects.
fn copy_dir_filtered(src: &Path, dst: &Path, keep: &dyn Fn(&Path) -> bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        if !keep(&from) {
            continue;
        }
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_filtered(&from, &to, keep)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Resolves `{asset, size?}` to a base64-PNG thumbnail reply. [`request_thumbnail`] classifies the
/// asset and either returns a cache hit or enqueues a main-graph render (reply `pending`); the host
/// drains the queue in `on_update`. Shared by `get-thumbnail` (128) + `view-asset` (512).
fn thumbnail_result(
    ctx: &mut EngineContext<'_>,
    params: &ThumbnailParams,
    default_size: u32,
) -> Result<ThumbnailResult> {
    let id = resolve_asset(ctx, &params.asset)?;
    let size = u32::try_from(params.size.unwrap_or(default_size as i32)).unwrap_or(default_size);
    let reply =
        request_thumbnail(&mut *ctx.assets, id, size).map_err(|e| Error::command(e.to_string()))?;
    if reply.pending {
        return Ok(ThumbnailResult {
            id: WireUuid(id.value()),
            format: ThumbnailFormatDto::Png,
            width: 0,
            height: 0,
            base64: String::new(),
            pending: true,
        });
    }
    Ok(ThumbnailResult {
        id: WireUuid(id.value()),
        format: ThumbnailFormatDto::Png,
        width: i32::try_from(reply.width).unwrap_or(0),
        height: i32::try_from(reply.height).unwrap_or(0),
        base64: base64_encode(&reply.png),
        pending: false,
    })
}

/// Drops the asset preview and restores the authored edit state. A no-op when no preview
/// is alive.
fn leave_asset_preview(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() {
        return;
    }
    let was_active = ctx.preview_active_view;
    ctx.preview_scene = None;
    ctx.preview_asset = Uuid(0);
    ctx.preview_root_entity = Entity::NULL;
    ctx.preview_floor_entity = Entity::NULL;
    ctx.preview_bone_by_node.clear();
    ctx.preview_active_view = false;
    if was_active {
        ctx.camera = ctx.saved_camera;
        ctx.skeleton_overlay = ctx.saved_overlay;
        let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection)
        {
            ctx.saved_selection
        } else {
            Entity::NULL
        };
        ctx.saved_selection = Entity::NULL;
        ctx.set_selection(restore);
    }
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Parks the preview orbit and restores the authored fly-cam/overlay/selection so the scene
/// view shows the authored scene. A no-op unless the preview is the active view.
fn deactivate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || !ctx.preview_active_view {
        return;
    }
    ctx.parked_preview_camera = ctx.camera;
    ctx.camera = ctx.saved_camera;
    ctx.skeleton_overlay = ctx.saved_overlay;
    let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection) {
        ctx.saved_selection
    } else {
        Entity::NULL
    };
    ctx.set_selection(restore);
    ctx.preview_active_view = false;
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Re-stashes the authored view and restores the parked preview orbit + overlay + selected
/// root. A no-op unless a preview scene is alive but not currently active.
fn activate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || ctx.preview_active_view {
        return;
    }
    ctx.saved_camera = ctx.camera;
    ctx.saved_selection = ctx.selected;
    ctx.saved_overlay = ctx.skeleton_overlay;
    ctx.camera = ctx.parked_preview_camera;
    ctx.skeleton_overlay.show = true;
    ctx.skeleton_overlay.highlight_joint = -1;
    ctx.preview_active_view = true;
    let root = ctx.preview_root_entity;
    ctx.set_selection(root);
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// The flat parent-indexed bone tree for a skinned container's nodes (the
/// `get-asset-model` rig walk): the joints plus their ancestor chains bounded at the
/// skeleton root, with node indices preserved.
fn build_bone_tree(meta: &ContainerMetadata) -> Vec<BoneDto> {
    let node_count = meta.nodes.as_array().map_or(0, Vec::len);
    let nodes = meta.nodes.as_array();
    let parents: Vec<i32> = (0..node_count)
        .map(|i| {
            nodes
                .and_then(|n| n[i].get("parent"))
                .and_then(Value::as_i64)
                .map_or(-1, |v| v as i32)
        })
        .collect();
    let mut is_joint = vec![false; node_count];
    let skeleton_root = meta
        .skin
        .get("skeletonRoot")
        .and_then(Value::as_i64)
        .map_or(-1, |v| v as i32);
    if let Some(joints) = meta.skin.get("joints").and_then(Value::as_array) {
        for joint in joints {
            if let Some(index) = joint.as_i64()
                && index >= 0
                && (index as usize) < node_count
            {
                is_joint[index as usize] = true;
            }
        }
    }
    let mut in_rig = vec![false; node_count];
    if skeleton_root >= 0 && (skeleton_root as usize) < node_count {
        in_rig[skeleton_root as usize] = true;
    }
    for start in is_joint
        .iter()
        .enumerate()
        .filter_map(|(i, &joint)| joint.then_some(i as i32))
    {
        let mut node = start;
        while node >= 0 && (node as usize) < node_count && !in_rig[node as usize] {
            in_rig[node as usize] = true;
            if node == skeleton_root {
                break;
            }
            node = parents[node as usize];
        }
    }
    let mut bones = Vec::new();
    for i in 0..node_count {
        if !in_rig[i] {
            continue;
        }
        let parent = parents[i];
        let parent = if parent >= 0 && (parent as usize) < node_count && in_rig[parent as usize] {
            parent
        } else {
            -1
        };
        bones.push(BoneDto {
            index: i as i32,
            name: nodes
                .and_then(|n| n[i].get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent,
            joint: is_joint[i],
        });
    }
    bones
}

/// The animation sub-asset clips a container carries (the `get-asset-model` clip walk), each
/// with its real per-channel keyframe data loaded from the clip. No live forest here, so
/// channel labels are the raw glTF target names.
fn container_clips(
    assets: &mut saffron_assets::AssetServer,
    meta: &ContainerMetadata,
) -> Vec<AnimationClipDto> {
    let subs: Vec<(u64, String, f32)> = meta
        .sub_assets
        .iter()
        .filter(|sub| sub.asset_type == AssetType::Animation)
        .map(|sub| (sub.sub_id.value(), sub.name.clone(), sub.duration))
        .collect();
    subs.into_iter()
        .map(|(sub_id, name, duration)| {
            let channels = assets
                .load_anim_clip(saffron_core::Uuid(sub_id))
                .map(|clip| {
                    crate::commands_animation::channels_of(&clip, |track| track.target_name.clone())
                })
                .unwrap_or_default();
            AnimationClipDto {
                id: WireUuid(sub_id),
                name,
                duration,
                channels,
            }
        })
        .collect()
}

/// Grows one native family's graph and reports what it produced.
/// The growth budget a preview request names, or the cook budget when it names none.
fn preview_budget(axes: Option<u32>, elements: Option<u32>) -> saffron_vegetation::BotanicalBudget {
    saffron_vegetation::BotanicalBudget {
        axes: axes.map(|value| value as usize),
        elements: elements.map(|value| value as usize),
        cancellation: None,
    }
}

/// One authored appearance on the wire.
fn phenotype_dto(
    phenotype: &saffron_vegetation::PlantPhenotype,
) -> saffron_protocol::PlantPhenotypeDto {
    use saffron_vegetation::PhenotypeRole;
    saffron_protocol::PlantPhenotypeDto {
        id: phenotype.id,
        role: match phenotype.role {
            PhenotypeRole::Healthy => saffron_protocol::PhenotypeRoleDto::Healthy,
            PhenotypeRole::Harvested => saffron_protocol::PhenotypeRoleDto::Harvested,
            PhenotypeRole::Damaged => saffron_protocol::PhenotypeRoleDto::Damaged,
            PhenotypeRole::Burned => saffron_protocol::PhenotypeRoleDto::Burned,
            PhenotypeRole::Dead => saffron_protocol::PhenotypeRoleDto::Dead,
            PhenotypeRole::Flowering => saffron_protocol::PhenotypeRoleDto::Flowering,
            PhenotypeRole::Fruiting => saffron_protocol::PhenotypeRoleDto::Fruiting,
            PhenotypeRole::Senescent => saffron_protocol::PhenotypeRoleDto::Senescent,
            PhenotypeRole::Wet => saffron_protocol::PhenotypeRoleDto::Wet,
        },
        variation: phenotype.variation,
        season_window: phenotype
            .season_window
            .map(|(start, end)| [u32::from(start), u32::from(end)]),
        material_remap: phenotype
            .material_remap
            .iter()
            .map(|(from, to)| [*from, *to])
            .collect(),
        // Part identities are u128 and stay strings end to end; narrowing one through a JSON
        // number would corrupt it silently.
        active_parts: phenotype
            .active_parts
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

fn phenotype_from_dto(
    dto: &saffron_protocol::PlantPhenotypeDto,
) -> Result<saffron_vegetation::PlantPhenotype> {
    use saffron_vegetation::PhenotypeRole;
    let season_window = dto
        .season_window
        .map(|[start, end]| {
            let narrow = |value: u32| {
                u16::try_from(value)
                    .ok()
                    .filter(|value| *value < 1000)
                    .ok_or_else(|| {
                        Error::command("seasonWindow is per-mille of the year (0..1000)")
                    })
            };
            Result::Ok((narrow(start)?, narrow(end)?))
        })
        .transpose()?;
    Ok(saffron_vegetation::PlantPhenotype {
        id: dto.id,
        role: match dto.role {
            saffron_protocol::PhenotypeRoleDto::Healthy => PhenotypeRole::Healthy,
            saffron_protocol::PhenotypeRoleDto::Harvested => PhenotypeRole::Harvested,
            saffron_protocol::PhenotypeRoleDto::Damaged => PhenotypeRole::Damaged,
            saffron_protocol::PhenotypeRoleDto::Burned => PhenotypeRole::Burned,
            saffron_protocol::PhenotypeRoleDto::Dead => PhenotypeRole::Dead,
            saffron_protocol::PhenotypeRoleDto::Flowering => PhenotypeRole::Flowering,
            saffron_protocol::PhenotypeRoleDto::Fruiting => PhenotypeRole::Fruiting,
            saffron_protocol::PhenotypeRoleDto::Senescent => PhenotypeRole::Senescent,
            saffron_protocol::PhenotypeRoleDto::Wet => PhenotypeRole::Wet,
        },
        season_window,
        variation: dto.variation,
        material_remap: dto
            .material_remap
            .iter()
            .map(|[from, to]| (*from, *to))
            .collect(),
        active_parts: dto
            .active_parts
            .iter()
            .map(|part| {
                part.parse::<u128>()
                    .map_err(|_| Error::command("activeParts entries are decimal part identities"))
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn plant_growth_dto(
    assets: &saffron_assets::AssetServer,
    family: &saffron_vegetation::PlantFamilyAsset,
    variation: u32,
    budget: &saffron_vegetation::BotanicalBudget,
) -> Result<saffron_protocol::BotanicalGrowthDto> {
    let saffron_vegetation::PlantFamilySource::Native { graph, .. } = &family.source else {
        return Err(Error::command(
            "plant family has an imported source, not a botanical graph",
        ));
    };
    let individual = graph
        .variations
        .get(variation as usize)
        .ok_or_else(|| Error::command(format!("plant family declares no variation {variation}")))?;
    let modules = saffron_assets::PlantModules::for_family(assets, family);
    let growth = saffron_vegetation::grow(graph, variation as usize, &modules, budget)
        .map_err(|error| Error::command(error.to_string()))?;
    let assembly = &growth.assembly;
    let geometry = saffron_vegetation::normalize_botanical_geometry(
        saffron_vegetation::native_plant_source_id(family.id),
        assembly,
        &std::collections::BTreeMap::new(),
    )
    .map_err(|error| Error::command(error.to_string()))?;
    let structure = saffron_vegetation::derive_family_structure(assembly)
        .map_err(|error| Error::command(error.to_string()))?;
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    Ok(saffron_protocol::BotanicalGrowthDto {
        truncated: growth.truncated,
        graph: graph.identity().to_string(),
        variation,
        seed: individual.seed.to_string(),
        age: individual.age.bits(),
        variations: count(graph.variations.len()),
        axes: count(assembly.axes.len()),
        frames: count(assembly.frames.len()),
        shells: count(assembly.shells.len()),
        elements: count(assembly.elements.len()),
        vertices: count(geometry.meshes.iter().map(|mesh| mesh.vertices.len()).sum()),
        triangles: count(
            geometry
                .meshes
                .iter()
                .map(|mesh| mesh.indices.len() / 3)
                .sum(),
        ),
        parts: count(structure.parts.len()),
        spines: count(structure.spines.len()),
        height_bits: structure.dimensions.height.bits(),
        grafts: count(assembly.grafts.len()),
        applied_edits: growth.diagnostics.applied,
        orphans: growth
            .diagnostics
            .orphans
            .iter()
            .map(crate::botanical_dto::orphan_dto)
            .collect(),
    })
}

/// Registers plant compiler commands at the vegetation-domain tail of the central table.
/// Registers the point-interchange commands: one door in from a content-creation tool, one out.
pub fn register_interchange_commands(reg: &mut CommandRegistry) {
    reg.register::<saffron_protocol::VegetationImportPointsParams, saffron_protocol::VegetationImportPointsResult>(
        "vegetation-import-points",
        "import instanced points from a content-creation tool into an authored map layer",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = resolve_asset(ctx, &params.map)?;
            let layer = u128::from_str_radix(&params.layer.0, 16)
                .map_err(|_| Error::command("layer must be 32 hex digits"))?;
            let expected_generation = params
                .expected_generation
                .parse::<u64>()
                .map_err(|_| Error::command("expectedGeneration is not a u64"))?;
            let families: std::collections::BTreeMap<String, saffron_core::Uuid> = params
                .prototypes
                .iter()
                .map(|prototype| {
                    (
                        prototype.name.clone(),
                        saffron_core::Uuid(prototype.family.0),
                    )
                })
                .collect();
            // The extension picks the reader: a Houdini point cloud and a glTF instancing node say
            // the same thing in different files, and both land in the one interchange vocabulary.
            let extension = std::path::Path::new(&params.path)
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            let payload = match extension.as_str() {
                "geo" | "json" => {
                    let document: saffron_json::Value = saffron_json::parse_json(
                        &std::fs::read_to_string(&params.path)
                            .map_err(|error| Error::command(error.to_string()))?,
                    )
                    .map_err(|error| Error::command(error.to_string()))?;
                    saffron_vegetation::read_houdini_points(&document, &families)
                        .map_err(|error| Error::command(error.to_string()))?
                }
                "usda" | "usd" => {
                    let text = std::fs::read_to_string(&params.path)
                        .map_err(|error| Error::command(error.to_string()))?;
                    saffron_vegetation::read_usd_point_instancers(&text, &families)
                        .map_err(|error| Error::command(error.to_string()))?
                }
                "gltf" | "glb" => {
                    let instancing = saffron_geometry::read_gltf_instancing(&params.path)
                        .map_err(|error| Error::command(error.to_string()))?;
                    saffron_vegetation::gltf_instancing_to_interchange(&instancing, &families)
                        .map_err(|error| Error::command(error.to_string()))?
                }
                other => {
                    return Err(Error::command(format!(
                        "no point reader for '{other}'; expected geo, usda, gltf, or glb"
                    )));
                }
            };
            // Bounds come from each family's own dimensions, so every named family has to be a real
            // plant asset rather than a name the caller invented.
            let mut dimensions = std::collections::BTreeMap::new();
            for prototype in &payload.prototypes {
                let plant = load_plant_family_asset(ctx.assets, prototype.family)
                    .map_err(|error| Error::command(error.to_string()))?;
                dimensions.insert(prototype.family.value(), plant.dimensions);
            }
            let anchors = saffron_vegetation::interchange_to_anchors(&payload, layer, &dimensions)
                .map_err(|error| Error::command(error.to_string()))?;

            // One chunk per tile the anchors landed in: an anchor lives in the tile of its own
            // position, exactly as a brush gesture's anchors do.
            let mut by_tile: std::collections::BTreeMap<
                saffron_spatial::WorldCellKey,
                Vec<saffron_vegetation::ExplicitPlantAnchor>,
            > = std::collections::BTreeMap::new();
            for anchor in anchors {
                by_tile
                    .entry(anchor.point.owner)
                    .or_default()
                    .push(anchor);
            }
            let existing = saffron_assets::load_vegetation_map_chunks(
                ctx.assets,
                map,
                &by_tile
                    .keys()
                    .map(|tile| saffron_vegetation::VegetationMapChunkKey {
                        layer,
                        tile: saffron_vegetation::VegetationMapTileKey::Cell(*tile),
                        kind: saffron_vegetation::VegetationMapChunkKind::AnchorOverride,
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let previous: std::collections::BTreeMap<_, _> = existing
                .into_iter()
                .map(|chunk| (chunk.key, chunk))
                .collect();
            let total = by_tile.values().map(Vec::len).sum::<usize>();
            let tiles = by_tile.len();
            let upserts = by_tile
                .into_iter()
                .map(|(tile, explicit_plants)| {
                    let key = saffron_vegetation::VegetationMapChunkKey {
                        layer,
                        tile: saffron_vegetation::VegetationMapTileKey::Cell(tile),
                        kind: saffron_vegetation::VegetationMapChunkKind::AnchorOverride,
                    };
                    // An import replaces the layer's anchors for the tiles it touches and leaves
                    // every other authored row alone.
                    let mut anchor_chunk = match previous.get(&key).map(|chunk| &chunk.payload) {
                        Some(saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(
                            chunk,
                        )) => chunk.clone(),
                        _ => saffron_vegetation::VegetationMapAnchorChunk {
                            explicit_plants: Vec::new(),
                            pins: Vec::new(),
                            transform_overrides: Vec::new(),
                            state_overrides: Vec::new(),
                            provenance: saffron_vegetation::ProvenanceTable::default(),
                        },
                    };
                    // Every anchor carries a lineage record saying an explicit anchor accepted it,
                    // the same shape a brush gesture's anchors carry, so the chunk explains where
                    // each plant came from.
                    let mut provenance = saffron_vegetation::ProvenanceTable::default();
                    let mut rows = explicit_plants;
                    for anchor in &mut rows {
                        let decision =
                            provenance.intern_decision(saffron_vegetation::ProvenanceDecision {
                                parents: Vec::new(),
                                subgraph_path: Vec::new(),
                                node: layer,
                                operator: saffron_vegetation::GraphOperator::ExplicitAnchors,
                                candidate: anchor.point.candidate,
                                outcome:
                                    saffron_vegetation::ProvenanceDecisionOutcome::Accepted,
                            });
                        let record = provenance.intern(saffron_vegetation::ProvenanceRecord {
                            map,
                            layer,
                            biome: saffron_core::Uuid(0),
                            decision,
                            candidate: anchor.point.candidate,
                            family: Some(anchor.family),
                            plant: Some(anchor.id),
                            variation: anchor.point.variation,
                        });
                        anchor.point.provenance = record.0;
                    }
                    anchor_chunk.provenance = provenance;
                    anchor_chunk.explicit_plants = rows;
                    saffron_vegetation::VegetationMapChunk {
                        version: saffron_vegetation::VEGETATION_MAP_CHUNK_VERSION,
                        map,
                        key,
                        revision: previous
                            .get(&key)
                            .map_or(1, |chunk| chunk.revision.saturating_add(1)),
                        payload: saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(
                            anchor_chunk,
                        ),
                    }
                })
                .collect();
            saffron_assets::commit_vegetation_map_transaction(
                ctx.assets,
                map,
                saffron_assets::VegetationMapTransaction {
                    expected_generation,
                    upserts,
                    removals: Vec::new(),
                },
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, map)
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::VegetationImportPointsResult {
                anchors: u32::try_from(total).unwrap_or(u32::MAX),
                tiles: u32::try_from(tiles).unwrap_or(u32::MAX),
                prototypes: u32::try_from(payload.prototypes.len()).unwrap_or(u32::MAX),
                unsupported: payload.unsupported,
                generation: root.generation.to_string(),
            })
        },
    );
    reg.register::<saffron_protocol::VegetationExportPointsParams, saffron_protocol::VegetationExportPointsResult>(
        "vegetation-export-points",
        "export one authored layer's anchors for a content-creation round trip",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = resolve_asset(ctx, &params.map)?;
            let layer = u128::from_str_radix(&params.layer.0, 16)
                .map_err(|_| Error::command("layer must be 32 hex digits"))?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, map)
                .map_err(|error| Error::command(error.to_string()))?;
            let keys: Vec<_> = root
                .inventory
                .iter()
                .map(|reference| reference.key)
                .filter(|key| {
                    key.layer == layer
                        && key.kind == saffron_vegetation::VegetationMapChunkKind::AnchorOverride
                })
                .collect();
            let chunks = saffron_assets::load_vegetation_map_chunks(ctx.assets, map, &keys)
                .map_err(|error| Error::command(error.to_string()))?;
            let mut anchors = Vec::new();
            for chunk in chunks {
                if let saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(payload) =
                    chunk.payload
                {
                    anchors.extend(payload.explicit_plants);
                }
            }
            anchors.sort_by_key(|anchor| anchor.id);
            // The prototype names are the families' catalog names, which is what a content-creation
            // tool shows an artist.
            let names = anchors
                .iter()
                .filter_map(|anchor| {
                    ctx.assets
                        .catalog()
                        .entries
                        .iter()
                        .find(|entry| entry.id == anchor.family)
                        .map(|entry| (anchor.family.value(), entry.name.clone()))
                })
                .collect();
            let payload = saffron_vegetation::anchors_to_interchange(&anchors, &names);
            let extension = std::path::Path::new(&params.path)
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            let text = match extension.as_str() {
                "geo" | "json" => saffron_json::dump_json(
                    &saffron_vegetation::write_houdini_points(&payload),
                    2,
                ),
                "usda" | "usd" => saffron_vegetation::write_usd_point_instancer(&payload),
                other => {
                    return Err(Error::command(format!(
                        "no point writer for '{other}'; expected geo or usda"
                    )));
                }
            };
            std::fs::write(&params.path, text)
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::VegetationExportPointsResult {
                instances: u32::try_from(payload.instances.len()).unwrap_or(u32::MAX),
                prototypes: u32::try_from(payload.prototypes.len()).unwrap_or(u32::MAX),
                path: params.path.clone(),
            })
        },
    );
}

pub fn register_plant_commands(reg: &mut CommandRegistry) {
    reg.register::<saffron_protocol::PlantCreateParams, saffron_protocol::PlantCreateResult>(
        "plant-create",
        "create a native plant family from the starter botanical graph",
        |ctx, params| {
            require_project_loaded(ctx)?;
            if params.name.trim().is_empty() {
                return Err(Error::command("plant name must not be empty"));
            }
            // A zero seed derives one from the name, so two plants created the same way are two
            // different individuals rather than the same tree twice.
            let seed = if params.seed.is_empty() || params.seed == "0" {
                saffron_vegetation::ContentHash::of(params.name.as_bytes()).bytes()[..16]
                    .try_into()
                    .map(u128::from_be_bytes)
                    .unwrap_or(1)
            } else {
                params
                    .seed
                    .parse::<u128>()
                    .map_err(|_| Error::command("seed must be a decimal u128"))?
            };
            // Wire UUIDs are catalog identities already; a material that is not in the catalog is
            // a caller mistake rather than something to invent a slot for.
            let materials = params
                .materials
                .iter()
                .map(|material| {
                    let id = saffron_core::Uuid(material.0);
                    ctx.assets
                        .catalog()
                        .entries
                        .iter()
                        .any(|entry| entry.id == id)
                        .then_some(id)
                        .ok_or_else(|| {
                            Error::command(format!("no material asset '{}'", material.0))
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let graph = saffron_vegetation::BotanicalGraphDocument::sapling(seed);
            let family = saffron_vegetation::native_plant_family(
                saffron_core::Uuid::new(),
                params.name.trim(),
                graph,
                materials,
                &saffron_vegetation::NoBotanicalModules,
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let growth = plant_growth_dto(
                ctx.assets,
                &family,
                0,
                &saffron_vegetation::BotanicalBudget::COOK,
            )?;
            let folder = if params.folder.trim().is_empty() {
                "plants"
            } else {
                params.folder.trim()
            };
            let plant = saffron_assets::save_plant_family_asset(
                ctx.assets,
                family,
                params.name.trim(),
                folder,
            )
            .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::PlantCreateResult {
                plant: WireUuid(plant.value()),
                growth,
            })
        },
    );
    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::PlantGraphResult>(
        "plant-graph",
        "read one native plant family's botanical graph and what it grows",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let saffron_vegetation::PlantFamilySource::Native { graph, grafts } = &plant.source
            else {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            };
            Ok(saffron_protocol::PlantGraphResult {
                plant: WireUuid(id.value()),
                graph: crate::botanical_dto::graph_dto(graph),
                grafts: grafts
                    .iter()
                    .map(crate::botanical_dto::graft_source_dto)
                    .collect(),
                growth: plant_growth_dto(
                    ctx.assets,
                    &plant,
                    params.variation,
                    &preview_budget(params.max_axes, params.max_elements),
                )?,
            })
        },
    );
    reg.register::<saffron_protocol::PlantGraphSetParams, saffron_protocol::PlantGraphResult>(
        "plant-graph-set",
        "replace one native plant family's botanical graph and regrow it",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let existing = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            if !matches!(
                existing.source,
                saffron_vegetation::PlantFamilySource::Native { .. }
            ) {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            }
            let graph = crate::botanical_dto::graph_from_dto(&params.graph)?;
            // A graft source keeps whatever content hash the last cook observed; a newly declared
            // one starts empty and the next cook records what it read.
            let previous: std::collections::BTreeMap<u128, [u8; 32]> = match &existing.source {
                saffron_vegetation::PlantFamilySource::Native { grafts, .. } => grafts
                    .iter()
                    .map(|graft| (graft.id, graft.content_hash))
                    .collect(),
                saffron_vegetation::PlantFamilySource::Imported(_) => Default::default(),
            };
            let mut grafts = params
                .grafts
                .iter()
                .map(|graft| {
                    let mut resolved = crate::botanical_dto::graft_source_from_dto(graft)?;
                    if let Some(hash) = previous.get(&resolved.id) {
                        resolved.content_hash = *hash;
                    }
                    Ok(resolved)
                })
                .collect::<Result<Vec<_>>>()?;
            grafts.sort_by_key(|graft| graft.id);
            // The family's parts, spines, and dimensions are what the new graph grows, never the
            // previous graph's leftovers: a native family's structure is a result.
            // The regrown family inherits the module table it is called with: `plant-graph-set`
            // replaces the graph, and a graph whose module calls lost their bindings would fail
            // validation on the way back in.
            let module_resolver = saffron_assets::PlantModules::for_family(ctx.assets, &existing);
            let mut updated = saffron_vegetation::native_plant_family(
                id,
                &existing.name,
                graph,
                existing.material_slots.clone(),
                &module_resolver,
            )
            .map_err(|error| Error::command(error.to_string()))?;
            if let saffron_vegetation::PlantFamilySource::Native {
                grafts: declared, ..
            } = &mut updated.source
            {
                *declared = grafts;
            }
            // Everything the artist authored around the graph survives the regrow. What the graph
            // derives — variations, appearances, proxies, dimensions, spines — is the NEW graph's,
            // never the previous one's leftovers.
            updated.tags = existing.tags.clone();
            updated.mechanics = existing.mechanics;
            updated.interaction_policy = existing.interaction_policy;
            updated.habitat = existing.habitat.clone();
            updated.ecology = existing.ecology.clone();
            saffron_vegetation::validate_plant_family(&updated)
                .map_err(|error| Error::command(error.to_string()))?;
            saffron_assets::update_plant_family_asset(ctx.assets, id, &updated)
                .map_err(|error| Error::command(error.to_string()))?;
            let saffron_vegetation::PlantFamilySource::Native { graph, grafts } = &updated.source
            else {
                return Err(Error::command("regrown family lost its botanical graph"));
            };
            Ok(saffron_protocol::PlantGraphResult {
                plant: WireUuid(id.value()),
                graph: crate::botanical_dto::graph_dto(graph),
                grafts: grafts
                    .iter()
                    .map(crate::botanical_dto::graft_source_dto)
                    .collect(),
                growth: plant_growth_dto(
                    ctx.assets,
                    &updated,
                    0,
                    &saffron_vegetation::BotanicalBudget::COOK,
                )?,
            })
        },
    );
    reg.register::<
        saffron_protocol::PlantPhenotypesParams,
        saffron_protocol::PlantPhenotypesResult,
    >(
        "plant-phenotypes",
        "plant-phenotypes {plant, phenotypes?} — read or replace one family's authored appearances",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let mut plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            if let Some(phenotypes) = &params.phenotypes {
                plant.phenotypes = phenotypes
                    .iter()
                    .map(phenotype_from_dto)
                    .collect::<Result<Vec<_>>>()?;
                // The family validator is the authority, not this command: it is what knows a
                // phenotype must name a declared variation, that two on one variation may not share
                // a role, that a remap must move between real slots, and that a family needs a
                // healthy appearance. Rejecting here would be a second rule to keep in sync.
                saffron_vegetation::validate_plant_family(&plant)
                    .map_err(|error| Error::command(error.to_string()))?;
                saffron_assets::update_plant_family_asset(ctx.assets, id, &plant)
                    .map_err(|error| Error::command(error.to_string()))?;
            }
            Ok(saffron_protocol::PlantPhenotypesResult {
                plant: WireUuid(id.value()),
                phenotypes: plant.phenotypes.iter().map(phenotype_dto).collect(),
            })
        },
    );
    reg.register::<saffron_protocol::PlantProxiesParams, saffron_protocol::PlantProxiesResult>(
        "plant-proxies",
        "plant-proxies {plant} — the collision and navigation proxies a family derived",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let metres = |value: saffron_spatial::DecisionScalar| value.bits() as f32 / 65_536.0;
            Ok(saffron_protocol::PlantProxiesResult {
                plant: WireUuid(id.value()),
                collision: plant
                    .collision_proxies
                    .iter()
                    .map(|proxy| saffron_protocol::PlantCollisionProxyDto {
                        shape: match proxy.shape {
                            saffron_vegetation::PlantCollisionShape::Box => {
                                saffron_protocol::PlantCollisionShapeDto::Box
                            }
                            saffron_vegetation::PlantCollisionShape::Sphere => {
                                saffron_protocol::PlantCollisionShapeDto::Sphere
                            }
                            saffron_vegetation::PlantCollisionShape::Capsule => {
                                saffron_protocol::PlantCollisionShapeDto::Capsule
                            }
                            saffron_vegetation::PlantCollisionShape::ConvexHull => {
                                saffron_protocol::PlantCollisionShapeDto::ConvexHull
                            }
                        },
                        center_m: [
                            metres(proxy.center[0]),
                            metres(proxy.center[1]),
                            metres(proxy.center[2]),
                        ],
                        dimensions_m: [
                            metres(proxy.dimensions[0]),
                            metres(proxy.dimensions[1]),
                            metres(proxy.dimensions[2]),
                        ],
                        breakable: proxy.breakable,
                    })
                    .collect(),
                navigation: plant
                    .navigation_proxies
                    .iter()
                    .map(|proxy| saffron_protocol::PlantNavigationProxyDto {
                        footprint_m: proxy
                            .footprint
                            .iter()
                            .map(|point| [metres(point[0]), metres(point[1])])
                            .collect(),
                        height_m: metres(proxy.height),
                        cost: f32::from(proxy.cost.bits()) / 65_535.0,
                    })
                    .collect(),
            })
        },
    );

    reg.register::<
        saffron_protocol::PlantSeasonPhenotypeParams,
        saffron_protocol::PlantSeasonPhenotypeResult,
    >(
        "plant-season-phenotype",
        "plant-season-phenotype {plant, seasonMille, lifecycle?} — the appearance a family renders then",
        |ctx, params| {
            require_project_loaded(ctx)?;
            if params.season_mille > 999 {
                return Err(Error::command("seasonMille is per-mille of the year (0..1000)"));
            }
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let lifecycle = params.lifecycle.map_or(
                saffron_vegetation::PlantLifecycle::Mature,
                crate::commands_vegetation_runtime::lifecycle_from_dto,
            );
            // The engine's own resolver, not a second reading of the same rules: lifecycle wins
            // over season (a dead plant does not turn autumnal), and the cooked phenotype is the
            // fallback throughout. A preview that resolved this differently from the renderer would
            // be showing an appearance the scene never picks.
            let phenotype = saffron_vegetation::resolve_rendered_phenotype(
                plant
                    .phenotypes
                    .iter()
                    .map(|entry| (entry.id, entry.role, entry.season_window)),
                plant.phenotypes.first().map_or(0, |entry| entry.id),
                lifecycle,
                u16::try_from(params.season_mille).unwrap_or(0),
            );
            let variation = plant
                .phenotypes
                .iter()
                .find(|entry| entry.id == phenotype)
                .map_or(0, |entry| entry.variation);
            Ok(saffron_protocol::PlantSeasonPhenotypeResult {
                plant: WireUuid(id.value()),
                phenotype,
                variation,
            })
        },
    );

    reg.register::<saffron_protocol::PlantHierarchyParams, saffron_protocol::PlantHierarchyResult>(
        "plant-hierarchy",
        "plant-hierarchy {plant} — the cooked cut: each node's representation, page, and declared error",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let options = saffron_assets::PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(None),
            };
            let published = match recook_plant_family(ctx.assets, &plant, &options)
                .map_err(Error::from)?
            {
                saffron_assets::PlantRecookOutcome::Published(published) => published,
                saffron_assets::PlantRecookOutcome::Rejected(_) => {
                    return Err(Error::command(
                        "plant family does not validate — run plant-validate for diagnostics",
                    ));
                }
            };
            let hierarchy = saffron_assets::plant_family_hierarchy(
                ctx.assets,
                published.publication.content_hash,
            )
            .map_err(Error::from)?;
            // Depth is computed once here rather than left to the reader: a caller that walked
            // parents per row would be O(n²) on a family with thousands of nodes, and the shape of
            // the cut is the first thing anyone looks at.
            //
            // The walk goes DOWN from the roots rather than reading each node's parent in array
            // order. The cooker emits leaves before the roots that own them, so a parent's depth is
            // usually still unset when its child is visited, and an array-order pass reports every
            // node one level below its root — a two-level family reads as if it were flat.
            let mut depth = vec![0_u32; hierarchy.nodes.len()];
            let mut frontier: Vec<u32> = hierarchy.roots.clone();
            let mut visited = vec![false; hierarchy.nodes.len()];
            for root in &frontier {
                if let Some(seen) = visited.get_mut(*root as usize) {
                    *seen = true;
                }
            }
            while let Some(index) = frontier.pop() {
                let Some(node) = hierarchy.nodes.get(index as usize) else {
                    continue;
                };
                let child_depth = depth[index as usize].saturating_add(1);
                for child in &node.children {
                    // The guard is against a malformed tree, not an expected shape: a cycle would
                    // otherwise spin here forever.
                    match visited.get_mut(*child as usize) {
                        Some(seen) if !*seen => *seen = true,
                        _ => continue,
                    }
                    depth[*child as usize] = child_depth;
                    frontier.push(*child);
                }
            }
            let mut triangle_nodes = 0;
            let mut voxel_nodes = 0;
            let nodes = hierarchy
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    let (representation, primitives) = match node.representation {
                        saffron_geometry::HierarchyRepresentation::Triangles { count, .. } => {
                            triangle_nodes += 1;
                            ("triangles", count)
                        }
                        saffron_geometry::HierarchyRepresentation::Voxel { brick } => {
                            voxel_nodes += 1;
                            let triangles = hierarchy
                                .voxel_bricks
                                .get(brick as usize)
                                .map_or(0, |brick| {
                                    u32::try_from(brick.indices.len() / 3).unwrap_or(u32::MAX)
                                });
                            ("voxel", triangles)
                        }
                    };
                    saffron_protocol::PlantHierarchyNodeDto {
                        id: node.id,
                        parent: node.parent,
                        depth: depth.get(index).copied().unwrap_or(0),
                        representation: representation.to_owned(),
                        primitives,
                        page: node.page,
                        child_count: u32::try_from(node.children.len()).unwrap_or(u32::MAX),
                        appearance_error: saffron_protocol::AppearanceErrorDto {
                            silhouette: node.appearance_error.silhouette,
                            coverage: node.appearance_error.coverage,
                            transmission: node.appearance_error.transmission,
                            material: node.appearance_error.material,
                            normal_distribution: node.appearance_error.normal_distribution,
                            total: node.appearance_error.total,
                        },
                    }
                })
                .collect();
            Ok(saffron_protocol::PlantHierarchyResult {
                plant: WireUuid(id.value()),
                triangle_nodes,
                voxel_nodes,
                nodes,
            })
        },
    );

    reg.register::<saffron_protocol::PlantAtlasParams, saffron_protocol::PlantAtlasResult>(
        "plant-atlas",
        "plant-atlas {plant, level?} — one cooked family's packed coverage atlas as a PNG",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let options = saffron_assets::PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(None),
            };
            // Recooking is a cache hit for a family already cooked, so this reads the published
            // artifact rather than packing a second atlas — the family's UVs address exactly one
            // layout, and a fresh packing would not be it.
            let published =
                match recook_plant_family(ctx.assets, &plant, &options).map_err(Error::from)? {
                    saffron_assets::PlantRecookOutcome::Published(published) => published,
                    saffron_assets::PlantRecookOutcome::Rejected(_) => {
                        return Err(Error::command(
                            "plant family does not validate — run plant-validate for diagnostics",
                        ));
                    }
                };
            let atlas = saffron_assets::plant_family_atlas_image(
                ctx.assets,
                published.publication.content_hash,
                params.level,
            )
            .map_err(Error::from)?
            .ok_or_else(|| {
                Error::command(
                    "this family cooked no packed atlas: its slots resolve to catalog materials",
                )
            })?;
            Ok(saffron_protocol::PlantAtlasResult {
                plant: WireUuid(id.value()),
                level: params.level,
                level_count: atlas.level_count,
                width: atlas.width,
                height: atlas.height,
                gutter: atlas.gutter,
                placements: atlas
                    .placements
                    .iter()
                    .map(|placement| saffron_protocol::PlantAtlasPlacementDto {
                        slot: placement.slot,
                        x: placement.x,
                        y: placement.y,
                        width: placement.width,
                        height: placement.height,
                    })
                    .collect(),
                base64: base64_encode(&atlas.png),
            })
        },
    );

    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::PlantElementsResult>(
        "plant-elements",
        "every element of one native plant family a manual edit can address",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let saffron_vegetation::PlantFamilySource::Native { graph, .. } = &plant.source else {
                return Err(Error::command(
                    "plant family has an imported source, not a botanical graph",
                ));
            };
            let modules = saffron_assets::PlantModules::for_family(ctx.assets, &plant);
            let growth = saffron_vegetation::grow(
                graph,
                params.variation as usize,
                &modules,
                &saffron_vegetation::BotanicalBudget::COOK,
            )
            .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::PlantElementsResult {
                plant: WireUuid(id.value()),
                axes: growth
                    .assembly
                    .axes
                    .iter()
                    .map(crate::botanical_dto::axis_dto)
                    .collect(),
                elements: growth
                    .assembly
                    .elements
                    .iter()
                    .map(crate::botanical_dto::placement_dto)
                    .collect(),
            })
        },
    );
    reg.register::<saffron_protocol::PlantGrowthParams, saffron_protocol::BotanicalGrowthDto>(
        "plant-growth",
        "what one plant family's botanical graph grows",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            plant_growth_dto(
                ctx.assets,
                &plant,
                params.variation,
                &preview_budget(params.max_axes, params.max_elements),
            )
        },
    );
    reg.register::<PlantValidateParams, PlantValidationResult>(
        "plant-validate",
        "validate one authored plant family and inspect its exact sources",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let outcome =
                validate_plant_family_sources(ctx.assets, &plant, PlantCompileLimits::default())
                    .map_err(Error::from)?;
            Ok(PlantValidationResult {
                plant: WireUuid(id.value()),
                validation: plant_validation_summary(&outcome),
                diagnostics: outcome
                    .compile
                    .diagnostics
                    .iter()
                    .map(plant_compile_diagnostic_dto)
                    .collect(),
                sources: plant_sources_dto(&plant, &outcome),
                dependencies: manifest_dependencies_dto(&outcome.dependencies),
                conflicts: outcome
                    .compile
                    .conflicts
                    .conflicts
                    .iter()
                    .map(plant_reimport_conflict_dto)
                    .collect(),
                source_updates: plant_source_updates_dto(&outcome),
                family_hash: outcome.compile.family_hash.as_ref().map(coverage_hash_text),
                statistics: plant_compile_statistics_dto(outcome.compile.statistics),
            })
        },
    );

    reg.register::<PlantRecookParams, PlantRecookResult>(
        "plant-recook",
        "normalize and atomically publish one plant family through its retained recipe",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.plant)?;
            let plant = load_plant_family_asset(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            if params
                .platform_profile
                .as_deref()
                .is_some_and(str::is_empty)
            {
                return Err(Error::command("platformProfile cannot be empty"));
            }
            let options = PlantRecookOptions {
                limits: PlantCompileLimits::default(),
                versions: vegetation_cook_versions(),
                platform: portable_vegetation_platform_profile(params.platform_profile.as_deref()),
            };
            match recook_plant_family(ctx.assets, &plant, &options).map_err(Error::from)? {
                PlantRecookOutcome::Published(published) => {
                    let family_hash = published
                        .validation
                        .compile
                        .family_hash
                        .as_ref()
                        .map(coverage_hash_text)
                        .ok_or_else(|| {
                            Error::command("plant compiler published without a family hash")
                        })?;
                    Ok(PlantRecookResult {
                        plant: WireUuid(id.value()),
                        family_hash,
                        artifact_hash: published.publication.content_hash.to_string(),
                        cache_hit: published.publication.cache_hit,
                        validation: plant_validation_summary(&published.validation),
                        diagnostics: published
                            .validation
                            .compile
                            .diagnostics
                            .iter()
                            .map(plant_compile_diagnostic_dto)
                            .collect(),
                        sources: plant_sources_dto(
                            &published.accepted_asset,
                            &published.validation,
                        ),
                        dependencies: manifest_dependencies_dto(&published.validation.dependencies),
                        source_updates: plant_source_updates_dto(&published.validation),
                        statistics: plant_compile_statistics_dto(
                            published.validation.compile.statistics,
                        ),
                    })
                }
                PlantRecookOutcome::Rejected(rejected) => {
                    let conflicts = rejected
                        .compile
                        .conflicts
                        .conflicts
                        .iter()
                        .map(plant_reimport_conflict_dto)
                        .collect::<Vec<_>>();
                    if !conflicts.is_empty() {
                        return Err(Error::Diagnostic {
                            message: format!("plant family {} has reimport conflicts", id.value()),
                            diagnostic: Box::new(ControlDiagnosticDto::ReimportConflict(
                                ReimportConflictDiagnosticDto {
                                    plant: WireUuid(id.value()),
                                    conflicts,
                                },
                            )),
                        });
                    }
                    let message = rejected
                        .compile
                        .diagnostics
                        .iter()
                        .find(|diagnostic| {
                            diagnostic.severity == PlantCompileDiagnosticSeverity::Error
                        })
                        .map(|diagnostic| diagnostic.message.clone())
                        .unwrap_or_else(|| {
                            format!("plant family {} failed validation", id.value())
                        });
                    Err(Error::command(message))
                }
            }
        },
    );
}

/// Registers the asset/project-domain commands in the frozen manifest order
/// (`get-project` … `quit`).
pub fn register_asset_commands(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, ProjectInfoDto>(
        "get-project",
        "get-project — active project metadata",
        |ctx, _params| Ok(project_dto(&current_project_info(ctx))),
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "project-status",
        "project-status — project-load phase + progress",
        |ctx, _params| Ok(project_status_dto(ctx)),
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "cancel-load",
        "cancel-load — abort the in-flight project load",
        |ctx, _params| {
            ctx.scene_edit.project_cancel = true;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<NewProjectParams, ProjectStatusDto>(
        "new-project",
        "new-project {name, displayName?, root?}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let name = params.name.unwrap_or_default();
            if !valid_project_name(&name) {
                return Err(Error::command(format!("invalid project name '{name}'")));
            }
            ctx.vegetation_cook_jobs.shutdown();
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::New(NewProjectSpec {
                name,
                display_name: params.display_name.unwrap_or_default(),
                root: params.root.unwrap_or_default(),
            }));
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<CreateScriptParams, CreateScriptResult>(
        "create-script",
        "create-script {name} — boilerplate .lua under the project src/",
        |ctx, params| {
            if !ctx.scene_edit.project_ready() {
                return Err(Error::command("no project loaded"));
            }
            let path = create_project_script(&ctx.scene_edit.project_root, &params.name)
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(CreateScriptResult { path })
        },
    );

    reg.register::<PathParams, ProjectStatusDto>(
        "open-project",
        "open-project {path}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            ctx.vegetation_cook_jobs.shutdown();
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(params.path.clone()));
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<ImportModelParams, ImportModelResult>(
        "import-model",
        "import-model {path} — optional store attribution",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let bake = ctx
                .assets
                .import_model(&params.path, saffron_assets::ImportOptions::default())
                .map_err(|e| Error::command(e.to_string()))?;
            if let Some(attribution) = params.attribution {
                let _ = ctx
                    .assets
                    .set_asset_attribution(bake.model_id, attribution_from_dto(attribution));
            }
            let name = ctx
                .assets
                .catalog()
                .find(bake.model_id)
                .map(|entry| entry.name.clone())
                .unwrap_or_default();
            Ok(ImportModelResult {
                id: WireUuid(bake.model_id.value()),
                name,
                r#type: "model".to_owned(),
            })
        },
    );

    reg.register::<InstantiateModelParams, EntityRef>(
        "instantiate-model",
        "instantiate-model {asset} [name]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let entry_type = ctx.assets.catalog().find(id).map(|e| e.asset_type);
            let entry_name = ctx
                .assets
                .catalog()
                .find(id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            if entry_type != Some(AssetType::Model) {
                return Err(Error::command(format!(
                    "asset {} is not a model",
                    id.value()
                )));
            }
            let name = match &params.name {
                Some(name) if !name.is_empty() => name.clone(),
                _ => entry_name,
            };
            let root = ctx
                .assets
                .instantiate_model(ctx.scene_edit.active_scene(), id, &name)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(root);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, root))
        },
    );

    reg.register::<AssetPlacementParams, AssetPlacementResult>(
        "asset-placement",
        "asset-placement {phase, asset?, u?, v?}",
        |ctx, params| match params.phase {
            AssetPlacementPhaseDto::Preview => preview_asset_placement(ctx, params),
            AssetPlacementPhaseDto::Commit => commit_asset_placement(ctx),
            AssetPlacementPhaseDto::Clear => {
                clear_placement_ghost(ctx);
                Ok(AssetPlacementResult {
                    active: false,
                    valid: true,
                    transform: None,
                    entity: None,
                    reason: None,
                })
            }
        },
    );

    reg.register::<ImportTextureParams, ImportTextureResult>(
        "import-texture",
        "import-texture {path} [colorspace] [role]",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            // A `role` hint (from an import connector, or manual) resolves both the stored role
            // and — absent an explicit `colorspace` override — the upload colorspace. No role and
            // no override leaves colorspace `None`, so the loader falls back to its ext heuristic.
            let role = params.role.as_deref().map(texture_role_from_hint);
            let colorspace = colorspace_from_str(params.colorspace.as_deref())
                .or_else(|| role.map(colorspace_for_role_explicit));
            let role = role.unwrap_or(TextureRole::Unknown);
            let assets = &mut *ctx.assets;
            let path = params.path.clone();
            let mut result = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = Some(assets.import_texture(gpu, &path, colorspace, role));
            });
            let id = result
                .ok_or_else(|| Error::command("upload seam unavailable"))?
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(ImportTextureResult {
                texture: WireUuid(id.value()),
            })
        },
    );

    reg.register::<saffron_protocol::ImportLutParams, saffron_protocol::ImportLutResult>(
        "import-lut",
        "import-lut {path} — import a creative .cube look as a LUT asset",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            let assets = &mut *ctx.assets;
            let path = params.path.clone();
            let mut result = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = Some(assets.import_cube_lut(gpu, &path));
            });
            let id = result
                .ok_or_else(|| Error::command("upload seam unavailable"))?
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(saffron_protocol::ImportLutResult {
                lut: WireUuid(id.value()),
            })
        },
    );

    reg.register::<ImportVegetationAssetParams, ImportVegetationAssetResult>(
        "import-vegetation-asset",
        "import-vegetation-asset {path} [folder] — import an authored .splant, .sbiome, or .svegmap package",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            let folder = params.folder.unwrap_or_default();
            if !folder.is_empty() && !has_folder(ctx.assets.catalog(), &folder) {
                return Err(Error::command(format!("no asset folder '{folder}'")));
            }
            let imported = import_vegetation_asset(ctx.assets, &params.path, &folder)
                .map_err(|error| Error::command(error.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(ImportVegetationAssetResult {
                id: WireUuid(imported.id.value()),
                name: imported.name,
                r#type: asset_type_dto(imported.asset_type),
            })
        },
    );

    reg.register::<EmptyParams, AssetList>(
        "list-assets",
        "list the project asset catalog",
        |ctx, _params| Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog())),
    );

    reg.register::<saffron_protocol::VegetationMapLayerCommitParams, saffron_protocol::VegetationMapLayerCommitResult>(
        "vegetation-map-layer-commit",
        "commit one optimistic authored-map layer transaction (upserts + removals)",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.map)?;
            let expected_generation = params
                .expected_generation
                .parse::<u64>()
                .map_err(|_| Error::command("expectedGeneration is not a u64"))?;
            let upserts = params
                .upserts
                .iter()
                .map(|layer| {
                    let layer = crate::vegetation_layer_dto::layer_from_dto(layer)?;
                    Ok(saffron_vegetation::VegetationMapChunk {
                        version: saffron_vegetation::VEGETATION_MAP_CHUNK_VERSION,
                        map: id,
                        key: saffron_vegetation::VegetationMapChunkKey {
                            layer: layer.id,
                            tile: saffron_vegetation::VegetationMapTileKey::Global,
                            kind: saffron_vegetation::VegetationMapChunkKind::LayerMetadata,
                        },
                        revision: layer.revision,
                        payload: saffron_vegetation::VegetationMapChunkPayload::LayerMetadata(
                            layer,
                        ),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let removals = params
                .removals
                .iter()
                .map(|guid| {
                    Ok(saffron_vegetation::VegetationMapChunkKey {
                        layer: u128::from_str_radix(&guid.0, 16)
                            .map_err(|_| Error::command("vegetation GUID is not canonical"))?,
                        tile: saffron_vegetation::VegetationMapTileKey::Global,
                        kind: saffron_vegetation::VegetationMapChunkKind::LayerMetadata,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            saffron_assets::commit_vegetation_map_transaction(
                ctx.assets,
                id,
                saffron_assets::VegetationMapTransaction {
                    expected_generation,
                    upserts,
                    removals,
                },
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::VegetationMapLayerCommitResult {
                generation: root.generation.to_string(),
            })
        },
    );

    reg.register::<saffron_protocol::VegetationMapChunkCommitParams, saffron_protocol::VegetationMapChunkCommitResult>(
        "vegetation-map-chunk-commit",
        "commit one optimistic authored-map chunk transaction (a brush gesture's tiles + anchors)",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.map)?;
            let expected_generation = params
                .expected_generation
                .parse::<u64>()
                .map_err(|_| Error::command("expectedGeneration is not a u64"))?;
            let upserts = params
                .upserts
                .iter()
                .map(|chunk| {
                    Ok(saffron_vegetation::VegetationMapChunk {
                        version: saffron_vegetation::VEGETATION_MAP_CHUNK_VERSION,
                        map: id,
                        key: crate::vegetation_layer_dto::chunk_key_from_dto(&chunk.key)?,
                        revision: chunk
                            .revision
                            .parse::<u64>()
                            .map_err(|_| Error::command("chunk revision is not a u64"))?,
                        payload: {
                            let key =
                                crate::vegetation_layer_dto::chunk_key_from_dto(&chunk.key)?;
                            crate::vegetation_layer_dto::chunk_payload_from_dto(
                                id,
                                key.layer,
                                &chunk.payload,
                            )?
                        },
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let removals = params
                .removals
                .iter()
                .map(crate::vegetation_layer_dto::chunk_key_from_dto)
                .collect::<Result<Vec<_>>>()?;
            saffron_assets::commit_vegetation_map_transaction(
                ctx.assets,
                id,
                saffron_assets::VegetationMapTransaction {
                    expected_generation,
                    upserts,
                    removals,
                },
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::VegetationMapChunkCommitResult {
                generation: root.generation.to_string(),
            })
        },
    );

    reg.register::<saffron_protocol::VegetationMapChunkReadParams, saffron_protocol::VegetationMapChunkReadResult>(
        "vegetation-map-chunk-read",
        "read authored map chunks by logical key (a brush gesture's read-modify-write baseline)",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.map)?;
            let keys = params
                .keys
                .iter()
                .map(crate::vegetation_layer_dto::chunk_key_from_dto)
                .collect::<Result<Vec<_>>>()?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, id)
                .map_err(|error| Error::command(error.to_string()))?;
            let chunks = saffron_assets::load_vegetation_map_chunks(ctx.assets, id, &keys)
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(saffron_protocol::VegetationMapChunkReadResult {
                generation: root.generation.to_string(),
                chunks: chunks
                    .iter()
                    .map(vegetation_map_chunk_dto)
                    .collect::<Result<Vec<_>>>()?,
            })
        },
    );

    reg.register::<VegetationAssetSummaryParams, VegetationAssetSummaryResult>(
        "vegetation-asset-summary",
        "vegetation-asset-summary {asset} — inspect an authored plant, biome, or vegetation map",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.asset)?;
            let entry = ctx
                .assets
                .catalog()
                .find(id)
                .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?
                .clone();
            let (summary, layers) = match entry.asset_type {
                AssetType::Plant => {
                    let plant = load_plant_family_asset(ctx.assets, id)
                        .map_err(|error| Error::command(error.to_string()))?;
                    let source = match &plant.source {
                        PlantFamilySource::Imported(_) => PlantSourceKindDto::Imported,
                        PlantFamilySource::Native { .. } => PlantSourceKindDto::Native,
                    };
                    let validation = validate_plant_family_sources(
                        ctx.assets,
                        &plant,
                        PlantCompileLimits::default(),
                    )
                    .map_err(Error::from)?;
                    let provenance = match &plant.source {
                        PlantFamilySource::Imported(recipe) => recipe
                            .sources
                            .iter()
                            .map(|source| source_provenance_dto(&source.provenance))
                            .collect(),
                        PlantFamilySource::Native { .. } => Vec::new(),
                    };
                    (
                        VegetationAssetSummaryDto::Plant(PlantAssetSummaryDto {
                            id: WireUuid(id.value()),
                            name: plant.name,
                            version: plant.version,
                            source,
                            part_count: u32::try_from(plant.parts.len()).unwrap_or(u32::MAX),
                            phenotype_count: u32::try_from(plant.phenotypes.len())
                                .unwrap_or(u32::MAX),
                            material_slots: plant
                                .material_slots
                                .into_iter()
                                .map(|material| WireUuid(material.value()))
                                .collect(),
                            validation: plant_validation_summary(&validation),
                            provenance,
                            dependencies: manifest_dependencies_dto(&validation.dependencies),
                            latest_cook: None,
                        }),
                        Vec::new(),
                    )
                }
                AssetType::Biome => {
                    let biome = load_biome_asset(ctx.assets, id)
                        .map_err(|error| Error::command(error.to_string()))?;
                    let (validation, dependencies) = biome_summary_metadata(ctx.assets, &biome);
                    (
                        VegetationAssetSummaryDto::Biome(BiomeAssetSummaryDto {
                            id: WireUuid(id.value()),
                            name: biome.name,
                            version: biome.version,
                            graph: biome.graph.clone(),
                            role: match biome.role {
                                BiomeRole::Root => BiomeRoleDto::Root,
                                BiomeRole::Module => BiomeRoleDto::Module,
                            },
                            plant_palette: biome
                                .palette
                                .into_iter()
                                .map(|item| WireUuid(item.plant.value()))
                                .collect(),
                            modules: biome
                                .modules
                                .into_iter()
                                .map(|item| WireUuid(item.biome.value()))
                                .collect(),
                            parameter_count: u32::try_from(biome.parameters.len())
                                .unwrap_or(u32::MAX),
                            validation,
                            provenance: Vec::new(),
                            dependencies,
                            latest_cook: None,
                        }),
                        Vec::new(),
                    )
                }
                AssetType::VegetationMap => {
                    let map = load_vegetation_map_snapshot(ctx.assets, id)
                        .map_err(|error| Error::command(error.to_string()))?;
                    let (validation, authored_dependencies) =
                        map_authored_metadata(ctx.assets, &map);
                    let (manifest, dependencies, latest_cook) =
                        match current_map_cook_metadata(ctx.assets, id)? {
                            Some((manifest, dependencies, statistics)) => {
                                (Some(manifest), dependencies, Some(statistics))
                            }
                            None => (None, authored_dependencies, None),
                        };
                    let dirty_layers = dirty_map_layers(id, &map.root.inventory, manifest.as_ref());
                    let layers = map.layers.iter().map(vegetation_layer_dto).collect();
                    (
                        VegetationAssetSummaryDto::VegetationMap(VegetationMapSummaryDto {
                            id: WireUuid(id.value()),
                            name: map.name.clone(),
                            version: map.version,
                            generation: map.generation.to_string(),
                            bounds: world_bounds_dto(map.bounds),
                            layer_count: u32::try_from(map.layers.len()).unwrap_or(u32::MAX),
                            biome_instances: map
                                .biome_instances
                                .iter()
                                .map(|instance| saffron_protocol::VegetationBiomeInstanceRefDto {
                                    instance: vegetation_guid(instance.id),
                                    biome: WireUuid(instance.biome.value()),
                                })
                                .collect(),
                            chunk_level: map.chunk_layout.level,
                            dirty_layers,
                            validation,
                            provenance: Vec::new(),
                            dependencies,
                            latest_cook,
                        }),
                        layers,
                    )
                }
                _ => {
                    return Err(Error::command(format!(
                        "asset {} is not a plant, biome, or vegetation map",
                        id.value()
                    )));
                }
            };
            Ok(VegetationAssetSummaryResult {
                r#type: asset_type_dto(entry.asset_type),
                summary,
                layers,
            })
        },
    );

    reg.register::<EmptyParams, ScanAssetsResult>("scan-assets", "scan-assets", |ctx, _params| {
        require_project_loaded(ctx)?;
        ctx.renderer.wait_gpu_idle();
        ctx.assets.clear_asset_caches();
        let delta = ctx
            .assets
            .scan_assets()
            .map_err(|e| Error::command(e.to_string()))?;
        ctx.assets.write_catalog_cache();
        Ok(ScanAssetsResult {
            added: i32::try_from(delta.added.len()).unwrap_or(i32::MAX),
            removed: i32::try_from(delta.removed.len()).unwrap_or(i32::MAX),
        })
    });

    reg.register::<ExtractSubAssetParams, AssetRef>(
        "extract-subasset",
        "extract-subasset {asset} {subAsset} [dest]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let model_id = resolve_asset(ctx, &params.asset)?;
            let dest = params.dest.clone().unwrap_or_default();
            let extracted =
                extract_sub_asset(ctx.assets, model_id, Uuid(params.sub_asset.0), &dest)
                    .map_err(|e| Error::command(e.to_string()))?;
            let name = ctx
                .assets
                .catalog()
                .find(extracted)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            Ok(AssetRef {
                id: WireUuid(extracted.value()),
                name,
                folder: None,
            })
        },
    );

    reg.register::<ClearExtractionParams, AssetRef>(
        "clear-extraction",
        "clear-extraction {asset} {subAsset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let model_id = resolve_asset(ctx, &params.asset)?;
            let sub_id = Uuid(params.sub_asset.0);
            clear_extraction(ctx.assets, model_id, sub_id)
                .map_err(|e| Error::command(e.to_string()))?;
            let name = ctx
                .assets
                .catalog()
                .find(sub_id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            Ok(AssetRef {
                id: WireUuid(sub_id.value()),
                name,
                folder: None,
            })
        },
    );

    reg.register::<ReimportModelParams, ReimportModelResult>(
        "reimport-model",
        "reimport-model {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            ctx.renderer.wait_gpu_idle();
            let delta =
                reimport_model(ctx.assets, id).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(ReimportModelResult {
                updated: i32::try_from(delta.updated.len()).unwrap_or(i32::MAX),
                added: i32::try_from(delta.added.len()).unwrap_or(i32::MAX),
                removed_from_source: i32::try_from(delta.removed_from_source.len())
                    .unwrap_or(i32::MAX),
                skipped: delta.skipped,
            })
        },
    );

    reg.register::<ModelInfoParams, ModelInfoResult>(
        "model-info",
        "model-info {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            if ctx.assets.catalog().find(id).map(|e| e.asset_type) != Some(AssetType::Model) {
                return Err(Error::command(format!(
                    "asset {} is not a model",
                    id.value()
                )));
            }
            let path = ctx
                .assets
                .catalog()
                .find(id)
                .map(|e| e.path.clone())
                .unwrap_or_default();
            let model = ctx
                .assets
                .load_model_asset(id)
                .ok_or_else(|| Error::command(format!("model {} is not loadable", id.value())))?;
            let meta = model.meta.clone();
            let total_bytes = std::fs::metadata(ctx.assets.root.join(&path))
                .map(|m| m.len())
                .unwrap_or(0);
            let mut material_count = 0;
            let mut sub_assets = Vec::new();
            for sub in &meta.sub_assets {
                if sub.asset_type == AssetType::Material {
                    material_count += 1;
                }
                let sub_row = AssetEntry {
                    id: sub.sub_id,
                    asset_type: sub.asset_type,
                    container: id,
                    ..AssetEntry::default()
                };
                let bytes = asset_bytes(ctx.assets, &sub_row);
                sub_assets.push(ModelSubAssetDto {
                    id: WireUuid(sub.sub_id.value()),
                    name: sub.name.clone(),
                    r#type: asset_type_name(sub.asset_type).to_owned(),
                    bytes,
                });
            }
            Ok(ModelInfoResult {
                id: WireUuid(id.value()),
                name: meta.name.clone(),
                source_path: meta.import.source_path.clone(),
                source_hash: meta.import.source_hash.clone(),
                material_count,
                has_skin: !meta.skin.is_null(),
                node_count: i32::try_from(meta.nodes.as_array().map_or(0, Vec::len))
                    .unwrap_or(i32::MAX),
                total_bytes,
                sub_assets,
            })
        },
    );

    reg.register::<AssetReferencesParams, AssetReferencesResult>(
        "asset-references",
        "asset-references {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let graph = build_dependency_graph(scene, assets);
            Ok(AssetReferencesResult {
                referenced_by: graph
                    .referenced_by(id)
                    .iter()
                    .map(|u| u.value().to_string())
                    .collect(),
                references: graph
                    .references_of(id)
                    .iter()
                    .map(|u| u.value().to_string())
                    .collect(),
                footprint: graph.footprint(id),
            })
        },
    );

    reg.register::<GetAssetModelParams, AssetModelResult>(
        "get-asset-model",
        "get-asset-model {asset} — a model's capabilities + bone tree + clips, from its .smodel container",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let entry = ctx
                .assets
                .catalog()
                .find(id)
                .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?;
            let container_id = if entry.asset_type == AssetType::Model {
                id
            } else {
                entry.container
            };
            if container_id.value() == 0 {
                return Err(Error::command(format!(
                    "asset {} is not part of a model container",
                    id.value()
                )));
            }
            let model = ctx.assets.load_model_asset(container_id).ok_or_else(|| {
                Error::command(format!("model {} is not loadable", container_id.value()))
            })?;
            let meta = model.meta.clone();
            let node_count = meta.nodes.as_array().map_or(0, Vec::len);
            let has_rig = !meta.skin.is_null();
            let bones = if has_rig { build_bone_tree(&meta) } else { Vec::new() };
            let clips = container_clips(ctx.assets, &meta);
            let mesh_count = meta
                .sub_assets
                .iter()
                .filter(|s| s.asset_type == AssetType::Mesh)
                .count();
            let material_count = meta
                .sub_assets
                .iter()
                .filter(|s| s.asset_type == AssetType::Material)
                .count();
            Ok(AssetModelResult {
                mesh: WireUuid(container_id.value()),
                name: meta.name.clone(),
                capabilities: AssetCapabilitiesDto {
                    mesh_count: i32::try_from(mesh_count).unwrap_or(i32::MAX),
                    material_count: i32::try_from(material_count).unwrap_or(i32::MAX),
                    node_count: i32::try_from(node_count).unwrap_or(i32::MAX),
                    has_rig,
                    bone_count: i32::try_from(bones.len()).unwrap_or(i32::MAX),
                    clip_count: i32::try_from(clips.len()).unwrap_or(i32::MAX),
                },
                bones,
                clips,
            })
        },
    );

    reg.register::<GetAssetModelParams, AssetPreviewResultWrap>(
        "enter-asset-preview",
        "enter-asset-preview {asset} — open any model in an isolated preview scene",
        enter_asset_preview,
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "exit-asset-preview",
        "exit-asset-preview — close the asset preview and restore the authored scene + camera",
        |ctx, _params| {
            if ctx.scene_edit.previewing() {
                ctx.renderer.set_active_view(ViewId::Scene);
                // Restore the authored tonemap exposure the HDRI preview's EV sweep may have moved.
                ctx.renderer.set_exposure(ctx.scene_edit.saved_exposure);
            }
            leave_asset_preview(ctx.scene_edit);
            Ok(play_state_result(ctx))
        },
    );

    reg.register::<SetActiveViewParams, SetActiveViewResult>(
        "set-active-view",
        "set-active-view {view} — switch the rendered view (scene | assetPreview)",
        |ctx, params| {
            let view = ViewId::from_wire(&params.view).ok_or_else(|| {
                Error::command(format!(
                    "unknown view '{}' (expected 'scene' or 'assetPreview')",
                    params.view
                ))
            })?;
            if view == ViewId::AssetPreview
                && ctx.renderer.view_desired_size(ViewId::AssetPreview).0 == 0
            {
                let (w, h) = (
                    ctx.renderer.viewport_width(),
                    ctx.renderer.viewport_height(),
                );
                let _ = ctx
                    .renderer
                    .set_view_desired_size(ViewId::AssetPreview, w, h);
            }
            ctx.renderer.set_active_view(view);
            if view == ViewId::AssetPreview {
                activate_preview_view(ctx.scene_edit);
            } else {
                // Leaving the preview for the scene: restore the authored exposure so an HDRI EV
                // sweep never bleeds into the scene tab (the preview workspace re-applies its EV on
                // return).
                if ctx.scene_edit.previewing() {
                    ctx.renderer.set_exposure(ctx.scene_edit.saved_exposure);
                }
                deactivate_preview_view(ctx.scene_edit);
            }
            Ok(SetActiveViewResult {
                view: view.wire().to_owned(),
            })
        },
    );

    reg.register::<CleanAssetsParams, CleanReport>(
        "clean-assets",
        "clean-assets [exclude...]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let exclude: Vec<Uuid> = params
                .exclude
                .unwrap_or_default()
                .iter()
                .map(|id| Uuid(id.parse::<u64>().unwrap_or(0)))
                .collect();
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let data = analyze_clean(scene, assets, &exclude);
            Ok(CleanReport {
                reclaimable_bytes: data.reclaimable_bytes,
                candidates: data
                    .candidates
                    .into_iter()
                    .map(|c| CleanCandidateDto {
                        id: WireUuid(c.id.value()),
                        path: c.path,
                        category: c.category.name().to_owned(),
                        bytes: c.bytes,
                        reason: c.reason,
                    })
                    .collect(),
            })
        },
    );

    reg.register::<DeleteUnusedParams, DeleteUnusedResult>(
        "delete-unused",
        "delete-unused {ids...} {confirm}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let ids: Vec<Uuid> = params
                .ids
                .iter()
                .map(|id| Uuid(id.parse::<u64>().unwrap_or(0)))
                .collect();
            ctx.renderer.wait_gpu_idle();
            ctx.assets.clear_asset_caches();
            let confirm = params.confirm.unwrap_or(false);
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let deleted = delete_unused(assets, scene, &ids, confirm)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(DeleteUnusedResult {
                deleted: deleted.deleted,
                reclaimed_bytes: deleted.reclaimed_bytes,
            })
        },
    );

    reg.register::<RenameAssetParams, AssetRef>(
        "rename-asset",
        "rename-asset {id|name, newName}",
        |ctx, params| {
            let selector = selector_string(&params.asset);
            if selector.is_empty() || params.name.is_empty() {
                return Err(Error::command("usage: rename-asset {id|name} {newName}"));
            }
            let by_id = selector.parse::<u64>().unwrap_or(0);
            let Some(id) = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .find(|entry| entry.id.value() == by_id || entry.name == selector)
                .map(|entry| entry.id)
            else {
                return Err(Error::command(format!("no asset '{selector}'")));
            };
            let renamed = ctx.assets.rename_asset(id, params.name.clone());
            debug_assert!(renamed, "selected asset remains catalogued");
            // Persist the new name to the durable sidecar so it survives a cold scan without a save.
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "rename-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(
                ctx.assets.catalog().find(id).expect("just renamed"),
            ))
        },
    );

    reg.register::<CreateAssetFolderParams, AssetList>(
        "create-asset-folder",
        "create-asset-folder {folder}",
        |ctx, params| {
            if !valid_folder_path(&params.folder) {
                return Err(Error::command(
                    "folder must be a non-empty path without empty segments",
                ));
            }
            if !has_folder(ctx.assets.catalog(), &params.folder) {
                ctx.assets.add_catalog_folder(params.folder.clone());
                ctx.scene_edit.scene_version += 1;
            }
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
        },
    );

    reg.register::<RenameAssetFolderParams, AssetList>(
        "rename-asset-folder",
        "rename-asset-folder {folder, name}",
        |ctx, params| {
            if !valid_folder_path(&params.name) {
                return Err(Error::command(
                    "folder path must be non-empty and cannot contain empty segments",
                ));
            }
            if !has_folder(ctx.assets.catalog(), &params.folder) {
                return Err(Error::command(format!(
                    "no asset folder '{}'",
                    params.folder
                )));
            }
            if params.folder == params.name {
                return Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()));
            }
            if is_folder_descendant(&params.name, &params.folder) {
                return Err(Error::command("asset folder cannot be moved inside itself"));
            }
            if has_folder(ctx.assets.catalog(), &params.name) {
                return Err(Error::command(format!(
                    "asset folder '{}' already exists",
                    params.name
                )));
            }
            let folders = ctx
                .assets
                .catalog_folders()
                .iter()
                .map(|folder| {
                    if *folder == params.folder || is_folder_descendant(folder, &params.folder) {
                        replace_folder_prefix(folder, &params.folder, &params.name)
                    } else {
                        folder.clone()
                    }
                })
                .collect();
            ctx.assets.replace_catalog_folders(folders);
            let touched = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| {
                    entry.folder == params.folder
                        || is_folder_descendant(&entry.folder, &params.folder)
                })
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            for id in &touched {
                let folder = ctx
                    .assets
                    .catalog()
                    .find(*id)
                    .map(|entry| replace_folder_prefix(&entry.folder, &params.folder, &params.name))
                    .expect("collected asset remains catalogued");
                let _ = ctx.assets.move_asset_to_folder(*id, folder);
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "rename-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
        },
    );

    reg.register::<DeleteAssetFolderParams, AssetList>(
        "delete-asset-folder",
        "delete-asset-folder {folder}",
        |ctx, params| {
            let mut removed = false;
            let mut folders = Vec::with_capacity(ctx.assets.catalog().folders.len());
            for folder in &ctx.assets.catalog().folders {
                if *folder == params.folder || is_folder_descendant(folder, &params.folder) {
                    removed = true;
                } else {
                    folders.push(folder.clone());
                }
            }
            if !removed {
                return Err(Error::command(format!(
                    "no asset folder '{}'",
                    params.folder
                )));
            }
            ctx.assets.replace_catalog_folders(folders);
            let touched = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| {
                    entry.folder == params.folder
                        || is_folder_descendant(&entry.folder, &params.folder)
                })
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            for id in &touched {
                let _ = ctx.assets.move_asset_to_folder(*id, String::new());
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "delete-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
        },
    );

    reg.register::<MoveAssetParams, AssetRef>(
        "move-asset",
        "move-asset {asset, folder?}",
        |ctx, params| {
            let index = resolve_asset_index(ctx, &params.asset)?;
            let folder = params.folder.clone().unwrap_or_default();
            if !folder.is_empty() && !has_folder(ctx.assets.catalog(), &folder) {
                return Err(Error::command(format!("no asset folder '{folder}'")));
            }
            let id = ctx.assets.catalog().entries[index].id;
            let moved = ctx.assets.move_asset_to_folder(id, folder);
            debug_assert!(moved, "selected asset remains catalogued");
            ctx.scene_edit.scene_version += 1;
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "move-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(
                ctx.assets.catalog().find(id).expect("just moved"),
            ))
        },
    );

    reg.register::<AssetUsagesParams, AssetUsagesResult>(
        "asset-usages",
        "asset-usages {asset}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.asset)?;
            let usages = collect_asset_usages(ctx.scene_edit.active_scene(), id);
            Ok(AssetUsagesResult { usages })
        },
    );

    reg.register::<AssetMetadataParams, AssetMetadataDto>(
        "probe-asset",
        "probe-asset {asset}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.asset)?;
            let entry = ctx
                .assets
                .catalog()
                .find(id)
                .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?
                .clone();
            let abs = ctx.assets.root.join(&entry.path);
            let size_bytes = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            let created_at = asset_created_at(&ctx.assets.root, &entry.path);
            let mut vertex_count = None;
            let mut triangle_count = None;
            if entry.asset_type == AssetType::Mesh
                && let Ok(counts) = ctx.assets.mesh_counts_for_asset(&entry)
            {
                vertex_count = Some(counts.vertex_count);
                triangle_count = Some(counts.index_count / 3);
            }
            Ok(AssetMetadataDto {
                id: WireUuid(entry.id.value()),
                name: entry.name.clone(),
                r#type: asset_type_dto(entry.asset_type),
                path: entry.path.clone(),
                folder: optional_folder(&entry.folder),
                size_bytes,
                vertex_count,
                triangle_count,
                created_at,
            })
        },
    );

    reg.register::<DeleteAssetParams, DeleteAssetResult>(
        "delete-asset",
        "delete-asset {asset}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let index = resolve_asset_index(ctx, &params.asset)?;
            let entry = ctx.assets.catalog().entries[index].clone();
            saffron_assets::remove_vegetation_map_package(ctx.assets, &entry)
                .map_err(|error| Error::command(error.to_string()))?;
            let cleared = clear_asset_usages(&mut ctx.scene_edit.scene, entry.id);
            let removed = ctx.assets.delete_asset_entry(entry.id);
            debug_assert!(removed.is_some(), "selected asset remains catalogued");
            let file_deleted = if entry.path.is_empty() {
                false
            } else {
                // Drop the co-located durable sidecar too (unless it's an embedded sub-asset,
                // whose `.smeta` is the shared model's — not ours to delete).
                if entry.container.value() == 0 {
                    let _ =
                        std::fs::remove_file(ctx.assets.root.join(format!("{}.smeta", entry.path)));
                }
                std::fs::remove_file(ctx.assets.root.join(&entry.path)).is_ok()
            };
            // The thumbnail cache is content-addressed and shared across assets/projects, so
            // a delete leaves its PNG for the eviction sweep — another asset may share it.
            ctx.scene_edit.scene_version += 1;
            Ok(DeleteAssetResult {
                id: WireUuid(entry.id.value()),
                name: entry.name.clone(),
                cleared,
                file_deleted,
            })
        },
    );

    reg.register::<AssignAssetParams, AssignAssetResult>(
        "assign-asset",
        "assign-asset {entity, slot:mesh|albedo|metallic-roughness, id|name}",
        |ctx, params| {
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            let selector = selector_string(&params.asset);
            let clearing = selector == "0" || selector.is_empty() || params.asset.id() == Some(0);
            let (assign_id, assign_name) = if clearing {
                (Uuid(0), String::new())
            } else if let Some(builtin) =
                BuiltinMesh::from_reserved_id(Uuid(selector_id(&params.asset)))
            {
                // A built-in primitive is referenced by its reserved id, not a catalog row —
                // its display name comes from the enum, not `catalog.find`.
                (builtin.reserved_id(), builtin.display_name().to_owned())
            } else {
                let id = resolve_asset(ctx, &params.asset)?;
                let name = ctx
                    .assets
                    .catalog()
                    .find(id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                (id, name)
            };
            let scene = ctx.scene_edit.active_scene();
            match params.slot {
                AssetSlotDto::Mesh => {
                    if !scene.has_component::<Mesh>(entity) {
                        let _ = scene.add_component(entity, Mesh::default());
                    }
                    let _ = scene.with_component_mut::<Mesh, _>(entity, |m| m.mesh = assign_id);
                }
                // Texture slots write a per-object override on the entity's material slot 0.
                // The packed ORM means `metallic-roughness` and `occlusion` share `ormTexture`.
                AssetSlotDto::Albedo => {
                    set_slot0_texture_override(scene, entity, "albedoTexture", assign_id);
                }
                AssetSlotDto::MetallicRoughness => {
                    set_slot0_texture_override(scene, entity, "ormTexture", assign_id);
                }
                AssetSlotDto::Normal => {
                    set_slot0_texture_override(scene, entity, "normalTexture", assign_id);
                }
                AssetSlotDto::Occlusion => {
                    set_slot0_texture_override(scene, entity, "ormTexture", assign_id);
                }
                AssetSlotDto::Emissive => {
                    set_slot0_texture_override(scene, entity, "emissiveTexture", assign_id);
                }
                AssetSlotDto::Height => {
                    set_slot0_texture_override(scene, entity, "heightTexture", assign_id);
                }
            }
            ctx.scene_edit.scene_version += 1;
            Ok(AssignAssetResult {
                id: WireUuid(assign_id.value()),
                name: assign_name,
                slot: params.slot,
            })
        },
    );

    reg.register::<MaterialCreateParams, MaterialCreateResult>(
        "material-create",
        "material-create {name} [from-entity]",
        |ctx, params| {
            let asset = default_material_asset();
            let name = if params.name.is_empty() {
                "Material".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &asset, &name, "")
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialAssignParams, MaterialAssignResult>(
        "material-assign",
        "material-assign {entity, material:id|name}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let selector = selector_string(&params.material);
            let clearing =
                selector == "0" || selector.is_empty() || params.material.id() == Some(0);
            let mat_id = if clearing {
                Uuid(0)
            } else {
                resolve_asset(ctx, &params.material)?
            };
            let scene = ctx.scene_edit.active_scene();
            // Assign to every mesh-bearing entity in the model's forest — the renderer reads
            // the material off the entity that carries the mesh, which on a multi-node model is
            // a child of the resolved container, not the container itself. A leaf selection
            // resolves to just itself; a non-mesh selection still takes the component directly.
            let mut targets = scene.model_mesh_entities(entity);
            if targets.is_empty() {
                targets.push(entity);
            }
            for target in targets {
                ensure_material_slot(scene, target);
                let _ = scene.with_component_mut::<MaterialSet, _>(target, |set| {
                    if set.slots.is_empty() {
                        set.slots.push(MaterialSlot::default());
                    }
                    for slot in &mut set.slots {
                        slot.material = mat_id;
                    }
                });
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialAssignResult {
                material: WireUuid(mat_id.value()),
            })
        },
    );

    reg.register::<MaterialImportParams, MaterialImportResultDto>(
        "material-import",
        "material-import {path} [name]",
        |ctx, params| {
            // Baking the material container is pure disk — no GPU uploader needed; the maps
            // load lazily from the container when the material is first rendered / previewed.
            let imported = import_material_folder(&mut *ctx.assets, &params.path, &params.name)
                .map_err(|e| Error::command(e.to_string()))?;
            if let Some(attribution) = params.attribution {
                let _ = ctx
                    .assets
                    .set_asset_attribution(imported.material, attribution_from_dto(attribution));
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialImportResultDto {
                id: WireUuid(imported.material.value()),
                roles: imported.roles,
            })
        },
    );

    reg.register::<EmptyParams, MaterialListResult>(
        "material-list",
        "material-list",
        |ctx, _params| {
            let materials = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| MaterialRefDto {
                    id: WireUuid(e.id.value()),
                    name: e.name.clone(),
                    folder: e.folder.clone(),
                })
                .collect();
            Ok(MaterialListResult { materials })
        },
    );

    reg.register::<MaterialGetParams, MaterialGetResult>(
        "material-get",
        "material-get {id|name}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let m = load_catalog_material_asset(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            let graph = load_catalog_material_asset_raw(ctx.assets, id)
                .ok()
                .filter(|raw| raw.graph.is_object())
                .map_or_else(|| json!({}), |raw| raw.graph);
            Ok(MaterialGetResult {
                id: WireUuid(id.value()),
                surface: material_surface_dto(&m.surface),
                blend: m.blend.clone(),
                unlit: m.unlit,
                base_color: vec4(m.base_color),
                metallic: m.metallic,
                roughness: m.roughness,
                emissive: vec3(m.emissive),
                emissive_strength: m.emissive_strength,
                height_scale: m.height_scale,
                height_mode: m.height_mode.as_wire().to_owned(),
                albedo_texture: WireUuid(m.albedo_texture.value()),
                orm_texture: WireUuid(m.orm_texture.value()),
                normal_texture: WireUuid(m.normal_texture.value()),
                emissive_texture: WireUuid(m.emissive_texture.value()),
                height_texture: WireUuid(m.height_texture.value()),
                vector_displacement_texture: WireUuid(m.vector_displacement_texture.value()),
                graph,
            })
        },
    );

    reg.register::<MaterialSchemaParams, MaterialSchemaResult>(
        "material-schema",
        "material-schema {id|name} — the material's exposed override parameters",
        |ctx, params| {
            // Validate the material resolves; the exposed set is the fixed übershader's for
            // now (the same list a `MaterialSet` slot's overrides validate against).
            resolve_asset(ctx, &params.material)?;
            let params = pbr_exposed_parameters()
                .into_iter()
                .map(|p| ExposedParamDto {
                    name: p.name.to_owned(),
                    kind: p.kind.as_wire().to_owned(),
                    default: p.default,
                })
                .collect();
            Ok(MaterialSchemaResult { params })
        },
    );

    reg.register::<MaterialUpdateParams, MaterialUpdateResult>(
        "material-update",
        "material-update {id} [baseColor metallic roughness emissive emissiveStrength]",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m = load_catalog_material_asset(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            if let Some(surface) = params.surface {
                m.surface = material_surface_from_dto(surface)?;
            }
            if let Some(base) = params.base_color {
                m.base_color = from_vec4(base);
            }
            if let Some(metallic) = params.metallic {
                m.metallic = metallic;
            }
            if let Some(roughness) = params.roughness {
                m.roughness = roughness;
            }
            if let Some(emissive) = params.emissive {
                m.emissive = from_vec3(emissive);
            }
            if let Some(strength) = params.emissive_strength {
                m.emissive_strength = strength;
            }
            if let Some(normal_strength) = params.normal_strength {
                m.normal_strength = normal_strength;
            }
            if let Some(height_scale) = params.height_scale {
                m.height_scale = height_scale;
            }
            if let Some(height_mode) = &params.height_mode {
                m.height_mode = HeightMode::from_wire(height_mode);
            }
            if let Some(tex) = params.albedo_texture {
                m.albedo_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.orm_texture {
                m.orm_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.normal_texture {
                m.normal_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.emissive_texture {
                m.emissive_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.height_texture {
                m.height_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.vector_displacement_texture {
                m.vector_displacement_texture = Uuid(tex.0);
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialUpdateResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<PreviewRenderParams, PreviewRenderResult>(
        "preview-render",
        "preview-render {material} [size]",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let size = params.size.unwrap_or(256);
            // Rendered through the main forward+ graph on the offscreen thumbnail view — the same
            // path the Assets tiles take — so the live preview pane matches a tile exactly
            // (displacement, procedural sky, floor, key light). A non-foldable graph shades through
            // its compiled `_mesh.spv` variant, a foldable one through its folded params.
            let bytes = ctx
                .renderer
                .render_material_preview_png(ctx.assets, PreviewSubject::Material(id), size)
                .map_err(Error::command)?;
            Ok(PreviewRenderResult {
                png: base64_encode(&bytes),
            })
        },
    );

    reg.register::<MaterialSetGraphParams, MaterialSetGraphResult>(
        "material-set-graph",
        "material-set-graph {material, graph}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m = load_catalog_material_asset(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            m.graph = params.graph.clone();
            let mut folded = m.clone();
            let foldable = lower_graph_to_params(&m.graph, &mut folded);
            if foldable {
                m = folded;
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            if !foldable {
                ctx.assets
                    .compile_material_mesh_shader(&m.graph, id)
                    .map_err(|e| Error::command(e.to_string()))?;
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetGraphResult {
                id: WireUuid(id.value()),
                foldable,
            })
        },
    );

    reg.register::<MaterialCreateInstanceParams, MaterialCreateResult>(
        "material-create-instance",
        "material-create-instance {parent} [name]",
        |ctx, params| {
            let parent = resolve_asset(ctx, &params.parent)?;
            let mut child = default_material_asset();
            child.parent = parent;
            let name = if params.name.is_empty() {
                "Instance".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &child, &name, "")
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialSetOverrideParams, MaterialSetOverrideResult>(
        "material-set-override",
        "material-set-override {material, field, value}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            // The override key must be one of the material's exposed parameters, and the
            // value must be well-typed for its kind — the schema is the single gate.
            let param = exposed_parameter(&params.field).ok_or_else(|| {
                Error::command(format!("unknown material parameter '{}'", params.field))
            })?;
            if !param.kind.accepts(&params.value) {
                return Err(Error::command(format!(
                    "material parameter '{}' expects a {} value",
                    params.field,
                    param.kind.as_wire()
                )));
            }
            let mut m = load_catalog_material_asset_raw(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            if !m.overrides.is_object() {
                m.overrides = json!({});
            }
            if let Some(map) = m.overrides.as_object_mut() {
                map.insert(params.field.clone(), params.value.clone());
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetOverrideResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<MaterialCompileParams, MaterialCompileResult>(
        "material-compile-graph",
        "material-compile-graph {material}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let raw = load_catalog_material_asset_raw(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                return Err(Error::command("material has no node graph to compile"));
            }
            ctx.assets
                .compile_material_graph(&raw.graph, id)
                .map_err(|e| Error::command(e.to_string()))?;
            let _ = ctx.assets.asset_edited(id);
            Ok(MaterialCompileResult {
                id: WireUuid(id.value()),
                ok: true,
            })
        },
    );

    reg.register::<EmptyParams, MaterialCookResult>(
        "material-cook",
        "material-cook",
        |ctx, _params| {
            let material_ids: Vec<Uuid> = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| e.id)
                .collect();
            let mut compiled = 0u32;
            let mut failed = 0u32;
            for id in material_ids {
                let Ok(raw) = load_catalog_material_asset_raw(ctx.assets, id) else {
                    continue;
                };
                if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                    continue;
                }
                let mut probe = raw.clone();
                if lower_graph_to_params(&raw.graph, &mut probe) {
                    continue;
                }
                if ctx
                    .assets
                    .compile_material_mesh_shader(&raw.graph, id)
                    .is_ok()
                {
                    let _ = ctx.assets.asset_edited(id);
                    compiled += 1;
                } else {
                    failed += 1;
                }
            }
            Ok(MaterialCookResult { compiled, failed })
        },
    );

    reg.register::<ExportAppParams, ExportAppResult>(
        "export-app",
        "export-app {outputDir, app} — cook the project into a standalone app folder",
        |ctx, params| export_app(ctx, &params),
    );

    reg.register::<PathParams, PathResult>("save-scene", "save-scene {path}", |ctx, params| {
        if params.path.is_empty() {
            return Err(Error::command("missing 'path'"));
        }
        let editor = &mut *ctx.scene_edit;
        editor
            .scene
            .write_scene(&editor.registry, &params.path)
            .map_err(|e| Error::command(e.to_string()))?;
        editor.scene_path = params.path.clone();
        Ok(PathResult { path: params.path })
    });

    reg.register::<PathParams, PathResult>("load-scene", "load-scene {path}", |ctx, params| {
        if ctx.scene_edit.play_state != PlayState::Edit {
            return Err(Error::command("stop play first"));
        }
        if params.path.is_empty() {
            return Err(Error::command("missing 'path'"));
        }
        {
            let editor = &mut *ctx.scene_edit;
            editor
                .scene
                .read_scene(&editor.registry, &params.path)
                .map_err(|e| Error::command(e.to_string()))?;
        }
        ctx.scene_edit.scene_path = params.path.clone();
        ctx.scene_edit.scene_version += 1;
        ctx.scene_edit.set_selection(Entity::NULL);
        Ok(PathResult { path: params.path })
    });

    reg.register::<OptionalPathParams, ProjectInfoDto>(
        "save-project",
        "save-project {path} — assets catalog + scene in one file",
        |ctx, params| {
            // The save barrier: every promoted plant's live transform and velocity reduces
            // through the vegetation reducer first, so the saved state is complete without
            // waiting for a falling object to settle.
            crate::commands_vegetation_runtime::flush_promoted_state(ctx)?;
            let mut path = params.path.clone().unwrap_or_default();
            let mut project = current_project_info(ctx);
            if path.is_empty() {
                path = project.path.clone();
            }
            if path.is_empty() {
                return Err(Error::command("no active project path"));
            }
            if !project.loaded {
                let fs_path = Path::new(&path);
                project.loaded = true;
                project.path = path.clone();
                let parent = fs_path.parent();
                project.root = match parent {
                    Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
                    _ => ".".to_owned(),
                };
                let dir_name = parent
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                project.name = if valid_project_name(&dir_name) {
                    dir_name
                } else {
                    "project".to_owned()
                };
                project.display_name = default_display_name(&project.name);
            }
            let sidecar = saffron_assets::ProjectSidecar {
                editor_camera: ctx.scene_edit.camera.to_json(),
                debug_overlays: saffron_sceneedit::debug_overlays_to_json(
                    &ctx.scene_edit.debug_overlays,
                ),
                stores: ctx.scene_edit.stores.clone(),
            };
            let host = RendererProjectHost {
                renderer: ctx.renderer,
            };
            ctx.assets
                .save_project(
                    &host,
                    &ctx.scene_edit.registry,
                    &mut ctx.scene_edit.scene,
                    &project,
                    &path,
                    &sidecar,
                )
                .map_err(|e| Error::command(e.to_string()))?;
            project.path = path;
            apply_project_info(ctx, &project);
            Ok(project_dto(&project))
        },
    );

    reg.register::<OptionalPathParams, ProjectStatusDto>(
        "load-project",
        "load-project {path} — assets catalog + scene",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let path = params
                .path
                .clone()
                .unwrap_or_else(|| "project.json".to_owned());
            load_project_into(ctx, &path);
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "reload-project",
        "reload-project — close and re-open the active project",
        |ctx, _params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            require_project_loaded(ctx)?;
            ctx.vegetation_cook_jobs.shutdown();
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Reload);
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<EmptyParams, ProjectStoresDto>(
        "get-stores",
        "get-stores — the project's enabled asset-store connectors",
        |ctx, _params| Ok(stores_dto_from_value(&ctx.scene_edit.stores)),
    );

    reg.register::<ProjectStoresDto, ProjectStoresDto>(
        "set-stores",
        "set-stores {enabled} — set the project's enabled asset-store connectors",
        |ctx, params| {
            require_project_loaded(ctx)?;
            ctx.scene_edit.stores =
                serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
            Ok(params)
        },
    );

    reg.register::<ScreenshotParams, ScreenshotResult>(
        "screenshot",
        "screenshot {target:viewport|window, path}",
        |ctx, params| {
            let target = params.target.unwrap_or(ScreenshotTargetDto::Viewport);
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            match target {
                ScreenshotTargetDto::Viewport => {
                    ctx.renderer
                        .capture_viewport(Path::new(&params.path))
                        .map_err(Error::Command)?;
                    Ok(ScreenshotResult {
                        target,
                        path: params.path,
                        pending: false,
                    })
                }
                ScreenshotTargetDto::Window => {
                    ctx.renderer
                        .request_window_capture(Path::new(&params.path))
                        .map_err(Error::Command)?;
                    Ok(ScreenshotResult {
                        target,
                        path: params.path,
                        pending: true,
                    })
                }
            }
        },
    );

    reg.register::<ThumbnailParams, ThumbnailResult>(
        "get-thumbnail",
        "get-thumbnail {asset:id|name, size=128} — base64 PNG preview",
        |ctx, params| thumbnail_result(ctx, &params, 128),
    );

    reg.register::<ThumbnailParams, ThumbnailResult>(
        "view-asset",
        "view-asset {asset:id|name, size=512} — larger base64 PNG preview",
        |ctx, params| thumbnail_result(ctx, &params, 512),
    );

    reg.register::<ThumbnailCacheParams, ThumbnailCacheResult>(
        "thumbnail-cache",
        "thumbnail-cache {action: stats|clear} — inspect or empty the app-level cache",
        |ctx, params| {
            if params.action == "clear" {
                let removed = ctx.assets.clear_thumbnail_cache_dir();
                return Ok(ThumbnailCacheResult {
                    entries: i32::try_from(removed.entries).unwrap_or(i32::MAX),
                    bytes: i64::try_from(removed.bytes).unwrap_or(i64::MAX),
                });
            }
            if params.action == "stats" || params.action.is_empty() {
                let stats = ctx.assets.thumbnail_cache_stats();
                return Ok(ThumbnailCacheResult {
                    entries: i32::try_from(stats.entries).unwrap_or(i32::MAX),
                    bytes: i64::try_from(stats.bytes).unwrap_or(i64::MAX),
                });
            }
            Err(Error::command(format!(
                "unknown action '{}' (stats|clear)",
                params.action
            )))
        },
    );

    reg.register::<EmptyParams, QuitResult>("quit", "close the running app", |ctx, _params| {
        ctx.window.request_close();
        Ok(QuitResult { quitting: true })
    });
}

/// Ensures the entity carries a [`MaterialSet`] with at least one slot before a slot write.
fn ensure_material_slot(scene: &mut Scene, entity: Entity) {
    if scene.has_component::<MaterialSet>(entity) {
        let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
            if set.slots.is_empty() {
                set.slots.push(MaterialSlot::default());
            }
        });
    } else {
        let _ = scene.add_component(
            entity,
            MaterialSet {
                slots: vec![MaterialSlot::default()],
            },
        );
    }
}

/// Sets (or clears, when `tex_id == 0`) one texture override on the entity's material slot 0,
/// attaching a default `MaterialSet` slot first. The `assign-asset` texture path — a
/// per-object override layered over the slot's referenced `.smat`.
fn set_slot0_texture_override(scene: &mut Scene, entity: Entity, key: &str, tex_id: Uuid) {
    ensure_material_slot(scene, entity);
    let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
        let Some(slot) = set.slots.first_mut() else {
            return;
        };
        if let Some(map) = slot.overrides.as_object_mut() {
            if tex_id.value() == 0 {
                map.remove(key);
            } else {
                map.insert(key.to_owned(), json!(tex_id.value().to_string()));
            }
        }
    });
}

/// The shared `load-project` body: seed the loader inbox with an `Open` request and set the
/// `Loading` phase, then return the current identity as an immediate ack. The non-blocking loader
/// (`ProjectLoader::advance`, driven from the host each frame) runs the read/parse/scan off-thread
/// and installs on the main thread, so the control drain never blocks.
fn load_project_into(ctx: &mut EngineContext<'_>, path: &str) {
    ctx.vegetation_cook_jobs.shutdown();
    ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(path.to_owned()));
    ctx.scene_edit.project_phase = ProjectPhase::Loading;
}

/// The `enter-asset-preview` body: build an isolated preview scene, commit it, furnish it
/// (floor / key light / procedural sky / framed fly-cam), and route the renderer + active
/// view to it.
fn enter_asset_preview(
    ctx: &mut EngineContext<'_>,
    params: GetAssetModelParams,
) -> Result<AssetPreviewResultWrap> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        return Err(Error::command("stop play first"));
    }
    // A native built-in primitive has no catalog row or model container — preview it on its
    // own geometry rather than resolving + instantiating a model.
    if let Some(builtin) = BuiltinMesh::from_reserved_id(Uuid(selector_id(&params.asset))) {
        return enter_builtin_preview(ctx, builtin);
    }
    let id = resolve_asset(ctx, &params.asset)?;
    let entry = ctx
        .assets
        .catalog()
        .find(id)
        .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?;
    let entry_type = entry.asset_type;
    let entry_role = entry.role;
    let entry_hdr = entry.hdr;
    // A standalone texture previews on the isolated sphere, not as a model: an HDRI lights + backs a
    // three-ball environment rig; every other role is its map on one lit sphere (role picks the slot).
    if entry_type == AssetType::Texture {
        if entry_role == TextureRole::Hdri || entry_hdr {
            return enter_hdri_preview(ctx, id);
        }
        return enter_texture_preview(ctx, id, entry_role);
    }
    // A material (`.smat`) previews as itself on the studio sphere — the same subject the
    // material-graph editor's live pane drives.
    if entry_type == AssetType::Material {
        return enter_material_preview(ctx, id);
    }
    // A plant family previews as its compiled renderable form on the studio floor.
    if entry_type == AssetType::Plant {
        return enter_plant_preview(ctx, id);
    }
    let container_id = if entry_type == AssetType::Model {
        id
    } else {
        entry.container
    };
    if container_id.value() == 0 {
        return Err(Error::command(format!(
            "asset {} is not part of a model container",
            id.value()
        )));
    }
    let model = ctx
        .assets
        .load_model_asset(container_id)
        .ok_or_else(|| Error::command(format!("model {} is not loadable", container_id.value())))?;
    let meta = model.meta.clone();

    // Build the preview scene locally so a failed swap stays on the prior model; commit only
    // once the model spawned a renderable mesh. Instantiation references mesh ids by uuid —
    // the GPU upload happens lazily at render — so no upload seam is needed here.
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = ctx
        .assets
        .instantiate_model(&mut preview, container_id, &meta.name)
        .map_err(|e| Error::command(e.to_string()))?;

    // A model is renderable when any entity in its forest carries a mesh — the meshes of a
    // multi-node forest ride child nodes, so probing only the resolved root rejects them.
    if !preview.model_has_renderable(root) {
        return Err(Error::command(format!(
            "model '{}' has no renderable mesh — re-import the asset",
            meta.name
        )));
    }
    let rig_entity = preview.model_rig_entity(root);
    // The animation authority (SkinnedMesh- or AnimationPlayer-bearing entity) the clip drives.
    let animatable = preview.animatable_descendant(root);

    // Open-from-clip: that clip becomes the active clip; the model opens paused at rest.
    if entry_type == AssetType::Animation && preview.has_component::<AnimationPlayer>(animatable) {
        let _ = preview.with_component_mut::<AnimationPlayer, _>(animatable, |player| {
            player.clip = id;
            player.time = 0.0;
            player.playing = false;
            player.preview_in_edit = false;
        });
    }

    let root_uuid = preview
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    let mut bone_by_node = Vec::new();
    let mut bones = Vec::new();
    if let Some(rig_entity) = rig_entity {
        let bone_uuids = preview
            .with_component::<SkinnedMesh, _>(rig_entity, |skin| skin.bones.clone())
            .unwrap_or_default();
        let joint_nodes: Vec<i32> = meta
            .skin
            .get("joints")
            .and_then(Value::as_array)
            .map(|joints| {
                joints
                    .iter()
                    .filter_map(|j| j.as_i64().map(|v| v as i32))
                    .collect()
            })
            .unwrap_or_default();
        let node_count = meta.nodes.as_array().map_or(0, Vec::len);
        bone_by_node = vec![Uuid(0); node_count];
        let joint_count = joint_nodes.len().min(bone_uuids.len());
        for k in 0..joint_count {
            let node_idx = joint_nodes[k];
            let uuid = bone_uuids[k];
            if node_idx >= 0 && (node_idx as usize) < node_count && uuid.value() != 0 {
                bone_by_node[node_idx as usize] = uuid;
                bones.push(saffron_protocol::BoneEntityDto {
                    index: node_idx,
                    entity: WireUuid(uuid.value()),
                });
            }
        }
    }

    // Furnish the instantiated model scene through the shared builder (floor / key light /
    // procedural sky / framed cam), then install it as the active preview (a rig keeps the bone
    // overlay on). A fresh enter makes the preview the active view; a swap keeps the authored stash.
    // A model is floor-standing geometry — default the floor on (the toggle still removes it).
    ctx.scene_edit.preview_show_floor = true;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut furnish = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        furnish = Some(furnish_preview_scene(&mut preview, assets, gpu, root, spec));
    });
    let furnish = furnish.expect("furnish ran");
    let (_root_uuid, framing) = install_preview_scene(
        ctx,
        preview,
        root,
        container_id,
        furnish,
        bone_by_node,
        true,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones,
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// Builds an isolated preview scene for a native built-in primitive (no catalog row, no
/// model container): a single entity carrying the reserved-id mesh + a default material,
/// framed on the shared asset-preview view. Mirrors the commit tail of
/// [`enter_asset_preview`] for a container-less, rig-less subject.
fn enter_builtin_preview(
    ctx: &mut EngineContext<'_>,
    builtin: BuiltinMesh,
) -> Result<AssetPreviewResultWrap> {
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = preview.create_entity(builtin.display_name());
    let _ = preview.add_component(
        root,
        Mesh {
            mesh: builtin.reserved_id(),
        },
    );
    let _ = preview.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot::default()],
        },
    );
    // A built-in primitive is floor-standing geometry — default the floor on.
    ctx.scene_edit.preview_show_floor = true;
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        builtin.reserved_id(),
        PreviewEnv::Procedural,
    ))
}

/// Compiles a plant family through its retained recipe (content-addressed — an
/// unchanged family republishes the identical artifact), registers its renderable
/// form under the family id, and spawns the floor-standing entity carrying the
/// family mesh with the family's material slots. Shared by the interactive plant
/// preview and the thumbnail subject build.
fn plant_preview_root(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    scene: &mut Scene,
    family: Uuid,
) -> Result<(Entity, saffron_assets::PlantFamilyRender)> {
    let plant = load_plant_family_asset(assets, family)
        .map_err(|error| Error::command(error.to_string()))?;
    let name = plant.name.clone();
    let options = PlantRecookOptions {
        limits: PlantCompileLimits::default(),
        versions: vegetation_cook_versions(),
        platform: portable_vegetation_platform_profile(None),
    };
    let published = match recook_plant_family(assets, &plant, &options).map_err(Error::from)? {
        PlantRecookOutcome::Published(published) => published,
        PlantRecookOutcome::Rejected(_) => {
            return Err(Error::command(
                "plant family does not validate — run plant-validate for diagnostics",
            ));
        }
    };
    let render = assets
        .load_plant_family(gpu, family, published.publication.content_hash)
        .ok_or_else(|| Error::command("plant family artifact did not load a renderable form"))?;
    let root = scene.create_entity(&name);
    let _ = scene.add_component(root, Mesh { mesh: family });
    let _ = scene.add_component(
        root,
        MaterialSet {
            slots: render
                .materials
                .iter()
                .map(|material| MaterialSlot {
                    material: *material,
                    ..MaterialSlot::default()
                })
                .collect(),
        },
    );
    Ok((root, render))
}

/// The `enter-asset-preview` branch for a plant family: [`plant_preview_root`] on an
/// isolated scene, committed with the floor on; the result carries the authored
/// combination domain the variation/phenotype scrub selects from.
fn enter_plant_preview(ctx: &mut EngineContext<'_>, id: Uuid) -> Result<AssetPreviewResultWrap> {
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let assets = &mut *ctx.assets;
    let mut built = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        built = Some(plant_preview_root(assets, gpu, &mut preview, id));
    });
    let (root, render) = built.expect("build ran")?;
    ctx.scene_edit.preview_show_floor = true;
    let mut result = commit_preview_subject(ctx, preview, root, id, PreviewEnv::Procedural);
    result.0.plant_combinations = render
        .combinations
        .iter()
        .map(
            |(variation, phenotype)| saffron_protocol::PlantCombinationDto {
                variation: *variation,
                phenotype: *phenotype,
            },
        )
        .collect();
    Ok(result)
}

/// The `enter-asset-preview` branch for a standalone texture: shade a preview sphere with an
/// ephemeral single-slot material carrying the texture in the slot its role feeds (albedo lit,
/// normal bumped, roughness/metallic/AO/ORM through the packed slot, height parallaxed, emissive
/// glowing). The material is seeded into `material_by_uuid` under the reserved
/// [`PREVIEW_MATERIAL_ID`] — no catalog row, rebuilt on each enter — so the scene resolver shades
/// the sphere with it. HDRI is filtered out upstream (it is the environment preview).
fn enter_texture_preview(
    ctx: &mut EngineContext<'_>,
    tid: Uuid,
    role: TextureRole,
) -> Result<AssetPreviewResultWrap> {
    let catalog = ctx.scene_edit.scene.catalog.clone();
    // A texture map previews on a floating surface sphere — default the floor off (non-model).
    ctx.scene_edit.preview_show_floor = false;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut build = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        build = Some(build_preview_scene(
            assets,
            gpu,
            catalog.clone(),
            PreviewSubject::TextureRole { tid, role },
            PREVIEW_MATERIAL_ID,
            spec,
        ));
    });
    let build = build.expect("build ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        build.scene,
        build.root,
        tid,
        build.furnish,
        Vec::new(),
        false,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones: Vec::new(),
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// The `enter-asset-preview` branch for an HDRI (an `.hdr`/`.exr` texture): the imported equirect
/// becomes both the visible backdrop and the IBL source (`SkyMode::Texture`), and three PBR balls —
/// chrome, diffuse grey, colored satin — read the environment's reflections / irradiance / color
/// response (the AmbientCG-style env rig). The balls differ only by per-slot overrides on the
/// default material, so no ephemeral material assets are needed. The center ball parents the other
/// two so the framing bounds cover all three.
fn enter_hdri_preview(
    ctx: &mut EngineContext<'_>,
    hdri_id: Uuid,
) -> Result<AssetPreviewResultWrap> {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    // (x offset, name, material overrides): chrome mirror / diffuse grey / colored satin.
    let balls = [
        (
            -2.3_f32,
            "Chrome",
            serde_json::json!({ "metallic": 1.0, "roughness": 0.04, "baseColor": [1.0, 1.0, 1.0, 1.0] }),
        ),
        (
            0.0_f32,
            "Diffuse",
            serde_json::json!({ "metallic": 0.0, "roughness": 1.0, "baseColor": [0.5, 0.5, 0.5, 1.0] }),
        ),
        (
            2.3_f32,
            "Satin",
            serde_json::json!({ "metallic": 0.0, "roughness": 0.4, "baseColor": [0.85, 0.5, 0.35, 1.0] }),
        ),
    ];
    let mut entities = Vec::with_capacity(balls.len());
    for (x, name, overrides) in &balls {
        let e = preview.create_entity(*name);
        let _ = preview.add_component(
            e,
            Mesh {
                mesh: BUILTIN_SPHERE_MESH_ID,
            },
        );
        let _ = preview.add_component(
            e,
            MaterialSet {
                slots: vec![MaterialSlot {
                    material: Uuid(0),
                    overrides: overrides.clone(),
                }],
            },
        );
        let _ = preview.with_component_mut::<Transform, _>(e, |t| {
            t.translation = GVec3::new(*x, 0.0, 0.0);
        });
        entities.push(e);
    }
    // The center ball is the framing root; the outer two parent to it so the AABB spans all three.
    let root = entities[1];
    for &e in &[entities[0], entities[2]] {
        let _ = preview.set_parent(e, Some(root), true);
    }
    // The HDRI env rig floats in its own equirect — no floor.
    ctx.scene_edit.preview_show_floor = false;
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        hdri_id,
        PreviewEnv::Hdri(hdri_id),
    ))
}

/// The `enter-asset-preview` branch for a material (`.smat`): a built-in sphere carrying that
/// material **by id**, in the shared procedural studio. Referencing by id (not a copy) is the join
/// point — a later `material-set-graph` / `material-update` mutates the `.smat` in the asset cache
/// and the sphere re-renders next frame, so the material-graph editor's live pane and a standalone
/// material "View" tab are one host path.
fn enter_material_preview(
    ctx: &mut EngineContext<'_>,
    mid: Uuid,
) -> Result<AssetPreviewResultWrap> {
    let catalog = ctx.scene_edit.scene.catalog.clone();
    // A material previews on a floating surface sphere, not floor-standing geometry — default the
    // floor off (the toggle still lets the user add one).
    ctx.scene_edit.preview_show_floor = false;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut build = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        build = Some(build_preview_scene(
            assets,
            gpu,
            catalog.clone(),
            PreviewSubject::Material(mid),
            PREVIEW_MATERIAL_ID,
            spec,
        ));
    });
    let build = build.expect("build ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        build.scene,
        build.root,
        mid,
        build.furnish,
        Vec::new(),
        false,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones: Vec::new(),
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// The ephemeral single-slot material the texture preview shades the sphere with: the texture in
/// the slot its role feeds, neutral factors elsewhere (neutral albedo = mid-grey), so the ball
/// reads the map the way a surface uses it. `Albedo`/`Opacity`/`Gloss`/`Unknown` fall back to the
/// base-color slot (show the map as a plain surface texture); HDRI never reaches here.
fn preview_material_for_texture(role: TextureRole, tid: Uuid) -> MaterialAsset {
    use saffron_geometry::glam::{Vec3 as GVec3, Vec4 as GVec4};
    let grey = |v: f32| GVec4::new(v, v, v, 1.0);
    let mut m = default_material_asset();
    m.metallic = 0.0;
    m.roughness = 0.6;
    match role {
        TextureRole::Normal => {
            m.normal_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Roughness => {
            m.orm_texture = tid;
            m.roughness = 1.0;
            m.base_color = grey(0.55);
        }
        TextureRole::Metallic => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 0.35;
            m.base_color = grey(0.8);
        }
        TextureRole::Ao => {
            m.orm_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Orm => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 1.0;
            m.base_color = grey(0.6);
        }
        TextureRole::Height => {
            // A bare height texture previews as parallax-occlusion mapping on the ordinary sphere —
            // never auto-routed to real displacement (that is a deliberate authored `.smat` choice
            // carrying a tessellation + BLAS cost). A Displacement-authored material bulges the same
            // sphere through the real tessellating path, so preview matches scene either way.
            m.height_texture = tid;
            m.height_scale = 0.05;
            m.height_mode = HeightMode::Parallax;
            m.base_color = grey(0.6);
        }
        TextureRole::Emissive => {
            m.emissive_texture = tid;
            m.emissive = GVec3::ONE;
            m.emissive_strength = 2.0;
            m.base_color = grey(0.02);
        }
        _ => {
            m.albedo_texture = tid;
            m.base_color = GVec4::ONE;
        }
    }
    m
}

/// Make the asset-preview view the active one on a *fresh* enter: stash the authored camera /
/// selection / overlay / exposure, size the preview view to the viewport, and switch the renderer's
/// active view. A swap (already previewing) keeps the authored stash and the active view.
fn activate_asset_preview_view(ctx: &mut EngineContext<'_>) {
    if ctx.scene_edit.previewing() {
        return;
    }
    ctx.scene_edit.saved_camera = ctx.scene_edit.camera;
    ctx.scene_edit.saved_selection = ctx.scene_edit.selected;
    ctx.scene_edit.saved_overlay = ctx.scene_edit.skeleton_overlay;
    ctx.scene_edit.saved_exposure = ctx.renderer.exposure_ev();
    ctx.scene_edit.preview_active_view = true;
    let (w, h) = (
        ctx.renderer.viewport_width(),
        ctx.renderer.viewport_height(),
    );
    let _ = ctx
        .renderer
        .set_view_desired_size(ViewId::AssetPreview, w, h);
    ctx.renderer.set_active_view(ViewId::AssetPreview);
}

/// Installs an already-furnished preview `scene` as the active preview subject: switch the view,
/// store the scene + framed camera + floor + rig state, select the root, and bump the versions.
/// Returns `(root uuid, framing)` for the wire result the caller assembles (with its own bones).
fn install_preview_scene(
    ctx: &mut EngineContext<'_>,
    scene: Scene,
    root: Entity,
    preview_asset: Uuid,
    furnish: PreviewFurnish,
    bone_by_node: Vec<Uuid>,
    overlay_show: bool,
) -> (u64, PreviewFraming) {
    let root_uuid = scene
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    activate_asset_preview_view(ctx);
    ctx.scene_edit.preview_scene = Some(scene);
    ctx.scene_edit.preview_asset = preview_asset;
    ctx.scene_edit.preview_root_entity = root;
    ctx.scene_edit.preview_bone_by_node = bone_by_node;
    ctx.scene_edit.preview_floor_entity = furnish.floor;
    ctx.scene_edit.skeleton_overlay.show = overlay_show;
    ctx.scene_edit.skeleton_overlay.highlight_joint = -1;
    ctx.scene_edit.camera = furnish.camera;
    ctx.scene_edit.set_selection(root);
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.animation_version += 1;
    (root_uuid, furnish.framing)
}

/// Builds a furnished, renderable preview scene for a sphere subject (a material by id, or a texture
/// map through an ephemeral single-slot material): the dense displacement sphere carrying the
/// subject, plus floor / key light / procedural sky / framed camera. Pure of the edit context —
/// the one builder shared by the interactive previewer and the background thumbnail render.
fn build_preview_scene(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    catalog: Option<std::sync::Arc<saffron_scene::AssetCatalog>>,
    subject: PreviewSubject,
    ephemeral_material_id: Uuid,
    spec: FurnishSpec,
) -> PreviewBuild {
    let mut scene = Scene::new();
    scene.catalog = catalog;
    let root = match subject {
        PreviewSubject::Material(mid) => {
            let name = assets
                .catalog()
                .find(mid)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Material".to_owned());
            let root = scene.create_entity(&name);
            attach_preview_sphere(&mut scene, root, mid);
            root
        }
        PreviewSubject::TextureRole { tid, role } => {
            let material = preview_material_for_texture(role, tid);
            assets.seed_preview_material(ephemeral_material_id, material);
            let name = assets
                .catalog()
                .find(tid)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Texture".to_owned());
            let root = scene.create_entity(&name);
            attach_preview_sphere(&mut scene, root, ephemeral_material_id);
            root
        }
        PreviewSubject::Mesh(mesh_id) => {
            let name = assets
                .catalog()
                .find(mesh_id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Mesh".to_owned());
            let root = scene.create_entity(&name);
            let _ = scene.add_component(root, Mesh { mesh: mesh_id });
            // The default material slot (`material: Uuid(0)`) resolves to the built-in default material.
            let _ = scene.add_component(
                root,
                MaterialSet {
                    slots: vec![MaterialSlot::default()],
                },
            );
            root
        }
        PreviewSubject::Model(model_id) => {
            let name = assets
                .catalog()
                .find(model_id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Model".to_owned());
            match assets.instantiate_model(&mut scene, model_id, name) {
                Ok(root) => root,
                Err(err) => {
                    tracing::warn!("preview: model {model_id:?} failed to instantiate: {err}");
                    Entity::NULL
                }
            }
        }
        PreviewSubject::Plant(family) => {
            match plant_preview_root(assets, gpu, &mut scene, family) {
                Ok((root, _)) => root,
                Err(err) => {
                    tracing::warn!("preview: plant {family:?} failed to build: {err}");
                    Entity::NULL
                }
            }
        }
        PreviewSubject::Hdri(_) => {
            // A mirror ball (metallic 1 / near-zero roughness) reflecting the HDRI env — the HDRI is
            // set as the scene's sky in `furnish_preview_scene` (`PreviewEnv::Hdri`), which also
            // backs the tile and drives the IBL prefilter the ball samples.
            let root = scene.create_entity("HDRI");
            let _ = scene.add_component(
                root,
                Mesh {
                    mesh: BUILTIN_SPHERE_MESH_ID,
                },
            );
            let _ = scene.add_component(
                root,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        material: Uuid(0),
                        overrides: json!({ "metallic": 1.0, "roughness": 0.04, "baseColor": [1.0, 1.0, 1.0, 1.0] }),
                    }],
                },
            );
            root
        }
    };
    let furnish = furnish_preview_scene(&mut scene, assets, gpu, root, spec);
    PreviewBuild {
        scene,
        root,
        furnish,
    }
}

/// Attach the ordinary low-poly builtin sphere + a single slot referencing `material_id`. A
/// displacement-enabled `.smat` gets its true bulged silhouette from the real tessellating path — the
/// same path a scene mesh uses — so no dense stand-in is needed and preview matches scene.
fn attach_preview_sphere(scene: &mut Scene, root: Entity, material_id: Uuid) {
    let _ = scene.add_component(
        root,
        Mesh {
            mesh: BUILTIN_SPHERE_MESH_ID,
        },
    );
    let _ = scene.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot {
                material: material_id,
                ..MaterialSlot::default()
            }],
        },
    );
}

/// Builds a furnished preview scene for an **offscreen thumbnail render** of `subject` — the public
/// entry the host drives for both the async Assets tiles and the sync `preview-render` pane. Returns
/// the scene, its root, and the framed orbit camera; the render goes through the main forward+ graph
/// on the [`saffron_assets::ViewId::Thumbnail`]-equivalent offscreen view, so a tile looks identical
/// to the interactive previewer (displacement + procedural sky + floor + key light). Frames from a
/// default camera (a square tile) and seeds a texture subject's synthetic material under
/// `ephemeral_material_id`, distinct from the interactive [`PREVIEW_MATERIAL_ID`] so a background
/// tile can render while a texture preview is open.
/// The environment + framing tightness a thumbnail subject furnishes with: a material / texture map
/// frames tight over the procedural sky with a little displacement headroom; an HDRI ball frames a
/// touch tighter (it is a smooth sphere) and backs itself with its own equirect; a model / mesh
/// frames looser so a 3/4 view of its bounding box does not clip.
fn thumbnail_subject_furnishing(subject: &PreviewSubject) -> (PreviewEnv, f32) {
    match subject {
        PreviewSubject::Material(_) | PreviewSubject::TextureRole { .. } => {
            (PreviewEnv::Procedural, THUMBNAIL_MATERIAL_FRAME_MARGIN)
        }
        PreviewSubject::Mesh(_) | PreviewSubject::Model(_) | PreviewSubject::Plant(_) => {
            (PreviewEnv::Procedural, THUMBNAIL_MODEL_FRAME_MARGIN)
        }
        PreviewSubject::Hdri(tid) => (PreviewEnv::Hdri(*tid), THUMBNAIL_CHROME_BALL_FRAME_MARGIN),
    }
}

pub fn build_preview_scene_for_thumbnail(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    subject: PreviewSubject,
    ephemeral_material_id: Uuid,
) -> (Scene, Entity, SceneEditCamera) {
    let (env, frame_margin) = thumbnail_subject_furnishing(&subject);
    let spec = FurnishSpec {
        base_cam: SceneEditCamera::default(),
        env,
        show_floor: false,
        frame_margin,
    };
    let build = build_preview_scene(assets, gpu, None, subject, ephemeral_material_id, spec);
    (build.scene, build.root, build.furnish.camera)
}

/// Commits a pre-built (unfurnished) container-less preview subject (a built-in primitive or the
/// HDRI ball rig): furnishes the scene through the shared builder, then installs it. The rig-less
/// commit tail of [`enter_asset_preview`].
fn commit_preview_subject(
    ctx: &mut EngineContext<'_>,
    mut preview: Scene,
    root: Entity,
    preview_asset: Uuid,
    env: PreviewEnv,
) -> AssetPreviewResultWrap {
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut furnish = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        furnish = Some(furnish_preview_scene(&mut preview, assets, gpu, root, spec));
    });
    let furnish = furnish.expect("furnish ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        preview,
        root,
        preview_asset,
        furnish,
        Vec::new(),
        false,
    );
    AssetPreviewResultWrap(saffron_protocol::AssetPreviewResult {
        root_entity: WireUuid(root_uuid),
        bones: Vec::new(),
        target: vec3(framing.target),
        distance: framing.distance,
        plant_combinations: Vec::new(),
    })
}

/// A newtype around [`AssetPreviewResult`](saffron_protocol::AssetPreviewResult) so the
/// `enter-asset-preview` handler can use a free fn (the closure form does not infer the
/// generic). It serializes transparently to the wire DTO.
#[derive(serde::Serialize)]
#[serde(transparent)]
pub struct AssetPreviewResultWrap(saffron_protocol::AssetPreviewResult);

/// The preview framing pivot + orbit distance.
#[derive(Clone, Copy)]
struct PreviewFraming {
    target: saffron_geometry::glam::Vec3,
    distance: f32,
}

/// What [`furnish_preview_scene`] produced: the framed camera, the floor entity (or `NULL`), and
/// the orbit framing — applied into the edit context by [`install_preview_scene`], or read directly
/// by a background thumbnail render.
struct PreviewFurnish {
    framing: PreviewFraming,
    camera: SceneEditCamera,
    floor: Entity,
}

/// How to furnish a preview scene: the base camera to frame from, the environment, whether to lay a
/// floor slab, and how tightly to frame the subject. One bundle shared by the interactive previewer
/// and the thumbnail render.
#[derive(Clone, Copy)]
struct FurnishSpec {
    base_cam: SceneEditCamera,
    env: PreviewEnv,
    show_floor: bool,
    /// Camera framing margin: how far past a snug fit the camera sits (`1.0` ≈ the bounding sphere
    /// touches the frame edge). The interactive previewer leaves generous room; a thumbnail pulls in
    /// tight so the subject fills the tile.
    frame_margin: f32,
}

/// The interactive previewer's framing margin — generous room around the subject for orbiting.
const INTERACTIVE_FRAME_MARGIN: f32 = 1.3;
/// A thumbnail's framing margin for a smooth chrome-ball subject (an HDRI reflection sphere): tight —
/// the AABB half-diagonal radius already over-frames a sphere by √3 — but with a little breathing room
/// so the ball does not touch the tile edges.
const THUMBNAIL_CHROME_BALL_FRAME_MARGIN: f32 = 0.72;
/// A thumbnail's framing margin for a displacement-sphere subject (a material or a texture role):
/// looser than the smooth ball because displacement pushes the silhouette outward at render time,
/// *after* the frame bounds are computed from the base mesh — so the base-sphere framing needs
/// headroom for the bulge, or a displaced material spills past the tile edges.
const THUMBNAIL_MATERIAL_FRAME_MARGIN: f32 = 0.85;
/// A thumbnail's framing margin for a model / mesh subject: still tighter than interactive, but with
/// enough room that a 3/4 view of an arbitrary bounding box does not clip its far corners.
const THUMBNAIL_MODEL_FRAME_MARGIN: f32 = 1.05;

/// A preview subject built into a throwaway scene and rendered through the main forward+ graph.
/// Every asset kind maps to one of these — the single thumbnail render path:
/// - `Material` / `TextureRole` shade the dense displacement sphere (a texture map through an
///   ephemeral single-slot material);
/// - `Mesh` shows a lone `.smesh` with the default material;
/// - `Model` instantiates the container's whole forest;
/// - `Hdri` is a chrome ball reflecting the HDRI, which also backs the scene.
pub enum PreviewSubject {
    Material(Uuid),
    TextureRole { tid: Uuid, role: TextureRole },
    Mesh(Uuid),
    Model(Uuid),
    Hdri(Uuid),
    Plant(Uuid),
}

/// A furnished, renderable preview scene built by [`build_preview_scene`].
struct PreviewBuild {
    scene: Scene,
    root: Entity,
    furnish: PreviewFurnish,
}

/// The previewed model's world-space bounding sphere from its mesh's rest-pose AABB.
pub(crate) struct PreviewBounds {
    center: saffron_geometry::glam::Vec3,
    radius: f32,
    min_y: f32,
}

/// The lighting/backdrop a preview subject is furnished with.
#[derive(Clone, Copy)]
enum PreviewEnv {
    /// A studio key light over the procedural sky (a model / a lone texture sphere).
    Procedural,
    /// An imported HDRI as both the visible backdrop and the IBL source (the environment rig).
    Hdri(Uuid),
}

/// Make the preview look like a preview: floor + key light + procedural sky, or the HDRI
/// environment; and frame the fly-cam. Pure of the edit context — operates on the passed `scene`
/// and a base camera, so both the interactive previewer and a background thumbnail render share
/// this one furnishing. Returns the framed camera, the floor entity, and the orbit framing.
fn furnish_preview_scene(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    root: Entity,
    spec: FurnishSpec,
) -> PreviewFurnish {
    use saffron_geometry::glam::Vec3 as GVec3;

    let bounds = compute_preview_bounds(scene, assets, gpu, root);
    // The HDRI environment is its own backdrop — no floor slab under the rig.
    let floor = if spec.show_floor && matches!(spec.env, PreviewEnv::Procedural) {
        spawn_preview_floor(scene, assets, gpu, &bounds)
    } else {
        Entity::NULL
    };

    match spec.env {
        PreviewEnv::Procedural => {
            let light = scene.create_entity("PreviewLight");
            let _ = scene.add_component(
                light,
                DirectionalLight {
                    direction: GVec3::new(-0.4, -1.0, -0.5).normalize(),
                    color: GVec3::ONE,
                    intensity: 3.0,
                    ambient: 0.25,
                    ..Default::default()
                },
            );
            scene.environment.sky_mode = SkyMode::Procedural;
            scene.environment.use_sky_for_ambient = true;
            scene.environment.ambient_intensity = 0.3;
        }
        PreviewEnv::Hdri(id) => {
            // The HDRI both lights (IBL prefilter of the equirect) and backs the scene — no
            // directional key light, so the balls read the environment's own illumination.
            scene.environment.sky_mode = SkyMode::Texture;
            scene.environment.sky_texture = id;
            scene.environment.sky_intensity = 1.0;
            scene.environment.use_sky_for_ambient = true;
            scene.environment.ambient_intensity = 1.0;
        }
    }

    let camera = frame_preview_camera(spec.base_cam, &bounds, spec.frame_margin);
    let fovy = camera.fov.to_radians();
    PreviewFurnish {
        framing: PreviewFraming {
            target: bounds.center,
            distance: bounds.radius / (fovy * 0.5).tan() * spec.frame_margin,
        },
        camera,
        floor,
    }
}

/// The previewed model's world-space bounding sphere. Pure of the edit context.
pub(crate) fn compute_preview_bounds(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    root: Entity,
) -> PreviewBounds {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut out = PreviewBounds {
        center: GVec3::ZERO,
        radius: 1.0,
        min_y: 0.0,
    };
    // The whole forest's world AABB — every mesh-bearing node, skinned through the joint
    // palette — not a single resolved entity's box.
    if !scene.valid(root) {
        return out;
    }
    let Some((lo, hi)) = model_render_aabb(gpu, scene, assets, root) else {
        // No resolvable mesh: fall back to the model root's position.
        out.center = scene.world_translation(root);
        out.min_y = out.center.y - 1.0;
        return out;
    };
    out.center = (lo + hi) * 0.5;
    out.radius = (hi - lo).length() * 0.5;
    out.min_y = lo.y;
    if out.radius <= 0.0001 {
        out.radius = 1.0;
    }
    out
}

/// A thin floor slab centered under the model's feet. Pure of the edit context.
pub(crate) fn spawn_preview_floor(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    bounds: &PreviewBounds,
) -> Entity {
    use saffron_geometry::glam::Vec3 as GVec3;

    if !assets.ensure_preview_floor_mesh(gpu) {
        return Entity::NULL;
    }
    let floor = scene.create_entity("PreviewFloor");
    let _ = scene.add_component(
        floor,
        Mesh {
            mesh: saffron_assets::PREVIEW_FLOOR_MESH_ID,
        },
    );
    let _ = scene.add_component(
        floor,
        MaterialSet {
            slots: vec![MaterialSlot {
                overrides: json!({
                    "baseColor": [0.32, 0.33, 0.35, 1.0],
                    "roughness": 0.92,
                    "metallic": 0.0,
                }),
                ..MaterialSlot::default()
            }],
        },
    );
    let span = (bounds.radius * 8.0).max(0.5);
    let thickness = (bounds.radius * 0.08).max(0.02);
    let _ = scene.with_component_mut::<Transform, _>(floor, |t| {
        t.translation = GVec3::new(
            bounds.center.x,
            bounds.min_y - thickness * 0.5,
            bounds.center.z,
        );
        t.scale = GVec3::new(span, thickness, span);
    });
    floor
}

/// Aim a fly-cam at the model: a 3/4 view fit to its bounding sphere, `margin` past a snug fit.
/// Starts from the current camera so the user's fov/near/far survive.
fn frame_preview_camera(
    mut cam: SceneEditCamera,
    bounds: &PreviewBounds,
    margin: f32,
) -> SceneEditCamera {
    use saffron_geometry::glam::Vec3 as GVec3;

    let fovy = cam.fov.to_radians();
    let distance = bounds.radius / (fovy * 0.5).tan() * margin;
    let eye = bounds.center + GVec3::new(1.0, 0.7, 1.0).normalize() * distance;
    let forward = (bounds.center - eye).normalize();
    cam.position = eye;
    cam.pitch = forward.y.clamp(-1.0, 1.0).asin().to_degrees();
    cam.yaw = forward.x.atan2(-forward.z).to_degrees();
    cam.far_plane = cam.far_plane.max(distance + bounds.radius * 4.0);
    cam.near_plane = (distance * 0.01).clamp(1e-4, 0.1);
    // Frame into orbit mode about the model centre so preview drags sweep the arc; the framed
    // pose shows at once (sync_target snaps pivot/distance/angles, no ease from the prior pose).
    cam.orbit = Some(OrbitState {
        pivot: bounds.center,
        distance,
        target_pivot: bounds.center,
        target_distance: distance,
    });
    cam.sync_target();
    cam
}

/// Builds the [`PlayStateResult`] from the editor state.
fn play_state_result(ctx: &EngineContext<'_>) -> PlayStateResult {
    let editor = &ctx.scene_edit;
    PlayStateResult {
        state: editor.play_state.name().to_owned(),
        play_version: i32::try_from(editor.play_version).unwrap_or(i32::MAX),
        scene_version: i32::try_from(editor.scene_version).unwrap_or(i32::MAX),
        has_primary_camera: editor.had_primary_camera,
        animation_version: i32::try_from(editor.animation_version).unwrap_or(i32::MAX),
        preview_asset: WireUuid(editor.preview_asset.value()),
    }
}

/// Converts a glam `Vec3` into the wire `Vec3`.
fn vec3(v: saffron_geometry::glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Converts a glam `Vec4` into the wire `Vec4`.
fn vec4(v: saffron_geometry::glam::Vec4) -> Vec4 {
    Vec4 {
        x: v.x,
        y: v.y,
        z: v.z,
        w: v.w,
    }
}

/// Converts a wire `Vec3` into a glam vector.
fn from_vec3(v: Vec3) -> saffron_geometry::glam::Vec3 {
    saffron_geometry::glam::Vec3::new(v.x, v.y, v.z)
}

/// Converts a wire `Vec4` into a glam vector.
fn from_vec4(v: Vec4) -> saffron_geometry::glam::Vec4 {
    saffron_geometry::glam::Vec4::new(v.x, v.y, v.z, v.w)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use saffron_assets::{
        default_material_asset, save_biome_asset, save_material_asset, save_plant_family_asset,
    };
    use saffron_protocol::PlantSourceSelectorDto;
    use saffron_scene::{AssetEntry, AssetType, MaterialSet, Mesh, VegetationField};
    use saffron_sceneedit::ProjectPhase;
    use saffron_spatial::{DecisionScalar, UnitInterval};
    use saffron_vegetation::{
        BIOME_ASSET_VERSION, BiomeAsset, BiomeGraphPolicy, BiomePaletteEntry, BiomeRole,
        BotanicalGraphDocument, InteractionPolicy, MechanicalResponse, PLANT_ASSET_VERSION,
        PhenotypeRole, PlantDimensions, PlantFamilyAsset, PlantFamilySource, PlantPart,
        PlantPartSemantic, PlantPhenotype, PlantVariation,
    };
    use serde_json::json;

    use crate::registry::{CommandRegistry, EngineContext, register_builtin_commands};
    use crate::selector::entity_uuid;
    use crate::test_support::{StubRenderer, with_stub};

    use super::plant_source_reference_dto;

    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    /// Roots the test's asset server at a unique scratch dir so the material file-writing
    /// commands never collide across parallel tests.
    fn scratch_root(ctx: &mut EngineContext<'_>, tag: &str) {
        let dir = std::env::temp_dir().join(format!(
            "saffron-control-asset-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ctx.assets.set_asset_root(dir.join("assets"));
    }

    /// Seeds a mesh catalog row, returning its decimal-string id.
    fn seed_mesh(ctx: &mut EngineContext<'_>, name: &str) -> u64 {
        let id = saffron_core::Uuid::new();
        ctx.assets.register_imported_asset(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: format!("models/{}.smesh", id.value()),
            ..AssetEntry::default()
        });
        id.value()
    }

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).expect("fixture scalar")
    }

    fn seed_native_plant(ctx: &mut EngineContext<'_>) -> u64 {
        let material = save_material_asset(ctx.assets, &default_material_asset(), "Leaf", "plants")
            .expect("save plant material");
        let plant = PlantFamilyAsset {
            role: saffron_vegetation::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id: saffron_core::Uuid(9_100),
            name: "Oak".to_owned(),
            tags: Vec::new(),
            source: PlantFamilySource::Native {
                graph: BotanicalGraphDocument::sapling(0x5a11),
                grafts: Vec::new(),
            },
            parts: vec![PlantPart {
                id: 12,
                parent: None,
                semantic: PlantPartSemantic::Trunk,
                material_slot: 0,
                sources: Vec::new(),
            }],
            dimensions: PlantDimensions {
                height: fixed(8),
                trunk_radius: fixed(1),
                crown_radius: [fixed(3); 2],
                root_radius: [fixed(4); 2],
                local_bounds_min: [fixed(-4), fixed(0), fixed(-4)],
                local_bounds_max: [fixed(4), fixed(8), fixed(4)],
            },
            material_slots: vec![material],
            spines: Vec::new(),
            mechanics: MechanicalResponse {
                stiffness: fixed(2),
                damping: UnitInterval::from_bits(1),
                drag: fixed(1),
                flutter: DecisionScalar::from_bits(1),
                bend_limit: UnitInterval::from_bits(2),
                damage_threshold: fixed(3),
                break_threshold: fixed(4),
            },
            variations: vec![PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: vec![saffron_vegetation::native_variation_source_id(0)],
                active_parts: Vec::new(),
            }],
            phenotypes: vec![PlantPhenotype {
                id: 0,
                role: PhenotypeRole::Healthy,
                season_window: None,
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Structural,
            habitat: None,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        };
        save_plant_family_asset(ctx.assets, plant, "Oak", "plants")
            .expect("save plant")
            .value()
    }

    fn seed_biome(ctx: &mut EngineContext<'_>, plant: u64) -> u64 {
        let biome = BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id: saffron_core::Uuid(9_101),
            name: "Forest".to_owned(),
            role: BiomeRole::Root,
            parameters: Vec::new(),
            palette: vec![BiomePaletteEntry {
                plant: saffron_core::Uuid(plant),
                weight: UnitInterval::ONE,
                seed_namespace: 23,
            }],
            density: fixed(1),
            clustering: UnitInterval::from_bits(24),
            suitability: Vec::new(),
            competition: Vec::new(),
            companions: Vec::new(),
            succession: Vec::new(),
            seed_namespaces: vec![("canopy".to_owned(), 23)],
            modules: Vec::new(),
            policy: BiomeGraphPolicy {
                maximum_recursion: 8,
                maximum_influence_radius: fixed(64),
                require_authoritative_fields: true,
            },
            graph: serde_json::Value::Object(Default::default()),
        };
        save_biome_asset(ctx.assets, biome, "Forest", "biomes")
            .expect("save biome")
            .value()
    }

    /// `list-assets` and `scan-assets` round-trip on an empty (just-loaded) project.
    #[test]
    fn list_and_scan_on_empty_project() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "empty");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;

            let list = reg.dispatch(ctx, &json!({ "cmd": "list-assets" }));
            assert_eq!(list["ok"], json!(true));
            assert_eq!(list["result"]["assets"], json!([]));
            assert_eq!(list["result"]["folders"], json!([]));

            let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
            assert_eq!(scan["ok"], json!(true));
            assert_eq!(scan["result"]["added"], json!(0));
            assert_eq!(scan["result"]["removed"], json!(0));
        });
    }

    /// `plant-phenotypes` reads a family's appearances and replaces them as one operation,
    /// refusing a set the family validator will not accept.
    #[test]
    fn plant_phenotypes_reads_replaces_and_refuses_an_invalid_set() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "phenotypes");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let plant = seed_native_plant(ctx);

            // A scaffolded family carries exactly one appearance, which is why nothing could
            // author a transition before this command existed.
            let read = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(read["ok"], json!(true));
            assert_eq!(
                read["result"]["phenotypes"].as_array().map(Vec::len),
                Some(1)
            );
            assert_eq!(read["result"]["phenotypes"][0]["role"], json!("healthy"));

            // A second appearance on the same variation with a different role is exactly what a
            // seasonal transition needs, and what the family validator allows.
            let replaced = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "plant-phenotypes",
                    "params": {
                        "plant": plant.to_string(),
                        "phenotypes": [
                            { "id": 0, "role": "healthy", "variation": 0 },
                            { "id": 1, "role": "senescent", "variation": 0, "seasonWindow": [700, 900] },
                        ],
                    },
                }),
            );
            assert_eq!(replaced["ok"], json!(true));
            assert_eq!(
                replaced["result"]["phenotypes"].as_array().map(Vec::len),
                Some(2)
            );

            // It persisted rather than only being echoed: a later read sees the new set.
            let again = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(again["result"]["phenotypes"][1]["role"], json!("senescent"));
            assert_eq!(
                again["result"]["phenotypes"][1]["seasonWindow"],
                json!([700, 900])
            );

            // And the validator is the authority: a family with no healthy appearance is refused,
            // and refusing means the stored set did not change.
            let refused = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "plant-phenotypes",
                    "params": {
                        "plant": plant.to_string(),
                        "phenotypes": [{ "id": 0, "role": "senescent", "variation": 0 }],
                    },
                }),
            );
            assert_eq!(refused["ok"], json!(false));
            let unchanged = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-phenotypes", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(
                unchanged["result"]["phenotypes"].as_array().map(Vec::len),
                Some(2),
                "a refused replacement still changed the family"
            );
        });
    }

    /// `plant-season-phenotype` resolves through the engine's own rule: lifecycle first, then the
    /// seasonal window, with the cooked phenotype as the fallback.
    #[test]
    fn plant_season_phenotype_follows_lifecycle_then_season() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "season");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let plant = seed_native_plant(ctx);
            reg.dispatch(
                ctx,
                &json!({
                    "cmd": "plant-phenotypes",
                    "params": {
                        "plant": plant.to_string(),
                        "phenotypes": [
                            { "id": 0, "role": "healthy", "variation": 0 },
                            { "id": 1, "role": "senescent", "variation": 0, "seasonWindow": [600, 800] },
                        ],
                    },
                }),
            );
            let mut at = |mille: u32, lifecycle: Option<&str>| {
                let mut params = json!({ "plant": plant.to_string(), "seasonMille": mille });
                if let Some(state) = lifecycle {
                    params["lifecycle"] = json!(state);
                }
                reg.dispatch(
                    ctx,
                    &json!({ "cmd": "plant-season-phenotype", "params": params }),
                )
            };

            // Inside the authored window the senescent appearance wins; outside it the healthy one
            // does. Both directions, because a resolver stuck on either answer passes one of them.
            assert_eq!(at(700, None)["result"]["phenotype"], json!(1));
            assert_eq!(at(100, None)["result"]["phenotype"], json!(0));

            // Lifecycle overrides the season: a dead plant in autumn is dead, not autumnal. With no
            // dead appearance authored it falls back to the cooked one rather than taking the
            // seasonal match, which is the part a season-first resolver gets wrong.
            assert_eq!(at(700, Some("dead"))["result"]["phenotype"], json!(0));

            // And the range is guarded rather than wrapped silently.
            assert_eq!(at(1000, None)["ok"], json!(false));
        });
    }

    /// `plant-proxies` reports the derived proxies in metres, which is what the overlay draws.
    #[test]
    fn plant_proxies_reports_what_the_family_derived() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "proxies");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let plant = seed_native_plant(ctx);
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-proxies", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(true));

            // Proxies are derived, so a family that grew nothing thick enough legitimately reports
            // none. What must hold either way is the SHAPE of the reply — the overlay reads these
            // fields directly, and a missing array would draw nothing while looking fine.
            assert!(reply["result"]["collision"].is_array());
            assert!(reply["result"]["navigation"].is_array());
            for proxy in reply["result"]["collision"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                // Metres, not Q15.16 bits: a dimension in raw fixed point would draw a capsule
                // sixty-five thousand times too big and look like a broken overlay rather than a
                // unit mistake.
                let dimensions = proxy["dimensionsM"].as_array().expect("dimensions");
                assert_eq!(dimensions.len(), 3);
                assert!(
                    dimensions
                        .iter()
                        .all(|value| value.as_f64().is_some_and(|v| v < 1.0e4))
                );
            }
        });
    }

    /// `scan-assets` (and the other project-gated commands) refuse without a loaded project.
    #[test]
    fn scan_assets_requires_a_project() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
            assert_eq!(scan["ok"], json!(false));
            assert_eq!(scan["error"]["message"], json!("no project loaded"));
        });
    }

    /// `assign-asset` resolves an `AssetSelector` by id and by name, assigns the mesh slot,
    /// and returns a decimal-string id matching the catalog row.
    #[test]
    fn assign_asset_resolves_id_and_name() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let mesh_id = seed_mesh(ctx, "cube");
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            // By numeric id.
            let by_id = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": mesh_id.to_string() } }),
            );
            assert_eq!(by_id["ok"], json!(true));
            assert_eq!(by_id["result"]["id"], json!(mesh_id.to_string()));
            assert_eq!(by_id["result"]["slot"], json!("mesh"));
            assert_eq!(by_id["result"]["name"], json!("cube"));
            assert_eq!(
                ctx.scene_edit
                    .active_scene()
                    .component::<Mesh>(entity)
                    .unwrap()
                    .mesh
                    .value(),
                mesh_id
            );

            // By name selector.
            let by_name = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "cube" } }),
            );
            assert_eq!(by_name["result"]["id"], json!(mesh_id.to_string()));

            // The null sentinel (id 0) clears the slot rather than resolving an asset.
            let clear = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "0" } }),
            );
            assert_eq!(clear["result"]["id"], json!("0"));
            assert_eq!(
                ctx.scene_edit
                    .active_scene()
                    .component::<Mesh>(entity)
                    .unwrap()
                    .mesh
                    .value(),
                0
            );
        });
    }

    /// `assign-asset` on a texture slot attaches a `Material` and writes the texture id.
    #[test]
    fn assign_asset_albedo_overrides_slot_zero() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let tex_id = {
                let id = saffron_core::Uuid::new();
                ctx.assets.register_imported_asset(AssetEntry {
                    id,
                    name: "albedo".to_owned(),
                    asset_type: AssetType::Texture,
                    ..AssetEntry::default()
                });
                id.value()
            };
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "albedo", "asset": tex_id.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["slot"], json!("albedo"));
            // A MaterialSet with a default slot is attached, and the albedo texture lands as
            // a per-object override (a decimal-string uuid) on slot 0.
            let albedo = ctx
                .scene_edit
                .active_scene()
                .with_component::<MaterialSet, _>(entity, |set| {
                    set.slots
                        .first()
                        .and_then(|s| s.overrides.as_object())
                        .and_then(|o| o.get("albedoTexture"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .ok()
                .flatten();
            assert_eq!(albedo.as_deref(), Some(tex_id.to_string().as_str()));
        });
    }

    /// `set-active-view` maps `scene` / `assetPreview` and errors on an unknown view.
    #[test]
    fn set_active_view_maps_and_errors() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scene = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "scene" } }),
            );
            assert_eq!(scene["ok"], json!(true));
            assert_eq!(scene["result"]["view"], json!("scene"));

            let preview = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "assetPreview" } }),
            );
            assert_eq!(preview["result"]["view"], json!("assetPreview"));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "nope" } }),
            );
            assert_eq!(bad["ok"], json!(false));
            assert_eq!(
                bad["error"]["message"],
                json!("unknown view 'nope' (expected 'scene' or 'assetPreview')")
            );
        });
    }

    /// `rename-asset` renames the catalog row and returns the new `{id, name}`.
    #[test]
    fn rename_asset_round_trips() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let id = seed_mesh(ctx, "old-name");
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "rename-asset", "params": { "asset": id.to_string(), "name": "new-name" } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["id"], json!(id.to_string()));
            assert_eq!(reply["result"]["name"], json!("new-name"));
            assert_eq!(
                ctx.assets
                    .catalog()
                    .find(saffron_core::Uuid(id))
                    .unwrap()
                    .name,
                "new-name"
            );
        });
    }

    /// `rename-asset` persists the new name to a durable `<path>.smeta` sidecar, so it survives a
    /// cold scan without a project save. Asserts the on-disk sidecar (not a rescan): `with_stub`
    /// reuses one `AssetServer`, so `preserve_name_folder` would mask a regression — the true
    /// cold-scan proof lives in the assets-crate unit test.
    #[test]
    fn rename_asset_writes_a_durable_smeta() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "renamesmeta");
            // A texture row + its directory so the sidecar writer's parent exists.
            let tex = saffron_core::Uuid::new();
            let rel = format!("textures/{}.png", tex.value());
            std::fs::create_dir_all(ctx.assets.root.join("textures")).unwrap();
            ctx.assets.register_imported_asset(AssetEntry {
                id: tex,
                name: "old".to_owned(),
                asset_type: AssetType::Texture,
                path: rel.clone(),
                ..AssetEntry::default()
            });

            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "rename-asset", "params": { "asset": tex.value().to_string(), "name": "Brick Wall" } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["name"], json!("Brick Wall"));

            let smeta = ctx.assets.root.join(format!("{rel}.smeta"));
            assert!(smeta.exists(), "rename writes the sidecar beside the file");
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&smeta).unwrap()).unwrap();
            assert_eq!(doc["name"], json!("Brick Wall"));
        });
    }

    /// `create-asset-folder` adds a folder and `list-assets` reflects it; an invalid path
    /// errors.
    #[test]
    fn create_asset_folder_and_list() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let made = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-asset-folder", "params": { "folder": "props/crates" } }),
            );
            assert_eq!(made["ok"], json!(true));
            assert_eq!(made["result"]["folders"], json!(["props/crates"]));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-asset-folder", "params": { "folder": "/leading" } }),
            );
            assert_eq!(bad["ok"], json!(false));
        });
    }

    /// `material-create` then `material-get` round-trips the `.smat`; `material-set-graph`
    /// stores an opaque graph that `material-get` reads back verbatim.
    #[test]
    fn material_set_graph_keeps_graph_opaque() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "material-graph");
            let create = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-create", "params": { "name": "Mat" } }),
            );
            assert_eq!(create["ok"], json!(true));
            let id = create["result"]["id"].as_str().unwrap().to_owned();

            // A graph object with a codegen-only shape (no fold) is stored verbatim.
            let graph = json!({ "nodes": [{ "id": 1, "type": "noise" }], "edges": [] });
            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-set-graph", "params": { "material": id, "graph": graph } }),
            );
            assert_eq!(set["ok"], json!(true), "set-graph: {set:?}");
            assert_eq!(set["result"]["id"], json!(id));

            let get = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-get", "params": { "material": id } }),
            );
            assert_eq!(get["ok"], json!(true), "get: {get:?}");
            assert_eq!(get["result"]["graph"], graph, "graph round-trips opaque");
        });
    }

    #[test]
    fn material_surface_union_round_trips_complete_thin_sheet_parameters() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "material-thin-sheet");
            let create = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-create", "params": { "name": "Leaf" } }),
            );
            let id = create["result"]["id"].as_str().unwrap();
            let surface = json!({
                "model": "thin-sheet-foliage",
                "parameters": {
                    "frontAlbedoResponse": 20_000,
                    "backAlbedoResponse": 21_000,
                    "thicknessBits": 655,
                    "absorptionColorBits": [1_000, 2_000, 3_000],
                    "transmissionColorBits": [10_000, 11_000, 12_000],
                    "roughness": 32_768,
                    "normalBehavior": "face-forward-back",
                    "coverageSource": { "kind": "albedo-alpha" },
                    "coverage": {
                        "referenceCutoff": 30_000,
                        "sourceExtent": [512, 256],
                        "spatialHashSalt": "9876543210987654321",
                        "classification": "masked",
                        "mipHashes": []
                    },
                    "voxelMoments": {
                        "occupancy": 20_000,
                        "albedoMeanBits": [4_000, 5_000, 6_000],
                        "roughnessMean": 30_000,
                        "transmissionMeanBits": [7_000, 8_000, 9_000],
                        "thicknessMeanBits": 327,
                        "normalSecondMomentsBits": [1, 2, 3, 4, 5, 6]
                    },
                    "opacityMicromap": {
                        "enabled": true,
                        "maxSubdivision": 5,
                        "transparentThreshold": 1_000,
                        "opaqueThreshold": 60_000
                    },
                    "energyLimit": 50_000
                }
            });
            let update = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-update", "params": { "material": id, "surface": surface } }),
            );
            assert_eq!(update["ok"], json!(true), "update: {update:?}");
            let get = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-get", "params": { "material": id } }),
            );
            assert_eq!(get["ok"], json!(true), "get: {get:?}");
            assert_eq!(get["result"]["surface"], surface);
        });
    }

    #[test]
    fn vegetation_import_and_summary_use_the_native_map_contract() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "vegetation-summary");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let source = ctx.assets.root.parent().unwrap().join("world.svegmap");
            let bounds = saffron_spatial::WorldBounds::new([0; 3], [4096; 3]).unwrap();
            let map = saffron_vegetation::VegetationMapAsset {
                version: saffron_vegetation::VEGETATION_MAP_VERSION,
                id: saffron_core::Uuid(9_001),
                name: "World vegetation".to_owned(),
                bounds,
                chunk_layout: saffron_vegetation::VegetationMapChunkLayout {
                    level: 0,
                    schema_hash: saffron_vegetation::vegetation_map_chunk_schema_hash(),
                },
                generation: 0,
                inventory: Vec::new(),
            };
            std::fs::write(
                &source,
                saffron_vegetation::write_vegetation_map_asset(&map).unwrap(),
            )
            .unwrap();

            let imported = reg.dispatch(
                ctx,
                &json!({ "cmd": "import-vegetation-asset", "params": { "path": source.to_string_lossy() } }),
            );
            assert_eq!(imported["ok"], json!(true), "import: {imported:?}");
            assert_eq!(imported["result"]["id"], json!("9001"));
            assert_eq!(imported["result"]["type"], json!("vegetation-map"));

            let summary = reg.dispatch(
                ctx,
                &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": "9001" } }),
            );
            assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
            assert_eq!(
                summary["result"]["summary"]["kind"],
                json!("vegetation-map")
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["name"],
                json!("World vegetation")
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["layerCount"],
                json!(0)
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["validation"],
                json!({ "valid": true, "issues": [] })
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["dependencies"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                summary["result"]["summary"]["asset"]
                    .get("latestCook")
                    .is_none()
            );
            assert_eq!(summary["result"]["layers"], json!([]));
        });
    }

    #[test]
    fn plant_validate_recook_and_summary_share_the_single_compiler_route() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "plant-control");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let plant = seed_native_plant(ctx);

            let validation = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-validate", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(validation["ok"], json!(true), "validation: {validation:?}");
            assert_eq!(validation["result"]["plant"], json!(plant.to_string()));
            assert_eq!(validation["result"]["validation"]["valid"], json!(true));
            assert_eq!(validation["result"]["sources"], json!([]));
            assert_eq!(validation["result"]["statistics"]["sources"], json!("1"));
            assert_eq!(validation["result"]["statistics"]["materials"], json!("1"));
            assert!(
                validation["result"]["dependencies"]
                    .as_array()
                    .is_some_and(|dependencies| dependencies.len() >= 5)
            );

            let first = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-recook", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(first["ok"], json!(true), "first recook: {first:?}");
            assert_eq!(first["result"]["validation"]["valid"], json!(true));
            assert_eq!(
                first["result"]["artifactHash"].as_str().map(str::len),
                Some(64)
            );
            assert_eq!(
                first["result"]["familyHash"].as_str().map(str::len),
                Some(64)
            );

            let second = reg.dispatch(
                ctx,
                &json!({ "cmd": "plant-recook", "params": { "plant": plant.to_string() } }),
            );
            assert_eq!(second["ok"], json!(true), "second recook: {second:?}");
            assert_eq!(
                second["result"]["artifactHash"],
                first["result"]["artifactHash"]
            );
            assert_eq!(second["result"]["cacheHit"], json!(true));

            let summary = reg.dispatch(
                ctx,
                &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": plant.to_string() } }),
            );
            assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
            assert_eq!(summary["result"]["summary"]["kind"], json!("plant"));
            assert_eq!(
                summary["result"]["summary"]["asset"]["validation"]["valid"],
                json!(true)
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["provenance"],
                json!([])
            );
            assert!(
                summary["result"]["summary"]["asset"]["dependencies"]
                    .as_array()
                    .is_some_and(|dependencies| dependencies.len() >= 5)
            );
            assert!(
                summary["result"]["summary"]["asset"]
                    .get("latestCook")
                    .is_none(),
                "standalone plant publications have no durable latest pointer"
            );

            let empty_profile = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "plant-recook",
                    "params": { "plant": plant.to_string(), "platformProfile": "" }
                }),
            );
            assert_eq!(empty_profile["ok"], json!(false));
            assert_eq!(
                empty_profile["error"]["message"],
                json!("platformProfile cannot be empty")
            );
        });
    }

    #[test]
    fn biome_summary_reports_recursive_source_validation_and_dependencies() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "biome-summary");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            let plant = seed_native_plant(ctx);
            let biome = seed_biome(ctx, plant);
            let summary = reg.dispatch(
                ctx,
                &json!({ "cmd": "vegetation-asset-summary", "params": { "asset": biome.to_string() } }),
            );
            assert_eq!(summary["ok"], json!(true), "summary: {summary:?}");
            assert_eq!(summary["result"]["summary"]["kind"], json!("biome"));
            assert_eq!(
                summary["result"]["summary"]["asset"]["validation"],
                json!({ "valid": true, "issues": [] })
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["plantPalette"],
                json!([plant.to_string()])
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["dependencies"]
                    .as_array()
                    .map(Vec::len),
                Some(2)
            );
            assert_eq!(
                summary["result"]["summary"]["asset"]["provenance"],
                json!([])
            );
            assert!(
                summary["result"]["summary"]["asset"]
                    .get("latestCook")
                    .is_none()
            );
        });
    }

    /// An exported package carries the cooked artifacts and leaves the authored vegetation sources
    /// behind: the runtime binds a generation from the artifact store and never reads them.
    #[test]
    fn authored_vegetation_sources_are_not_packaged() {
        for name in [
            "forest.splant",
            "meadow.sbiome",
            "world.svegmap",
            "world.svegmap.data",
        ] {
            assert!(
                super::is_authored_vegetation(
                    std::path::Path::new("/project/assets/vegetation")
                        .join(name)
                        .as_path()
                ),
                "{name} is an authored source"
            );
        }
        // Everything a runtime does read stays.
        for name in [
            "bark.smat",
            "oak.smesh",
            "oak.smodel",
            "bark.png",
            "world.svegmanifest",
            "cell.svegcell",
            "oak.splantc",
        ] {
            assert!(
                !super::is_authored_vegetation(
                    std::path::Path::new("/project/assets").join(name).as_path()
                ),
                "{name} is packaged"
            );
        }
    }

    #[test]
    fn plant_source_dto_keeps_observed_hash_and_complete_provenance() {
        let source = saffron_vegetation::PlantSourceReference {
            id: 7,
            locator: saffron_vegetation::PlantSourceLocator::File(
                "file:///plants/oak.glb".to_owned(),
            ),
            role: saffron_vegetation::PlantSourceRole::Geometry,
            selector: saffron_vegetation::PlantSourceSelector::Element {
                id: 8,
                path: "Oak/Trunk".to_owned(),
            },
            content_hash: [1; 32],
            settings: saffron_vegetation::PlantImportSettings::default(),
            provenance: saffron_vegetation::SourceProvenance {
                source: "studio-library".to_owned(),
                source_uri: "https://assets.example/oak".to_owned(),
                license_id: "CC-BY-4.0".to_owned(),
                license_uri: "https://creativecommons.org/licenses/by/4.0/".to_owned(),
                author: "Ada".to_owned(),
                attribution: "Oak by Ada".to_owned(),
                requires_attribution: true,
            },
        };
        let dto = plant_source_reference_dto(&source, &BTreeMap::from([(7, [2; 32])]));
        assert_eq!(dto.content_hash, "02".repeat(32));
        assert_eq!(dto.provenance.author, "Ada");
        assert_eq!(dto.provenance.attribution, "Oak by Ada");
        assert!(dto.provenance.requires_attribution);
        assert!(matches!(
            dto.selector,
            PlantSourceSelectorDto::Element { id, path }
                if id.0 == "00000000000000000000000000000008" && path == "Oak/Trunk"
        ));
    }

    /// `thumbnail-cache stats` reports a clean cache; an unknown action errors.
    #[test]
    fn thumbnail_cache_stats_and_unknown_action() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "thumb-cache");
            let stats = reg.dispatch(
                ctx,
                &json!({ "cmd": "thumbnail-cache", "params": { "action": "stats" } }),
            );
            assert_eq!(stats["ok"], json!(true));
            assert_eq!(stats["result"]["entries"], json!(0));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "thumbnail-cache", "params": { "action": "nope" } }),
            );
            assert_eq!(bad["ok"], json!(false));
            assert_eq!(
                bad["error"]["message"],
                json!("unknown action 'nope' (stats|clear)")
            );
        });
    }

    /// `get-project` reports the editor's project identity; it is loaded after a field set.
    #[test]
    fn get_project_reports_identity() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            ctx.scene_edit.project_name = "demo".to_owned();
            ctx.scene_edit.project_display_name = "Demo".to_owned();
            let reply = reg.dispatch(ctx, &json!({ "cmd": "get-project" }));
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["loaded"], json!(true));
            assert_eq!(reply["result"]["name"], json!("demo"));
            assert_eq!(reply["result"]["displayName"], json!("Demo"));
        });
    }

    #[test]
    fn new_project_rejects_an_invalid_name_before_queueing_load() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "new-project", "params": { "name": "Bad_Name" } }),
            );
            assert_eq!(reply["ok"], json!(false));
            assert_eq!(
                reply["error"]["message"],
                json!("invalid project name 'Bad_Name'")
            );
            assert!(ctx.scene_edit.project_load_inbox.is_none());
        });
    }

    /// The asset commands register in their frozen manifest order, with the global `quit` command
    /// checked separately at the end of the registry.
    #[test]
    fn asset_commands_register_in_manifest_order() {
        const FROZEN: &[&str] = &[
            "get-project",
            "project-status",
            "cancel-load",
            "new-project",
            "create-script",
            "open-project",
            "import-model",
            "instantiate-model",
            "asset-placement",
            "import-texture",
            "import-lut",
            "import-vegetation-asset",
            "list-assets",
            "vegetation-map-layer-commit",
            "vegetation-map-chunk-commit",
            "vegetation-map-chunk-read",
            "vegetation-asset-summary",
            "scan-assets",
            "extract-subasset",
            "clear-extraction",
            "reimport-model",
            "model-info",
            "asset-references",
            "get-asset-model",
            "enter-asset-preview",
            "exit-asset-preview",
            "set-active-view",
            "clean-assets",
            "delete-unused",
            "rename-asset",
            "create-asset-folder",
            "rename-asset-folder",
            "delete-asset-folder",
            "move-asset",
            "asset-usages",
            "probe-asset",
            "delete-asset",
            "assign-asset",
            "material-create",
            "material-assign",
            "material-import",
            "material-list",
            "material-get",
            "material-schema",
            "material-update",
            "preview-render",
            "material-set-graph",
            "material-create-instance",
            "material-set-override",
            "material-compile-graph",
            "material-cook",
            "export-app",
            "save-scene",
            "load-scene",
            "save-project",
            "load-project",
            "reload-project",
            "get-stores",
            "set-stores",
            "screenshot",
            "get-thumbnail",
            "view-asset",
            "thumbnail-cache",
        ];
        let reg = registry();
        let names: Vec<&str> = reg.rows().iter().map(|c| c.name).collect();
        let start = names
            .iter()
            .position(|&n| n == "get-project")
            .expect("get-project is registered");
        assert_eq!(
            &names[start..start + FROZEN.len()],
            FROZEN,
            "the asset domain registers contiguously in the frozen manifest order"
        );
        // `quit` is the last command in the registry.
        assert_eq!(names.last(), Some(&"quit"));
    }

    /// `asset-usages` reports a mesh slot that references the queried asset, with the entity
    /// id as a decimal string.
    #[test]
    fn asset_usages_reports_mesh_slot() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let mesh_id = seed_mesh(ctx, "cube");
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let _ = ctx.scene_edit.active_scene().add_component(
                entity,
                Mesh {
                    mesh: saffron_core::Uuid(mesh_id),
                },
            );
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "asset-usages", "params": { "asset": mesh_id.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(true));
            let usages = reply["result"]["usages"].as_array().unwrap();
            assert_eq!(usages.len(), 1);
            assert_eq!(usages[0]["slot"], json!("mesh"));
            assert_eq!(usages[0]["entity"], json!(entity_uuid));
        });
    }

    #[test]
    fn vegetation_map_usage_and_delete_clear_the_scene_field() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let map = saffron_core::Uuid::new();
            ctx.assets.register_imported_asset(AssetEntry {
                id: map,
                name: "World vegetation".to_owned(),
                asset_type: AssetType::VegetationMap,
                ..AssetEntry::default()
            });
            let entity = ctx.scene_edit.active_scene().create_entity("Vegetation");
            ctx.scene_edit
                .active_scene()
                .add_component(entity, VegetationField { map, enabled: true })
                .unwrap();
            let entity_id = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            let usages = reg.dispatch(
                ctx,
                &json!({ "cmd": "asset-usages", "params": { "asset": map.value().to_string() } }),
            );
            assert_eq!(usages["ok"], json!(true), "usages: {usages:?}");
            assert_eq!(
                usages["result"]["usages"],
                json!([{
                    "entity": entity_id,
                    "entityName": "Vegetation",
                    "slot": "vegetationField.map"
                }])
            );

            let deleted = reg.dispatch(
                ctx,
                &json!({ "cmd": "delete-asset", "params": { "asset": map.value().to_string() } }),
            );
            assert_eq!(deleted["ok"], json!(true), "delete: {deleted:?}");
            assert_eq!(deleted["result"]["cleared"], usages["result"]["usages"]);
            assert!(ctx.assets.catalog().find(map).is_none());
            assert_eq!(
                ctx.scene_edit
                    .active_scene()
                    .component::<VegetationField>(entity)
                    .unwrap(),
                VegetationField {
                    map: saffron_core::Uuid(0),
                    enabled: false,
                }
            );
        });
    }
}
