//! The canonical dependency sets a cook key hashes: per-cell, per-instance, per-global-stage,
//! and the manifest's merged view of all of them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, SurfaceField, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    CompiledBiomeGraph, ContentHash, CookDependency, CookDependencyAddress, CookGraph,
    CookNodeAddress, CookNodeRecord, CookVersionSet, CookWorkEstimate, GraphDependencySource,
    GraphEvaluationResult, VegetationMapChunkKind, VegetationMapSnapshot, VegetationMapTileKey,
    vegetation_base_manifest_schema_hash, vegetation_cell_artifact_schema_hash,
};

use crate::{Error, Result};

use super::measure::result_actual;
use super::stage::EvaluatedInstance;
use super::{VegetationCookEvent, VegetationCookRequest, bounds_intersect};

pub(super) fn append_global_nodes(
    nodes: &mut Vec<CookNodeRecord>,
    evaluated: &[EvaluatedInstance],
    map: &VegetationMapSnapshot,
    request: &VegetationCookRequest,
    plant_outputs: &BTreeMap<u64, ContentHash>,
    versions: CookVersionSet,
    emit: &mut impl FnMut(VegetationCookEvent),
) -> Result<()> {
    let mut completed = BTreeMap::<(u128, ContentHash, WorldCellKey), ContentHash>::new();
    for instance in evaluated {
        for result in &instance.global_results {
            let stage_hash = ContentHash::new(result.stage);
            let address = CookNodeAddress::GlobalStage {
                map: request.map,
                biome_instance: instance.id,
                stage: stage_hash,
                owner: result.owner,
            };
            let input = instance
                .global_inputs
                .get(&(result.stage, result.owner))
                .copied()
                .ok_or_else(|| {
                    Error::Io("evaluator returned an unplanned global-stage tile".to_owned())
                })?;
            let stage = instance
                .graph
                .spatial_plan()
                .global_stage(result.stage)
                .ok_or_else(|| Error::Io("compiled global stage is missing".to_owned()))?;
            let mut dependencies = instance_dependencies(
                instance.id,
                &instance.graph,
                input.read_bounds,
                stage.upstream_halo,
                map,
                request,
                plant_outputs,
            )?;
            push_dependency(
                &mut dependencies,
                CookDependency {
                    address: CookDependencyAddress::Contract {
                        namespace: format!(
                            "evaluator-input/{}/{}/{stage_hash}",
                            instance.id, result.owner
                        ),
                    },
                    content_hash: input.input_snapshot,
                    bounds: Some(input.read_bounds),
                    halo: stage.upstream_halo,
                    ancestor_level: Some(result.owner.level()),
                },
            )?;
            let prerequisite_stages = stage
                .input_pins
                .iter()
                .filter_map(|pin| {
                    instance
                        .graph
                        .spatial_plan()
                        .global_stage_for_node(&pin.node)
                        .map(|stage| ContentHash::new(stage.id))
                })
                .collect::<BTreeSet<_>>();
            for ((owner_instance, prerequisite_stage, owner), output_hash) in &completed {
                if *owner_instance == instance.id
                    && prerequisite_stages.contains(prerequisite_stage)
                    && bounds_intersect(owner.bounds(), input.read_bounds)
                {
                    push_dependency(
                        &mut dependencies,
                        CookDependency {
                            address: CookDependencyAddress::Node(CookNodeAddress::GlobalStage {
                                map: request.map,
                                biome_instance: instance.id,
                                stage: *prerequisite_stage,
                                owner: *owner,
                            }),
                            content_hash: *output_hash,
                            bounds: Some(owner.bounds()),
                            halo: stage.upstream_halo,
                            ancestor_level: Some(owner.level()),
                        },
                    )?;
                }
            }
            let output = result.result.canonical_bytes()?;
            let output_hash = ContentHash::of(&output);
            let estimate = CookWorkEstimate {
                work_units: stage
                    .estimate
                    .candidates
                    .saturating_add(stage.estimate.accepted)
                    .saturating_add(stage.estimate.micro_samples),
                peak_memory_bytes: stage.estimate.memory_bytes,
                input_bytes: 0,
                output_bytes: u64::try_from(output.len())
                    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
            };
            let mut node = CookNodeRecord {
                address: address.clone(),
                cook_key: ContentHash::default(),
                output_hash,
                dependencies,
                estimate,
                actual: result_actual(&result.result, estimate.output_bytes, false),
            };
            node.actual.peak_memory_bytes =
                node.actual.peak_memory_bytes.max(result.resident_bytes);
            node.cook_key = node.calculate_cook_key(versions, &request.platform)?;
            completed.insert((instance.id, stage_hash, result.owner), output_hash);
            emit(VegetationCookEvent::Completed {
                node: address,
                cache_hit: false,
                published_cell: false,
            });
            nodes.push(node);
        }
    }
    Ok(())
}

pub(super) struct CellDependencyContext<'a> {
    pub(super) evaluated: &'a [EvaluatedInstance],
    pub(super) map: &'a VegetationMapSnapshot,
    pub(super) request: &'a VegetationCookRequest,
    pub(super) plant_outputs: &'a BTreeMap<u64, ContentHash>,
    pub(super) global_outputs: &'a BTreeMap<(u128, ContentHash, WorldCellKey), ContentHash>,
}

/// The cell's own (ancestor-independent) dependency half: contracts, the ecology tick, and
/// every intersecting instance's graph, asset, map-layer, and global-stage inputs. Complete at
/// plan time — this is the half a work item's payload carries and its own-input key hashes.
/// [`cell_ancestor_dependencies`] adds the half that waits on completed ancestors.
pub(super) fn cell_own_dependencies(
    cell: WorldCellKey,
    context: &CellDependencyContext<'_>,
) -> Result<Vec<CookDependency>> {
    let mut dependencies = contract_dependencies()?;
    push_dependency(
        &mut dependencies,
        CookDependency {
            address: CookDependencyAddress::Contract {
                namespace: "ecology-snapshot/tick".to_owned(),
            },
            content_hash: ContentHash::of(&context.request.ecology_tick.to_be_bytes()),
            bounds: Some(cell.bounds()),
            halo: DecisionScalar::from_bits(0),
            ancestor_level: None,
        },
    )?;
    for instance in context.evaluated {
        let Some(read_bounds) = instance.cell_read_bounds.get(&cell).copied() else {
            continue;
        };
        let halo = instance.graph.required_halo(cell.level());
        for dependency in instance_dependencies(
            instance.id,
            &instance.graph,
            read_bounds,
            halo,
            context.map,
            context.request,
            context.plant_outputs,
        )? {
            push_dependency(&mut dependencies, dependency)?;
        }
        for ((owner_instance, stage, owner), output_hash) in context.global_outputs {
            if *owner_instance == instance.id && bounds_intersect(owner.bounds(), read_bounds) {
                push_dependency(
                    &mut dependencies,
                    CookDependency {
                        address: CookDependencyAddress::Node(CookNodeAddress::GlobalStage {
                            map: context.request.map,
                            biome_instance: instance.id,
                            stage: *stage,
                            owner: *owner,
                        }),
                        content_hash: *output_hash,
                        bounds: Some(owner.bounds()),
                        halo,
                        ancestor_level: Some(owner.level()),
                    },
                )?;
            }
        }
    }
    Ok(dependencies)
}

/// The ancestor dependency half: one exact-output reference per coarse cell the evaluation
/// actually read, resolvable only once those cells' cooks completed. A claimant composes the
/// full cook key from the payload's own half plus this one.
pub(super) fn cell_ancestor_dependencies(
    map: Uuid,
    cell: WorldCellKey,
    result: &GraphEvaluationResult,
    cell_outputs: &BTreeMap<WorldCellKey, ContentHash>,
) -> Result<Vec<CookDependency>> {
    let mut dependencies = Vec::new();
    for ancestor in &result.ancestor_references {
        let output_hash = cell_outputs.get(ancestor).copied().ok_or_else(|| {
            Error::Io(format!(
                "vegetation cell {cell} references uncooked ancestor {ancestor}"
            ))
        })?;
        push_dependency(
            &mut dependencies,
            CookDependency {
                address: CookDependencyAddress::Node(CookNodeAddress::Cell {
                    map,
                    cell: *ancestor,
                }),
                content_hash: output_hash,
                bounds: Some(ancestor.bounds()),
                halo: DecisionScalar::from_bits(0),
                ancestor_level: Some(ancestor.level()),
            },
        )?;
    }
    Ok(dependencies)
}

fn instance_dependencies(
    instance_id: u128,
    graph: &CompiledBiomeGraph,
    read_bounds: WorldBounds,
    halo: DecisionScalar,
    map: &VegetationMapSnapshot,
    request: &VegetationCookRequest,
    plant_outputs: &BTreeMap<u64, ContentHash>,
) -> Result<Vec<CookDependency>> {
    let mut dependencies = contract_dependencies()?;
    push_dependency(
        &mut dependencies,
        CookDependency {
            address: CookDependencyAddress::BiomeIr {
                map: request.map,
                instance: instance_id,
            },
            content_hash: ContentHash::new(graph.identity),
            bounds: Some(read_bounds),
            halo,
            ancestor_level: None,
        },
    )?;
    let map_layers = graph
        .dependencies()
        .iter()
        .filter_map(|dependency| match dependency.source {
            GraphDependencySource::MapLayer(layer) => Some(layer),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for dependency in graph.dependencies() {
        match dependency.source {
            GraphDependencySource::Asset(asset) => {
                let (address, content_hash) = plant_outputs.get(&asset.value()).map_or_else(
                    || {
                        (
                            CookDependencyAddress::SourceAsset { asset },
                            ContentHash::new(dependency.content_hash),
                        )
                    },
                    |output| {
                        (
                            CookDependencyAddress::Node(CookNodeAddress::Plant { family: asset }),
                            *output,
                        )
                    },
                );
                push_dependency(
                    &mut dependencies,
                    CookDependency {
                        address,
                        content_hash,
                        bounds: None,
                        halo,
                        ancestor_level: None,
                    },
                )?;
            }
            GraphDependencySource::Field(channel) => push_dependency(
                &mut dependencies,
                CookDependency {
                    address: CookDependencyAddress::Contract {
                        namespace: format!("surface-field/{}", field_channel_name(channel)),
                    },
                    content_hash: ContentHash::new(dependency.content_hash),
                    bounds: Some(read_bounds),
                    halo,
                    ancestor_level: None,
                },
            )?,
            GraphDependencySource::SurfaceProvider(provider) => {
                if let Some(surface) = request
                    .surface_providers
                    .iter()
                    .find(|surface| surface.descriptor().id.0 == provider)
                {
                    append_surface_dependencies(
                        &mut dependencies,
                        surface,
                        graph,
                        read_bounds,
                        halo,
                    )?;
                }
            }
            GraphDependencySource::MapLayer(_) => {}
        }
    }
    for reference in &map.root.inventory {
        let include = match reference.key.tile {
            VegetationMapTileKey::Global => {
                (reference.key.kind == VegetationMapChunkKind::GraphInstance
                    && reference.key.layer == instance_id)
                    || (reference.key.kind == VegetationMapChunkKind::LayerMetadata
                        && map_layers.contains(&reference.key.layer))
            }
            VegetationMapTileKey::Cell(cell) => {
                matches!(
                    reference.key.kind,
                    VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride
                ) && map_layers.contains(&reference.key.layer)
                    && bounds_intersect(cell.bounds(), read_bounds)
            }
        };
        if include {
            push_dependency(
                &mut dependencies,
                CookDependency {
                    address: CookDependencyAddress::MapObject {
                        map: request.map,
                        key: reference.key,
                    },
                    content_hash: ContentHash::new(reference.content_hash),
                    bounds: match reference.key.tile {
                        VegetationMapTileKey::Global => None,
                        VegetationMapTileKey::Cell(cell) => Some(cell.bounds()),
                    },
                    halo,
                    ancestor_level: None,
                },
            )?;
        }
    }
    Ok(dependencies)
}

fn append_surface_dependencies(
    dependencies: &mut Vec<CookDependency>,
    surface: &Arc<dyn SurfaceField>,
    graph: &CompiledBiomeGraph,
    read_bounds: WorldBounds,
    halo: DecisionScalar,
) -> Result<()> {
    let descriptor = surface.descriptor();
    let provider_hash = saffron_vegetation::canonical_surface_provider_set_hash(
        &[Arc::clone(surface)],
        graph.limits.max_input_tiles,
    )?;
    push_dependency(
        dependencies,
        CookDependency {
            address: CookDependencyAddress::SurfaceProvider {
                provider: descriptor.id,
                revision: descriptor.revision,
            },
            content_hash: ContentHash::new(provider_hash),
            bounds: Some(descriptor.bounds),
            halo,
            ancestor_level: None,
        },
    )?;
    let required_channels = graph
        .dependencies()
        .iter()
        .filter_map(|dependency| match dependency.source {
            GraphDependencySource::Field(channel) => Some(channel),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for channel in required_channels {
        let mut tiles = surface.authoritative_tiles(channel, read_bounds);
        tiles.sort_by_key(|tile| (tile.bounds.min_ticks(), tile.dimensions));
        for tile in tiles {
            let mut bytes = Vec::with_capacity(128);
            bytes.extend_from_slice(&tile.provider.0.to_be_bytes());
            bytes.extend_from_slice(&tile.revision.0.to_be_bytes());
            let (channel_tag, channel_user) = channel.canonical_code();
            bytes.push(channel_tag);
            bytes.extend_from_slice(&channel_user.to_be_bytes());
            for value in tile.bounds.min_ticks() {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            for value in tile.bounds.max_ticks_exclusive() {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            for value in tile.dimensions {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            bytes.extend_from_slice(&tile.value_quantum_bits.to_be_bytes());
            push_dependency(
                dependencies,
                CookDependency {
                    address: CookDependencyAddress::SurfaceTile {
                        provider: tile.provider,
                        revision: tile.revision,
                        channel: Some(channel),
                        bounds: tile.bounds,
                    },
                    content_hash: ContentHash::of(&bytes),
                    bounds: Some(tile.bounds),
                    halo,
                    ancestor_level: None,
                },
            )?;
        }
    }
    Ok(())
}

pub(super) fn field_channel_name(channel: saffron_spatial::FieldChannel) -> String {
    let (tag, user) = channel.canonical_code();
    if tag == 14 {
        format!("user/{user}")
    } else {
        [
            "altitude",
            "slope",
            "curvature",
            "concavity",
            "drainage",
            "moisture",
            "temperature",
            "precipitation",
            "sunlight",
            "exposure",
            "water-distance",
            "water-depth",
            "signed-blocker",
            "spline-distance",
        ][usize::from(tag)]
        .to_owned()
    }
}

fn contract_dependencies() -> Result<Vec<CookDependency>> {
    let contracts = [
        ("artifact/svegcell", vegetation_cell_artifact_schema_hash()),
        (
            "point-columns",
            ContentHash::new(saffron_vegetation::point_schema_hash()),
        ),
    ];
    let mut dependencies = Vec::with_capacity(contracts.len());
    for (namespace, content_hash) in contracts {
        push_dependency(
            &mut dependencies,
            CookDependency {
                address: CookDependencyAddress::Contract {
                    namespace: namespace.to_owned(),
                },
                content_hash,
                bounds: None,
                halo: DecisionScalar::from_bits(0),
                ancestor_level: None,
            },
        )?;
    }
    Ok(dependencies)
}

pub(super) fn push_dependency(
    dependencies: &mut Vec<CookDependency>,
    dependency: CookDependency,
) -> Result<()> {
    if let Some(existing) = dependencies
        .iter()
        .find(|existing| existing.address == dependency.address)
    {
        if existing != &dependency {
            return Err(Error::Io(
                "one vegetation dependency address resolved to conflicting content or support"
                    .to_owned(),
            ));
        }
        return Ok(());
    }
    dependencies.push(dependency);
    Ok(())
}

pub(super) fn manifest_dependencies(
    graph: &CookGraph,
    map: Uuid,
    map_hash: ContentHash,
) -> Result<Vec<CookDependency>> {
    let mut dependencies = vec![CookDependency {
        address: CookDependencyAddress::MapManifest { map },
        content_hash: map_hash,
        bounds: None,
        halo: DecisionScalar::from_bits(0),
        ancestor_level: None,
    }];
    merge_manifest_dependency(
        &mut dependencies,
        CookDependency {
            address: CookDependencyAddress::Contract {
                namespace: "manifest/vegetation-base".to_owned(),
            },
            content_hash: vegetation_base_manifest_schema_hash(),
            bounds: None,
            halo: DecisionScalar::from_bits(0),
            ancestor_level: None,
        },
    )?;
    for node in &graph.nodes {
        for dependency in &node.dependencies {
            merge_manifest_dependency(&mut dependencies, dependency.clone())?;
        }
    }
    Ok(dependencies)
}

fn merge_manifest_dependency(
    dependencies: &mut Vec<CookDependency>,
    dependency: CookDependency,
) -> Result<()> {
    let Some(existing) = dependencies
        .iter_mut()
        .find(|existing| existing.address == dependency.address)
    else {
        dependencies.push(dependency);
        return Ok(());
    };
    if existing.content_hash != dependency.content_hash {
        return Err(Error::Io(
            "manifest dependency address resolves to conflicting content".to_owned(),
        ));
    }
    existing.bounds = match (existing.bounds, dependency.bounds) {
        (Some(left), Some(right)) => Some(left.union(right)),
        _ => None,
    };
    if dependency.halo > existing.halo {
        existing.halo = dependency.halo;
    }
    existing.ancestor_level = existing
        .ancestor_level
        .into_iter()
        .chain(dependency.ancestor_level)
        .max();
    Ok(())
}
