//! Parallel job execution across bounded cell workers.

use super::*;

use crate::{CompiledBiomeGraph, Error, GraphComputeExecutor, GraphGpuScheduling, Result};

pub(super) fn evaluate_job(
    graph: &CompiledBiomeGraph,
    mut inputs: GraphEvaluationJobInputs,
    worker_count: usize,
    cancellation: &GraphCancellationToken,
    compute: Option<&dyn GraphComputeExecutor>,
) -> Result<GraphEvaluationJobResult> {
    if worker_count == 0 {
        return Err(Error::GraphLimit {
            resource: "worker count",
            requested: 0,
            limit: 1,
        });
    }
    let deadline = evaluation_deadline(graph)?;
    let guard = PreflightGuard {
        cancellation,
        deadline,
        time_limit_ms: graph.limits.max_time_ms,
    };
    guard.check()?;
    check_job_collection_limits(graph, &inputs)?;
    guard.check()?;
    for input in &mut inputs.cells {
        canonicalize_evaluation_input(input);
    }
    for stage in &mut inputs.global_stages {
        canonicalize_evaluation_input(&mut stage.inputs);
    }
    inputs.cells.sort_unstable_by_key(|input| input.output_cell);
    guard.check()?;
    inputs.global_stages.sort_unstable_by_key(|input| {
        (
            global_stage_order(graph, input.stage),
            input.owner,
            input.input_snapshot,
        )
    });
    guard.check()?;
    let workers = worker_count.min(inputs.cells.len().max(1));
    let gpu = compute.map(|compute| GraphGpuScheduling {
        profile: compute.profile(),
        qualifications: compute.qualifications(),
    });
    let plans = build_evaluation_plans(graph, workers > 1, gpu, guard)?;
    let execution_plan = &plans.execution;
    let preparation_plan = plans.preparation.as_ref();
    preflight_evaluation_job(
        graph,
        &inputs,
        workers,
        execution_plan,
        plans.allocation_bytes,
        guard,
    )?;
    #[cfg(test)]
    cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::AfterPreflight,
        graph.limits.max_time_ms,
    )?;
    let mut global_store = GlobalStageStore::default();
    let mut global_results = Vec::new();
    crate::memory::reserve_exact(
        &mut global_results,
        inputs.global_stages.len(),
        "global stage results",
    )?;
    for input in inputs.global_stages {
        guard.check()?;
        let GlobalStageEvaluationInputs {
            stage: stage_id,
            owner,
            input_snapshot,
            inputs: stage_inputs,
            ..
        } = input;
        let stage =
            graph
                .spatial_plan()
                .global_stage(stage_id)
                .ok_or_else(|| Error::GraphDocument {
                    path: "evaluation.globalStages".to_owned(),
                    reason: "global-stage identity is absent from the compiled graph".to_owned(),
                })?;
        let map = stage_inputs.map.value();
        let biome_instance = stage_inputs.biome_instance;
        let context = EvaluationContext {
            graph,
            cancellation,
            compute,
            execution_plan,
            scope: EvaluationScope::Global {
                stage,
                global_store: &global_store,
            },
            deadline,
        };
        let evaluated = evaluate_cell_atomically(stage_inputs, context, preparation_plan)?;
        for output in &stage.output_pins {
            guard.check()?;
            if !evaluated.materialized_outputs.contains_key(output) {
                return Err(Error::GraphDocument {
                    path: "evaluation.globalStages".to_owned(),
                    reason: format!(
                        "stage {} did not materialize boundary pin '{}'",
                        hex_hash(stage.id),
                        output.pin
                    ),
                });
            }
        }
        guard.check()?;
        let resident_bytes = retained_global_tile_bytes(&evaluated)?;
        guard.check()?;
        let public_result = evaluated.result;
        let tile = GlobalStageTile {
            key: GlobalStageCacheKey {
                stage: stage_id,
                map,
                biome_instance,
                owner,
                input_snapshot,
            },
            outputs: evaluated.materialized_outputs,
            provenance: public_result.provenance.clone(),
            candidate_decisions: evaluated.candidate_decisions,
            resident_bytes,
        };
        global_store.insert(tile)?;
        guard.check()?;
        check_limit(
            "memory bytes",
            global_store.resident_bytes()?,
            graph.limits.max_memory_bytes,
        )?;
        global_results.push(GlobalStageEvaluationResult {
            stage: stage_id,
            owner,
            result: public_result,
            resident_bytes,
        });
    }
    guard.check()?;

    let cell_count = inputs.cells.len();
    let mut shards = Vec::new();
    crate::memory::reserve_exact(&mut shards, workers, "cell worker shards")?;
    for worker in 0..workers {
        let capacity = cell_count
            .checked_add(workers - 1 - worker)
            .ok_or(Error::NumericOverflow)?
            / workers;
        let mut shard = Vec::new();
        crate::memory::reserve_exact(&mut shard, capacity, "cell worker shard")?;
        shards.push(shard);
    }
    for (index, input) in inputs.cells.into_iter().enumerate() {
        guard.check()?;
        shards[index % workers].push((index, input));
    }
    guard.check()?;
    let global_store = &global_store;
    let mut results = std::thread::scope(|scope| -> Result<Vec<_>> {
        let mut handles = Vec::new();
        crate::memory::reserve_exact(&mut handles, workers, "cell worker handles")?;
        for (worker, shard) in shards.into_iter().enumerate() {
            let handle = std::thread::Builder::new()
                .name(format!("vegetation-graph-{worker}"))
                .stack_size(EVALUATOR_WORKER_STACK_BYTES)
                .spawn_scoped(scope, move || -> Result<Vec<_>> {
                    let mut worker_results = Vec::new();
                    crate::memory::reserve_exact(
                        &mut worker_results,
                        shard.len(),
                        "cell worker results",
                    )?;
                    for (index, input) in shard {
                        let context = EvaluationContext {
                            graph,
                            cancellation,
                            compute,
                            execution_plan,
                            scope: EvaluationScope::Cell { global_store },
                            deadline,
                        };
                        worker_results.push((
                            index,
                            evaluate_cell_atomically(input, context, preparation_plan)
                                .map(|planned| planned.result),
                        ));
                    }
                    Ok(worker_results)
                })
                .map_err(|source| Error::GraphWorkerSpawn { source })?;
            handles.push(handle);
        }
        let mut joined = Vec::new();
        crate::memory::reserve_exact(&mut joined, cell_count, "joined cell results")?;
        for handle in handles {
            let mut worker_results = handle.join().map_err(|_| Error::GraphWorkerPanicked)??;
            joined.append(&mut worker_results);
        }
        Ok(joined)
    })?;
    guard.check()?;
    results.sort_unstable_by_key(|(index, _)| *index);
    guard.check()?;
    let mut cell_results = Vec::new();
    crate::memory::reserve_exact(&mut cell_results, results.len(), "cell results")?;
    for (_, result) in results {
        guard.check()?;
        cell_results.push(result?);
    }
    guard.check()?;
    validate_result_totals(
        graph,
        global_results
            .iter()
            .map(|global| &global.result)
            .chain(cell_results.iter()),
        guard,
    )?;
    let evaluated = GraphEvaluationJobResult {
        cells: cell_results,
        global_stages: global_results,
    };
    guard.check()?;
    #[cfg(test)]
    cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::BeforePublication,
        graph.limits.max_time_ms,
    )?;
    Ok(evaluated)
}

fn validate_result_totals<'a>(
    graph: &CompiledBiomeGraph,
    results: impl IntoIterator<Item = &'a GraphEvaluationResult>,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    let mut candidates = 0_u64;
    let mut accepted = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    for result in results {
        guard.check()?;
        candidates = candidates
            .checked_add(result.diagnostics.candidate_count)
            .ok_or(Error::NumericOverflow)?;
        accepted = accepted
            .checked_add(result.diagnostics.accepted_count)
            .ok_or(Error::NumericOverflow)?;
        for tile in &result.micro_fields {
            guard.check()?;
            micro_samples = micro_samples
                .checked_add(tile.density.len() as u64)
                .ok_or(Error::NumericOverflow)?;
        }
        for node in &result.diagnostics.nodes {
            guard.check()?;
            transfer_bytes = transfer_bytes
                .checked_add(node.transfer_bytes)
                .ok_or(Error::NumericOverflow)?;
        }
        for group in &result.diagnostics.gpu_groups {
            guard.check()?;
            transfer_bytes = transfer_bytes
                .checked_add(group.transfer_bytes)
                .ok_or(Error::NumericOverflow)?;
        }
    }
    check_limit("candidate count", candidates, graph.limits.max_candidates)?;
    check_limit("accepted count", accepted, graph.limits.max_macro_points)?;
    check_limit(
        "micro samples",
        micro_samples,
        graph.limits.max_micro_samples,
    )?;
    check_limit(
        "transfer bytes",
        transfer_bytes,
        graph.limits.max_transfer_bytes,
    )?;
    guard.check()
}
