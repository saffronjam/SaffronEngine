//! Asset-server I/O for canonical vegetation assets and sparse map packages.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_scene::{AssetEntry, AssetType};
use saffron_spatial::{
    DecisionScalar, FieldChannel, FieldDerivative, LOCAL_TICKS_PER_METER, SurfaceField,
    WorldBounds, WorldCellKey, div_round_ties_even, world_cells_covering_bounds,
};
use saffron_vegetation::{
    BiomeAsset, BiomeGraphResolver, CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphUnit,
    EvaluationAnchor, EvaluationFieldSource, EvaluationFieldTile, EvaluationRegion,
    EvaluationRegionKind, EvaluationSpline, FieldBlendOperator, GlobalStageEvaluationInputs,
    GraphCompileOptions, GraphDependencyFingerprint, GraphDependencySource, GraphEvaluationInputs,
    GraphEvaluationJobInputs, PlantFamilyAsset, PlantPointColumns, PlantPrototype,
    QuantizedFieldTileValues, VegetationLayerOperator, VegetationMapAsset, VegetationMapChunk,
    VegetationMapChunkKey, VegetationMapChunkKind, VegetationMapChunkPayload,
    VegetationMapChunkReference, VegetationMapSnapshot, VegetationMapTileKey,
    VegetationMapTileSnapshot, canonical_surface_provider_set_hash, compile_biome_graph,
    read_biome_asset, read_plant_asset, read_vegetation_map_asset as decode_map,
    read_vegetation_map_chunk, vegetation_content_hash, write_biome_asset, write_plant_asset,
    write_vegetation_map_asset as encode_map, write_vegetation_map_chunk,
};

use crate::cook_reader::CookAssetAccess;
use crate::import::hash_bytes_fnv;
use crate::{AssetServer, Error, Result};

/// One validated native vegetation asset copied into the project catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationImport {
    /// Stable authored identity preserved from the source asset.
    pub id: Uuid,
    /// Unique catalog display name.
    pub name: String,
    /// Imported logical asset kind.
    pub asset_type: AssetType,
}

/// One optimistic multi-object authored map transaction.
#[derive(Clone, Debug)]
pub struct VegetationMapTransaction {
    /// Root generation captured before editing began.
    pub expected_generation: u64,
    /// Complete immutable replacements for logical object keys.
    pub upserts: Vec<VegetationMapChunk>,
    /// Logical object keys removed from the sparse root inventory.
    pub removals: Vec<saffron_vegetation::VegetationMapChunkKey>,
}

/// A catalog-resolved graph and the complete family prototype table it can emit.
#[derive(Clone, Debug)]
pub struct ResolvedBiomeGraph {
    /// Single compiled graph IR used by every evaluator surface.
    pub graph: CompiledBiomeGraph,
    /// Bound local biome instance, absent only for standalone schema/compile inspection.
    pub biome_instance: Option<u128>,
    /// Sorted authoritative family prototypes required by the graph and its modules.
    pub plant_prototypes: Vec<PlantPrototype>,
}

/// Catalog and canonical non-asset dependency resolver for biome compilation.
pub struct CatalogBiomeGraphResolver<'a> {
    assets: &'a AssetServer,
    external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
}

struct CookBiomeGraphResolver<'a> {
    assets: &'a dyn CookAssetAccess,
    external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
}

impl BiomeGraphResolver for CookBiomeGraphResolver<'_> {
    fn resolve_biome(
        &self,
        id: Uuid,
    ) -> std::result::Result<BiomeAsset, saffron_vegetation::Error> {
        load_biome_asset_from(self.assets, id)
            .map_err(|error| dependency_error(GraphDependencySource::Asset(id), error))
    }

    fn resolve_dependency_hash(
        &self,
        source: GraphDependencySource,
    ) -> std::result::Result<[u8; 32], saffron_vegetation::Error> {
        if let GraphDependencySource::Asset(id) = source {
            let entry = self.assets.catalog().find(id).ok_or_else(|| {
                saffron_vegetation::Error::GraphDocument {
                    path: dependency_path(source),
                    reason: "catalog asset is missing".to_owned(),
                }
            })?;
            let bytes = self
                .assets
                .read_file(&self.assets.root().join(&entry.path))
                .map_err(|error| dependency_error(source, error))?;
            return Ok(vegetation_content_hash(&bytes));
        }
        self.external_dependencies
            .get(&source)
            .copied()
            .ok_or_else(|| saffron_vegetation::Error::GraphDocument {
                path: dependency_path(source),
                reason: "canonical external dependency identity is missing".to_owned(),
            })
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        self.external_dependencies.keys().copied().collect()
    }
}

impl<'a> CatalogBiomeGraphResolver<'a> {
    /// Binds catalog assets and exact field/provider/map dependency identities.
    #[must_use]
    pub fn new(
        assets: &'a AssetServer,
        external_dependencies: &'a BTreeMap<GraphDependencySource, [u8; 32]>,
    ) -> Self {
        Self {
            assets,
            external_dependencies,
        }
    }
}

impl BiomeGraphResolver for CatalogBiomeGraphResolver<'_> {
    fn resolve_biome(
        &self,
        id: Uuid,
    ) -> std::result::Result<BiomeAsset, saffron_vegetation::Error> {
        load_biome_asset(self.assets, id)
            .map_err(|error| dependency_error(GraphDependencySource::Asset(id), error))
    }

    fn resolve_dependency_hash(
        &self,
        source: GraphDependencySource,
    ) -> std::result::Result<[u8; 32], saffron_vegetation::Error> {
        if let GraphDependencySource::Asset(id) = source {
            let entry = self.assets.catalog.find(id).ok_or_else(|| {
                saffron_vegetation::Error::GraphDocument {
                    path: dependency_path(source),
                    reason: "catalog asset is missing".to_owned(),
                }
            })?;
            let bytes = std::fs::read(self.assets.root.join(&entry.path))
                .map_err(|error| dependency_error(source, Error::Io(error.to_string())))?;
            return Ok(vegetation_content_hash(&bytes));
        }
        self.external_dependencies
            .get(&source)
            .copied()
            .ok_or_else(|| saffron_vegetation::Error::GraphDocument {
                path: dependency_path(source),
                reason: "canonical external dependency identity is missing".to_owned(),
            })
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        self.external_dependencies.keys().copied().collect()
    }
}

/// Compiles a catalog biome and resolves every family prototype reachable through its modules.
pub fn compile_catalog_biome_graph(
    assets: &AssetServer,
    biome: Uuid,
    root_bindings: &[(u128, serde_json::Value)],
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    compile_catalog_biome_graph_from(assets, biome, root_bindings, external_dependencies, options)
}

pub(crate) fn compile_catalog_biome_graph_from(
    assets: &dyn CookAssetAccess,
    biome: Uuid,
    root_bindings: &[(u128, serde_json::Value)],
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    let root = load_biome_asset_from(assets, biome)?;
    let resolver = CookBiomeGraphResolver {
        assets,
        external_dependencies,
    };
    let graph = compile_biome_graph(&root, root_bindings, &resolver, options)?;
    let mut families = BTreeSet::new();
    collect_graph_families(&graph.root, &mut families);
    let plant_prototypes = families
        .into_iter()
        .map(|family| {
            let asset = load_plant_family_asset_from(assets, Uuid(family))?;
            Ok(PlantPrototype::from_family(&asset)?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ResolvedBiomeGraph {
        graph,
        biome_instance: None,
        plant_prototypes,
    })
}

/// Compiles one map-local biome instance with its exact typed bindings.
pub fn compile_catalog_biome_instance_graph(
    assets: &AssetServer,
    map: Uuid,
    biome_instance: u128,
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    compile_catalog_biome_instance_graph_from(
        assets,
        map,
        biome_instance,
        external_dependencies,
        options,
    )
}

pub(crate) fn compile_catalog_biome_instance_graph_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    biome_instance: u128,
    external_dependencies: &BTreeMap<GraphDependencySource, [u8; 32]>,
    options: GraphCompileOptions,
) -> Result<ResolvedBiomeGraph> {
    let map_asset = load_vegetation_map_snapshot_from(assets, map)?;
    let instance = map_asset
        .biome_instances
        .iter()
        .find(|instance| instance.id == biome_instance)
        .ok_or_else(|| {
            Error::Io("vegetation biome instance is not present in the map".to_owned())
        })?;
    let mut resolved = compile_catalog_biome_graph_from(
        assets,
        instance.biome,
        &instance.bindings,
        external_dependencies,
        options,
    )?;
    resolved.biome_instance = Some(biome_instance);
    Ok(resolved)
}

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

/// Resolves canonical authored-map and surface-provider hashes for graph compilation.
pub fn vegetation_graph_dependency_hashes(
    assets: &AssetServer,
    map: Uuid,
    surface_providers: &[Arc<dyn SurfaceField>],
) -> Result<BTreeMap<GraphDependencySource, [u8; 32]>> {
    vegetation_graph_dependency_hashes_from(assets, map, surface_providers)
}

pub(crate) fn vegetation_graph_dependency_hashes_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    surface_providers: &[Arc<dyn SurfaceField>],
) -> Result<BTreeMap<GraphDependencySource, [u8; 32]>> {
    let map_asset = load_vegetation_map_snapshot_from(assets, map)?;
    let mut result = BTreeMap::new();
    let layer_ids = map_asset
        .layers
        .iter()
        .map(|layer| layer.id)
        .collect::<BTreeSet<_>>();
    for layer in &map_asset.layers {
        let reference = map_asset
            .inventory
            .iter()
            .find(|reference| {
                reference.key.layer == layer.id
                    && reference.key.kind == VegetationMapChunkKind::LayerMetadata
                    && reference.key.tile == VegetationMapTileKey::Global
            })
            .ok_or_else(|| Error::Io("vegetation-map layer object is missing".to_owned()))?;
        result.insert(
            GraphDependencySource::MapLayer(layer.id),
            reference.content_hash,
        );
    }
    for chunk in &map_asset.chunks {
        if let VegetationMapChunkPayload::Field(payload) = &chunk.payload {
            for field in payload.fields.iter().chain(&payload.blockers) {
                if !layer_ids.contains(&field.layer) {
                    return Err(Error::Io(
                        "authored field references a layer absent from its vegetation map"
                            .to_owned(),
                    ));
                }
                let channel = field.channel;
                let source = GraphDependencySource::Field(channel);
                let mut preimage = b"saffron-anima/map-field-contract/v1\0".to_vec();
                preimage.extend_from_slice(&source.canonical_bytes());
                result
                    .entry(source)
                    .or_insert_with(|| vegetation_content_hash(&preimage));
            }
        }
    }
    let mut ordered_providers = surface_providers.iter().collect::<Vec<_>>();
    ordered_providers.sort_by_key(|provider| provider.descriptor().id);
    for provider in ordered_providers {
        let descriptor = provider.descriptor();
        let hash = canonical_surface_provider_set_hash(&[Arc::clone(provider)], 1)?;
        if result
            .insert(
                GraphDependencySource::SurfaceProvider(descriptor.id.0),
                hash,
            )
            .is_some()
        {
            return Err(Error::Io(
                "surface provider identity is duplicated".to_owned(),
            ));
        }
        for channel in provider.field_channels() {
            let preimage = result
                .entry(GraphDependencySource::Field(channel))
                .or_insert_with(|| vegetation_content_hash(b"saffron-anima/surface-field/v1\0"));
            let mut bytes = preimage.to_vec();
            bytes.extend_from_slice(&descriptor.id.0.to_be_bytes());
            bytes.extend_from_slice(&hash);
            *preimage = vegetation_content_hash(&bytes);
        }
    }
    Ok(result)
}

fn intersect_bounds(left: WorldBounds, right: WorldBounds) -> Option<WorldBounds> {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    let minimum = std::array::from_fn(|axis| left_minimum[axis].max(right_minimum[axis]));
    let maximum = std::array::from_fn(|axis| left_maximum[axis].min(right_maximum[axis]));
    WorldBounds::new(minimum, maximum).ok()
}

fn expand_bounds(bounds: WorldBounds, halo: DecisionScalar) -> Result<WorldBounds> {
    let halo_ticks = i128::try_from(
        div_round_ties_even(
            i128::from(halo.bits())
                .checked_mul(i128::from(LOCAL_TICKS_PER_METER))
                .ok_or(Error::Vegetation(
                    saffron_vegetation::Error::NumericOverflow,
                ))?,
            65_536,
        )?
        .unsigned_abs(),
    )
    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut expanded_minimum = [0_i128; 3];
    let mut expanded_maximum = [0_i128; 3];
    for axis in 0..3 {
        expanded_minimum[axis] = minimum[axis]
            .checked_sub(halo_ticks)
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
        expanded_maximum[axis] = maximum[axis]
            .checked_add(halo_ticks)
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
    }
    Ok(WorldBounds::new(expanded_minimum, expanded_maximum)?)
}

fn canonical_hierarchical_regions(
    bounds: WorldBounds,
    level: u8,
    namespace: u128,
    limit: u64,
) -> Result<Vec<EvaluationRegion>> {
    world_cells_covering_bounds(bounds, level, limit)
        .map_err(|error| match error {
            saffron_spatial::Error::CellEnumerationLimit { requested, limit } => {
                Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                    resource: "input region cells",
                    requested,
                    limit,
                })
            }
            error => Error::Spatial(error),
        })?
        .into_iter()
        .map(|cell| {
            let cell_bounds = intersect_bounds(bounds, cell.bounds()).ok_or_else(|| {
                Error::Io(
                    "enumerated hierarchical region does not intersect its source bounds"
                        .to_owned(),
                )
            })?;
            let hash = vegetation_content_hash(
                &[
                    b"saffron-anima/hierarchical-region/v1\0".as_slice(),
                    namespace.to_be_bytes().as_slice(),
                    cell.canonical_bytes().as_slice(),
                ]
                .concat(),
            );
            Ok(EvaluationRegion {
                id: u128::from_be_bytes(hash[..16].try_into().unwrap()),
                kind: EvaluationRegionKind::Biome,
                layer: namespace,
                hierarchy_namespace: Some(namespace),
                seed_cell: cell,
                bounds: cell_bounds,
            })
        })
        .collect()
}

fn insert_global_owners(
    read_bounds: WorldBounds,
    owner_level: u8,
    stage_index: usize,
    owners_by_stage: &mut [BTreeSet<WorldCellKey>],
    global_tile_count: &mut u64,
    limit: u64,
) -> Result<()> {
    let owners =
        world_cells_covering_bounds(read_bounds, owner_level, limit).map_err(
            |error| match error {
                saffron_spatial::Error::CellEnumerationLimit { requested, limit } => {
                    Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                        resource: "global stage tiles",
                        requested,
                        limit,
                    })
                }
                error => Error::Spatial(error),
            },
        )?;
    let stage_owners = owners_by_stage
        .get_mut(stage_index)
        .ok_or_else(|| Error::Io("compiled global-stage index is invalid".to_owned()))?;
    for owner in owners {
        if !stage_owners.insert(owner) {
            continue;
        }
        *global_tile_count = global_tile_count.checked_add(1).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
        if *global_tile_count > limit {
            return Err(Error::Vegetation(saffron_vegetation::Error::GraphLimit {
                resource: "global stage tiles",
                requested: *global_tile_count,
                limit,
            }));
        }
    }
    Ok(())
}

fn global_stage_input_snapshot(
    stage: &CompiledGlobalStage,
    inputs: &GraphEvaluationInputs,
    prerequisite_snapshots: &[([u8; 32], WorldCellKey, [u8; 32])],
) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-global-stage-input/v1\0".to_vec();
    bytes.extend_from_slice(&stage.id);
    bytes.push(stage.owner_level);
    bytes.push(stage.minimum_input_level);
    bytes.extend_from_slice(&stage.upstream_halo.bits().to_be_bytes());
    bytes.extend_from_slice(&inputs.map.value().to_be_bytes());
    bytes.extend_from_slice(&inputs.biome_instance.to_be_bytes());
    bytes.extend_from_slice(&inputs.output_cell.canonical_bytes());
    append_bounds(&mut bytes, inputs.output_bounds);
    append_bounds(&mut bytes, inputs.read_bounds);
    bytes.extend_from_slice(&inputs.ecology_tick.to_be_bytes());

    bytes.extend_from_slice(&(stage.dependencies.len() as u64).to_be_bytes());
    for dependency in &stage.dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    bytes.extend_from_slice(&(prerequisite_snapshots.len() as u64).to_be_bytes());
    for (prerequisite_stage, prerequisite_owner, prerequisite_snapshot) in prerequisite_snapshots {
        bytes.extend_from_slice(prerequisite_stage);
        bytes.extend_from_slice(&prerequisite_owner.canonical_bytes());
        bytes.extend_from_slice(prerequisite_snapshot);
    }

    bytes.extend_from_slice(&(inputs.regions.len() as u64).to_be_bytes());
    for region in &inputs.regions {
        bytes.extend_from_slice(&region.id.to_be_bytes());
        bytes.push(match region.kind {
            EvaluationRegionKind::Biome => 0,
            EvaluationRegionKind::Shape => 1,
        });
        bytes.extend_from_slice(&region.layer.to_be_bytes());
        append_optional_u128(&mut bytes, region.hierarchy_namespace);
        bytes.extend_from_slice(&region.seed_cell.canonical_bytes());
        append_bounds(&mut bytes, region.bounds);
    }

    bytes.extend_from_slice(&(inputs.splines.len() as u64).to_be_bytes());
    for spline in &inputs.splines {
        bytes.extend_from_slice(&spline.id.to_be_bytes());
        bytes.extend_from_slice(&spline.layer.to_be_bytes());
        bytes.extend_from_slice(&(spline.points.len() as u64).to_be_bytes());
        for point in &spline.points {
            append_world_ticks(&mut bytes, point.global_ticks());
        }
    }

    bytes.extend_from_slice(&(inputs.anchors.len() as u64).to_be_bytes());
    for anchor in &inputs.anchors {
        bytes.extend_from_slice(&anchor.layer.to_be_bytes());
    }
    let anchor_points = inputs
        .anchors
        .iter()
        .map(|anchor| anchor.point.clone())
        .collect::<Vec<_>>();
    let anchor_bytes = PlantPointColumns::from_points(anchor_points)?.canonical_bytes()?;
    bytes.extend_from_slice(&(anchor_bytes.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&anchor_bytes);

    bytes.extend_from_slice(&(inputs.plant_prototypes.len() as u64).to_be_bytes());
    for prototype in &inputs.plant_prototypes {
        bytes.extend_from_slice(&prototype.family.value().to_be_bytes());
        for radius in prototype.crown_radius {
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
        for radius in prototype.root_radius {
            bytes.extend_from_slice(&radius.bits().to_be_bytes());
        }
        for bound in prototype.local_bounds_min {
            bytes.extend_from_slice(&bound.bits().to_be_bytes());
        }
        for bound in prototype.local_bounds_max {
            bytes.extend_from_slice(&bound.bits().to_be_bytes());
        }
        bytes.extend_from_slice(&prototype.shade_tolerance.bits().to_be_bytes());
    }

    bytes.extend_from_slice(&(inputs.fields.len() as u64).to_be_bytes());
    for field in &inputs.fields {
        append_field_source(&mut bytes, field.source);
        append_field_channel(&mut bytes, field.channel);
        bytes.push(match field.derivative {
            FieldDerivative::Value => 0,
            FieldDerivative::Gradient => 1,
            FieldDerivative::Hessian => 2,
        });
        bytes.push(match field.blend {
            FieldBlendOperator::Replace => 0,
            FieldBlendOperator::Add => 1,
            FieldBlendOperator::Multiply => 2,
            FieldBlendOperator::Minimum => 3,
            FieldBlendOperator::Maximum => 4,
        });
        bytes.extend_from_slice(&field.weight.bits().to_be_bytes());
        bytes.extend_from_slice(&field.layer_order.0.to_be_bytes());
        bytes.extend_from_slice(&field.layer_order.1.to_be_bytes());
        bytes.extend_from_slice(&field.source_hash);
        append_bounds(&mut bytes, field.bounds);
        for dimension in field.dimensions {
            bytes.extend_from_slice(&dimension.to_be_bytes());
        }
        match &field.values {
            QuantizedFieldTileValues::Scalar(values) => {
                bytes.push(0);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            QuantizedFieldTileValues::Gradient(values) => {
                bytes.push(1);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    for lane in value {
                        bytes.extend_from_slice(&lane.to_be_bytes());
                    }
                }
            }
            QuantizedFieldTileValues::Hessian(values) => {
                bytes.push(2);
                bytes.extend_from_slice(&(values.len() as u64).to_be_bytes());
                for value in values {
                    for lane in value {
                        bytes.extend_from_slice(&lane.to_be_bytes());
                    }
                }
            }
        }
    }

    bytes.extend_from_slice(&(inputs.surface_projection_tiles.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&(inputs.surface_field_query_tiles.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&inputs.surface_provider_set_hash);
    Ok(vegetation_content_hash(&bytes))
}

fn append_dependency_source(bytes: &mut Vec<u8>, source: GraphDependencySource) {
    match source {
        GraphDependencySource::Asset(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.value().to_be_bytes());
        }
        GraphDependencySource::Field(channel) => {
            bytes.push(1);
            append_field_channel(bytes, channel);
        }
        GraphDependencySource::SurfaceProvider(provider) => {
            bytes.push(2);
            bytes.extend_from_slice(&provider.to_be_bytes());
        }
        GraphDependencySource::MapLayer(layer) => {
            bytes.push(3);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
    }
}

fn append_field_source(bytes: &mut Vec<u8>, source: EvaluationFieldSource) {
    match source {
        EvaluationFieldSource::MapLayer(layer) => {
            bytes.push(0);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
        EvaluationFieldSource::SurfaceProvider { provider, revision } => {
            bytes.push(1);
            bytes.extend_from_slice(&provider.0.to_be_bytes());
            bytes.extend_from_slice(&revision.0.to_be_bytes());
        }
    }
}

fn append_field_channel(bytes: &mut Vec<u8>, channel: FieldChannel) {
    let (tag, user) = match channel {
        FieldChannel::Altitude => (0, None),
        FieldChannel::Slope => (1, None),
        FieldChannel::Curvature => (2, None),
        FieldChannel::Concavity => (3, None),
        FieldChannel::Drainage => (4, None),
        FieldChannel::Moisture => (5, None),
        FieldChannel::Temperature => (6, None),
        FieldChannel::Precipitation => (7, None),
        FieldChannel::Sunlight => (8, None),
        FieldChannel::Exposure => (9, None),
        FieldChannel::WaterDistance => (10, None),
        FieldChannel::WaterDepth => (11, None),
        FieldChannel::SignedBlocker => (12, None),
        FieldChannel::SplineDistance => (13, None),
        FieldChannel::User(id) => (14, Some(id)),
    };
    bytes.push(tag);
    if let Some(user) = user {
        bytes.extend_from_slice(&user.to_be_bytes());
    }
}

fn append_optional_u128(bytes: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn append_bounds(bytes: &mut Vec<u8>, bounds: WorldBounds) {
    append_world_ticks(bytes, bounds.min_ticks());
    append_world_ticks(bytes, bounds.max_ticks_exclusive());
}

fn append_world_ticks(bytes: &mut Vec<u8>, ticks: [i128; 3]) {
    for tick in ticks {
        bytes.extend_from_slice(&tick.to_be_bytes());
    }
}

fn collect_graph_families(unit: &CompiledGraphUnit, families: &mut BTreeSet<u64>) {
    families.extend(unit.palette.iter().map(|entry| entry.plant.value()));
    for node in &unit.nodes {
        if let Some(module) = node.module.as_deref() {
            collect_graph_families(module, families);
        }
    }
}

fn dependency_path(source: GraphDependencySource) -> String {
    match source {
        GraphDependencySource::Asset(id) => format!("dependencies.asset.{}", id.value()),
        GraphDependencySource::Field(channel) => format!("dependencies.field.{channel:?}"),
        GraphDependencySource::SurfaceProvider(provider) => {
            format!("dependencies.surfaceProvider.{provider}")
        }
        GraphDependencySource::MapLayer(layer) => {
            format!("dependencies.mapLayer.{layer:032x}")
        }
    }
}

fn dependency_error(source: GraphDependencySource, error: Error) -> saffron_vegetation::Error {
    saffron_vegetation::Error::GraphDocument {
        path: dependency_path(source),
        reason: error.to_string(),
    }
}

/// Imports one authored `.splant`, `.sbiome`, or complete `.svegmap` package.
pub fn import_vegetation_asset(
    assets: &mut AssetServer,
    source: impl AsRef<Path>,
    folder: &str,
) -> Result<VegetationImport> {
    let source = source.as_ref();
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let bytes = std::fs::read(source).map_err(|error| Error::Io(error.to_string()))?;
    let (id, asset_type) = match extension.as_str() {
        "splant" => {
            let asset = read_plant_asset(&bytes)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::Plant,
                format!("vegetation/plants/{}.splant", asset.id.value()),
                &bytes,
            )?;
            (id, AssetType::Plant)
        }
        "sbiome" => {
            let asset = read_biome_asset(&bytes)?;
            validate_biome_cycles(assets, &asset)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::Biome,
                format!("vegetation/biomes/{}.sbiome", asset.id.value()),
                &bytes,
            )?;
            (id, AssetType::Biome)
        }
        "svegmap" => {
            let asset = decode_map(&bytes)?;
            let chunks = read_source_map_chunks(source, &asset)?;
            let path = format!("vegetation/maps/{}.svegmap", asset.id.value());
            let package = assets.root.join(map_package_directory(&path));
            if package.exists() {
                return Err(Error::Io(format!(
                    "vegetation asset identity {} already has a map package in the project",
                    asset.id.value()
                )));
            }
            publish_imported_map_objects(&package, &asset, &chunks)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::VegetationMap,
                path.clone(),
                &bytes,
            )
            .inspect_err(|_| {
                let _ = std::fs::remove_dir_all(&package);
            })?;
            (id, AssetType::VegetationMap)
        }
        "splantc" | "svegcell" => {
            return Err(Error::Io(
                "generated vegetation artifacts cannot be imported".to_owned(),
            ));
        }
        _ => {
            return Err(Error::Io(
                "expected an authored .splant, .sbiome, or .svegmap asset".to_owned(),
            ));
        }
    };
    let name = assets
        .catalog
        .find(id)
        .map(|entry| entry.name.clone())
        .ok_or(Error::NotInCatalog(id.value()))?;
    Ok(VegetationImport {
        id,
        name,
        asset_type,
    })
}

/// Reads a complete `.splant` from the catalog.
pub fn load_plant_family_asset(assets: &AssetServer, id: Uuid) -> Result<PlantFamilyAsset> {
    load_plant_family_asset_from(assets, id)
}

pub(crate) fn load_plant_family_asset_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<PlantFamilyAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::Plant, "plant")?;
    Ok(read_plant_asset(&bytes)?)
}

/// Writes a new `.splant` and registers it in the catalog.
pub fn save_plant_family_asset(
    assets: &mut AssetServer,
    mut asset: PlantFamilyAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    let bytes = write_plant_asset(&asset)?;
    save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::Plant,
        format!("vegetation/plants/{}.splant", asset.id.value()),
        &bytes,
    )
}

/// Rewrites an existing `.splant` without changing its catalog identity.
pub fn update_plant_family_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset: &PlantFamilyAsset,
) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "plant-family identity does not match catalog row".to_owned(),
        ));
    }
    let bytes = write_plant_asset(asset)?;
    update_typed_asset(assets, id, AssetType::Plant, "plant", &bytes)
}

/// Reads a complete `.sbiome` from the catalog.
pub fn load_biome_asset(assets: &AssetServer, id: Uuid) -> Result<BiomeAsset> {
    load_biome_asset_from(assets, id)
}

pub(crate) fn load_biome_asset_from(assets: &dyn CookAssetAccess, id: Uuid) -> Result<BiomeAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::Biome, "biome")?;
    Ok(read_biome_asset(&bytes)?)
}

/// Writes a new `.sbiome` and registers it in the catalog.
pub fn save_biome_asset(
    assets: &mut AssetServer,
    mut asset: BiomeAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    validate_biome_cycles(assets, &asset)?;
    let bytes = write_biome_asset(&asset)?;
    save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::Biome,
        format!("vegetation/biomes/{}.sbiome", asset.id.value()),
        &bytes,
    )
}

/// Rewrites an existing `.sbiome` without changing its catalog identity.
pub fn update_biome_asset(assets: &mut AssetServer, id: Uuid, asset: &BiomeAsset) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "biome identity does not match catalog row".to_owned(),
        ));
    }
    validate_biome_cycles(assets, asset)?;
    let bytes = write_biome_asset(asset)?;
    update_typed_asset(assets, id, AssetType::Biome, "biome", &bytes)
}

/// Reads the atomically visible `.svegmap` root from the catalog.
pub fn load_vegetation_map_root(assets: &AssetServer, id: Uuid) -> Result<VegetationMapAsset> {
    load_vegetation_map_root_from(assets, id)
}

pub(crate) fn load_vegetation_map_root_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<VegetationMapAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    Ok(decode_map(&bytes)?)
}

/// Resolves one complete authored map snapshot from its root and immutable object inventory.
pub fn load_vegetation_map_snapshot(
    assets: &AssetServer,
    id: Uuid,
) -> Result<VegetationMapSnapshot> {
    load_vegetation_map_snapshot_from(assets, id)
}

/// Reads the authored chunks for the requested logical keys; a key with no
/// inventory entry contributes no row.
pub fn load_vegetation_map_chunks(
    assets: &AssetServer,
    id: Uuid,
    keys: &[VegetationMapChunkKey],
) -> Result<Vec<VegetationMapChunk>> {
    let entry = typed_entry_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, id)?;
    let package = assets.root().join(map_package_directory(&entry.path));
    let references = root
        .inventory
        .iter()
        .map(|reference| (reference.key, *reference))
        .collect::<BTreeMap<_, _>>();
    let mut chunks = Vec::new();
    for key in keys {
        if let Some(reference) = references.get(key) {
            chunks.push(read_map_object_from(assets, &package, id, reference)?);
        }
    }
    Ok(chunks)
}

pub(crate) fn load_vegetation_map_snapshot_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<VegetationMapSnapshot> {
    let entry = typed_entry_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, id)?;
    let package = assets.root().join(map_package_directory(&entry.path));
    let mut chunks = Vec::with_capacity(root.inventory.len());
    for reference in &root.inventory {
        chunks.push(read_map_object_from(assets, &package, id, reference)?);
    }
    let mut layers = Vec::new();
    let mut biome_instances = Vec::new();
    let mut brush_history = Vec::new();
    for chunk in &chunks {
        match &chunk.payload {
            VegetationMapChunkPayload::LayerMetadata(layer) => layers.push(layer.clone()),
            VegetationMapChunkPayload::GraphInstance(instance) => {
                biome_instances.push(instance.clone());
            }
            VegetationMapChunkPayload::EditorMetadata(gestures) => {
                brush_history.extend(gestures.iter().cloned());
            }
            VegetationMapChunkPayload::Field(_) | VegetationMapChunkPayload::AnchorOverride(_) => {}
        }
    }
    layers.sort_by_key(saffron_vegetation::VegetationLayer::order_key);
    biome_instances.sort_by_key(|instance| instance.id);
    brush_history.sort_by_key(|gesture| (gesture.layer, gesture.gesture));
    let layer_ids = layers.iter().map(|layer| layer.id).collect::<BTreeSet<_>>();
    if chunks.iter().any(|chunk| match chunk.key.kind {
        VegetationMapChunkKind::Field
        | VegetationMapChunkKind::AnchorOverride
        | VegetationMapChunkKind::EditorMetadata => !layer_ids.contains(&chunk.key.layer),
        VegetationMapChunkKind::GraphInstance | VegetationMapChunkKind::LayerMetadata => false,
    }) {
        return Err(Error::Io(
            "vegetation-map object references missing layer metadata".to_owned(),
        ));
    }
    Ok(VegetationMapSnapshot {
        root,
        layers,
        biome_instances,
        brush_history,
        chunks,
    })
}

/// Writes a new empty `.svegmap` root and registers it.
pub fn save_vegetation_map_asset(
    assets: &mut AssetServer,
    mut asset: VegetationMapAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    if asset.generation != 0 || !asset.inventory.is_empty() {
        return Err(Error::Io(
            "a new vegetation-map root must have generation zero and an empty inventory".to_owned(),
        ));
    }
    let bytes = encode_map(&asset)?;
    let path = format!("vegetation/maps/{}.svegmap", asset.id.value());
    let id = save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::VegetationMap,
        path.clone(),
        &bytes,
    )?;
    if let Err(error) = std::fs::create_dir_all(
        assets
            .root
            .join(map_package_directory(&path))
            .join("objects"),
    ) {
        rollback_new_asset(assets, id);
        return Err(Error::Io(error.to_string()));
    }
    Ok(id)
}

/// Atomically updates root metadata while retaining the committed object inventory.
pub fn update_vegetation_map_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset: &VegetationMapAsset,
) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "vegetation-map identity does not match catalog row".to_owned(),
        ));
    }
    let entry = typed_entry(assets, id, AssetType::VegetationMap, "vegetation map")?.clone();
    let package = assets.root.join(map_package_directory(&entry.path));
    std::fs::create_dir_all(&package).map_err(|error| Error::Io(error.to_string()))?;
    let _lock = lock_map_package(&package)?;
    let current = load_vegetation_map_root(assets, id)?;
    if asset.generation != current.generation
        || asset.inventory != current.inventory
        || asset.chunk_layout != current.chunk_layout
    {
        return Err(Error::Io(
            "vegetation-map root metadata update is based on a stale or altered inventory"
                .to_owned(),
        ));
    }
    let mut next = current;
    next.name.clone_from(&asset.name);
    next.bounds = asset.bounds;
    next.generation = next.generation.checked_add(1).ok_or(Error::Vegetation(
        saffron_vegetation::Error::NumericOverflow,
    ))?;
    let bytes = encode_map(&next)?;
    atomic_write(&assets.root.join(&entry.path), &bytes)?;
    let content_hash = hash_bytes_fnv(&bytes);
    let updated = assets.update_asset_content_hash(id, content_hash);
    debug_assert!(updated, "validated vegetation map remains catalogued");
    Ok(())
}

/// Commits one optimistic authored-map transaction and publishes its root last.
pub fn commit_vegetation_map_transaction(
    assets: &mut AssetServer,
    map: Uuid,
    transaction: VegetationMapTransaction,
) -> Result<()> {
    let entry = typed_entry(assets, map, AssetType::VegetationMap, "vegetation map")?.clone();
    let package = assets.root.join(map_package_directory(&entry.path));
    let objects = package.join("objects");
    std::fs::create_dir_all(&objects).map_err(|error| Error::Io(error.to_string()))?;
    let _lock = lock_map_package(&package)?;
    let mut root = load_vegetation_map_root(assets, map)?;
    if root.generation != transaction.expected_generation {
        return Err(Error::VegetationMapGenerationConflict {
            expected: transaction.expected_generation,
            actual: root.generation,
        });
    }

    let mut keys = BTreeSet::new();
    let mut encoded = Vec::with_capacity(transaction.upserts.len());
    for chunk in &transaction.upserts {
        if chunk.map != map || !keys.insert(chunk.key) {
            return Err(Error::Io(
                "map object identity or batch key uniqueness is invalid".to_owned(),
            ));
        }
        if let VegetationMapTileKey::Cell(cell) = chunk.key.tile
            && cell.level() != root.chunk_layout.level
        {
            return Err(Error::Io(
                "map object cell level does not match its root".to_owned(),
            ));
        }
        let bytes = write_vegetation_map_chunk(chunk)?;
        let reference = map_object_reference(chunk, &bytes)?;
        encoded.push((reference, bytes));
    }
    let mut removals = transaction.removals;
    removals.sort_unstable();
    if removals.iter().any(|key| !keys.insert(*key))
        || removals.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(Error::Io(
            "map transaction contains duplicate or contradictory logical keys".to_owned(),
        ));
    }
    encoded.sort_by_key(|(reference, _)| reference.order_key());

    let previous = root
        .inventory
        .iter()
        .map(|reference| (reference.key, *reference))
        .collect::<BTreeMap<_, _>>();
    let mut inventory = previous.clone();
    for key in removals {
        inventory.remove(&key);
    }
    let mut published = Vec::new();
    for (reference, bytes) in &encoded {
        let path = map_object_path(&package, &reference.content_hash);
        if path.exists() {
            let existing = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    rollback_map_objects(&published);
                    return Err(Error::Io(error.to_string()));
                }
            };
            if existing != *bytes {
                rollback_map_objects(&published);
                return Err(Error::VegetationMapObjectCollision {
                    path: path.display().to_string(),
                });
            }
            read_map_object_from(assets, &package, map, reference)?;
        } else if let Err(error) = atomic_write(&path, bytes) {
            rollback_map_objects(&published);
            return Err(error);
        } else {
            published.push(path);
        }
        inventory.insert(reference.key, *reference);
    }
    let layer_ids = inventory
        .keys()
        .filter(|key| key.kind == VegetationMapChunkKind::LayerMetadata)
        .map(|key| key.layer)
        .collect::<BTreeSet<_>>();
    if inventory.keys().any(|key| {
        matches!(
            key.kind,
            VegetationMapChunkKind::Field
                | VegetationMapChunkKind::AnchorOverride
                | VegetationMapChunkKind::EditorMetadata
        ) && !layer_ids.contains(&key.layer)
    }) {
        rollback_map_objects(&published);
        return Err(Error::Io(
            "vegetation-map transaction leaves an object without layer metadata".to_owned(),
        ));
    }
    if inventory == previous {
        rollback_map_objects(&published);
        return Ok(());
    }
    root.inventory = inventory.into_values().collect();
    root.generation = root.generation.checked_add(1).ok_or(Error::Vegetation(
        saffron_vegetation::Error::NumericOverflow,
    ))?;
    let root_bytes = match encode_map(&root) {
        Ok(bytes) => bytes,
        Err(error) => {
            rollback_map_objects(&published);
            return Err(error.into());
        }
    };
    if let Err(error) = atomic_write(&assets.root.join(&entry.path), &root_bytes) {
        rollback_map_objects(&published);
        return Err(error);
    }
    assets
        .catalog
        .set_content_hash(map, hash_bytes_fnv(&root_bytes));
    Ok(())
}

/// Resolves all typed authored objects that contribute to one spatial map tile.
pub fn load_vegetation_map_tile_snapshot(
    assets: &AssetServer,
    map: Uuid,
    cell: WorldCellKey,
) -> Result<Option<VegetationMapTileSnapshot>> {
    load_vegetation_map_tile_snapshot_from(assets, map, cell)
}

fn load_vegetation_map_tile_snapshot_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    cell: WorldCellKey,
) -> Result<Option<VegetationMapTileSnapshot>> {
    let entry = typed_entry_from(assets, map, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, map)?;
    if cell.level() != root.chunk_layout.level {
        return Err(Error::Io(
            "requested vegetation-map tile level does not match its root".to_owned(),
        ));
    }
    let package = assets.root().join(map_package_directory(&entry.path));
    let references = root
        .inventory
        .iter()
        .filter(|reference| reference.key.tile == VegetationMapTileKey::Cell(cell));
    let mut snapshot = VegetationMapTileSnapshot {
        map,
        cell,
        fields: Vec::new(),
        blockers: Vec::new(),
        explicit_plants: Vec::new(),
        pins: Vec::new(),
        transform_overrides: Vec::new(),
        state_overrides: Vec::new(),
        provenance: Default::default(),
    };
    let mut found = false;
    for reference in references {
        found = true;
        let chunk = read_map_object(&package, map, reference)?;
        match chunk.payload {
            VegetationMapChunkPayload::Field(payload) => {
                snapshot.fields.extend(payload.fields);
                snapshot.blockers.extend(payload.blockers);
            }
            VegetationMapChunkPayload::AnchorOverride(payload) => {
                let handles = (0..payload.provenance.records().len())
                    .map(|index| saffron_vegetation::ProvenanceHandle(index as u32))
                    .collect::<Vec<_>>();
                let remap = snapshot
                    .provenance
                    .import_fragment(&payload.provenance, &handles)?;
                for mut anchor in payload.explicit_plants {
                    let source = saffron_vegetation::ProvenanceHandle(anchor.point.provenance);
                    anchor.point.provenance = remap
                        .records
                        .get(&source)
                        .ok_or_else(|| {
                            Error::Io("explicit map anchor provenance was not imported".to_owned())
                        })?
                        .0;
                    snapshot.explicit_plants.push(anchor);
                }
                snapshot.pins.extend(payload.pins);
                snapshot
                    .transform_overrides
                    .extend(payload.transform_overrides);
                snapshot.state_overrides.extend(payload.state_overrides);
            }
            VegetationMapChunkPayload::GraphInstance(_)
            | VegetationMapChunkPayload::LayerMetadata(_)
            | VegetationMapChunkPayload::EditorMetadata(_) => {
                return Err(Error::Io(
                    "global vegetation-map metadata was addressed as a spatial tile".to_owned(),
                ));
            }
        }
    }
    if !found {
        return Ok(None);
    }
    snapshot
        .fields
        .sort_by_key(|field| (field.layer, field.channel));
    snapshot
        .blockers
        .sort_by_key(|field| (field.layer, field.channel));
    snapshot.explicit_plants.sort_by_key(|anchor| anchor.id);
    snapshot.pins.sort_unstable();
    snapshot
        .transform_overrides
        .sort_by_key(|value| value.plant);
    snapshot.state_overrides.sort_by_key(|value| value.plant);
    if snapshot
        .explicit_plants
        .windows(2)
        .any(|pair| pair[0].id == pair[1].id)
        || snapshot.pins.windows(2).any(|pair| pair[0] == pair[1])
        || snapshot
            .transform_overrides
            .windows(2)
            .any(|pair| pair[0].plant == pair[1].plant)
        || snapshot
            .state_overrides
            .windows(2)
            .any(|pair| pair[0].plant == pair[1].plant)
    {
        return Err(Error::Io(
            "vegetation-map tile contains duplicate cross-layer identities".to_owned(),
        ));
    }
    Ok(Some(snapshot))
}

/// Removes the internal authored chunk directory owned by a vegetation-map catalog row.
pub fn remove_vegetation_map_package(assets: &AssetServer, entry: &AssetEntry) -> Result<()> {
    if entry.asset_type != AssetType::VegetationMap || entry.path.is_empty() {
        return Ok(());
    }
    let directory = assets.root.join(map_package_directory(&entry.path));
    match std::fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error.to_string())),
    }
}

pub(crate) fn vegetation_map_package_bytes(assets: &AssetServer, entry: &AssetEntry) -> u64 {
    let manifest = assets.root.join(&entry.path);
    let mut bytes = std::fs::metadata(&manifest).map_or(0, |metadata| metadata.len());
    let objects = assets
        .root
        .join(map_package_directory(&entry.path))
        .join("objects");
    if let Ok(entries) = std::fs::read_dir(objects) {
        for object in entries.filter_map(std::result::Result::ok) {
            if object
                .path()
                .extension()
                .is_some_and(|extension| extension == "svegmapc")
            {
                bytes =
                    bytes.saturating_add(object.metadata().map_or(0, |metadata| metadata.len()));
            }
        }
    }
    bytes
}

pub(crate) fn vegetation_map_dependencies(
    assets: &AssetServer,
    entry: &AssetEntry,
) -> Result<Vec<Uuid>> {
    let map = load_vegetation_map_snapshot(assets, entry.id)?;
    let mut dependencies: BTreeSet<u64> = map
        .biome_instances
        .iter()
        .map(|instance| instance.biome.value())
        .collect();
    for layer in &map.layers {
        if let saffron_vegetation::VegetationLayerOperator::SpeciesWeights(weights) =
            &layer.operator
        {
            dependencies.extend(weights.iter().map(|weight| weight.family.value()));
        }
    }

    for chunk in map.chunks {
        if let VegetationMapChunkPayload::AnchorOverride(payload) = chunk.payload {
            dependencies.extend(
                payload
                    .explicit_plants
                    .into_iter()
                    .map(|anchor| anchor.family.value()),
            );
        }
    }
    dependencies.remove(&0);
    Ok(dependencies.into_iter().map(Uuid).collect())
}

fn read_typed_asset_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
) -> Result<Vec<u8>> {
    let entry = typed_entry_from(assets, id, asset_type, wanted)?;
    assets.read_file(&assets.root().join(&entry.path))
}

fn typed_entry<'a>(
    assets: &'a AssetServer,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
) -> Result<&'a AssetEntry> {
    typed_entry_from(assets, id, asset_type, wanted)
}

fn typed_entry_from<'a>(
    assets: &'a dyn CookAssetAccess,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
) -> Result<&'a AssetEntry> {
    let entry = assets
        .catalog()
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != asset_type {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted,
        });
    }
    Ok(entry)
}

fn save_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    name: &str,
    folder: &str,
    asset_type: AssetType,
    relative_path: String,
    bytes: &[u8],
) -> Result<Uuid> {
    assets.ensure_asset_directories();
    atomic_write(&assets.root.join(&relative_path), bytes)?;
    let unique_name = assets.catalog().unique_name(name);
    assets.register_imported_asset(AssetEntry {
        id,
        name: unique_name,
        asset_type,
        path: relative_path.clone(),
        folder: folder.to_owned(),
        content_hash: hash_bytes_fnv(bytes),
        ..AssetEntry::default()
    });
    if let Err(error) = assets.write_asset_sidecar(id) {
        let _ = assets.delete_asset_entry(id);
        let _ = std::fs::remove_file(assets.root.join(&relative_path));
        return Err(error);
    }
    Ok(id)
}

fn import_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    name: &str,
    folder: &str,
    asset_type: AssetType,
    relative_path: String,
    bytes: &[u8],
) -> Result<Uuid> {
    if id.value() < 1024 {
        return Err(Error::Io(
            "authored vegetation asset identity is in the reserved range".to_owned(),
        ));
    }
    if assets.catalog().find(id).is_some() || assets.root.join(&relative_path).exists() {
        return Err(Error::Io(format!(
            "vegetation asset identity {} already exists in the project",
            id.value()
        )));
    }
    save_typed_asset(assets, id, name, folder, asset_type, relative_path, bytes)
}

fn update_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
    bytes: &[u8],
) -> Result<()> {
    let path = typed_entry(assets, id, asset_type, wanted)?.path.clone();
    atomic_write(&assets.root.join(path), bytes)?;
    let content_hash = hash_bytes_fnv(bytes);
    let updated = assets.update_asset_content_hash(id, content_hash);
    debug_assert!(updated, "validated vegetation asset remains catalogued");
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = AtomicWriteFile::options()
        .open(path)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.commit().map_err(|error| Error::Io(error.to_string()))
}

fn read_source_map_chunks(
    source: &Path,
    map: &VegetationMapAsset,
) -> Result<Vec<VegetationMapChunk>> {
    let package = map_package_path(source);
    if map.inventory.is_empty() && !package.exists() {
        return Ok(Vec::new());
    }
    if !package.is_dir() {
        return Err(Error::Io(
            "vegetation-map object package is missing or is not a directory".to_owned(),
        ));
    }
    let mut chunks = Vec::with_capacity(map.inventory.len());
    for reference in &map.inventory {
        chunks.push(read_map_object(&package, map.id, reference)?);
    }
    Ok(chunks)
}

fn publish_imported_map_objects(
    package: &Path,
    map: &VegetationMapAsset,
    chunks: &[VegetationMapChunk],
) -> Result<()> {
    if chunks.len() != map.inventory.len() {
        return Err(Error::Io(
            "vegetation-map import object count does not match its root inventory".to_owned(),
        ));
    }
    std::fs::create_dir_all(package.join("objects"))
        .map_err(|error| Error::Io(error.to_string()))?;
    for (reference, chunk) in map.inventory.iter().zip(chunks) {
        let bytes = write_vegetation_map_chunk(chunk)?;
        if map_object_reference(chunk, &bytes)? != *reference {
            let _ = std::fs::remove_dir_all(package);
            return Err(Error::Io(
                "vegetation-map import object does not match its root inventory".to_owned(),
            ));
        }
        if let Err(error) = atomic_write(&map_object_path(package, &reference.content_hash), &bytes)
        {
            let _ = std::fs::remove_dir_all(package);
            return Err(error);
        }
    }
    Ok(())
}

fn map_object_reference(
    chunk: &VegetationMapChunk,
    bytes: &[u8],
) -> Result<VegetationMapChunkReference> {
    Ok(VegetationMapChunkReference {
        key: chunk.key,
        content_hash: vegetation_content_hash(bytes),
        byte_length: u64::try_from(bytes.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
        revision: chunk.revision,
    })
}

fn read_map_object(
    package: &Path,
    map: Uuid,
    reference: &VegetationMapChunkReference,
) -> Result<VegetationMapChunk> {
    let path = map_object_path(package, &reference.content_hash);
    let bytes = std::fs::read(&path).map_err(|error| Error::Io(error.to_string()))?;
    validate_map_object_bytes(map, reference, &bytes)
}

fn validate_map_object_bytes(
    map: Uuid,
    reference: &VegetationMapChunkReference,
    bytes: &[u8],
) -> Result<VegetationMapChunk> {
    if u64::try_from(bytes.len()).ok() != Some(reference.byte_length) {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactFormat {
                format: ".svegmap object",
                field: "root.inventory.byteLength".to_owned(),
            },
        ));
    }
    if vegetation_content_hash(bytes) != reference.content_hash {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactHashMismatch {
                format: ".svegmap object",
                subject: "root.inventory.contentHash".to_owned(),
            },
        ));
    }
    let chunk = read_vegetation_map_chunk(bytes)?;
    if chunk.map != map || chunk.key != reference.key || chunk.revision != reference.revision {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactFormat {
                format: ".svegmap object",
                field: "root.inventory.identity".to_owned(),
            },
        ));
    }
    Ok(chunk)
}

fn read_map_object_from(
    assets: &dyn CookAssetAccess,
    package: &Path,
    map: Uuid,
    reference: &VegetationMapChunkReference,
) -> Result<VegetationMapChunk> {
    let path = map_object_path(package, &reference.content_hash);
    let bytes = assets.read_file(&path)?;
    validate_map_object_bytes(map, reference, &bytes)
}

fn lock_map_package(package: &Path) -> Result<std::fs::File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(package.join("write.lock"))
        .map_err(|error| Error::Io(error.to_string()))?;
    file.lock().map_err(|error| Error::Io(error.to_string()))?;
    Ok(file)
}

fn rollback_map_objects(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = std::fs::remove_file(path);
    }
}

fn map_package_directory(map_path: &str) -> String {
    format!("{map_path}.data")
}

fn map_package_path(map_path: &Path) -> PathBuf {
    let mut value = map_path.as_os_str().to_os_string();
    value.push(".data");
    PathBuf::from(value)
}

fn map_object_path(package: &Path, hash: &[u8; 32]) -> PathBuf {
    let mut name = String::with_capacity(64 + ".svegmapc".len());
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").unwrap();
    }
    name.push_str(".svegmapc");
    package.join("objects").join(name)
}

fn rollback_new_asset(assets: &mut AssetServer, id: Uuid) {
    let Some(entry) = assets.catalog().find(id).cloned() else {
        return;
    };
    assets.remove_asset_sidecar(id);
    let _ = remove_vegetation_map_package(assets, &entry);
    if !entry.path.is_empty() {
        let _ = std::fs::remove_file(assets.root.join(&entry.path));
    }
    let _ = assets.delete_asset_entry(id);
}

pub(crate) fn vegetation_map_content_hash_path(path: &Path) -> Result<u64> {
    let root_bytes = std::fs::read(path).map_err(|error| Error::Io(error.to_string()))?;
    let root = decode_map(&root_bytes)?;
    let package = map_package_path(path);
    for reference in &root.inventory {
        read_map_object(&package, root.id, reference)?;
    }
    Ok(hash_bytes_fnv(&root_bytes))
}

fn validate_biome_cycles(assets: &AssetServer, candidate: &BiomeAsset) -> Result<()> {
    let mut graph = std::collections::BTreeMap::<u64, Vec<u64>>::new();
    for entry in &assets.catalog.entries {
        if entry.asset_type != AssetType::Biome || entry.id == candidate.id {
            continue;
        }
        let biome = load_biome_asset(assets, entry.id)?;
        graph.insert(
            biome.id.value(),
            biome
                .modules
                .iter()
                .map(|module| module.biome.value())
                .collect(),
        );
    }
    graph.insert(
        candidate.id.value(),
        candidate
            .modules
            .iter()
            .map(|module| module.biome.value())
            .collect(),
    );
    let mut complete = BTreeSet::new();
    let mut active = BTreeSet::new();
    for id in graph.keys().copied().collect::<Vec<_>>() {
        visit_biome(id, &graph, &mut active, &mut complete)?;
    }
    Ok(())
}

fn visit_biome(
    id: u64,
    graph: &std::collections::BTreeMap<u64, Vec<u64>>,
    active: &mut BTreeSet<u64>,
    complete: &mut BTreeSet<u64>,
) -> Result<()> {
    if complete.contains(&id) || !graph.contains_key(&id) {
        return Ok(());
    }
    if !active.insert(id) {
        return Err(Error::Io("biome module dependency cycle".to_owned()));
    }
    for dependency in &graph[&id] {
        visit_biome(*dependency, graph, active, complete)?;
    }
    active.remove(&id);
    complete.insert(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use saffron_scene::Scene;
    use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldBounds, WorldCellKey};
    use saffron_vegetation::{
        AuthoredFieldTile, BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION,
        BIOME_NODE_VERSION, BiomeAsset, BiomeGraphDocument, BiomeGraphEvaluator, BiomeGraphPolicy,
        BiomeModuleReference, BiomePaletteEntry, BiomeRole, BotanicalGraphDocument,
        FieldBlendOperator, FieldTileLayer, GraphAuthority, GraphCancellationToken, GraphDomain,
        GraphEdge, GraphInterfaceOutput, GraphNodeDefinition, GraphOperator, GraphParameterValue,
        GraphSink, HabitatPreferences, InteractionPolicy, LocalBiomeInstance, MechanicalResponse,
        NodeSpatialPolicy, PLANT_ASSET_VERSION, PlantDimensions, PlantFamilyAsset,
        PlantFamilySource, PlantPart, PlantPartSemantic, VEGETATION_MAP_CHUNK_VERSION,
        VEGETATION_MAP_VERSION, VegetationLayer, VegetationLayerOperator, VegetationMapAsset,
        VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
        VegetationMapChunkLayout, VegetationMapChunkPayload, VegetationMapFieldChunk,
        VegetationMapTileKey, vegetation_map_chunk_schema_hash,
    };
    use serde_json::Value;

    use super::*;

    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "saffron-vegetation-assets-{tag}-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixed(value: i32) -> DecisionScalar {
        DecisionScalar::from_integer(value).expect("representable fixture scalar")
    }

    fn plant_fixture(id: Uuid, name: &str) -> PlantFamilyAsset {
        PlantFamilyAsset {
            role: saffron_vegetation::PlantFamilyRole::Family,
            modules: Vec::new(),
            module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
            version: PLANT_ASSET_VERSION,
            id,
            name: name.to_owned(),
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
            material_slots: vec![Uuid(13)],
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
            variations: vec![saffron_vegetation::PlantVariation {
                id: 0,
                name: "Default".to_owned(),
                sources: vec![saffron_vegetation::native_variation_source_id(0)],
                active_parts: Vec::new(),
            }],
            phenotypes: vec![saffron_vegetation::PlantPhenotype {
                id: 0,
                role: saffron_vegetation::PhenotypeRole::Healthy,
                season_window: None,
                variation: 0,
                material_remap: Vec::new(),
                active_parts: Vec::new(),
            }],
            collision_proxies: Vec::new(),
            navigation_proxies: Vec::new(),
            interaction_policy: InteractionPolicy::Structural,
            habitat: Some(HabitatPreferences {
                fields: vec![(FieldChannel::Moisture, fixed(0), fixed(1))],
                surface_tags: vec![14],
                shade_tolerance: UnitInterval::from_bits(32_768),
            }),
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        }
    }

    fn biome_fixture(id: Uuid, name: &str, plant: Uuid) -> BiomeAsset {
        BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id,
            name: name.to_owned(),
            role: BiomeRole::Root,
            parameters: Vec::new(),
            palette: vec![BiomePaletteEntry {
                plant,
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
            graph: Value::Object(Default::default()),
        }
    }

    fn map_fixture(id: Uuid, name: &str) -> VegetationMapAsset {
        let bounds = WorldBounds::new([0; 3], [1024; 3]).expect("fixture bounds");
        VegetationMapAsset {
            version: VEGETATION_MAP_VERSION,
            id,
            name: name.to_owned(),
            bounds,
            chunk_layout: VegetationMapChunkLayout {
                level: 0,
                schema_hash: vegetation_map_chunk_schema_hash(),
            },
            generation: 0,
            inventory: Vec::new(),
        }
    }

    fn layer_fixture(bounds: WorldBounds) -> VegetationLayer {
        VegetationLayer {
            id: 32,
            name: "Density".to_owned(),
            coordinate_space: saffron_vegetation::LayerCoordinateSpace::World,
            bounds,
            operator: VegetationLayerOperator::Density(FieldTileLayer {
                channel: FieldChannel::Moisture,
                tile_set: 33,
                blend: FieldBlendOperator::Multiply,
                weight: UnitInterval::ONE,
            }),
            dependencies: Vec::new(),
            order: 0,
            locked: false,
            muted: false,
            revision: 1,
        }
    }

    fn layer_chunk(map: Uuid, layer: VegetationLayer) -> VegetationMapChunk {
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map,
            key: VegetationMapChunkKey {
                layer: layer.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::LayerMetadata,
            },
            revision: layer.revision,
            payload: VegetationMapChunkPayload::LayerMetadata(layer),
        }
    }

    fn graph_instance_chunk(map: Uuid, instance: LocalBiomeInstance) -> VegetationMapChunk {
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map,
            key: VegetationMapChunkKey {
                layer: instance.id,
                tile: VegetationMapTileKey::Global,
                kind: VegetationMapChunkKind::GraphInstance,
            },
            revision: instance.revision,
            payload: VegetationMapChunkPayload::GraphInstance(instance),
        }
    }

    fn chunk_fixture(map: Uuid, cell: WorldCellKey, revision: u64) -> VegetationMapChunk {
        VegetationMapChunk {
            version: VEGETATION_MAP_CHUNK_VERSION,
            map,
            key: VegetationMapChunkKey {
                layer: 32,
                tile: VegetationMapTileKey::Cell(cell),
                kind: VegetationMapChunkKind::Field,
            },
            revision,
            payload: VegetationMapChunkPayload::Field(VegetationMapFieldChunk {
                fields: vec![AuthoredFieldTile {
                    channel: FieldChannel::Moisture,
                    layer: 32,
                    dimensions: [1, 1, 1],
                    quantum_bits: 1,
                    values: vec![0],
                }],
                blockers: Vec::new(),
            }),
        }
    }

    fn commit_chunks(assets: &mut AssetServer, map: Uuid, upserts: Vec<VegetationMapChunk>) {
        let expected_generation = load_vegetation_map_root(assets, map).unwrap().generation;
        commit_vegetation_map_transaction(
            assets,
            map,
            VegetationMapTransaction {
                expected_generation,
                upserts,
                removals: Vec::new(),
            },
        )
        .unwrap();
    }

    #[test]
    fn all_authored_asset_kinds_round_trip_and_chunk_writes_are_sparse() {
        let scratch = Scratch::new("round-trip");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);

        let plant_id =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "plants")
                .expect("save plant");
        let mut expected_plant = plant_fixture(plant_id, "Oak");
        expected_plant.id = plant_id;
        assert_eq!(
            load_plant_family_asset(&assets, plant_id).unwrap(),
            expected_plant
        );

        let biome_id = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(2), "Forest", plant_id),
            "Forest",
            "biomes",
        )
        .expect("save biome");
        let mut expected_biome = biome_fixture(biome_id, "Forest", plant_id);
        expected_biome.id = biome_id;
        assert_eq!(load_biome_asset(&assets, biome_id).unwrap(), expected_biome);

        let map_id =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(3), "World"), "World", "maps")
                .expect("save map");
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        commit_chunks(
            &mut assets,
            map_id,
            vec![layer_chunk(map_id, layer_fixture(bounds))],
        );
        let snapshot = load_vegetation_map_snapshot(&assets, map_id).unwrap();
        assert_eq!(snapshot.name, "World");
        assert_eq!(snapshot.layers, vec![layer_fixture(bounds)]);

        for (id, encoder) in [
            (
                plant_id,
                write_plant_asset(&load_plant_family_asset(&assets, plant_id).unwrap()).unwrap(),
            ),
            (
                biome_id,
                write_biome_asset(&load_biome_asset(&assets, biome_id).unwrap()).unwrap(),
            ),
            (
                map_id,
                encode_map(&load_vegetation_map_root(&assets, map_id).unwrap()).unwrap(),
            ),
        ] {
            let entry = assets.catalog.find(id).unwrap();
            assert_eq!(std::fs::read(root.join(&entry.path)).unwrap(), encoder);
        }

        let first = WorldCellKey::base(0, 0, 0);
        let second = WorldCellKey::base(1, 0, 0);
        commit_chunks(
            &mut assets,
            map_id,
            vec![
                chunk_fixture(map_id, first, 1),
                chunk_fixture(map_id, second, 1),
            ],
        );
        let entry = assets.catalog.find(map_id).unwrap().clone();
        let manifest_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let root_before = load_vegetation_map_root(&assets, map_id).unwrap();
        let first_reference = root_before
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
            .copied()
            .unwrap();
        let second_reference = root_before
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(second))
            .copied()
            .unwrap();
        let first_path = map_object_path(&package, &first_reference.content_hash);
        let second_path = map_object_path(&package, &second_reference.content_hash);
        let dependencies_before = vegetation_graph_dependency_hashes(&assets, map_id, &[]).unwrap();
        let manifest_before = std::fs::read(&manifest_path).unwrap();
        let second_before = std::fs::read(&second_path).unwrap();
        #[cfg(unix)]
        let second_inode_before =
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&second_path).unwrap());

        commit_chunks(&mut assets, map_id, vec![chunk_fixture(map_id, first, 2)]);
        assert_ne!(std::fs::read(&manifest_path).unwrap(), manifest_before);
        assert!(first_path.exists());
        assert_eq!(std::fs::read(&second_path).unwrap(), second_before);
        let root_after = load_vegetation_map_root(&assets, map_id).unwrap();
        let dependencies_after = vegetation_graph_dependency_hashes(&assets, map_id, &[]).unwrap();
        assert_eq!(dependencies_after, dependencies_before);
        assert_eq!(root_after.generation, root_before.generation + 1);
        assert_eq!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
                .unwrap()
                .revision,
            2
        );
        assert_ne!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
                .unwrap()
                .content_hash,
            first_reference.content_hash
        );
        assert_eq!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(second))
                .unwrap(),
            &second_reference
        );
        assert!(
            load_vegetation_map_tile_snapshot(&assets, map_id, first)
                .unwrap()
                .is_some()
        );
        #[cfg(unix)]
        {
            assert_eq!(
                std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&second_path).unwrap()),
                second_inode_before
            );
        }

        let current_first_reference = root_after
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
            .unwrap();
        let current_first_path = map_object_path(&package, &current_first_reference.content_hash);
        let authored_paths = [manifest_path, current_first_path, second_path];
        let authored_bytes: Vec<Vec<u8>> = authored_paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        assets.thumbnail_cache_root = root.join(".cache/thumbnails");
        for id in [biome_id, map_id] {
            assert!(
                !crate::request_thumbnail(&mut assets, id, 128)
                    .unwrap()
                    .pending
            );
        }
        // The plant renders through the main graph: a cold cache replies pending.
        assert!(
            crate::request_thumbnail(&mut assets, plant_id, 128)
                .unwrap()
                .pending
        );
        let removed = assets.clear_thumbnail_cache_dir();
        assert_eq!(removed.entries, 2);
        assets.clear_asset_caches();
        for (path, expected) in authored_paths.iter().zip(authored_bytes) {
            assert_eq!(std::fs::read(path).unwrap(), expected);
        }
    }

    #[test]
    fn stale_map_transactions_publish_nothing() {
        let scratch = Scratch::new("stale-transaction");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let chunk = chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1);
        let reference = chunk.reference().unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let root_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let before = std::fs::read(&root_path).unwrap();

        let error = commit_vegetation_map_transaction(
            &mut assets,
            map,
            VegetationMapTransaction {
                expected_generation: 1,
                upserts: vec![chunk],
                removals: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::VegetationMapGenerationConflict {
                expected: 1,
                actual: 0
            }
        ));
        assert_eq!(std::fs::read(root_path).unwrap(), before);
        assert!(!map_object_path(&package, &reference.content_hash).exists());
    }

    #[test]
    fn failed_multi_object_transaction_rolls_back_before_root_publication() {
        let scratch = Scratch::new("transaction-rollback");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        let metadata = layer_chunk(map, layer_fixture(bounds));
        let first = chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1);
        let second = chunk_fixture(map, WorldCellKey::base(1, 0, 0), 1);
        let metadata_reference = metadata.reference().unwrap();
        let first_reference = first.reference().unwrap();
        let second_reference = second.reference().unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let root_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let before = std::fs::read(&root_path).unwrap();
        std::fs::create_dir(map_object_path(&package, &second_reference.content_hash)).unwrap();

        assert!(
            commit_vegetation_map_transaction(
                &mut assets,
                map,
                VegetationMapTransaction {
                    expected_generation: 0,
                    upserts: vec![second, first, metadata],
                    removals: Vec::new(),
                },
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root_path).unwrap(), before);
        assert!(!map_object_path(&package, &metadata_reference.content_hash).exists());
        assert!(!map_object_path(&package, &first_reference.content_hash).exists());
        assert!(
            load_vegetation_map_root(&assets, map)
                .unwrap()
                .inventory
                .is_empty()
        );
    }

    #[test]
    fn sparse_removal_hides_old_immutable_object_and_corruption_is_typed() {
        let scratch = Scratch::new("remove-corruption");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let cell = WorldCellKey::base(-1, 2, 0);
        let chunk = chunk_fixture(map, cell, 1);
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        commit_chunks(
            &mut assets,
            map,
            vec![layer_chunk(map, layer_fixture(bounds)), chunk.clone()],
        );
        let reference = load_vegetation_map_root(&assets, map)
            .unwrap()
            .inventory
            .into_iter()
            .find(|reference| reference.key == chunk.key)
            .unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let package = root.join(map_package_directory(&entry.path));
        let path = map_object_path(&package, &reference.content_hash);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(matches!(
            load_vegetation_map_snapshot(&assets, map),
            Err(Error::Vegetation(
                saffron_vegetation::Error::ArtifactFormat { .. }
            ))
        ));
        std::fs::write(&path, bytes).unwrap();

        commit_vegetation_map_transaction(
            &mut assets,
            map,
            VegetationMapTransaction {
                expected_generation: 1,
                upserts: Vec::new(),
                removals: vec![chunk.key],
            },
        )
        .unwrap();
        assert_eq!(
            load_vegetation_map_root(&assets, map)
                .unwrap()
                .inventory
                .len(),
            1
        );
        assert!(
            load_vegetation_map_tile_snapshot(&assets, map, cell)
                .unwrap()
                .is_none()
        );
        assert!(path.exists());
    }

    #[test]
    fn cold_scan_recovers_authored_types_and_ignores_generated_artifacts() {
        let scratch = Scratch::new("cold-scan");
        let root = scratch.path().join("assets");
        let mut writer = AssetServer::new(&root);
        let plant =
            save_plant_family_asset(&mut writer, plant_fixture(Uuid(1), "Oak"), "Oak", "plants")
                .unwrap();
        let biome = save_biome_asset(
            &mut writer,
            biome_fixture(Uuid(2), "Forest", plant),
            "Forest",
            "biomes",
        )
        .unwrap();
        let map =
            save_vegetation_map_asset(&mut writer, map_fixture(Uuid(3), "World"), "World", "maps")
                .unwrap();
        for (id, name, folder) in [
            (plant, "Oak renamed", "catalog/plants"),
            (biome, "Forest renamed", "catalog/biomes"),
            (map, "World renamed", "catalog/maps"),
        ] {
            let entry = writer
                .catalog
                .entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .unwrap();
            entry.name = name.to_owned();
            entry.folder = folder.to_owned();
            writer.write_asset_sidecar(id).unwrap();
        }
        commit_chunks(
            &mut writer,
            map,
            vec![
                layer_chunk(
                    map,
                    layer_fixture(WorldBounds::new([0; 3], [1024; 3]).unwrap()),
                ),
                chunk_fixture(map, WorldCellKey::base(-1, 2, 0), 1),
            ],
        );
        let expected_map_hash = writer.catalog.find(map).unwrap().content_hash;
        std::fs::write(root.join("vegetation/plants/999.splantc"), b"generated").unwrap();
        std::fs::write(root.join("vegetation/maps/998.svegcell"), b"generated").unwrap();

        let mut reader = AssetServer::new(&root);
        reader.scan_assets().expect("cold scan");
        let plant_entry = reader.catalog.find(plant).unwrap();
        assert_eq!(plant_entry.asset_type, AssetType::Plant);
        assert_eq!(plant_entry.name, "Oak renamed");
        assert_eq!(plant_entry.folder, "catalog/plants");
        let biome_entry = reader.catalog.find(biome).unwrap();
        assert_eq!(biome_entry.asset_type, AssetType::Biome);
        assert_eq!(biome_entry.name, "Forest renamed");
        assert_eq!(biome_entry.folder, "catalog/biomes");
        let map_entry = reader.catalog.find(map).unwrap();
        assert_eq!(map_entry.asset_type, AssetType::VegetationMap);
        assert_eq!(map_entry.name, "World renamed");
        assert_eq!(map_entry.folder, "catalog/maps");
        assert_eq!(map_entry.content_hash, expected_map_hash);
        assert!(reader.catalog.find(Uuid(998)).is_none());
        assert!(reader.catalog.find(Uuid(999)).is_none());
    }

    #[test]
    fn authored_types_have_cached_vector_thumbnails() {
        let scratch = Scratch::new("thumbnails");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        assets.thumbnail_cache_root = scratch.path().join("thumbnail-cache");
        let plant =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "").unwrap();
        let biome = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(2), "Forest", plant),
            "Forest",
            "",
        )
        .unwrap();
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(3), "World"), "World", "")
                .unwrap();

        for id in [biome, map] {
            let first = crate::request_thumbnail(&mut assets, id, 128).unwrap();
            assert!(!first.pending);
            assert_eq!((first.width, first.height), (128, 128));
            assert_eq!(&first.png[..8], b"\x89PNG\r\n\x1a\n");
            let cached = crate::request_thumbnail(&mut assets, id, 128).unwrap();
            assert_eq!(cached, first);
        }
        // A plant family renders through the main graph like a mesh/model tile: a cold
        // cache replies pending and enqueues one preview render for the host to drain.
        let pending = crate::request_thumbnail(&mut assets, plant, 128).unwrap();
        assert!(pending.pending);
        assert_eq!(assets.take_preview_render_jobs(4).len(), 1);
        assert_eq!(assets.thumbnail_cache_stats().entries, 2);
    }

    #[test]
    fn authored_imports_preserve_identity_and_copy_complete_map_packages() {
        let scratch = Scratch::new("import");
        let source_root = scratch.path().join("source");
        let project_root = scratch.path().join("assets");
        std::fs::create_dir_all(&source_root).unwrap();
        let plant_path = source_root.join("oak.splant");
        let biome_path = source_root.join("forest.sbiome");
        let map_path = source_root.join("world.svegmap");
        std::fs::write(
            &plant_path,
            write_plant_asset(&plant_fixture(Uuid(4_101), "Oak")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &biome_path,
            write_biome_asset(&biome_fixture(Uuid(4_102), "Forest", Uuid(4_101))).unwrap(),
        )
        .unwrap();
        let mut source_map = map_fixture(Uuid(4_103), "World");
        let cell = WorldCellKey::base(4, -2, 0);
        let source_chunk = chunk_fixture(source_map.id, cell, 7);
        source_map.inventory = vec![source_chunk.reference().unwrap()];
        source_map.generation = 1;
        std::fs::write(&map_path, encode_map(&source_map).unwrap()).unwrap();
        publish_imported_map_objects(&map_package_path(&map_path), &source_map, &[source_chunk])
            .unwrap();

        let mut assets = AssetServer::new(&project_root);
        let imported_plant = import_vegetation_asset(&mut assets, &plant_path, "imports").unwrap();
        let imported_biome = import_vegetation_asset(&mut assets, &biome_path, "imports").unwrap();
        let imported_map = import_vegetation_asset(&mut assets, &map_path, "imports").unwrap();
        assert_eq!(imported_plant.asset_type, AssetType::Plant);
        assert_eq!(imported_biome.asset_type, AssetType::Biome);
        assert_eq!(imported_map.asset_type, AssetType::VegetationMap);
        assert_eq!(imported_plant.id, Uuid(4_101));
        assert_eq!(imported_biome.id, Uuid(4_102));
        assert_eq!(imported_map.id, Uuid(4_103));
        assert_eq!(
            load_biome_asset(&assets, imported_biome.id)
                .unwrap()
                .palette[0]
                .plant,
            imported_plant.id
        );
        let imported_chunk = load_vegetation_map_tile_snapshot(&assets, imported_map.id, cell)
            .unwrap()
            .unwrap();
        assert_eq!(imported_chunk.map, imported_map.id);
        assert_eq!(
            load_vegetation_map_root(&assets, imported_map.id)
                .unwrap()
                .inventory[0]
                .revision,
            7
        );

        let generated = source_root.join("compiled.splantc");
        std::fs::write(&generated, b"generated").unwrap();
        assert!(import_vegetation_asset(&mut assets, generated, "").is_err());
    }

    #[test]
    fn biome_cycles_are_rejected_across_existing_assets() {
        let scratch = Scratch::new("cycles");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let first = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(1), "First", Uuid(11)),
            "First",
            "",
        )
        .unwrap();
        let mut second_asset = biome_fixture(Uuid(2), "Second", Uuid(11));
        second_asset.modules.push(BiomeModuleReference {
            biome: first,
            call_guid: 100,
            bindings: Vec::new(),
        });
        let second = save_biome_asset(&mut assets, second_asset, "Second", "").unwrap();
        let mut first_asset = load_biome_asset(&assets, first).unwrap();
        first_asset.modules.push(BiomeModuleReference {
            biome: second,
            call_guid: 200,
            bindings: Vec::new(),
        });
        assert!(update_biome_asset(&mut assets, first, &first_asset).is_err());
    }

    #[test]
    fn deleting_unused_map_removes_root_and_authored_object_package() {
        let scratch = Scratch::new("delete-package");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        commit_chunks(
            &mut assets,
            map,
            vec![
                layer_chunk(
                    map,
                    layer_fixture(WorldBounds::new([0; 3], [1024; 3]).unwrap()),
                ),
                chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1),
            ],
        );
        let entry = assets.catalog.find(map).unwrap().clone();
        let manifest = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let mut scene = Scene::new();
        let deleted = crate::delete_unused(&mut assets, &mut scene, &[map], true).unwrap();
        assert_eq!(deleted.deleted, 1);
        assert!(!manifest.exists());
        assert!(!package.exists());
        assert!(assets.catalog.find(map).is_none());
    }

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
