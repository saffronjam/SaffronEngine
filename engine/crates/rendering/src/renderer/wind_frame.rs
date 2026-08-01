use super::*;

impl Renderer {
    /// Creates (and zero-fills, on first use) this frame's per-world wind sway records and
    /// interaction field, stages the impulse ring, and publishes the GPU-scene address block.
    pub(super) fn record_wind_frame_resources(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
    ) -> Result<WindFrameResources> {
        // The wind deformation prepass output buffer: one sway record per instance slot,
        // (re)created behind an idle wait, then zero-filled in this frame's graph before any read.
        let wind_world = self.active_view.gpu_scene_world();
        let wind_capacity = self
            .gpu_scene_uploader
            .world_instance_capacity(wind_world)
            .max(1);
        let wind_records_created = self
            .wind_deform_records
            .get(&wind_world.0)
            .is_none_or(|records| records.capacity < wind_capacity);
        if wind_records_created {
            self.device.wait_idle()?;
            let buffer = crate::Buffer::new(
                self.device.resources(),
                u64::from(wind_capacity) * size_of::<crate::GpuWindInstanceRecord>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferDevice,
                    ..Default::default()
                },
            )?;
            self.wind_deform_records.insert(
                wind_world.0,
                WindDeformRecords {
                    buffer,
                    capacity: wind_capacity,
                },
            );
        }
        let wind_records_handle = self.wind_deform_records[&wind_world.0].buffer.handle();
        let wind_records_address = self.device.buffer_device_address(wind_records_handle);
        // The world interaction field: fixed size, created once per world; the
        // impulse ring holds this frame's staged impulses for the step pass.
        let interaction_created = !self.interaction_fields.contains_key(&wind_world.0);
        if interaction_created {
            let buffer = crate::Buffer::new(
                self.device.resources(),
                crate::GPU_INTERACTION_FIELD_BYTES,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferDevice,
                    ..Default::default()
                },
            )?;
            self.interaction_fields.insert(wind_world.0, buffer);
        }
        let interaction_handle = self.interaction_fields[&wind_world.0].handle();
        let interaction_address = self.device.buffer_device_address(interaction_handle);
        while self.interaction_impulse_ring.len() < crate::MAX_FRAMES_IN_FLIGHT {
            self.interaction_impulse_ring.push(crate::Buffer::new(
                self.device.resources(),
                256 * size_of::<crate::InteractionImpulse>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }
        self.interaction_impulses.truncate(256);
        let interaction_impulse_count = self.interaction_impulses.len() as u32;
        if interaction_impulse_count > 0 {
            let ring = &self.interaction_impulse_ring[frame];
            // SAFETY: HOST_VISIBLE + MAPPED; the frame slot's fence passed before reuse.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.interaction_impulses.as_ptr().cast::<u8>(),
                    ring.mapped_ptr(),
                    interaction_impulse_count as usize * size_of::<crate::InteractionImpulse>(),
                );
            }
        }
        let interaction_impulse_address = self
            .device
            .buffer_device_address(self.interaction_impulse_ring[frame].handle());
        self.interaction_impulses.clear();
        // The ray-instance identity table is sized before the block is published, for the same
        // reason the amplification arena is: the block carries its address.
        let ray_instances = self.rt.ensure_frame_ray_instances(
            &self.device,
            frame,
            self.frame_deformation.deformed_rt_instances.len(),
        );
        let address_block = self.gpu_scene_uploader.build_address_block(
            &self.device,
            &self.global_gpu_data,
            self.active_view.gpu_scene_world(),
            frame,
            self.skinning.frame_deformed_addresses(frame, &self.device),
            wind_records_address,
            interaction_address,
            // Only armed while the profiler is: an atomic in every geometry fragment is a real
            // cost, and a zero address makes an unprofiled frame execute no increment at all.
            if self.gpu_profiler.mode == ProfilerMode::Off {
                0
            } else {
                self.views[self.active_view.index()]
                    .visibility_view
                    .as_ref()
                    .map_or(0, |view| view.counters_address(&self.device, frame))
            },
            self.displaced_addresses,
            ray_instances,
            self.views[self.active_view.index()].jitter_index,
        );
        self.gpu_scene_uploader
            .write_address_block(frame, address_block);
        let wind_records_res = graph.import_buffer(wind_records_handle, None);
        let interaction_field_res = graph.import_buffer(interaction_handle, None);
        if wind_records_created || interaction_created {
            let raw = self.device.raw().clone();
            let clear_records = wind_records_created.then_some(wind_records_handle);
            let clear_field = interaction_created.then_some(interaction_handle);
            let mut clear = crate::RgPass::compute("wind-clear");
            if clear_records.is_some() {
                clear = clear.access(wind_records_res, crate::RgUsage::TransferWrite);
            }
            if clear_field.is_some() {
                clear = clear.access(interaction_field_res, crate::RgUsage::TransferWrite);
            }
            graph.add_pass(clear.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Both buffers are TRANSFER_DST.
                unsafe {
                    if let Some(handle) = clear_records {
                        raw.cmd_fill_buffer(cmd, handle, 0, vk::WHOLE_SIZE, 0);
                    }
                    if let Some(handle) = clear_field {
                        raw.cmd_fill_buffer(cmd, handle, 0, vk::WHOLE_SIZE, 0);
                    }
                }
            }));
        }
        Ok(WindFrameResources {
            address_block,
            records: wind_records_res,
            interaction_field: interaction_field_res,
            interaction_address,
            impulse_address: interaction_impulse_address,
            impulse_count: interaction_impulse_count,
        })
    }
}
