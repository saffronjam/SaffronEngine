//! Bounded asynchronous vegetation cook jobs and main-thread commit handoff.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use saffron_assets::{
    CookProjectView, StagedVegetationCook, VegetationCookEvent, VegetationCookOutput,
    VegetationCookRequest, stage_vegetation_cook,
};
use saffron_core::Uuid;
use saffron_protocol::{
    ControlFailureDto, VegetationCookJobDto, VegetationCookJobStateDto, VegetationCookProgressDto,
    VegetationCookScopeDto, VegetationCookStatusDto,
};
use saffron_vegetation::GraphCancellationToken;

use crate::owned_worker::{OwnedWorker, WorkerFailure, WorkerPoll};
use crate::{Error, Result};

const MAX_LIVE_JOBS: usize = 16;
const MAX_TERMINAL_JOBS: usize = 128;

#[derive(Clone, Debug, Default)]
struct CookProgress {
    completed_nodes: u64,
    total_nodes: u64,
    cache_hits: u64,
    published_cells: u64,
    current: Option<saffron_vegetation::CookNodeAddress>,
}

struct CookTask {
    project: CookProjectView,
    request: VegetationCookRequest,
}

enum CookJobState {
    Queued(CookTask),
    Running {
        worker: OwnedWorker<WorkerCookResult>,
        cancelled: bool,
        superseded: bool,
    },
    Committing,
    Completed(Box<VegetationCookOutput>),
    Cancelled,
    Superseded,
    Failed(ControlFailureDto),
}

impl CookJobState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed(_) | Self::Cancelled | Self::Superseded | Self::Failed(_)
        )
    }
}

struct CookJob {
    map: Uuid,
    scope: VegetationCookScopeDto,
    workers: u16,
    cancellation: GraphCancellationToken,
    progress: Arc<Mutex<CookProgress>>,
    state: CookJobState,
}

/// One staged result that must be committed automatically on the control thread.
pub(crate) struct ReadyVegetationCook {
    pub(crate) job: u64,
    pub(crate) staged: StagedVegetationCook,
    pub(crate) cancellation: GraphCancellationToken,
}

/// Owns bounded background cook workers and retained terminal results.
pub(crate) struct VegetationCookJobs {
    next_job: u64,
    jobs: BTreeMap<u64, CookJob>,
    active_by_map: BTreeMap<u64, u64>,
}

impl Default for VegetationCookJobs {
    fn default() -> Self {
        Self {
            next_job: 1,
            jobs: BTreeMap::new(),
            active_by_map: BTreeMap::new(),
        }
    }
}

impl VegetationCookJobs {
    pub(crate) fn enqueue(
        &mut self,
        project: CookProjectView,
        request: VegetationCookRequest,
        scope: VegetationCookScopeDto,
    ) -> Result<VegetationCookJobDto> {
        let live = self
            .jobs
            .values()
            .filter(|job| !job.state.is_terminal())
            .count();
        if live >= MAX_LIVE_JOBS {
            return Err(Error::command(format!(
                "vegetation cook live-job limit exceeded: requested {}, limit {MAX_LIVE_JOBS}",
                live + 1
            )));
        }
        if let Some(previous) = self.active_by_map.get(&request.map.value()).copied()
            && let Some(job) = self.jobs.get_mut(&previous)
        {
            job.cancellation.cancel();
            match &mut job.state {
                CookJobState::Queued(_) => job.state = CookJobState::Superseded,
                CookJobState::Running { superseded, .. } => *superseded = true,
                CookJobState::Committing
                | CookJobState::Completed(_)
                | CookJobState::Cancelled
                | CookJobState::Superseded
                | CookJobState::Failed(_) => {}
            }
        }
        let job_id = self.next_job;
        self.next_job = job_id
            .checked_add(1)
            .ok_or_else(|| Error::command("vegetation cook job identity overflowed"))?;
        let map = request.map;
        let workers = request.workers;
        let job = CookJob {
            map,
            scope,
            workers,
            cancellation: GraphCancellationToken::default(),
            progress: Arc::new(Mutex::new(CookProgress::default())),
            state: CookJobState::Queued(CookTask { project, request }),
        };
        self.jobs.insert(job_id, job);
        self.active_by_map.insert(map.value(), job_id);
        self.start_queued()?;
        self.retain_terminals();
        Ok(job_dto(job_id, self.job(job_id)?))
    }

    pub(crate) fn poll_ready(&mut self) -> Result<Vec<ReadyVegetationCook>> {
        let ready = self.poll_workers()?;
        self.start_queued()?;
        self.retain_terminals();
        Ok(ready)
    }

    pub(crate) fn complete_commit(
        &mut self,
        job_id: u64,
        result: saffron_assets::Result<VegetationCookOutput>,
    ) -> Result<()> {
        let job = self.job_mut(job_id)?;
        if !matches!(job.state, CookJobState::Committing) {
            return Err(Error::command(format!(
                "vegetation cook job {job_id} is not awaiting commit"
            )));
        }
        job.state = match result {
            Ok(output) => CookJobState::Completed(Box::new(output)),
            Err(error) => CookJobState::Failed(Error::from(error).into_failure()),
        };
        self.remove_active_if_terminal(job_id);
        self.retain_terminals();
        Ok(())
    }

    pub(crate) fn status(&mut self, job_id: u64) -> Result<VegetationCookStatusDto> {
        status_dto(job_id, self.job(job_id)?)
    }

    pub(crate) fn cancel(&mut self, job_id: u64) -> Result<VegetationCookStatusDto> {
        let job = self.job_mut(job_id)?;
        job.cancellation.cancel();
        match &mut job.state {
            CookJobState::Queued(_) => job.state = CookJobState::Cancelled,
            CookJobState::Running { cancelled, .. } => *cancelled = true,
            CookJobState::Committing
            | CookJobState::Completed(_)
            | CookJobState::Cancelled
            | CookJobState::Superseded
            | CookJobState::Failed(_) => {}
        }
        self.remove_active_if_terminal(job_id);
        status_dto(job_id, self.job(job_id)?)
    }

    pub(crate) fn latest_statistics(
        &self,
        map: Uuid,
        manifest: saffron_vegetation::ContentHash,
    ) -> Option<saffron_assets::VegetationCookStatistics> {
        self.jobs
            .iter()
            .rev()
            .find_map(|(_, job)| match &job.state {
                CookJobState::Completed(output)
                    if job.map == map && output.manifest_identity == manifest =>
                {
                    Some(output.statistics.clone())
                }
                _ => None,
            })
    }

    pub(crate) fn shutdown(&mut self) {
        for job in self.jobs.values() {
            job.cancellation.cancel();
        }
        for job in self.jobs.values_mut() {
            match &mut job.state {
                CookJobState::Queued(_) | CookJobState::Committing => {
                    job.state = CookJobState::Cancelled;
                }
                CookJobState::Running { worker, .. } => {
                    let _ = worker.finish();
                    job.state = CookJobState::Cancelled;
                }
                CookJobState::Completed(_)
                | CookJobState::Cancelled
                | CookJobState::Superseded
                | CookJobState::Failed(_) => {}
            }
        }
        self.active_by_map.clear();
    }

    fn poll_workers(&mut self) -> Result<Vec<ReadyVegetationCook>> {
        let mut ready = Vec::new();
        let ids = self.jobs.keys().copied().collect::<Vec<_>>();
        for job_id in ids {
            let outcome = {
                let job = self.job_mut(job_id)?;
                let CookJobState::Running {
                    worker,
                    cancelled,
                    superseded,
                } = &mut job.state
                else {
                    continue;
                };
                match worker.poll() {
                    Ok(WorkerPoll::Pending) => continue,
                    Ok(WorkerPoll::Complete(result)) => Some((result, *cancelled, *superseded)),
                    Err(failure) => {
                        job.state = CookJobState::Failed(worker_failure(failure));
                        None
                    }
                }
            };
            let Some((result, cancelled, superseded)) = outcome else {
                self.remove_active_if_terminal(job_id);
                continue;
            };
            let job = self.job_mut(job_id)?;
            if superseded {
                job.state = CookJobState::Superseded;
            } else if cancelled {
                job.state = CookJobState::Cancelled;
            } else {
                match result {
                    Ok(staged) => {
                        job.state = CookJobState::Committing;
                        ready.push(ReadyVegetationCook {
                            job: job_id,
                            staged: *staged,
                            cancellation: job.cancellation.clone(),
                        });
                    }
                    Err(error)
                        if matches!(
                            error.as_ref(),
                            saffron_assets::Error::Vegetation(
                                saffron_vegetation::Error::GraphCancelled,
                            )
                        ) =>
                    {
                        job.state = CookJobState::Cancelled
                    }
                    Err(error) => {
                        job.state = CookJobState::Failed(Error::from(*error).into_failure())
                    }
                }
            }
            self.remove_active_if_terminal(job_id);
        }
        Ok(ready)
    }

    fn start_queued(&mut self) -> Result<()> {
        let budget = std::thread::available_parallelism().map_or(1_u64, |value| value.get() as u64);
        let mut admitted = self
            .jobs
            .values()
            .filter_map(|job| match job.state {
                CookJobState::Running { .. } => Some(u64::from(job.workers)),
                _ => None,
            })
            .sum::<u64>();
        let queued = self
            .jobs
            .iter()
            .filter_map(|(id, job)| matches!(job.state, CookJobState::Queued(_)).then_some(*id))
            .collect::<Vec<_>>();
        for job_id in queued {
            let workers = u64::from(self.job(job_id)?.workers);
            if admitted != 0 && admitted.saturating_add(workers) > budget {
                continue;
            }
            self.start(job_id)?;
            admitted = admitted.saturating_add(workers);
        }
        Ok(())
    }

    fn start(&mut self, job_id: u64) -> Result<()> {
        let job = self.job_mut(job_id)?;
        let state = std::mem::replace(&mut job.state, CookJobState::Cancelled);
        let CookJobState::Queued(task) = state else {
            job.state = state;
            return Ok(());
        };
        let cancellation = job.cancellation.clone();
        let progress = Arc::clone(&job.progress);
        let worker = OwnedWorker::spawn(format!("saffron-vegetation-cook-{job_id}"), move || {
            stage_vegetation_cook(task.project, task.request, &cancellation, |event| {
                update_progress(&progress, event);
            })
            .map(Box::new)
            .map_err(Box::new)
        })
        .map_err(|error| {
            Error::command(format!("vegetation cook worker failed to start: {error}"))
        })?;
        job.state = CookJobState::Running {
            worker,
            cancelled: false,
            superseded: false,
        };
        Ok(())
    }

    fn remove_active_if_terminal(&mut self, job_id: u64) {
        let Some(job) = self.jobs.get(&job_id) else {
            return;
        };
        if job.state.is_terminal() && self.active_by_map.get(&job.map.value()) == Some(&job_id) {
            self.active_by_map.remove(&job.map.value());
        }
    }

    fn retain_terminals(&mut self) {
        let terminal = self
            .jobs
            .iter()
            .filter_map(|(id, job)| job.state.is_terminal().then_some(*id))
            .collect::<Vec<_>>();
        let excess = terminal.len().saturating_sub(MAX_TERMINAL_JOBS);
        for job_id in terminal.into_iter().take(excess) {
            self.jobs.remove(&job_id);
        }
    }

    fn job(&self, job_id: u64) -> Result<&CookJob> {
        self.jobs
            .get(&job_id)
            .ok_or_else(|| Error::command(format!("unknown vegetation cook job {job_id}")))
    }

    fn job_mut(&mut self, job_id: u64) -> Result<&mut CookJob> {
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| Error::command(format!("unknown vegetation cook job {job_id}")))
    }
}

type WorkerCookResult = std::result::Result<Box<StagedVegetationCook>, Box<saffron_assets::Error>>;

fn update_progress(progress: &Mutex<CookProgress>, event: VegetationCookEvent) {
    let mut progress = progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match event {
        VegetationCookEvent::Planned { total_nodes } => progress.total_nodes = total_nodes,
        VegetationCookEvent::Started { node } => progress.current = Some(node),
        VegetationCookEvent::Completed {
            cache_hit,
            published_cell,
            ..
        } => {
            progress.completed_nodes = progress.completed_nodes.saturating_add(1);
            progress.cache_hits = progress.cache_hits.saturating_add(u64::from(cache_hit));
            progress.published_cells = progress
                .published_cells
                .saturating_add(u64::from(published_cell));
            progress.current = None;
        }
    }
}

fn job_dto(job_id: u64, job: &CookJob) -> VegetationCookJobDto {
    VegetationCookJobDto {
        job: job_id.to_string(),
        state: state_dto(&job.state),
        scope: job.scope.clone(),
        progress: progress_dto(job),
    }
}

fn status_dto(job_id: u64, job: &CookJob) -> Result<VegetationCookStatusDto> {
    let (statistics, manifest, error) = match &job.state {
        CookJobState::Completed(output) => (
            Some(crate::vegetation_cook_dto::statistics_dto(
                &output.statistics,
            )),
            Some(crate::vegetation_cook_dto::manifest_dto(&output.manifest)?),
            None,
        ),
        CookJobState::Failed(error) => (None, None, Some(error.clone())),
        CookJobState::Queued(_)
        | CookJobState::Running { .. }
        | CookJobState::Committing
        | CookJobState::Cancelled
        | CookJobState::Superseded => (None, None, None),
    };
    Ok(VegetationCookStatusDto {
        job: job_id.to_string(),
        state: state_dto(&job.state),
        progress: progress_dto(job),
        statistics,
        manifest,
        error,
    })
}

fn state_dto(state: &CookJobState) -> VegetationCookJobStateDto {
    match state {
        CookJobState::Queued(_) => VegetationCookJobStateDto::Queued,
        CookJobState::Running {
            superseded: true, ..
        }
        | CookJobState::Superseded => VegetationCookJobStateDto::Superseded,
        CookJobState::Running {
            cancelled: true, ..
        }
        | CookJobState::Cancelled => VegetationCookJobStateDto::Cancelled,
        CookJobState::Running { .. } | CookJobState::Committing => {
            VegetationCookJobStateDto::Running
        }
        CookJobState::Completed(_) => VegetationCookJobStateDto::Completed,
        CookJobState::Failed(_) => VegetationCookJobStateDto::Failed,
    }
}

fn progress_dto(job: &CookJob) -> VegetationCookProgressDto {
    let progress = job
        .progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    VegetationCookProgressDto {
        completed_nodes: progress.completed_nodes.to_string(),
        total_nodes: progress.total_nodes.to_string(),
        cache_hits: progress.cache_hits.to_string(),
        published_cells: progress.published_cells.to_string(),
        current: progress
            .current
            .as_ref()
            .map(crate::vegetation_cook_dto::cook_node_address_dto),
    }
}

fn worker_failure(failure: WorkerFailure) -> ControlFailureDto {
    ControlFailureDto::Command {
        message: match failure {
            WorkerFailure::Disconnected => "vegetation cook worker disconnected".to_owned(),
            WorkerFailure::Panicked => "vegetation cook worker panicked".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_job(map: Uuid, result: WorkerCookResult, workers: u16) -> CookJob {
        CookJob {
            map,
            scope: VegetationCookScopeDto::All,
            workers,
            cancellation: GraphCancellationToken::default(),
            progress: Arc::new(Mutex::new(CookProgress::default())),
            state: CookJobState::Running {
                worker: OwnedWorker::completed(result),
                cancelled: false,
                superseded: false,
            },
        }
    }

    fn failed_worker(message: &str) -> WorkerCookResult {
        Err(Box::new(saffron_assets::Error::Io(message.to_owned())))
    }

    fn project_view() -> CookProjectView {
        CookProjectView {
            asset_root: "test-assets".into(),
            cache_root: "test-cache".into(),
            catalog: Arc::new(saffron_scene::AssetCatalog::default()),
        }
    }

    fn cook_request(map: Uuid, workers: u16) -> VegetationCookRequest {
        VegetationCookRequest {
            world: Uuid(11),
            map,
            expected_manifest: None,
            cells: Vec::new(),
            ecology_tick: 0,
            workers,
            platform: saffron_assets::portable_vegetation_platform_profile(None),
            surface_providers: Vec::new(),
        }
    }

    #[test]
    fn status_does_not_consume_a_completed_worker_result() {
        let map = Uuid(21);
        let mut jobs = VegetationCookJobs::default();
        jobs.jobs
            .insert(1, running_job(map, failed_worker("fixture failure"), 1));
        jobs.active_by_map.insert(map.value(), 1);

        let status = jobs.status(1).unwrap();
        assert_eq!(status.state, VegetationCookJobStateDto::Running);
        assert!(matches!(
            &jobs.job(1).unwrap().state,
            CookJobState::Running { .. }
        ));

        assert!(jobs.poll_ready().unwrap().is_empty());
        let status = jobs.status(1).unwrap();
        assert_eq!(status.state, VegetationCookJobStateDto::Failed);
        assert!(status.error.is_some());
        assert!(!jobs.active_by_map.contains_key(&map.value()));
    }

    #[test]
    fn cancelling_a_running_job_leaves_its_worker_for_poll_ready() {
        let map = Uuid(22);
        let mut jobs = VegetationCookJobs::default();
        jobs.jobs
            .insert(1, running_job(map, failed_worker("unused failure"), 1));
        jobs.active_by_map.insert(map.value(), 1);

        let status = jobs.cancel(1).unwrap();
        assert_eq!(status.state, VegetationCookJobStateDto::Cancelled);
        let job = jobs.job(1).unwrap();
        assert!(job.cancellation.is_cancelled());
        assert!(matches!(
            &job.state,
            CookJobState::Running {
                cancelled: true,
                superseded: false,
                ..
            }
        ));
        assert_eq!(jobs.active_by_map.get(&map.value()), Some(&1));

        assert!(jobs.poll_ready().unwrap().is_empty());
        assert_eq!(
            jobs.status(1).unwrap().state,
            VegetationCookJobStateDto::Cancelled
        );
        assert!(!jobs.active_by_map.contains_key(&map.value()));
    }

    #[test]
    fn newer_same_map_job_supersedes_the_running_job() {
        let map = Uuid(23);
        let mut jobs = VegetationCookJobs::default();
        jobs.jobs.insert(
            1,
            running_job(map, failed_worker("superseded result"), u16::MAX),
        );
        jobs.active_by_map.insert(map.value(), 1);
        jobs.next_job = 2;

        let replacement = jobs
            .enqueue(
                project_view(),
                cook_request(map, 1),
                VegetationCookScopeDto::All,
            )
            .unwrap();

        assert_eq!(replacement.job, "2");
        let superseded = jobs.job(1).unwrap();
        assert!(superseded.cancellation.is_cancelled());
        assert!(matches!(
            &superseded.state,
            CookJobState::Running {
                cancelled: false,
                superseded: true,
                ..
            }
        ));
        assert_eq!(
            jobs.status(1).unwrap().state,
            VegetationCookJobStateDto::Superseded
        );
        assert_eq!(jobs.active_by_map.get(&map.value()), Some(&2));
        jobs.shutdown();
    }

    #[test]
    fn poll_ready_finalizes_a_superseded_worker_as_superseded() {
        let map = Uuid(24);
        let mut jobs = VegetationCookJobs::default();
        let mut job = running_job(map, failed_worker("superseded result"), 1);
        job.cancellation.cancel();
        let CookJobState::Running { superseded, .. } = &mut job.state else {
            unreachable!();
        };
        *superseded = true;
        jobs.jobs.insert(1, job);
        jobs.active_by_map.insert(map.value(), 1);

        assert!(jobs.poll_ready().unwrap().is_empty());
        assert_eq!(
            jobs.status(1).unwrap().state,
            VegetationCookJobStateDto::Superseded
        );
        assert!(!jobs.active_by_map.contains_key(&map.value()));
    }

    #[test]
    fn terminal_history_retains_only_the_newest_bounded_window() {
        let mut jobs = VegetationCookJobs::default();
        let total = u64::try_from(MAX_TERMINAL_JOBS).unwrap() + 3;
        for job_id in 1..=total {
            jobs.jobs.insert(
                job_id,
                CookJob {
                    map: Uuid(1_000 + job_id),
                    scope: VegetationCookScopeDto::All,
                    workers: 1,
                    cancellation: GraphCancellationToken::default(),
                    progress: Arc::new(Mutex::new(CookProgress::default())),
                    state: CookJobState::Cancelled,
                },
            );
        }

        jobs.retain_terminals();

        assert_eq!(jobs.jobs.len(), MAX_TERMINAL_JOBS);
        assert_eq!(jobs.jobs.first_key_value().map(|(id, _)| *id), Some(4));
        assert_eq!(jobs.jobs.last_key_value().map(|(id, _)| *id), Some(total));
    }
}
