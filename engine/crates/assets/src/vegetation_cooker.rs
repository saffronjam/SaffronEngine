//! Deterministic authored-map to immutable vegetation-generation cooking.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, SurfaceField, WorldBounds, WorldCellKey};
use saffron_vegetation::{
    CandidateRejectionReason, CompiledBiomeGraph, ContentHash, CookDependency,
    CookDependencyAddress, CookGraph, CookNodeAddress, CookNodeRecord, CookPlatformProfile,
    CookVersionSet, CookWorkActual, CookWorkEstimate, GlobalStageEvaluationResult,
    GraphCancellationToken, GraphCompileOptions, GraphDependencySource, GraphEvaluationDiagnostics,
    GraphEvaluationResult, ManifestCellDependency, ManifestCellDependencyRole, ManifestCellSection,
    ManifestSpeciesCount, PlantCompileLimits, PlantCompiledArtifactIndex, PlantPointColumns,
    ProvenanceTable, VegetationBaseManifest, VegetationCellArtifactHeader,
    VegetationCellArtifactIndex, VegetationManifestCell, VegetationManifestPlant,
    VegetationMapChunkKind, VegetationMapSnapshot, VegetationMapTileKey, VegetationSeedNamespace,
    merge_graph_evaluation_results, vegetation_base_manifest_schema_hash,
    vegetation_cell_artifact_schema_hash, vegetation_content_hash, write_plant_asset,
    write_vegetation_cell_artifact, write_vegetation_map_asset,
};

use crate::cook_reader::CookAssetReader;
use crate::plant_cook::{prepare_plant_family_sources_from, stage_prepared_plant_family};
use crate::vegetation::{
    assemble_biome_graph_evaluation_job_from, compile_catalog_biome_instance_graph_from,
    load_plant_family_asset_from, load_vegetation_map_snapshot_from,
    vegetation_graph_dependency_hashes_from,
};
use crate::{
    AssetServer, AuthoredInputGuard, CookProjectView, Error, PlantRecookOptions,
    PlantRecookOutcome, PreparedPlantFamily, Result, VegetationArtifactPublication,
    VegetationArtifactStore, update_plant_family_asset,
};

/// Complete immutable input scope for one world cook.
pub struct VegetationCookRequest {
    /// Stable world identity owning the scene-level vegetation field.
    pub world: Uuid,
    /// Authored vegetation-map asset.
    pub map: Uuid,
    /// Current generation root captured before this job was queued.
    pub expected_manifest: Option<ContentHash>,
    /// Exact output cells. The cooker canonicalizes order and rejects duplicates.
    pub cells: Vec<WorldCellKey>,
    /// Read-only ecology snapshot tick visible to graph rules.
    pub ecology_tick: u64,
    /// Bounded evaluator worker count.
    pub workers: u16,
    /// Complete platform profile participating in every cook key.
    pub platform: CookPlatformProfile,
    /// Immutable surface snapshots captured before the worker starts.
    pub surface_providers: Vec<Arc<dyn SurfaceField>>,
}

/// Monotonic event emitted by the synchronous cooker for asynchronous job observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationCookEvent {
    /// The exact graph node count is known.
    Planned { total_nodes: u64 },
    /// Work began for one logical output.
    Started { node: CookNodeAddress },
    /// One node completed validation and any required atomic publication.
    Completed {
        node: CookNodeAddress,
        cache_hit: bool,
        published_cell: bool,
    },
}

/// Aggregate measured work and typed rejection counts for one generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationCookStatistics {
    /// Cook-graph node count.
    pub nodes: u64,
    /// Total measured wall time.
    pub elapsed_micros: u64,
    /// Maximum measured resident-memory requirement of any node.
    pub peak_memory_bytes: u64,
    /// Canonical input bytes consumed by all nodes.
    pub input_bytes: u64,
    /// Validated output bytes produced by all nodes.
    pub output_bytes: u64,
    /// Nodes satisfied by a validated existing artifact.
    pub cache_hits: u64,
    /// Nodes that executed and produced new bytes.
    pub cache_misses: u64,
    /// Immutable cells published by this generation.
    pub published_cells: u64,
    /// Typed rejection totals in stable enum order.
    pub rejections: Vec<(CandidateRejectionReason, u64)>,
}

/// Atomically published result of one complete vegetation generation cook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationCookOutput {
    /// Complete canonical dependency DAG and node measurements.
    pub cook_graph: CookGraph,
    /// Complete immutable base manifest.
    pub manifest: VegetationBaseManifest,
    /// Manifest identity used by saves and future sessions.
    pub manifest_identity: ContentHash,
    /// CAS publication and current-root update result.
    pub publication: VegetationArtifactPublication,
    /// Aggregate observational statistics.
    pub statistics: VegetationCookStatistics,
}

/// One authored plant update admitted only if the staged generation commits.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantSourceAcceptance {
    /// Plant-family catalog identity.
    pub family: Uuid,
    /// Exact authored bytes read by the worker.
    pub expected_authored_hash: ContentHash,
    /// Accepted source-observation state produced by the compiler.
    pub accepted_asset: saffron_vegetation::PlantFamilyAsset,
}

/// Complete immutable cook result awaiting main-thread authored/root publication.
#[derive(Clone, Debug)]
pub struct StagedVegetationCook {
    /// Project roots and catalog snapshot used by the worker.
    pub project: CookProjectView,
    /// Vegetation map whose visible generation may advance.
    pub map: Uuid,
    /// Generation root captured when the job was queued.
    pub expected_manifest: Option<ContentHash>,
    /// Exact file spans read while staging.
    pub authored_guards: Vec<AuthoredInputGuard>,
    /// Surface snapshot identities read by the evaluator.
    pub surface_descriptors: Vec<saffron_spatial::SurfaceProviderDescriptor>,
    /// Authored source hashes accepted by successful plant compilation.
    pub source_acceptances: Vec<PlantSourceAcceptance>,
    /// Complete canonical dependency graph.
    pub cook_graph: CookGraph,
    /// Complete immutable base manifest.
    pub manifest: VegetationBaseManifest,
    /// Canonical manifest bytes already resident in CAS.
    pub manifest_bytes: Vec<u8>,
    /// Manifest content identity.
    pub manifest_identity: ContentHash,
    /// Observational work statistics retained by the job manager.
    pub statistics: VegetationCookStatistics,
}

struct EvaluatedInstance {
    id: u128,
    graph: CompiledBiomeGraph,
    cell_read_bounds: BTreeMap<WorldCellKey, WorldBounds>,
    global_inputs: BTreeMap<([u8; 32], WorldCellKey), GlobalInputRecord>,
    global_results: Vec<GlobalStageEvaluationResult>,
    cells: Vec<GraphEvaluationResult>,
}

struct PreviousGeneration {
    manifest: VegetationBaseManifest,
    nodes: BTreeMap<CookNodeAddress, CookNodeRecord>,
    plants: BTreeMap<u64, VegetationManifestPlant>,
    cells: BTreeMap<WorldCellKey, VegetationManifestCell>,
}

#[derive(Clone, Copy)]
struct GlobalInputRecord {
    read_bounds: WorldBounds,
    input_snapshot: ContentHash,
}

fn load_previous_generation(
    store: &VegetationArtifactStore,
    request: &VegetationCookRequest,
) -> Result<Option<PreviousGeneration>> {
    let Some(identity) = request.expected_manifest else {
        return Ok(None);
    };
    let manifest_bytes = store.read_manifest(identity)?;
    let manifest = VegetationBaseManifest::from_canonical_bytes(&manifest_bytes)?;
    if manifest.identity()? != identity
        || manifest.map != request.map
        || manifest.world != request.world
    {
        return Err(Error::Io(
            "current vegetation generation belongs to a different world or map".to_owned(),
        ));
    }
    let graph_bytes = store.read_cook_graph(manifest.cook_graph_hash)?;
    let graph = CookGraph::from_canonical_bytes(&graph_bytes)?;
    if graph.identity()? != manifest.cook_graph_hash {
        return Err(Error::Io(
            "current vegetation generation has an invalid cook graph identity".to_owned(),
        ));
    }
    let nodes = graph
        .nodes
        .into_iter()
        .map(|node| (node.address.clone(), node))
        .collect();
    let plants = manifest
        .plants
        .iter()
        .cloned()
        .map(|plant| (plant.family.value(), plant))
        .collect();
    let cells = manifest
        .cells
        .iter()
        .cloned()
        .map(|cell| (cell.cell, cell))
        .collect();
    Ok(Some(PreviousGeneration {
        manifest,
        nodes,
        plants,
        cells,
    }))
}

fn reusable_plant(
    store: &VegetationArtifactStore,
    previous: Option<&PreviousGeneration>,
    prepared: &PreparedPlantFamily,
    options: &PlantRecookOptions,
) -> Result<Option<(CookNodeRecord, VegetationManifestPlant)>> {
    let Some(previous) = previous.filter(|previous| {
        previous.manifest.versions == options.versions
            && previous.manifest.platform == options.platform
    }) else {
        return Ok(None);
    };
    if !prepared.validation.compile.publishable() {
        return Ok(None);
    }
    let family = prepared.accepted_asset.id;
    let address = CookNodeAddress::Plant { family };
    let cook_key = prepared.cook_key(options)?;
    let source_hash = ContentHash::of(&write_plant_asset(&prepared.accepted_asset)?);
    let Some(previous_node) = previous.nodes.get(&address) else {
        return Ok(None);
    };
    let Some(previous_plant) = previous.plants.get(&family.value()) else {
        return Ok(None);
    };
    if previous_node.cook_key != cook_key
        || previous_node.dependencies != prepared.validation.dependencies
        || previous_plant.source_hash != source_hash
        || previous_plant.artifact_hash != previous_node.output_hash
    {
        return Ok(None);
    }
    let Some(bytes) = store.read_plant_if_present(previous_node.output_hash)? else {
        return Ok(None);
    };
    let index = PlantCompiledArtifactIndex::open(&bytes)?;
    if index.family != family
        || index.cook_key != cook_key
        || index.platform_profile != options.platform.identity()?
    {
        return Ok(None);
    }
    let output_bytes = u64::try_from(bytes.len())
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let estimate = plant_estimate(&prepared.validation.compile, output_bytes);
    let node = CookNodeRecord {
        address,
        cook_key,
        output_hash: previous_node.output_hash,
        dependencies: prepared.validation.dependencies.clone(),
        estimate,
        actual: CookWorkActual {
            peak_memory_bytes: output_bytes,
            input_bytes: u64::try_from(prepared.validation.dependencies.len())
                .unwrap_or(u64::MAX)
                .saturating_mul(32),
            output_bytes,
            rejection_count: prepared.validation.compile.statistics.rejected,
            cache_hit: true,
            ..CookWorkActual::default()
        },
    };
    let plant = VegetationManifestPlant {
        family,
        tags: prepared.accepted_asset.tags.clone(),
        source_hash,
        artifact_hash: previous_node.output_hash,
        local_bounds_min: prepared.accepted_asset.dimensions.local_bounds_min,
        local_bounds_max: prepared.accepted_asset.dimensions.local_bounds_max,
        variation_count: u32::try_from(prepared.accepted_asset.variations.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
        phenotype_count: u32::try_from(prepared.accepted_asset.phenotypes.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
    };
    Ok(Some((node, plant)))
}

/// Stages one complete generation without changing authored assets or the visible map root.
pub fn stage_vegetation_cook(
    project: CookProjectView,
    mut request: VegetationCookRequest,
    cancellation: &GraphCancellationToken,
    mut emit: impl FnMut(VegetationCookEvent),
) -> Result<StagedVegetationCook> {
    let started = Instant::now();
    validate_request(&mut request)?;
    cancellation_checkpoint(cancellation)?;
    let mut assets = CookAssetReader::new(project.clone());
    let store = VegetationArtifactStore::new(assets.cache_root());
    if store.current_manifest_hash(request.map)? != request.expected_manifest {
        return Err(Error::VegetationGenerationSuperseded {
            map: request.map.value(),
            expected: optional_content_hash(request.expected_manifest),
            current: optional_content_hash(store.current_manifest_hash(request.map)?),
        });
    }
    let previous = load_previous_generation(&store, &request)?;
    let map = load_vegetation_map_snapshot_from(&assets, request.map)?;
    if request
        .cells
        .iter()
        .any(|cell| !bounds_intersect(cell.bounds(), map.bounds))
    {
        return Err(Error::Io(
            "vegetation cook output cell lies outside the authored map bounds".to_owned(),
        ));
    }
    if let Some(previous) = &previous {
        request.cells.extend(
            previous
                .cells
                .keys()
                .copied()
                .filter(|cell| bounds_intersect(cell.bounds(), map.bounds)),
        );
        request.cells.sort_unstable();
        request.cells.dedup();
    }
    let map_bytes = write_vegetation_map_asset(&map.root)?;
    let map_hash = ContentHash::of(&map_bytes);
    let external_dependencies =
        vegetation_graph_dependency_hashes_from(&assets, request.map, &request.surface_providers)?;

    let mut families = BTreeMap::<u64, Uuid>::new();
    let mut planned_global_nodes = 0_u64;
    for instance in &map.biome_instances {
        let cells = intersecting_cells(&request.cells, instance.bounds);
        if cells.is_empty() {
            continue;
        }
        let resolved = compile_catalog_biome_instance_graph_from(
            &assets,
            request.map,
            instance.id,
            &external_dependencies,
            GraphCompileOptions::canonical(),
        )?;
        for prototype in &resolved.plant_prototypes {
            families.insert(prototype.family.value(), prototype.family);
        }
        let inputs = assemble_biome_graph_evaluation_job_from(
            &assets,
            &resolved,
            request.map,
            &cells,
            request.ecology_tick,
            clone_surface_providers(&request.surface_providers),
        )?;
        planned_global_nodes = planned_global_nodes
            .checked_add(
                u64::try_from(inputs.global_stages.len())
                    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
            )
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))?;
    }
    for chunk in &map.chunks {
        if let saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(payload) =
            &chunk.payload
        {
            for anchor in &payload.explicit_plants {
                if request.cells.contains(&anchor.point.owner) {
                    families.insert(anchor.family.value(), anchor.family);
                }
            }
        }
    }
    let total_nodes = u64::try_from(families.len())
        .ok()
        .and_then(|count| count.checked_add(planned_global_nodes))
        .and_then(|count| count.checked_add(u64::try_from(request.cells.len()).ok()?))
        .ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    emit(VegetationCookEvent::Planned { total_nodes });

    let versions = vegetation_cook_versions();
    let platform_profile = request.platform.identity()?;
    let mut nodes = Vec::<CookNodeRecord>::new();
    let mut plants = Vec::<VegetationManifestPlant>::new();
    let mut plant_outputs = BTreeMap::<u64, ContentHash>::new();
    let mut source_acceptances = Vec::<PlantSourceAcceptance>::new();
    for family in families.values().copied() {
        cancellation_checkpoint(cancellation)?;
        let address = CookNodeAddress::Plant { family };
        emit(VegetationCookEvent::Started {
            node: address.clone(),
        });
        let source = load_plant_family_asset_from(&assets, family)?;
        let expected_authored_hash = ContentHash::of(&write_plant_asset(&source)?);
        let options = PlantRecookOptions {
            limits: PlantCompileLimits::default(),
            versions,
            platform: request.platform.clone(),
        };
        let prepared = prepare_plant_family_sources_from(&mut assets, &source, options.limits)?;
        if let Some((node, plant)) = reusable_plant(&store, previous.as_ref(), &prepared, &options)?
        {
            if source != prepared.accepted_asset {
                source_acceptances.push(PlantSourceAcceptance {
                    family,
                    expected_authored_hash,
                    accepted_asset: prepared.accepted_asset.clone(),
                });
            }
            plant_outputs.insert(family.value(), node.output_hash);
            emit(VegetationCookEvent::Completed {
                node: address,
                cache_hit: true,
                published_cell: false,
            });
            nodes.push(node);
            plants.push(plant);
            continue;
        }
        let outcome = stage_prepared_plant_family(&store, prepared, &options)?;
        let PlantRecookOutcome::Published(publication) = outcome else {
            return Err(Error::PlantCompilationRejected {
                family: family.value(),
            });
        };
        let source_hash = ContentHash::of(&write_plant_asset(&publication.accepted_asset)?);
        let estimate = plant_estimate(
            &publication.validation.compile,
            publication.publication.bytes,
        );
        nodes.push(CookNodeRecord {
            address: address.clone(),
            cook_key: publication.cook_key,
            output_hash: publication.publication.content_hash,
            dependencies: publication.validation.dependencies.clone(),
            estimate,
            actual: publication.work,
        });
        plants.push(VegetationManifestPlant {
            family,
            tags: publication.accepted_asset.tags.clone(),
            source_hash,
            artifact_hash: publication.publication.content_hash,
            local_bounds_min: publication.accepted_asset.dimensions.local_bounds_min,
            local_bounds_max: publication.accepted_asset.dimensions.local_bounds_max,
            variation_count: u32::try_from(publication.accepted_asset.variations.len())
                .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
            phenotype_count: u32::try_from(publication.accepted_asset.phenotypes.len())
                .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
        });
        plant_outputs.insert(family.value(), publication.publication.content_hash);
        if source != publication.accepted_asset {
            source_acceptances.push(PlantSourceAcceptance {
                family,
                expected_authored_hash,
                accepted_asset: publication.accepted_asset.clone(),
            });
        }
        emit(VegetationCookEvent::Completed {
            node: address,
            cache_hit: publication.publication.cache_hit,
            published_cell: false,
        });
    }

    let external_dependencies =
        vegetation_graph_dependency_hashes_from(&assets, request.map, &request.surface_providers)?;
    let mut evaluated = Vec::<EvaluatedInstance>::new();
    for instance in &map.biome_instances {
        let cells = intersecting_cells(&request.cells, instance.bounds);
        if cells.is_empty() {
            continue;
        }
        cancellation_checkpoint(cancellation)?;
        let resolved = compile_catalog_biome_instance_graph_from(
            &assets,
            request.map,
            instance.id,
            &external_dependencies,
            GraphCompileOptions::canonical(),
        )?;
        let inputs = assemble_biome_graph_evaluation_job_from(
            &assets,
            &resolved,
            request.map,
            &cells,
            request.ecology_tick,
            clone_surface_providers(&request.surface_providers),
        )?;
        let cell_read_bounds = inputs
            .cells
            .iter()
            .map(|input| (input.output_cell, input.read_bounds))
            .collect::<BTreeMap<_, _>>();
        let global_inputs = inputs
            .global_stages
            .iter()
            .map(|input| {
                (
                    (input.stage, input.owner),
                    GlobalInputRecord {
                        read_bounds: input.inputs.read_bounds,
                        input_snapshot: ContentHash::new(input.input_snapshot),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        for input in &inputs.global_stages {
            emit(VegetationCookEvent::Started {
                node: CookNodeAddress::GlobalStage {
                    map: request.map,
                    biome_instance: instance.id,
                    stage: ContentHash::new(input.stage),
                    owner: input.owner,
                },
            });
        }
        let evaluator = saffron_vegetation::BiomeGraphEvaluator::new(
            Arc::new(resolved.graph.clone()),
            usize::from(request.workers),
        )?;
        evaluator.preflight(&inputs, cancellation)?;
        let results = evaluator.evaluate(inputs, cancellation)?;
        evaluated.push(EvaluatedInstance {
            id: instance.id,
            graph: resolved.graph,
            cell_read_bounds,
            global_inputs,
            global_results: results.global_stages,
            cells: results.cells,
        });
    }

    append_global_nodes(
        &mut nodes,
        &evaluated,
        &map,
        &request,
        &plant_outputs,
        versions,
        &mut emit,
    )?;
    let global_outputs = nodes
        .iter()
        .filter_map(|node| match &node.address {
            CookNodeAddress::GlobalStage {
                biome_instance,
                stage,
                owner,
                ..
            } => Some(((*biome_instance, *stage, *owner), node.output_hash)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut result_inputs = request
        .cells
        .iter()
        .copied()
        .map(|cell| (cell, Vec::<(u128, GraphEvaluationResult)>::new()))
        .collect::<BTreeMap<_, _>>();
    for instance in &evaluated {
        for result in &instance.cells {
            result_inputs
                .get_mut(&result.cell)
                .ok_or_else(|| Error::Io("evaluator returned an unrequested cell".to_owned()))?
                .push((instance.id, result.clone()));
        }
    }
    let mut merged_results = result_inputs
        .into_iter()
        .map(|(cell, mut inputs)| {
            let result = if inputs.is_empty() {
                empty_result(cell)
            } else if inputs.len() == 1 {
                inputs
                    .pop()
                    .ok_or_else(|| Error::Io("merged cell input disappeared".to_owned()))?
                    .1
            } else {
                merge_graph_evaluation_results(inputs)?
            };
            Ok((cell, result))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut rejection_totals = [0_u64; 7];
    for rejected in evaluated
        .iter()
        .flat_map(|instance| &instance.global_results)
        .flat_map(|result| &result.result.diagnostics.rejected)
        .chain(
            merged_results
                .iter()
                .flat_map(|(_, result)| &result.diagnostics.rejected),
        )
    {
        let total = &mut rejection_totals[rejection_reason_index(rejected.reason)];
        *total = total.checked_add(1).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    }
    merged_results.sort_unstable_by(|left, right| {
        right
            .0
            .level()
            .cmp(&left.0.level())
            .then(left.0.cmp(&right.0))
    });

    let mut cells = Vec::<VegetationManifestCell>::new();
    let mut cell_outputs = BTreeMap::<WorldCellKey, ContentHash>::new();
    let dependency_context = CellDependencyContext {
        evaluated: &evaluated,
        map: &map,
        request: &request,
        plant_outputs: &plant_outputs,
        global_outputs: &global_outputs,
    };
    for (cell, result) in merged_results {
        cancellation_checkpoint(cancellation)?;
        let address = CookNodeAddress::Cell {
            map: request.map,
            cell,
        };
        emit(VegetationCookEvent::Started {
            node: address.clone(),
        });
        let dependencies = cell_dependencies(cell, &result, &dependency_context, &cell_outputs)?;
        let estimate = result_estimate(&result)?;
        let mut node = CookNodeRecord {
            address: address.clone(),
            cook_key: ContentHash::default(),
            output_hash: ContentHash::default(),
            dependencies,
            estimate,
            actual: CookWorkActual::default(),
        };
        node.cook_key = node.calculate_cook_key(versions, &request.platform)?;
        let sections = result.cell_artifact_sections()?;
        let bytes = write_vegetation_cell_artifact(
            VegetationCellArtifactHeader {
                cell,
                cook_key: node.cook_key,
                platform_profile,
            },
            &sections,
        )?;
        cancellation_checkpoint(cancellation)?;
        let publication = store.publish_cell(&bytes)?;
        let index = VegetationCellArtifactIndex::open(&bytes)?;
        node.output_hash = publication.content_hash;
        node.actual = result_actual(&result, publication.bytes, publication.cache_hit);
        let species_counts = species_counts(&result)?;
        let macro_count = u64::try_from(result.macro_points.ids.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
        let micro_count = species_counts.iter().try_fold(0_u64, |total, species| {
            total
                .checked_add(species.micro_count)
                .ok_or(Error::Vegetation(
                    saffron_vegetation::Error::NumericOverflow,
                ))
        })?;
        let cell_dependencies = result
            .ancestor_references
            .iter()
            .map(|ancestor| {
                let content_hash = cell_outputs.get(ancestor).copied().ok_or_else(|| {
                    Error::Io(format!(
                        "vegetation cell {cell} references uncooked ancestor {ancestor}"
                    ))
                })?;
                Ok(ManifestCellDependency {
                    cell: *ancestor,
                    content_hash,
                    role: ManifestCellDependencyRole::Ancestor,
                    halo: DecisionScalar::from_bits(0),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        cells.push(VegetationManifestCell {
            cell,
            bounds: cell.bounds(),
            artifact_hash: publication.content_hash,
            payload_hash: index.payload_hash,
            dependencies: cell_dependencies,
            species_counts,
            macro_count,
            micro_count,
            resident_memory_bytes: estimate.peak_memory_bytes,
            stored_bytes: publication.bytes,
            estimate,
            actual: node.actual,
            sections: manifest_sections(&index),
        });
        cell_outputs.insert(cell, publication.content_hash);
        emit(VegetationCookEvent::Completed {
            node: address,
            cache_hit: publication.cache_hit,
            published_cell: true,
        });
        nodes.push(node);
    }

    let cook_graph = CookGraph {
        versions,
        platform: request.platform.clone(),
        nodes,
    };
    if u64::try_from(cook_graph.nodes.len()).unwrap_or(u64::MAX) != total_nodes {
        return Err(Error::Io(
            "vegetation cook plan node count changed during execution".to_owned(),
        ));
    }
    let cook_graph_bytes = cook_graph.canonical_bytes()?;
    let cook_graph_hash = cook_graph.identity()?;
    let graph_publication = store.publish_cook_graph(&cook_graph_bytes)?;
    if graph_publication.content_hash != cook_graph_hash {
        return Err(Error::VegetationArtifactHash {
            path: graph_publication.path.display().to_string(),
        });
    }
    let mut manifest = VegetationBaseManifest::current(
        request.world,
        request.map,
        map_hash,
        versions,
        request.platform,
        cook_graph_hash,
    );
    manifest.dependencies = manifest_dependencies(&cook_graph, request.map, map_hash)?;
    manifest.seed_namespaces = seed_namespaces(request.world, request.map, &map);
    manifest.plants = plants;
    manifest.cells = cells;
    let manifest_bytes = manifest.canonical_bytes()?;
    let manifest_identity = manifest.identity()?;
    cancellation_checkpoint(cancellation)?;
    let publication = store.publish_manifest(&manifest_bytes)?;
    if publication.content_hash != manifest_identity {
        return Err(Error::VegetationArtifactHash {
            path: publication.path.display().to_string(),
        });
    }
    let statistics = cook_statistics(&cook_graph, elapsed_micros(started), rejection_totals);
    let authored_guards = assets.guards();
    for guard in &authored_guards {
        guard.validate()?;
    }
    Ok(StagedVegetationCook {
        project,
        map: request.map,
        expected_manifest: request.expected_manifest,
        authored_guards,
        surface_descriptors: request
            .surface_providers
            .iter()
            .map(|provider| provider.descriptor())
            .collect(),
        source_acceptances,
        cook_graph,
        manifest,
        manifest_bytes,
        manifest_identity,
        statistics,
    })
}

/// Commits one fully staged generation after revalidating every live authored input.
pub fn commit_staged_vegetation_cook(
    assets: &mut AssetServer,
    current_surfaces: &[Arc<dyn SurfaceField>],
    staged: StagedVegetationCook,
    cancellation: &GraphCancellationToken,
) -> Result<VegetationCookOutput> {
    cancellation_checkpoint(cancellation)?;
    if assets.root != staged.project.asset_root
        || assets.vegetation_cache_root != staged.project.cache_root
    {
        return Err(Error::VegetationCookInputChanged {
            path: staged.project.asset_root.display().to_string(),
        });
    }
    let store = assets.vegetation_artifact_store();
    let _authored_lock = store.lock_authored()?;
    let generation_lock = store.lock_generation(staged.map)?;
    recover_source_transaction(assets, &store, staged.map)?;
    validate_live_catalog(assets, &staged)?;
    for guard in &staged.authored_guards {
        guard.validate()?;
    }
    let mut surface_descriptors = current_surfaces
        .iter()
        .map(|provider| provider.descriptor())
        .collect::<Vec<_>>();
    surface_descriptors.sort_by_key(|descriptor| (descriptor.id, descriptor.revision));
    if surface_descriptors != staged.surface_descriptors {
        return Err(Error::VegetationCookInputChanged {
            path: "surface-providers".to_owned(),
        });
    }
    if store.current_manifest_hash(staged.map)? != staged.expected_manifest {
        return Err(Error::VegetationGenerationSuperseded {
            map: staged.map.value(),
            expected: optional_content_hash(staged.expected_manifest),
            current: optional_content_hash(store.current_manifest_hash(staged.map)?),
        });
    }

    let mut originals = Vec::with_capacity(staged.source_acceptances.len());
    for acceptance in &staged.source_acceptances {
        let original = crate::load_plant_family_asset(assets, acceptance.family)?;
        if ContentHash::of(&write_plant_asset(&original)?) != acceptance.expected_authored_hash {
            return Err(Error::VegetationCookInputChanged {
                path: format!("plant-family/{}", acceptance.family.value()),
            });
        }
        originals.push(original);
    }
    let mut installed = 0_usize;
    let journal = SourceTransactionJournal {
        map: staged.map,
        expected_manifest: staged.expected_manifest,
        new_manifest: staged.manifest_identity,
        entries: staged
            .source_acceptances
            .iter()
            .zip(&originals)
            .map(|(acceptance, original)| {
                Ok(SourceTransactionEntry {
                    family: acceptance.family,
                    old_bytes: write_plant_asset(original)?,
                    new_bytes: write_plant_asset(&acceptance.accepted_asset)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let journal_path = source_transaction_path(&store, staged.map);
    write_source_transaction(&journal_path, &journal)?;
    for acceptance in &staged.source_acceptances {
        if let Err(error) = cancellation_checkpoint(cancellation) {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
        if let Err(error) =
            update_plant_family_asset(assets, acceptance.family, &acceptance.accepted_asset)
        {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
        installed += 1;
    }
    let publication = match store.publish_generation_locked(
        &generation_lock,
        staged.expected_manifest,
        &staged.manifest_bytes,
    ) {
        Ok(publication) => publication,
        Err(error) => {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
    };
    let _ = std::fs::remove_file(journal_path);
    Ok(VegetationCookOutput {
        cook_graph: staged.cook_graph,
        manifest: staged.manifest,
        manifest_identity: staged.manifest_identity,
        publication,
        statistics: staged.statistics,
    })
}

const SOURCE_TRANSACTION_MAGIC: &[u8; 8] = b"SVTXN001";

struct SourceTransactionEntry {
    family: Uuid,
    old_bytes: Vec<u8>,
    new_bytes: Vec<u8>,
}

struct SourceTransactionJournal {
    map: Uuid,
    expected_manifest: Option<ContentHash>,
    new_manifest: ContentHash,
    entries: Vec<SourceTransactionEntry>,
}

fn source_transaction_path(store: &VegetationArtifactStore, map: Uuid) -> std::path::PathBuf {
    store
        .root()
        .join("transactions")
        .join(format!("map-{}.txn", map.value()))
}

fn write_source_transaction(
    path: &std::path::Path,
    journal: &SourceTransactionJournal,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Io("vegetation transaction journal has no parent".to_owned()))?;
    std::fs::create_dir_all(parent).map_err(|error| Error::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SOURCE_TRANSACTION_MAGIC);
    bytes.extend_from_slice(&journal.map.value().to_be_bytes());
    bytes.push(u8::from(journal.expected_manifest.is_some()));
    bytes.extend_from_slice(&journal.expected_manifest.unwrap_or_default().bytes());
    bytes.extend_from_slice(&journal.new_manifest.bytes());
    bytes.extend_from_slice(
        &u32::try_from(journal.entries.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?
            .to_be_bytes(),
    );
    for entry in &journal.entries {
        bytes.extend_from_slice(&entry.family.value().to_be_bytes());
        append_journal_bytes(&mut bytes, &entry.old_bytes)?;
        append_journal_bytes(&mut bytes, &entry.new_bytes)?;
    }
    let mut file = AtomicWriteFile::options()
        .open(path)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.write_all(&bytes)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.commit().map_err(|error| Error::Io(error.to_string()))
}

fn append_journal_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    output.extend_from_slice(
        &u64::try_from(bytes.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?
            .to_be_bytes(),
    );
    output.extend_from_slice(bytes);
    Ok(())
}

fn recover_source_transaction(
    assets: &mut AssetServer,
    store: &VegetationArtifactStore,
    map: Uuid,
) -> Result<()> {
    let path = source_transaction_path(store, map);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error.to_string())),
    };
    let journal = read_source_transaction(&bytes)?;
    if journal.map != map {
        return Err(Error::Io(
            "vegetation transaction journal belongs to another map".to_owned(),
        ));
    }
    let current = store.current_manifest_hash(map)?;
    let use_new = if current == journal.expected_manifest {
        false
    } else if current == Some(journal.new_manifest) {
        true
    } else {
        return Err(Error::VegetationTransactionConflict {
            map: map.value(),
            current: optional_content_hash(current),
        });
    };
    for entry in &journal.entries {
        let bytes = if use_new {
            &entry.new_bytes
        } else {
            &entry.old_bytes
        };
        let asset = saffron_vegetation::read_plant_asset(bytes)?;
        if asset.id != entry.family {
            return Err(Error::Io(
                "vegetation transaction plant identity is invalid".to_owned(),
            ));
        }
        update_plant_family_asset(assets, entry.family, &asset)?;
    }
    std::fs::remove_file(path).map_err(|error| Error::Io(error.to_string()))
}

fn read_source_transaction(bytes: &[u8]) -> Result<SourceTransactionJournal> {
    let mut cursor = 0_usize;
    if take_journal(bytes, &mut cursor, SOURCE_TRANSACTION_MAGIC.len())? != SOURCE_TRANSACTION_MAGIC
    {
        return Err(Error::Io(
            "vegetation transaction journal magic is invalid".to_owned(),
        ));
    }
    let map = Uuid(read_journal_u64(bytes, &mut cursor)?);
    let expected_present = match take_journal(bytes, &mut cursor, 1)?[0] {
        0 => false,
        1 => true,
        _ => {
            return Err(Error::Io(
                "vegetation transaction expected-root flag is invalid".to_owned(),
            ));
        }
    };
    let expected_hash = ContentHash::new(read_journal_array(bytes, &mut cursor)?);
    if expected_present == expected_hash.is_zero() {
        return Err(Error::Io(
            "vegetation transaction expected root is invalid".to_owned(),
        ));
    }
    let expected_manifest = expected_present.then_some(expected_hash);
    let new_manifest = ContentHash::new(read_journal_array(bytes, &mut cursor)?);
    if new_manifest.is_zero() {
        return Err(Error::Io(
            "vegetation transaction new root is invalid".to_owned(),
        ));
    }
    let count = usize::try_from(read_journal_u32(bytes, &mut cursor)?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    if count > bytes.len().saturating_sub(cursor) / 24 {
        return Err(Error::Io(
            "vegetation transaction entry count exceeds its payload".to_owned(),
        ));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let family = Uuid(read_journal_u64(bytes, &mut cursor)?);
        let old_bytes = read_journal_bytes(bytes, &mut cursor)?;
        let new_bytes = read_journal_bytes(bytes, &mut cursor)?;
        if family.value() == 0 {
            return Err(Error::Io(
                "vegetation transaction plant identity is invalid".to_owned(),
            ));
        }
        entries.push(SourceTransactionEntry {
            family,
            old_bytes,
            new_bytes,
        });
    }
    if cursor != bytes.len() {
        return Err(Error::Io(
            "vegetation transaction journal has trailing bytes".to_owned(),
        ));
    }
    Ok(SourceTransactionJournal {
        map,
        expected_manifest,
        new_manifest,
        entries,
    })
}

fn read_journal_bytes(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    let length = usize::try_from(read_journal_u64(bytes, cursor)?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    Ok(take_journal(bytes, cursor, length)?.to_vec())
}

fn read_journal_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    Ok(u32::from_be_bytes(read_journal_array(bytes, cursor)?))
}

fn read_journal_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    Ok(u64::from_be_bytes(read_journal_array(bytes, cursor)?))
}

fn read_journal_array<const N: usize>(bytes: &[u8], cursor: &mut usize) -> Result<[u8; N]> {
    take_journal(bytes, cursor, N)?
        .try_into()
        .map_err(|_| Error::Io("vegetation transaction journal is truncated".to_owned()))
}

fn take_journal<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| Error::Io("vegetation transaction journal length overflowed".to_owned()))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| Error::Io("vegetation transaction journal is truncated".to_owned()))?;
    *cursor = end;
    Ok(value)
}

fn validate_live_catalog(assets: &AssetServer, staged: &StagedVegetationCook) -> Result<()> {
    let mut ids = BTreeSet::from([staged.map.value()]);
    ids.extend(
        staged
            .manifest
            .plants
            .iter()
            .map(|plant| plant.family.value()),
    );
    for dependency in &staged.manifest.dependencies {
        match dependency.address {
            CookDependencyAddress::SourceAsset { asset }
            | CookDependencyAddress::MaterialCoverage { material: asset } => {
                ids.insert(asset.value());
            }
            CookDependencyAddress::SourceFile { .. }
            | CookDependencyAddress::BiomeIr { .. }
            | CookDependencyAddress::MapManifest { .. }
            | CookDependencyAddress::MapObject { .. }
            | CookDependencyAddress::SurfaceProvider { .. }
            | CookDependencyAddress::SurfaceTile { .. }
            | CookDependencyAddress::Contract { .. }
            | CookDependencyAddress::Node(_) => {}
        }
    }
    let mut pending = ids.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let expected = staged.project.catalog.find(Uuid(id));
        let current = assets.catalog.find(Uuid(id));
        if expected != current {
            return Err(Error::VegetationCookInputChanged {
                path: format!("catalog/{id}"),
            });
        }
        if let Some(entry) = expected
            && entry.container.value() != 0
            && ids.insert(entry.container.value())
        {
            pending.push(entry.container.value());
        }
    }
    Ok(())
}

fn rollback_source_acceptances(
    assets: &mut AssetServer,
    acceptances: &[PlantSourceAcceptance],
    originals: &[saffron_vegetation::PlantFamilyAsset],
    installed: usize,
) -> Result<()> {
    for index in (0..installed).rev() {
        update_plant_family_asset(assets, acceptances[index].family, &originals[index])?;
    }
    Ok(())
}

fn validate_request(request: &mut VegetationCookRequest) -> Result<()> {
    if request.world.value() == 0 || request.map.value() == 0 {
        return Err(Error::Io(
            "vegetation cook requires non-zero world and map identities".to_owned(),
        ));
    }
    if request.workers == 0 {
        return Err(Error::Io(
            "vegetation cook worker count must be greater than zero".to_owned(),
        ));
    }
    if request.cells.is_empty() {
        return Err(Error::Io(
            "vegetation cook requires at least one output cell".to_owned(),
        ));
    }
    request.platform.identity()?;
    request.cells.sort_unstable();
    if request.cells.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::Io(
            "vegetation cook output cells contain a duplicate".to_owned(),
        ));
    }
    request.surface_providers.sort_by_key(|provider| {
        let descriptor = provider.descriptor();
        (descriptor.id, descriptor.revision)
    });
    if request
        .surface_providers
        .windows(2)
        .any(|pair| pair[0].descriptor().id == pair[1].descriptor().id)
    {
        return Err(Error::Io(
            "vegetation cook surface providers contain a duplicate identity".to_owned(),
        ));
    }
    Ok(())
}

fn intersecting_cells(cells: &[WorldCellKey], bounds: WorldBounds) -> Vec<WorldCellKey> {
    cells
        .iter()
        .copied()
        .filter(|cell| bounds_intersect(cell.bounds(), bounds))
        .collect()
}

fn clone_surface_providers(providers: &[Arc<dyn SurfaceField>]) -> Vec<Arc<dyn SurfaceField>> {
    providers.iter().map(Arc::clone).collect()
}

fn optional_content_hash(hash: Option<ContentHash>) -> String {
    hash.map_or_else(|| "none".to_owned(), |hash| hash.to_string())
}

fn plant_estimate(
    compile: &saffron_vegetation::PlantCompileOutput,
    output_bytes: u64,
) -> CookWorkEstimate {
    CookWorkEstimate {
        work_units: compile
            .statistics
            .vertices
            .saturating_add(compile.statistics.indices)
            .saturating_add(compile.statistics.joints)
            .saturating_add(compile.statistics.materials),
        peak_memory_bytes: output_bytes,
        input_bytes: compile.statistics.sources.saturating_mul(32),
        output_bytes,
    }
}

fn append_global_nodes(
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

struct CellDependencyContext<'a> {
    evaluated: &'a [EvaluatedInstance],
    map: &'a VegetationMapSnapshot,
    request: &'a VegetationCookRequest,
    plant_outputs: &'a BTreeMap<u64, ContentHash>,
    global_outputs: &'a BTreeMap<(u128, ContentHash, WorldCellKey), ContentHash>,
}

fn cell_dependencies(
    cell: WorldCellKey,
    result: &GraphEvaluationResult,
    context: &CellDependencyContext<'_>,
    cell_outputs: &BTreeMap<WorldCellKey, ContentHash>,
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
                    map: context.request.map,
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
            let (channel_tag, channel_user) = field_channel_identity(channel);
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

const fn field_channel_identity(channel: saffron_spatial::FieldChannel) -> (u8, u64) {
    use saffron_spatial::FieldChannel;
    match channel {
        FieldChannel::Altitude => (0, 0),
        FieldChannel::Slope => (1, 0),
        FieldChannel::Curvature => (2, 0),
        FieldChannel::Concavity => (3, 0),
        FieldChannel::Drainage => (4, 0),
        FieldChannel::Moisture => (5, 0),
        FieldChannel::Temperature => (6, 0),
        FieldChannel::Precipitation => (7, 0),
        FieldChannel::Sunlight => (8, 0),
        FieldChannel::Exposure => (9, 0),
        FieldChannel::WaterDistance => (10, 0),
        FieldChannel::WaterDepth => (11, 0),
        FieldChannel::SignedBlocker => (12, 0),
        FieldChannel::SplineDistance => (13, 0),
        FieldChannel::User(value) => (14, value),
    }
}

fn field_channel_name(channel: saffron_spatial::FieldChannel) -> String {
    let (tag, user) = field_channel_identity(channel);
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

fn push_dependency(
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

fn manifest_dependencies(
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

fn seed_namespaces(
    world: Uuid,
    map: Uuid,
    snapshot: &VegetationMapSnapshot,
) -> Vec<VegetationSeedNamespace> {
    let mut seeds = vec![VegetationSeedNamespace {
        name: "vegetation-world".to_owned(),
        namespace: derive_seed_namespace(world, map, 0),
    }];
    seeds.extend(
        snapshot
            .biome_instances
            .iter()
            .map(|instance| VegetationSeedNamespace {
                name: format!("biome-instance/{:032x}", instance.id),
                namespace: derive_seed_namespace(world, map, instance.id),
            }),
    );
    seeds
}

fn derive_seed_namespace(world: Uuid, map: Uuid, domain: u128) -> u128 {
    let mut bytes = Vec::with_capacity(40);
    bytes.extend_from_slice(b"saffron-anima/vegetation-seed/v1\0");
    bytes.extend_from_slice(&world.value().to_be_bytes());
    bytes.extend_from_slice(&map.value().to_be_bytes());
    bytes.extend_from_slice(&domain.to_be_bytes());
    let hash = vegetation_content_hash(&bytes);
    let value = u128::from_be_bytes(hash[..16].try_into().expect("SHA-256 prefix"));
    if value == 0 { 1 } else { value }
}

fn cook_statistics(
    graph: &CookGraph,
    elapsed_micros: u64,
    rejection_totals: [u64; 7],
) -> VegetationCookStatistics {
    let reasons = [
        CandidateRejectionReason::SurfaceMiss,
        CandidateRejectionReason::Threshold,
        CandidateRejectionReason::WeightedElimination,
        CandidateRejectionReason::PriorityExclusion,
        CandidateRejectionReason::Competition,
        CandidateRejectionReason::ForeignOwner,
        CandidateRejectionReason::NoSpecies,
    ];
    VegetationCookStatistics {
        nodes: u64::try_from(graph.nodes.len()).unwrap_or(u64::MAX),
        elapsed_micros,
        peak_memory_bytes: graph
            .nodes
            .iter()
            .map(|node| node.actual.peak_memory_bytes)
            .max()
            .unwrap_or(0),
        input_bytes: graph.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.actual.input_bytes)
        }),
        output_bytes: graph.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.actual.output_bytes)
        }),
        cache_hits: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| node.actual.cache_hit)
                .count(),
        )
        .unwrap_or(u64::MAX),
        cache_misses: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| !node.actual.cache_hit)
                .count(),
        )
        .unwrap_or(u64::MAX),
        published_cells: u64::try_from(
            graph
                .nodes
                .iter()
                .filter(|node| matches!(&node.address, CookNodeAddress::Cell { .. }))
                .count(),
        )
        .unwrap_or(u64::MAX),
        rejections: reasons
            .into_iter()
            .enumerate()
            .map(|(index, reason)| (reason, rejection_totals[index]))
            .collect(),
    }
}

const fn rejection_reason_index(reason: CandidateRejectionReason) -> usize {
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

fn bounds_intersect(
    left: saffron_spatial::WorldBounds,
    right: saffron_spatial::WorldBounds,
) -> bool {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    (0..3).all(|axis| {
        left_minimum[axis] < right_maximum[axis] && right_minimum[axis] < left_maximum[axis]
    })
}

fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
    GraphEvaluationResult {
        cell,
        macro_points: PlantPointColumns::default(),
        micro_fields: Vec::new(),
        surface_projection_tiles: Vec::new(),
        surface_field_query_tiles: Vec::new(),
        ancestor_references: Vec::new(),
        provenance: ProvenanceTable::default(),
        diagnostics: GraphEvaluationDiagnostics::default(),
    }
}

fn species_counts(result: &GraphEvaluationResult) -> Result<Vec<ManifestSpeciesCount>> {
    let mut counts = BTreeMap::<u64, (Uuid, u64, u64)>::new();
    for family in &result.macro_points.families {
        let count = counts.entry(family.value()).or_insert((*family, 0, 0));
        count.1 = count.1.checked_add(1).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    }
    for tile in &result.micro_fields {
        let samples = u64::try_from(tile.density.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
        let count = counts
            .entry(tile.family.value())
            .or_insert((tile.family, 0, 0));
        count.2 = count.2.checked_add(samples).ok_or(Error::Vegetation(
            saffron_vegetation::Error::NumericOverflow,
        ))?;
    }
    Ok(counts
        .into_iter()
        .map(
            |(_, (family, macro_count, micro_count))| ManifestSpeciesCount {
                family,
                macro_count,
                micro_count,
            },
        )
        .collect())
}

fn manifest_sections(index: &VegetationCellArtifactIndex) -> Vec<ManifestCellSection> {
    index
        .sections
        .iter()
        .map(|section| ManifestCellSection {
            kind: section.kind,
            version: section.version,
            codec: section.codec,
            alignment: section.alignment,
            stored_size: section.stored_size,
            decoded_size: section.decoded_size,
            content_hash: section.content_hash,
        })
        .collect()
}

fn result_estimate(result: &GraphEvaluationResult) -> Result<CookWorkEstimate> {
    let output_bytes = u64::try_from(result.canonical_byte_len()?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    let micro_samples = result.micro_fields.iter().try_fold(0_u64, |total, tile| {
        total
            .checked_add(
                u64::try_from(tile.density.len())
                    .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
            )
            .ok_or(Error::Vegetation(
                saffron_vegetation::Error::NumericOverflow,
            ))
    })?;
    Ok(CookWorkEstimate {
        work_units: result
            .diagnostics
            .candidate_count
            .saturating_add(result.diagnostics.accepted_count)
            .saturating_add(micro_samples),
        peak_memory_bytes: output_bytes,
        input_bytes: 0,
        output_bytes,
    })
}

fn result_actual(
    result: &GraphEvaluationResult,
    output_bytes: u64,
    cache_hit: bool,
) -> CookWorkActual {
    CookWorkActual {
        elapsed_micros: result.diagnostics.nodes.iter().fold(0_u64, |total, node| {
            total.saturating_add(node.elapsed_micros)
        }),
        peak_memory_bytes: result
            .canonical_byte_len()
            .ok()
            .and_then(|bytes| u64::try_from(bytes).ok())
            .unwrap_or(u64::MAX),
        input_bytes: 0,
        output_bytes,
        rejection_count: u64::try_from(result.diagnostics.rejected.len()).unwrap_or(u64::MAX),
        cache_hit,
    }
}

fn cancellation_checkpoint(cancellation: &GraphCancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Vegetation(saffron_vegetation::Error::GraphCancelled))
    } else {
        Ok(())
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Builds the required portable content profile for this compile target.
pub fn portable_vegetation_platform_profile(content_profile: Option<&str>) -> CookPlatformProfile {
    CookPlatformProfile {
        target: target_triple(),
        content_profile: content_profile
            .filter(|profile| !profile.is_empty())
            .unwrap_or("portable-vulkan")
            .to_owned(),
        toolchain: include_str!("../../../../rust-toolchain.toml")
            .trim()
            .to_owned(),
        features: vec![
            "compute".to_owned(),
            "indexed-multi-draw-indirect".to_owned(),
            "vulkan-1.4".to_owned(),
        ],
    }
}

/// Returns the exact semantic contract versions used by the production cooker.
#[must_use]
pub const fn vegetation_cook_versions() -> CookVersionSet {
    CookVersionSet::current()
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn target_triple() -> String {
    "aarch64-apple-darwin".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
fn target_triple() -> String {
    "x86_64-apple-darwin".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn target_triple() -> String {
    "x86_64-unknown-linux-gnu".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
fn target_triple() -> String {
    "aarch64-unknown-linux-gnu".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "musl"))]
fn target_triple() -> String {
    "x86_64-unknown-linux-musl".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "musl"))]
fn target_triple() -> String {
    "aarch64-unknown-linux-musl".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
fn target_triple() -> String {
    "x86_64-pc-windows-msvc".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "windows", target_env = "msvc"))]
fn target_triple() -> String {
    "aarch64-pc-windows-msvc".to_owned()
}

#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
    all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "musl"),
    all(target_arch = "aarch64", target_os = "linux", target_env = "musl"),
    all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"),
    all(target_arch = "aarch64", target_os = "windows", target_env = "msvc")
)))]
fn target_triple() -> String {
    format!(
        "{}-unknown-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use saffron_vegetation::{
        VEGETATION_MAP_VERSION, VegetationMapAsset, VegetationMapChunkLayout,
        vegetation_map_chunk_schema_hash,
    };

    use super::*;

    static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    fn run_cook(
        assets: &mut AssetServer,
        request: VegetationCookRequest,
        cancellation: &GraphCancellationToken,
        emit: impl FnMut(VegetationCookEvent),
    ) -> Result<VegetationCookOutput> {
        let surfaces = clone_surface_providers(&request.surface_providers);
        let staged = stage_vegetation_cook(
            CookProjectView::capture(assets),
            request,
            cancellation,
            emit,
        )?;
        commit_staged_vegetation_cook(assets, &surfaces, staged, cancellation)
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "saffron-vegetation-cooker-{tag}-{}-{nanos}-{sequence}",
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

    fn save_empty_map(assets: &mut AssetServer, cells: &[WorldCellKey]) -> Result<Uuid> {
        let bounds = cells
            .iter()
            .map(|cell| cell.bounds())
            .reduce(WorldBounds::union)
            .expect("map fixture cells");
        crate::save_vegetation_map_asset(
            assets,
            VegetationMapAsset {
                version: VEGETATION_MAP_VERSION,
                id: Uuid(1),
                name: "Cook map".to_owned(),
                bounds,
                chunk_layout: VegetationMapChunkLayout {
                    level: 0,
                    schema_hash: vegetation_map_chunk_schema_hash(),
                },
                generation: 0,
                inventory: Vec::new(),
            },
            "Cook map",
            "",
        )
    }

    fn cook_request(
        world: Uuid,
        map: Uuid,
        expected_manifest: Option<ContentHash>,
        cells: Vec<WorldCellKey>,
        workers: u16,
    ) -> VegetationCookRequest {
        VegetationCookRequest {
            world,
            map,
            expected_manifest,
            cells,
            ecology_tick: 0,
            workers,
            platform: portable_vegetation_platform_profile(Some("test-portable")),
            surface_providers: Vec::new(),
        }
    }

    #[test]
    fn portable_profile_is_complete_and_canonical() {
        let profile = portable_vegetation_platform_profile(None);
        assert!(!profile.target.is_empty());
        assert_eq!(profile.content_profile, "portable-vulkan");
        assert!(profile.toolchain.contains("1.96.0"));
        assert!(profile.identity().is_ok());
        assert_eq!(vegetation_cook_versions(), CookVersionSet::current());
    }

    #[test]
    fn repeated_incremental_cooks_preserve_identity_and_prior_cells() {
        let scratch = Scratch::new("incremental-identity");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let first_cell = WorldCellKey::base(0, 0, 0);
        let second_cell = WorldCellKey::base(1, 0, 0);
        let map = save_empty_map(&mut assets, &[first_cell, second_cell]).unwrap();
        let world = Uuid(77);
        let cancellation = GraphCancellationToken::default();

        let first = run_cook(
            &mut assets,
            cook_request(world, map, None, vec![first_cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        let expanded = run_cook(
            &mut assets,
            cook_request(
                world,
                map,
                Some(first.manifest_identity),
                vec![second_cell],
                4,
            ),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            expanded
                .manifest
                .cells
                .iter()
                .map(|cell| cell.cell)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([first_cell, second_cell])
        );

        let repeated = run_cook(
            &mut assets,
            cook_request(
                world,
                map,
                Some(expanded.manifest_identity),
                vec![first_cell, second_cell],
                2,
            ),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(repeated.manifest_identity, expanded.manifest_identity);
        assert_eq!(
            repeated.manifest.canonical_bytes().unwrap(),
            expanded.manifest.canonical_bytes().unwrap()
        );
        assert_eq!(
            repeated.cook_graph.canonical_bytes().unwrap(),
            expanded.cook_graph.canonical_bytes().unwrap()
        );
        assert_eq!(repeated.statistics.cache_hits, 2);
    }

    #[test]
    fn deleting_cell_cache_rebuilds_the_same_generation() {
        let scratch = Scratch::new("cache-rebuild");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(-2, 0, 3);
        let map = save_empty_map(&mut assets, &[cell]).unwrap();
        let world = Uuid(88);
        let cancellation = GraphCancellationToken::default();
        let first = run_cook(
            &mut assets,
            cook_request(world, map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        let cell_hash = first.manifest.cells[0].artifact_hash;
        let mut reader = assets
            .vegetation_artifact_store()
            .open_cell(cell_hash)
            .unwrap();
        assert_eq!(reader.index().cell, cell);
        assert!(
            reader
                .read_section(saffron_vegetation::VegetationCellSectionKind::MacroPoints)
                .unwrap()
                .is_some()
        );
        let cell_path = assets
            .vegetation_artifact_store()
            .path(crate::VegetationArtifactKind::Cell, cell_hash);
        std::fs::remove_file(cell_path).unwrap();

        let rebuilt = run_cook(
            &mut assets,
            cook_request(world, map, Some(first.manifest_identity), vec![cell], 3),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(rebuilt.manifest_identity, first.manifest_identity);
        assert_eq!(rebuilt.manifest.cells[0].artifact_hash, cell_hash);
        assert_eq!(rebuilt.statistics.cache_misses, 1);
    }

    #[test]
    fn cancelled_staged_cook_never_advances_the_generation_root() {
        let scratch = Scratch::new("cancelled-stage");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(0, 0, 0);
        let map = save_empty_map(&mut assets, &[cell]).unwrap();
        let cancellation = GraphCancellationToken::default();
        let staged = stage_vegetation_cook(
            CookProjectView::capture(&assets),
            cook_request(Uuid(99), map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        cancellation.cancel();
        assert!(matches!(
            commit_staged_vegetation_cook(&mut assets, &[], staged, &cancellation),
            Err(Error::Vegetation(saffron_vegetation::Error::GraphCancelled))
        ));
        assert_eq!(
            assets
                .vegetation_artifact_store()
                .current_manifest_hash(map)
                .unwrap(),
            None
        );
    }

    #[test]
    fn authored_edit_after_staging_rejects_commit_without_a_root() {
        let scratch = Scratch::new("changed-input");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(0, 0, 0);
        let map = save_empty_map(&mut assets, &[cell]).unwrap();
        let cancellation = GraphCancellationToken::default();
        let staged = stage_vegetation_cook(
            CookProjectView::capture(&assets),
            cook_request(Uuid(100), map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        let entry = assets.catalog.find(map).unwrap();
        let path = assets.root.join(&entry.path);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        std::fs::write(path, bytes).unwrap();
        assert!(matches!(
            commit_staged_vegetation_cook(&mut assets, &[], staged, &cancellation),
            Err(Error::VegetationCookInputChanged { .. })
        ));
        assert_eq!(
            assets
                .vegetation_artifact_store()
                .current_manifest_hash(map)
                .unwrap(),
            None
        );
    }

    #[test]
    fn source_transaction_journal_is_strict_and_roundtrips() {
        let scratch = Scratch::new("transaction-codec");
        let path = scratch.path().join("journal.txn");
        let journal = SourceTransactionJournal {
            map: Uuid(41),
            expected_manifest: Some(ContentHash::of(b"old")),
            new_manifest: ContentHash::of(b"new"),
            entries: vec![SourceTransactionEntry {
                family: Uuid(42),
                old_bytes: vec![1, 2, 3],
                new_bytes: vec![4, 5],
            }],
        };
        write_source_transaction(&path, &journal).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let decoded = read_source_transaction(&bytes).unwrap();
        assert_eq!(decoded.map, journal.map);
        assert_eq!(decoded.expected_manifest, journal.expected_manifest);
        assert_eq!(decoded.new_manifest, journal.new_manifest);
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].family, Uuid(42));
        assert_eq!(decoded.entries[0].old_bytes, vec![1, 2, 3]);
        assert_eq!(decoded.entries[0].new_bytes, vec![4, 5]);
        assert!(read_source_transaction(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(read_source_transaction(&trailing).is_err());
    }
}
