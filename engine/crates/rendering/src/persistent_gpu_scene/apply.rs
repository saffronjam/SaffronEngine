use super::validate::{referenced, validate_device_handle, validate_light};
use super::*;

impl PersistentGpuScene {
    /// Applies one validated shared-record delta and queues its coalesced upload.
    pub fn apply_shared_delta(
        &mut self,
        delta: GpuSceneSharedDelta,
    ) -> Result<GpuSceneSharedDeltaResult, GpuSceneError> {
        let revision = increment(self.revision)?;
        let result = match delta {
            GpuSceneSharedDelta::CreatePrototype(record) => {
                self.validate_prototype(&record)?;
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Prototype,
                    &GpuSceneUploadPayload::Prototype(record.clone()),
                )?;
                let handle = self.shared.prototypes.insert(record.clone(), "prototype")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Prototype,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Prototype(record),
                )?;
                GpuSceneSharedDeltaResult::PrototypeCreated(handle)
            }
            GpuSceneSharedDelta::UpdatePrototype { handle, record } => {
                self.validate_prototype(&record)?;
                for instance in self
                    .worlds
                    .values()
                    .flat_map(|world| world.instances.iter())
                    .filter_map(|(_, instance)| (instance.prototype == handle).then_some(instance))
                {
                    self.validate_instance_against_prototype(instance, &record)?;
                }
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Prototype,
                    &GpuSceneUploadPayload::Prototype(record.clone()),
                )?;
                self.shared
                    .prototypes
                    .update(handle, record.clone(), "prototype")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Prototype,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Prototype(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemovePrototype(handle) => {
                if self
                    .worlds
                    .values()
                    .flat_map(|world| world.instances.iter())
                    .any(|(_, instance)| instance.prototype == handle)
                {
                    return Err(referenced("prototype", handle.raw()));
                }
                self.shared.prototypes.remove(handle, "prototype")?;
                self.queue_tombstone(GpuSceneUploadTarget::Prototype, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateMaterial(record) => {
                validate_device_handle(record.table, "material")?;
                let handle = self.shared.materials.insert(record, "material")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Material,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Material(record),
                )?;
                GpuSceneSharedDeltaResult::MaterialCreated(handle)
            }
            GpuSceneSharedDelta::UpdateMaterial { handle, record } => {
                validate_device_handle(record.table, "material")?;
                self.shared.materials.update(handle, record, "material")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Material,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Material(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveMaterial(handle) => {
                if self.material_is_referenced(handle) {
                    return Err(referenced("material", handle.raw()));
                }
                self.shared.materials.remove(handle, "material")?;
                self.queue_tombstone(GpuSceneUploadTarget::Material, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateDeformation(record) => {
                validate_device_handle(record.provider, "deformation")?;
                let handle = self.shared.deformations.insert(record, "deformation")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Deformation,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Deformation(record),
                )?;
                GpuSceneSharedDeltaResult::DeformationCreated(handle)
            }
            GpuSceneSharedDelta::UpdateDeformation { handle, record } => {
                validate_device_handle(record.provider, "deformation")?;
                self.shared
                    .deformations
                    .update(handle, record, "deformation")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Deformation,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Deformation(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveDeformation(handle) => {
                if self.deformation_is_referenced(handle) {
                    return Err(referenced("deformation", handle.raw()));
                }
                self.shared.deformations.remove(handle, "deformation")?;
                self.queue_tombstone(GpuSceneUploadTarget::Deformation, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreateSdf(record) => {
                validate_device_handle(record.resource, "SDF")?;
                let handle = self.shared.sdfs.insert(record, "SDF")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Sdf,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Sdf(record),
                )?;
                GpuSceneSharedDeltaResult::SdfCreated(handle)
            }
            GpuSceneSharedDelta::UpdateSdf { handle, record } => {
                validate_device_handle(record.resource, "SDF")?;
                self.shared.sdfs.update(handle, record, "SDF")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Sdf,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Sdf(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemoveSdf(handle) => {
                if self.sdf_is_referenced(handle) {
                    return Err(referenced("SDF", handle.raw()));
                }
                self.shared.sdfs.remove(handle, "SDF")?;
                self.queue_tombstone(GpuSceneUploadTarget::Sdf, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
            GpuSceneSharedDelta::CreatePage(record) => {
                self.validate_page(None, &record)?;
                let handle = self.shared.pages.insert(record, "page")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Page,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Page(record),
                )?;
                GpuSceneSharedDeltaResult::PageCreated(handle)
            }
            GpuSceneSharedDelta::UpdatePage { handle, record } => {
                self.validate_page(Some(handle), &record)?;
                self.shared.pages.update(handle, record, "page")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Page,
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Page(record),
                )?;
                GpuSceneSharedDeltaResult::Updated
            }
            GpuSceneSharedDelta::RemovePage(handle) => {
                if self.page_is_referenced(handle) {
                    return Err(referenced("page", handle.raw()));
                }
                self.shared.pages.remove(handle, "page")?;
                self.queue_tombstone(GpuSceneUploadTarget::Page, handle.raw(), revision)?;
                GpuSceneSharedDeltaResult::Removed
            }
        };
        self.revision = revision;
        Ok(result)
    }

    /// One instance's swept world boxes: its prototype's cooked per-page swept bounds pushed
    /// through the transform, one box per page, appended to `sink`.
    ///
    /// Per page rather than per instance because the consumer dirties a page grid: a single box
    /// over a tall sparse canopy covers many times the footprint its clusters occupy, and every
    /// extra cell it touches is a shadow page re-rendered for geometry that is not there. Both
    /// matrices of a dynamic transform contribute, so the boxes sweep the frame's motion, and the
    /// page bounds are the cook's DEFORMED extent, so they already cover the deformation the
    /// payload can reach. A prototype that cooked no hierarchy falls back to its bounding sphere,
    /// which is all such a mesh has.
    fn instance_moved_bounds(
        &self,
        record: &GpuSceneInstanceRecord,
        sink: &mut Vec<GpuSceneMovedBounds>,
    ) -> usize {
        let Some(prototype) = self.shared.prototypes.get(record.prototype) else {
            return 0;
        };
        let sphere = prototype.bounds;
        let reach = (sphere[0].powi(2) + sphere[1].powi(2) + sphere[2].powi(2)).sqrt() + sphere[3];
        let fallback = [GpuScenePageBounds {
            min: [sphere[0] - reach, sphere[1] - reach, sphere[2] - reach],
            max: [sphere[0] + reach, sphere[1] + reach, sphere[2] + reach],
        }];
        let pages: &[GpuScenePageBounds] = if prototype.page_bounds.is_empty() {
            &fallback
        } else {
            &prototype.page_bounds
        };
        let matrices: [saffron_geometry::glam::Mat4; 2] = match &record.transform {
            GpuSceneTransform::Static(transform) => {
                let matrix = transform.to_matrix();
                [matrix, matrix]
            }
            GpuSceneTransform::Dynamic(transform) => [transform.current, transform.previous],
        };
        let mut written = 0;
        for page in pages {
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for matrix in &matrices {
                let mut corner_min = saffron_geometry::glam::Vec3::splat(f32::MAX);
                let mut corner_max = saffron_geometry::glam::Vec3::splat(f32::MIN);
                saffron_geometry::world_aabb_from_corners(
                    matrix,
                    saffron_geometry::glam::Vec3::from(page.min),
                    saffron_geometry::glam::Vec3::from(page.max),
                    &mut corner_min,
                    &mut corner_max,
                );
                for axis in 0..3 {
                    min[axis] = min[axis].min(corner_min[axis]);
                    max[axis] = max[axis].max(corner_max[axis]);
                }
            }
            if min[0].is_finite() && max[0].is_finite() {
                sink.push((min, max));
                written += 1;
            }
        }
        written
    }

    /// Records one instance's swept world boxes into the frame's moved list.
    ///
    /// The cap counts INSTANCES, not boxes: a page-granular expansion makes the box count a
    /// property of how finely the moved content is cooked, and a per-box cap would turn a
    /// finely-paged scene into a blanket invalidation while a coarsely-paged one of the same size
    /// stayed exact.
    fn note_moved(&mut self, record: Option<&GpuSceneInstanceRecord>) {
        const MOVED_INSTANCE_CAP: usize = 4_096;
        let Some(record) = record else {
            return;
        };
        if self.moved_instances >= MOVED_INSTANCE_CAP {
            self.moved_overflow = true;
            return;
        }
        let mut boxes = std::mem::take(&mut self.moved_bounds);
        if self.instance_moved_bounds(record, &mut boxes) > 0 {
            self.moved_instances += 1;
        }
        self.moved_bounds = boxes;
    }

    /// Notes live instances whose GPU-deformed content changed this frame (compute
    /// skinning writes new vertices without a scene delta), so their shadow pages
    /// re-render.
    pub fn note_instances_moved(
        &mut self,
        world_id: GpuSceneWorldId,
        handles: &[GpuSceneInstanceHandle],
    ) {
        for handle in handles {
            let record = self
                .worlds
                .get(&world_id)
                .and_then(|world| world.instances.get(*handle))
                .cloned();
            self.note_moved(record.as_ref());
        }
    }

    /// Drains the accumulated moved-instance bounds and the overflow flag.
    pub fn take_moved_bounds(&mut self) -> (Vec<GpuSceneMovedBounds>, bool) {
        let overflow = std::mem::take(&mut self.moved_overflow);
        self.moved_instances = 0;
        (std::mem::take(&mut self.moved_bounds), overflow)
    }

    pub fn apply_world_delta(
        &mut self,
        world_id: GpuSceneWorldId,
        delta: GpuSceneWorldDelta,
    ) -> Result<GpuSceneWorldDeltaResult, GpuSceneError> {
        match &delta {
            GpuSceneWorldDelta::CreateInstance(record)
            | GpuSceneWorldDelta::UpdateInstance { record, .. } => {
                self.validate_instance(record)?;
                self.ensure_payload_fits(
                    GpuSceneUploadTarget::Instance(world_id),
                    &GpuSceneUploadPayload::Instance(record.clone()),
                )?;
            }
            GpuSceneWorldDelta::CreateLight(record)
            | GpuSceneWorldDelta::UpdateLight { record, .. } => validate_light(record)?,
            GpuSceneWorldDelta::RemoveInstance(_) | GpuSceneWorldDelta::RemoveLight(_) => {}
        }
        let revision = increment(self.revision)?;
        // The shadow pages an instance mutation overlaps re-render: note the OLD
        // record's bounds (update/remove reveal or vacate coverage) and the NEW
        // record's (create/update cast fresh coverage).
        let mut moved: Vec<GpuSceneInstanceRecord> = Vec::new();
        match &delta {
            GpuSceneWorldDelta::CreateInstance(record) => {
                moved.push(record.clone());
            }
            GpuSceneWorldDelta::UpdateInstance { handle, record } => {
                if let Some(old) = self
                    .worlds
                    .get(&world_id)
                    .and_then(|world| world.instances.get(*handle))
                {
                    moved.push(old.clone());
                }
                moved.push(record.clone());
            }
            GpuSceneWorldDelta::RemoveInstance(handle) => {
                if let Some(old) = self
                    .worlds
                    .get(&world_id)
                    .and_then(|world| world.instances.get(*handle))
                {
                    moved.push(old.clone());
                }
            }
            GpuSceneWorldDelta::CreateLight(_)
            | GpuSceneWorldDelta::UpdateLight { .. }
            | GpuSceneWorldDelta::RemoveLight(_) => {}
        }
        for record in &moved {
            self.note_moved(Some(record));
        }
        let world = self
            .worlds
            .get_mut(&world_id)
            .ok_or(GpuSceneError::MissingWorld(world_id))?;
        let result = match delta {
            GpuSceneWorldDelta::CreateInstance(record) => {
                let handle = world.instances.insert(record.clone(), "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
                GpuSceneWorldDeltaResult::InstanceCreated(handle)
            }
            GpuSceneWorldDelta::UpdateInstance { handle, record } => {
                world.instances.update(handle, record.clone(), "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Instance(record),
                )?;
                GpuSceneWorldDeltaResult::Updated
            }
            GpuSceneWorldDelta::RemoveInstance(handle) => {
                world.instances.remove(handle, "instance")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Instance(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Tombstone,
                )?;
                GpuSceneWorldDeltaResult::Removed
            }
            GpuSceneWorldDelta::CreateLight(record) => {
                let handle = world.lights.insert(record, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
                GpuSceneWorldDeltaResult::LightCreated(handle)
            }
            GpuSceneWorldDelta::UpdateLight { handle, record } => {
                world.lights.update(handle, record, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Light(record),
                )?;
                GpuSceneWorldDeltaResult::Updated
            }
            GpuSceneWorldDelta::RemoveLight(handle) => {
                world.lights.remove(handle, "light")?;
                self.uploads.enqueue(
                    GpuSceneUploadTarget::Light(world_id),
                    handle.raw(),
                    revision,
                    GpuSceneUploadPayload::Tombstone,
                )?;
                GpuSceneWorldDeltaResult::Removed
            }
        };
        world.revision = revision;
        self.revision = revision;
        Ok(result)
    }
}
