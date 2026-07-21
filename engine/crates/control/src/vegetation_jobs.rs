//! Owned asynchronous vegetation evaluation jobs and retained provenance results.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use saffron_protocol::{
    ControlFailureDto, Uuid as WireUuid, VegetationCandidateIdentityDto,
    VegetationDiagnosticCandidateSampleDto, VegetationDiagnosticProvenanceIdDto,
    VegetationDiagnosticRejectionDto, VegetationDiagnosticResultSourceDto,
    VegetationDiagnosticScalarSampleDto, VegetationDiagnosticStreamScopeDto,
    VegetationEvaluationJobDto, VegetationEvaluationJobStateDto, VegetationEvaluationPreflightDto,
    VegetationEvaluationStatusDto, VegetationEvaluationSummaryDto, VegetationExecutionDomainDto,
    VegetationGpuGroupEvaluationDiagnosticDto, VegetationGraphNodeAddressDto,
    VegetationNamedDiagnosticStreamDto, VegetationNodeEvaluationDiagnosticDto, WorldCellDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    BiomeGraphEvaluator, CandidateIdentity, CandidateRejectionReason, DiagnosticCandidateSample,
    DiagnosticScalarSample, DiagnosticStreamScope, GraphCancellationToken,
    GraphEvaluationJobInputs, GraphEvaluationJobResult, GraphEvaluationPreflight,
    GraphEvaluationResult, GraphExecutionDomain, GraphNodeAddress, GraphOperator,
    NamedDiagnosticStream, PlantId, ProvenanceExplanation, RejectedCandidate,
    VegetationContentHasher,
};

use crate::owned_worker::{OwnedWorker, WorkerFailure, WorkerPoll};
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

const MAX_LIVE_JOBS: usize = 16;
const MAX_TERMINAL_JOBS: usize = 128;
const MAX_RETAINED_PREPARED_INPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_RETAINED_CANONICAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

enum EvaluationJobState {
    Prepared {
        evaluator: BiomeGraphEvaluator,
        inputs: GraphEvaluationJobInputs,
    },
    Running {
        worker: OwnedWorker<EvaluationOutcome>,
    },
    Completed {
        results: Arc<GraphEvaluationJobResult>,
        summary: Box<VegetationEvaluationSummaryDto>,
        canonical_bytes: u64,
    },
    Cancelled,
    Failed(ControlFailureDto),
}

impl EvaluationJobState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled | Self::Failed(_)
        )
    }
}

struct EvaluationJob {
    cancellation: GraphCancellationToken,
    preflight: GraphEvaluationPreflight,
    state: EvaluationJobState,
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
    /// Retains one exact preflighted job without starting an evaluation worker.
    pub(crate) fn prepare(
        &mut self,
        evaluator: BiomeGraphEvaluator,
        inputs: GraphEvaluationJobInputs,
    ) -> Result<VegetationEvaluationJobDto> {
        self.maintain(None)?;
        let live = self
            .jobs
            .values()
            .filter(|job| {
                matches!(
                    job.state,
                    EvaluationJobState::Prepared { .. } | EvaluationJobState::Running { .. }
                )
            })
            .count();
        if live >= MAX_LIVE_JOBS {
            return Err(Error::command(format!(
                "vegetation evaluation live-job limit exceeded: requested {}, limit {}",
                live + 1,
                MAX_LIVE_JOBS
            )));
        }
        let job_id = self.next_job;
        let next_job = job_id
            .checked_add(1)
            .ok_or_else(|| Error::command("vegetation evaluation job identity overflowed"))?;
        let cancellation = GraphCancellationToken::default();
        let preflight = evaluator
            .preflight(&inputs, &cancellation)
            .map_err(Error::from)?;
        let retained_prepared_bytes = self
            .jobs
            .values()
            .filter(|job| matches!(job.state, EvaluationJobState::Prepared { .. }))
            .try_fold(preflight.retained_input_bytes, |total, job| {
                total
                    .checked_add(job.preflight.retained_input_bytes)
                    .ok_or_else(|| {
                        Error::command("retained prepared vegetation input size overflowed")
                    })
            })?;
        if retained_prepared_bytes > MAX_RETAINED_PREPARED_INPUT_BYTES {
            return Err(Error::command(format!(
                "vegetation evaluation prepared-input limit exceeded: requested {retained_prepared_bytes}, limit {MAX_RETAINED_PREPARED_INPUT_BYTES}"
            )));
        }
        let job = EvaluationJob {
            cancellation,
            preflight,
            state: EvaluationJobState::Prepared { evaluator, inputs },
        };
        let dto = job_dto(job_id, &job);
        self.next_job = next_job;
        self.jobs.insert(job_id, job);
        Ok(dto)
    }

    /// Starts the exact evaluator and inputs retained by one prepared job.
    pub(crate) fn start(&mut self, job_id: u64) -> Result<VegetationEvaluationJobDto> {
        self.maintain(Some(job_id))?;
        let job = self.job_mut(job_id)?;
        let state = std::mem::replace(
            &mut job.state,
            EvaluationJobState::Failed(ControlFailureDto::Command {
                message: "evaluation worker did not start".to_owned(),
            }),
        );
        let EvaluationJobState::Prepared { evaluator, inputs } = state else {
            job.state = state;
            return Err(Error::command(format!(
                "vegetation evaluation job {job_id} is not prepared"
            )));
        };
        let worker_cancellation = job.cancellation.clone();
        let worker = match OwnedWorker::spawn(format!("saffron-vegetation-{job_id}"), move || {
            evaluator.evaluate(inputs, &worker_cancellation)
        }) {
            Ok(worker) => worker,
            Err(error) => {
                let failure =
                    Error::from(saffron_vegetation::Error::GraphWorkerSpawn { source: error })
                        .into_failure();
                job.state = EvaluationJobState::Failed(failure.clone());
                return Err(Error::Failure(Box::new(failure)));
            }
        };
        job.state = EvaluationJobState::Running { worker };
        Ok(job_dto(job_id, job))
    }

    /// Cancels and joins every worker during control-plane shutdown.
    pub(crate) fn shutdown(&mut self) {
        for job in self.jobs.values() {
            job.cancellation.cancel();
        }
        for job in self.jobs.values_mut() {
            if matches!(job.state, EvaluationJobState::Prepared { .. }) {
                job.state = EvaluationJobState::Cancelled;
            }
            let _ = finish_running_worker(job);
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
        match &job.state {
            EvaluationJobState::Prepared { .. } => {
                job.cancellation.cancel();
                job.state = EvaluationJobState::Cancelled;
            }
            EvaluationJobState::Running { .. } => job.cancellation.cancel(),
            EvaluationJobState::Completed { .. }
            | EvaluationJobState::Cancelled
            | EvaluationJobState::Failed(_) => {}
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
        match &job.state {
            EvaluationJobState::Completed { results, .. } => Ok(results),
            EvaluationJobState::Prepared { .. } => {
                Err(Error::command("vegetation evaluation has not started"))
            }
            EvaluationJobState::Running { .. } => {
                Err(Error::command("vegetation evaluation is still running"))
            }
            EvaluationJobState::Cancelled => {
                Err(Error::command("vegetation evaluation was cancelled"))
            }
            EvaluationJobState::Failed(error) => Err(Error::Failure(Box::new(error.clone()))),
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
                .filter(|job| job.state.is_terminal())
                .count();
            let retained_bytes = self.jobs.values().try_fold(0_u64, |total, job| {
                let bytes = match &job.state {
                    EvaluationJobState::Completed {
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
                    (Some(*job_id) != protected && job.state.is_terminal()).then_some(*job_id)
                })
                .or_else(|| {
                    protected.filter(|job_id| {
                        self.jobs
                            .get(job_id)
                            .is_some_and(|job| job.state.is_terminal())
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
            let _ = finish_running_worker(job);
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

fn finish_running_worker(job: &mut EvaluationJob) -> std::result::Result<(), WorkerFailure> {
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

struct CanonicalJobDigest {
    hasher: VegetationContentHasher,
    byte_len: u64,
}

impl CanonicalJobDigest {
    fn new() -> Self {
        Self {
            hasher: VegetationContentHasher::new(),
            byte_len: 0,
        }
    }

    fn update(&mut self, bytes: &[u8]) -> saffron_vegetation::Result<()> {
        let fragment_len =
            u64::try_from(bytes.len()).map_err(|_| saffron_vegetation::Error::NumericOverflow)?;
        let byte_len = self
            .byte_len
            .checked_add(fragment_len)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        self.hasher.update(bytes)?;
        self.byte_len = byte_len;
        Ok(())
    }

    fn update_result(
        &mut self,
        result: &GraphEvaluationResult,
        encoded_len: u64,
    ) -> saffron_vegetation::Result<()> {
        let byte_len = self
            .byte_len
            .checked_add(encoded_len)
            .ok_or(saffron_vegetation::Error::NumericOverflow)?;
        result.update_content_hasher(&mut self.hasher)?;
        self.byte_len = byte_len;
        Ok(())
    }

    fn finalize(self) -> saffron_vegetation::Result<([u8; 32], u64)> {
        Ok((self.hasher.finalize()?, self.byte_len))
    }
}

fn summarize(
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

fn job_dto(job_id: u64, job: &EvaluationJob) -> VegetationEvaluationJobDto {
    VegetationEvaluationJobDto {
        job: job_id.to_string(),
        state: job_state_dto(&job.state),
        preflight: preflight_dto(job.preflight),
    }
}

fn job_state_dto(state: &EvaluationJobState) -> VegetationEvaluationJobStateDto {
    match state {
        EvaluationJobState::Prepared { .. } => VegetationEvaluationJobStateDto::Prepared,
        EvaluationJobState::Running { .. } => VegetationEvaluationJobStateDto::Running,
        EvaluationJobState::Completed { .. } => VegetationEvaluationJobStateDto::Completed,
        EvaluationJobState::Cancelled => VegetationEvaluationJobStateDto::Cancelled,
        EvaluationJobState::Failed(_) => VegetationEvaluationJobStateDto::Failed,
    }
}

fn preflight_dto(preflight: GraphEvaluationPreflight) -> VegetationEvaluationPreflightDto {
    VegetationEvaluationPreflightDto {
        output_cells: preflight.output_cells.to_string(),
        global_stage_tiles: preflight.global_stage_tiles.to_string(),
        input_tiles: preflight.input_tiles.to_string(),
        retained_input_bytes: preflight.retained_input_bytes.to_string(),
        generated_input_bytes: preflight.generated_input_bytes.to_string(),
        candidate_count: preflight.candidate_count.to_string(),
        accepted_count: preflight.accepted_count.to_string(),
        micro_samples: preflight.micro_samples.to_string(),
        preflight_peak_bytes: preflight.preflight_peak_bytes.to_string(),
        execution_peak_bytes: preflight.execution_peak_bytes.to_string(),
        memory_bytes: preflight.memory_bytes.to_string(),
        transfer_bytes: preflight.transfer_bytes.to_string(),
        worker_count: preflight.worker_count,
        time_limit_ms: preflight.time_limit_ms.to_string(),
        limits: crate::commands_vegetation::graph_limits_dto(preflight.limits),
    }
}

fn status_dto(job_id: u64, job: &EvaluationJob) -> VegetationEvaluationStatusDto {
    let (summary, error) = match &job.state {
        EvaluationJobState::Prepared { .. } | EvaluationJobState::Running { .. } => (None, None),
        EvaluationJobState::Completed { summary, .. } => (Some((**summary).clone()), None),
        EvaluationJobState::Cancelled => (None, None),
        EvaluationJobState::Failed(error) => (None, Some(error.clone())),
    };
    VegetationEvaluationStatusDto {
        job: job_id.to_string(),
        state: job_state_dto(&job.state),
        preflight: preflight_dto(job.preflight),
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
        GraphNodeAddress, GraphSafetyLimits, NamedDiagnosticStream, PlantPointColumns,
        ProvenanceHandle, ProvenanceTable, RejectedCandidate, vegetation_content_hash,
    };

    fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
        GraphEvaluationResult {
            cell,
            macro_points: PlantPointColumns::from_points(Vec::new()).unwrap(),
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
        EvaluationJob {
            cancellation,
            preflight: sample_preflight(),
            state: EvaluationJobState::Running {
                worker: OwnedWorker::completed(outcome),
            },
        }
    }

    fn failed_job(error: impl Into<String>) -> EvaluationJob {
        EvaluationJob {
            cancellation: GraphCancellationToken::default(),
            preflight: sample_preflight(),
            state: EvaluationJobState::Failed(ControlFailureDto::Command {
                message: error.into(),
            }),
        }
    }

    fn sample_preflight() -> GraphEvaluationPreflight {
        GraphEvaluationPreflight {
            output_cells: 1,
            global_stage_tiles: 2,
            input_tiles: 3,
            retained_input_bytes: 4,
            generated_input_bytes: 5,
            candidate_count: 6,
            accepted_count: 7,
            micro_samples: 8,
            preflight_peak_bytes: 9,
            execution_peak_bytes: 10,
            memory_bytes: 10,
            transfer_bytes: 11,
            worker_count: 1,
            time_limit_ms: 12,
            limits: GraphSafetyLimits::default(),
        }
    }

    fn completed_job(canonical_bytes: u64) -> EvaluationJob {
        let results = GraphEvaluationJobResult::default();
        let (summary, measured_bytes) = summarize(&results).unwrap();
        assert!(measured_bytes > 0);
        EvaluationJob {
            cancellation: GraphCancellationToken::default(),
            preflight: sample_preflight(),
            state: EvaluationJobState::Completed {
                results: Arc::new(results),
                summary: Box::new(summary),
                canonical_bytes,
            },
        }
    }

    fn reference_job_canonical_bytes(results: &GraphEvaluationJobResult) -> Vec<u8> {
        let mut canonical = b"saffron-anima/vegetation-evaluation-job/v2\0".to_vec();
        canonical.extend_from_slice(&u64::try_from(results.cells.len()).unwrap().to_be_bytes());
        for result in &results.cells {
            let bytes = result.canonical_bytes().unwrap();
            canonical.push(0);
            canonical.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_be_bytes());
            canonical.extend_from_slice(&bytes);
        }
        canonical.extend_from_slice(
            &u64::try_from(results.global_stages.len())
                .unwrap()
                .to_be_bytes(),
        );
        for stage in &results.global_stages {
            let bytes = stage.result.canonical_bytes().unwrap();
            canonical.push(1);
            canonical.extend_from_slice(&stage.stage);
            canonical.extend_from_slice(&stage.owner.canonical_bytes());
            canonical.extend_from_slice(&stage.resident_bytes.to_be_bytes());
            canonical.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_be_bytes());
            canonical.extend_from_slice(&bytes);
        }
        canonical
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

        assert!(matches!(&job.state, EvaluationJobState::Cancelled));
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
        assert_eq!(status.preflight.output_cells, "1");
        assert_eq!(status.preflight.global_stage_tiles, "2");
        assert_eq!(status.preflight.retained_input_bytes, "4");
        assert_eq!(status.preflight.generated_input_bytes, "5");
        assert_eq!(status.preflight.preflight_peak_bytes, "9");
        assert_eq!(status.preflight.execution_peak_bytes, "10");
        assert_eq!(status.preflight.memory_bytes, "10");
        assert!(matches!(
            &jobs.jobs.get(&1).unwrap().state,
            EvaluationJobState::Completed { .. }
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
        let reference = reference_job_canonical_bytes(&results);
        let (summary, retained) = summarize(&results).unwrap();
        assert_eq!(summary.cells, "1");
        assert_eq!(summary.global_stages, "1");
        assert_eq!(summary.global_resident_bytes, "4096");
        assert_eq!(
            summary.canonical_hash,
            hex_hash(vegetation_content_hash(&reference))
        );
        assert_eq!(retained, u64::try_from(reference.len()).unwrap());

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
