use std::collections::BTreeMap;

use super::delta::{GpuSceneSharedRecords, GpuSceneWorld};
use super::table::SceneTable;
use super::table::frame_bit;
use super::upload_ring::GpuSceneUploadRing;
use super::*;
use crate::GpuHandle;

/// View-local temporal, visibility, HZB, and command preparation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSceneViewState {
    /// World observed by this view.
    pub world: GpuSceneWorldId,
    /// Shared-scene revision consumed by the latest preparation.
    pub shared_revision: u64,
    /// World revision consumed by the latest preparation.
    pub world_revision: u64,
    /// Monotonic visibility-output revision.
    pub visibility_revision: u64,
    /// Monotonic HZB publication revision.
    pub hzb_revision: u64,
    /// Monotonic indirect-command publication revision.
    pub command_revision: u64,
    /// Temporal history generation.
    pub history_generation: u64,
    /// Whether previous visibility/HZB data may be consumed.
    pub history_valid: bool,
    /// Most recent invalidation reason.
    pub invalidation: GpuSceneHistoryInvalidation,
}

impl GpuSceneViewState {
    fn new(world: GpuSceneWorldId) -> Self {
        Self {
            world,
            shared_revision: 0,
            world_revision: 0,
            visibility_revision: 0,
            hzb_revision: 0,
            command_revision: 0,
            history_generation: 1,
            history_valid: false,
            invalidation: GpuSceneHistoryInvalidation::NewView,
        }
    }

    /// Invalidates all temporal data while preserving shared scene records.
    pub fn invalidate(&mut self, reason: GpuSceneHistoryInvalidation) -> Result<(), GpuSceneError> {
        self.history_generation = self
            .history_generation
            .checked_add(1)
            .ok_or(GpuSceneError::RevisionOverflow)?;
        self.history_valid = false;
        self.invalidation = reason;
        Ok(())
    }

    /// Publishes independently versioned visibility, HZB, and command state.
    pub fn publish(
        &mut self,
        shared_revision: u64,
        world_revision: u64,
    ) -> Result<(), GpuSceneError> {
        self.shared_revision = shared_revision;
        self.world_revision = world_revision;
        self.visibility_revision = increment(self.visibility_revision)?;
        self.hzb_revision = increment(self.hzb_revision)?;
        self.command_revision = increment(self.command_revision)?;
        self.history_valid = true;
        Ok(())
    }
}

pub(super) fn increment(value: u64) -> Result<u64, GpuSceneError> {
    value.checked_add(1).ok_or(GpuSceneError::RevisionOverflow)
}

/// Complete shared-table snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSceneSharedSnapshot {
    /// Prototype slots.
    pub prototypes: GpuSceneTableSnapshot<GpuScenePrototypeRecord>,
    /// Material slots.
    pub materials: GpuSceneTableSnapshot<GpuSceneMaterialRecord>,
    /// Deformation slots.
    pub deformations: GpuSceneTableSnapshot<GpuSceneDeformationRecord>,
    /// SDF slots.
    pub sdfs: GpuSceneTableSnapshot<GpuSceneSdfRecord>,
    /// Page slots.
    pub pages: GpuSceneTableSnapshot<GpuScenePageRecord>,
}

/// Complete snapshot of one per-world store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuSceneWorldSnapshot {
    /// Caller-owned world identifier.
    pub id: GpuSceneWorldId,
    /// Instance slots.
    pub instances: GpuSceneTableSnapshot<GpuSceneInstanceRecord>,
    /// Light slots.
    pub lights: GpuSceneTableSnapshot<GpuSceneLightRecord>,
    /// Last derived world revision.
    pub revision: u64,
}

/// Reconstructible persistent GPU-scene snapshot; view history is excluded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistentGpuSceneSnapshot {
    /// Shared immutable records.
    pub shared: GpuSceneSharedSnapshot,
    /// Independent per-world records.
    pub worlds: Vec<GpuSceneWorldSnapshot>,
    /// Last derived shared revision.
    pub revision: u64,
}

/// One moved instance's conservative swept world AABB (min, max).
pub type GpuSceneMovedBounds = ([f32; 3], [f32; 3]);

/// Persistent derived render mirror over canonical ECS, asset, and vegetation state.
pub struct PersistentGpuScene {
    pub(super) shared: GpuSceneSharedRecords,
    pub(super) worlds: BTreeMap<GpuSceneWorldId, GpuSceneWorld>,
    pub(super) views: BTreeMap<GpuSceneViewId, GpuSceneViewState>,
    pub(super) uploads: GpuSceneUploadRing,
    pub(super) revision: u64,
    /// Swept world AABBs — one per hierarchy page — of instances created, updated, or removed
    /// since the last [`PersistentGpuScene::take_moved_bounds`]; the shadow system dirties the
    /// virtual pages these overlap.
    pub(super) moved_bounds: Vec<GpuSceneMovedBounds>,
    /// Instances the list covers. Capped; past the cap only the overflow flag grows.
    pub(super) moved_instances: usize,
    pub(super) moved_overflow: bool,
}

impl PersistentGpuScene {
    /// Creates an empty mirror with explicit bounded upload limits.
    pub fn new(upload_limits: GpuSceneUploadLimits) -> Result<Self, GpuSceneError> {
        Ok(Self {
            shared: GpuSceneSharedRecords::default(),
            worlds: BTreeMap::new(),
            views: BTreeMap::new(),
            uploads: GpuSceneUploadRing::new(upload_limits)?,
            revision: 0,
            moved_bounds: Vec::new(),
            moved_instances: 0,
            moved_overflow: false,
        })
    }

    /// Reconstructs the complete derived mirror and queues a full table upload.
    pub fn from_snapshot(
        snapshot: PersistentGpuSceneSnapshot,
        upload_limits: GpuSceneUploadLimits,
    ) -> Result<Self, GpuSceneError> {
        let shared = GpuSceneSharedRecords {
            prototypes: SceneTable::from_snapshot(snapshot.shared.prototypes)?,
            materials: SceneTable::from_snapshot(snapshot.shared.materials)?,
            deformations: SceneTable::from_snapshot(snapshot.shared.deformations)?,
            sdfs: SceneTable::from_snapshot(snapshot.shared.sdfs)?,
            pages: SceneTable::from_snapshot(snapshot.shared.pages)?,
        };
        let mut worlds = BTreeMap::new();
        for world in snapshot.worlds {
            let id = world.id;
            if world.revision > snapshot.revision {
                return Err(GpuSceneError::InvalidSnapshot(
                    "world revision exceeds shared revision",
                ));
            }
            if worlds
                .insert(
                    id,
                    GpuSceneWorld {
                        instances: SceneTable::from_snapshot(world.instances)?,
                        lights: SceneTable::from_snapshot(world.lights)?,
                        revision: world.revision,
                    },
                )
                .is_some()
            {
                return Err(GpuSceneError::InvalidSnapshot("duplicate world identifier"));
            }
        }
        let mut scene = Self {
            shared,
            worlds,
            views: BTreeMap::new(),
            uploads: GpuSceneUploadRing::new(upload_limits)?,
            revision: snapshot.revision,
            moved_bounds: Vec::new(),
            moved_instances: 0,
            moved_overflow: false,
        };
        scene.validate_all_references()?;
        scene.queue_full_snapshot()?;
        Ok(scene)
    }

    /// Captures all derived shared and per-world records without view-local history.
    #[must_use]
    pub fn snapshot(&self) -> PersistentGpuSceneSnapshot {
        PersistentGpuSceneSnapshot {
            shared: GpuSceneSharedSnapshot {
                prototypes: self.shared.prototypes.snapshot(),
                materials: self.shared.materials.snapshot(),
                deformations: self.shared.deformations.snapshot(),
                sdfs: self.shared.sdfs.snapshot(),
                pages: self.shared.pages.snapshot(),
            },
            worlds: self
                .worlds
                .iter()
                .map(|(&id, world)| GpuSceneWorldSnapshot {
                    id,
                    instances: world.instances.snapshot(),
                    lights: world.lights.snapshot(),
                    revision: world.revision,
                })
                .collect(),
            revision: self.revision,
        }
    }

    /// Current shared-scene revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current revision of one world.
    pub fn world_revision(&self, world: GpuSceneWorldId) -> Result<u64, GpuSceneError> {
        self.worlds
            .get(&world)
            .map(|world| world.revision)
            .ok_or(GpuSceneError::MissingWorld(world))
    }

    /// Creates an empty caller-keyed world store.
    pub fn create_world(&mut self, world: GpuSceneWorldId) -> Result<(), GpuSceneError> {
        if self.worlds.contains_key(&world) {
            return Err(GpuSceneError::DuplicateWorld(world));
        }
        self.worlds.insert(world, GpuSceneWorld::default());
        Ok(())
    }

    /// Removes an empty world that has no attached view.
    pub fn remove_world(&mut self, world: GpuSceneWorldId) -> Result<(), GpuSceneError> {
        let value = self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?;
        if !value.instances.is_empty()
            || !value.lights.is_empty()
            || self.views.values().any(|view| view.world == world)
        {
            return Err(GpuSceneError::WorldNotEmpty(world));
        }
        self.worlds.remove(&world);
        Ok(())
    }

    /// Creates independent temporal state for one view over an existing world.
    pub fn create_view(
        &mut self,
        view: GpuSceneViewId,
        world: GpuSceneWorldId,
    ) -> Result<(), GpuSceneError> {
        if !self.worlds.contains_key(&world) {
            return Err(GpuSceneError::MissingWorld(world));
        }
        if self.views.contains_key(&view) {
            return Err(GpuSceneError::DuplicateView(view));
        }
        self.views.insert(view, GpuSceneViewState::new(world));
        Ok(())
    }

    /// Removes view-local state without touching shared or per-world records.
    pub fn remove_view(&mut self, view: GpuSceneViewId) -> Result<(), GpuSceneError> {
        self.views
            .remove(&view)
            .map(|_| ())
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns immutable view-local state.
    pub fn view(&self, view: GpuSceneViewId) -> Result<&GpuSceneViewState, GpuSceneError> {
        self.views
            .get(&view)
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns mutable view-local state without exposing scene-table mutation.
    pub fn view_mut(
        &mut self,
        view: GpuSceneViewId,
    ) -> Result<&mut GpuSceneViewState, GpuSceneError> {
        self.views
            .get_mut(&view)
            .ok_or(GpuSceneError::MissingView(view))
    }

    /// Returns one live prototype after generation validation.
    #[must_use]
    pub fn prototype(&self, handle: GpuScenePrototypeHandle) -> Option<&GpuScenePrototypeRecord> {
        self.shared.prototypes.get(handle)
    }

    /// Returns one live material after generation validation.
    #[must_use]
    pub fn material(&self, handle: GpuSceneMaterialHandle) -> Option<&GpuSceneMaterialRecord> {
        self.shared.materials.get(handle)
    }

    /// Returns one live deformation provider after generation validation.
    #[must_use]
    pub fn deformation(
        &self,
        handle: GpuSceneDeformationHandle,
    ) -> Option<&GpuSceneDeformationRecord> {
        self.shared.deformations.get(handle)
    }

    /// Returns one live SDF reference after generation validation.
    #[must_use]
    pub fn sdf(&self, handle: GpuSceneSdfHandle) -> Option<&GpuSceneSdfRecord> {
        self.shared.sdfs.get(handle)
    }

    /// Returns one live page after generation validation.
    #[must_use]
    pub fn page(&self, handle: GpuScenePageHandle) -> Option<&GpuScenePageRecord> {
        self.shared.pages.get(handle)
    }

    /// Iterates prototypes in stable slot order.
    pub fn prototypes(
        &self,
    ) -> impl Iterator<Item = (GpuScenePrototypeHandle, &GpuScenePrototypeRecord)> {
        self.shared.prototypes.iter()
    }

    /// Iterates materials in stable slot order.
    pub fn materials(
        &self,
    ) -> impl Iterator<Item = (GpuSceneMaterialHandle, &GpuSceneMaterialRecord)> {
        self.shared.materials.iter()
    }

    /// Iterates pages in stable slot order.
    pub fn pages(&self) -> impl Iterator<Item = (GpuScenePageHandle, &GpuScenePageRecord)> {
        self.shared.pages.iter()
    }

    /// Returns one live instance from a selected world.
    #[must_use]
    pub fn instance(
        &self,
        world: GpuSceneWorldId,
        handle: GpuSceneInstanceHandle,
    ) -> Option<&GpuSceneInstanceRecord> {
        self.worlds
            .get(&world)
            .and_then(|world| world.instances.get(handle))
    }

    /// Returns one live light from a selected world.
    #[must_use]
    pub fn light(
        &self,
        world: GpuSceneWorldId,
        handle: GpuSceneLightHandle,
    ) -> Option<&GpuSceneLightRecord> {
        self.worlds
            .get(&world)
            .and_then(|world| world.lights.get(handle))
    }

    /// Iterates instances in one world in stable slot order.
    pub fn instances(
        &self,
        world: GpuSceneWorldId,
    ) -> Result<
        impl Iterator<Item = (GpuSceneInstanceHandle, &GpuSceneInstanceRecord)>,
        GpuSceneError,
    > {
        Ok(self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?
            .instances
            .iter())
    }

    /// Iterates lights in one world in stable slot order.
    pub fn lights(
        &self,
        world: GpuSceneWorldId,
    ) -> Result<impl Iterator<Item = (GpuSceneLightHandle, &GpuSceneLightRecord)>, GpuSceneError>
    {
        Ok(self
            .worlds
            .get(&world)
            .ok_or(GpuSceneError::MissingWorld(world))?
            .lights
            .iter())
    }

    /// Releases one completed frame slot for handle reuse and bounded upload staging.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<(), GpuSceneError> {
        frame_bit(completed_frame_slot)?;
        self.shared.prototypes.begin_frame(completed_frame_slot)?;
        self.shared.materials.begin_frame(completed_frame_slot)?;
        self.shared.deformations.begin_frame(completed_frame_slot)?;
        self.shared.sdfs.begin_frame(completed_frame_slot)?;
        self.shared.pages.begin_frame(completed_frame_slot)?;
        for world in self.worlds.values_mut() {
            world.instances.begin_frame(completed_frame_slot)?;
            world.lights.begin_frame(completed_frame_slot)?;
        }
        self.uploads.begin_frame(completed_frame_slot)
    }

    /// Stages the next coalesced batch within batch and frame-slot limits.
    pub fn stage_upload_batch(
        &mut self,
        frame_slot: usize,
    ) -> Result<GpuSceneUploadBatch, GpuSceneError> {
        self.uploads.stage(frame_slot)
    }

    pub(super) fn queue_tombstone(
        &mut self,
        target: GpuSceneUploadTarget,
        handle: GpuHandle,
        revision: u64,
    ) -> Result<(), GpuSceneError> {
        self.uploads
            .enqueue(target, handle, revision, GpuSceneUploadPayload::Tombstone)
    }

    pub(super) fn ensure_payload_fits(
        &self,
        target: GpuSceneUploadTarget,
        payload: &GpuSceneUploadPayload,
    ) -> Result<(), GpuSceneError> {
        let bytes = payload.byte_len();
        if bytes > self.uploads.limits.max_batch_bytes {
            return Err(GpuSceneError::RecordExceedsBatch {
                target,
                bytes,
                limit: self.uploads.limits.max_batch_bytes,
            });
        }
        Ok(())
    }

    fn queue_full_snapshot(&mut self) -> Result<(), GpuSceneError> {
        let revision = self.revision;
        let prototypes: Vec<_> = self
            .shared
            .prototypes
            .iter()
            .map(|(handle, record)| (handle.raw(), record.clone()))
            .collect();
        let materials: Vec<_> = self
            .shared
            .materials
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let deformations: Vec<_> = self
            .shared
            .deformations
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let sdfs: Vec<_> = self
            .shared
            .sdfs
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        let pages: Vec<_> = self
            .shared
            .pages
            .iter()
            .map(|(handle, record)| (handle.raw(), *record))
            .collect();
        for (handle, record) in prototypes {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Prototype,
                handle,
                revision,
                GpuSceneUploadPayload::Prototype(record),
            )?;
        }
        for (handle, record) in materials {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Material,
                handle,
                revision,
                GpuSceneUploadPayload::Material(record),
            )?;
        }
        for (handle, record) in deformations {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Deformation,
                handle,
                revision,
                GpuSceneUploadPayload::Deformation(record),
            )?;
        }
        for (handle, record) in sdfs {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Sdf,
                handle,
                revision,
                GpuSceneUploadPayload::Sdf(record),
            )?;
        }
        for (handle, record) in pages {
            self.uploads.enqueue(
                GpuSceneUploadTarget::Page,
                handle,
                revision,
                GpuSceneUploadPayload::Page(record),
            )?;
        }
        let worlds: Vec<_> = self
            .worlds
            .iter()
            .map(|(&id, world)| {
                let instances = world
                    .instances
                    .iter()
                    .map(|(handle, record)| (handle.raw(), record.clone()))
                    .collect::<Vec<_>>();
                let lights = world
                    .lights
                    .iter()
                    .map(|(handle, record)| (handle.raw(), *record))
                    .collect::<Vec<_>>();
                (id, instances, lights)
            })
            .collect();
        for (world, instances, lights) in worlds {
            for (handle, record) in instances {
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world),
                    handle,
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
            }
            for (handle, record) in lights {
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world),
                    handle,
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
            }
        }
        Ok(())
    }
}
