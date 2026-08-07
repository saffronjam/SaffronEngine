//! Symbolic admission of a complete graph job against its limits.

use super::*;

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::sync::Arc;

use saffron_spatial::{
    FieldChannel, FieldDerivative, SurfaceField, SurfaceProviderDescriptor, WeightedSurfaceTag,
    WorldCellKey, WorldPosition, world_cells_covering_bounds,
};

use crate::memory::{
    ALLOCATION_OVERHEAD_BYTES, checked_memory_sum, requested_btree_bytes, requested_btree_with,
    requested_vec_bytes,
};
use crate::{
    CompiledBiomeGraph, Error, GraphExecutionPlan, PlantPointColumns, ProvenanceDecisionHandle,
    Result,
};

fn preflight_evaluation_inputs(
    graph: &CompiledBiomeGraph,
    inputs: &[GraphEvaluationInputs],
    worker_count: usize,
    plan: &GraphExecutionPlan,
    global_store: &SymbolicGlobalStore,
    guard: PreflightGuard<'_>,
) -> Result<SymbolicInputPreflight> {
    guard.check()?;
    check_limit(
        "output cells",
        inputs.len() as u64,
        graph.limits.max_output_cells,
    )?;
    let mut cells = BTreeSet::new();
    let mut input_tiles = 0_u64;
    let mut retained_input_bytes = 0_u64;
    let mut generated_input_bytes = 0_u64;
    let mut candidate_count = 0_u64;
    let mut accepted_count = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    let mut retained_result_bytes = 0_u64;
    let mut worker_memory = BinaryHeap::with_capacity(worker_count);
    for input in inputs {
        guard.check()?;
        if !cells.insert(input.output_cell) {
            return Err(Error::GraphDocument {
                path: "evaluation.outputCells".to_owned(),
                reason: format!("output cell {} is duplicated", input.output_cell),
            });
        }
        input_tiles = bound_add(
            "input tiles",
            input_tiles,
            evaluation_input_tile_count(input)?,
            graph.limits.max_input_tiles,
        )?;
        retained_input_bytes = bound_add(
            "memory bytes",
            retained_input_bytes,
            estimated_input_bytes(input, guard)?,
            graph.limits.max_memory_bytes,
        )?;
        validate_static_inputs(graph, input, None, guard)?;
        guard.check()?;
        let symbolic = symbolic_evaluation_bound(
            graph,
            input,
            plan,
            SymbolicEvaluationScope::Cell { global_store },
            guard,
        )?;
        input_tiles = bound_add(
            "input tiles",
            input_tiles,
            symbolic.bound.generated_input_tiles,
            graph.limits.max_input_tiles,
        )?;
        generated_input_bytes = bound_add(
            "memory bytes",
            generated_input_bytes,
            symbolic.bound.generated_input_bytes,
            graph.limits.max_memory_bytes,
        )?;
        retained_result_bytes = bound_add(
            "memory bytes",
            retained_result_bytes,
            symbolic.public_result_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let ancestor_references = symbolic.ancestor_references;
        let bound = symbolic.bound;
        let estimated_candidates = bound.candidate_peak;
        candidate_count = bound_add(
            "candidate count",
            candidate_count,
            estimated_candidates,
            graph.limits.max_candidates,
        )?;
        accepted_count = bound_add(
            "accepted count",
            accepted_count,
            bound.accepted,
            graph.limits.max_macro_points,
        )?;
        micro_samples = bound_add(
            "micro samples",
            micro_samples,
            bound.micro_samples,
            graph.limits.max_micro_samples,
        )?;
        transfer_bytes = bound_add(
            "transfer bytes",
            transfer_bytes,
            bound.transfer_bytes,
            graph.limits.max_transfer_bytes,
        )?;
        let point_column_peak = PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?;
        let worker_bytes = checked_memory_sum([
            bound.memory_bytes,
            input_validation_scratch_bytes(input)?,
            point_column_peak,
            runtime_traversal_allocation_bound(
                graph,
                graph.demand_plan().public_slice(),
                plan,
                ancestor_references,
            )?,
        ])?;
        check_limit("memory bytes", worker_bytes, graph.limits.max_memory_bytes)?;
        if worker_memory.len() < worker_count {
            worker_memory.push(Reverse(worker_bytes));
        } else if worker_memory
            .peek()
            .is_some_and(|minimum| worker_bytes > minimum.0)
        {
            worker_memory.pop();
            worker_memory.push(Reverse(worker_bytes));
        }
    }
    let active_worker_memory =
        worker_memory
            .into_iter()
            .try_fold(0_u64, |total, Reverse(value)| {
                bound_add("memory bytes", total, value, graph.limits.max_memory_bytes)
            })?;
    Ok(SymbolicInputPreflight {
        input_tiles,
        retained_input_bytes,
        generated_input_bytes,
        candidate_count,
        accepted_count,
        micro_samples,
        transfer_bytes,
        active_worker_memory,
        retained_result_bytes,
    })
}

fn estimated_input_bytes(input: &GraphEvaluationInputs, guard: PreflightGuard<'_>) -> Result<u64> {
    let mut bytes = checked_memory_sum([
        requested_vec_bytes::<EvaluationFieldTile>(input.fields.capacity())?,
        requested_vec_bytes::<QuantizedSurfaceProjectionTile>(
            input.surface_projection_tiles.capacity(),
        )?,
        requested_vec_bytes::<QuantizedSurfaceFieldQueryTile>(
            input.surface_field_query_tiles.capacity(),
        )?,
        requested_vec_bytes::<EvaluationRegion>(input.regions.capacity())?,
        requested_vec_bytes::<EvaluationSpline>(input.splines.capacity())?,
        requested_vec_bytes::<EvaluationAnchor>(input.anchors.capacity())?,
        requested_vec_bytes::<PlantPrototype>(input.plant_prototypes.capacity())?,
        requested_vec_bytes::<Arc<dyn SurfaceField>>(input.surface_providers.capacity())?,
    ])?;
    for tile in &input.fields {
        guard.check()?;
        let values = match &tile.values {
            QuantizedFieldTileValues::Scalar(values) => {
                requested_vec_bytes::<i32>(values.capacity())?
            }
            QuantizedFieldTileValues::Gradient(values) => {
                requested_vec_bytes::<[i32; 3]>(values.capacity())?
            }
            QuantizedFieldTileValues::Hessian(values) => {
                requested_vec_bytes::<[i32; 6]>(values.capacity())?
            }
        };
        bytes = bytes.checked_add(values).ok_or(Error::NumericOverflow)?;
    }
    for tile in &input.surface_projection_tiles {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<QuantizedSurfaceProjectionEntry>(
                tile.samples.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
        for entry in &tile.samples {
            guard.check()?;
            if let Some(sample) = &entry.sample {
                bytes = bytes
                    .checked_add(requested_vec_bytes::<WeightedSurfaceTag>(
                        sample.tags.capacity(),
                    )?)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
    }
    for tile in &input.surface_field_query_tiles {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<QuantizedSurfaceFieldQueryEntry>(
                tile.samples.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
    }
    for spline in &input.splines {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<WorldPosition>(
                spline.points.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
    }
    guard.check()?;
    Ok(bytes)
}

fn input_validation_scratch_bytes(input: &GraphEvaluationInputs) -> Result<u64> {
    let projection_queries =
        input
            .surface_projection_tiles
            .iter()
            .try_fold(0_usize, |total, tile| {
                total
                    .checked_add(tile.samples.len())
                    .ok_or(Error::NumericOverflow)
            })?;
    let field_queries =
        input
            .surface_field_query_tiles
            .iter()
            .try_fold(0_usize, |total, tile| {
                total
                    .checked_add(tile.samples.len())
                    .ok_or(Error::NumericOverflow)
            })?;
    checked_memory_sum([
        requested_btree_bytes::<(u128, u32, WorldPosition), ()>(projection_queries)?,
        requested_btree_bytes::<
            (
                u128,
                u32,
                FieldChannel,
                FieldDerivative,
                CandidateIdentity,
                WorldPosition,
            ),
            (),
        >(field_queries)?,
        requested_vec_bytes::<SurfaceProviderDescriptor>(input.surface_providers.len())?,
    ])
}

fn preflight_scratch_bytes(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
) -> Result<u64> {
    let maximum_expected_global = inputs.global_stages.len();
    let guarded_expected_global = maximum_expected_global
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    let stage_count = graph.spatial_plan().global_stages().len();
    let validation_scratch = inputs
        .cells
        .iter()
        .chain(inputs.global_stages.iter().map(|stage| &stage.inputs))
        .try_fold(0_u64, |maximum, input| {
            Ok::<_, Error>(maximum.max(input_validation_scratch_bytes(input)?))
        })?;
    checked_memory_sum([
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], u8), ()>(stage_count)?,
        requested_vec_bytes::<WorldCellKey>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(inputs.global_stages.len())?,
        requested_vec_bytes::<&GlobalStageEvaluationInputs>(inputs.global_stages.len())?,
        requested_btree_bytes::<WorldCellKey, ()>(inputs.cells.len())?,
        requested_vec_bytes::<u64>(worker_count)?,
        symbolic_traversal_allocation_bound(graph, graph.demand_plan().execution_slice())?,
        validation_scratch,
    ])
}

fn job_input_container_bytes(inputs: &GraphEvaluationJobInputs) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes::<GraphEvaluationInputs>(inputs.cells.capacity())?,
        requested_vec_bytes::<GlobalStageEvaluationInputs>(inputs.global_stages.capacity())?,
    ])
}

fn runtime_job_allocation_bytes(
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
    plan_allocation_bytes: u64,
) -> Result<u64> {
    let worker_count_u64 = u64::try_from(worker_count).map_err(|_| Error::NumericOverflow)?;
    let shard_allocations = worker_count_u64
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let worker_stacks = worker_count_u64
        .checked_mul(EVALUATOR_WORKER_STACK_BYTES as u64)
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        plan_allocation_bytes,
        worker_stacks,
        requested_vec_bytes::<Vec<(usize, GraphEvaluationInputs)>>(worker_count)?,
        requested_vec_bytes::<(usize, GraphEvaluationInputs)>(inputs.cells.len())?,
        shard_allocations,
        requested_vec_bytes::<
            std::thread::ScopedJoinHandle<
                'static,
                Result<Vec<(usize, Result<GraphEvaluationResult>)>>,
            >,
        >(worker_count)?,
        requested_vec_bytes::<(usize, Result<GraphEvaluationResult>)>(inputs.cells.len())?,
        shard_allocations,
        requested_vec_bytes::<(usize, Result<GraphEvaluationResult>)>(inputs.cells.len())?,
        requested_vec_bytes::<GraphEvaluationResult>(inputs.cells.len())?,
        requested_vec_bytes::<GlobalStageEvaluationResult>(inputs.global_stages.len())?,
        requested_btree_bytes::<GlobalStageCacheKey, GlobalStageTile>(inputs.global_stages.len())?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), GlobalStageCacheKey>(
            inputs.global_stages.len(),
        )?,
    ])
}

pub(super) fn evaluation_input_tile_count(input: &GraphEvaluationInputs) -> Result<u64> {
    [
        input.fields.len(),
        input.surface_projection_tiles.len(),
        input.surface_field_query_tiles.len(),
        input.regions.len(),
        input.splines.len(),
        input.surface_providers.len(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, count| {
        total
            .checked_add(u64::try_from(count).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })
}

pub(super) fn check_limit(resource: &'static str, requested: u64, limit: u64) -> Result<()> {
    if requested > limit {
        return Err(Error::GraphLimit {
            resource,
            requested,
            limit,
        });
    }
    Ok(())
}

pub(super) fn global_stage_order(graph: &CompiledBiomeGraph, stage_id: [u8; 32]) -> usize {
    graph
        .spatial_plan()
        .global_stages()
        .iter()
        .position(|stage| stage.id == stage_id)
        .unwrap_or(usize::MAX)
}

pub(super) fn preflight_evaluation_job(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
    plan: &GraphExecutionPlan,
    plan_allocation_bytes: u64,
    guard: PreflightGuard<'_>,
) -> Result<GraphEvaluationPreflight> {
    guard.check()?;
    let preflight_scratch = preflight_scratch_bytes(graph, inputs, worker_count)?;
    check_limit(
        "memory bytes",
        checked_memory_sum([plan_allocation_bytes, preflight_scratch])?,
        graph.limits.max_memory_bytes,
    )?;
    let mut job_identity = None;
    for input in inputs
        .cells
        .iter()
        .chain(inputs.global_stages.iter().map(|stage| &stage.inputs))
    {
        let identity = (input.map, input.biome_instance, input.ecology_tick);
        if job_identity
            .replace(identity)
            .is_some_and(|expected| expected != identity)
        {
            return Err(Error::GraphDocument {
                path: "evaluation.job".to_owned(),
                reason: "one graph job must use one map, biome instance, and ecology snapshot"
                    .to_owned(),
            });
        }
    }
    check_limit(
        "global stage tiles",
        inputs.global_stages.len() as u64,
        graph.limits.max_global_stage_tiles,
    )?;

    let expected = expected_global_stage_tiles_guarded(
        graph,
        &inputs.cells,
        Some(guard),
        inputs.global_stages.len(),
    )?;
    let mut actual = BTreeSet::new();
    let mut global_input_tiles = 0_u64;
    let mut global_retained_input_bytes = 0_u64;
    let mut global_generated_input_bytes = 0_u64;
    let mut global_resident_memory = 0_u64;
    let mut global_peak_memory = 0_u64;
    let mut global_candidates = 0_u64;
    let mut global_accepted = 0_u64;
    let mut global_micro_samples = 0_u64;
    let mut global_transfer_bytes = 0_u64;
    let mut symbolic_global_store = SymbolicGlobalStore::default();
    let mut ordered_global_inputs = Vec::new();
    crate::memory::reserve_exact(
        &mut ordered_global_inputs,
        inputs.global_stages.len(),
        "ordered global-stage inputs",
    )?;
    ordered_global_inputs.extend(inputs.global_stages.iter());
    ordered_global_inputs.sort_unstable_by_key(|input| {
        (
            global_stage_order(graph, input.stage),
            input.owner,
            input.input_snapshot,
        )
    });
    for input in ordered_global_inputs {
        guard.check()?;
        let stage = graph
            .spatial_plan()
            .global_stage(input.stage)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!("unknown global stage {}", hex_hash(input.stage)),
            })?;
        if !actual.insert((input.stage, input.owner)) {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "a global-stage owner tile is duplicated".to_owned(),
            });
        }
        let expected_solve_bounds = input.owner.bounds();
        let expected_read_bounds = expand_bounds_checked(
            expected_solve_bounds,
            fixed_meters_to_ticks(stage.upstream_halo)?.unsigned_abs() as i128,
        )?;
        if input.owner.level() != stage.owner_level
            || input.solve_bounds != expected_solve_bounds
            || input.inputs.output_cell != input.owner
            || input.inputs.output_bounds != expected_solve_bounds
            || input.inputs.read_bounds != expected_read_bounds
            || input.input_snapshot == [0; 32]
        {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!(
                    "stage {} owner, solve/read bounds, or immutable snapshot is invalid",
                    hex_hash(stage.id)
                ),
            });
        }
        global_input_tiles = bound_add(
            "input tiles",
            global_input_tiles,
            evaluation_input_tile_count(&input.inputs)?,
            graph.limits.max_input_tiles,
        )?;
        global_retained_input_bytes = bound_add(
            "memory bytes",
            global_retained_input_bytes,
            estimated_input_bytes(&input.inputs, guard)?,
            graph.limits.max_memory_bytes,
        )?;
        validate_static_inputs(graph, &input.inputs, Some(stage), guard)?;
        guard.check()?;
        let symbolic = symbolic_evaluation_bound(
            graph,
            &input.inputs,
            plan,
            SymbolicEvaluationScope::Global {
                stage,
                global_store: &symbolic_global_store,
            },
            guard,
        )?;
        global_input_tiles = bound_add(
            "input tiles",
            global_input_tiles,
            symbolic.bound.generated_input_tiles,
            graph.limits.max_input_tiles,
        )?;
        global_generated_input_bytes = bound_add(
            "memory bytes",
            global_generated_input_bytes,
            symbolic.bound.generated_input_bytes,
            graph.limits.max_memory_bytes,
        )?;
        if symbolic.materialized_outputs.len() != stage.output_pins.len() {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!(
                    "symbolic stage {} did not materialize every boundary output",
                    hex_hash(stage.id)
                ),
            });
        }
        let stage_retained_bytes = symbolic.global_tile_bytes;
        check_limit(
            "memory bytes",
            stage_retained_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let ancestor_references = symbolic.ancestor_references;
        let bound = symbolic.bound;
        let global_worker_bytes = checked_memory_sum([
            bound.memory_bytes,
            input_validation_scratch_bytes(&input.inputs)?,
            PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?,
            runtime_traversal_allocation_bound(
                graph,
                graph
                    .demand_plan()
                    .stage_slice(stage.id)
                    .ok_or_else(|| Error::GraphDocument {
                        path: "graph.demandPlan".to_owned(),
                        reason: "global stage has no demand slice".to_owned(),
                    })?,
                plan,
                ancestor_references,
            )?,
        ])?;
        let live_during_stage = bound_add(
            "memory bytes",
            global_resident_memory,
            global_worker_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let resident_with_stage = bound_add(
            "memory bytes",
            global_resident_memory,
            stage_retained_bytes,
            graph.limits.max_memory_bytes,
        );
        let resident_with_stage = resident_with_stage?;
        global_peak_memory = global_peak_memory
            .max(live_during_stage)
            .max(resident_with_stage);
        global_candidates = bound_add(
            "candidate count",
            global_candidates,
            bound.candidate_peak,
            graph.limits.max_candidates,
        )?;
        global_accepted = bound_add(
            "accepted count",
            global_accepted,
            bound.accepted,
            graph.limits.max_macro_points,
        )?;
        global_micro_samples = bound_add(
            "micro samples",
            global_micro_samples,
            bound.micro_samples,
            graph.limits.max_micro_samples,
        )?;
        global_transfer_bytes = bound_add(
            "transfer bytes",
            global_transfer_bytes,
            bound.transfer_bytes,
            graph.limits.max_transfer_bytes,
        )?;
        global_resident_memory = resident_with_stage;
        let provenance_records = bound
            .accepted
            .checked_add(bound.rejected)
            .and_then(|records| records.checked_add(bound.imported_provenance_records))
            .ok_or(Error::NumericOverflow)?;
        let symbolic_tile = SymbolicGlobalTile {
            outputs: symbolic.materialized_outputs,
            provenance_decisions: bound.candidate_events,
            provenance_records,
            provenance_bytes: bound.provenance_bytes,
        };
        if symbolic_global_store
            .tiles
            .insert((stage.id, input.owner), symbolic_tile)
            .is_some()
        {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "symbolic global-stage owner tile is duplicated".to_owned(),
            });
        }
    }
    if actual != expected {
        let missing = expected.difference(&actual).count();
        let unexpected = actual.difference(&expected).count();
        return Err(Error::GraphDocument {
            path: "evaluation.globalStages".to_owned(),
            reason: format!(
                "global-stage tile set is not closed (missing {missing}, unexpected {unexpected})"
            ),
        });
    }

    let cells = preflight_evaluation_inputs(
        graph,
        &inputs.cells,
        worker_count,
        plan,
        &symbolic_global_store,
        guard,
    )?;
    let input_tiles = bound_add(
        "input tiles",
        cells.input_tiles,
        global_input_tiles,
        graph.limits.max_input_tiles,
    )?;
    let candidate_count = bound_add(
        "candidate count",
        cells.candidate_count,
        global_candidates,
        graph.limits.max_candidates,
    )?;
    let accepted_count = bound_add(
        "accepted count",
        cells.accepted_count,
        global_accepted,
        graph.limits.max_macro_points,
    )?;
    let micro_samples = bound_add(
        "micro samples",
        cells.micro_samples,
        global_micro_samples,
        graph.limits.max_micro_samples,
    )?;
    let transfer_bytes = bound_add(
        "transfer bytes",
        cells.transfer_bytes,
        global_transfer_bytes,
        graph.limits.max_transfer_bytes,
    )?;

    let retained_results_memory = bound_add(
        "memory bytes",
        global_resident_memory,
        cells.retained_result_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let runtime_peak_memory = global_peak_memory.max(bound_add(
        "memory bytes",
        retained_results_memory,
        cells.active_worker_memory,
        graph.limits.max_memory_bytes,
    )?);
    let nested_retained_input_bytes = bound_add(
        "memory bytes",
        cells.retained_input_bytes,
        global_retained_input_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let retained_input_bytes = bound_add(
        "memory bytes",
        nested_retained_input_bytes,
        job_input_container_bytes(inputs)?,
        graph.limits.max_memory_bytes,
    )?;
    let generated_input_bytes = bound_add(
        "memory bytes",
        cells.generated_input_bytes,
        global_generated_input_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let runtime_allocations =
        runtime_job_allocation_bytes(inputs, worker_count, plan_allocation_bytes)?;
    let execution_peak_bytes = checked_memory_sum([
        retained_input_bytes,
        runtime_peak_memory,
        runtime_allocations,
    ])?;
    let preflight_peak_bytes = checked_memory_sum([
        retained_input_bytes,
        plan_allocation_bytes,
        preflight_scratch,
        symbolic_global_store.requested_memory_bytes()?,
    ])?;
    let memory_bytes = execution_peak_bytes.max(preflight_peak_bytes);
    check_limit("memory bytes", memory_bytes, graph.limits.max_memory_bytes)?;
    let preflight = GraphEvaluationPreflight {
        output_cells: inputs.cells.len() as u64,
        global_stage_tiles: inputs.global_stages.len() as u64,
        input_tiles,
        retained_input_bytes,
        generated_input_bytes,
        candidate_count,
        accepted_count,
        micro_samples,
        preflight_peak_bytes,
        execution_peak_bytes,
        memory_bytes,
        transfer_bytes,
        worker_count: u16::try_from(worker_count).map_err(|_| Error::NumericOverflow)?,
        time_limit_ms: graph.limits.max_time_ms,
        limits: graph.limits,
    };
    guard.check()?;
    Ok(preflight)
}

#[cfg(test)]
pub(super) fn expected_global_stage_tiles(
    graph: &CompiledBiomeGraph,
    cells: &[GraphEvaluationInputs],
) -> Result<BTreeSet<([u8; 32], WorldCellKey)>> {
    expected_global_stage_tiles_guarded(
        graph,
        cells,
        None,
        usize::try_from(graph.limits.max_global_stage_tiles).map_err(|_| Error::NumericOverflow)?,
    )
}

fn expected_global_stage_tiles_guarded(
    graph: &CompiledBiomeGraph,
    cells: &[GraphEvaluationInputs],
    guard: Option<PreflightGuard<'_>>,
    maximum_expected: usize,
) -> Result<BTreeSet<([u8; 32], WorldCellKey)>> {
    let guarded_expected = maximum_expected
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    let mut expected = BTreeSet::new();
    for stage in graph.spatial_plan().global_stages() {
        for cell in cells {
            if let Some(guard) = guard {
                guard.check()?;
            }
            for owner in world_cells_covering_bounds(
                cell.read_bounds,
                stage.owner_level,
                u64::try_from(guarded_expected).map_err(|_| Error::NumericOverflow)?,
            )? {
                expected.insert((stage.id, owner));
                ensure_expected_global_tile_capacity(&expected, maximum_expected)?;
            }
        }
    }

    loop {
        let mut additions = BTreeSet::new();
        for (stage_id, owner) in expected.iter().copied() {
            if let Some(guard) = guard {
                guard.check()?;
            }
            let stage = graph.spatial_plan().global_stage(stage_id).ok_or_else(|| {
                Error::GraphDocument {
                    path: "graph.spatialPlan".to_owned(),
                    reason: "selected global stage disappeared from the compiled plan".to_owned(),
                }
            })?;
            let read_bounds = expand_bounds_checked(
                owner.bounds(),
                fixed_meters_to_ticks(stage.upstream_halo)?.unsigned_abs() as i128,
            )?;
            let prerequisite_stages = stage
                .input_pins
                .iter()
                .filter_map(|pin| graph.spatial_plan().global_stage_for_node(&pin.node))
                .map(|stage| (stage.id, stage.owner_level))
                .collect::<BTreeSet<_>>();
            for (prerequisite, owner_level) in prerequisite_stages {
                for prerequisite_owner in world_cells_covering_bounds(
                    read_bounds,
                    owner_level,
                    u64::try_from(guarded_expected).map_err(|_| Error::NumericOverflow)?,
                )? {
                    if !expected.contains(&(prerequisite, prerequisite_owner)) {
                        additions.insert((prerequisite, prerequisite_owner));
                        if expected
                            .len()
                            .checked_add(additions.len())
                            .ok_or(Error::NumericOverflow)?
                            > maximum_expected
                        {
                            return Err(Error::GraphDocument {
                                path: "evaluation.globalStages".to_owned(),
                                reason: "global-stage tile set is not closed (missing tiles)"
                                    .to_owned(),
                            });
                        }
                    }
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        expected.extend(additions);
        ensure_expected_global_tile_capacity(&expected, maximum_expected)?;
    }
    Ok(expected)
}

fn ensure_expected_global_tile_capacity(
    expected: &BTreeSet<([u8; 32], WorldCellKey)>,
    maximum_expected: usize,
) -> Result<()> {
    if expected.len() > maximum_expected {
        return Err(Error::GraphDocument {
            path: "evaluation.globalStages".to_owned(),
            reason: "global-stage tile set is not closed (missing tiles)".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn retained_global_tile_bytes(evaluated: &PlannedEvaluation) -> Result<u64> {
    checked_memory_sum([
        requested_btree_with(
            &evaluated.materialized_outputs,
            qualified_graph_pin_memory,
            GraphValue::requested_memory_bytes,
        )?,
        retained_result_bytes(&evaluated.result)?,
        evaluated.result.provenance.requested_memory_bytes()?,
        requested_btree_bytes::<CandidateIdentity, ProvenanceDecisionHandle>(
            evaluated.candidate_decisions.len(),
        )?,
    ])
}

pub(super) fn retained_result_bytes(result: &GraphEvaluationResult) -> Result<u64> {
    graph_result_memory(result)
}
