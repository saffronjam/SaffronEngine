use super::*;

impl Renderer {
    /// Records the adaptive-tessellation prep + emit passes for the frame's displaced instances:
    /// **factor** (one fractional factor per unique base edge) → **scan** (predict + atomic-carry
    /// prefix-sum the exact dice output into packed per-instance offsets) → **finalize** (the
    /// per-row draw seed + RT prim count) → **args** (the emit-dispatch size) → **emit**.
    ///
    /// Publishes the frame's amplification arena on the way: the buffers every executor pass
    /// reads ([`Renderer::displaced_frame`]) and their addresses for the GPU-scene address block
    /// ([`Renderer::displaced_addresses`]), including the slot-sorted row table the visibility
    /// traversal looks displaced instances up in. Records before the address block is published,
    /// for that reason.
    ///
    /// Returns the coarse RT VB/IB graph resources when a tessellated instance is RT-consumed this
    /// frame, so the caller can declare them `AccelStructBuildRead` on `tlas-build`; `None`
    /// otherwise. A no-op unless a displaced instance is mirrored, carries watertight
    /// conditioning, and all the PSOs build.
    pub(super) fn record_tess_prep(
        &mut self,
        graph: &mut crate::RenderGraph,
        frame: usize,
        raw: &ash::Device,
    ) -> Option<(RgResource, RgResource)> {
        self.displaced_frame = None;
        self.displaced_addresses = crate::DisplacedFrameAddresses::default();
        if self.frame_deformation.tess_buckets.is_empty() {
            return None;
        }

        // The per-instance inputs (Copy handles + owned params) lifted out of the deformation
        // frame up front, so the transient + pipeline borrows below never alias its borrow.
        struct Inst {
            welded: vk::Buffer,
            edges: vk::Buffer,
            tri_edges: vk::Buffer,
            base_vertices: vk::Buffer,
            base_indices: vk::Buffer,
            instance_slot: u32,
            entity: u64,
            edge_count: u32,
            tri_count: u32,
            model: Mat4,
            factor_cap: f32,
            min_factor: f32,
            edge_length_target: f32,
            height_index: u32,
            height_scale: f32,
            vector_index: u32,
            uv_transform: [f32; 4],
        }
        let mut insts: Vec<Inst> = Vec::with_capacity(self.frame_deformation.tess_buckets.len());
        for bucket in &self.frame_deformation.tess_buckets {
            let Some(cond) = bucket.mesh.conditioning() else {
                continue;
            };
            // An instance the GPU scene never mirrored has no slot for the traversal to
            // look a row up by, so amplifying it would produce geometry nothing draws.
            if bucket.instance_slot == crate::RT_UNMIRRORED_INSTANCE {
                continue;
            }
            if insts.len() as u32 == crate::TESS_MAX_INSTANCES {
                break;
            }
            insts.push(Inst {
                welded: cond.welded.0,
                edges: cond.edges.0,
                tri_edges: cond.tri_edges.0,
                base_vertices: bucket.mesh.vertex_buffer(),
                base_indices: bucket.mesh.index_buffer(),
                instance_slot: bucket.instance_slot,
                entity: bucket.entity,
                edge_count: cond.edge_count,
                tri_count: bucket.mesh.index_count / 3,
                model: bucket.model,
                factor_cap: bucket.factor_cap,
                min_factor: bucket.min_factor,
                edge_length_target: bucket.edge_length_target,
                height_index: bucket.height_index,
                height_scale: bucket.height_scale,
                vector_index: bucket.vector_index,
                uv_transform: bucket.uv_transform,
            });
        }
        if insts.is_empty() {
            return None;
        }

        // Hard triangle budget: coarsen each instance's factor cap so the *summed* worst-case
        // reservation fits `TESS_MICRO_VERTEX_BUDGET`. The reservation, scan, and emit all read the
        // adjusted `factor_cap`, so the GPU can never write past the reserved transient arena.
        {
            let budget_input: Vec<(u32, f32, f32)> = insts
                .iter()
                .map(|i| (i.tri_count, i.factor_cap, i.min_factor))
                .collect();
            let scaled = crate::tessellation::budget_scaled_caps(
                &budget_input,
                crate::tessellation::TESS_MICRO_VERTEX_BUDGET,
            );
            for (inst, &cap) in insts.iter_mut().zip(&scaled) {
                inst.factor_cap = cap;
            }
        }

        // The tessellation camera, derived from the mirrored cluster camera (world position via the
        // inverse view; `tan(½fov)` from the projection's `y` scale).
        let view = self.cluster_camera.view;
        let proj = self.cluster_camera.projection;
        let cam = crate::TessCamera {
            view_proj: proj * view,
            cam_pos: view.inverse().w_axis.truncate(),
            viewport: [
                self.cluster_camera.width as f32,
                self.cluster_camera.height as f32,
            ],
            tan_half_fov_y: 1.0 / proj.y_axis.y.abs().max(1e-4),
            near: self.cluster_camera.near.max(1e-4),
        };

        // Per-instance placement (prefix sums) into the shared buffers, reserved worst-case at the cap.
        let mut layouts: Vec<crate::TessInstanceLayout> = Vec::with_capacity(insts.len());
        let (mut edge_cur, mut tri_cur, mut vb_cur, mut ib_cur) = (0u32, 0u32, 0u32, 0u32);
        for (row, inst) in insts.iter().enumerate() {
            let (verts, indices) =
                crate::tessellation::tess_worst_case(inst.tri_count, inst.factor_cap as u32);
            layouts.push(crate::TessInstanceLayout {
                factor_base: edge_cur,
                tri_base: tri_cur,
                instance_row: row as u32,
                vertex_base: vb_cur,
                index_base: ib_cur,
            });
            edge_cur += inst.edge_count;
            tri_cur += inst.tri_count;
            vb_cur = vb_cur.saturating_add(verts as u32);
            ib_cur = ib_cur.saturating_add(indices as u32);
        }
        let instance_rows = insts.len() as u32;

        // The temporal factor ping-pong (owned by `Tessellation`, not the rewound transient pool):
        // write this frame's factors into `slot[frame % 2]`, read last frame's from the other. The
        // prev slot serves the emit kernel's prev-position stream only when its surviving factors
        // are laid out identically; otherwise prev falls back to cur, so the geomorph delta is zero.
        let edge_layout: Vec<u32> = insts.iter().map(|inst| inst.edge_count).collect();
        let cur_slot = frame % 2;
        let prev_slot = (frame + 1) % 2;
        let factors = match self.tessellation.ensure_factor_slot(cur_slot, edge_cur) {
            Ok(buffer) => buffer,
            Err(err) => {
                tracing::error!("tess prep: ensure factor slot {cur_slot}: {err}");
                return None;
            }
        };
        let prev_factors = if self
            .tessellation
            .factor_layout_matches(prev_slot, &edge_layout)
        {
            self.tessellation.factor_slot(prev_slot).unwrap_or(factors)
        } else {
            factors
        };

        // The four PSOs (set-0 layouts are Copy handles, so no `self.tessellation` borrow spans the
        // `&mut self.pipelines` request).
        let factor_layout = self.tessellation.factor_layout();
        let scan_layout = self.tessellation.scan_layout();
        let finalize_layout = self.tessellation.finalize_layout();
        let args_layout = self.tessellation.args_layout();
        let emit_layout = self.tessellation.emit_layout();
        let bindless_layout = self.descriptors.bindless_set_layout();
        let bindless_set = self.descriptors.bindless_set();
        let (Some(factor_pso), Some(scan_pso), Some(finalize_pso), Some(args_pso), Some(emit_pso)) = (
            self.pipelines
                .request_tess_factor(bindless_layout, factor_layout),
            self.pipelines.request_tess_scan(scan_layout),
            self.pipelines.request_tess_finalize(finalize_layout),
            self.pipelines.request_tess_args(args_layout),
            self.pipelines
                .request_tessellate(bindless_layout, emit_layout),
        ) else {
            return None;
        };

        // The per-frame transient scratch (grow-only, keyed). All are storage buffers; the
        // emit-dispatch args also carry `INDIRECT_BUFFER`, and the draw seeds a device address
        // (the binner reads them through the frame's address block).
        let storage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let indirect = storage | vk::BufferUsageFlags::INDIRECT_BUFFER;
        let addressed = storage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        // Storage buffers additionally cleared each frame via `cmd_fill_buffer` (a transfer op) need
        // TRANSFER_DST: the scan's per-instance counters + global totals, and the emit's degenerate-pad
        // clear of the index stream.
        let storage_cleared = storage | vk::BufferUsageFlags::TRANSFER_DST;
        // The AS-build-input flag the RT BLAS requires on the geometry buffers it references by
        // device address. The flag is valid only with `VK_KHR_acceleration_structure`.
        let accel_input = if self.rt.supported() {
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
        } else {
            vk::BufferUsageFlags::empty()
        };
        let acquire = |graph: &mut RenderGraph,
                       t: &mut RenderGraphResources,
                       key: &'static str,
                       bytes: u64,
                       usage|
         -> Option<vk::Buffer> {
            match graph.create_buffer(
                t,
                frame,
                key,
                crate::RgBufferDesc {
                    size: bytes.max(16),
                    usage,
                    lifetime: crate::RgBufferLifetime::Transient,
                },
            ) {
                Ok(resource) => Some(graph.buffer(resource)),
                Err(err) => {
                    tracing::error!("tess prep: acquire {key}: {err}");
                    None
                }
            }
        };
        let counters_bytes = instance_rows as u64 * 8;
        let (Some(pertri), Some(counters), Some(global), Some(seeds), Some(prims), Some(dispatch)) = (
            acquire(
                graph,
                &mut self.transient,
                "tess.pertri",
                tri_cur as u64 * 16,
                storage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.counters",
                counters_bytes,
                storage_cleared,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.global",
                8,
                storage_cleared,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.seeds",
                instance_rows as u64 * 20,
                addressed | vk::BufferUsageFlags::TRANSFER_DST,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.prims",
                instance_rows as u64 * 4,
                storage,
            ),
            acquire(graph, &mut self.transient, "tess.dispatch", 12, indirect),
        ) else {
            return None;
        };
        // The amplified geometry stream: the worst-case-reserved transient VB (48 B micro-vertices)
        // + IB (u32). They are storage-written by the emit kernel; the VB is pulled through its
        // device address by the executor vertex paths and the IB is bound as the index stream of the
        // frame's displaced draw buckets, so the same handles serve the raster and RT consumers.
        let vb_usage = storage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS | accel_input;
        // The index stream is also degenerate-pad cleared (`cmd_fill_buffer` → TRANSFER_DST) and, like
        // the VB, is a BLAS build input (ACCEL_BUILD_INPUT) — the emit writes it, the raster draws it,
        // and the RT BLAS builds from it.
        let ib_usage = storage
            | vk::BufferUsageFlags::INDEX_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | accel_input
            | vk::BufferUsageFlags::TRANSFER_DST;
        // The prev-position stream: same worst-case size as `tess.vb`, storage-written by the emit
        // kernel and pulled by the motion pass through its device address. Motion-only, so it is
        // neither an index stream nor a BLAS input.
        let prev_vb_usage = storage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let (Some(out_vb), Some(out_ib), Some(out_prev_vb)) = (
            acquire(
                graph,
                &mut self.transient,
                "tess.vb",
                vb_cur as u64 * 48,
                vb_usage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.ib",
                ib_cur as u64 * 4,
                ib_usage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.vb.prev",
                vb_cur as u64 * 48,
                prev_vb_usage,
            ),
        ) else {
            return None;
        };

        // Publish the arena: the slot-sorted row table the traversal looks instances up in, and the
        // addresses the frame's GPU-scene address block carries. Row `r` owns draw seed `r * 20` in
        // `seeds`, which the binner turns into that row's executor command.
        let mut rows: Vec<crate::DisplacedRow> = insts
            .iter()
            .enumerate()
            .map(|(row, inst)| crate::DisplacedRow {
                slot: inst.instance_slot,
                row: row as u32,
            })
            .collect();
        let rows_address = match self
            .tessellation
            .publish_rows(&self.device, frame, &mut rows)
        {
            Ok(address) => address,
            Err(err) => {
                tracing::error!("tess prep: publish displaced rows: {err}");
                return None;
            }
        };
        self.displaced_frame = Some(crate::DisplacedFrameBuffers {
            vertices: out_vb,
            prev_vertices: out_prev_vb,
            indices: out_ib,
            draws: seeds,
        });
        self.displaced_addresses = crate::DisplacedFrameAddresses {
            vertices: self.device.buffer_device_address(out_vb),
            prev_vertices: self.device.buffer_device_address(out_prev_vb),
            indices: self.device.buffer_device_address(out_ib),
            draws: self.device.buffer_device_address(seeds),
            rows: rows_address,
            row_count: rows.len() as u32,
        };

        // The tessellated RT instances' slices are filled from the separate coarse (secondary-ray)
        // chain below, not these fine raster buffers, so the BLAS builds from lower-density geometry.

        // Wire one factor + scan + finalize + emit descriptor set per instance (shared buffers bound at
        // whole range; the pushes carry each instance's slice bases), plus the one global args set.
        let pool = self.tessellation.pool(frame);
        let mut factor_calls: Vec<(vk::DescriptorSet, crate::TessFactorPush, u32)> = Vec::new();
        let mut scan_calls: Vec<(vk::DescriptorSet, crate::TessScanPush, u32)> = Vec::new();
        let mut finalize_calls: Vec<(vk::DescriptorSet, crate::TessFinalizePush)> = Vec::new();
        let mut emit_calls: Vec<(vk::DescriptorSet, crate::TessEmitPush, u32)> = Vec::new();
        for (row, inst) in insts.iter().enumerate() {
            let layout = layouts[row];
            let (Some(factor_set), Some(scan_set), Some(finalize_set), Some(emit_set)) = (
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    factor_layout,
                    &[inst.welded, inst.edges, factors],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    scan_layout,
                    &[inst.tri_edges, factors, pertri, counters, global],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    finalize_layout,
                    &[counters, seeds, prims],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    emit_layout,
                    &[
                        inst.base_vertices,
                        inst.base_indices,
                        pertri,
                        inst.tri_edges,
                        factors,
                        out_vb,
                        out_ib,
                        prev_factors,
                        out_prev_vb,
                    ],
                ),
            ) else {
                continue;
            };
            factor_calls.push((
                factor_set,
                crate::tessellation::factor_push(
                    &cam,
                    inst.model,
                    inst.factor_cap,
                    inst.min_factor,
                    inst.edge_length_target,
                    layout.factor_base,
                    inst.edge_count,
                    // Full local-space displacement amplitude (world `height_scale` mapped to local by
                    // the uniform world scale); the kernel scales it by the per-edge local height range
                    // sampled from the min/max pyramid at `height_index`.
                    inst.height_scale,
                    inst.height_index,
                    [inst.uv_transform[0], inst.uv_transform[1]],
                ),
                inst.edge_count.div_ceil(64).max(1),
            ));
            scan_calls.push((
                scan_set,
                crate::TessScanPush {
                    tri_count: inst.tri_count,
                    tri_base: layout.tri_base,
                    counter_base: layout.instance_row * 2,
                    factor_cap: inst.factor_cap,
                    _pad: [0; 4],
                },
                inst.tri_count.div_ceil(64).max(1),
            ));
            finalize_calls.push((
                finalize_set,
                crate::TessFinalizePush {
                    counter_base: layout.instance_row * 2,
                    seed_base: layout.instance_row * 5,
                    vertex_base: layout.vertex_base,
                    index_base: layout.index_base,
                    instance_row: layout.instance_row,
                    _pad: [0; 3],
                },
            ));
            emit_calls.push((
                emit_set,
                crate::TessEmitPush {
                    tri_base: layout.tri_base,
                    vertex_base: layout.vertex_base,
                    index_base: layout.index_base,
                    height_index: inst.height_index,
                    height_scale: inst.height_scale,
                    factor_cap: inst.factor_cap,
                    vector_index: inst.vector_index,
                    _pad0: 0,
                    uv_transform: inst.uv_transform,
                    factor_base: layout.factor_base,
                    _pad: [0; 3],
                },
                inst.tri_count,
            ));
        }
        let args_set =
            crate::tessellation::wire_storage_set(raw, pool, args_layout, &[global, dispatch])?;

        let factors_res = graph.import_buffer(factors, None);
        let pertri_res = graph.import_buffer(pertri, None);
        let counters_res = graph.import_buffer(counters, None);
        let global_res = graph.import_buffer(global, None);
        let seeds_res = graph.import_buffer(seeds, None);
        let prims_res = graph.import_buffer(prims, None);
        let dispatch_res = graph.import_buffer(dispatch, None);
        let out_vb_res = graph.import_buffer(out_vb, None);
        let out_ib_res = graph.import_buffer(out_ib, None);
        let out_prev_vb_res = graph.import_buffer(out_prev_vb, None);

        // Factor: one thread per unique base edge writes its shared fractional factor. Bindless set 0
        // (the min/max pyramid tap) is bound once; each instance binds its edge set (set 1) + push.
        {
            let pso = factor_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = factor_calls;
            let pass = crate::RgPass::compute("tess-factor")
                .access(factors_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. PSO + sets are valid this frame; each dispatch covers one
                    // instance's edges.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[bindless_set],
                            &[],
                        );
                        for (set, push, groups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                1,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Scan: clear the atomic accumulators (transfer write → the one hand-written transfer→compute
        // barrier, as the graph has no fill primitive), then one thread per base triangle predicts +
        // prefix-sums the exact dice counts.
        {
            let pso = scan_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = scan_calls;
            let counters_buf = counters;
            let global_buf = global;
            let clear_bytes = counters_bytes.max(8);
            let pass = crate::RgPass::compute("tess-scan")
                .access(factors_res, crate::RgUsage::StorageReadCompute)
                .access(pertri_res, crate::RgUsage::StorageWriteCompute)
                .access(counters_res, crate::RgUsage::StorageWriteCompute)
                .access(global_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The fills zero the atomic accumulators; the barrier orders
                    // them before the scan's atomic reads/writes.
                    unsafe {
                        raw_body.cmd_fill_buffer(cmd, counters_buf, 0, clear_bytes, 0);
                        raw_body.cmd_fill_buffer(cmd, global_buf, 0, 8, 0);
                        let barrier = |buffer, size| {
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(
                                    vk::AccessFlags2::SHADER_STORAGE_READ
                                        | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                )
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(buffer)
                                .offset(0)
                                .size(size)
                        };
                        let barriers = [barrier(counters_buf, clear_bytes), barrier(global_buf, 8)];
                        let dep = vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                        raw_body.cmd_pipeline_barrier2(cmd, &dep);
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        for (set, push, groups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Finalize: one thread per instance writes the indirect draw seed + RT prim count.
        {
            let pso = finalize_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = finalize_calls;
            let seeds_buf = seeds;
            let seed_bytes = instance_rows as u64 * 20;
            let pass = crate::RgPass::compute("tess-finalize")
                .access(counters_res, crate::RgUsage::StorageReadCompute)
                .access(seeds_res, crate::RgUsage::StorageWriteCompute)
                .access(prims_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The fill zeroes every row's seed first: the binner
                    // builds a command from whichever row a record names, and a row whose
                    // descriptor set failed to wire would otherwise hand it last use's bytes.
                    // A zero index count is a no-op draw.
                    unsafe {
                        raw_body.cmd_fill_buffer(cmd, seeds_buf, 0, seed_bytes.max(4), 0);
                        let cleared = vk::MemoryBarrier2::default()
                            .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                            .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                        let barriers = [cleared];
                        let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
                        raw_body.cmd_pipeline_barrier2(cmd, &dep);
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        for (set, push) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, 1, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Args: one thread turns the global micro-vertex total into the emit dispatch size.
        {
            let pso = args_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let pass = crate::RgPass::compute("tess-args")
                .access(global_res, crate::RgUsage::StorageReadCompute)
                .access(dispatch_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[args_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, 1, 1, 1);
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Emit: one workgroup per base triangle (dispatched per instance) dices + displaces + welds
        // + writes the amplified micro-vertices + generated index stream into the transient VB/IB at
        // the scan's predicted offsets, reading the static base stream via bindless set 0.
        {
            let pso = emit_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = emit_calls;
            let ib_clear_bytes = ib_cur as u64 * 4;
            let out_ib_buf = out_ib;
            let pass = crate::RgPass::compute("tess-emit")
                .access(pertri_res, crate::RgUsage::StorageReadCompute)
                .access(factors_res, crate::RgUsage::StorageReadCompute)
                .access(out_vb_res, crate::RgUsage::StorageWriteCompute)
                .access(out_ib_res, crate::RgUsage::StorageWriteCompute)
                .access(out_prev_vb_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Bindless set 0 is bound once; each instance binds its emit
                    // set (set 1) + push and dispatches one workgroup per base triangle.
                    unsafe {
                        // Zero the whole index slice first so every index past the real (GPU-packed)
                        // triangles is a degenerate `(0,0,0)` triangle — the worst-case RT build reads
                        // the full reserved range and the AS builder discards degenerates.
                        if ib_clear_bytes > 0 {
                            raw_body.cmd_fill_buffer(cmd, out_ib_buf, 0, ib_clear_bytes, 0);
                            let clear_barrier = vk::MemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                            let cb = [clear_barrier];
                            let dep = vk::DependencyInfo::default().memory_barriers(&cb);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                        }
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[bindless_set],
                            &[],
                        );
                        for (set, push, workgroups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                1,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *workgroups, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // RT secondary-ray coarsening: the RT BLAS is built from a separate, coarser run of the same
        // factor→scan→emit chain — a larger per-edge LOD target and a smaller dice cap, so the
        // worst-case reservation and the per-frame build both shrink. The coarse geometry is still
        // Phong-smoothed, displaced, and watertight, only lower-density; the raster path keeps fine.
        let rt_entities: std::collections::HashSet<u64> = self
            .frame_deformation
            .deformed_rt_instances
            .iter()
            .map(|rt| rt.entity)
            .collect();
        let rt_active = insts
            .iter()
            .any(|inst| inst.entity != 0 && rt_entities.contains(&inst.entity));
        let mut tess_rt_res: Option<(RgResource, RgResource)> = None;
        if rt_active {
            // Coarse per-instance placement: identical edge/tri prefix sums (same base topology), but the
            // vertex/index reservations use the coarse cap, so the coarse arena is ~1/COARSEN² of the fine.
            let mut rt_layouts: Vec<crate::TessInstanceLayout> = Vec::with_capacity(insts.len());
            let mut rt_caps: Vec<f32> = Vec::with_capacity(insts.len());
            let (mut r_edge, mut r_tri, mut r_vb, mut r_ib) = (0u32, 0u32, 0u32, 0u32);
            for (row, inst) in insts.iter().enumerate() {
                let cap = crate::tessellation::rt_coarsen_cap(inst.factor_cap, inst.min_factor);
                rt_caps.push(cap);
                let (verts, indices) =
                    crate::tessellation::tess_worst_case(inst.tri_count, cap as u32);
                rt_layouts.push(crate::TessInstanceLayout {
                    factor_base: r_edge,
                    tri_base: r_tri,
                    instance_row: row as u32,
                    vertex_base: r_vb,
                    index_base: r_ib,
                });
                r_edge += inst.edge_count;
                r_tri += inst.tri_count;
                r_vb = r_vb.saturating_add(verts as u32);
                r_ib = r_ib.saturating_add(indices as u32);
            }

            // Coarse transient scratch (grow-only, keyed, distinct from the fine buffers). The coarse
            // VB/IB feed only the RT BLAS build via device address, so they carry
            // SHADER_DEVICE_ADDRESS + ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY and never
            // VERTEX/INDEX. `tess.vb.rt.prev` backs the emit kernel's mandatory prev-stream write.
            let rt_ib_bytes = r_ib as u64 * 4;
            let rt_as_usage = storage
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
            let (
                Some(factors_rt),
                Some(pertri_rt),
                Some(counters_rt),
                Some(global_rt),
                Some(out_vb_rt),
                Some(out_ib_rt),
                Some(out_prev_vb_rt),
            ) = (
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.factor.rt",
                    r_edge as u64 * 4,
                    storage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.pertri.rt",
                    r_tri as u64 * 16,
                    storage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.counters.rt",
                    counters_bytes,
                    storage_cleared,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.global.rt",
                    8,
                    storage_cleared,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.vb.rt",
                    r_vb as u64 * 48,
                    rt_as_usage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.ib.rt",
                    rt_ib_bytes,
                    rt_as_usage | vk::BufferUsageFlags::TRANSFER_DST,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.vb.rt.prev",
                    r_vb as u64 * 48,
                    storage,
                ),
            )
            else {
                return None;
            };

            // Point each RT-consumed instance's slice at the coarse geometry + coarse worst case, so
            // the BLAS plan reads the coarse VB/IB (the degenerate-padded IB tail is discarded).
            for (row, inst) in insts.iter().enumerate() {
                if inst.entity == 0 {
                    continue;
                }
                let layout = rt_layouts[row];
                let (wc_verts, wc_indices) =
                    crate::tessellation::tess_worst_case(inst.tri_count, rt_caps[row] as u32);
                for rt in self.frame_deformation.deformed_rt_instances.iter_mut() {
                    if rt.entity == inst.entity {
                        rt.tess = Some(crate::TessRtSlice {
                            vertex_buffer: out_vb_rt,
                            index_buffer: out_ib_rt,
                            vertex_base: layout.vertex_base,
                            index_base: layout.index_base,
                            worst_case_verts: wc_verts as u32,
                            worst_case_prims: (wc_indices / 3) as u32,
                        });
                    }
                }
            }

            // The coarse chain reuses the fine PSOs (cached; the request clones the Arc) — factor + scan +
            // emit only (RT needs neither the raster indirect-draw finalize nor the emit-dispatch args).
            let (Some(factor_pso_rt), Some(scan_pso_rt), Some(emit_pso_rt)) = (
                self.pipelines
                    .request_tess_factor(bindless_layout, factor_layout),
                self.pipelines.request_tess_scan(scan_layout),
                self.pipelines
                    .request_tessellate(bindless_layout, emit_layout),
            ) else {
                return None;
            };

            let mut factor_calls_rt: Vec<(vk::DescriptorSet, crate::TessFactorPush, u32)> =
                Vec::new();
            let mut scan_calls_rt: Vec<(vk::DescriptorSet, crate::TessScanPush, u32)> = Vec::new();
            let mut emit_calls_rt: Vec<(vk::DescriptorSet, crate::TessEmitPush, u32)> = Vec::new();
            for (row, inst) in insts.iter().enumerate() {
                let layout = rt_layouts[row];
                let cap = rt_caps[row];
                let (Some(factor_set), Some(scan_set), Some(emit_set)) = (
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        factor_layout,
                        &[inst.welded, inst.edges, factors_rt],
                    ),
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        scan_layout,
                        &[
                            inst.tri_edges,
                            factors_rt,
                            pertri_rt,
                            counters_rt,
                            global_rt,
                        ],
                    ),
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        emit_layout,
                        &[
                            inst.base_vertices,
                            inst.base_indices,
                            pertri_rt,
                            inst.tri_edges,
                            factors_rt,
                            out_vb_rt,
                            out_ib_rt,
                            // RT has no temporal history: prev factors == cur ⇒ zero geomorph motion.
                            factors_rt,
                            out_prev_vb_rt,
                        ],
                    ),
                ) else {
                    continue;
                };
                factor_calls_rt.push((
                    factor_set,
                    crate::tessellation::factor_push(
                        &cam,
                        inst.model,
                        cap,
                        inst.min_factor,
                        // The coarsened LOD target — the sole per-edge coarsening lever (the factor kernel
                        // is otherwise identical, so a shared edge stays bit-identical → crack-free).
                        crate::tessellation::rt_coarsen_target(inst.edge_length_target),
                        layout.factor_base,
                        inst.edge_count,
                        inst.height_scale,
                        inst.height_index,
                        [inst.uv_transform[0], inst.uv_transform[1]],
                    ),
                    inst.edge_count.div_ceil(64).max(1),
                ));
                scan_calls_rt.push((
                    scan_set,
                    crate::TessScanPush {
                        tri_count: inst.tri_count,
                        tri_base: layout.tri_base,
                        counter_base: layout.instance_row * 2,
                        factor_cap: cap,
                        _pad: [0; 4],
                    },
                    inst.tri_count.div_ceil(64).max(1),
                ));
                emit_calls_rt.push((
                    emit_set,
                    crate::TessEmitPush {
                        tri_base: layout.tri_base,
                        vertex_base: layout.vertex_base,
                        index_base: layout.index_base,
                        height_index: inst.height_index,
                        height_scale: inst.height_scale,
                        factor_cap: cap,
                        vector_index: inst.vector_index,
                        _pad0: 0,
                        uv_transform: inst.uv_transform,
                        factor_base: layout.factor_base,
                        _pad: [0; 3],
                    },
                    inst.tri_count,
                ));
            }

            let factors_rt_res = graph.import_buffer(factors_rt, None);
            let pertri_rt_res = graph.import_buffer(pertri_rt, None);
            let counters_rt_res = graph.import_buffer(counters_rt, None);
            let global_rt_res = graph.import_buffer(global_rt, None);
            let out_vb_rt_res = graph.import_buffer(out_vb_rt, None);
            let out_ib_rt_res = graph.import_buffer(out_ib_rt, None);
            let out_prev_vb_rt_res = graph.import_buffer(out_prev_vb_rt, None);

            // Coarse factor: one thread per unique base edge, coarse LOD target + coarse cap.
            {
                let pso = factor_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = factor_calls_rt;
                let pass = crate::RgPass::compute("tess-factor-rt")
                    .access(factors_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. PSO + sets are valid this frame; one dispatch per instance.
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set],
                                &[],
                            );
                            for (set, push, groups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    1,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            // Coarse scan: clear the atomic accumulators, then predict + prefix-sum the coarse dice counts.
            {
                let pso = scan_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = scan_calls_rt;
                let counters_buf = counters_rt;
                let global_buf = global_rt;
                let clear_bytes = counters_bytes.max(8);
                let pass = crate::RgPass::compute("tess-scan-rt")
                    .access(factors_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(pertri_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(counters_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(global_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. The fills zero the atomic accumulators; the barrier orders
                        // them before the scan's atomic reads/writes.
                        unsafe {
                            raw_body.cmd_fill_buffer(cmd, counters_buf, 0, clear_bytes, 0);
                            raw_body.cmd_fill_buffer(cmd, global_buf, 0, 8, 0);
                            let barrier = |buffer, size| {
                                vk::BufferMemoryBarrier2::default()
                                    .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                    .dst_access_mask(
                                        vk::AccessFlags2::SHADER_STORAGE_READ
                                            | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                    )
                                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .buffer(buffer)
                                    .offset(0)
                                    .size(size)
                            };
                            let barriers =
                                [barrier(counters_buf, clear_bytes), barrier(global_buf, 8)];
                            let dep =
                                vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            for (set, push, groups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    0,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            // Coarse emit: degenerate-pad the whole coarse IB, then dice + displace + weld into the
            // coarse VB/IB. No vertex/index/indirect fetch barrier — the coarse output feeds only the
            // RT BLAS build, whose emit→build barrier the graph derives from the declared accesses.
            {
                let pso = emit_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = emit_calls_rt;
                let ib_clear_bytes = rt_ib_bytes;
                let out_ib_buf = out_ib_rt;
                let pass = crate::RgPass::compute("tess-emit-rt")
                    .access(pertri_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(factors_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(out_vb_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(out_ib_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(out_prev_vb_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. Bindless set 0 is bound once; each instance binds its emit
                        // set (set 1) + push and dispatches one workgroup per base triangle.
                        unsafe {
                            // Zero the whole coarse index slice so every index past the GPU-packed tail is
                            // a degenerate `(0,0,0)` triangle — the worst-case RT BUILD range reads the full
                            // reserved span and the AS builder discards the degenerates (watertight floor).
                            if ib_clear_bytes > 0 {
                                raw_body.cmd_fill_buffer(cmd, out_ib_buf, 0, ib_clear_bytes, 0);
                                let clear_barrier = vk::MemoryBarrier2::default()
                                    .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                                let cb = [clear_barrier];
                                let dep = vk::DependencyInfo::default().memory_barriers(&cb);
                                raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            }
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set],
                                &[],
                            );
                            for (set, push, workgroups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    1,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *workgroups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            tess_rt_res = Some((out_vb_rt_res, out_ib_rt_res));
        }

        // The factor pass above is committed to write `slot[cur_slot]` this frame with `edge_layout`;
        // stamp that so next frame's prev-stream lookup can tell whether the slot is layout-aligned.
        self.tessellation.set_factor_layout(cur_slot, edge_layout);
        tess_rt_res
    }
}

/// Declares one raster pass's reads on the frame's displacement arena: the amplified
/// micro-vertex stream its displaced buckets pull through the address block, and the index
/// stream those buckets bind. `with_prev` adds the previous-frame micro-vertex stream (the
/// motion pass). A no-op on a frame where nothing displaces.
pub(super) fn access_displaced_arena(
    graph: &mut RenderGraph,
    pass: RgPass,
    displaced: Option<crate::DisplacedFrameBuffers>,
    with_prev: bool,
) -> RgPass {
    let Some(arena) = displaced else {
        return pass;
    };
    let vertices = graph.import_buffer(arena.vertices, None);
    let indices = graph.import_buffer(arena.indices, None);
    let mut pass = pass
        .access(vertices, RgUsage::ShaderDeviceAddressRead)
        .access(indices, RgUsage::IndexInputRead);
    if with_prev {
        let prev = graph.import_buffer(arena.prev_vertices, None);
        pass = pass.access(prev, RgUsage::ShaderDeviceAddressRead);
    }
    pass
}
