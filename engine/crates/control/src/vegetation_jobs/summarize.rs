use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_protocol::{
    ControlFailureDto, VegetationEvaluationSummaryDto, VegetationGpuGroupEvaluationDiagnosticDto,
    VegetationNamedDiagnosticStreamDto, VegetationNodeEvaluationDiagnosticDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    CandidateRejectionReason, DiagnosticStreamScope, GraphEvaluationJobResult,
    GraphEvaluationResult, GraphExecutionDomain, GraphNodeAddress, GraphOperator,
    NamedDiagnosticStream,
};

use super::*;
use crate::Error;
use crate::owned_worker::{WorkerFailure, WorkerPoll};

pub(crate) fn matching_results(
    results: &GraphEvaluationJobResult,
    cell: WorldCellKey,
) -> impl Iterator<Item = &GraphEvaluationResult> {
    results
        .cells
        .iter()
        .filter(move |result| result.cell == cell)
        .chain(
            results
                .global_stages
                .iter()
                .filter(move |stage| stage.owner == cell)
                .map(|stage| &stage.result),
        )
}

pub(crate) fn refresh(job: &mut EvaluationJob) {
    let outcome = match &mut job.state {
        EvaluationJobState::Running { worker } => match worker.poll() {
            Ok(WorkerPoll::Complete(outcome)) => outcome,
            Ok(WorkerPoll::Pending) => return,
            Err(WorkerFailure::Disconnected | WorkerFailure::Panicked) => {
                job.state = EvaluationJobState::Failed(ControlFailureDto::Diagnostic {
                    message: "vegetation graph worker disconnected before publishing a result"
                        .to_owned(),
                    diagnostic: saffron_protocol::ControlDiagnosticDto::VegetationGraph(
                        saffron_protocol::VegetationGraphDiagnosticDto::WorkerPanicked,
                    ),
                });
                return;
            }
        },
        EvaluationJobState::Prepared { .. }
        | EvaluationJobState::Completed { .. }
        | EvaluationJobState::Cancelled
        | EvaluationJobState::Failed(_) => return,
    };
    match outcome {
        Ok(_results) if job.cancellation.is_cancelled() => {
            job.state = EvaluationJobState::Cancelled;
        }
        Ok(results) => match summarize(&results) {
            Ok((summary, canonical_bytes)) => {
                job.state = EvaluationJobState::Completed {
                    results: Arc::new(results),
                    summary: Box::new(summary),
                    canonical_bytes,
                };
            }
            Err(error) => {
                job.state = EvaluationJobState::Failed(Error::from(error).into_failure());
            }
        },
        Err(saffron_vegetation::Error::GraphCancelled) => {
            job.state = EvaluationJobState::Cancelled;
        }
        Err(error) => {
            job.state = EvaluationJobState::Failed(Error::from(error).into_failure());
        }
    }
}

pub(crate) fn finish_running_worker(
    job: &mut EvaluationJob,
) -> std::result::Result<(), WorkerFailure> {
    match &mut job.state {
        EvaluationJobState::Running { worker } => worker.finish(),
        EvaluationJobState::Prepared { .. }
        | EvaluationJobState::Completed { .. }
        | EvaluationJobState::Cancelled
        | EvaluationJobState::Failed(_) => Ok(()),
    }
}

impl DiagnosticStreamAggregate {
    fn new(
        source: &DiagnosticResultSource,
        stream: &NamedDiagnosticStream,
    ) -> saffron_vegetation::Result<Self> {
        let mut aggregate = Self {
            candidate_samples: stream.candidates.as_ref().map(|_| BTreeMap::new()),
            scalar_samples: stream.field.as_ref().map(|_| BTreeMap::new()),
            rejected: BTreeMap::new(),
        };
        aggregate.extend(source, stream)?;
        Ok(aggregate)
    }

    fn extend(
        &mut self,
        source: &DiagnosticResultSource,
        stream: &NamedDiagnosticStream,
    ) -> saffron_vegetation::Result<()> {
        match (&mut self.candidate_samples, &stream.candidates) {
            (Some(aggregate), Some(samples)) => {
                for sample in samples {
                    insert_identical_diagnostic(
                        aggregate,
                        (source.clone(), sample.identity),
                        sample.clone(),
                        &stream.label,
                    )?;
                }
            }
            (None, None) => {}
            _ => return Err(inconsistent_diagnostic_stream(&stream.label)),
        }
        match (&mut self.scalar_samples, &stream.field) {
            (Some(aggregate), Some(samples)) => {
                for sample in samples {
                    insert_identical_diagnostic(
                        aggregate,
                        (source.clone(), sample.candidate),
                        *sample,
                        &stream.label,
                    )?;
                }
            }
            (None, None) => {}
            _ => return Err(inconsistent_diagnostic_stream(&stream.label)),
        }
        for rejected in &stream.rejected {
            insert_identical_diagnostic(
                &mut self.rejected,
                (
                    source.clone(),
                    rejected.candidate,
                    rejection_reason_order(rejected.reason),
                    rejected.provenance.0,
                ),
                rejected.clone(),
                &stream.label,
            )?;
        }
        Ok(())
    }
}

pub(crate) fn insert_identical_diagnostic<K: Ord, V: PartialEq>(
    destination: &mut BTreeMap<K, V>,
    key: K,
    value: V,
    label: &str,
) -> saffron_vegetation::Result<()> {
    match destination.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &value => Ok(()),
        std::collections::btree_map::Entry::Occupied(_) => {
            Err(inconsistent_diagnostic_stream(label))
        }
    }
}

pub(crate) fn inconsistent_diagnostic_stream(label: &str) -> saffron_vegetation::Error {
    saffron_vegetation::Error::GraphDocument {
        path: format!("evaluation.diagnostics.streams.{label}"),
        reason: "retained samples disagree for the same canonical identity".to_owned(),
    }
}

pub(crate) fn rejection_reason_order(reason: CandidateRejectionReason) -> u8 {
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

pub(crate) fn summarize(
    results: &GraphEvaluationJobResult,
) -> saffron_vegetation::Result<(VegetationEvaluationSummaryDto, u64)> {
    let mut canonical = CanonicalJobDigest::new();
    canonical.update(b"saffron-anima/vegetation-evaluation-job/v2\0")?;
    let cell_count = u64::try_from(results.cells.len())
        .map_err(|_| saffron_vegetation::Error::NumericOverflow)?;
    canonical.update(&cell_count.to_be_bytes())?;
    for result in &results.cells {
        let encoded_len = u64::try_from(result.canonical_byte_len()?)
            .map_err(|_| saffron_vegetation::Error::NumericOverflow)?;
        canonical.update(&[0])?;
        canonical.update(&encoded_len.to_be_bytes())?;
        canonical.update_result(result, encoded_len)?;
    }
    let global_stage_count = u64::try_from(results.global_stages.len())
        .map_err(|_| saffron_vegetation::Error::NumericOverflow)?;
    canonical.update(&global_stage_count.to_be_bytes())?;
    for stage in &results.global_stages {
        let encoded_len = u64::try_from(stage.result.canonical_byte_len()?)
            .map_err(|_| saffron_vegetation::Error::NumericOverflow)?;
        canonical.update(&[1])?;
        canonical.update(&stage.stage)?;
        canonical.update(&stage.owner.canonical_bytes())?;
        canonical.update(&stage.resident_bytes.to_be_bytes())?;
        canonical.update(&encoded_len.to_be_bytes())?;
        canonical.update_result(&stage.result, encoded_len)?;
    }
    let (canonical_hash, canonical_bytes) = canonical.finalize()?;
    let mut candidates = 0_u64;
    let mut accepted = 0_u64;
    let mut micro_tiles = 0_u64;
    let mut rejected = 0_u64;
    let global_resident_bytes = results
        .global_stages
        .iter()
        .try_fold(0_u64, |total, stage| {
            total
                .checked_add(stage.resident_bytes)
                .ok_or(saffron_vegetation::Error::NumericOverflow)
        })?;
    let mut nodes = BTreeMap::<
        (Vec<u128>, u128, GraphOperator, String, GraphExecutionDomain),
        NodeAggregate,
    >::new();
    let mut gpu_groups = BTreeMap::new();
    let mut streams = BTreeMap::<
        (GraphNodeAddress, String, DiagnosticStreamScope),
        DiagnosticStreamAggregate,
    >::new();
    let scoped_results = results
        .cells
        .iter()
        .map(|result| (DiagnosticResultSource::Cell(result.cell), result))
        .chain(results.global_stages.iter().map(|stage| {
            (
                DiagnosticResultSource::GlobalStage {
                    stage: stage.stage,
                    owner: stage.owner,
                },
                &stage.result,
            )
        }));
    for (source, result) in scoped_results {
        candidates = candidates
            .checked_add(result.diagnostics.candidate_count)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        accepted = accepted
            .checked_add(result.diagnostics.accepted_count)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        micro_tiles = micro_tiles
            .checked_add(result.micro_fields.len() as u64)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        rejected = rejected
            .checked_add(result.diagnostics.rejected.len() as u64)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        for diagnostic in &result.diagnostics.nodes {
            let aggregate = nodes
                .entry((
                    diagnostic.module_path.clone(),
                    diagnostic.node,
                    diagnostic.operator,
                    diagnostic.symbol.clone(),
                    diagnostic.execution_domain,
                ))
                .or_default();
            aggregate.input_candidates = aggregate
                .input_candidates
                .checked_add(diagnostic.input_candidates)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.output_candidates = aggregate
                .output_candidates
                .checked_add(diagnostic.output_candidates)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.output_bytes = aggregate
                .output_bytes
                .checked_add(diagnostic.output_bytes)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.predicted_transfer_bytes = aggregate
                .predicted_transfer_bytes
                .checked_add(diagnostic.predicted_transfer_bytes)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.elapsed_micros = aggregate
                .elapsed_micros
                .checked_add(diagnostic.elapsed_micros)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        }
        for diagnostic in &result.diagnostics.gpu_groups {
            let aggregate = gpu_groups
                .entry(diagnostic.nodes.clone())
                .or_insert_with(GpuGroupAggregate::default);
            aggregate.invocation_count = aggregate
                .invocation_count
                .checked_add(diagnostic.invocation_count)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.output_bytes = aggregate
                .output_bytes
                .checked_add(diagnostic.output_bytes)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.transfer_bytes = aggregate
                .transfer_bytes
                .checked_add(diagnostic.transfer_bytes)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
            aggregate.elapsed_micros = aggregate
                .elapsed_micros
                .checked_add(diagnostic.elapsed_micros)
                .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        }
        for stream in &result.diagnostics.streams {
            let key = (stream.node.clone(), stream.label.clone(), stream.scope);
            match streams.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(DiagnosticStreamAggregate::new(&source, stream)?);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().extend(&source, stream)?;
                }
            }
        }
    }
    Ok((
        VegetationEvaluationSummaryDto {
            cells: results.cells.len().to_string(),
            global_stages: results.global_stages.len().to_string(),
            global_resident_bytes: global_resident_bytes.to_string(),
            candidates: candidates.to_string(),
            accepted: accepted.to_string(),
            micro_tiles: micro_tiles.to_string(),
            rejected: rejected.to_string(),
            canonical_hash: hex_hash(canonical_hash),
            nodes: nodes
                .into_iter()
                .map(
                    |((module_path, node, operator, symbol, execution_domain), aggregate)| {
                        VegetationNodeEvaluationDiagnosticDto {
                            module_path: module_path
                                .into_iter()
                                .map(crate::commands_vegetation::vegetation_guid)
                                .collect(),
                            node: crate::commands_vegetation::vegetation_guid(node),
                            operator: crate::commands_vegetation::graph_operator_dto(operator),
                            symbol,
                            input_candidates: aggregate.input_candidates.to_string(),
                            output_candidates: aggregate.output_candidates.to_string(),
                            output_bytes: aggregate.output_bytes.to_string(),
                            predicted_transfer_bytes: aggregate
                                .predicted_transfer_bytes
                                .to_string(),
                            elapsed_micros: aggregate.elapsed_micros.to_string(),
                            execution_domain: execution_domain_dto(execution_domain),
                        }
                    },
                )
                .collect(),
            gpu_groups: gpu_groups
                .into_iter()
                .map(
                    |(nodes, aggregate)| VegetationGpuGroupEvaluationDiagnosticDto {
                        nodes: nodes.into_iter().map(graph_node_address_dto).collect(),
                        invocation_count: aggregate.invocation_count.to_string(),
                        output_bytes: aggregate.output_bytes.to_string(),
                        transfer_bytes: aggregate.transfer_bytes.to_string(),
                        elapsed_micros: aggregate.elapsed_micros.to_string(),
                    },
                )
                .collect(),
            streams: streams
                .into_iter()
                .map(
                    |((node, label, scope), aggregate)| VegetationNamedDiagnosticStreamDto {
                        node: graph_node_address_dto(node),
                        label,
                        scope: diagnostic_stream_scope_dto(scope),
                        candidate_samples: aggregate.candidate_samples.map(|samples| {
                            samples
                                .into_iter()
                                .map(|((source, _), sample)| {
                                    diagnostic_candidate_sample_dto(source, sample)
                                })
                                .collect()
                        }),
                        scalar_samples: aggregate.scalar_samples.map(|samples| {
                            samples
                                .into_iter()
                                .map(|((source, _), sample)| {
                                    diagnostic_scalar_sample_dto(source, sample)
                                })
                                .collect()
                        }),
                        rejected: aggregate
                            .rejected
                            .into_iter()
                            .map(|((source, _, _, _), rejected)| {
                                diagnostic_rejection_dto(source, rejected)
                            })
                            .collect(),
                    },
                )
                .collect(),
        },
        canonical_bytes,
    ))
}
