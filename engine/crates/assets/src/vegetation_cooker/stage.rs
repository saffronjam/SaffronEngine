//! Staging one complete generation: plant cooks, graph evaluation, the distributed work
//! plan, and the manifest that names the result.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use saffron_core::Uuid;
use saffron_spatial::{WorldBounds, WorldCellKey};
use saffron_vegetation::{
    CompiledBiomeGraph, ContentHash, CookGraph, CookNodeAddress, CookNodeRecord, CookWorkActual,
    CookWorkEstimate, GlobalStageEvaluationResult, GraphCancellationToken, GraphCompileOptions,
    GraphEvaluationResult, PlantCompileLimits, PlantCompiledArtifactIndex, VegetationBaseManifest,
    VegetationManifestCell, VegetationManifestPlant, VegetationMapSnapshot,
    VegetationSeedNamespace, merge_graph_evaluation_results, vegetation_content_hash,
    write_plant_asset, write_vegetation_map_asset,
};

use crate::cook_reader::CookAssetReader;
use crate::plant_cook::{
    dependency_input_bytes, prepare_plant_family_sources_from, stage_prepared_plant_family,
};
use crate::vegetation::{
    assemble_biome_graph_evaluation_job_from, compile_catalog_biome_instance_graph_from,
    load_plant_family_asset_from, load_vegetation_map_snapshot_from,
    vegetation_graph_dependency_hashes_from,
};
use crate::{
    CookProjectView, Error, PlantRecookOptions, PlantRecookOutcome, PreparedPlantFamily, Result,
    VegetationArtifactStore,
};

use super::dependencies::{
    CellDependencyContext, append_global_nodes, cell_own_dependencies, manifest_dependencies,
};
use super::measure::{
    cook_statistics, elapsed_micros, empty_result, rejection_reason_index, result_estimate,
};
use super::platform::vegetation_cook_versions;
use super::work::{WORK_CLAIM_LEASE, run_work_items};
use super::{
    PlantSourceAcceptance, StagedVegetationCook, VegetationCookEvent, VegetationCookRequest,
    bounds_intersect, cancellation_checkpoint, clone_surface_providers,
};
use crate::vegetation_store::optional_hash;

pub(super) struct EvaluatedInstance {
    pub(super) id: u128,
    pub(super) graph: CompiledBiomeGraph,
    pub(super) cell_read_bounds: BTreeMap<WorldCellKey, WorldBounds>,
    pub(super) global_inputs: BTreeMap<([u8; 32], WorldCellKey), GlobalInputRecord>,
    pub(super) global_results: Vec<GlobalStageEvaluationResult>,
    pub(super) cells: Vec<GraphEvaluationResult>,
}

struct PreviousGeneration {
    manifest: VegetationBaseManifest,
    nodes: BTreeMap<CookNodeAddress, CookNodeRecord>,
    plants: BTreeMap<u64, VegetationManifestPlant>,
    cells: BTreeMap<WorldCellKey, VegetationManifestCell>,
}

#[derive(Clone, Copy)]
pub(super) struct GlobalInputRecord {
    pub(super) read_bounds: WorldBounds,
    pub(super) input_snapshot: ContentHash,
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
    let index = PlantCompiledArtifactIndex::open(
        &bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
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
            input_bytes: dependency_input_bytes(&prepared.validation.dependencies),
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
        ecology: prepared.accepted_asset.ecology.clone(),
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
            expected: optional_hash(request.expected_manifest),
            current: optional_hash(store.current_manifest_hash(request.map)?),
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
            ecology: publication.accepted_asset.ecology.clone(),
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

    let dependency_context = CellDependencyContext {
        evaluated: &evaluated,
        map: &map,
        request: &request,
        plant_outputs: &plant_outputs,
        global_outputs: &global_outputs,
    };
    // The distributed work plan: one claimable item per cell, coarsest level first. Each item's
    // payload — its own-input dependency half — publishes to the store, so the plan is durable and
    // self-describing; `blocked_by` names the containing planned cell at every coarser level, which
    // bounds the evaluation's actual ancestor references.
    let planned_levels = merged_results
        .iter()
        .map(|(cell, _)| cell.level())
        .collect::<BTreeSet<_>>();
    let mut items = Vec::<saffron_vegetation::CookWorkItem>::new();
    let mut item_index_by_cell = BTreeMap::<WorldCellKey, u32>::new();
    for (cell, result) in &merged_results {
        cancellation_checkpoint(cancellation)?;
        let address = CookNodeAddress::Cell {
            map: request.map,
            cell: *cell,
        };
        let own_dependencies = cell_own_dependencies(*cell, &dependency_context)?;
        let payload = saffron_vegetation::CookWorkPayload {
            address: address.clone(),
            own_dependencies: own_dependencies.clone(),
        };
        let payload_publication = store.publish_work_payload(&payload.canonical_bytes()?)?;
        let own_input_key = saffron_vegetation::cook_work_own_input_key(
            versions,
            &request.platform,
            &address,
            &own_dependencies,
        )?;
        let mut blocked_by = Vec::new();
        for &level in planned_levels.iter().filter(|&&level| level > cell.level()) {
            let ancestor = cell.ancestor(level).map_err(Error::Spatial)?;
            if let Some(&blocker) = item_index_by_cell.get(&ancestor) {
                blocked_by.push(blocker);
            }
        }
        let index = u32::try_from(items.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
        item_index_by_cell.insert(*cell, index);
        items.push(saffron_vegetation::CookWorkItem {
            address,
            own_input_key,
            payload: payload_publication.content_hash,
            blocked_by,
            estimate: result_estimate(result)?,
        });
    }
    let surface_provider_set_hash = if request.surface_providers.is_empty() {
        ContentHash::new([0; 32])
    } else {
        let mut providers = clone_surface_providers(&request.surface_providers);
        providers.sort_by_key(|provider| provider.descriptor().id);
        ContentHash::new(saffron_vegetation::canonical_surface_provider_set_hash(
            &providers,
            u64::try_from(providers.len())
                .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
        )?)
    };
    let work_manifest = saffron_vegetation::CookWorkManifest {
        versions,
        platform: request.platform.clone(),
        world: request.world,
        map: request.map,
        ecology_tick: request.ecology_tick,
        expected_manifest: request.expected_manifest,
        surface_provider_set_hash,
        items,
    };
    let work_manifest_bytes = work_manifest.canonical_bytes()?;
    let work_identity = work_manifest.identity()?;
    store.publish_work_manifest(&work_manifest_bytes)?;
    store.sweep_stale_work_claims(work_identity, WORK_CLAIM_LEASE)?;

    // Execute the plan: claimant workers race the on-disk claims and record completions; the
    // committer below assembles them in item order. This in-process pool is the local
    // degenerate case of a remote fleet — the claim protocol is the same either way.
    run_work_items(
        &store,
        work_identity,
        &work_manifest,
        merged_results,
        platform_profile,
        usize::from(request.workers.max(1)),
        cancellation,
        &mut emit,
    )?;

    // The single committer: read every completion in item order and assemble the generation's
    // manifest cells and cook-graph nodes.
    let mut cells = Vec::<VegetationManifestCell>::new();
    for index in 0..work_manifest.items.len() {
        let item = u32::try_from(index)
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
        let completion = store
            .read_work_completion_if_present(work_identity, item)?
            .ok_or_else(|| {
                Error::Io(format!(
                    "vegetation work item {item} finished without a completion record"
                ))
            })?;
        cells.push(completion.manifest_cell);
        nodes.push(completion.node);
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        AUTHORED_LAYER, Scratch, cell_artifact_hashes, cook_request, edit_field_chunk, run_cook,
        save_populated_map,
    };
    use super::*;
    use crate::AssetServer;
    use saffron_vegetation::{CookDependencyAddress, VegetationMapChunkKind, VegetationMapTileKey};

    /// Whether the cook-graph node for `cell` names the bounded field chunk `owner` owns — the one
    /// dependency the halo-expanded read bounds decide.
    fn depends_on_field_chunk(graph: &CookGraph, cell: WorldCellKey, owner: WorldCellKey) -> bool {
        graph
            .nodes
            .iter()
            .find(|node| matches!(&node.address, CookNodeAddress::Cell { cell: node, .. } if *node == cell))
            .expect("cooked cell node")
            .dependencies
            .iter()
            .any(|dependency| match &dependency.address {
                CookDependencyAddress::MapObject { key, .. } => {
                    key.layer == AUTHORED_LAYER
                        && key.kind == VegetationMapChunkKind::Field
                        && key.tile == VegetationMapTileKey::Cell(owner)
                }
                _ => false,
            })
    }

    fn cache_hit(events: &[VegetationCookEvent], cell: WorldCellKey) -> Option<bool> {
        events.iter().find_map(|event| match event {
            VegetationCookEvent::Completed {
                node: CookNodeAddress::Cell { cell: node, .. },
                cache_hit,
                ..
            } if *node == cell => Some(*cache_hit),
            _ => None,
        })
    }

    /// Editing one bounded authored field re-keys exactly the cells whose halo-expanded read bounds
    /// reach it. The far cell's bytes are untouched, so its content-addressed artifact is still
    /// there and the cook reports it as a hit.
    #[test]
    fn editing_one_bounded_field_invalidates_only_its_region_and_declared_halo() {
        let scratch = Scratch::new("bounded-field-scope");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let edited = WorldCellKey::base(0, 0, 0);
        let adjacent = WorldCellKey::base(1, 0, 0);
        let distant = WorldCellKey::base(2, 0, 0);
        let cells = vec![edited, adjacent, distant];
        // A one-metre halo reaches across exactly one cell boundary at this level.
        let populated = save_populated_map(&mut assets, &cells, 1).unwrap();
        let world = Uuid(101);
        let cancellation = GraphCancellationToken::default();

        let first = run_cook(
            &mut assets,
            cook_request(world, populated.map, None, cells.clone(), 2),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert!(
            first.manifest.cells.iter().all(|cell| cell.macro_count > 0),
            "the scope below is measured over populated cells"
        );
        let before = cell_artifact_hashes(&first);

        edit_field_chunk(&mut assets, populated.map, edited, 2, 4);
        let mut events = Vec::new();
        let second = run_cook(
            &mut assets,
            cook_request(
                world,
                populated.map,
                Some(first.manifest_identity),
                cells,
                2,
            ),
            &cancellation,
            |event| events.push(event),
        )
        .unwrap();
        let after = cell_artifact_hashes(&second);

        // The mechanism: the dependency region is the halo-expanded read bounds, so the adjacent
        // cell names the edited chunk and the far cell does not.
        assert!(depends_on_field_chunk(&second.cook_graph, edited, edited));
        assert!(depends_on_field_chunk(&second.cook_graph, adjacent, edited));
        assert!(!depends_on_field_chunk(&second.cook_graph, distant, edited));

        // The consequence: only that region republished.
        assert_ne!(before[&edited], after[&edited]);
        assert_ne!(before[&adjacent], after[&adjacent]);
        assert_eq!(before[&distant], after[&distant]);
        assert_eq!(cache_hit(&events, edited), Some(false));
        assert_eq!(cache_hit(&events, adjacent), Some(false));
        assert_eq!(cache_hit(&events, distant), Some(true));
    }

    #[test]
    fn repeated_incremental_cooks_preserve_identity_and_prior_cells() {
        let scratch = Scratch::new("incremental-identity");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let first_cell = WorldCellKey::base(0, 0, 0);
        let second_cell = WorldCellKey::base(1, 0, 0);
        let populated = save_populated_map(&mut assets, &[first_cell, second_cell], 0).unwrap();
        let map = populated.map;
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
        // Byte identity is only worth asserting over artifacts that carry content.
        assert!(
            expanded
                .manifest
                .cells
                .iter()
                .all(|cell| cell.macro_count > 0),
            "every published cell carries macro plants"
        );
        let expanded_hashes = cell_artifact_hashes(&expanded);
        let read_cells = |assets: &AssetServer| {
            expanded_hashes
                .values()
                .map(|hash| assets.vegetation_artifact_store().read_cell(*hash).unwrap())
                .collect::<Vec<_>>()
        };
        let expanded_bytes = read_cells(&assets);

        // A third schedule: two workers, and the request order reversed, which the cooker
        // canonicalizes before planning.
        let repeated = run_cook(
            &mut assets,
            cook_request(
                world,
                map,
                Some(expanded.manifest_identity),
                vec![second_cell, first_cell],
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
        assert_eq!(cell_artifact_hashes(&repeated), expanded_hashes);
        assert_eq!(read_cells(&assets), expanded_bytes);
        // One compiled family plus two cells, every one satisfied by bytes already in the store.
        assert_eq!(repeated.statistics.cache_hits, 3);
        assert_eq!(repeated.statistics.cache_misses, 0);
    }

    #[test]
    fn deleting_cell_cache_rebuilds_the_same_generation() {
        let scratch = Scratch::new("cache-rebuild");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(-2, 0, 3);
        let populated = save_populated_map(&mut assets, &[cell], 0).unwrap();
        let map = populated.map;
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
}
