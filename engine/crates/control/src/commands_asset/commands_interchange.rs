use saffron_assets::load_plant_family_asset;

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;

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
                            .map_err(Error::command)?,
                    )
                    .map_err(Error::command)?;
                    saffron_vegetation::read_houdini_points(&document, &families)
                        .map_err(Error::command)?
                }
                "usda" | "usd" => {
                    let text = std::fs::read_to_string(&params.path)
                        .map_err(Error::command)?;
                    saffron_vegetation::read_usd_point_instancers(&text, &families)
                        .map_err(Error::command)?
                }
                "gltf" | "glb" => {
                    let instancing = saffron_geometry::read_gltf_instancing(&params.path)
                        .map_err(Error::command)?;
                    saffron_vegetation::gltf_instancing_to_interchange(&instancing, &families)
                        .map_err(Error::command)?
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
                    .map_err(Error::command)?;
                dimensions.insert(prototype.family.value(), plant.dimensions);
            }
            let anchors = saffron_vegetation::interchange_to_anchors(&payload, layer, &dimensions)
                .map_err(Error::command)?;

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
            .map_err(Error::command)?;
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
            .map_err(Error::command)?;
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, map)
                .map_err(Error::command)?;
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
                .map_err(Error::command)?;
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
                .map_err(Error::command)?;
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
                .map_err(Error::command)?;
            Ok(saffron_protocol::VegetationExportPointsResult {
                instances: u32::try_from(payload.instances.len()).unwrap_or(u32::MAX),
                prototypes: u32::try_from(payload.prototypes.len()).unwrap_or(u32::MAX),
                path: params.path.clone(),
            })
        },
    );
}
