//! Assembly of one immutable evaluation job: the per-cell input scopes plus the closed
//! global-stage tile set they depend on.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, FieldDerivative, SurfaceField, WorldBounds, WorldCellKey,
    world_cells_covering_bounds,
};
use saffron_vegetation::{
    EvaluationAnchor, EvaluationFieldSource, EvaluationFieldTile, EvaluationRegion,
    EvaluationRegionKind, EvaluationSpline, FieldBlendOperator, GlobalStageEvaluationInputs,
    GraphDependencyFingerprint, GraphDependencySource, GraphEvaluationInputs,
    GraphEvaluationJobInputs, QuantizedFieldTileValues, VegetationLayerOperator,
    VegetationMapSnapshot, canonical_surface_provider_set_hash,
};

use crate::cook_reader::CookAssetAccess;
use crate::{AssetServer, Error, Result};

use super::bounds::{
    canonical_hierarchical_regions, expand_bounds, insert_global_owners, intersect_bounds,
};
use super::graph::ResolvedBiomeGraph;
use super::input_snapshot::global_stage_input_snapshot;
use super::map_package::{
    load_vegetation_map_snapshot_from, load_vegetation_map_tile_snapshot_from,
};

/// Assembles one immutable batch and its complete compiler-owned global-stage input closure.
pub fn assemble_biome_graph_evaluation_job(
    assets: &AssetServer,
    resolved: &ResolvedBiomeGraph,
    map: Uuid,
    requested_cells: &[WorldCellKey],
    ecology_tick: u64,
    surface_providers: Vec<Arc<dyn SurfaceField>>,
) -> Result<GraphEvaluationJobInputs> {
    assemble_biome_graph_evaluation_job_from(
        assets,
        resolved,
        map,
        requested_cells,
        ecology_tick,
        surface_providers,
    )
}

pub(crate) fn assemble_biome_graph_evaluation_job_from(
    assets: &dyn CookAssetAccess,
    resolved: &ResolvedBiomeGraph,
    map: Uuid,
    requested_cells: &[WorldCellKey],
    ecology_tick: u64,
    surface_providers: Vec<Arc<dyn SurfaceField>>,
) -> Result<GraphEvaluationJobInputs> {
    let biome_instance = resolved.biome_instance.ok_or_else(|| {
        Error::Io("graph evaluation requires a map-local biome instance".to_owned())
    })?;
    let map_asset = load_vegetation_map_snapshot_from(assets, map)?;
    let instance = map_asset
        .biome_instances
        .iter()
        .find(|instance| instance.id == biome_instance)
        .ok_or_else(|| {
            Error::Io("vegetation biome instance is not present in the map".to_owned())
        })?;
    if instance.biome != resolved.graph.biome {
        return Err(Error::Io(
            "compiled biome does not match the map-local instance".to_owned(),
        ));
    }

    let mut cells = requested_cells.to_vec();
    cells.sort_unstable();
    cells.dedup();
    if cells.len() as u64 > resolved.graph.limits.max_output_cells {
        return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
            resource: "output cells",
            requested: cells.len() as u64,
            limit: resolved.graph.limits.max_output_cells,
        }));
    }
    if surface_providers.len() as u64 > resolved.graph.limits.max_input_tiles {
        return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
            resource: "input tiles",
            requested: surface_providers.len() as u64,
            limit: resolved.graph.limits.max_input_tiles,
        }));
    }
    let context = GraphInputAssemblyContext {
        assets,
        resolved,
        map,
        map_asset: &map_asset,
        biome_instance,
        instance_bounds: instance.bounds,
        instance_namespace: instance.id,
        ecology_tick,
        surface_providers: &surface_providers,
    };
    let cell_dependencies = resolved.graph.dependencies();
    let mut cell_inputs = Vec::with_capacity(cells.len());
    for cell in cells {
        cell_inputs.push(assemble_graph_input_scope(
            &context,
            GraphInputScope {
                output_cell: cell,
                output_bounds: cell.bounds(),
                read_bounds: expand_bounds(
                    cell.bounds(),
                    resolved.graph.required_halo(cell.level()),
                )?,
                region_level: cell.level(),
                require_biome_intersection: true,
                dependencies: cell_dependencies,
            },
        )?);
    }

    let stages = resolved.graph.spatial_plan().global_stages();
    let stage_indices = stages
        .iter()
        .enumerate()
        .map(|(index, stage)| (stage.id, index))
        .collect::<BTreeMap<_, _>>();
    let prerequisites_by_stage = stages
        .iter()
        .enumerate()
        .map(|(stage_index, stage)| {
            let prerequisites = stage
                .input_pins
                .iter()
                .map(|pin| {
                    resolved
                        .graph
                        .spatial_plan()
                        .global_stage_for_node(&pin.node)
                        .and_then(|prerequisite| stage_indices.get(&prerequisite.id).copied())
                        .ok_or_else(|| {
                            Error::Io(
                                "compiled global-stage input does not resolve to an earlier stage"
                                    .to_owned(),
                            )
                        })
                })
                .collect::<Result<BTreeSet<_>>>()?;
            if prerequisites
                .iter()
                .any(|prerequisite| *prerequisite >= stage_index)
            {
                return Err(Error::Io(
                    "compiled global-stage dependency order is invalid".to_owned(),
                ));
            }
            Ok(prerequisites)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut owners_by_stage = vec![BTreeSet::new(); stages.len()];
    let mut global_tile_count = 0_u64;
    for (stage_index, stage) in stages.iter().enumerate() {
        for inputs in &cell_inputs {
            insert_global_owners(
                inputs.read_bounds,
                stage.owner_level,
                stage_index,
                &mut owners_by_stage,
                &mut global_tile_count,
                resolved.graph.limits.max_global_stage_tiles,
            )?;
        }
    }
    for stage_index in (0..stages.len()).rev() {
        let stage = &stages[stage_index];
        let owners = owners_by_stage[stage_index]
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for owner in owners {
            let read_bounds = expand_bounds(owner.bounds(), stage.upstream_halo)?;
            for prerequisite in &prerequisites_by_stage[stage_index] {
                insert_global_owners(
                    read_bounds,
                    stages[*prerequisite].owner_level,
                    *prerequisite,
                    &mut owners_by_stage,
                    &mut global_tile_count,
                    resolved.graph.limits.max_global_stage_tiles,
                )?;
            }
        }
    }

    let mut global_stages = Vec::with_capacity(
        usize::try_from(global_tile_count)
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
    );
    let mut snapshots_by_tile = BTreeMap::new();
    for (stage_index, stage) in stages.iter().enumerate() {
        for owner in &owners_by_stage[stage_index] {
            let solve_bounds = owner.bounds();
            let inputs = assemble_graph_input_scope(
                &context,
                GraphInputScope {
                    output_cell: *owner,
                    output_bounds: solve_bounds,
                    read_bounds: expand_bounds(solve_bounds, stage.upstream_halo)?,
                    region_level: stage.minimum_input_level,
                    require_biome_intersection: false,
                    dependencies: &stage.dependencies,
                },
            )?;
            let mut prerequisite_snapshots = Vec::new();
            for prerequisite in &prerequisites_by_stage[stage_index] {
                for prerequisite_owner in world_cells_covering_bounds(
                    inputs.read_bounds,
                    stages[*prerequisite].owner_level,
                    resolved.graph.limits.max_global_stage_tiles,
                )? {
                    let prerequisite_stage = stages[*prerequisite].id;
                    let prerequisite_snapshot = snapshots_by_tile
                        .get(&(prerequisite_stage, prerequisite_owner))
                        .copied()
                        .ok_or_else(|| {
                            Error::Io(
                                "global-stage prerequisite snapshot is missing from the closed job"
                                    .to_owned(),
                            )
                        })?;
                    prerequisite_snapshots.push((
                        prerequisite_stage,
                        prerequisite_owner,
                        prerequisite_snapshot,
                    ));
                }
            }
            prerequisite_snapshots.sort_unstable();
            prerequisite_snapshots.dedup();
            let input_snapshot =
                global_stage_input_snapshot(stage, &inputs, &prerequisite_snapshots)?;
            if snapshots_by_tile
                .insert((stage.id, *owner), input_snapshot)
                .is_some()
            {
                return Err(Error::Io(
                    "global-stage input snapshot was assembled more than once".to_owned(),
                ));
            }
            global_stages.push(GlobalStageEvaluationInputs {
                stage: stage.id,
                owner: *owner,
                solve_bounds,
                input_snapshot,
                inputs,
            });
        }
    }
    Ok(GraphEvaluationJobInputs {
        cells: cell_inputs,
        global_stages,
    })
}

struct GraphInputAssemblyContext<'a> {
    assets: &'a dyn CookAssetAccess,
    resolved: &'a ResolvedBiomeGraph,
    map: Uuid,
    map_asset: &'a VegetationMapSnapshot,
    biome_instance: u128,
    instance_bounds: WorldBounds,
    instance_namespace: u128,
    ecology_tick: u64,
    surface_providers: &'a [Arc<dyn SurfaceField>],
}

struct GraphInputScope<'a> {
    output_cell: WorldCellKey,
    output_bounds: WorldBounds,
    read_bounds: WorldBounds,
    region_level: u8,
    require_biome_intersection: bool,
    dependencies: &'a [GraphDependencyFingerprint],
}

fn assemble_graph_input_scope(
    context: &GraphInputAssemblyContext<'_>,
    scope: GraphInputScope<'_>,
) -> Result<GraphEvaluationInputs> {
    let mut inputs = GraphEvaluationInputs::for_cell(
        context.map,
        context.biome_instance,
        scope.output_cell,
        DecisionScalar::from_bits(0),
    )?;
    inputs.output_bounds = scope.output_bounds;
    inputs.read_bounds = scope.read_bounds;
    let region_bounds = intersect_bounds(inputs.read_bounds, context.map_asset.bounds)
        .and_then(|bounds| intersect_bounds(bounds, context.instance_bounds));
    if scope.require_biome_intersection && region_bounds.is_none() {
        return Err(Error::Io(
            "evaluation cell does not intersect the biome instance".to_owned(),
        ));
    }
    inputs.regions = region_bounds
        .map(|bounds| {
            canonical_hierarchical_regions(
                bounds,
                scope.region_level,
                context.instance_namespace,
                context.resolved.graph.limits.max_input_tiles,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let dependency_hashes = scope
        .dependencies
        .iter()
        .map(|dependency| (dependency.source, dependency.content_hash))
        .collect::<BTreeMap<_, _>>();
    inputs.plant_prototypes = context
        .resolved
        .plant_prototypes
        .iter()
        .copied()
        .filter(|prototype| {
            dependency_hashes.contains_key(&GraphDependencySource::Asset(prototype.family))
        })
        .collect();
    inputs.ecology_tick = context.ecology_tick;

    let layer_ids = context
        .map_asset
        .layers
        .iter()
        .map(|layer| layer.id)
        .collect::<BTreeSet<_>>();
    let layers_by_id = context
        .map_asset
        .layers
        .iter()
        .map(|layer| (layer.id, layer))
        .collect::<BTreeMap<_, _>>();
    let chunk_cells = region_bounds
        .map(|bounds| {
            world_cells_covering_bounds(
                bounds,
                context.map_asset.chunk_layout.level,
                context.resolved.graph.limits.max_input_tiles,
            )
            .map_err(|error| match error {
                saffron_spatial::Error::CellEnumerationLimit { requested, limit } => {
                    Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                        resource: "input chunk cells",
                        requested,
                        limit,
                    })
                }
                error => Error::Spatial(error),
            })
        })
        .transpose()?
        .unwrap_or_default();
    let mut tile_count = 0_u64;
    let mut anchor_ids = BTreeSet::new();
    for chunk_cell in chunk_cells {
        let Some(chunk) =
            load_vegetation_map_tile_snapshot_from(context.assets, context.map, chunk_cell)?
        else {
            continue;
        };
        for (field, blocker) in chunk
            .fields
            .iter()
            .map(|field| (field, false))
            .chain(chunk.blockers.iter().map(|field| (field, true)))
        {
            if !layer_ids.contains(&field.layer) {
                return Err(Error::Io(
                    "authored field references a layer absent from its vegetation map".to_owned(),
                ));
            }
            let Some(source_hash) = dependency_hashes
                .get(&GraphDependencySource::MapLayer(field.layer))
                .copied()
            else {
                continue;
            };
            let layer = layers_by_id
                .get(&field.layer)
                .copied()
                .ok_or_else(|| Error::Io("authored field layer metadata is missing".to_owned()))?;
            if layer.muted {
                continue;
            }
            let (blend, weight) = match (&layer.operator, blocker) {
                (VegetationLayerOperator::ScalarField(definition), false)
                | (VegetationLayerOperator::Density(definition), false)
                    if definition.channel == field.channel =>
                {
                    (definition.blend, definition.weight)
                }
                (VegetationLayerOperator::Mask { .. }, false) => (
                    FieldBlendOperator::Replace,
                    saffron_spatial::UnitInterval::ONE,
                ),
                (VegetationLayerOperator::Blocker { .. }, true)
                    if field.channel == saffron_spatial::FieldChannel::SignedBlocker =>
                {
                    (
                        FieldBlendOperator::Replace,
                        saffron_spatial::UnitInterval::ONE,
                    )
                }
                _ => {
                    return Err(Error::Io(
                        "authored field payload does not match its typed vegetation layer"
                            .to_owned(),
                    ));
                }
            };
            tile_count = tile_count.checked_add(1).ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
            if tile_count > context.resolved.graph.limits.max_input_tiles {
                return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                    resource: "input tiles",
                    requested: tile_count,
                    limit: context.resolved.graph.limits.max_input_tiles,
                }));
            }
            let values = field
                .values
                .iter()
                .map(|value| {
                    value
                        .checked_mul(field.quantum_bits)
                        .ok_or(Error::Vegetation(
                            saffron_vegetation::Error::NumericOverflow,
                        ))
                })
                .collect::<Result<Vec<_>>>()?;
            inputs.fields.push(EvaluationFieldTile {
                source: EvaluationFieldSource::MapLayer(field.layer),
                channel: field.channel,
                derivative: FieldDerivative::Value,
                blend,
                weight,
                layer_order: layer.order_key(),
                source_hash,
                bounds: chunk.cell.bounds(),
                dimensions: field.dimensions,
                values: QuantizedFieldTileValues::Scalar(values),
            });
        }
        for anchor in chunk.explicit_plants {
            if !dependency_hashes.contains_key(&GraphDependencySource::MapLayer(anchor.layer)) {
                continue;
            }
            if !layer_ids.contains(&anchor.layer) || !anchor_ids.insert(anchor.id) {
                return Err(Error::Io(
                    "explicit anchor layer or identity is invalid for its vegetation map"
                        .to_owned(),
                ));
            }
            inputs.anchors.push(EvaluationAnchor {
                layer: anchor.layer,
                point: anchor.point,
            });
        }
    }
    inputs.fields.sort_by_key(|field| {
        (
            field.layer_order,
            field.source,
            field.channel,
            field.bounds.min_ticks(),
        )
    });
    inputs.anchors.sort_by_key(|anchor| anchor.point.id);
    let mut surface_providers = context
        .surface_providers
        .iter()
        .filter(|provider| {
            let descriptor = provider.descriptor();
            dependency_hashes.contains_key(&GraphDependencySource::SurfaceProvider(descriptor.id.0))
                || provider.field_channels().into_iter().any(|channel| {
                    dependency_hashes.contains_key(&GraphDependencySource::Field(channel))
                })
        })
        .map(Arc::clone)
        .collect::<Vec<_>>();
    surface_providers.sort_by_key(|provider| provider.descriptor().id);
    if !surface_providers.is_empty() {
        inputs.surface_provider_set_hash = canonical_surface_provider_set_hash(
            &surface_providers,
            context.resolved.graph.limits.max_input_tiles,
        )?;
        inputs.surface_providers = surface_providers;
    }
    let mut layers = context.map_asset.layers.iter().collect::<Vec<_>>();
    layers.sort_by_key(|layer| layer.order_key());
    for layer in layers {
        if layer.muted
            || !dependency_hashes.contains_key(&GraphDependencySource::MapLayer(layer.id))
        {
            continue;
        }
        match &layer.operator {
            VegetationLayerOperator::Volume(volume) => {
                let Some(bounds) = region_bounds
                    .and_then(|read_bounds| intersect_bounds(volume.bounds, read_bounds))
                else {
                    continue;
                };
                tile_count = tile_count.checked_add(1).ok_or(Error::Vegetation(
                    saffron_vegetation::Error::NumericOverflow,
                ))?;
                inputs.regions.push(EvaluationRegion {
                    id: layer.id,
                    kind: EvaluationRegionKind::Shape,
                    layer: layer.id,
                    hierarchy_namespace: None,
                    seed_cell: inputs.output_cell,
                    bounds,
                });
            }
            VegetationLayerOperator::Spline(spline) => {
                if region_bounds
                    .and_then(|read_bounds| intersect_bounds(layer.bounds, read_bounds))
                    .is_none()
                {
                    continue;
                }
                tile_count = tile_count.checked_add(1).ok_or(Error::Vegetation(
                    saffron_vegetation::Error::NumericOverflow,
                ))?;
                inputs.splines.push(EvaluationSpline {
                    id: spline.spline,
                    layer: layer.id,
                    points: spline.points.clone(),
                });
            }
            _ => {}
        }
        if tile_count > context.resolved.graph.limits.max_input_tiles {
            return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                resource: "input tiles",
                requested: tile_count,
                limit: context.resolved.graph.limits.max_input_tiles,
            }));
        }
    }
    inputs
        .splines
        .sort_by_key(|spline| (spline.layer, spline.id));
    Ok(inputs)
}

#[cfg(test)]
mod tests {
    use super::super::bounds::expand_bounds;
    use super::super::test_support::{
        Scratch, biome_fixture, commit_chunks, fixed, graph_instance_chunk, layer_chunk,
        layer_fixture, map_fixture, plant_fixture,
    };
    use super::*;
    use crate::vegetation::{
        compile_catalog_biome_instance_graph, save_biome_asset, save_plant_family_asset,
        save_vegetation_map_asset, vegetation_graph_dependency_hashes,
    };
    use saffron_vegetation::{
        BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION, BiomeGraphDocument,
        BiomeGraphEvaluator, GraphAuthority, GraphCancellationToken, GraphCompileOptions,
        GraphDomain, GraphEdge, GraphInterfaceOutput, GraphNodeDefinition, GraphOperator,
        GraphParameterValue, GraphSink, LocalBiomeInstance, NodeSpatialPolicy,
    };

    #[test]
    fn catalog_instance_compile_and_input_assembly_feed_the_single_evaluator() {
        let scratch = Scratch::new("graph-inputs");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let plant =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "").unwrap();
        let node = |guid, operator| GraphNodeDefinition {
            guid,
            version: BIOME_NODE_VERSION,
            semantic_revision: 1,
            operator,
            authority: GraphAuthority::Authoritative,
            spatial: NodeSpatialPolicy::Partitioned {
                level: 0,
                influence_radius: DecisionScalar::from_bits(0),
            },
            dependencies: Vec::new(),
            seed_namespaces: BTreeMap::new(),
            parameters: BTreeMap::new(),
        };
        let region = node(1, GraphOperator::RegionInput);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage);
        coverage.seed_namespaces.insert("sampling".to_owned(), 23);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        let species = node(3, GraphOperator::SpeciesInput);
        let mut output = node(4, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 23);
        let graph = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 400,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, coverage, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut biome_asset = biome_fixture(Uuid(2), "Forest", plant);
        biome_asset.graph = graph.to_json();
        let biome = save_biome_asset(&mut assets, biome_asset, "Forest", "").unwrap();
        let cell = WorldCellKey::base(0, 0, 0);
        let mut map_asset = map_fixture(Uuid(3), "World");
        map_asset.bounds = cell.bounds();
        let map = save_vegetation_map_asset(&mut assets, map_asset, "World", "").unwrap();
        let mut layer = layer_fixture(cell.bounds());
        layer.bounds = cell.bounds();
        let instance = LocalBiomeInstance {
            id: 91,
            biome,
            bounds: cell.bounds(),
            bindings: Vec::new(),
            revision: 1,
        };
        commit_chunks(
            &mut assets,
            map,
            vec![layer_chunk(map, layer), graph_instance_chunk(map, instance)],
        );
        let dependencies = vegetation_graph_dependency_hashes(&assets, map, &[]).unwrap();
        let resolved = compile_catalog_biome_instance_graph(
            &assets,
            map,
            91,
            &dependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let inputs =
            assemble_biome_graph_evaluation_job(&assets, &resolved, map, &[cell], 17, Vec::new())
                .unwrap();
        assert_eq!(inputs.cells[0].biome_instance, 91);
        assert_eq!(inputs.cells[0].plant_prototypes.len(), 1);
        let result = BiomeGraphEvaluator::new(Arc::new(resolved.graph), 1)
            .unwrap()
            .evaluate(inputs, &GraphCancellationToken::default())
            .unwrap();
        assert_eq!(result.cells[0].macro_points.ids.len(), 4);
        assert!(
            result.cells[0]
                .macro_points
                .families
                .iter()
                .all(|family| *family == plant)
        );
    }

    #[test]
    fn batch_assembly_closes_global_stage_prerequisites_and_is_request_order_independent() {
        let scratch = Scratch::new("global-graph-inputs");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let plant =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "").unwrap();
        let node = |guid, operator| GraphNodeDefinition {
            guid,
            version: BIOME_NODE_VERSION,
            semantic_revision: 1,
            operator,
            authority: GraphAuthority::Authoritative,
            spatial: NodeSpatialPolicy::Partitioned {
                level: 0,
                influence_radius: DecisionScalar::from_bits(0),
            },
            dependencies: Vec::new(),
            seed_namespaces: BTreeMap::new(),
            parameters: BTreeMap::new(),
        };
        let region = node(1, GraphOperator::RegionInput);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage);
        coverage.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: fixed(2),
        };
        coverage.seed_namespaces.insert("sampling".to_owned(), 23);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        let species = node(3, GraphOperator::SpeciesInput);
        let mut output = node(4, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 23);
        let mut fine_global = node(5, GraphOperator::Transform);
        fine_global.spatial = NodeSpatialPolicy::Global { level: 0 };
        fine_global
            .seed_namespaces
            .insert("variation".to_owned(), 29);
        let mut coarse_global = node(6, GraphOperator::Transform);
        coarse_global.spatial = NodeSpatialPolicy::Global { level: 2 };
        coarse_global
            .seed_namespaces
            .insert("variation".to_owned(), 31);
        let graph = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 400,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![
                output,
                coarse_global,
                fine_global,
                species,
                coverage,
                region,
            ],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut biome_asset = biome_fixture(Uuid(2), "Forest", plant);
        biome_asset.seed_namespaces.extend([
            ("fine-variation".to_owned(), 29),
            ("coarse-variation".to_owned(), 31),
        ]);
        biome_asset.graph = graph.to_json();
        let biome = save_biome_asset(&mut assets, biome_asset, "Forest", "").unwrap();
        let biome_bounds = WorldCellKey::new(0, 0, 0, 2).unwrap().bounds();
        let mut map_asset = map_fixture(Uuid(3), "World");
        map_asset.bounds = biome_bounds;
        let map = save_vegetation_map_asset(&mut assets, map_asset, "World", "").unwrap();
        let mut layer = layer_fixture(biome_bounds);
        layer.bounds = biome_bounds;
        let instance = LocalBiomeInstance {
            id: 91,
            biome,
            bounds: biome_bounds,
            bindings: Vec::new(),
            revision: 1,
        };
        commit_chunks(
            &mut assets,
            map,
            vec![layer_chunk(map, layer), graph_instance_chunk(map, instance)],
        );
        let dependencies = vegetation_graph_dependency_hashes(&assets, map, &[]).unwrap();
        let resolved = compile_catalog_biome_instance_graph(
            &assets,
            map,
            91,
            &dependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let stages = resolved.graph.spatial_plan().global_stages();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].owner_level, 0);
        assert_eq!(stages[0].upstream_halo, fixed(2));
        assert_eq!(stages[1].owner_level, 2);
        assert_eq!(stages[1].input_pins.len(), 1);

        let first = WorldCellKey::base(0, 0, 0);
        let second = WorldCellKey::base(1, 0, 0);
        let forward = assemble_biome_graph_evaluation_job(
            &assets,
            &resolved,
            map,
            &[first, second],
            17,
            Vec::new(),
        )
        .unwrap();
        let reversed = assemble_biome_graph_evaluation_job(
            &assets,
            &resolved,
            map,
            &[second, first, second],
            17,
            Vec::new(),
        )
        .unwrap();
        let forward_result = BiomeGraphEvaluator::new(Arc::new(resolved.graph.clone()), 2)
            .unwrap()
            .evaluate(forward.clone(), &GraphCancellationToken::default())
            .unwrap();
        let reversed_result = BiomeGraphEvaluator::new(Arc::new(resolved.graph.clone()), 1)
            .unwrap()
            .evaluate(reversed.clone(), &GraphCancellationToken::default())
            .unwrap();
        let canonical_results = |result: saffron_vegetation::GraphEvaluationJobResult| {
            let cells = result
                .cells
                .into_iter()
                .map(|cell| (cell.cell, cell.canonical_bytes().unwrap()))
                .collect::<BTreeMap<_, _>>();
            let global = result
                .global_stages
                .into_iter()
                .map(|stage| {
                    (
                        (stage.stage, stage.owner),
                        stage.result.canonical_bytes().unwrap(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            (cells, global)
        };
        assert_eq!(
            canonical_results(forward_result),
            canonical_results(reversed_result)
        );
        assert_eq!(
            forward
                .cells
                .iter()
                .map(|inputs| inputs.output_cell)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        let signatures = |job: &GraphEvaluationJobInputs| {
            job.global_stages
                .iter()
                .map(|inputs| (inputs.stage, inputs.owner, inputs.input_snapshot))
                .collect::<Vec<_>>()
        };
        assert_eq!(signatures(&forward), signatures(&reversed));
        let dependent = forward
            .global_stages
            .iter()
            .find(|inputs| inputs.stage == stages[1].id)
            .unwrap();
        let fine_snapshots = forward
            .global_stages
            .iter()
            .filter(|inputs| inputs.stage == stages[0].id)
            .map(|inputs| (inputs.owner, inputs.input_snapshot))
            .collect::<BTreeMap<_, _>>();
        let mut prerequisite_snapshots = world_cells_covering_bounds(
            dependent.inputs.read_bounds,
            stages[0].owner_level,
            resolved.graph.limits.max_global_stage_tiles,
        )
        .unwrap()
        .into_iter()
        .map(|owner| (stages[0].id, owner, fine_snapshots[&owner]))
        .collect::<Vec<_>>();
        assert_eq!(
            global_stage_input_snapshot(&stages[1], &dependent.inputs, &prerequisite_snapshots)
                .unwrap(),
            dependent.input_snapshot
        );
        prerequisite_snapshots[0].2[0] ^= 1;
        assert_ne!(
            global_stage_input_snapshot(&stages[1], &dependent.inputs, &prerequisite_snapshots)
                .unwrap(),
            dependent.input_snapshot
        );
        let coarse_owners = forward
            .global_stages
            .iter()
            .filter(|inputs| inputs.stage == stages[1].id)
            .map(|inputs| inputs.owner)
            .collect::<BTreeSet<_>>();
        let expected_coarse_owners = forward
            .cells
            .iter()
            .flat_map(|inputs| {
                world_cells_covering_bounds(
                    inputs.read_bounds,
                    stages[1].owner_level,
                    resolved.graph.limits.max_global_stage_tiles,
                )
                .unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(coarse_owners, expected_coarse_owners);
        let fine_owners = forward
            .global_stages
            .iter()
            .filter(|inputs| inputs.stage == stages[0].id)
            .map(|inputs| inputs.owner)
            .collect::<BTreeSet<_>>();
        let expected_fine_owners = forward
            .global_stages
            .iter()
            .filter(|inputs| inputs.stage == stages[1].id)
            .flat_map(|inputs| {
                world_cells_covering_bounds(
                    inputs.inputs.read_bounds,
                    stages[0].owner_level,
                    resolved.graph.limits.max_global_stage_tiles,
                )
                .unwrap()
            })
            .collect::<BTreeSet<_>>();
        let cell_seeded_fine_owners = forward
            .cells
            .iter()
            .flat_map(|inputs| {
                world_cells_covering_bounds(
                    inputs.read_bounds,
                    stages[0].owner_level,
                    resolved.graph.limits.max_global_stage_tiles,
                )
                .unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(fine_owners, expected_fine_owners);
        assert!(fine_owners.len() > cell_seeded_fine_owners.len());
        assert!(
            forward
                .global_stages
                .iter()
                .filter(|inputs| inputs.stage == stages[0].id)
                .all(|inputs| {
                    inputs.inputs.read_bounds
                        == expand_bounds(inputs.solve_bounds, stages[0].upstream_halo).unwrap()
                        && inputs
                            .inputs
                            .regions
                            .iter()
                            .all(|region| region.seed_cell.level() == stages[0].minimum_input_level)
                })
        );
    }
}
