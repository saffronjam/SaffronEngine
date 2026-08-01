use super::*;

impl Renderer {
    /// Records the micro-blade ray-geometry materialization for the frame's resident field tiles
    /// and publishes the tiles the acceleration-structure plan builds from
    /// ([`Renderer::micro_rt_tiles`]).
    ///
    /// Returns the arena's vertex and index graph resources so the caller can declare them
    /// `AccelStructBuildRead` on `tlas-build`; `None` on a frame with no field, no ray consumer,
    /// or no materialization PSO.
    pub(super) fn record_micro_rt_prep(
        &mut self,
        graph: &mut crate::RenderGraph,
        frame: usize,
        micro_rt: Option<&Arc<crate::Pipeline>>,
        interaction_field: RgResource,
    ) -> Option<(RgResource, RgResource)> {
        self.micro_rt_tiles.clear();
        let pipeline = Arc::clone(micro_rt?);
        // Ray geometry exists only for the consumers that trace it; the raster path reconstructs
        // blades from candidates either way.
        if !(self.rt.use_rt_shadows() || self.rt.use_rt_reflections()) || !self.rt.supported() {
            return None;
        }
        let (directory_offset, directory_count) = self.micro_field_directory?;
        let arena = crate::plan_micro_rt_arena(directory_count)?;
        let set = self.views[self.active_view.index()]
            .visibility_view
            .as_ref()?
            .micro_set(frame);
        if set == vk::DescriptorSet::null() {
            return None;
        }

        // The arena feeds only the bottom-level builds, so it carries the build-input flag and a
        // device address, never a vertex or index binding. The index slice is additionally
        // cleared each frame, so every index past the blades the dispatch wrote is a degenerate
        // triangle the builder discards.
        let vertex_usage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
        let index_usage = vertex_usage | vk::BufferUsageFlags::TRANSFER_DST;
        let acquire = |graph: &mut crate::RenderGraph,
                       transient: &mut RenderGraphResources,
                       key: &'static str,
                       bytes: u64,
                       usage|
         -> Option<vk::Buffer> {
            match graph.create_buffer(
                transient,
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
                    tracing::error!("micro rt prep: acquire {key}: {err}");
                    None
                }
            }
        };
        let index_bytes = u64::from(arena.indices) * 4;
        let (Some(vertices), Some(indices)) = (
            acquire(
                graph,
                &mut self.transient,
                "micro.rt.vb",
                u64::from(arena.vertices) * size_of::<saffron_geometry::Vertex>() as u64,
                vertex_usage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "micro.rt.ib",
                index_bytes,
                index_usage,
            ),
        ) else {
            return None;
        };

        self.micro_rt_tiles = crate::plan_micro_rt_tiles(arena, vertices, indices);
        let wind = self.lighting.wind_deform_push();
        let push = crate::MicroRtDeformPush {
            eye: self.page_demand_view().eye.to_array(),
            max_distance: crate::MICRO_RT_REACH_METRES,
            directory_offset,
            directory_count: arena.tiles,
            blades_per_tile: crate::MICRO_RT_BLADES_PER_TILE,
            reserved: 0,
            vertices: self.device.buffer_device_address(vertices),
            indices: self.device.buffer_device_address(indices),
            wind_dir_speed_gust: wind.dir_speed_gust,
            wind_params: wind.params,
            wind_octaves: wind.octaves,
            wind_seed: wind.seed,
            wind_time_current: wind.time_current,
            wind_time_previous: wind.time_previous,
            wind_sources: wind.sources,
            wind_source_count: wind.source_count,
            wind_reserved: 0,
        };

        let vertices_res = graph.import_buffer(vertices, None);
        let indices_res = graph.import_buffer(indices, None);
        let raw = self.device.raw().clone();
        graph.add_pass(
            crate::RgPass::compute("micro-rt-deform")
                .access(vertices_res, crate::RgUsage::StorageWriteCompute)
                .access(indices_res, crate::RgUsage::StorageWriteCompute)
                // The bend the blades carry comes from the interaction field the wind step wrote,
                // so declaring the read orders this after it: without it the graph is free to
                // materialize from last frame's trampling.
                .access(interaction_field, crate::RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The fill clears the whole index slice so an
                    // unwritten index reads as a degenerate triangle; the barrier orders it
                    // before the dispatch's stores.
                    unsafe {
                        raw.cmd_fill_buffer(cmd, indices, 0, index_bytes, 0);
                        let cleared = vk::MemoryBarrier2::default()
                            .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                            .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                        let barriers = [cleared];
                        let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
                        raw.cmd_pipeline_barrier2(cmd, &dep);
                    }
                    crate::record_micro_rt_deform(&raw, cmd, &pipeline, set, &push);
                }),
        );
        Some((vertices_res, indices_res))
    }
}
