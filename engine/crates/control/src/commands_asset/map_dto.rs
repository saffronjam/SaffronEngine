use std::collections::{BTreeMap, BTreeSet};

use saffron_assets::{AssetServer, load_biome_asset};
use saffron_core::Uuid;
use saffron_protocol::{
    AlphaClassificationDto, CoverageMipMetadataDto, CoverageSourceDto, FieldBlendOperatorDto,
    InclusionOperatorDto, InteractionPolicyDto, LayerCoordinateSpaceDto, MaterialSurfaceDto,
    OpacityMicromapDerivationDto, PlantStateOverrideDto, PlantTransformOverrideDto,
    SpeciesWeightDto, ThinSheetFoliageParametersDto, ThinSheetNormalBehaviorDto, Uuid as WireUuid,
    VegetationCandidateRejectionReasonDto, VegetationCookNodeAddressDto,
    VegetationCookRejectionTotalDto, VegetationCookStatisticsDto, VegetationGuid,
    VegetationLayerDto, VegetationLayerOperatorDto, VegetationManifestDependencyAddressDto,
    VegetationManifestDependencyDto, VegetationValidationIssueDto, VegetationValidationSeverityDto,
    VegetationValidationSummaryDto, VoxelMaterialMomentsDto,
};
use saffron_vegetation::{
    AlphaClassification, CandidateRejectionReason, ContentHash, CookDependency,
    CookDependencyAddress, CookGraph, CookNodeAddress, CoverageMipMetadata, CoverageSource,
    FieldBlendOperator, InclusionOperator, LayerCoordinateSpace, MaterialSurface,
    OpacityMicromapDerivation, ThinSheetFoliageParameters, ThinSheetNormalBehavior,
    VegetationBaseManifest, VegetationCellArtifactIndex, VegetationCellSectionKind,
    VegetationLayer, VegetationLayerOperator, VegetationMapChunkKind, VoxelMaterialMoments,
    vegetation_rejection_totals,
};

use super::*;
use crate::error::{Error, Result};

pub(crate) fn plant_point_dto(
    point: &saffron_vegetation::PlantPoint,
) -> saffron_protocol::PlantPointDto {
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
pub(crate) fn vegetation_map_chunk_dto(
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

pub(crate) fn manifest_dependency_dto(
    dependency: &CookDependency,
) -> VegetationManifestDependencyDto {
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

pub(crate) fn manifest_dependencies_dto(
    dependencies: &[CookDependency],
) -> Vec<VegetationManifestDependencyDto> {
    dependencies.iter().map(manifest_dependency_dto).collect()
}

pub(crate) fn validation_error(
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

pub(crate) fn source_asset_dependency(
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

pub(crate) fn biome_summary_metadata(
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

pub(crate) fn map_authored_metadata(
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

pub(crate) fn rejection_reason_index(reason: CandidateRejectionReason) -> usize {
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

pub(crate) fn current_map_cook_metadata(
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
pub(crate) fn dirty_map_layers(
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

pub(crate) fn field_blend_dto(operator: FieldBlendOperator) -> FieldBlendOperatorDto {
    match operator {
        FieldBlendOperator::Replace => FieldBlendOperatorDto::Replace,
        FieldBlendOperator::Add => FieldBlendOperatorDto::Add,
        FieldBlendOperator::Multiply => FieldBlendOperatorDto::Multiply,
        FieldBlendOperator::Minimum => FieldBlendOperatorDto::Minimum,
        FieldBlendOperator::Maximum => FieldBlendOperatorDto::Maximum,
    }
}

pub(crate) fn inclusion_dto(operator: InclusionOperator) -> InclusionOperatorDto {
    match operator {
        InclusionOperator::Include => InclusionOperatorDto::Include,
        InclusionOperator::Exclude => InclusionOperatorDto::Exclude,
    }
}

pub(crate) fn interaction_policy_dto(
    policy: saffron_vegetation::InteractionPolicy,
) -> InteractionPolicyDto {
    match policy {
        saffron_vegetation::InteractionPolicy::Decorative => InteractionPolicyDto::Decorative,
        saffron_vegetation::InteractionPolicy::Interactive => InteractionPolicyDto::Interactive,
        saffron_vegetation::InteractionPolicy::Harvestable => InteractionPolicyDto::Harvestable,
        saffron_vegetation::InteractionPolicy::Structural => InteractionPolicyDto::Structural,
    }
}

pub(crate) fn coverage_hash_text(hash: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut text, "{byte:02x}").expect("writing to String cannot fail");
    }
    text
}

pub(crate) fn parse_coverage_hash(value: &str) -> Result<[u8; 32]> {
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

pub(crate) fn material_surface_dto(surface: &MaterialSurface) -> MaterialSurfaceDto {
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

pub(crate) fn material_surface_from_dto(surface: MaterialSurfaceDto) -> Result<MaterialSurface> {
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
    surface.validate().map_err(Error::command)?;
    Ok(surface)
}

pub(crate) fn vegetation_layer_dto(layer: &VegetationLayer) -> VegetationLayerDto {
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
