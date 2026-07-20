//! Owned asynchronous vegetation evaluation jobs and retained provenance results.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use saffron_protocol::{
    Uuid as WireUuid, VegetationCandidateIdentityDto, VegetationDiagnosticCandidateSampleDto,
    VegetationDiagnosticProvenanceIdDto, VegetationDiagnosticRejectionDto,
    VegetationDiagnosticResultSourceDto, VegetationDiagnosticScalarSampleDto,
    VegetationDiagnosticStreamScopeDto, VegetationEvaluationJobDto,
    VegetationEvaluationJobStateDto, VegetationEvaluationStatusDto, VegetationEvaluationSummaryDto,
    VegetationExecutionDomainDto, VegetationGpuGroupEvaluationDiagnosticDto,
    VegetationGraphNodeAddressDto, VegetationNamedDiagnosticStreamDto,
    VegetationNodeEvaluationDiagnosticDto, WorldCellDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    BiomeGraphEvaluator, CandidateIdentity, CandidateRejectionReason, DiagnosticCandidateSample,
    DiagnosticScalarSample, DiagnosticStreamScope, GraphCancellationToken,
    GraphEvaluationJobInputs, GraphEvaluationJobResult, GraphEvaluationResult,
    GraphExecutionDomain, GraphNodeAddress, GraphOperator, NamedDiagnosticStream, PlantId,
    ProvenanceExplanation, RejectedCandidate, vegetation_content_hash,
};

use crate::{Error, Result};

#[derive(Default)]
struct NodeAggregate {
    input_candidates: u64,
    output_candidates: u64,
    output_bytes: u64,
    predicted_transfer_bytes: u64,
    elapsed_micros: u64,
}

#[derive(Default)]
struct GpuGroupAggregate {
    invocation_count: u64,
    output_bytes: u64,
    transfer_bytes: u64,
    elapsed_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DiagnosticResultSource {
    Cell(WorldCellKey),
    GlobalStage {
        stage: [u8; 32],
        owner: WorldCellKey,
    },
}

struct DiagnosticStreamAggregate {
    candidate_samples:
        Option<BTreeMap<(DiagnosticResultSource, CandidateIdentity), DiagnosticCandidateSample>>,
    scalar_samples:
        Option<BTreeMap<(DiagnosticResultSource, CandidateIdentity), DiagnosticScalarSample>>,
    rejected: BTreeMap<(DiagnosticResultSource, CandidateIdentity, u8, u32), RejectedCandidate>,
}

type EvaluationOutcome = saffron_vegetation::Result<GraphEvaluationJobResult>;

const MAX_ACTIVE_JOBS: usize = 16;
const MAX_TERMINAL_JOBS: usize = 128;
const MAX_RETAINED_CANONICAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

enum JobTerminal {
    Running,
    Completed {
        results: Arc<GraphEvaluationJobResult>,
        summary: Box<VegetationEvaluationSummaryDto>,
        canonical_bytes: u64,
    },
    Cancelled,
    Failed(String),
}

struct EvaluationJob {
    cancellation: GraphCancellationToken,
    receiver: Option<Receiver<EvaluationOutcome>>,
    worker: Option<JoinHandle<()>>,
    terminal: JobTerminal,
}

/// Owns every bounded evaluator worker and its atomically published result.
pub(crate) struct VegetationEvaluationJobs {
    next_job: u64,
    jobs: BTreeMap<u64, EvaluationJob>,
}

impl Default for VegetationEvaluationJobs {
    fn default() -> Self {
        Self {
            next_job: 1,
            jobs: BTreeMap::new(),
        }
    }
}

impl VegetationEvaluationJobs {
    /// Starts one worker whose result becomes visible only when every cell completes.
    pub(crate) fn start(
        &mut self,
        evaluator: BiomeGraphEvaluator,
        inputs: GraphEvaluationJobInputs,
    ) -> Result<VegetationEvaluationJobDto> {
        self.maintain(None)?;
        let active = self
            .jobs
            .values()
            .filter(|job| matches!(job.terminal, JobTerminal::Running))
            .count();
        if active >= MAX_ACTIVE_JOBS {
            return Err(Error::command(format!(
                "vegetation evaluation active-job limit exceeded: requested {}, limit {}",
                active + 1,
                MAX_ACTIVE_JOBS
            )));
        }
        let job_id = self.next_job;
        let next_job = job_id
            .checked_add(1)
            .ok_or_else(|| Error::command("vegetation evaluation job identity overflowed"))?;
        let cell_count = u64::try_from(inputs.cells.len())
            .map_err(|_| Error::command("vegetation evaluation cell count overflowed"))?;
        let cancellation = GraphCancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(format!("saffron-vegetation-{job_id}"))
            .spawn(move || {
                let outcome = evaluator.evaluate(inputs, &worker_cancellation);
                let _ = sender.send(outcome);
            })
            .map_err(|error| {
                Error::command(format!("could not start vegetation evaluation: {error}"))
            })?;
        self.next_job = next_job;
        self.jobs.insert(
            job_id,
            EvaluationJob {
                cancellation,
                receiver: Some(receiver),
                worker: Some(worker),
                terminal: JobTerminal::Running,
            },
        );
        Ok(VegetationEvaluationJobDto {
            job: job_id.to_string(),
            state: VegetationEvaluationJobStateDto::Running,
            cells: cell_count.to_string(),
        })
    }

    /// Cancels and joins every worker during control-plane shutdown.
    pub(crate) fn shutdown(&mut self) {
        for job in self.jobs.values() {
            job.cancellation.cancel();
        }
        for job in self.jobs.values_mut() {
            let _ = finish_worker(job);
            refresh(job);
        }
    }

    /// Returns the current state, polling the worker without blocking the control thread.
    pub(crate) fn status(&mut self, job_id: u64) -> Result<VegetationEvaluationStatusDto> {
        self.maintain(Some(job_id))?;
        let job = self.job_mut(job_id)?;
        Ok(status_dto(job_id, job))
    }

    /// Requests cooperative cancellation without publishing a partial result.
    pub(crate) fn cancel(&mut self, job_id: u64) -> Result<VegetationEvaluationStatusDto> {
        self.maintain(Some(job_id))?;
        let job = self.job_mut(job_id)?;
        if matches!(&job.terminal, JobTerminal::Running) {
            job.cancellation.cancel();
        }
        Ok(status_dto(job_id, job))
    }

    /// Explains one accepted plant retained by a completed job.
    pub(crate) fn explain_plant(
        &mut self,
        job_id: u64,
        cell: WorldCellKey,
        plant: PlantId,
    ) -> Result<ProvenanceExplanation> {
        let results = self.completed_results(job_id)?;
        let matches = matching_results(results, cell)
            .filter_map(|result| result.explain_plant(plant).ok())
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [explanation] => Ok(explanation.clone()),
            [] => Err(Error::command(
                "accepted plant is not present in the selected completed cell or global stage",
            )),
            _ => Err(Error::command(
                "accepted plant is duplicated across completed evaluation products",
            )),
        }
    }

    /// Explains one rejected candidate retained by a completed job.
    pub(crate) fn explain_rejection(
        &mut self,
        job_id: u64,
        cell: WorldCellKey,
        candidate: CandidateIdentity,
    ) -> Result<ProvenanceExplanation> {
        let results = self.completed_results(job_id)?;
        let matches = matching_results(results, cell)
            .filter_map(|result| result.explain_rejection(candidate).ok())
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [explanation] => Ok(explanation.clone()),
            [] => Err(Error::command(
                "rejected candidate is not present in the selected completed cell or global stage",
            )),
            _ => Err(Error::command(
                "rejected candidate is duplicated across completed evaluation products",
            )),
        }
    }

    fn completed_results(&mut self, job_id: u64) -> Result<&GraphEvaluationJobResult> {
        self.maintain(Some(job_id))?;
        let job = self.job_mut(job_id)?;
        match &job.terminal {
            JobTerminal::Completed { results, .. } => Ok(results),
            JobTerminal::Running => Err(Error::command("vegetation evaluation is still running")),
            JobTerminal::Cancelled => Err(Error::command("vegetation evaluation was cancelled")),
            JobTerminal::Failed(error) => Err(Error::command(format!(
                "vegetation evaluation failed: {error}"
            ))),
        }
    }

    fn job_mut(&mut self, job_id: u64) -> Result<&mut EvaluationJob> {
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| Error::command(format!("unknown vegetation evaluation job {job_id}")))
    }

    fn maintain(&mut self, protected: Option<u64>) -> Result<()> {
        for job in self.jobs.values_mut() {
            refresh(job);
        }
        loop {
            let terminal_count = self
                .jobs
                .values()
                .filter(|job| !matches!(job.terminal, JobTerminal::Running))
                .count();
            let retained_bytes = self.jobs.values().try_fold(0_u64, |total, job| {
                let bytes = match &job.terminal {
                    JobTerminal::Completed {
                        canonical_bytes, ..
                    } => *canonical_bytes,
                    _ => 0,
                };
                total
                    .checked_add(bytes)
                    .ok_or_else(|| Error::command("retained vegetation result size overflowed"))
            })?;
            if terminal_count <= MAX_TERMINAL_JOBS && retained_bytes <= MAX_RETAINED_CANONICAL_BYTES
            {
                return Ok(());
            }
            let evict = self
                .jobs
                .iter()
                .find_map(|(job_id, job)| {
                    (Some(*job_id) != protected && !matches!(job.terminal, JobTerminal::Running))
                        .then_some(*job_id)
                })
                .or_else(|| {
                    protected.filter(|job_id| {
                        self.jobs
                            .get(job_id)
                            .is_some_and(|job| !matches!(job.terminal, JobTerminal::Running))
                    })
                })
                .ok_or_else(|| Error::command("vegetation job retention limit cannot be met"))?;
            self.jobs.remove(&evict);
            if Some(evict) == protected {
                return Err(Error::command(format!(
                    "vegetation evaluation job {evict} exceeded the retained-result limit"
                )));
            }
        }
    }
}

impl Drop for VegetationEvaluationJobs {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancellation.cancel();
        }
        for job in self.jobs.values_mut() {
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn matching_results(
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

fn refresh(job: &mut EvaluationJob) {
    if !matches!(&job.terminal, JobTerminal::Running) {
        return;
    }
    let outcome = match job.receiver.as_ref().map(Receiver::try_recv) {
        Some(Ok(outcome)) => Some(outcome),
        Some(Err(TryRecvError::Empty)) => return,
        Some(Err(TryRecvError::Disconnected)) | None => {
            let _ = finish_worker(job);
            job.terminal = JobTerminal::Failed(
                "evaluation worker disconnected before publishing a result".to_owned(),
            );
            job.receiver = None;
            return;
        }
    };
    job.receiver = None;
    if finish_worker(job).is_err() {
        job.terminal = JobTerminal::Failed("evaluation worker panicked".to_owned());
        return;
    }
    match outcome {
        Some(Ok(_results)) if job.cancellation.is_cancelled() => {
            job.terminal = JobTerminal::Cancelled;
        }
        Some(Ok(results)) => match summarize(&results) {
            Ok((summary, canonical_bytes)) => {
                job.terminal = JobTerminal::Completed {
                    results: Arc::new(results),
                    summary: Box::new(summary),
                    canonical_bytes,
                };
            }
            Err(error) => job.terminal = JobTerminal::Failed(error.to_string()),
        },
        Some(Err(saffron_vegetation::Error::GraphCancelled)) => {
            job.terminal = JobTerminal::Cancelled;
        }
        Some(Err(error)) => job.terminal = JobTerminal::Failed(error.to_string()),
        None => {}
    }
}

fn finish_worker(job: &mut EvaluationJob) -> std::thread::Result<()> {
    match job.worker.take() {
        Some(worker) => worker.join(),
        None => Ok(()),
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

fn insert_identical_diagnostic<K: Ord, V: PartialEq>(
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

fn inconsistent_diagnostic_stream(label: &str) -> saffron_vegetation::Error {
    saffron_vegetation::Error::GraphDocument {
        path: format!("evaluation.diagnostics.streams.{label}"),
        reason: "retained samples disagree for the same canonical identity".to_owned(),
    }
}

fn rejection_reason_order(reason: CandidateRejectionReason) -> u8 {
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

fn summarize(
    results: &GraphEvaluationJobResult,
) -> saffron_vegetation::Result<(VegetationEvaluationSummaryDto, u64)> {
    let mut canonical = b"saffron-anima/vegetation-evaluation-job/v2\0".to_vec();
    canonical.extend_from_slice(&(results.cells.len() as u64).to_be_bytes());
    for result in &results.cells {
        let bytes = result.canonical_bytes()?;
        canonical.push(0);
        canonical.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        canonical.extend_from_slice(&bytes);
    }
    canonical.extend_from_slice(&(results.global_stages.len() as u64).to_be_bytes());
    for stage in &results.global_stages {
        let bytes = stage.result.canonical_bytes()?;
        canonical.push(1);
        canonical.extend_from_slice(&stage.stage);
        canonical.extend_from_slice(&stage.owner.canonical_bytes());
        canonical.extend_from_slice(&stage.resident_bytes.to_be_bytes());
        canonical.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        canonical.extend_from_slice(&bytes);
    }
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
            canonical_hash: hex_hash(vegetation_content_hash(&canonical)),
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
        u64::try_from(canonical.len()).map_err(|_| saffron_vegetation::Error::NumericOverflow)?,
    ))
}

fn graph_node_address_dto(address: GraphNodeAddress) -> VegetationGraphNodeAddressDto {
    VegetationGraphNodeAddressDto {
        module_path: address
            .module_path
            .into_iter()
            .map(crate::commands_vegetation::vegetation_guid)
            .collect(),
        node: crate::commands_vegetation::vegetation_guid(address.node),
    }
}

fn diagnostic_stream_scope_dto(scope: DiagnosticStreamScope) -> VegetationDiagnosticStreamScopeDto {
    match scope {
        DiagnosticStreamScope::GlobalSnapshot => VegetationDiagnosticStreamScopeDto::GlobalSnapshot,
        DiagnosticStreamScope::CandidateLineage(lineage) => {
            VegetationDiagnosticStreamScopeDto::CandidateLineage {
                lineage: crate::commands_vegetation::vegetation_guid(lineage.0),
            }
        }
    }
}

fn diagnostic_source_dto(source: DiagnosticResultSource) -> VegetationDiagnosticResultSourceDto {
    match source {
        DiagnosticResultSource::Cell(cell) => VegetationDiagnosticResultSourceDto::Cell {
            cell: world_cell_dto(cell),
        },
        DiagnosticResultSource::GlobalStage { stage, owner } => {
            VegetationDiagnosticResultSourceDto::GlobalStage {
                stage: hex_hash(stage),
                owner: world_cell_dto(owner),
            }
        }
    }
}

fn diagnostic_candidate_sample_dto(
    source: DiagnosticResultSource,
    sample: DiagnosticCandidateSample,
) -> VegetationDiagnosticCandidateSampleDto {
    VegetationDiagnosticCandidateSampleDto {
        source: diagnostic_source_dto(source),
        identity: candidate_identity_dto(sample.identity),
        owner: world_cell_dto(sample.owner),
        position_ticks: sample.position.global_ticks().map(|tick| tick.to_string()),
        family: sample.family.map(WireUuid::from),
        variation: sample.variation,
        priority_bits: sample.priority.bits(),
        ecology_tick: sample.ecology_tick.to_string(),
    }
}

fn diagnostic_scalar_sample_dto(
    source: DiagnosticResultSource,
    sample: DiagnosticScalarSample,
) -> VegetationDiagnosticScalarSampleDto {
    VegetationDiagnosticScalarSampleDto {
        source: diagnostic_source_dto(source),
        candidate: candidate_identity_dto(sample.candidate),
        value_bits: sample.value.bits(),
    }
}

fn diagnostic_rejection_dto(
    source: DiagnosticResultSource,
    rejected: RejectedCandidate,
) -> VegetationDiagnosticRejectionDto {
    VegetationDiagnosticRejectionDto {
        candidate: candidate_identity_dto(rejected.candidate),
        reason: crate::commands_vegetation::candidate_rejection_reason_dto(rejected.reason),
        provenance: VegetationDiagnosticProvenanceIdDto {
            source: diagnostic_source_dto(source),
            handle: rejected.provenance.0,
        },
    }
}

fn candidate_identity_dto(identity: CandidateIdentity) -> VegetationCandidateIdentityDto {
    VegetationCandidateIdentityDto {
        node: crate::commands_vegetation::vegetation_guid(identity.node),
        node_address: crate::commands_vegetation::vegetation_guid(identity.node_address),
        node_semantic_revision: identity.node_semantic_revision,
        ordinal: identity.ordinal.to_string(),
        ancestor: identity.ancestor.to_string(),
    }
}

fn world_cell_dto(cell: WorldCellKey) -> WorldCellDto {
    WorldCellDto {
        coordinates: cell.coordinates().map(|coordinate| coordinate.to_string()),
        level: cell.level(),
    }
}

fn execution_domain_dto(domain: GraphExecutionDomain) -> VegetationExecutionDomainDto {
    match domain {
        GraphExecutionDomain::ReferenceCpu => VegetationExecutionDomainDto::ReferenceCpu,
        GraphExecutionDomain::ParallelCpu => VegetationExecutionDomainDto::ParallelCpu,
        GraphExecutionDomain::SlangCompute => VegetationExecutionDomainDto::SlangCompute,
    }
}

fn status_dto(job_id: u64, job: &EvaluationJob) -> VegetationEvaluationStatusDto {
    let (state, summary, error) = match &job.terminal {
        JobTerminal::Running => (VegetationEvaluationJobStateDto::Running, None, None),
        JobTerminal::Completed { summary, .. } => (
            VegetationEvaluationJobStateDto::Completed,
            Some((**summary).clone()),
            None,
        ),
        JobTerminal::Cancelled => (VegetationEvaluationJobStateDto::Cancelled, None, None),
        JobTerminal::Failed(error) => (
            VegetationEvaluationJobStateDto::Failed,
            None,
            Some(error.clone()),
        ),
    };
    VegetationEvaluationStatusDto {
        job: job_id.to_string(),
        state,
        summary,
        error,
    }
}

fn hex_hash(hash: [u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_spatial::{DecisionScalar, WorldPosition};
    use saffron_vegetation::{
        CandidateLineage, DiagnosticCandidateSample, DiagnosticScalarSample,
        GlobalStageEvaluationResult, GpuGroupEvaluationDiagnostic, GraphEvaluationDiagnostics,
        GraphNodeAddress, NamedDiagnosticStream, PlantPointColumns, ProvenanceHandle,
        ProvenanceTable, RejectedCandidate,
    };

    fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
        GraphEvaluationResult {
            cell,
            macro_points: PlantPointColumns::from_points(&[]).unwrap(),
            micro_fields: Vec::new(),
            surface_projection_tiles: Vec::new(),
            surface_field_query_tiles: Vec::new(),
            ancestor_references: Vec::new(),
            provenance: ProvenanceTable::default(),
            diagnostics: GraphEvaluationDiagnostics::default(),
        }
    }

    fn running_job_with_outcome(
        cancellation: GraphCancellationToken,
        outcome: EvaluationOutcome,
    ) -> EvaluationJob {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(outcome).unwrap();
        EvaluationJob {
            cancellation,
            receiver: Some(receiver),
            worker: None,
            terminal: JobTerminal::Running,
        }
    }

    fn failed_job(error: impl Into<String>) -> EvaluationJob {
        EvaluationJob {
            cancellation: GraphCancellationToken::default(),
            receiver: None,
            worker: None,
            terminal: JobTerminal::Failed(error.into()),
        }
    }

    fn completed_job(canonical_bytes: u64) -> EvaluationJob {
        let results = GraphEvaluationJobResult::default();
        let (summary, measured_bytes) = summarize(&results).unwrap();
        assert!(measured_bytes > 0);
        EvaluationJob {
            cancellation: GraphCancellationToken::default(),
            receiver: None,
            worker: None,
            terminal: JobTerminal::Completed {
                results: Arc::new(results),
                summary: Box::new(summary),
                canonical_bytes,
            },
        }
    }

    #[test]
    fn terminal_count_retention_evicts_the_oldest_unprotected_job() {
        let mut jobs = VegetationEvaluationJobs::default();
        let protected = u64::try_from(MAX_TERMINAL_JOBS).unwrap() + 1;
        for job_id in 1..=protected {
            jobs.jobs
                .insert(job_id, failed_job(format!("job {job_id}")));
        }

        jobs.maintain(Some(protected)).unwrap();

        assert_eq!(jobs.jobs.len(), MAX_TERMINAL_JOBS);
        assert!(!jobs.jobs.contains_key(&1));
        assert!(jobs.jobs.contains_key(&protected));
    }

    #[test]
    fn retained_result_cap_evicts_an_older_completed_result() {
        let mut jobs = VegetationEvaluationJobs::default();
        jobs.jobs
            .insert(1, completed_job(MAX_RETAINED_CANONICAL_BYTES));
        jobs.jobs.insert(2, completed_job(1));

        jobs.maintain(Some(2)).unwrap();

        assert!(!jobs.jobs.contains_key(&1));
        assert!(jobs.jobs.contains_key(&2));
    }

    #[test]
    fn cancellation_requested_before_refresh_wins_a_queued_success() {
        let cancellation = GraphCancellationToken::default();
        let mut job = running_job_with_outcome(
            cancellation.clone(),
            Ok(GraphEvaluationJobResult::default()),
        );
        cancellation.cancel();

        refresh(&mut job);

        assert!(matches!(&job.terminal, JobTerminal::Cancelled));
    }

    #[test]
    fn completed_result_observed_before_cancel_remains_completed() {
        let mut jobs = VegetationEvaluationJobs::default();
        jobs.jobs.insert(
            1,
            running_job_with_outcome(
                GraphCancellationToken::default(),
                Ok(GraphEvaluationJobResult::default()),
            ),
        );

        let status = jobs.cancel(1).unwrap();

        assert_eq!(status.state, VegetationEvaluationJobStateDto::Completed);
        assert!(matches!(
            &jobs.jobs.get(&1).unwrap().terminal,
            JobTerminal::Completed { .. }
        ));
    }

    #[test]
    fn summary_frames_global_stage_identity_and_resident_bytes() {
        let owner = WorldCellKey::base(0, 0, 0);
        let results = GraphEvaluationJobResult {
            cells: vec![empty_result(owner)],
            global_stages: vec![GlobalStageEvaluationResult {
                stage: [7; 32],
                owner,
                result: empty_result(owner),
                resident_bytes: 4096,
            }],
        };
        let (summary, retained) = summarize(&results).unwrap();
        assert_eq!(summary.cells, "1");
        assert_eq!(summary.global_stages, "1");
        assert_eq!(summary.global_resident_bytes, "4096");
        assert!(retained > 0);

        let mut changed = results;
        changed.global_stages[0].stage[0] ^= 1;
        let (changed_summary, _) = summarize(&changed).unwrap();
        assert_ne!(summary.canonical_hash, changed_summary.canonical_hash);
    }

    #[test]
    fn summary_aggregates_actual_gpu_work_by_canonical_group_boundary() {
        let owner = WorldCellKey::base(0, 0, 0);
        let nodes = vec![
            GraphNodeAddress {
                module_path: vec![1],
                node: 2,
            },
            GraphNodeAddress {
                module_path: vec![1],
                node: 3,
            },
        ];
        let mut cell = empty_result(owner);
        cell.diagnostics
            .gpu_groups
            .push(GpuGroupEvaluationDiagnostic {
                nodes: nodes.clone(),
                invocation_count: 10,
                transfer_bytes: 100,
                output_bytes: 80,
                elapsed_micros: 7,
            });
        let mut global = empty_result(owner);
        global
            .diagnostics
            .gpu_groups
            .push(GpuGroupEvaluationDiagnostic {
                nodes,
                invocation_count: 20,
                transfer_bytes: 200,
                output_bytes: 160,
                elapsed_micros: 11,
            });
        let results = GraphEvaluationJobResult {
            cells: vec![cell],
            global_stages: vec![GlobalStageEvaluationResult {
                stage: [7; 32],
                owner,
                result: global,
                resident_bytes: 4096,
            }],
        };

        let (summary, _) = summarize(&results).unwrap();

        assert_eq!(summary.gpu_groups.len(), 1);
        let group = &summary.gpu_groups[0];
        assert_eq!(group.invocation_count, "30");
        assert_eq!(group.transfer_bytes, "300");
        assert_eq!(group.output_bytes, "240");
        assert_eq!(group.elapsed_micros, "18");
        assert_eq!(group.nodes.len(), 2);
        assert_eq!(group.nodes[0].module_path[0].0, format!("{:032x}", 1));
        assert_eq!(group.nodes[0].node.0, format!("{:032x}", 2));
        assert_eq!(group.nodes[1].node.0, format!("{:032x}", 3));
    }

    #[test]
    fn summary_merges_named_streams_with_exact_result_scopes() {
        let owner = WorldCellKey::base(0, 0, 0);
        let node = GraphNodeAddress {
            module_path: vec![1],
            node: 2,
        };
        let stream =
            |identity: CandidateIdentity, value: i32, provenance: u32| NamedDiagnosticStream {
                node: node.clone(),
                label: "density audit".to_owned(),
                scope: DiagnosticStreamScope::CandidateLineage(CandidateLineage(9)),
                candidates: Some(vec![DiagnosticCandidateSample {
                    identity,
                    owner,
                    position: WorldPosition::from_global_ticks([i128::from(value), 2, 3]).unwrap(),
                    family: None,
                    variation: 4,
                    priority: DecisionScalar::from_bits(value),
                    ecology_tick: 5,
                }]),
                field: Some(vec![DiagnosticScalarSample {
                    candidate: identity,
                    value: DecisionScalar::from_bits(value),
                }]),
                rejected: vec![RejectedCandidate {
                    candidate: identity,
                    reason: CandidateRejectionReason::Threshold,
                    provenance: ProvenanceHandle(provenance),
                }],
            };
        let first_identity = CandidateIdentity {
            node: 10,
            node_address: 11,
            node_semantic_revision: 1,
            ordinal: 12,
            ancestor: 13,
        };
        let second_identity = CandidateIdentity {
            ordinal: 14,
            ..first_identity
        };
        let mut cell = empty_result(owner);
        cell.diagnostics.streams.push(stream(first_identity, 16, 7));
        let mut global = empty_result(owner);
        global
            .diagnostics
            .streams
            .push(stream(second_identity, 32, 8));
        let results = GraphEvaluationJobResult {
            cells: vec![cell],
            global_stages: vec![GlobalStageEvaluationResult {
                stage: [7; 32],
                owner,
                result: global,
                resident_bytes: 4096,
            }],
        };

        let (summary, _) = summarize(&results).unwrap();

        assert_eq!(summary.streams.len(), 1);
        let stream = &summary.streams[0];
        assert_eq!(stream.label, "density audit");
        assert_eq!(stream.node.node.0, format!("{:032x}", 2));
        assert_eq!(
            stream.scope,
            VegetationDiagnosticStreamScopeDto::CandidateLineage {
                lineage: crate::commands_vegetation::vegetation_guid(9),
            }
        );
        assert_eq!(stream.candidate_samples.as_ref().unwrap().len(), 2);
        assert_eq!(stream.scalar_samples.as_ref().unwrap().len(), 2);
        assert_eq!(stream.rejected.len(), 2);
        assert!(matches!(
            stream.candidate_samples.as_ref().unwrap()[0].source,
            VegetationDiagnosticResultSourceDto::Cell { .. }
        ));
        assert!(matches!(
            stream.candidate_samples.as_ref().unwrap()[1].source,
            VegetationDiagnosticResultSourceDto::GlobalStage { .. }
        ));
        assert_eq!(stream.rejected[0].provenance.handle, 7);
        assert_eq!(stream.rejected[1].provenance.handle, 8);
    }
}
