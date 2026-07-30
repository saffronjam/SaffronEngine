//! The [`RenderGraph`] itself: pass registration, batching into submissions, and recording
//! the derived barriers plus each pass body into command buffers.

use super::*;

/// A frame's render graph: imported resources plus the passes over them. Rebuilt
/// every frame (cheap) and recorded by [`RenderGraph::execute`].
#[derive(Default)]
pub struct RenderGraph {
    pub(super) resources: Vec<RgResourceState>,
    pub(super) passes: Vec<RgPass>,
    pub(super) external_states: Vec<RgExternalState>,
    pub(super) external_buffer_states: Vec<RgExternalBufferState>,
}

impl RenderGraph {
    /// A fresh, empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a cross-frame image-state slot for [`RenderGraph::import_image`].
    pub fn alloc_external_state(&mut self, initial: RgExternalState) -> usize {
        self.external_states.push(initial);
        self.external_states.len() - 1
    }

    /// The resolved image state currently in a slot.
    pub fn external_state(&self, slot: usize) -> RgExternalState {
        self.external_states[slot]
    }

    /// Allocates a cross-frame buffer-state slot for [`RenderGraph::import_buffer`].
    pub fn alloc_external_buffer_state(&mut self, initial: RgExternalBufferState) -> usize {
        self.external_buffer_states.push(initial);
        self.external_buffer_states.len() - 1
    }

    /// The resolved byte-range state currently in a buffer slot.
    pub fn external_buffer_state(&self, slot: usize) -> RgExternalBufferState {
        self.external_buffer_states[slot].clone()
    }

    /// Imports an external image (offscreen/swapchain target). When `external` is
    /// set, the slot's layout seeds the entry layout and receives the resolved
    /// layout after execute, so the image's layout carries across frames.
    pub fn import_image(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        aspect: vk::ImageAspectFlags,
        initial_layout: vk::ImageLayout,
        external: Option<usize>,
    ) -> RgResource {
        let mut r = RgResourceState {
            is_image: true,
            image,
            view,
            aspect,
            layout: initial_layout,
            external_state: external,
            ..RgResourceState::default()
        };
        if let Some(slot) = external {
            let state = self.external_states[slot];
            r.layout = state.layout;
            r.last_stage = state.stage;
            r.last_access = state.access;
            r.last_was_write = state.was_write;
            r.touched = state.touched;
            r.queue_family = state.queue_family;
            r.queue = state.queue;
        } else {
            seed_image_state(&mut r);
        }
        self.resources.push(r);
        RgResource {
            index: (self.resources.len() - 1) as u32,
        }
    }

    /// Imports an external 3D image (e.g. a GDF cascade volume). Tracked identically
    /// to a 2D image for barrier purposes — the barrier transitions the whole image
    /// and dimensionality is irrelevant.
    pub fn import_image_3d(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        initial_layout: vk::ImageLayout,
        external: Option<usize>,
    ) -> RgResource {
        self.import_image(
            image,
            view,
            vk::ImageAspectFlags::COLOR,
            initial_layout,
            external,
        )
    }

    /// Imports an external buffer produced and/or consumed within the frame.
    pub fn import_buffer(&mut self, buffer: vk::Buffer, external: Option<usize>) -> RgResource {
        let resource = self.register_buffer(RgBufferResource {
            buffer,
            size: vk::WHOLE_SIZE,
            usage: vk::BufferUsageFlags::empty(),
            lifetime: RgBufferLifetime::Imported,
        });
        if let Some(slot) = external {
            let state = &mut self.resources[resource.index as usize];
            state.external_buffer_state = Some(slot);
            state.buffer_accesses = self.external_buffer_states[slot].accesses.clone();
        }
        resource
    }

    /// Registers an imported or graph-allocated buffer in the unified resource table.
    ///
    /// A graph allocator supplies transient and persistent handles with their exact size and
    /// creation usages. External buffers use [`RgBufferLifetime::Imported`].
    pub fn register_buffer(&mut self, resource: RgBufferResource) -> RgResource {
        assert!(resource.size != 0, "render-graph buffers must be non-empty");
        if let Some((index, state)) = self
            .resources
            .iter_mut()
            .enumerate()
            .find(|(_, state)| !state.is_image && state.buffer == resource.buffer)
        {
            if state.buffer_lifetime == RgBufferLifetime::Imported {
                state.buffer_lifetime = resource.lifetime;
            } else {
                assert!(
                    state.buffer_lifetime == resource.lifetime
                        || resource.lifetime == RgBufferLifetime::Imported,
                    "one Vulkan buffer cannot have two graph-owned lifetimes"
                );
            }
            if state.buffer_size == vk::WHOLE_SIZE {
                state.buffer_size = resource.size;
            } else if resource.size != vk::WHOLE_SIZE {
                assert_eq!(
                    state.buffer_size, resource.size,
                    "one Vulkan buffer cannot have conflicting declared sizes"
                );
            }
            state.buffer_usage |= resource.usage;
            return RgResource {
                index: index as u32,
            };
        }
        let r = RgResourceState {
            is_image: false,
            buffer: resource.buffer,
            buffer_size: resource.size,
            buffer_usage: resource.usage,
            buffer_lifetime: resource.lifetime,
            ..RgResourceState::default()
        };
        self.resources.push(r);
        RgResource {
            index: (self.resources.len() - 1) as u32,
        }
    }

    /// Allocates and registers a graph-owned buffer through the frame-safe resource pool.
    pub fn create_buffer(
        &mut self,
        resources: &mut crate::RenderGraphResources,
        frame: usize,
        key: &'static str,
        desc: RgBufferDesc,
    ) -> crate::Result<RgResource> {
        let resource = resources.allocate_graph_buffer(frame, key, desc)?;
        let handle = resource.buffer;
        let graph_resource = self.register_buffer(resource);
        let index = graph_resource.index as usize;
        if self.resources[index].external_buffer_state.is_none() {
            let slot = self.alloc_external_buffer_state(resources.buffer_state(handle));
            self.resources[index].external_buffer_state = Some(slot);
            self.resources[index].buffer_accesses =
                self.external_buffer_states[slot].accesses.clone();
        }
        Ok(graph_resource)
    }

    /// Appends a pass to the graph.
    pub fn add_pass(&mut self, pass: RgPass) {
        self.passes.push(pass);
    }

    /// The underlying image handle of an imaged resource (null for a buffer
    /// resource). Pass bodies resolve handles through the graph rather than
    /// recapturing the renderer aggregate.
    pub fn image(&self, resource: RgResource) -> vk::Image {
        self.resources[resource.index as usize].image
    }

    /// The underlying image-view handle of an imaged resource (null for a buffer).
    pub fn view(&self, resource: RgResource) -> vk::ImageView {
        self.resources[resource.index as usize].view
    }

    /// The underlying buffer handle of a buffer resource (null for an image).
    pub fn buffer(&self, resource: RgResource) -> vk::Buffer {
        self.resources[resource.index as usize].buffer
    }

    /// Exact declaration for a registered buffer resource, or `None` for an image.
    pub fn buffer_resource(&self, resource: RgResource) -> Option<RgBufferResource> {
        let state = &self.resources[resource.index as usize];
        (!state.is_image).then_some(RgBufferResource {
            buffer: state.buffer,
            size: state.buffer_size,
            usage: state.buffer_usage,
            lifetime: state.buffer_lifetime,
        })
    }

    pub(crate) fn resolved_graph_buffer_states(&self) -> Vec<(vk::Buffer, RgExternalBufferState)> {
        self.resources
            .iter()
            .filter(|resource| {
                !resource.is_image && resource.buffer_lifetime != RgBufferLifetime::Imported
            })
            .filter_map(|resource| {
                resource
                    .external_buffer_state
                    .map(|slot| (resource.buffer, self.external_buffer_states[slot].clone()))
            })
            .collect()
    }

    /// Resolves queue preferences without changing pass declarations.
    pub fn queue_assignments(&self, families: RgQueueFamilies) -> Vec<RgQueueAssignment> {
        self.passes
            .iter()
            .enumerate()
            .map(|(index, pass)| self.resolve_queue(pass, index, families).0)
            .collect()
    }

    pub(super) fn resolve_queue(
        &self,
        pass: &RgPass,
        pass_index: usize,
        families: RgQueueFamilies,
    ) -> (RgQueueAssignment, u32) {
        let resolved = families.resolve(pass);
        if resolved.0 == RgQueueAssignment::AsyncCompute
            && (pass.accesses.is_empty()
                || !pass.accesses.iter().all(|access| {
                    let resource = &self.resources[access.resource.index as usize];
                    if resource.is_image {
                        (resource.external_state.is_some() && resource.queue.is_some())
                            || !resource.touched
                            || self.prior_pass_touches(access.resource, pass_index)
                    } else {
                        resource.buffer_lifetime != RgBufferLifetime::Imported
                            || (resource.external_buffer_state.is_some()
                                && buffer_state_covers(resource, access.buffer_range))
                            || self.prior_pass_covers_buffer(
                                access.resource,
                                access.buffer_range,
                                pass_index,
                            )
                    }
                }))
        {
            return (RgQueueAssignment::Graphics, families.graphics);
        }
        resolved
    }

    pub(super) fn prior_pass_touches(&self, resource: RgResource, pass_index: usize) -> bool {
        self.passes[..pass_index].iter().any(|pass| {
            pass.accesses
                .iter()
                .any(|access| access.resource == resource)
                || pass.colors.iter().any(|attachment| {
                    attachment.resource == resource || attachment.resolve == Some(resource)
                })
                || pass.depth.as_ref().is_some_and(|attachment| {
                    attachment.resource == resource || attachment.resolve == Some(resource)
                })
        })
    }

    pub(super) fn prior_pass_covers_buffer(
        &self,
        resource: RgResource,
        range: Option<RgBufferRange>,
        pass_index: usize,
    ) -> bool {
        let state = &self.resources[resource.index as usize];
        let (start, end) = buffer_range_bounds(state.buffer_size, range);
        let mut spans = self.passes[..pass_index]
            .iter()
            .flat_map(|pass| pass.accesses.iter())
            .filter(|access| access.resource == resource)
            .map(|access| buffer_range_bounds(state.buffer_size, access.buffer_range))
            .filter(|(span_start, span_end)| *span_end > start && *span_start < end)
            .map(|(span_start, span_end)| (span_start.max(start), span_end.min(end)))
            .collect::<Vec<_>>();
        spans.sort_unstable_by_key(|span| span.0);
        let mut cursor = start;
        for (span_start, span_end) in spans {
            if span_start > cursor {
                return false;
            }
            cursor = cursor.max(span_end);
            if cursor >= end {
                return true;
            }
        }
        false
    }

    /// Compiles pass-local synchronization and queue ownership for a queue topology.
    ///
    /// This pure preview leaves the graph's execution state untouched. The frame submitter uses
    /// the returned queue assignments, emits `after_*` releases on source queues, signals a
    /// timeline semaphore, then emits matching `before_*` acquires on destination queues.
    pub fn barrier_schedule(&self, families: RgQueueFamilies) -> Vec<RgPassBarriers> {
        self.compile_barriers(families).0
    }

    /// Compiles barriers into maximal contiguous queue batches.
    pub fn submission_plan(&self, families: RgQueueFamilies) -> RgSubmissionPlan {
        let (passes, exit_resources, entry_releases) = self.compile_barriers(families);
        let mut batches = Vec::<RgPassBatch>::new();
        let mut entry_batch_queues = Vec::<RgQueueAssignment>::new();
        for release in &entry_releases {
            assert_eq!(
                families.family(release.source_queue),
                Some(release.source_family),
                "persisted queue identity and family must match the device topology"
            );
            if entry_batch_queues.contains(&release.source_queue) {
                continue;
            }
            entry_batch_queues.push(release.source_queue);
            batches.push(RgPassBatch {
                queue: release.source_queue,
                passes: 0..0,
                wait_for_batches: Vec::new(),
                entry_release_images: Vec::new(),
                entry_release_buffers: Vec::new(),
            });
        }
        let mut pass_to_batch = Vec::with_capacity(passes.len());
        for (pass_index, pass) in passes.iter().enumerate() {
            let batch_index = if let Some(last) = batches.last_mut()
                && last.queue == pass.queue
            {
                last.passes.end = pass_index + 1;
                batches.len() - 1
            } else {
                batches.push(RgPassBatch {
                    queue: pass.queue,
                    passes: pass_index..pass_index + 1,
                    wait_for_batches: Vec::new(),
                    entry_release_images: Vec::new(),
                    entry_release_buffers: Vec::new(),
                });
                batches.len() - 1
            };
            pass_to_batch.push(batch_index);
        }
        for release in entry_releases {
            let release_batch = entry_batch_queues
                .iter()
                .position(|queue| *queue == release.source_queue)
                .expect("entry release queue was batched");
            match release.barrier {
                RgQueueReleaseBarrier::Image(barrier) => {
                    batches[release_batch].entry_release_images.push(barrier);
                }
                RgQueueReleaseBarrier::Buffer(barrier) => {
                    batches[release_batch].entry_release_buffers.push(barrier);
                }
                RgQueueReleaseBarrier::Dependency => {}
            }
            let destination_batch = pass_to_batch[release.destination_pass];
            if !batches[destination_batch]
                .wait_for_batches
                .contains(&release_batch)
            {
                batches[destination_batch]
                    .wait_for_batches
                    .push(release_batch);
            }
        }
        for (pass_index, pass) in passes.iter().enumerate() {
            let batch_index = pass_to_batch[pass_index];
            for &source_pass in &pass.wait_for_passes {
                let source_batch = pass_to_batch[source_pass];
                if source_batch != batch_index
                    && !batches[batch_index]
                        .wait_for_batches
                        .contains(&source_batch)
                {
                    batches[batch_index].wait_for_batches.push(source_batch);
                }
            }
        }
        RgSubmissionPlan {
            passes,
            batches,
            exit_resources,
        }
    }

    pub(super) fn compile_barriers(
        &self,
        families: RgQueueFamilies,
    ) -> (
        Vec<RgPassBarriers>,
        Vec<RgResourceState>,
        Vec<RgEntryRelease>,
    ) {
        let mut resources = self.resources.clone();
        let assignments = self
            .passes
            .iter()
            .enumerate()
            .map(|(index, pass)| self.resolve_queue(pass, index, families))
            .collect::<Vec<_>>();
        let mut schedule = assignments
            .iter()
            .map(|&(queue, queue_family)| RgPassBarriers {
                queue,
                queue_family,
                wait_for_passes: Vec::new(),
                before_images: Vec::new(),
                before_buffers: Vec::new(),
                after_images: Vec::new(),
                after_buffers: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut entry_releases = Vec::new();
        for (pass_index, pass) in self.passes.iter().enumerate() {
            let queue_family = assignments[pass_index].1;
            let barriers = derive_pass_barriers_for(
                &mut resources,
                pass,
                pass_index,
                assignments[pass_index].0,
                queue_family,
            );
            schedule[pass_index].before_images = barriers.image;
            schedule[pass_index].before_buffers = barriers.buffer;
            for release in barriers.releases {
                if let Some(source_pass) = release.pass {
                    if !schedule[pass_index].wait_for_passes.contains(&source_pass) {
                        schedule[pass_index].wait_for_passes.push(source_pass);
                    }
                    match release.barrier {
                        RgQueueReleaseBarrier::Image(barrier) => {
                            schedule[source_pass].after_images.push(barrier);
                        }
                        RgQueueReleaseBarrier::Buffer(barrier) => {
                            schedule[source_pass].after_buffers.push(barrier);
                        }
                        RgQueueReleaseBarrier::Dependency => {}
                    }
                } else {
                    entry_releases.push(RgEntryRelease {
                        destination_pass: pass_index,
                        source_family: release.source_family,
                        source_queue: release.source_queue,
                        barrier: release.barrier,
                    });
                }
            }
        }
        (schedule, resources, entry_releases)
    }

    /// Derives the barriers a pass needs from its declared accesses and attachments,
    /// advancing the resource table. Color/depth attachments are treated as the
    /// matching write usage; an MSAA resolve target is a second write of that kind.
    /// Pure logic — no GPU — so it is the unit-tested core.
    #[cfg(test)]
    pub(super) fn derive_pass_barriers(
        &mut self,
        pass: &RgPass,
        pass_index: usize,
        queue: RgQueueAssignment,
        queue_family: u32,
    ) -> DerivedBarriers {
        derive_pass_barriers_for(&mut self.resources, pass, pass_index, queue, queue_family)
    }
}

pub(super) fn derive_pass_barriers_for(
    resources: &mut [RgResourceState],
    pass: &RgPass,
    pass_index: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
) -> DerivedBarriers {
    let mut barriers = DerivedBarriers::default();
    for access in &pass.accesses {
        let resource = &resources[access.resource.index as usize];
        validate_declared_access(resource, access.usage);
        assert!(
            !resource.is_image || access.buffer_range.is_none(),
            "an image access cannot declare a buffer byte range"
        );
        apply_access_queued(
            &mut resources[access.resource.index as usize],
            usage_info(access.usage),
            access.buffer_range,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
    }
    for att in &pass.colors {
        assert!(
            resources[att.resource.index as usize].is_image,
            "a color attachment must be an image"
        );
        apply_access_queued(
            &mut resources[att.resource.index as usize],
            usage_info(RgUsage::ColorWrite),
            None,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
        if let Some(resolve) = att.resolve {
            assert!(
                resources[resolve.index as usize].is_image,
                "a color resolve attachment must be an image"
            );
            apply_access_queued(
                &mut resources[resolve.index as usize],
                usage_info(RgUsage::ColorWrite),
                None,
                pass_index,
                queue,
                queue_family,
                &mut barriers,
            );
        }
    }
    if let Some(depth) = &pass.depth {
        assert!(
            resources[depth.resource.index as usize].is_image,
            "a depth attachment must be an image"
        );
        apply_access_queued(
            &mut resources[depth.resource.index as usize],
            usage_info(RgUsage::DepthWrite),
            None,
            pass_index,
            queue,
            queue_family,
            &mut barriers,
        );
        if let Some(resolve) = depth.resolve {
            assert!(
                resources[resolve.index as usize].is_image,
                "a depth resolve attachment must be an image"
            );
            apply_access_queued(
                &mut resources[resolve.index as usize],
                usage_info(RgUsage::DepthWrite),
                None,
                pass_index,
                queue,
                queue_family,
                &mut barriers,
            );
        }
    }
    barriers
}

impl RenderGraph {
    /// Records a compiled multi-queue plan into one primary command buffer per batch.
    pub fn record_submission_plan_profiled(
        &mut self,
        device: &Device,
        plan: RgSubmissionPlan,
        commands: RgBatchCommandBuffers<'_>,
        recorders: &mut ProfileRecorders<'_>,
    ) -> crate::Result<Vec<RgRecordedBatch>> {
        self.record_compiled_plan_profiled(device, plan, commands, recorders, true)
    }

    pub(super) fn record_compiled_plan_profiled(
        &mut self,
        device: &Device,
        plan: RgSubmissionPlan,
        commands: RgBatchCommandBuffers<'_>,
        recorders: &mut ProfileRecorders<'_>,
        manage_command_buffers: bool,
    ) -> crate::Result<Vec<RgRecordedBatch>> {
        if commands.graphics.len() != plan.graphics_batch_count()
            || commands.compute.len() != plan.compute_batch_count()
            || plan.passes.len() != self.passes.len()
        {
            return Err(crate::Error::InvalidUploadData(
                "render-graph submission plan does not match its command buffers".to_owned(),
            ));
        }
        self.resources = plan.exit_resources;
        let raw = device.raw();
        let mut passes = std::mem::take(&mut self.passes)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut graphics_index = 0;
        let mut compute_index = 0;
        let mut recorded = Vec::with_capacity(plan.batches.len());
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        for batch in &plan.batches {
            let command_buffer = match batch.queue {
                RgQueueAssignment::Graphics => {
                    let command = commands.graphics[graphics_index];
                    graphics_index += 1;
                    command
                }
                RgQueueAssignment::AsyncCompute => {
                    let command = commands.compute[compute_index];
                    compute_index += 1;
                    command
                }
            };
            if manage_command_buffers {
                crate::checked(
                    unsafe { raw.begin_command_buffer(command_buffer, &begin) },
                    "begin render-graph batch",
                )?;
            }

            for pass_index in batch.passes.clone() {
                let pass = passes[pass_index]
                    .take()
                    .expect("each render-graph pass belongs to one batch");
                if let Some(checkpoints) = device.resources().checkpoints() {
                    checkpoints.mark(command_buffer, &pass.name);
                }
                let barriers = &plan.passes[pass_index];
                let gpu_timestamps_supported = batch.queue == RgQueueAssignment::Graphics
                    || device
                        .compute_timestamp_valid_bits
                        .is_some_and(|bits| bits != 0);
                let cpu_index = recorders.cpu.as_mut().map(|(registry, buffer)| {
                    buffer.begin_span(registry, &pass.name, cpu_now_ns())
                });
                let gpu_index = gpu_timestamps_supported
                    .then(|| {
                        recorders.gpu.as_mut().and_then(|timestamps| {
                            timestamps.begin_scope(raw, command_buffer, &pass.name)
                        })
                    })
                    .flatten();
                emit_barriers(
                    raw,
                    command_buffer,
                    &barriers.before_images,
                    &barriers.before_buffers,
                );
                if records_pipeline_statistics(batch.queue)
                    && let (Some(index), Some(timestamps)) = (gpu_index, recorders.gpu.as_mut())
                {
                    let pixels =
                        u64::from(pass.render_area.width) * u64::from(pass.render_area.height);
                    let _ = timestamps.reserve_stats_slot(index, pixels);
                }
                {
                    let gpu = if gpu_timestamps_supported {
                        recorders.gpu.as_deref_mut()
                    } else {
                        None
                    };
                    let mut nested = NestedScopeRecorder::new(
                        raw,
                        command_buffer,
                        gpu,
                        recorders.cpu.as_mut().map(|(r, b)| (&mut **r, &mut **b)),
                    );
                    match pass.kind {
                        RgPassKind::Graphics => {
                            self.record_graphics(device, command_buffer, pass, &mut nested);
                        }
                        RgPassKind::GraphicsCommands | RgPassKind::Compute => {
                            if let Some(body) = pass.execute {
                                body(command_buffer, &mut nested);
                            }
                        }
                    }
                }
                emit_barriers(
                    raw,
                    command_buffer,
                    &barriers.after_images,
                    &barriers.after_buffers,
                );
                if gpu_timestamps_supported && let Some(timestamps) = recorders.gpu.as_mut() {
                    timestamps.end_scope(raw, command_buffer, gpu_index);
                }
                if let (Some(index), Some((_, buffer))) = (cpu_index, recorders.cpu.as_mut()) {
                    buffer.end_span(index, cpu_now_ns());
                }
            }
            emit_barriers(
                raw,
                command_buffer,
                &batch.entry_release_images,
                &batch.entry_release_buffers,
            );
            if manage_command_buffers {
                crate::checked(
                    unsafe { raw.end_command_buffer(command_buffer) },
                    "end render-graph batch",
                )?;
            }
            recorded.push(RgRecordedBatch {
                queue: batch.queue,
                command_buffer,
                wait_for_batches: batch.wait_for_batches.clone(),
                passes: batch.passes.clone(),
            });
        }
        self.write_external_states();
        Ok(recorded)
    }

    /// Derives and emits each pass's barriers from its declared usage, then records
    /// the pass body inside its rendering scope (graphics) or directly (compute).
    /// After every pass, resolves cross-frame layouts into their external slots.
    ///
    /// Recording is single-threaded: the body closures run here on the render
    /// thread, exactly once each, while `cmd` records.
    pub fn execute(&mut self, device: &Device, cmd: vk::CommandBuffer) {
        self.execute_profiled(device, cmd, &mut ProfileRecorders::default());
    }

    /// [`RenderGraph::execute`] with the profiler recorders armed: each pass body is
    /// bracketed by a GPU timestamp scope (when `recorders.gpu` is armed) and a CPU
    /// span (when `recorders.cpu` is armed), and a top-level graphics pass reserves a
    /// pipeline-statistics slot. Unarmed recorders make every scope a cheap branch.
    pub fn execute_profiled(
        &mut self,
        device: &Device,
        cmd: vk::CommandBuffer,
        recorders: &mut ProfileRecorders<'_>,
    ) {
        let plan =
            self.submission_plan(RgQueueFamilies::graphics_only(device.graphics_queue_family));
        debug_assert_eq!(plan.compute_batch_count(), 0);
        debug_assert!(plan.graphics_batch_count() <= 1);
        let graphics = (plan.graphics_batch_count() != 0).then_some(cmd);
        self.record_compiled_plan_profiled(
            device,
            plan,
            RgBatchCommandBuffers {
                graphics: graphics.as_slice(),
                compute: &[],
            },
            recorders,
            false,
        )
        .expect("graphics-only render-graph recording has matching command buffers");
    }

    pub(super) fn write_external_states(&mut self) {
        for r in &self.resources {
            if let Some(slot) = r.external_state {
                self.external_states[slot] = RgExternalState {
                    layout: r.layout,
                    queue_family: r.queue_family,
                    queue: r.queue,
                    stage: r.last_stage,
                    access: r.last_access,
                    was_write: r.last_was_write,
                    touched: r.touched,
                };
            }
            if let Some(slot) = r.external_buffer_state {
                self.external_buffer_states[slot] = RgExternalBufferState {
                    accesses: r.buffer_accesses.clone(),
                };
            }
        }
    }

    /// Opens a `cmd_begin_rendering` scope for a graphics pass — color/depth
    /// attachment infos (incl. MSAA color `AVERAGE` / depth `SAMPLE_ZERO` resolve),
    /// the full-area viewport/scissor — runs the body, then closes the scope.
    pub(super) fn record_graphics(
        &self,
        device: &Device,
        cmd: vk::CommandBuffer,
        pass: RgPass,
        scopes: &mut NestedScopeRecorder<'_>,
    ) {
        let raw = device.raw();
        let mut color_infos = Vec::with_capacity(pass.colors.len());
        for att in &pass.colors {
            let r = &self.resources[att.resource.index as usize];
            let mut info = vk::RenderingAttachmentInfo::default()
                .image_view(r.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(att.load_op)
                .store_op(att.store_op)
                .clear_value(att.clear_value);
            if let Some(resolve) = att.resolve {
                info = info
                    .resolve_mode(vk::ResolveModeFlags::AVERAGE)
                    .resolve_image_view(self.resources[resolve.index as usize].view)
                    .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            }
            color_infos.push(info);
        }

        let depth_info = pass.depth.as_ref().map(|depth| {
            let r = &self.resources[depth.resource.index as usize];
            let mut info = vk::RenderingAttachmentInfo::default()
                .image_view(r.view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(depth.load_op)
                .store_op(depth.store_op)
                .clear_value(depth.clear_value);
            if let Some(resolve) = depth.resolve {
                info = info
                    .resolve_mode(vk::ResolveModeFlags::SAMPLE_ZERO)
                    .resolve_image_view(self.resources[resolve.index as usize].view)
                    .resolve_image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL);
            }
            info
        });

        let mut rendering = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: pass.render_area,
            })
            .layer_count(1)
            .color_attachments(&color_infos);
        if let Some(ref depth) = depth_info {
            rendering = rendering.depth_attachment(depth);
        }

        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: pass.render_area.width as f32,
            height: pass.render_area.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: pass.render_area,
        };

        // SAFETY: the ash seam. The attachment infos reference imported views; the
        // rendering scope is opened and closed in this method and the body records
        // between them.
        unsafe {
            raw.cmd_begin_rendering(cmd, &rendering);
            raw.cmd_set_viewport(cmd, 0, &[viewport]);
            raw.cmd_set_scissor(cmd, 0, &[scissor]);
        }
        if let Some(body) = pass.execute {
            body(cmd, scopes);
        }
        // SAFETY: the ash seam. Closes the rendering scope opened above.
        unsafe { raw.cmd_end_rendering(cmd) };
    }
}

pub(super) fn emit_barriers(
    raw: &ash::Device,
    command_buffer: vk::CommandBuffer,
    images: &[vk::ImageMemoryBarrier2<'static>],
    buffers: &[vk::BufferMemoryBarrier2<'static>],
) {
    if images.is_empty() && buffers.is_empty() {
        return;
    }
    let dependency = vk::DependencyInfo::default()
        .image_memory_barriers(images)
        .buffer_memory_barriers(buffers);
    unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dependency) };
}

pub(super) fn records_pipeline_statistics(queue: RgQueueAssignment) -> bool {
    queue == RgQueueAssignment::Graphics
}
