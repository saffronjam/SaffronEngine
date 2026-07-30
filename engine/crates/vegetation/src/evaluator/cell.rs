//! The public evaluator facade and single-cell evaluation.

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use saffron_spatial::FieldDerivative;

use crate::graph::CompiledDemandSlice;
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphUnit, Error, GraphAuthority,
    GraphComputeExecutor, GraphDependencySource, GraphExecutionPlan, GraphGpuScheduling,
    GraphOperator, PlantPointColumns, ProvenanceTable, Result, build_execution_plan,
};

/// The single evaluator surface shared by preview, cooking, and runtime generation.
#[derive(Clone)]
pub struct BiomeGraphEvaluator {
    pub(super) graph: Arc<CompiledBiomeGraph>,
    pub(super) worker_count: usize,
    pub(super) compute: Option<Arc<dyn GraphComputeExecutor>>,
}

pub(super) struct EvaluationPlans {
    pub(super) execution: GraphExecutionPlan,
    pub(super) preparation: Option<GraphExecutionPlan>,
    pub(super) allocation_bytes: u64,
}

pub(super) fn build_evaluation_plans(
    graph: &CompiledBiomeGraph,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
    guard: PreflightGuard<'_>,
) -> Result<EvaluationPlans> {
    guard.check()?;
    let one_plan_bytes = execution_plan_allocation_bound(graph)?;
    let needs_preparation =
        demand_requires_canonical_preparation(&graph.root, graph.demand_plan().execution_slice())?;
    let plan_count = 1 + u64::from(needs_preparation);
    let allocation_bytes = bound_mul(
        "memory bytes",
        one_plan_bytes,
        plan_count,
        graph.limits.max_memory_bytes,
    )?;
    guard.check()?;
    let execution = build_execution_plan(graph, parallel_cpu, gpu)?;
    guard.check()?;
    let preparation = needs_preparation
        .then(|| build_execution_plan(graph, false, None))
        .transpose()?;
    guard.check()?;
    Ok(EvaluationPlans {
        execution,
        preparation,
        allocation_bytes,
    })
}

impl BiomeGraphEvaluator {
    /// Creates an evaluator with an explicit bounded cell-worker count.
    pub fn new(graph: Arc<CompiledBiomeGraph>, worker_count: usize) -> Result<Self> {
        if worker_count == 0 {
            return Err(Error::GraphLimit {
                resource: "worker count",
                requested: 0,
                limit: 1,
            });
        }
        if worker_count > usize::from(graph.limits.max_workers) {
            return Err(Error::GraphLimit {
                resource: "worker count",
                requested: worker_count as u64,
                limit: u64::from(graph.limits.max_workers),
            });
        }
        Ok(Self {
            graph,
            worker_count,
            compute: None,
        })
    }

    /// Installs the qualified Slang executor used by eligible nodes in the same compiled IR.
    #[must_use]
    pub fn with_compute_executor(mut self, compute: Arc<dyn GraphComputeExecutor>) -> Self {
        self.compute = Some(compute);
        self
    }

    /// Compiled graph used by every evaluation surface.
    #[must_use]
    pub fn graph(&self) -> &CompiledBiomeGraph {
        &self.graph
    }

    /// Predicts and checks the complete job before any worker or GPU dispatch starts.
    pub fn preflight(
        &self,
        inputs: &GraphEvaluationJobInputs,
        cancellation: &GraphCancellationToken,
    ) -> Result<GraphEvaluationPreflight> {
        let deadline = evaluation_deadline(&self.graph)?;
        let guard = PreflightGuard {
            cancellation,
            deadline,
            time_limit_ms: self.graph.limits.max_time_ms,
        };
        guard.check()?;
        check_job_collection_limits(&self.graph, inputs)?;
        guard.check()?;
        let workers = self.worker_count.min(inputs.cells.len().max(1));
        let gpu = self.compute.as_deref().map(|compute| GraphGpuScheduling {
            profile: compute.profile(),
            qualifications: compute.qualifications(),
        });
        let plans = build_evaluation_plans(&self.graph, workers > 1, gpu, guard)?;
        preflight_evaluation_job(
            &self.graph,
            inputs,
            workers,
            &plans.execution,
            plans.allocation_bytes,
            guard,
        )
    }

    /// Evaluates one complete batch and publishes no cells or global tiles on failure.
    pub fn evaluate(
        &self,
        inputs: GraphEvaluationJobInputs,
        cancellation: &GraphCancellationToken,
    ) -> Result<GraphEvaluationJobResult> {
        evaluate_job(
            &self.graph,
            inputs,
            self.worker_count,
            cancellation,
            self.compute.as_deref(),
        )
    }
}

#[cfg(test)]
pub(super) fn evaluate_cell_reference(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    cancellation: &GraphCancellationToken,
) -> Result<GraphEvaluationResult> {
    let mut result = evaluate_job(
        graph,
        GraphEvaluationJobInputs {
            cells: vec![inputs.clone()],
            global_stages: Vec::new(),
        },
        1,
        cancellation,
        None,
    )?;
    result.cells.pop().ok_or_else(|| Error::GraphDocument {
        path: "evaluation.results".to_owned(),
        reason: "single-cell reference evaluation produced no result".to_owned(),
    })
}

fn evaluate_cell_planned(
    context: &EvaluationContext<'_>,
    inputs: &GraphEvaluationInputs,
    pass: EvaluationPass,
) -> Result<PlannedEvaluation> {
    let graph = context.graph;
    let scope = context.scope;
    let guard = PreflightGuard {
        cancellation: context.cancellation,
        deadline: context.deadline,
        time_limit_ms: graph.limits.max_time_ms,
    };
    validate_static_inputs(graph, inputs, scope.current_global_stage(), guard)?;
    let demand = compiled_demand_slice(graph, scope.current_global_stage())?;
    let root_demand = root_demand_unit(demand)?;
    let mut state = EvaluationState {
        graph,
        inputs,
        cancellation: context.cancellation,
        compute: context.compute,
        execution_plan: context.execution_plan,
        scope,
        pass,
        deadline: context.deadline,
        provenance: ProvenanceTable::default(),
        candidate_decisions: BTreeMap::new(),
        prepared_surface_projections: BTreeMap::new(),
        prepared_surface_fields: BTreeMap::new(),
        ancestor_references: BTreeSet::new(),
        transferred_bytes: 0,
        diagnostics: GraphEvaluationDiagnostics::default(),
        rejected_by_lineage: BTreeMap::new(),
        materialized_outputs: BTreeMap::new(),
        current_node_live_bytes: 0,
    };
    state.check_abort()?;
    let mut outputs = evaluate_unit(
        &graph.root,
        root_demand,
        demand,
        &BTreeMap::new(),
        &mut state,
    )?;
    #[cfg(test)]
    context.cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::AfterTraversal,
        graph.limits.max_time_ms,
    )?;
    state.check_abort()?;
    let mut macro_points = Vec::new();
    let mut micro_fields = Vec::new();
    let mut diagnostic_streams = Vec::new();
    let (macro_count, micro_count, stream_count) = graph.root.outputs.iter().try_fold(
        (0_usize, 0_usize, 0_usize),
        |(macro_count, micro_count, stream_count), output| -> Result<_> {
            match outputs.get(&output.name) {
                Some(GraphValue::Macro(points)) => Ok((
                    macro_count
                        .checked_add(points.len())
                        .ok_or(Error::NumericOverflow)?,
                    micro_count,
                    stream_count,
                )),
                Some(GraphValue::Micro(tiles)) => Ok((
                    macro_count,
                    micro_count
                        .checked_add(tiles.len())
                        .ok_or(Error::NumericOverflow)?,
                    stream_count,
                )),
                Some(GraphValue::Diagnostics(streams)) => Ok((
                    macro_count,
                    micro_count,
                    stream_count
                        .checked_add(streams.len())
                        .ok_or(Error::NumericOverflow)?,
                )),
                _ => Ok((macro_count, micro_count, stream_count)),
            }
        },
    )?;
    crate::memory::reserve_exact(&mut macro_points, macro_count, "terminal macro points")?;
    crate::memory::reserve_exact(&mut micro_fields, micro_count, "terminal micro fields")?;
    crate::memory::reserve_exact(
        &mut diagnostic_streams,
        stream_count,
        "terminal diagnostic streams",
    )?;
    for output in &graph.root.outputs {
        state.check_abort()?;
        let Some(value) = outputs.remove(&output.name) else {
            if state.scope.current_global_stage().is_some() {
                continue;
            }
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "evaluator did not produce output".to_owned(),
            });
        };
        match value {
            GraphValue::Macro(mut points) => macro_points.append(&mut points),
            GraphValue::Micro(mut tiles) => micro_fields.append(&mut tiles),
            GraphValue::Diagnostics(mut streams) => diagnostic_streams.append(&mut streams),
            _ => {}
        }
    }
    drop(outputs);
    state.check_abort()?;
    macro_points.sort_unstable_by_key(|point| point.id);
    micro_fields.sort_unstable_by_key(|tile| (tile.cell, tile.family.value()));
    state.check_abort()?;
    state.diagnostics.accepted_count = macro_points.len() as u64;
    state
        .diagnostics
        .rejected
        .sort_unstable_by_key(rejected_order_key);
    for stream in &mut diagnostic_streams {
        state.check_abort()?;
        if let Some(candidates) = &mut stream.candidates {
            candidates.sort_unstable_by_key(|sample| sample.identity);
        }
        if let Some(field) = &mut stream.field {
            field.sort_unstable_by_key(|sample| sample.candidate);
        }
        stream.rejected.sort_unstable_by_key(rejected_order_key);
    }
    diagnostic_streams.sort_unstable_by(|left, right| {
        diagnostic_stream_order_key(left).cmp(&diagnostic_stream_order_key(right))
    });
    state.check_abort()?;
    state.diagnostics.streams = diagnostic_streams;
    state.check_count(
        "accepted count",
        macro_points.len() as u64,
        graph.limits.max_macro_points,
    )?;
    let columns = PlantPointColumns::from_points(macro_points)?;
    state.check_abort()?;
    let mut surface_projection_tiles = Vec::new();
    crate::memory::reserve_exact(
        &mut surface_projection_tiles,
        state.prepared_surface_projections.len(),
        "prepared surface projection tiles",
    )?;
    for ((node, node_semantic_revision, provider_set_hash), samples) in
        state.prepared_surface_projections
    {
        let mut entries = Vec::new();
        crate::memory::reserve_exact(
            &mut entries,
            samples.len(),
            "prepared surface projection entries",
        )?;
        entries.extend(
            samples
                .into_iter()
                .map(|(query, sample)| QuantizedSurfaceProjectionEntry { query, sample }),
        );
        surface_projection_tiles.push(QuantizedSurfaceProjectionTile {
            node,
            node_semantic_revision,
            samples: entries,
            provider_set_hash,
        });
    }
    let mut surface_field_query_tiles = Vec::new();
    crate::memory::reserve_exact(
        &mut surface_field_query_tiles,
        state.prepared_surface_fields.len(),
        "prepared surface field query tiles",
    )?;
    for ((node, node_semantic_revision, channel, derivative, provider_set_hash), samples) in
        state.prepared_surface_fields
    {
        let mut entries = Vec::new();
        crate::memory::reserve_exact(
            &mut entries,
            samples.len(),
            "prepared surface field query entries",
        )?;
        entries.extend(samples.into_iter().map(|((candidate, query), value)| {
            QuantizedSurfaceFieldQueryEntry {
                candidate,
                query,
                value,
            }
        }));
        surface_field_query_tiles.push(QuantizedSurfaceFieldQueryTile {
            node,
            node_semantic_revision,
            channel,
            derivative,
            samples: entries,
            provider_set_hash,
        });
    }
    let mut ancestor_references = Vec::new();
    crate::memory::reserve_exact(
        &mut ancestor_references,
        state.ancestor_references.len(),
        "ancestor references",
    )?;
    ancestor_references.extend(state.ancestor_references);
    guard.check()?;
    let result = GraphEvaluationResult {
        cell: inputs.output_cell,
        macro_points: columns,
        micro_fields,
        surface_projection_tiles,
        surface_field_query_tiles,
        ancestor_references,
        provenance: state.provenance,
        diagnostics: state.diagnostics,
    };
    #[cfg(test)]
    context.cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::BeforeFinalValidation,
        graph.limits.max_time_ms,
    )?;
    result.validate_canonical_encoding_guarded(guard)?;
    let evaluated = PlannedEvaluation {
        result,
        materialized_outputs: state.materialized_outputs,
        candidate_decisions: state.candidate_decisions,
    };
    guard.check()?;
    Ok(evaluated)
}

pub(super) fn validate_projection_tile_guarded(
    tile: &QuantizedSurfaceProjectionTile,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    if tile.node == 0 || tile.node_semantic_revision == 0 || tile.provider_set_hash == [0; 32] {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProjectionTiles".to_owned(),
            reason: "projection tile identity or exact query ordering is invalid".to_owned(),
        });
    }
    let mut previous_query = None;
    for entry in &tile.samples {
        guard.check()?;
        if previous_query.is_some_and(|previous| previous >= entry.query) {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile identity or exact query ordering is invalid".to_owned(),
            });
        }
        previous_query = Some(entry.query);
        if let Some(sample) = &entry.sample {
            for pair in sample.tags.windows(2) {
                guard.check()?;
                if pair[0].tag >= pair[1].tag {
                    return Err(Error::GraphDocument {
                        path: "evaluation.surfaceProjectionTiles.tags".to_owned(),
                        reason: "projection sample tags must be sorted and unique".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_field_query_tile_guarded(
    tile: &QuantizedSurfaceFieldQueryTile,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    validate_field_query_tile_with_guard(tile, Some(guard))
}

pub(super) fn validate_field_query_tile_with_guard(
    tile: &QuantizedSurfaceFieldQueryTile,
    guard: Option<PreflightGuard<'_>>,
) -> Result<()> {
    guard.map_or(Ok(()), PreflightGuard::check)?;
    if tile.node == 0 || tile.node_semantic_revision == 0 || tile.provider_set_hash == [0; 32] {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceFieldQueryTiles".to_owned(),
            reason: "field query tile identity, type, or exact query ordering is invalid"
                .to_owned(),
        });
    }
    let mut previous_query = None;
    for entry in &tile.samples {
        guard.map_or(Ok(()), PreflightGuard::check)?;
        let type_matches = matches!(
            (tile.derivative, entry.value),
            (
                FieldDerivative::Value,
                QuantizedSurfaceFieldValue::Scalar(_)
            ) | (
                FieldDerivative::Gradient,
                QuantizedSurfaceFieldValue::Gradient(_)
            ) | (
                FieldDerivative::Hessian,
                QuantizedSurfaceFieldValue::Hessian(_)
            )
        );
        let query = (entry.candidate, entry.query);
        if !type_matches || previous_query.is_some_and(|previous| previous >= query) {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile identity, type, or exact query ordering is invalid"
                    .to_owned(),
            });
        }
        previous_query = Some(query);
    }
    guard.map_or(Ok(()), PreflightGuard::check)
}

pub(super) fn validate_static_inputs(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    global_stage: Option<&CompiledGlobalStage>,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    guard.check()?;
    let required_halo = fixed_meters_to_ticks(global_stage.map_or_else(
        || graph.required_halo(inputs.output_cell.level()),
        |stage| stage.upstream_halo,
    ))?
    .unsigned_abs() as i128;
    let required_read_bounds = expand_bounds_checked(inputs.output_bounds, required_halo)?;
    if !bounds_contains_bounds(inputs.read_bounds, required_read_bounds) {
        return Err(Error::GraphAuthoritativeInput {
            node: 0,
            input: format!(
                "immutable halo of {} fixed ticks around {}",
                required_halo, inputs.output_cell
            ),
        });
    }
    for field in &inputs.fields {
        guard.check()?;
        field.validate()?;
        validate_field_dependency(graph, inputs, field)?;
    }
    let mut projection_queries = BTreeSet::new();
    for tile in &inputs.surface_projection_tiles {
        guard.check()?;
        validate_projection_tile_guarded(tile, guard)?;
        if tile.provider_set_hash != inputs.surface_provider_set_hash {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile does not match the declared provider set".to_owned(),
            });
        }
        for entry in &tile.samples {
            guard.check()?;
            if !projection_queries.insert((tile.node, tile.node_semantic_revision, entry.query)) {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceProjectionTiles".to_owned(),
                    reason: "an exact projection query is duplicated".to_owned(),
                });
            }
        }
    }
    let mut field_queries = BTreeSet::new();
    for tile in &inputs.surface_field_query_tiles {
        guard.check()?;
        validate_field_query_tile_guarded(tile, guard)?;
        if tile.provider_set_hash != inputs.surface_provider_set_hash {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile does not match the declared provider set".to_owned(),
            });
        }
        for entry in &tile.samples {
            guard.check()?;
            if !field_queries.insert((
                tile.node,
                tile.node_semantic_revision,
                tile.channel,
                tile.derivative,
                entry.candidate,
                entry.query,
            )) {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                    reason: "an exact field query is duplicated".to_owned(),
                });
            }
        }
    }
    if !inputs.surface_providers.is_empty()
        && canonical_surface_provider_set_hash_guarded(
            &inputs.surface_providers,
            graph.limits.max_input_tiles,
            Some(guard),
        )? != inputs.surface_provider_set_hash
    {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviderSetHash".to_owned(),
            reason: "surface provider descriptors do not match the declared set identity"
                .to_owned(),
        });
    }
    for provider in &inputs.surface_providers {
        guard.check()?;
        let descriptor = provider.descriptor();
        let source = GraphDependencySource::SurfaceProvider(descriptor.id.0);
        let Some(dependency) = graph
            .dependencies()
            .iter()
            .find(|dependency| dependency.source == source)
        else {
            continue;
        };
        let actual = canonical_surface_provider_set_hash_guarded(
            &[Arc::clone(provider)],
            graph.limits.max_input_tiles,
            Some(guard),
        )?;
        if actual != dependency.content_hash {
            return Err(Error::GraphAuthoritativeInput {
                node: 0,
                input: format!("content hash for {source:?}"),
            });
        }
    }
    for prototype in &inputs.plant_prototypes {
        guard.check()?;
        prototype.validate()?;
    }
    if inputs
        .plant_prototypes
        .windows(2)
        .any(|pair| pair[0].family.value() >= pair[1].family.value())
    {
        return Err(Error::GraphDocument {
            path: "evaluation.plantPrototypes".to_owned(),
            reason: "plant prototypes must be sorted by unique family identity".to_owned(),
        });
    }
    guard.check()
}

fn validate_field_dependency(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    field: &EvaluationFieldTile,
) -> Result<()> {
    let source = match field.source {
        EvaluationFieldSource::MapLayer(layer) => GraphDependencySource::MapLayer(layer),
        EvaluationFieldSource::SurfaceProvider { provider, revision } => {
            if let Some(descriptor) = inputs
                .surface_providers
                .iter()
                .map(|provider| provider.descriptor())
                .find(|descriptor| descriptor.id == provider)
                && descriptor.revision != revision
            {
                return Err(Error::GraphAuthoritativeInput {
                    node: 0,
                    input: format!("surface provider {} revision", provider.0),
                });
            }
            GraphDependencySource::SurfaceProvider(provider.0)
        }
    };
    let dependency = graph
        .dependencies()
        .iter()
        .find(|dependency| dependency.source == source)
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: 0,
            input: format!("compiled dependency {source:?}"),
        })?;
    if dependency.content_hash != field.source_hash {
        return Err(Error::GraphAuthoritativeInput {
            node: 0,
            input: format!("content hash for {source:?}"),
        });
    }
    Ok(())
}

pub(super) fn evaluate_cell_atomically(
    mut inputs: GraphEvaluationInputs,
    context: EvaluationContext<'_>,
    preparation_plan: Option<&GraphExecutionPlan>,
) -> Result<PlannedEvaluation> {
    let demand = compiled_demand_slice(context.graph, context.scope.current_global_stage())?;
    if !inputs.surface_providers.is_empty()
        && demand_requires_canonical_preparation(&context.graph.root, demand)?
    {
        let preparation_plan = preparation_plan.ok_or_else(|| Error::GraphDocument {
            path: "evaluation.preparationPlan".to_owned(),
            reason: "canonical surface preparation plan is missing".to_owned(),
        })?;
        let preparation_context = EvaluationContext {
            graph: context.graph,
            cancellation: context.cancellation,
            compute: None,
            execution_plan: preparation_plan,
            scope: context.scope,
            deadline: context.deadline,
        };
        let (surface_projection_tiles, surface_field_query_tiles) = {
            let prepared = evaluate_cell_planned(
                &preparation_context,
                &inputs,
                EvaluationPass::PrepareCanonicalInputs,
            )?;
            let GraphEvaluationResult {
                surface_projection_tiles,
                surface_field_query_tiles,
                ..
            } = prepared.result;
            (surface_projection_tiles, surface_field_query_tiles)
        };
        #[cfg(test)]
        context.cancellation.check_test_checkpoint(
            TestEvaluationCheckpoint::AfterPreparation,
            context.graph.limits.max_time_ms,
        )?;
        inputs.surface_projection_tiles = surface_projection_tiles;
        inputs.surface_field_query_tiles = surface_field_query_tiles;
        check_limit(
            "input tiles",
            evaluation_input_tile_count(&inputs)?,
            context.graph.limits.max_input_tiles,
        )?;
        return evaluate_cell_planned(&context, &inputs, EvaluationPass::AuthoritativeReplay);
    }
    evaluate_cell_planned(&context, &inputs, EvaluationPass::AuthoritativeReplay)
}

pub(super) fn demand_requires_canonical_preparation(
    unit: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<bool> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| Error::GraphDocument {
            path: "graph.demandPlan".to_owned(),
            reason: "live unit has no preparation demand slice".to_owned(),
        })?;
    for node in unit.nodes.iter().filter(|node| {
        unit_demand.contains_node(node.definition.guid) && demand.executes_node(&node.address())
    }) {
        if node.definition.authority != GraphAuthority::Cosmetic
            && matches!(
                node.definition.operator,
                GraphOperator::SurfaceProjection | GraphOperator::FieldSample
            )
        {
            return Ok(true);
        }
        if let Some(module) = node.module.as_deref()
            && find_child_demand_unit(demand, node).is_some()
            && demand_requires_canonical_preparation(module, demand)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn evaluation_deadline(graph: &CompiledBiomeGraph) -> Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_millis(graph.limits.max_time_ms))
        .ok_or(Error::NumericOverflow)
}

pub(super) fn check_job_collection_limits(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
) -> Result<()> {
    check_limit(
        "output cells",
        inputs.cells.len() as u64,
        graph.limits.max_output_cells,
    )?;
    check_limit(
        "global stage tiles",
        inputs.global_stages.len() as u64,
        graph.limits.max_global_stage_tiles,
    )
}

pub(super) fn canonicalize_evaluation_input(input: &mut GraphEvaluationInputs) {
    input.regions.sort_unstable_by_key(|region| {
        (
            region.id,
            region.layer,
            region.kind as u8,
            region.hierarchy_namespace,
            region.seed_cell,
            region.bounds.min_ticks(),
            region.bounds.max_ticks_exclusive(),
        )
    });
    input.splines.sort_unstable_by_key(|spline| spline.id);
    input
        .anchors
        .sort_unstable_by_key(|anchor| (anchor.layer, anchor.point.id));
    input
        .plant_prototypes
        .sort_unstable_by_key(|prototype| prototype.family.value());
    input.fields.sort_unstable_by_key(|tile| {
        (
            tile.channel,
            tile.derivative,
            tile.layer_order,
            tile.source,
            tile.bounds.min_ticks(),
            tile.bounds.max_ticks_exclusive(),
            tile.source_hash,
        )
    });
    input.surface_projection_tiles.sort_unstable_by_key(|tile| {
        (
            tile.node,
            tile.node_semantic_revision,
            tile.samples.first().map(|entry| entry.query),
            tile.samples.last().map(|entry| entry.query),
            tile.provider_set_hash,
        )
    });
    input
        .surface_field_query_tiles
        .sort_unstable_by_key(|tile| {
            (
                tile.node,
                tile.node_semantic_revision,
                tile.channel,
                tile.derivative,
                tile.samples
                    .first()
                    .map(|entry| (entry.candidate, entry.query)),
                tile.samples
                    .last()
                    .map(|entry| (entry.candidate, entry.query)),
                tile.provider_set_hash,
            )
        });
    input
        .surface_providers
        .sort_unstable_by_key(|provider| provider.descriptor().id);
}
