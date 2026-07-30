use saffron_assets::{
    load_biome_asset, load_plant_family_asset, load_vegetation_map_snapshot,
    validate_plant_family_sources,
};
use saffron_protocol::{
    AssetList, BiomeAssetSummaryDto, BiomeRoleDto, EmptyParams, PlantAssetSummaryDto,
    PlantSourceKindDto, Uuid as WireUuid, VegetationAssetSummaryDto, VegetationAssetSummaryParams,
    VegetationAssetSummaryResult, VegetationMapSummaryDto,
};
use saffron_scene::AssetType;
use saffron_vegetation::{BiomeRole, PlantCompileLimits, PlantFamilySource};

use super::*;
use crate::error::{Error, Result};
use crate::registry::CommandRegistry;

/// Registers the catalog listing plus the vegetation-map authoring commands.
pub(crate) fn register_vegetation_map(reg: &mut CommandRegistry) {
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
            .map_err(Error::command)?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, id)
                .map_err(Error::command)?;
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
            .map_err(Error::command)?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, id)
                .map_err(Error::command)?;
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
                .map_err(Error::command)?;
            let chunks = saffron_assets::load_vegetation_map_chunks(ctx.assets, id, &keys)
                .map_err(Error::command)?;
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
                    let plant = load_plant_family_asset(ctx.assets, id).map_err(Error::command)?;
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
                    let biome = load_biome_asset(ctx.assets, id).map_err(Error::command)?;
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
                    let map =
                        load_vegetation_map_snapshot(ctx.assets, id).map_err(Error::command)?;
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
}
