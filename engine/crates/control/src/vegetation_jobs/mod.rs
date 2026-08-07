//! Owned asynchronous vegetation evaluation jobs and retained provenance results.

use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_protocol::{
    ControlFailureDto, VegetationEvaluationJobDto, VegetationEvaluationStatusDto,
    VegetationEvaluationSummaryDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    BiomeGraphEvaluator, CandidateIdentity, DiagnosticCandidateSample, DiagnosticScalarSample,
    GraphCancellationToken, GraphEvaluationJobInputs, GraphEvaluationJobResult,
    GraphEvaluationPreflight, GraphEvaluationResult, PlantId, ProvenanceExplanation,
    RejectedCandidate, VegetationContentHasher,
};

use crate::owned_worker::OwnedWorker;
use crate::{Error, Result};

mod dto;
mod summarize;
#[cfg(test)]
mod tests;

pub(crate) use dto::*;
pub(crate) use summarize::*;

#[derive(Default)]
pub(crate) struct NodeAggregate {
    input_candidates: u64,
    output_candidates: u64,
    output_bytes: u64,
    predicted_transfer_bytes: u64,
    elapsed_micros: u64,
}

#[derive(Default)]
pub(crate) struct GpuGroupAggregate {
    invocation_count: u64,
    output_bytes: u64,
    transfer_bytes: u64,
    elapsed_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DiagnosticResultSource {
    Cell(WorldCellKey),
    GlobalStage {
        stage: [u8; 32],
        owner: WorldCellKey,
    },
}

pub(crate) struct DiagnosticStreamAggregate {
    candidate_samples:
        Option<BTreeMap<(DiagnosticResultSource, CandidateIdentity), DiagnosticCandidateSample>>,
    scalar_samples:
        Option<BTreeMap<(DiagnosticResultSource, CandidateIdentity), DiagnosticScalarSample>>,
    rejected: BTreeMap<(DiagnosticResultSource, CandidateIdentity, u8, u32), RejectedCandidate>,
}

pub(crate) type EvaluationOutcome = saffron_vegetation::Result<GraphEvaluationJobResult>;

pub(crate) const MAX_LIVE_JOBS: usize = 16;

pub(crate) const MAX_TERMINAL_JOBS: usize = 128;

pub(crate) const MAX_RETAINED_PREPARED_INPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(crate) const MAX_RETAINED_CANONICAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub(crate) enum EvaluationJobState {
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

pub(crate) struct EvaluationJob {
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

pub(crate) struct CanonicalJobDigest {
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
