//! Biome-graph compilation, schema inspection, bounded evaluation, and provenance commands.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use saffron_assets::{
    ResolvedBiomeGraph, assemble_biome_graph_evaluation_job, compile_catalog_biome_graph,
    compile_catalog_biome_instance_graph, load_vegetation_map_asset, scene_surface_field_snapshots,
    vegetation_graph_dependency_hashes,
};
use saffron_core::Uuid;
use saffron_protocol::{
    ProvenanceDecisionDto, ProvenanceDecisionOutcomeDto, ProvenanceDto, ProvenanceExplanationDto,
    Uuid as WireUuid, VegetationCandidateIdentityDto, VegetationCandidateRejectionReasonDto,
    VegetationCompileBiomeParams, VegetationCompileBiomeResult, VegetationCompileTargetDto,
    VegetationEvaluateRegionParams, VegetationEvaluationJobDto, VegetationEvaluationJobParams,
    VegetationEvaluationStatusDto, VegetationExplainPointParams, VegetationExplainSubjectDto,
    VegetationGraphDependencyDto, VegetationGraphEstimateDto, VegetationGraphLimitsDto,
    VegetationGraphOperatorDto, VegetationGraphParameterDto, VegetationGraphPinDto, VegetationGuid,
    VegetationNodeSchemaDto, VegetationNodeSchemaParams, VegetationNodeSchemaResult,
    WorldBoundsDto, WorldCellDto,
};
use saffron_spatial::{
    FieldChannel, SurfaceField, WorldBounds, WorldCellKey, world_cells_covering_bounds,
};
use saffron_vegetation::{
    BiomeGraphEvaluator, CandidateIdentity, CandidateRejectionReason, GraphCompileOptions,
    GraphDependencySource, GraphOperator, PlantId, ProvenanceDecisionOutcome,
    ProvenanceExplanation,
};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};

/// Registers the one production biome-graph control surface.
pub fn register_vegetation_commands(reg: &mut CommandRegistry) {
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

    reg.register::<VegetationEvaluateRegionParams, VegetationEvaluationJobDto>(
        "vegetation-evaluate-region",
        "start one bounded asynchronous biome evaluation through the canonical evaluator",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let map = Uuid::from(params.map);
            let biome_instance = parse_guid(&params.biome_instance)?;
            let requested_bounds = parse_bounds(&params.bounds)?;
            let ecology_tick =
                parse_optional_u64(params.ecology_tick.as_deref(), "ecologyTick")?.unwrap_or(0);
            let providers = capture_surface_snapshots(ctx)?;
            let dependencies = vegetation_graph_dependency_hashes(ctx.assets, map, &providers)
                .map_err(|error| Error::command(error.to_string()))?;
            let resolved = compile_catalog_biome_instance_graph(
                ctx.assets,
                map,
                biome_instance,
                &dependencies,
                GraphCompileOptions::canonical(),
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let map_asset = load_vegetation_map_asset(ctx.assets, map)
                .map_err(|error| Error::command(error.to_string()))?;
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
            .map_err(|error| Error::command(error.to_string()))?;
            let workers = match params.workers {
                Some(workers) => usize::from(workers),
                None => std::thread::available_parallelism().map_or(1, usize::from),
            };
            let mut evaluator = BiomeGraphEvaluator::new(Arc::new(resolved.graph), workers)
                .map_err(|error| Error::command(error.to_string()))?;
            if let Some(compute) = ctx.vegetation_compute_executor()? {
                evaluator = evaluator.with_compute_executor(compute);
            }
            ctx.vegetation_jobs.start(evaluator, inputs)
        },
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
                    let plant = PlantId::from_str(&plant.0)
                        .map_err(|error| Error::command(error.to_string()))?;
                    ctx.vegetation_jobs.explain_plant(job, cell, plant)?
                }
                VegetationExplainSubjectDto::Rejected { candidate } => ctx
                    .vegetation_jobs
                    .explain_rejection(job, cell, candidate_identity(&candidate)?)?,
            };
            Ok(provenance_explanation(explanation))
        },
    );
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
        .map_err(|error| Error::command(error.to_string())),
        VegetationCompileTargetDto::Instance {
            map,
            biome_instance,
        } => {
            let map = Uuid::from(map);
            let biome_instance = parse_guid(&biome_instance)?;
            let providers = capture_surface_snapshots(ctx)?;
            let dependencies = vegetation_graph_dependency_hashes(ctx.assets, map, &providers)
                .map_err(|error| Error::command(error.to_string()))?;
            compile_catalog_biome_instance_graph(
                ctx.assets,
                map,
                biome_instance,
                &dependencies,
                GraphCompileOptions::canonical(),
            )
            .map_err(|error| Error::command(error.to_string()))
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
        .map_err(|error| Error::command(error.to_string()))
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
        limits: VegetationGraphLimitsDto {
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
        },
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

fn parse_u64(value: &str, field: &str) -> Result<u64> {
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
            assert_eq!(reply["error"], json!("no project loaded"));
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
