//! Reflection probes: local environment captures prefiltered into the shared IBL set.

use super::*;

/// One captured + prefiltered local reflection probe. Mirrors the [`Ibl`] cube layout but
/// per-probe: a captured env cube + 6 face render views + a depth scratch, convolved into a
/// per-probe irradiance + prefiltered cube. Sampled via the IBL set (bindings 3-4) at
/// this slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflectionProbe {
    /// World-space origin (the entity translation).
    pub origin: Vec3,
    /// Influence radius (world units).
    pub influence_radius: f32,
    /// Specular intensity multiplier.
    pub intensity: f32,
    /// Box-projection (parallax-corrected) reflections.
    pub box_projection: bool,
    /// Box half-extents.
    pub box_extent: Vec3,
    /// Owning entity id (the capture re-uses the slot when re-armed).
    pub entity: u64,
    /// Cubes created (the lazy per-slot allocation).
    pub allocated: bool,
    /// Captured + written into the IBL set at least once (else the slot resolves to the
    /// global IBL cubes).
    pub valid: bool,
    /// (Re)capture pending this frame.
    pub dirty: bool,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            influence_radius: 10.0,
            intensity: 1.0,
            box_projection: false,
            box_extent: Vec3::splat(10.0),
            entity: 0,
            allocated: false,
            valid: false,
            dirty: false,
        }
    }
}

/// The reflection-probe array + the metadata SSBO (IBL set bindings 3-5) + the per-frame
/// capture state. Every array slot is seeded with the global IBL cubes so the bind is
/// always valid; real probes overwrite
/// their slot on capture.
pub struct ReflectionProbes {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) probes: [ReflectionProbe; MAX_REFLECTION_PROBES as usize],
    pub(super) count: u32,
    pub(super) sampler: vk::Sampler,
    pub(super) meta_buffers: Vec<Buffer>,
    /// Master probe toggle.
    pub use_probes: bool,
    /// Any probe dirty this frame → capture at the next idle point.
    pub capture_pending: bool,
    pub(super) warned_overflow: bool,
}

impl ReflectionProbes {
    /// Allocates one probe-metadata SSBO per frame slot, the probe sampler, and seeds the
    /// buffers to zero. Descriptor seeding happens after the first IBL bake via
    /// [`ReflectionProbes::seed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing buffer/sampler step.
    pub fn new(device: &Device) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let raw = resources.device();

        let sampler = create_ibl_sampler(raw)?;
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let size = (size_of::<ProbeMetaGpu>() * MAX_REFLECTION_PROBES as usize) as vk::DeviceSize;
        let mut meta_buffers = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let mut buffer = match Buffer::new(
                &resources,
                size,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                &alloc_info,
            ) {
                Ok(buffer) => buffer,
                Err(err) => {
                    // SAFETY: the ash seam. Free the sampler on the early return; any
                    // buffers already allocated free through their Drop implementation.
                    unsafe { raw.destroy_sampler(sampler, None) };
                    return Err(err);
                }
            };
            if let Some(dst) = buffer.mapped_bytes() {
                dst.fill(0);
            }
            meta_buffers.push(buffer);
        }

        Ok(Self {
            resources,
            probes: std::array::from_fn(|_| ReflectionProbe::default()),
            count: 0,
            sampler,
            meta_buffers,
            use_probes: true,
            capture_pending: false,
            warned_overflow: false,
        })
    }

    /// Seeds every probe array slot in every frame's IBL set (bindings 3/4) with the global
    /// IBL cubes and binds that frame's metadata SSBO at binding 5. Called once after the
    /// first IBL bake.
    pub fn seed(&self, ibl: &Ibl) {
        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            self.seed_set(ibl.set(frame), ibl, &self.meta_buffers[frame]);
        }
    }

    pub(super) fn seed_set(&self, dst_set: vk::DescriptorSet, ibl: &Ibl, meta_buffer: &Buffer) {
        let raw = self.resources.device();
        for slot in 0..MAX_REFLECTION_PROBES {
            self.write_slot(raw, dst_set, ibl, slot as usize);
        }
        let buffer_info = [vk::DescriptorBufferInfo::default()
            .buffer(meta_buffer.handle())
            .offset(0)
            .range(meta_buffer.size())];
        let write = [vk::WriteDescriptorSet::default()
            .dst_set(dst_set)
            .dst_binding(5)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&buffer_info)];
        // SAFETY: the ash seam. Host access at the (idle) post-bake point is single-threaded.
        unsafe { raw.update_descriptor_sets(&write, &[]) };
    }

    /// Rebinds uncaptured project probe slots to a newly committed global environment.
    /// Bindings 3/4 are update-after-bind; binding 5 remains fixed for the renderer lifetime.
    pub fn refresh_fallbacks(&self, ibl: &Ibl) {
        let raw = self.resources.device();
        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            let set = ibl.set(frame);
            for (slot, probe) in self.probes.iter().enumerate() {
                if !probe.valid {
                    self.write_slot(raw, set, ibl, slot);
                }
            }
        }
    }

    /// Rebinds every fallback slot in a secondary IBL whose sets never carry captured probes.
    /// Bindings 3/4 are update-after-bind; binding 5 remains fixed for the renderer lifetime.
    pub fn refresh_secondary_fallbacks(&self, ibl: &Ibl) {
        let raw = self.resources.device();
        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            let set = ibl.set(frame);
            for slot in 0..MAX_REFLECTION_PROBES as usize {
                self.write_slot(raw, set, ibl, slot);
            }
        }
    }

    /// Writes one probe slot's prefiltered (binding 3) + irradiance (binding 4) cube into
    /// the IBL set. A slot with no captured probe falls back to the global IBL cubes, so
    /// every array element is always valid.
    pub(super) fn write_slot(
        &self,
        raw: &ash::Device,
        dst_set: vk::DescriptorSet,
        ibl: &Ibl,
        slot: usize,
    ) {
        // There is no per-slot cube storage — every slot resolves to the global IBL cubes
        // (a real capture overwrites the slot via `write_captured`).
        let pre = ibl.live.prefiltered.view;
        let irr = ibl.front.env.view;
        let infos = [
            vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(pre)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
            vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(irr)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(3)
                .dst_array_element(slot as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&infos[0])),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(4)
                .dst_array_element(slot as u32)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&infos[1])),
        ];
        // SAFETY: the ash seam. Host access at the (idle) post-bake point is single-threaded.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// Folds the host's per-frame probe uploads in:
    /// re-arms a slot on a real change (new/moved/resized probe, or an explicit dirty flag),
    /// drops removed slots, and stages the metadata represented by the next frame slot.
    /// Overflow past `MAX_REFLECTION_PROBES` is logged once.
    pub fn submit(&mut self, uploads: &[ReflectionProbeUpload]) {
        let cap = MAX_REFLECTION_PROBES as usize;
        let mut count = uploads.len();
        if count > cap {
            if !self.warned_overflow {
                tracing::warn!("more than {cap} reflection probes — excess ignored");
                self.warned_overflow = true;
            }
            count = cap;
        }

        let mut any_dirty = false;
        for (probe, up) in self.probes.iter_mut().zip(&uploads[..count]) {
            let slot_changed = probe.entity != up.entity
                || probe.origin != up.origin
                || probe.influence_radius != up.influence_radius;
            if up.dirty || slot_changed || !probe.valid {
                probe.dirty = true;
                any_dirty = true;
            }
            probe.entity = up.entity;
            probe.origin = up.origin;
            probe.influence_radius = up.influence_radius;
            probe.intensity = up.intensity;
            probe.box_projection = up.box_projection;
            probe.box_extent = up.box_extent;
        }
        for probe in &mut self.probes[count..] {
            probe.entity = 0;
            probe.valid = false;
            probe.dirty = false;
        }
        self.count = count as u32;
        if any_dirty {
            self.capture_pending = true;
        }
    }

    /// Writes probe metadata into the frame slot whose fence has completed. The shader reads
    /// only `frame_probe_count()` records; disabled probes therefore sample no records.
    pub fn prepare_frame(&mut self, frame: usize) {
        let sample_count = self.frame_probe_count();
        let mut meta = [ProbeMetaGpu::zeroed(); MAX_REFLECTION_PROBES as usize];
        for (slot, probe) in meta
            .iter_mut()
            .zip(&self.probes)
            .take(sample_count as usize)
        {
            *slot = ProbeMetaGpu {
                origin_radius: probe.origin.extend(probe.influence_radius),
                extent_intensity: probe.box_extent.extend(probe.intensity),
                flags: UVec4::new(
                    u32::from(probe.valid),
                    u32::from(probe.box_projection),
                    0,
                    0,
                ),
            };
        }
        if let Some(dst) = self.meta_buffers[frame].mapped_bytes() {
            let bytes = bytemuck::bytes_of(&meta);
            dst[..bytes.len()].copy_from_slice(bytes);
        }
    }

    /// The reflection-probe count the mesh fragment iterates this frame (0 when probes are
    /// disabled). The renderer folds this into the light UBO's `ambientColor.w`.
    pub fn frame_probe_count(&self) -> u32 {
        if self.use_probes { self.count } else { 0 }
    }

    /// The active probe-slot count (≤ `MAX_REFLECTION_PROBES`).
    pub fn count(&self) -> u32 {
        self.count
    }

    /// The captured reflection probes (origin / radius / intensity / validity), in slot
    /// order — the `list-probes` control command's source.
    pub fn probes(&self) -> &[ReflectionProbe] {
        &self.probes[..self.count as usize]
    }
}

impl Drop for ReflectionProbes {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The device idled before teardown; the sampler is freed once.
        // The meta buffers free via their Drop; the sets free with the shared pool.
        unsafe {
            self.resources.device().destroy_sampler(self.sampler, None);
        }
    }
}
