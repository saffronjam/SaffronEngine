//! Biome-graph compilation, schema inspection, bounded evaluation, and provenance commands.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use saffron_assets::{
    CookProjectView, ResolvedBiomeGraph, VegetationCookRequest,
    assemble_biome_graph_evaluation_job, compile_catalog_biome_graph,
    compile_catalog_biome_instance_graph, load_vegetation_map_snapshot,
    portable_vegetation_platform_profile, scene_surface_field_snapshots,
    vegetation_graph_dependency_hashes,
};
use saffron_core::Uuid;
use saffron_protocol::{
    ProvenanceDecisionDto, ProvenanceDecisionOutcomeDto, ProvenanceDto, ProvenanceExplanationDto,
    Uuid as WireUuid, VegetationCandidateIdentityDto, VegetationCandidateRejectionReasonDto,
    VegetationCellInspectParams, VegetationCellInspectResult, VegetationCellSummaryDto,
    VegetationCompileBiomeParams, VegetationCompileBiomeResult, VegetationCompileTargetDto,
    VegetationCookJobDto, VegetationCookJobParams, VegetationCookParams, VegetationCookScopeDto,
    VegetationCookStatusDto, VegetationEvaluationJobDto, VegetationEvaluationJobParams,
    VegetationEvaluationStatusDto, VegetationExplainPointParams, VegetationExplainSubjectDto,
    VegetationGraphDependencyDto, VegetationGraphEstimateDto, VegetationGraphLimitsDto,
    VegetationGraphOperatorDto, VegetationGraphParameterDto, VegetationGraphPinDto, VegetationGuid,
    VegetationManifestParams, VegetationManifestResult, VegetationNodeSchemaDto,
    VegetationNodeSchemaParams, VegetationNodeSchemaResult, VegetationPreflightRegionParams,
    WorldBoundsDto, WorldCellDto,
};
use saffron_scene::{IdComponent, VegetationField};
use saffron_spatial::{
    FieldChannel, SurfaceField, WorldBounds, WorldCellKey, world_cells_covering_bounds,
};
use saffron_vegetation::{
    BiomeGraphEvaluator, CandidateIdentity, CandidateRejectionReason, ContentHash,
    GraphCompileOptions, GraphDependencySource, GraphOperator, GraphSafetyLimits, PlantId,
    ProvenanceDecisionOutcome, ProvenanceExplanation, VegetationBaseManifest,
};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

/// Registers the one production biome-graph control surface.
pub fn register_vegetation_commands(reg: &mut CommandRegistry) {
    crate::commands_vegetation_runtime::register_runtime_vegetation_commands(reg);
    reg.register::<VegetationCompileBiomeParams, VegetationCompileBiomeResult>(
        "vegetation-compile-biome",
        "compile a biome graph and inspect its dependencies, halo, estimates, and caps",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let resolved = compile_target(ctx, params.target)?;
            Ok(compile_result(&resolved))
        },
    );

    reg.register::<VegetationNodeSchemaParams, VegetationNodeSchemaResult>(
        "vegetation-node-schema",
        "inspect the typed pins, parameters, seed namespaces, and execution capability of biome nodes",
        |_ctx, params| {
            let operators = GraphOperator::ALL
                .iter()
                .copied()
                .filter(|operator| {
                    params
                        .operator
                        .map(graph_operator_from_dto)
                        .is_none_or(|filter| filter == *operator)
                })
                .map(node_schema)
                .collect();
            Ok(VegetationNodeSchemaResult { nodes: operators })
        },
    );

    reg.register::<VegetationPreflightRegionParams, VegetationEvaluationJobDto>(
        "vegetation-preflight-region",
        "assemble, comprehensively bound, and retain one biome evaluation without starting a worker",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = Uuid::from(params.map);
            let biome_instance = parse_guid(&params.biome_instance)?;
            let requested_bounds = parse_bounds(&params.bounds)?;
            let ecology_tick =
                parse_optional_u64(params.ecology_tick.as_deref(), "ecologyTick")?.unwrap_or(0);
            let providers = capture_surface_snapshots(ctx)?;
            let dependencies = vegetation_graph_dependency_hashes(ctx.assets, map, &providers)
                .map_err(Error::from)?;
            let resolved = compile_catalog_biome_instance_graph(
                ctx.assets,
                map,
                biome_instance,
                &dependencies,
                GraphCompileOptions::canonical(),
            )
            .map_err(Error::from)?;
            let map_asset = load_vegetation_map_snapshot(ctx.assets, map)
                .map_err(Error::from)?;
            let instance = map_asset
                .biome_instances
                .iter()
                .find(|instance| instance.id == biome_instance)
                .ok_or_else(|| {
                    Error::command("vegetation biome instance is not present in the map")
                })?;
            let bounds = intersect_bounds(requested_bounds, map_asset.bounds)
                .and_then(|value| intersect_bounds(value, instance.bounds))
                .ok_or_else(|| {
                    Error::command("evaluation bounds do not intersect the biome instance")
                })?;
            let cells = world_cells_covering_bounds(
                bounds,
                params.level,
                resolved.graph.limits.max_output_cells,
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let inputs = assemble_biome_graph_evaluation_job(
                ctx.assets,
                &resolved,
                map,
                &cells,
                ecology_tick,
                providers.iter().map(Arc::clone).collect(),
            )
            .map_err(Error::from)?;
            let workers = match params.workers {
                Some(workers) => usize::from(workers),
                None => std::thread::available_parallelism().map_or(1, usize::from),
            };
            let mut evaluator = BiomeGraphEvaluator::new(Arc::new(resolved.graph), workers)
                .map_err(Error::from)?;
            if let Some(compute) = ctx.vegetation_compute_executor()? {
                evaluator = evaluator.with_compute_executor(compute);
            }
            ctx.vegetation_jobs.prepare(evaluator, inputs)
        },
    );

    reg.register::<VegetationEvaluationJobParams, VegetationEvaluationJobDto>(
        "vegetation-start-evaluation",
        "start the exact evaluator and inputs retained by a prepared vegetation job",
        |ctx, params| ctx.vegetation_jobs.start(parse_u64(&params.job, "job")?),
    );

    reg.register::<VegetationEvaluationJobParams, VegetationEvaluationStatusDto>(
        "vegetation-evaluation-status",
        "poll one asynchronous vegetation evaluation",
        |ctx, params| ctx.vegetation_jobs.status(parse_u64(&params.job, "job")?),
    );

    reg.register::<VegetationEvaluationJobParams, VegetationEvaluationStatusDto>(
        "vegetation-cancel-evaluation",
        "cancel one asynchronous vegetation evaluation without publishing partial output",
        |ctx, params| ctx.vegetation_jobs.cancel(parse_u64(&params.job, "job")?),
    );

    reg.register::<VegetationExplainPointParams, ProvenanceExplanationDto>(
        "vegetation-explain-point",
        "explain an accepted plant or rejected candidate from a completed evaluation",
        |ctx, params| {
            let job = parse_u64(&params.job, "job")?;
            let cell = parse_cell(&params.cell)?;
            let explanation = match params.subject {
                VegetationExplainSubjectDto::Plant { plant } => {
                    let plant = PlantId::from_str(&plant.0).map_err(Error::from)?;
                    ctx.vegetation_jobs.explain_plant(job, cell, plant)?
                }
                VegetationExplainSubjectDto::Rejected { candidate } => ctx
                    .vegetation_jobs
                    .explain_rejection(job, cell, candidate_identity(&candidate)?)?,
            };
            Ok(provenance_explanation(explanation))
        },
    );

    reg.register::<VegetationCookParams, VegetationCookJobDto>(
        "vegetation-cook",
        "start one deterministic content-addressed vegetation cook",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = crate::commands_asset::resolve_asset(ctx, &params.map)?;
            let map_asset = load_vegetation_map_snapshot(ctx.assets, map).map_err(Error::from)?;
            let world = vegetation_world_identity(ctx, map)?;
            let scope = params.scope.clone();
            let cells = cook_scope_cells(&params.scope, &map_asset)?;
            let providers = capture_surface_snapshots(ctx)?;
            let workers = params.workers.unwrap_or_else(default_cook_workers);
            if workers == 0 {
                return Err(Error::command(
                    "vegetation cook workers must be greater than zero",
                ));
            }
            let store = ctx.assets.vegetation_artifact_store();
            let request = VegetationCookRequest {
                world,
                map,
                expected_manifest: store.current_manifest_hash(map).map_err(Error::from)?,
                cells,
                ecology_tick: 0,
                workers,
                platform: portable_vegetation_platform_profile(params.platform_profile.as_deref()),
                surface_providers: providers,
            };
            ctx.vegetation_cook_jobs
                .enqueue(CookProjectView::capture(ctx.assets), request, scope)
        },
    );

    reg.register::<VegetationCookJobParams, VegetationCookStatusDto>(
        "vegetation-cook-status",
        "poll one asynchronous vegetation cook",
        |ctx, params| {
            ctx.vegetation_cook_jobs
                .status(parse_u64(&params.job, "job")?)
        },
    );

    reg.register::<VegetationCookJobParams, VegetationCookStatusDto>(
        "vegetation-cancel-cook",
        "cancel one vegetation cook without publishing partial output",
        |ctx, params| {
            ctx.vegetation_cook_jobs
                .cancel(parse_u64(&params.job, "job")?)
        },
    );

    reg.register::<VegetationManifestParams, VegetationManifestResult>(
        "vegetation-manifest",
        "inspect one current or content-addressed vegetation manifest",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = crate::commands_asset::resolve_asset(ctx, &params.map)?;
            let store = ctx.assets.vegetation_artifact_store();
            let identity = match params.identity {
                Some(identity) => parse_content_hash(&identity, "identity")?,
                None => store
                    .current_manifest_hash(map)
                    .map_err(Error::from)?
                    .ok_or_else(|| Error::command("vegetation map has no cooked generation"))?,
            };
            let manifest = read_manifest(&store, identity, map)?;
            let latest_cook = ctx
                .vegetation_cook_jobs
                .latest_statistics(map, identity)
                .as_ref()
                .map(crate::vegetation_cook_dto::statistics_dto);
            Ok(VegetationManifestResult {
                manifest: crate::vegetation_cook_dto::manifest_dto(&manifest)?,
                latest_cook,
            })
        },
    );

    reg.register::<VegetationCellInspectParams, VegetationCellInspectResult>(
        "vegetation-cell-inspect",
        "inspect one validated immutable vegetation cell header and section table",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = crate::commands_asset::resolve_asset(ctx, &params.map)?;
            let cell = parse_cell(&params.cell)?;
            let store = ctx.assets.vegetation_artifact_store();
            let manifest_identity = match params.manifest {
                Some(identity) => parse_content_hash(&identity, "manifest")?,
                None => store
                    .current_manifest_hash(map)
                    .map_err(Error::from)?
                    .ok_or_else(|| Error::command("vegetation map has no cooked generation"))?,
            };
            let manifest = read_manifest(&store, manifest_identity, map)?;
            let row = manifest
                .cells
                .iter()
                .find(|candidate| candidate.cell == cell)
                .ok_or_else(|| Error::command("cell is absent from the selected manifest"))?;
            let reader = store.open_cell(row.artifact_hash).map_err(Error::from)?;
            let index = reader.index();
            Ok(VegetationCellInspectResult {
                cell: VegetationCellSummaryDto {
                    map: WireUuid(map.value()),
                    manifest: manifest_identity.to_string(),
                    cell: crate::vegetation_cook_dto::world_cell_dto(cell),
                    content_hash: row.artifact_hash.to_string(),
                    cook_key: index.cook_key.to_string(),
                    platform_profile: index.platform_profile.to_string(),
                    payload_hash: index.payload_hash.to_string(),
                    bounds: crate::vegetation_cook_dto::world_bounds_dto(row.bounds),
                    macro_points: row.macro_count.to_string(),
                    micro_samples: row.micro_count.to_string(),
                    sections: index
                        .sections
                        .iter()
                        .map(crate::vegetation_cook_dto::cell_section_dto)
                        .collect(),
                },
            })
        },
    );

    reg.register::<saffron_protocol::VegetationRejectionsParams, saffron_protocol::VegetationRejectionsResult>(
        "vegetation-rejections",
        "one cooked cell's rejected candidates: position, reason, and ordinal (capped rows)",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = crate::commands_asset::resolve_asset(ctx, &params.map)?;
            let cell = parse_cell(&params.cell)?;
            let store = ctx.assets.vegetation_artifact_store();
            let manifest_identity = match &params.manifest {
                Some(identity) => parse_content_hash(identity, "manifest")?,
                None => store
                    .current_manifest_hash(map)
                    .map_err(Error::from)?
                    .ok_or_else(|| Error::command("vegetation map has no cooked generation"))?,
            };
            let manifest = read_manifest(&store, manifest_identity, map)?;
            let row = manifest
                .cells
                .iter()
                .find(|candidate| candidate.cell == cell)
                .ok_or_else(|| Error::command("cell is absent from the selected manifest"))?;
            let bytes = store
                .read_cell_section(
                    row.artifact_hash,
                    saffron_vegetation::VegetationCellSectionKind::RejectionDiagnostics,
                )
                .map_err(Error::from)?
                .ok_or_else(|| {
                    Error::command("vegetation cell has no rejection diagnostics section")
                })?;
            let facet =
                saffron_vegetation::decode_vegetation_rejection_diagnostics(bytes.as_ref())
                    .map_err(Error::from)?;
            let limit = usize::try_from(params.limit.unwrap_or(1024))
                .map_err(|_| Error::command("limit is not representable"))?;
            Ok(saffron_protocol::VegetationRejectionsResult {
                candidates: facet.candidate_count.to_string(),
                accepted: facet.accepted_count.to_string(),
                total_rejected: facet.rejected.len().to_string(),
                rows: facet
                    .rejected
                    .iter()
                    .take(limit)
                    .map(|rejected| saffron_protocol::VegetationRejectionDto {
                        reason: crate::commands_asset::rejection_reason_dto(rejected.reason),
                        position_ticks: rejected
                            .position
                            .global_ticks()
                            .map(|ticks| ticks.to_string()),
                        ordinal: rejected.candidate.ordinal.to_string(),
                    })
                    .collect(),
            })
        },
    );

    reg.register::<saffron_protocol::VegetationTopologyDiffParams, saffron_protocol::VegetationTopologyDiffResult>(
        "vegetation-topology-diff",
        "diff two cooked manifests per cell: added/removed/moved plants + override conflicts",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = crate::commands_asset::resolve_asset(ctx, &params.map)?;
            let cell_filter = params
                .cells
                .as_ref()
                .map(|cells| {
                    cells
                        .iter()
                        .map(parse_cell)
                        .collect::<Result<std::collections::BTreeSet<_>>>()
                })
                .transpose()?;
            let store = ctx.assets.vegetation_artifact_store();
            let from_identity = parse_content_hash(&params.from, "from")?;
            let to_identity = match &params.to {
                Some(identity) => parse_content_hash(identity, "to")?,
                None => store
                    .current_manifest_hash(map)
                    .map_err(Error::from)?
                    .ok_or_else(|| Error::command("vegetation map has no cooked generation"))?,
            };
            let from = read_manifest(&store, from_identity, map)?;
            let to = read_manifest(&store, to_identity, map)?;
            let from_cells: std::collections::BTreeMap<_, _> = from
                .cells
                .iter()
                .map(|row| (row.cell, row.artifact_hash))
                .collect();
            let to_cells: std::collections::BTreeMap<_, _> = to
                .cells
                .iter()
                .map(|row| (row.cell, row.artifact_hash))
                .collect();
            let union: std::collections::BTreeSet<_> = from_cells
                .keys()
                .chain(to_cells.keys())
                .copied()
                .filter(|cell| {
                    cell_filter
                        .as_ref()
                        .is_none_or(|filter| filter.contains(cell))
                })
                .collect();
            let macro_map = |hash: Option<&saffron_vegetation::ContentHash>| -> Result<
                std::collections::BTreeMap<saffron_vegetation::PlantId, [i128; 3]>,
            > {
                let Some(hash) = hash else {
                    return Ok(Default::default());
                };
                let Some(bytes) = store
                    .read_cell_section(
                        *hash,
                        saffron_vegetation::VegetationCellSectionKind::MacroPoints,
                    )
                    .map_err(Error::from)?
                else {
                    return Ok(Default::default());
                };
                let columns = saffron_vegetation::PlantPointColumns::from_canonical_bytes(&bytes)
                    .map_err(Error::from)?;
                Ok(columns
                    .ids
                    .iter()
                    .zip(&columns.positions)
                    .map(|(id, position)| (*id, position.global_ticks()))
                    .collect())
            };
            const ID_CAP: usize = 64;
            let mut cells = Vec::new();
            let mut changed_cells = Vec::new();
            for cell in union {
                let from_hash = from_cells.get(&cell);
                let to_hash = to_cells.get(&cell);
                if from_hash == to_hash {
                    continue;
                }
                let from_points = macro_map(from_hash)?;
                let to_points = macro_map(to_hash)?;
                let added: Vec<_> = to_points
                    .keys()
                    .filter(|id| !from_points.contains_key(id))
                    .copied()
                    .collect();
                let removed: Vec<_> = from_points
                    .keys()
                    .filter(|id| !to_points.contains_key(id))
                    .copied()
                    .collect();
                let moved: Vec<_> = to_points
                    .iter()
                    .filter(|(id, position)| {
                        from_points
                            .get(*id)
                            .is_some_and(|previous| previous != *position)
                    })
                    .map(|(id, _)| *id)
                    .collect();
                changed_cells.push((cell, to_points));
                cells.push((cell, added, removed, moved));
            }
            drop(store);
            // Unresolved authored overrides: rows in the changed cells' AnchorOverride
            // chunks whose plant is absent from the newer manifest's macro set.
            let root = saffron_assets::load_vegetation_map_root(ctx.assets, map)
                .map_err(|error| Error::command(error.to_string()))?;
            let mut result_cells = Vec::new();
            for ((cell, added, removed, moved), (_, to_points)) in
                cells.into_iter().zip(changed_cells)
            {
                let keys: Vec<_> = root
                    .inventory
                    .iter()
                    .filter(|reference| {
                        reference.key.tile
                            == saffron_vegetation::VegetationMapTileKey::Cell(cell)
                            && reference.key.kind
                                == saffron_vegetation::VegetationMapChunkKind::AnchorOverride
                    })
                    .map(|reference| reference.key)
                    .collect();
                let chunks = saffron_assets::load_vegetation_map_chunks(ctx.assets, map, &keys)
                    .map_err(|error| Error::command(error.to_string()))?;
                let mut conflicts = Vec::new();
                for chunk in &chunks {
                    let saffron_vegetation::VegetationMapChunkPayload::AnchorOverride(payload) =
                        &chunk.payload
                    else {
                        continue;
                    };
                    let layer = vegetation_guid(chunk.key.layer);
                    let mut push = |kind: &str, plant: saffron_vegetation::PlantId| {
                        if !to_points.contains_key(&plant) {
                            conflicts.push(saffron_protocol::VegetationOverrideConflictDto {
                                kind: kind.to_owned(),
                                plant: saffron_protocol::PlantId(plant.to_string()),
                                layer: layer.clone(),
                            });
                        }
                    };
                    for anchor in &payload.explicit_plants {
                        push("anchor", anchor.id);
                    }
                    for pin in &payload.pins {
                        push("pin", *pin);
                    }
                    for row in &payload.transform_overrides {
                        push("transform-override", row.plant);
                    }
                    for row in &payload.state_overrides {
                        push("state-override", row.plant);
                    }
                }
                result_cells.push(saffron_protocol::VegetationTopologyCellDiffDto {
                    cell: crate::vegetation_cook_dto::world_cell_dto(cell),
                    added: added.len().to_string(),
                    removed: removed.len().to_string(),
                    moved: moved.len().to_string(),
                    added_ids: added
                        .iter()
                        .take(ID_CAP)
                        .map(|id| saffron_protocol::PlantId(id.to_string()))
                        .collect(),
                    removed_ids: removed
                        .iter()
                        .take(ID_CAP)
                        .map(|id| saffron_protocol::PlantId(id.to_string()))
                        .collect(),
                    moved_ids: moved
                        .iter()
                        .take(ID_CAP)
                        .map(|id| saffron_protocol::PlantId(id.to_string()))
                        .collect(),
                    conflicts,
                });
            }
            Ok(saffron_protocol::VegetationTopologyDiffResult {
                from: from_identity.to_string(),
                to: to_identity.to_string(),
                cells: result_cells,
            })
        },
    );
}

fn vegetation_world_identity(ctx: &mut EngineContext<'_>, map: Uuid) -> Result<Uuid> {
    let mut matches = Vec::new();
    ctx.scene_edit
        .active_scene()
        .for_each::<(&VegetationField, &IdComponent), _>(|_, (field, id)| {
            if field.enabled && field.map == map {
                matches.push(id.id);
            }
        });
    match matches.as_slice() {
        [world] if world.value() != 0 => Ok(*world),
        [] => Err(Error::command(
            "active scene has no enabled VegetationField for this map",
        )),
        _ => Err(Error::command(
            "active scene vegetation world identity is invalid",
        )),
    }
}

fn cook_scope_cells(
    scope: &VegetationCookScopeDto,
    map: &saffron_vegetation::VegetationMapSnapshot,
) -> Result<Vec<WorldCellKey>> {
    let max_cells = GraphSafetyLimits::default().max_output_cells;
    match scope {
        VegetationCookScopeDto::All => {
            world_cells_covering_bounds(map.bounds, map.root.chunk_layout.level, max_cells)
                .map_err(|error| Error::command(error.to_string()))
        }
        VegetationCookScopeDto::Bounds { bounds, level } => {
            let bounds = intersect_bounds(parse_bounds(bounds)?, map.bounds)
                .ok_or_else(|| Error::command("cook bounds do not intersect the map"))?;
            world_cells_covering_bounds(bounds, *level, max_cells)
                .map_err(|error| Error::command(error.to_string()))
        }
        VegetationCookScopeDto::Cells { cells } => {
            if u64::try_from(cells.len()).unwrap_or(u64::MAX) > max_cells {
                return Err(Error::command(
                    "vegetation cook cell scope exceeds the hard cap",
                ));
            }
            let mut parsed = cells.iter().map(parse_cell).collect::<Result<Vec<_>>>()?;
            parsed.sort_unstable();
            if parsed.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(Error::command(
                    "vegetation cook cell scope contains duplicates",
                ));
            }
            if parsed
                .iter()
                .any(|cell| intersect_bounds(cell.bounds(), map.bounds).is_none())
            {
                return Err(Error::command("vegetation cook cell lies outside the map"));
            }
            Ok(parsed)
        }
    }
}

fn default_cook_workers() -> u16 {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .try_into()
        .unwrap_or(u16::MAX)
}

fn parse_content_hash(value: &str, field: &str) -> Result<ContentHash> {
    value
        .parse()
        .map_err(|error: saffron_vegetation::Error| Error::command(format!("{field}: {error}")))
}

fn read_manifest(
    store: &saffron_assets::VegetationArtifactStore,
    identity: ContentHash,
    map: Uuid,
) -> Result<VegetationBaseManifest> {
    let bytes = store.read_manifest(identity).map_err(Error::from)?;
    let manifest = VegetationBaseManifest::from_canonical_bytes(&bytes).map_err(Error::from)?;
    if manifest.map != map || manifest.identity().map_err(Error::from)? != identity {
        return Err(Error::command(
            "vegetation manifest does not belong to the selected map",
        ));
    }
    Ok(manifest)
}

fn require_project_loaded(ctx: &EngineContext<'_>) -> Result<()> {
    if ctx.scene_edit.project_ready() {
        Ok(())
    } else {
        Err(Error::command("no project loaded"))
    }
}

fn compile_target(
    ctx: &mut EngineContext<'_>,
    target: VegetationCompileTargetDto,
) -> Result<ResolvedBiomeGraph> {
    match target {
        VegetationCompileTargetDto::Asset { biome } => compile_catalog_biome_graph(
            ctx.assets,
            Uuid::from(biome),
            &[],
            &BTreeMap::new(),
            GraphCompileOptions::canonical(),
        )
        .map_err(Error::from),
        VegetationCompileTargetDto::Instance {
            map,
            biome_instance,
        } => {
            let map = Uuid::from(map);
            let biome_instance = parse_guid(&biome_instance)?;
            let providers = capture_surface_snapshots(ctx)?;
            let dependencies = vegetation_graph_dependency_hashes(ctx.assets, map, &providers)
                .map_err(Error::from)?;
            compile_catalog_biome_instance_graph(
                ctx.assets,
                map,
                biome_instance,
                &dependencies,
                GraphCompileOptions::canonical(),
            )
            .map_err(Error::from)
        }
    }
}

fn capture_surface_snapshots(ctx: &mut EngineContext<'_>) -> Result<Vec<Arc<dyn SurfaceField>>> {
    let assets = &mut *ctx.assets;
    let mut snapshots = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        snapshots = Some(scene_surface_field_snapshots(
            gpu,
            ctx.scene_edit.active_scene(),
            assets,
        ));
    });
    snapshots
        .ok_or_else(|| Error::command("renderer did not provide a vegetation surface snapshot"))?
        .map_err(Error::from)
}

fn compile_result(resolved: &ResolvedBiomeGraph) -> VegetationCompileBiomeResult {
    let graph = &resolved.graph;
    let estimate = graph.root.estimate;
    let limits = graph.limits;
    VegetationCompileBiomeResult {
        biome: WireUuid::from(graph.biome),
        biome_instance: resolved.biome_instance.map(vegetation_guid),
        graph_identity: hex_hash(graph.identity),
        required_halo_bits: graph.required_halo(0).bits(),
        estimate: VegetationGraphEstimateDto {
            candidates: estimate.candidates.to_string(),
            accepted: estimate.accepted.to_string(),
            micro_samples: estimate.micro_samples.to_string(),
            memory_bytes: estimate.memory_bytes.to_string(),
            transfer_bytes: estimate.transfer_bytes.to_string(),
        },
        limits: graph_limits_dto(limits),
        dependencies: graph
            .dependencies()
            .iter()
            .map(|dependency| {
                let (kind, identity) = dependency_identity(dependency.source);
                VegetationGraphDependencyDto {
                    kind: kind.to_owned(),
                    identity,
                    content_hash: hex_hash(dependency.content_hash),
                }
            })
            .collect(),
    }
}

pub(crate) fn graph_limits_dto(limits: GraphSafetyLimits) -> VegetationGraphLimitsDto {
    VegetationGraphLimitsDto {
        workers: limits.max_workers,
        output_cells: limits.max_output_cells.to_string(),
        global_stage_tiles: limits.max_global_stage_tiles.to_string(),
        input_tiles: limits.max_input_tiles.to_string(),
        candidates: limits.max_candidates.to_string(),
        macro_points: limits.max_macro_points.to_string(),
        micro_samples: limits.max_micro_samples.to_string(),
        memory_bytes: limits.max_memory_bytes.to_string(),
        transfer_bytes: limits.max_transfer_bytes.to_string(),
        module_depth: limits.max_module_depth,
        time_ms: limits.max_time_ms.to_string(),
    }
}

fn node_schema(operator: GraphOperator) -> VegetationNodeSchemaDto {
    VegetationNodeSchemaDto {
        operator: graph_operator_dto(operator),
        inputs: operator
            .input_pins()
            .into_iter()
            .map(|pin| VegetationGraphPinDto {
                name: pin.name,
                domain: pin.domain.as_wire().to_owned(),
                required: pin.required,
            })
            .collect(),
        outputs: operator
            .output_pins()
            .into_iter()
            .map(|pin| VegetationGraphPinDto {
                name: pin.name,
                domain: pin.domain.as_wire().to_owned(),
                required: pin.required,
            })
            .collect(),
        parameters: operator
            .parameter_schema()
            .into_iter()
            .map(|parameter| VegetationGraphParameterDto {
                name: parameter.name.to_owned(),
                kind: parameter.parameter_type.as_wire().to_owned(),
                required: parameter.required,
            })
            .collect(),
        seed_namespaces: operator
            .seed_namespace_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        slang_compute: operator.has_slang_executor(),
    }
}

fn dependency_identity(source: GraphDependencySource) -> (&'static str, String) {
    match source {
        GraphDependencySource::Asset(asset) => ("asset", asset.value().to_string()),
        GraphDependencySource::Field(channel) => ("field", field_channel_wire(channel)),
        GraphDependencySource::SurfaceProvider(provider) => {
            ("surface-provider", provider.to_string())
        }
        GraphDependencySource::MapLayer(layer) => ("map-layer", format!("{layer:032x}")),
    }
}

fn field_channel_wire(channel: FieldChannel) -> String {
    match channel {
        FieldChannel::Altitude => "altitude".to_owned(),
        FieldChannel::Slope => "slope".to_owned(),
        FieldChannel::Curvature => "curvature".to_owned(),
        FieldChannel::Concavity => "concavity".to_owned(),
        FieldChannel::Drainage => "drainage".to_owned(),
        FieldChannel::Moisture => "moisture".to_owned(),
        FieldChannel::Temperature => "temperature".to_owned(),
        FieldChannel::Precipitation => "precipitation".to_owned(),
        FieldChannel::Sunlight => "sunlight".to_owned(),
        FieldChannel::Exposure => "exposure".to_owned(),
        FieldChannel::WaterDistance => "water-distance".to_owned(),
        FieldChannel::WaterDepth => "water-depth".to_owned(),
        FieldChannel::SignedBlocker => "signed-blocker".to_owned(),
        FieldChannel::SplineDistance => "spline-distance".to_owned(),
        FieldChannel::User(user) => format!("user:{user}"),
    }
}

fn provenance_explanation(explanation: ProvenanceExplanation) -> ProvenanceExplanationDto {
    let record = explanation.record;
    ProvenanceExplanationDto {
        handle: explanation.provenance.0,
        record: ProvenanceDto {
            map: WireUuid::from(record.map),
            layer: vegetation_guid(record.layer),
            biome: WireUuid::from(record.biome),
            decision: record.decision.0,
            candidate: record.candidate.to_string(),
            family: record.family.map(WireUuid::from),
            plant: record
                .plant
                .map(|plant| saffron_protocol::PlantId(plant.to_string())),
            variation: record.variation,
        },
        decisions: explanation
            .decisions
            .into_iter()
            .map(|(handle, decision)| ProvenanceDecisionDto {
                handle: handle.0,
                parents: decision
                    .parents
                    .into_iter()
                    .map(|parent| parent.0)
                    .collect(),
                subgraph_path: decision
                    .subgraph_path
                    .into_iter()
                    .map(vegetation_guid)
                    .collect(),
                node: vegetation_guid(decision.node),
                operator: decision.operator.as_wire().to_owned(),
                candidate: decision.candidate.to_string(),
                outcome: match decision.outcome {
                    ProvenanceDecisionOutcome::Produced => ProvenanceDecisionOutcomeDto::Produced,
                    ProvenanceDecisionOutcome::Retained => ProvenanceDecisionOutcomeDto::Retained,
                    ProvenanceDecisionOutcome::Accepted => ProvenanceDecisionOutcomeDto::Accepted,
                    ProvenanceDecisionOutcome::Rejected => ProvenanceDecisionOutcomeDto::Rejected,
                },
            })
            .collect(),
        rejection_reason: explanation
            .rejection_reason
            .map(candidate_rejection_reason_dto),
    }
}

pub(crate) fn candidate_rejection_reason_dto(
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

fn candidate_identity(value: &VegetationCandidateIdentityDto) -> Result<CandidateIdentity> {
    Ok(CandidateIdentity {
        node: parse_guid(&value.node)?,
        node_address: parse_guid(&value.node_address)?,
        node_semantic_revision: value.node_semantic_revision,
        ordinal: parse_u64(&value.ordinal, "subject.candidate.ordinal")?,
        ancestor: parse_u64(&value.ancestor, "subject.candidate.ancestor")?,
    })
}

fn parse_bounds(value: &WorldBoundsDto) -> Result<WorldBounds> {
    let minimum = parse_i128_lanes(&value.min_ticks, "bounds.minTicks")?;
    let maximum = parse_i128_lanes(&value.max_ticks_exclusive, "bounds.maxTicksExclusive")?;
    WorldBounds::new(minimum, maximum).map_err(|error| Error::command(error.to_string()))
}

fn parse_cell(value: &WorldCellDto) -> Result<WorldCellKey> {
    let x = parse_i64(&value.coordinates[0], "cell.coordinates[0]")?;
    let y = parse_i64(&value.coordinates[1], "cell.coordinates[1]")?;
    let z = parse_i64(&value.coordinates[2], "cell.coordinates[2]")?;
    WorldCellKey::new(x, y, z, value.level).map_err(|error| Error::command(error.to_string()))
}

fn parse_i128_lanes(values: &[String; 3], field: &str) -> Result<[i128; 3]> {
    Ok([
        parse_i128(&values[0], field)?,
        parse_i128(&values[1], field)?,
        parse_i128(&values[2], field)?,
    ])
}

fn parse_guid(value: &VegetationGuid) -> Result<u128> {
    u128::from_str_radix(&value.0, 16)
        .map_err(|_| Error::command("vegetation GUID is not canonical"))
}

fn parse_optional_u64(value: Option<&str>, field: &str) -> Result<Option<u64>> {
    value.map(|value| parse_u64(value, field)).transpose()
}

pub(crate) fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical u64 decimal string")))
}

fn parse_i64(value: &str, field: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i64 decimal string")))
}

fn parse_i128(value: &str, field: &str) -> Result<i128> {
    value
        .parse()
        .map_err(|_| Error::command(format!("{field} must be a canonical i128 decimal string")))
}

fn intersect_bounds(left: WorldBounds, right: WorldBounds) -> Option<WorldBounds> {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    WorldBounds::new(
        [
            left_minimum[0].max(right_minimum[0]),
            left_minimum[1].max(right_minimum[1]),
            left_minimum[2].max(right_minimum[2]),
        ],
        [
            left_maximum[0].min(right_maximum[0]),
            left_maximum[1].min(right_maximum[1]),
            left_maximum[2].min(right_maximum[2]),
        ],
    )
    .ok()
}

pub(crate) fn vegetation_guid(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

fn hex_hash(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in hash {
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

pub(crate) fn graph_operator_dto(operator: GraphOperator) -> VegetationGraphOperatorDto {
    use GraphOperator as O;
    use VegetationGraphOperatorDto as D;
    match operator {
        O::InterfaceInput => D::InterfaceInput,
        O::RegionInput => D::RegionInput,
        O::SplineInput => D::SplineInput,
        O::SpeciesInput => D::SpeciesInput,
        O::CommunityInput => D::CommunityInput,
        O::ExplicitAnchors => D::ExplicitAnchors,
        O::StratifiedCoverage => D::StratifiedCoverage,
        O::BlueNoisePoisson => D::BlueNoisePoisson,
        O::SurfaceProjection => D::SurfaceProjection,
        O::FieldSample => D::FieldSample,
        O::PaintedTile => D::PaintedTile,
        O::Noise => D::Noise,
        O::Gradient => D::Gradient,
        O::Curve => D::Curve,
        O::Remap => D::Remap,
        O::Combine => D::Combine,
        O::Clamp => D::Clamp,
        O::DistanceField => D::DistanceField,
        O::WeightedElimination => D::WeightedElimination,
        O::VariableSpacing => D::VariableSpacing,
        O::FieldImportance => D::FieldImportance,
        O::ClusterPatchColony => D::ClusterPatchColony,
        O::SplineFollow => D::SplineFollow,
        O::RecursiveCompanion => D::RecursiveCompanion,
        O::Transform => D::Transform,
        O::PriorityExclusion => D::PriorityExclusion,
        O::BoundsOverlap => D::BoundsOverlap,
        O::Competition => D::Competition,
        O::Suitability => D::Suitability,
        O::CommunityBlend => D::CommunityBlend,
        O::SuccessionInput => D::SuccessionInput,
        O::MacroOutput => D::MacroOutput,
        O::MicroOutput => D::MicroOutput,
        O::DiagnosticOutput => D::DiagnosticOutput,
        O::ModuleCall => D::ModuleCall,
    }
}

fn graph_operator_from_dto(operator: VegetationGraphOperatorDto) -> GraphOperator {
    use GraphOperator as O;
    use VegetationGraphOperatorDto as D;
    match operator {
        D::InterfaceInput => O::InterfaceInput,
        D::RegionInput => O::RegionInput,
        D::SplineInput => O::SplineInput,
        D::SpeciesInput => O::SpeciesInput,
        D::CommunityInput => O::CommunityInput,
        D::ExplicitAnchors => O::ExplicitAnchors,
        D::StratifiedCoverage => O::StratifiedCoverage,
        D::BlueNoisePoisson => O::BlueNoisePoisson,
        D::SurfaceProjection => O::SurfaceProjection,
        D::FieldSample => O::FieldSample,
        D::PaintedTile => O::PaintedTile,
        D::Noise => O::Noise,
        D::Gradient => O::Gradient,
        D::Curve => O::Curve,
        D::Remap => O::Remap,
        D::Combine => O::Combine,
        D::Clamp => O::Clamp,
        D::DistanceField => O::DistanceField,
        D::WeightedElimination => O::WeightedElimination,
        D::VariableSpacing => O::VariableSpacing,
        D::FieldImportance => O::FieldImportance,
        D::ClusterPatchColony => O::ClusterPatchColony,
        D::SplineFollow => O::SplineFollow,
        D::RecursiveCompanion => O::RecursiveCompanion,
        D::Transform => O::Transform,
        D::PriorityExclusion => O::PriorityExclusion,
        D::BoundsOverlap => O::BoundsOverlap,
        D::Competition => O::Competition,
        D::Suitability => O::Suitability,
        D::CommunityBlend => O::CommunityBlend,
        D::SuccessionInput => O::SuccessionInput,
        D::MacroOutput => O::MacroOutput,
        D::MicroOutput => O::MicroOutput,
        D::DiagnosticOutput => O::DiagnosticOutput,
        D::ModuleCall => O::ModuleCall,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::registry::{CommandRegistry, register_builtin_commands};
    use crate::test_support::{StubRenderer, with_stub};

    fn registry() -> CommandRegistry {
        let mut registry = CommandRegistry::new();
        register_builtin_commands(&mut registry);
        registry
    }

    #[test]
    fn node_schema_exposes_the_complete_typed_operator_vocabulary() {
        let registry = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |context| {
            let reply = registry.dispatch(
                context,
                &json!({ "cmd": "vegetation-node-schema", "params": {} }),
            );
            assert_eq!(reply["ok"], json!(true));
            let nodes = reply["result"]["nodes"].as_array().unwrap();
            assert_eq!(nodes.len(), GraphOperator::ALL.len());
            let noise = nodes
                .iter()
                .find(|node| node["operator"] == json!("noise"))
                .unwrap();
            assert_eq!(noise["slangCompute"], json!(true));
            assert_eq!(noise["inputs"][0]["domain"], json!("candidates"));
            assert_eq!(noise["outputs"][0]["domain"], json!("scalar-field"));
            assert_eq!(noise["seedNamespaces"], json!(["noise"]));
        });
    }

    #[test]
    fn compile_command_requires_the_canonical_loaded_project_catalog() {
        let registry = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |context| {
            let reply = registry.dispatch(
                context,
                &json!({
                    "cmd": "vegetation-compile-biome",
                    "params": { "target": { "scope": "asset", "biome": "1" } }
                }),
            );
            assert_eq!(reply["ok"], json!(false));
            assert_eq!(reply["error"]["message"], json!("no project loaded"));
        });
    }

    #[test]
    fn candidate_rejection_reasons_map_exhaustively_to_protocol() {
        let cases = [
            (
                CandidateRejectionReason::SurfaceMiss,
                VegetationCandidateRejectionReasonDto::SurfaceMiss,
            ),
            (
                CandidateRejectionReason::Threshold,
                VegetationCandidateRejectionReasonDto::Threshold,
            ),
            (
                CandidateRejectionReason::WeightedElimination,
                VegetationCandidateRejectionReasonDto::WeightedElimination,
            ),
            (
                CandidateRejectionReason::PriorityExclusion,
                VegetationCandidateRejectionReasonDto::PriorityExclusion,
            ),
            (
                CandidateRejectionReason::Competition,
                VegetationCandidateRejectionReasonDto::Competition,
            ),
            (
                CandidateRejectionReason::ForeignOwner,
                VegetationCandidateRejectionReasonDto::ForeignOwner,
            ),
            (
                CandidateRejectionReason::NoSpecies,
                VegetationCandidateRejectionReasonDto::NoSpecies,
            ),
        ];

        for (reason, expected) in cases {
            assert_eq!(candidate_rejection_reason_dto(reason), expected);
        }
    }
}
