use std::sync::Arc;

use ash::vk;

use super::dispatch::{record_binning_dispatch, record_dispatch};
use super::*;
use crate::device::Device;
use crate::nested_scopes::NestedScopeRecorder;
use crate::render_graph::{RenderGraph, RgPass, RgResource, RgUsage};

impl SceneVisibilityView {
    /// Records the interaction-field step for `frame`: reset scrolled texels, splat
    /// this frame's impulses, and integrate the damped oscillators — before the
    /// wind prepass and the micro scatter sample the field.
    pub fn add_wind_interact_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        interaction_field: RgResource,
        push: WindInteractPush,
    ) {
        let slot = &self.frames[frame];
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = (crate::GPU_INTERACTION_CASCADES
            * crate::GPU_INTERACTION_TEXELS
            * crate::GPU_INTERACTION_TEXELS)
            .div_ceil(64);
        graph.add_pass(
            RgPass::compute("wind-interact")
                .access(interaction_field, RgUsage::StorageReadWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the wind deformation prepass for `frame`: one dispatch over the
    /// world's instance slots writes each wind-flagged instance's sway record
    /// before the cull and every raster pass read it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_wind_deform_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        wind_records: RgResource,
        interaction_field: RgResource,
        instance_capacity: u32,
        push: WindDeformPush,
    ) {
        let slot = &self.frames[frame];
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = instance_capacity.max(1).div_ceil(64);
        graph.add_pass(
            RgPass::compute("wind-deform")
                .access(wind_records, RgUsage::StorageWriteCompute)
                .access(interaction_field, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the counter clear + cull passes for `frame`. `hzb` is the previous
    /// pyramid resource the cull samples (GENERAL layout); `wind_records` is the
    /// sway-record buffer whose slack the sphere compose reads.
    #[allow(clippy::too_many_arguments)]
    pub fn add_cull_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        hzb: RgResource,
        wind_records: RgResource,
        instance_capacity: u32,
        push: SceneVisibilityPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let commands_res = graph.import_buffer(slot.commands.handle(), None);
        let mesh_args_res = graph.import_buffer(slot.mesh_args.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let bin_counts = slot.bin_counts.handle();
        let commands = slot.commands.handle();
        let mesh_args = slot.mesh_args.handle();
        // The transition state table is cross-frame: zero it exactly once, on the
        // view's first recorded frame, so stale allocations never alias live keys.
        let transitions = (!self
            .transitions_cleared
            .swap(true, std::sync::atomic::Ordering::Relaxed))
        .then(|| self.transitions.handle());
        let mut clear = RgPass::compute("visibility-clear")
            .access(counters_res, RgUsage::TransferWrite)
            .access(bin_counts_res, RgUsage::TransferWrite)
            .access(commands_res, RgUsage::TransferWrite)
            .access(mesh_args_res, RgUsage::TransferWrite);
        if transitions.is_some() {
            let transitions_res = graph.import_buffer(self.transitions.handle(), None);
            clear = clear.access(transitions_res, RgUsage::TransferWrite);
        }
        graph.add_pass(clear.body({
            let raw = raw.clone();
            move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Every filled buffer is TRANSFER_DST.
                unsafe {
                    raw.cmd_fill_buffer(cmd, counters, 0, SCENE_VISIBILITY_COUNTER_WORDS * 4, 0);
                    raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    raw.cmd_fill_buffer(cmd, commands, 0, vk::WHOLE_SIZE, 0);
                    // Unwritten mesh-task slots must dispatch nothing, exactly as unwritten
                    // indexed commands draw nothing.
                    raw.cmd_fill_buffer(cmd, mesh_args, 0, vk::WHOLE_SIZE, 0);
                    if let Some(transitions) = transitions {
                        raw.cmd_fill_buffer(cmd, transitions, 0, vk::WHOLE_SIZE, 0);
                    }
                };
            }
        }));
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = instance_capacity.max(1).div_ceil(64);
        graph.add_pass(
            RgPass::compute("instance-cull")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(hzb, RgUsage::StorageImageRwCompute)
                .access(wind_records, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the retest pass for `frame` after the current pyramid's build. The
    /// dispatch covers the whole list capacity; the shader bounds itself by the retest
    /// counter.
    #[allow(clippy::too_many_arguments)]
    pub fn add_retest_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        hzb: RgResource,
        wind_records: RgResource,
        push: SceneVisibilityPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.retest_set;
        let groups = self.capacity.div_ceil(64);
        graph.add_pass(
            RgPass::compute("instance-retest")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(hzb, RgUsage::StorageImageRwCompute)
                .access(wind_records, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the traversal pass for `frame` over the merged visible list: refine by
    /// projected appearance error where children are resident, request missing pages,
    /// and emit the semantic record stream.
    pub fn add_traversal_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        push: SceneTraversalPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let transitions_res = graph.import_buffer(self.transitions.handle(), None);
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.traversal_set;
        let groups = self.capacity.div_ceil(64);
        graph.add_pass(
            RgPass::compute("scene-traversal")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(records_res, RgUsage::StorageWriteCompute)
                .access(transitions_res, RgUsage::StorageReadWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the push
                    // spans the declared range; the dispatch covers the list capacity.
                    unsafe {
                        raw.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            pipeline.handle(),
                        );
                        raw.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            pipeline.layout(),
                            0,
                            &[set],
                            &[],
                        );
                        raw.cmd_push_constants(
                            cmd,
                            pipeline.layout(),
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            bytemuck::bytes_of(&push),
                        );
                        raw.cmd_dispatch(cmd, groups, 1, 1);
                    }
                }),
        );
    }

    /// Records the micro-field count → scan → scatter chain for `frame`: count the
    /// post-cull blade survivors per resident tile, scan the counts into exact
    /// exclusive bases under the frame's candidate and record budgets, then scatter
    /// each survivor's candidate and [`crate::GpuDrawRecord`] to its exact slot —
    /// no atomics order the stream, so record order is stable frame to frame.
    #[allow(clippy::too_many_arguments)]
    pub fn add_micro_field_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        frame: usize,
        candidates: vk::Buffer,
        interaction_field: RgResource,
        push: SceneMicroFieldPush,
    ) {
        let (count, scan, scatter) = pipelines;
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let candidates_res = graph.import_buffer(candidates, None);
        let scratch_res = graph.import_buffer(slot.micro_scratch.handle(), None);
        let raw = device.raw().clone();
        let set = slot.micro_set;
        // One workgroup per directory entry; its threads stride the tile's texels.
        let tile_groups = push
            .directory_count
            .clamp(1, SCENE_MICRO_DIRECTORY_CAPACITY);

        let record_micro_dispatch = move |cmd: vk::CommandBuffer,
                                          raw: &ash::Device,
                                          pipeline: &crate::Pipeline,
                                          groups: (u32, u32)| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the push
            // spans the declared range; the dispatch covers the declared groups.
            unsafe {
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline.layout(),
                    0,
                    &[set],
                    &[],
                );
                raw.cmd_push_constants(
                    cmd,
                    pipeline.layout(),
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                raw.cmd_dispatch(cmd, groups.0, groups.1, 1);
            }
        };

        let pipeline = Arc::clone(count);
        graph.add_pass(
            RgPass::compute("scene-micro-count")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(scratch_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_micro_dispatch(cmd, &raw, &pipeline, (1, tile_groups));
                    }
                }),
        );
        let pipeline = Arc::clone(scan);
        graph.add_pass(
            RgPass::compute("scene-micro-scan")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(scratch_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_micro_dispatch(cmd, &raw, &pipeline, (1, 1));
                    }
                }),
        );
        let pipeline = Arc::clone(scatter);
        graph.add_pass(
            RgPass::compute("scene-micro-scatter")
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(records_res, RgUsage::StorageReadWriteCompute)
                .access(candidates_res, RgUsage::StorageWriteCompute)
                .access(scratch_res, RgUsage::StorageReadCompute)
                .access(interaction_field, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_micro_dispatch(cmd, &raw, &pipeline, (1, tile_groups));
                }),
        );
    }

    /// Records the three binning passes for `frame`: per-bin counts, the exclusive
    /// scan, and the scatter that builds the indirect command stream.
    #[allow(clippy::too_many_arguments)]
    pub fn add_binning_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        frame: usize,
        survivor: bool,
        micro_template: (u32, u32),
        displaced: Option<crate::DisplacedFrameBuffers>,
    ) {
        let (count, seed, scatter) = pipelines;
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let bin_cursors_res = graph.import_buffer(slot.bin_cursors.handle(), None);
        let commands_res = graph.import_buffer(slot.commands.handle(), None);
        let raw = device.raw().clone();
        // Words 2-3: the shared blade-template index count + first index for
        // micro-blade commands.
        let push = [
            self.record_capacity,
            u32::from(survivor),
            micro_template.0,
            micro_template.1,
        ];
        let groups = self.record_capacity.div_ceil(64);

        let pipeline = Arc::clone(count);
        let set = slot.bin_count_set;
        graph.add_pass(
            RgPass::compute("scene-bucket-count")
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(records_res, RgUsage::StorageReadCompute)
                .access(bin_counts_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&push)),
                            groups,
                        );
                    }
                }),
        );
        let pipeline = Arc::clone(seed);
        let set = slot.bin_seed_set;
        graph.add_pass(
            RgPass::compute("scene-bucket-seed")
                .access(bin_cursors_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            None,
                            SCENE_EXECUTOR_BUCKET_CAPACITY.div_ceil(64),
                        );
                    }
                }),
        );
        let pipeline = Arc::clone(scatter);
        let set = slot.bin_scatter_set;
        let mut scatter_pass = RgPass::compute("scene-bucket-scatter")
            .access(counters_res, RgUsage::StorageReadWriteCompute)
            .access(records_res, RgUsage::StorageReadCompute)
            .access(bin_cursors_res, RgUsage::StorageReadWriteCompute)
            .access(commands_res, RgUsage::StorageWriteCompute);
        // A displaced record's command is built from its row's draw seed, which the
        // amplification chain's finalize kernel wrote earlier this frame.
        if let Some(arena) = displaced {
            let seeds_res = graph.import_buffer(arena.draws, None);
            scatter_pass = scatter_pass.access(seeds_res, RgUsage::StorageReadCompute);
        }
        graph.add_pass(
            scatter_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_binning_dispatch(
                    &raw,
                    cmd,
                    &pipeline,
                    set,
                    Some(bytemuck::cast_slice(&push)),
                    groups,
                );
            }),
        );
    }

    /// The value captures a graphics pass body needs to issue the executor draw.
    #[must_use]
    pub fn executor_draw_inputs(
        &self,
        frame: usize,
        draw_records: u32,
        displaced_indices: vk::Buffer,
    ) -> ExecutorDrawInputs {
        let slot = &self.frames[frame];
        ExecutorDrawInputs {
            commands: slot.commands.handle(),
            mesh_args: slot.mesh_args.handle(),
            counters: slot.counters.handle(),
            bucket_counts: slot.bin_counts.handle(),
            displaced_indices,
            record_capacity: self.record_capacity,
            draw_bound: draw_records.clamp(1, self.record_capacity),
        }
    }

    /// The frame slot's indirect command stream (for graph usage declarations).
    pub fn mesh_args(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].mesh_args.handle()
    }

    /// The frame slot's indexed indirect command stream.
    pub fn commands(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].commands.handle()
    }

    /// The frame slot's per-bucket record counts (the draws' indirect count buffer).
    pub fn bucket_counts(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].bin_counts.handle()
    }

    /// Publishes the frame's bucket table (`build_executor_buckets` bytes).
    pub fn write_bucket_table(&self, frame: usize, table: &[u8]) {
        let buffer = &self.frames[frame].bucket_table;
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's prior GPU reads completed with its
        // fence before this frame reused the slot.
        unsafe {
            std::ptr::copy_nonoverlapping(
                table.as_ptr(),
                buffer.mapped_ptr(),
                table.len().min(buffer.size() as usize),
            );
        }
    }

    /// Records the provisional-count snapshot: copies the visible and record counts
    /// into counter words 6/7 and clears the bucket counts, so the survivor
    /// traversal/binning process only the retest tail. Record after the provisional
    /// raster and before the retest pass.
    pub fn add_survivor_snapshot_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let bin_counts = slot.bin_counts.handle();
        graph.add_pass(
            RgPass::compute("survivor-snapshot")
                .access(counters_res, RgUsage::TransferWrite)
                .access(bin_counts_res, RgUsage::TransferWrite)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Word copies within the counters buffer +
                    // the bucket-count clear, ordered by the graph's transfer usage.
                    unsafe {
                        raw.cmd_copy_buffer(
                            cmd,
                            counters,
                            counters,
                            &[
                                vk::BufferCopy {
                                    src_offset: 0,
                                    dst_offset: 24,
                                    size: 4,
                                },
                                vk::BufferCopy {
                                    src_offset: 12,
                                    dst_offset: 28,
                                    size: 4,
                                },
                            ],
                        );
                        raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    }
                }),
        );
    }

    /// Records a bucket-count clear (fill 0) — the prologue for a fresh binning run
    /// over an already-drawn command stream (the full re-bin after the survivor
    /// raster restores the complete cut for later consumers).
    pub fn add_bucket_count_clear_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let raw = device.raw().clone();
        let bin_counts = slot.bin_counts.handle();
        graph.add_pass(
            RgPass::compute("bucket-count-clear")
                .access(bin_counts_res, RgUsage::TransferWrite)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The clear is ordered by the graph's
                    // transfer usage against every earlier count read.
                    unsafe {
                        raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    }
                }),
        );
    }

    /// Records the counters → readback copy; the CPU folds the slot's words into
    /// stats once its fence completes.
    pub fn add_counters_readback_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let readback = slot.readback.handle();
        graph.add_pass(
            RgPass::compute("visibility-readback")
                .access(counters_res, RgUsage::TransferRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Both buffers are valid this frame; the
                    // graph ordered the copy after the chain's writes.
                    unsafe {
                        raw.cmd_copy_buffer(
                            cmd,
                            counters,
                            readback,
                            &[vk::BufferCopy {
                                src_offset: 0,
                                dst_offset: 0,
                                size: SCENE_VISIBILITY_COUNTER_WORDS * 4,
                            }],
                        );
                    }
                }),
        );
    }

    /// The fence-completed slot's counter words (visible/retest/overflow/records/
    /// record-pressure/transparent).
    #[must_use]
    pub fn read_counters(&self, frame: usize) -> [u32; SCENE_VISIBILITY_COUNTER_WORDS as usize] {
        let slot = &self.frames[frame];
        let mut words = [0_u32; SCENE_VISIBILITY_COUNTER_WORDS as usize];
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's copy completed with its fence
        // before this frame reused the slot.
        unsafe {
            std::ptr::copy_nonoverlapping(
                slot.readback.mapped_ptr(),
                words.as_mut_ptr().cast::<u8>(),
                words.len() * 4,
            );
        }
        words
    }

    /// The frame slot's back-to-front transparent command stream (counter word 5 is
    /// its draw count).
    pub fn transparent_commands(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].transparent_commands.handle()
    }

    /// Records the transparent back-to-front sort for `frame`: key collection, four
    /// stable LSD radix passes (histogram → scan → scatter, ping-ponging the pair
    /// buffers), and the reverse-order command reorder into the transparent stream.
    /// `view_row2` is the camera view matrix's third row (view-space depth).
    pub fn add_transparent_sort_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: TransparentSortPipelines<'_>,
        frame: usize,
        view_row2: [f32; 4],
        blend_bucket_keys: &[u32],
    ) {
        debug_assert!(
            blend_bucket_keys.len() as u32 <= self.transparent_group_capacity,
            "blend buckets exceed the allocated transparent stream slices"
        );
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let pairs_res = [
            graph.import_buffer(slot.pairs[0].handle(), None),
            graph.import_buffer(slot.pairs[1].handle(), None),
        ];
        let histograms_res = graph.import_buffer(slot.histograms.handle(), None);
        let transparent_res = graph.import_buffer(slot.transparent_commands.handle(), None);
        let raw = device.raw().clone();
        let workgroups = self.record_capacity.div_ceil(SCENE_RADIX_WORKGROUP);
        let record_groups = self.record_capacity.div_ceil(64);

        let mut keys_push = [0_u32; 8];
        keys_push[..4].copy_from_slice(bytemuck::cast_slice(&view_row2));
        keys_push[4] = self.record_capacity;
        let pipeline = Arc::clone(pipelines.keys);
        let set = slot.transparent_keys_set;
        graph.add_pass(
            RgPass::compute("transparent-keys")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(pairs_res[0], RgUsage::StorageWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&keys_push)),
                            record_groups,
                        );
                    }
                }),
        );

        for pass in 0..4_u32 {
            let direction = (pass % 2) as usize;
            let shift_push = [pass * 8, self.record_capacity, workgroups, 0];
            let pipeline = Arc::clone(pipelines.histogram);
            let set = slot.radix_histogram_sets[direction];
            graph.add_pass(
                RgPass::compute("radix-histogram")
                    .access(pairs_res[direction], RgUsage::StorageReadCompute)
                    .access(histograms_res, RgUsage::StorageReadWriteCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&shift_push)),
                                workgroups,
                            );
                        }
                    }),
            );
            let pipeline = Arc::clone(pipelines.scan);
            let set = slot.radix_scan_set;
            let entries = workgroups * 256;
            graph.add_pass(
                RgPass::compute("radix-scan")
                    .access(histograms_res, RgUsage::StorageReadWriteCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&[entries, workgroups, 0, 0])),
                                1,
                            );
                        }
                    }),
            );
            let pipeline = Arc::clone(pipelines.scatter);
            let set = slot.radix_scatter_sets[direction];
            graph.add_pass(
                RgPass::compute("radix-scatter")
                    .access(pairs_res[direction], RgUsage::StorageReadCompute)
                    .access(pairs_res[direction ^ 1], RgUsage::StorageWriteCompute)
                    .access(histograms_res, RgUsage::StorageReadCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&shift_push)),
                                workgroups,
                            );
                        }
                    }),
            );
        }

        // One reorder dispatch per live blend bucket: each writes that bucket's
        // full-length command slice (zero-masking other buckets' pairs), so every
        // blend PSO replays the whole back-to-front order. The dispatches write
        // disjoint slices — no barrier between them.
        let pipeline = Arc::clone(pipelines.reorder);
        let set = slot.transparent_reorder_set;
        let reorder_capacity = self.record_capacity;
        let groups: Vec<u32> = blend_bucket_keys.to_vec();
        graph.add_pass(
            RgPass::compute("transparent-reorder")
                .access(pairs_res[0], RgUsage::StorageReadCompute)
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(transparent_res, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    for (group_slot, key) in groups.iter().enumerate() {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&[
                                reorder_capacity,
                                *key,
                                group_slot as u32 * reorder_capacity,
                                0,
                            ])),
                            record_groups,
                        );
                    }
                }),
        );
    }
}
