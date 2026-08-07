//! Shared runtime vegetation binding and bounded cell-load scheduling.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use saffron_assets::{
    AssetServer, CookProjectView, StagedVegetationCook, VegetationArtifactStore,
    VegetationCookRequest, commit_staged_vegetation_cook, stage_vegetation_cook,
};
use saffron_scene::{Scene, VegetationField};
use saffron_spatial::SurfaceField;
use saffron_spatial::{ResidencyManager, WorldCellKey};
use saffron_vegetation::{
    ContentHash, GraphCancellationToken, StagedVegetationCellGeneration, VegetationBaseManifest,
    VegetationCellLoad, VegetationResidencyBudgets, VegetationWorld,
};

const MAX_CELL_LOAD_WORKERS: usize = 8;

/// Closed reason the sole runtime vegetation authority is not available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationRuntimeUnavailableReason {
    /// Project selection/loading is not complete.
    NoProject,
    /// The active scene has no enabled vegetation field.
    NoEnabledField,
    /// The selected map has no committed cooked generation.
    NoCookedManifest,
    /// Selection, validation, scheduling, or publication failed.
    Fault,
}

/// Current binding state of the sole RuntimeSession vegetation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationRuntimeBindingStatus {
    /// No exact cooked generation is bound.
    Unavailable {
        /// Closed unavailability class.
        reason: VegetationRuntimeUnavailableReason,
        /// Typed error presentation when `reason` is [`VegetationRuntimeUnavailableReason::Fault`].
        detail: Option<String>,
    },
    /// One exact cooked generation is bound.
    Available,
}

impl Default for VegetationRuntimeBindingStatus {
    fn default() -> Self {
        Self::Unavailable {
            reason: VegetationRuntimeUnavailableReason::NoProject,
            detail: None,
        }
    }
}

/// A runtime vegetation binding, scheduling, artifact, or worker failure.
#[derive(Debug, thiserror::Error)]
pub enum VegetationRuntimeError {
    /// The selected scene binding is not singular and valid.
    #[error("{0}")]
    Binding(&'static str),
    /// An exact asset or CAS operation failed.
    #[error(transparent)]
    Asset(#[from] Box<saffron_assets::Error>),
    /// Strict vegetation validation or publication failed.
    #[error(transparent)]
    Vegetation(#[from] saffron_vegetation::Error),
    /// A bounded loader thread could not be spawned.
    #[error("vegetation cell-load worker spawn failed: {0}")]
    WorkerSpawn(#[source] std::io::Error),
    /// A loader thread exited without one terminal result.
    #[error("vegetation cell-load worker disconnected")]
    WorkerDisconnected,
    /// A loader thread panicked.
    #[error("vegetation cell-load worker panicked")]
    WorkerPanicked,
}

#[derive(Debug, thiserror::Error)]
enum CellLoadError {
    #[error("cell artifact is absent from disposable CAS")]
    MissingArtifact,
    #[error(transparent)]
    Asset(#[from] Box<saffron_assets::Error>),
    #[error(transparent)]
    Vegetation(#[from] saffron_vegetation::Error),
}

impl From<saffron_assets::Error> for VegetationRuntimeError {
    fn from(error: saffron_assets::Error) -> Self {
        Self::Asset(Box::new(error))
    }
}

impl From<saffron_assets::Error> for CellLoadError {
    fn from(error: saffron_assets::Error) -> Self {
        Self::Asset(Box::new(error))
    }
}

type CellLoadOutcome = Result<StagedVegetationCellGeneration, CellLoadError>;
type RegenerationOutcome = saffron_assets::Result<StagedVegetationCook>;

struct CellLoadWorker {
    receiver: Receiver<CellLoadOutcome>,
    thread: Option<JoinHandle<()>>,
}

struct RegenerationWorker {
    cancellation: GraphCancellationToken,
    receiver: Receiver<RegenerationOutcome>,
    thread: Option<JoinHandle<()>>,
}

impl RegenerationWorker {
    fn spawn(
        project: CookProjectView,
        request: VegetationCookRequest,
    ) -> Result<Self, VegetationRuntimeError> {
        let cancellation = GraphCancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("vegetation-runtime-regeneration".to_owned())
            .spawn(move || {
                let result = stage_vegetation_cook(project, request, &worker_cancellation, |_| {});
                let _ = sender.send(result);
            })
            .map_err(VegetationRuntimeError::WorkerSpawn)?;
        Ok(Self {
            cancellation,
            receiver,
            thread: Some(thread),
        })
    }

    fn poll(&mut self) -> Result<Option<RegenerationOutcome>, VegetationRuntimeError> {
        match self.receiver.try_recv() {
            Ok(result) => {
                self.join()?;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.join()?;
                Err(VegetationRuntimeError::WorkerDisconnected)
            }
        }
    }

    fn cancel_and_join(&mut self) -> Result<(), VegetationRuntimeError> {
        self.cancellation.cancel();
        self.join()
    }

    fn join(&mut self) -> Result<(), VegetationRuntimeError> {
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| VegetationRuntimeError::WorkerPanicked),
            None => Ok(()),
        }
    }
}

impl CellLoadWorker {
    fn spawn(
        cell: WorldCellKey,
        store: VegetationArtifactStore,
        artifact: ContentHash,
        load: VegetationCellLoad,
    ) -> Result<Self, VegetationRuntimeError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let coordinates = cell.coordinates();
        let thread = std::thread::Builder::new()
            .name(format!(
                "vegetation-cell-{}-{}-{}-{}",
                coordinates[0],
                coordinates[1],
                coordinates[2],
                cell.level()
            ))
            .spawn(move || {
                let result = match store.read_cell_if_present(artifact) {
                    Ok(Some(bytes)) => load.stage(&bytes).map_err(CellLoadError::from),
                    Ok(None) => Err(CellLoadError::MissingArtifact),
                    Err(error) => Err(CellLoadError::from(error)),
                };
                let _ = sender.send(result);
            })
            .map_err(VegetationRuntimeError::WorkerSpawn)?;
        Ok(Self {
            receiver,
            thread: Some(thread),
        })
    }

    fn poll(&mut self) -> Result<Option<CellLoadOutcome>, VegetationRuntimeError> {
        match self.receiver.try_recv() {
            Ok(result) => {
                self.join()?;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.join()?;
                Err(VegetationRuntimeError::WorkerDisconnected)
            }
        }
    }

    fn join(&mut self) -> Result<(), VegetationRuntimeError> {
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| VegetationRuntimeError::WorkerPanicked),
            None => Ok(()),
        }
    }
}

/// RuntimeSession-owned scheduler for the one vegetation authority.
#[derive(Default)]
pub(crate) struct VegetationRuntimeScheduler {
    workers: BTreeMap<WorldCellKey, CellLoadWorker>,
    missing_cells: BTreeSet<WorldCellKey>,
    regeneration: Option<RegenerationWorker>,
}

enum ManifestSelection {
    NoEnabledField,
    NoCookedManifest,
    Selected(Box<VegetationBaseManifest>),
}

impl VegetationRuntimeScheduler {
    pub(crate) fn advance(
        &mut self,
        runtime: &mut Option<VegetationWorld>,
        scene: &mut Scene,
        assets: &AssetServer,
        spatial: &ResidencyManager,
    ) -> Result<VegetationRuntimeBindingStatus, VegetationRuntimeError> {
        let manifest = match selected_manifest(scene, assets)? {
            ManifestSelection::NoEnabledField => {
                self.clear(runtime)?;
                return Ok(VegetationRuntimeBindingStatus::Unavailable {
                    reason: VegetationRuntimeUnavailableReason::NoEnabledField,
                    detail: None,
                });
            }
            ManifestSelection::NoCookedManifest => {
                self.clear(runtime)?;
                return Ok(VegetationRuntimeBindingStatus::Unavailable {
                    reason: VegetationRuntimeUnavailableReason::NoCookedManifest,
                    detail: None,
                });
            }
            ManifestSelection::Selected(manifest) => *manifest,
        };
        let identity = manifest.identity()?;
        if runtime
            .as_ref()
            .is_none_or(|world| world.manifest_identity() != identity)
        {
            // What the outgoing world accumulated in memory outranks what the map last wrote to
            // disk; a bind that read the durable copy instead would drop every mutation taken
            // since the last save barrier.
            let carried = match runtime.as_ref() {
                Some(world) => Some(world.persistent_state().clone()),
                None => assets
                    .vegetation_state_store()
                    .read_baseline_if_present(manifest.map)
                    .map_err(|error| VegetationRuntimeError::Asset(Box::new(error)))?,
            };
            self.join_workers()?;
            let mut world =
                VegetationWorld::new(manifest.clone(), VegetationResidencyBudgets::default())?;
            // A world often starts with disturbance or growth already in it: an author's saved
            // barrier, or a shipped package's starting state. State binds to one generation and a
            // recook publishes another, so it rebases onto the incoming base — dropping only what
            // that base no longer carries — rather than being silently discarded.
            if let Some(state) = carried {
                let bound = if state.manifest_identity() == identity.bytes() {
                    state
                } else {
                    state.rebase(&manifest)?
                };
                world.replace_persistent_state(bound)?;
            }
            *runtime = Some(world);
            self.missing_cells.clear();
        }
        let world = runtime.as_mut().expect("runtime was initialized");
        synchronize_sources(world, spatial)?;
        self.publish_completed(world)?;
        self.start_pending(world, assets)?;
        Ok(VegetationRuntimeBindingStatus::Available)
    }

    pub(crate) fn clear(
        &mut self,
        runtime: &mut Option<VegetationWorld>,
    ) -> Result<(), VegetationRuntimeError> {
        self.join_workers()?;
        self.missing_cells.clear();
        *runtime = None;
        Ok(())
    }

    pub(crate) fn missing_cells(&self) -> Vec<WorldCellKey> {
        self.missing_cells.iter().copied().collect()
    }

    pub(crate) fn needs_regeneration(&self) -> bool {
        !self.missing_cells.is_empty() || self.regeneration.is_some()
    }

    pub(crate) fn regenerate_missing(
        &mut self,
        runtime: &mut Option<VegetationWorld>,
        assets: &mut AssetServer,
        surface_providers: &[std::sync::Arc<dyn SurfaceField>],
    ) -> Result<(), VegetationRuntimeError> {
        let world = runtime.as_ref().ok_or(VegetationRuntimeError::Binding(
            "vegetation runtime is unavailable for cell regeneration",
        ))?;
        if let Some(worker) = self.regeneration.as_mut()
            && let Some(outcome) = worker.poll()?
        {
            self.regeneration = None;
            let staged = outcome?;
            let cancellation = GraphCancellationToken::default();
            commit_staged_vegetation_cook(assets, surface_providers, staged, &cancellation)?;
            self.missing_cells.clear();
        }
        if self.regeneration.is_none() && !self.missing_cells.is_empty() {
            let worker_count = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(usize::from(u16::MAX));
            let workers = u16::try_from(worker_count).map_err(|_| {
                VegetationRuntimeError::Binding("worker count is not representable")
            })?;
            let request = VegetationCookRequest {
                world: world.manifest().world,
                map: world.manifest().map,
                expected_manifest: Some(world.manifest_identity()),
                cells: self.missing_cells.iter().copied().collect(),
                ecology_tick: 0,
                workers,
                platform: world.manifest().platform.clone(),
                surface_providers: surface_providers.to_vec(),
            };
            self.regeneration = Some(RegenerationWorker::spawn(
                CookProjectView::capture(assets),
                request,
            )?);
        }
        Ok(())
    }

    fn publish_completed(
        &mut self,
        world: &mut VegetationWorld,
    ) -> Result<(), VegetationRuntimeError> {
        let cells = self.workers.keys().copied().collect::<Vec<_>>();
        for cell in cells {
            let completed = self
                .workers
                .get_mut(&cell)
                .expect("worker key was snapshotted")
                .poll()?;
            let Some(outcome) = completed else {
                continue;
            };
            self.workers.remove(&cell);
            match outcome {
                Ok(staged) => {
                    let _published = world.publish_staged(staged)?;
                }
                Err(CellLoadError::MissingArtifact) => {
                    self.missing_cells.insert(cell);
                }
                Err(CellLoadError::Asset(error)) => return Err(error.into()),
                Err(CellLoadError::Vegetation(error)) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn start_pending(
        &mut self,
        world: &mut VegetationWorld,
        assets: &AssetServer,
    ) -> Result<(), VegetationRuntimeError> {
        let capacity = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_CELL_LOAD_WORKERS);
        let available = capacity.saturating_sub(self.workers.len());
        if available == 0 {
            return Ok(());
        }
        let pending = world.residency_report()?.pending;
        let store = assets.vegetation_artifact_store();
        let requests = pending
            .into_iter()
            .filter(|request| {
                !self.workers.contains_key(&request.cell)
                    && !self.missing_cells.contains(&request.cell)
            })
            .take(available)
            .collect::<Vec<_>>();
        for request in requests {
            let artifact = world
                .manifest()
                .cells
                .iter()
                .find(|row| row.cell == request.cell)
                .ok_or(VegetationRuntimeError::Binding(
                    "runtime load request is absent from the selected manifest",
                ))?
                .artifact_hash;
            let load = world.begin_load(request.cell, request.facets)?;
            let worker = CellLoadWorker::spawn(request.cell, store.clone(), artifact, load)?;
            self.workers.insert(request.cell, worker);
        }
        Ok(())
    }

    fn join_workers(&mut self) -> Result<(), VegetationRuntimeError> {
        if let Some(mut worker) = self.regeneration.take() {
            worker.cancel_and_join()?;
        }
        let workers = std::mem::take(&mut self.workers);
        for (_, mut worker) in workers {
            worker.join()?;
        }
        Ok(())
    }
}

impl Drop for VegetationRuntimeScheduler {
    fn drop(&mut self) {
        if let Err(error) = self.join_workers() {
            tracing::error!("vegetation runtime scheduler teardown failed: {error}");
        }
    }
}

fn selected_manifest(
    scene: &mut Scene,
    assets: &AssetServer,
) -> Result<ManifestSelection, VegetationRuntimeError> {
    let mut maps = Vec::new();
    scene.for_each::<&VegetationField, _>(|_, field| {
        if field.enabled {
            maps.push(field.map);
        }
    });
    let map = match maps.as_slice() {
        [] => return Ok(ManifestSelection::NoEnabledField),
        [map] if map.value() != 0 => *map,
        [_] => {
            return Err(VegetationRuntimeError::Binding(
                "active VegetationField has an invalid map",
            ));
        }
        _ => {
            return Err(VegetationRuntimeError::Binding(
                "active scene has multiple enabled VegetationFields",
            ));
        }
    };
    let Some(bytes) = assets
        .vegetation_artifact_store()
        .read_current_manifest(map)?
    else {
        return Ok(ManifestSelection::NoCookedManifest);
    };
    let manifest = VegetationBaseManifest::from_canonical_bytes(&bytes)?;
    if manifest.map != map {
        return Err(VegetationRuntimeError::Binding(
            "selected vegetation manifest belongs to a different map",
        ));
    }
    Ok(ManifestSelection::Selected(Box::new(manifest)))
}

fn synchronize_sources(
    world: &mut VegetationWorld,
    spatial: &ResidencyManager,
) -> Result<(), VegetationRuntimeError> {
    let desired = spatial.sources();
    let desired_ids = desired
        .iter()
        .map(|source| source.id)
        .collect::<BTreeSet<_>>();
    for source in world.sources() {
        if !desired_ids.contains(&source.id) {
            world.remove_source(source.id)?;
        }
    }
    let current = world
        .sources()
        .into_iter()
        .map(|source| (source.id, source))
        .collect::<BTreeMap<_, _>>();
    for source in desired {
        if current.get(&source.id) != Some(&source) {
            world.update_source(source)?;
        }
    }
    Ok(())
}
