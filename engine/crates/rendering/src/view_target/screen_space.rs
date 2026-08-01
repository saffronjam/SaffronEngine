//! The screen-space effect chain's per-view images and the descriptor sets that bind them: the
//! thin G-buffer, AO / contact / SSGI / DFAO maps, and their histories.

use super::*;

impl ViewTarget {
    /// Allocates this view's per-view screen-space descriptor sets once. The sets are
    /// rewritten by
    /// [`ViewTarget::build_screen_space`] whenever the images recreate; allocating
    /// them once (not per resize) avoids churning the pool.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if any `vkAllocateDescriptorSets` fails.
    pub fn allocate_screen_space_sets(
        &mut self,
        descriptors: &Descriptors,
        ssao: &Ssao,
    ) -> Result<()> {
        self.gtao_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ao_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.contact_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ssgi_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.ssr_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.ssgi_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.dfao_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.dfao_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.dfao_accum_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        // Specular occlusion: the trace's I/O set is the compute3 shape (two samplers — the
        // G-buffer + roughness — plus the storage image), unlike DFAO's compute2 trace.
        self.specocc_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.specocc_blur_set = descriptors.allocate_set(ssao.compute3_layout())?;
        self.copy_color_set = descriptors.allocate_set(ssao.compute2_layout())?;
        // The scene-resolve copy (input scratch -> display offscreen) shares the copy_color
        // compute2 shape; the depth-upscale set is the single fragment-sampler graphics layout.
        self.scene_resolve_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.depth_upscale_set = descriptors.allocate_set(descriptors.depth_upscale_layout())?;
        for slot in &mut self.gi_resolve_sets {
            *slot = descriptors.allocate_set(ssao.gi_resolve_layout())?;
        }
        self.motion_vis_set = descriptors.allocate_set(ssao.compute2_layout())?;
        self.ssgi_accum_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        self.fxaa_set = descriptors.allocate_set(descriptors.fxaa_set_layout())?;
        self.taa_sets = [
            descriptors.allocate_set(descriptors.taa_set_layout())?,
            descriptors.allocate_set(descriptors.taa_set_layout())?,
        ];
        self.mesh_set = descriptors.allocate_set(mesh_set_layout(descriptors))?;
        self.tonemap_set = descriptors.allocate_set(descriptors.tonemap_set_layout())?;
        self.fog_set = descriptors.allocate_set(descriptors.fog_set_layout())?;
        // Bloom binds one source/target pair per pyramid pass, allocated per frame-in-flight so a
        // slot's sets are only rewritten `MAX_FRAMES_IN_FLIGHT` frames after their last GPU use
        // (the transient mip images they bind are themselves per-slot).
        self.bloom_sets.clear();
        for _ in 0..(BLOOM_PASSES_PER_FRAME * MAX_FRAMES_IN_FLIGHT) {
            self.bloom_sets
                .push(descriptors.allocate_set(descriptors.bloom_set_layout())?);
        }
        Ok(())
    }

    /// (Re)creates the screen-space images at the current viewport extent and writes
    /// every per-view set to bind them, transitioning the mesh-sampled maps + prevColor
    /// to `SHADER_READ_ONLY_OPTIMAL` so set 4 is valid even before the passes first run
    /// (each read is gated by its enable flag in the übershader). Resets the SSGI
    /// history validity (a resize invalidates the reprojection). `ssao` supplies the
    /// device-shared nearest G-buffer sampler the set writes bind.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_screen_space(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        ssao: &Ssao,
    ) -> Result<()> {
        // The whole screen-space / G-buffer chain rasterises at the INPUT (render) extent.
        let extent = self.scaled_render_extent();
        if extent.width == 0 || extent.height == 0 {
            return Ok(());
        }
        let resources = device.resources();

        let storage_sampled = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::STORAGE;
        // The G-buffer is a color attachment + sampled only (never a storage image).
        let g_normal = Image::new(
            resources,
            &ImageDesc::color_2d(
                extent,
                G_NORMAL_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            ),
        )?;
        // The G-buffer's roughness target (R8): a color attachment + sampled, like g_normal.
        let g_roughness = Image::new(
            resources,
            &ImageDesc::color_2d(
                extent,
                ROUGHNESS_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            ),
        )?;
        let g_depth = Image::new(
            resources,
            &ImageDesc {
                extent,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;
        // SSGI + GTAO trace into half-resolution targets (`ao_raw`, `ssgi_map`); the bilateral
        // `ao-blur` / `ssgi-blur` passes upsample them back to full-res against the full-res
        // G-buffer depth. Halving the ray-march raster is the bulk of the screen-space GI cost.
        // Round up so an odd extent still covers every full-res pixel after the 2x upsample.
        let half_extent = vk::Extent2D {
            width: extent.width.div_ceil(2).max(1),
            height: extent.height.div_ceil(2).max(1),
        };
        let display_extent = self.published_extent();
        let cloud_extent = vk::Extent2D {
            width: display_extent.width.div_ceil(2).max(1),
            height: display_extent.height.div_ceil(2).max(1),
        };
        let ao_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, AO_FORMAT, storage_sampled),
        )?;
        let ao_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, AO_FORMAT, storage_sampled),
        )?;
        let contact_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, AO_FORMAT, storage_sampled),
        )?;
        let ssgi_map = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssr_map = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssgi_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let ssgi_resolved = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        // DFAO: the cone trace runs at half res (like GTAO/SSGI), then a bilateral upsample
        // (`dfao_denoised`) + temporal accumulation (`dfao_resolved` + history) bring it to full
        // res and denoise it across frames.
        let dfao_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let dfao_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let dfao_resolved = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut dfao_history_0 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut dfao_history_1 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        // Specular occlusion is spatial-only (view-dependent — no temporal reuse): a half-res
        // trace + a full-res bilateral upsample, both rgba16f.
        let specocc_raw = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let specocc_denoised = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut prev_color = Image::new(
            resources,
            &ImageDesc::color_2d(extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut ssgi_history_0 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut ssgi_history_1 = Image::new(
            resources,
            &ImageDesc::color_2d(extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_0 = Image::new(
            resources,
            &ImageDesc::color_2d(cloud_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_1 = Image::new(
            resources,
            &ImageDesc::color_2d(cloud_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_reduced_depth = Image::new(
            resources,
            &ImageDesc::color_2d(
                cloud_extent,
                vk::Format::R16_SFLOAT,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE,
            ),
        )?;
        let mut cloud_full_color = Image::new(
            resources,
            &ImageDesc::color_2d(display_extent, OFFSCREEN_COLOR_FORMAT, storage_sampled),
        )?;
        let mut cloud_full_depth = Image::new(
            resources,
            &ImageDesc::color_2d(
                display_extent,
                vk::Format::R32_SFLOAT,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE,
            ),
        )?;
        let mut gi_indirect = Image::new(
            resources,
            &ImageDesc::color_2d(half_extent, G_NORMAL_FORMAT, storage_sampled),
        )?;
        let mut ao_map = ao_map;
        let mut contact_map = contact_map;
        let mut ssgi_map = ssgi_map;
        let mut ssr_map = ssr_map;
        let mut ssgi_denoised = ssgi_denoised;
        let mut ssgi_resolved = ssgi_resolved;
        let mut dfao_raw = dfao_raw;
        let mut dfao_denoised = dfao_denoised;
        let mut dfao_resolved = dfao_resolved;
        let mut specocc_raw = specocc_raw;
        let mut specocc_denoised = specocc_denoised;

        // Transition the mesh-sampled maps + prevColor + the SSGI history to
        // ShaderReadOnly so their descriptors are valid even before the passes run (the
        // shader gates each read), and so the SSGI / mesh samplers + the graph's seed
        // layout agree. A one-time init transition. The
        // storage-only scratch (ao_raw, ssgi_map written first by their producing pass)
        // stay UNDEFINED until the graph transitions them — except ssgi_map, also read
        // as a sampler by ssgi_blur, so it is seeded too.
        let read_only: [&Image; 22] = [
            &ao_map,
            &contact_map,
            &ssgi_map,
            &ssr_map,
            &ssgi_denoised,
            &ssgi_resolved,
            &dfao_raw,
            &dfao_denoised,
            &dfao_resolved,
            &dfao_history_0,
            &dfao_history_1,
            &specocc_raw,
            &specocc_denoised,
            &prev_color,
            &ssgi_history_0,
            &ssgi_history_1,
            &cloud_reduced_0,
            &cloud_reduced_1,
            &cloud_reduced_depth,
            &cloud_full_color,
            &cloud_full_depth,
            &gi_indirect,
        ];
        initialize_screen_space_layouts(device, &read_only)?;
        for image in [
            &mut ao_map,
            &mut contact_map,
            &mut ssgi_map,
            &mut ssr_map,
            &mut ssgi_denoised,
            &mut ssgi_resolved,
            &mut dfao_raw,
            &mut dfao_denoised,
            &mut dfao_resolved,
            &mut dfao_history_0,
            &mut dfao_history_1,
            &mut specocc_raw,
            &mut specocc_denoised,
            &mut prev_color,
            &mut ssgi_history_0,
            &mut ssgi_history_1,
            &mut cloud_reduced_0,
            &mut cloud_reduced_1,
            &mut cloud_reduced_depth,
            &mut cloud_full_color,
            &mut cloud_full_depth,
            &mut gi_indirect,
        ] {
            image.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }

        // The gi-resolve half-res indirect-diffuse output + its per-frame-slot params UBOs (mapped,
        // memcpy'd per frame — the descriptor sets stay stable so there is no in-flight hazard).
        let gi_params_size = size_of::<crate::ssao::GiParams>() as vk::DeviceSize;
        let mut gi_params_ubos = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            gi_params_ubos.push(Buffer::new(
                resources,
                gi_params_size,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }

        self.g_normal = Some(g_normal);
        self.g_roughness = Some(g_roughness);
        self.g_depth = Some(g_depth);
        self.ao_raw = Some(ao_raw);
        self.ao_map = Some(ao_map);
        self.contact_map = Some(contact_map);
        self.ssgi_map = Some(ssgi_map);
        self.ssr_map = Some(ssr_map);
        self.ssgi_denoised = Some(ssgi_denoised);
        self.ssgi_resolved = Some(ssgi_resolved);
        self.dfao_raw = Some(dfao_raw);
        self.dfao_denoised = Some(dfao_denoised);
        self.dfao_resolved = Some(dfao_resolved);
        self.dfao_history = [Some(dfao_history_0), Some(dfao_history_1)];
        self.specocc_raw = Some(specocc_raw);
        self.specocc_denoised = Some(specocc_denoised);
        self.gi_indirect = Some(gi_indirect);
        self.gi_params_ubos = gi_params_ubos;
        self.prev_color = Some(prev_color);
        self.ssgi_history = [Some(ssgi_history_0), Some(ssgi_history_1)];
        self.cloud_reduced = [Some(cloud_reduced_0), Some(cloud_reduced_1)];
        self.cloud_reduced_depth = Some(cloud_reduced_depth);
        self.cloud_full_color = Some(cloud_full_color);
        self.cloud_full_depth = Some(cloud_full_depth);
        // A resize invalidates the temporal reprojection; the next frame re-seeds.
        self.history_valid = false;
        self.history_index = 0;

        self.write_screen_space_sets(device, descriptors, ssao.nearest_sampler());
        Ok(())
    }

    /// Writes every per-view screen-space set to bind this view's freshly built images.
    pub(super) fn write_screen_space_sets(
        &self,
        device: &Device,
        descriptors: &Descriptors,
        nearest: vk::Sampler,
    ) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let g_normal = self.view_of(&self.g_normal);
        let g_roughness = self.view_of(&self.g_roughness);
        let ao_raw = self.view_of(&self.ao_raw);
        let ao_map = self.view_of(&self.ao_map);
        let contact_map = self.view_of(&self.contact_map);
        let ssgi_map = self.view_of(&self.ssgi_map);
        let ssr_map = self.view_of(&self.ssr_map);
        let ssgi_denoised = self.view_of(&self.ssgi_denoised);
        let ssgi_resolved = self.view_of(&self.ssgi_resolved);
        let dfao_raw = self.view_of(&self.dfao_raw);
        let dfao_denoised = self.view_of(&self.dfao_denoised);
        let dfao_resolved = self.view_of(&self.dfao_resolved);
        let specocc_raw = self.view_of(&self.specocc_raw);
        let specocc_denoised = self.view_of(&self.specocc_denoised);
        let gi_indirect = self.view_of(&self.gi_indirect);
        let prev_color = self.view_of(&self.prev_color);
        let offscreen = self.offscreen.view();
        let depth = self.depth.view();
        let cloud_full_color = self.view_of(&self.cloud_full_color);
        let cloud_full_depth = self.view_of(&self.cloud_full_depth);

        // Each binding is a `(set, binding, kind)`. A sampler binding pairs a sampler +
        // a view (ShaderReadOnly); a storage binding is a view only (GENERAL). The whole
        // plan is one literal so the `DescriptorImageInfo` arena (filled below) parallels
        // it — keeping every borrow valid for the single `update_descriptor_sets` call.
        let mut plan: Vec<Binding> = vec![
            // gtao: g_normal -> ao_raw
            Binding::sampled(self.gtao_set, 0, nearest, g_normal),
            Binding::storage(self.gtao_set, 1, ao_raw),
            // ao_blur: ao_raw + g_normal -> ao_map. ao_raw is half-res, so a LINEAR sampler
            // bilinearly upsamples it; the depth-weighted taps then keep edges crisp.
            Binding::sampled(self.ao_blur_set, 0, linear, ao_raw),
            Binding::sampled(self.ao_blur_set, 1, nearest, g_normal),
            Binding::storage(self.ao_blur_set, 2, ao_map),
            // contact: g_normal -> contact_map
            Binding::sampled(self.contact_set, 0, nearest, g_normal),
            Binding::storage(self.contact_set, 1, contact_map),
            // ssgi: g_normal + prev_color -> ssgi_map
            Binding::sampled(self.ssgi_set, 0, nearest, g_normal),
            Binding::sampled(self.ssgi_set, 1, linear, prev_color),
            Binding::storage(self.ssgi_set, 2, ssgi_map),
            // ssr: g_normal + prev_color -> ssr_map
            Binding::sampled(self.ssr_set, 0, nearest, g_normal),
            Binding::sampled(self.ssr_set, 1, linear, prev_color),
            Binding::storage(self.ssr_set, 2, ssr_map),
            // ssgi_blur: ssgi_map + g_normal -> ssgi_denoised. ssgi_map is half-res, so a LINEAR
            // sampler bilinearly upsamples it; the depth-weighted taps keep edges crisp.
            Binding::sampled(self.ssgi_blur_set, 0, linear, ssgi_map),
            Binding::sampled(self.ssgi_blur_set, 1, nearest, g_normal),
            Binding::storage(self.ssgi_blur_set, 2, ssgi_denoised),
            // dfao trace (set 2): g_normal -> dfao_raw (the GDF sky-visibility cone trace).
            Binding::sampled(self.dfao_set, 0, nearest, g_normal),
            Binding::storage(self.dfao_set, 1, dfao_raw),
            // dfao-blur: dfao_raw + g_normal -> dfao_denoised. dfao_raw is half-res, so a LINEAR
            // sampler bilinearly upsamples it (reuses the ssgi-blur PSO shape).
            Binding::sampled(self.dfao_blur_set, 0, linear, dfao_raw),
            Binding::sampled(self.dfao_blur_set, 1, nearest, g_normal),
            Binding::storage(self.dfao_blur_set, 2, dfao_denoised),
            // specocc trace (set 2): g_normal + g_roughness -> specocc_raw (the GDF reflection
            // occlusion cone trace). Compute3 shape: two nearest-sampled inputs + one storage out.
            Binding::sampled(self.specocc_set, 0, nearest, g_normal),
            Binding::sampled(self.specocc_set, 1, nearest, g_roughness),
            Binding::storage(self.specocc_set, 2, specocc_raw),
            // specocc-blur: specocc_raw + g_normal -> specocc_denoised. specocc_raw is half-res, so
            // a LINEAR sampler bilinearly upsamples it (reuses the ssgi-blur PSO shape).
            Binding::sampled(self.specocc_blur_set, 0, linear, specocc_raw),
            Binding::sampled(self.specocc_blur_set, 1, nearest, g_normal),
            Binding::storage(self.specocc_blur_set, 2, specocc_denoised),
            // copy_color: offscreen -> prev_color
            Binding::sampled(self.copy_color_set, 0, linear, offscreen),
            Binding::storage(self.copy_color_set, 1, prev_color),
            // mesh set 4: AO + contact + denoised SSGI (all linear-sampled). Without motion
            // it samples the spatially denoised map — the accum pass is off.
            Binding::sampled_image(self.mesh_set, 0, ao_map),
            Binding::sampled_image(self.mesh_set, 1, contact_map),
            Binding::sampled_image(self.mesh_set, 2, ssgi_denoised),
            Binding::sampled_image(self.mesh_set, 3, ssr_map),
            Binding::sampled_image(self.mesh_set, 4, prev_color),
            // mesh set 4 binding 5: the temporally-resolved DFAO sky-visibility. The accum runs
            // whenever motion runs (TAA / SSGI / DFAO all force it on), so the mesh always samples
            // the resolved map; on a frame with no valid history it equals the spatial result.
            Binding::sampled_image(self.mesh_set, 5, dfao_resolved),
            // mesh set 4 binding 6: the spatially-denoised specular reflection-occlusion. It is
            // view-dependent, so it is not temporally accumulated (surface-motion reprojection would
            // smear it); the half-res trace + bilateral upsample are its whole denoise.
            Binding::sampled_image(self.mesh_set, 6, specocc_denoised),
            // mesh set 4 binding 7: the half-res screen-space indirect-diffuse resolve (DDGI + IBL
            // diffuse × sky-vis), linear-sampled to bilinearly upsample. Replaces the fragment's own
            // per-pixel DDGI cage + IBL-diffuse resolve.
            Binding::sampled_image(self.mesh_set, 7, gi_indirect),
            // The mandatory tonemap set: binding 0 = the offscreen color as a storage
            // image (GENERAL).
            Binding::storage(self.tonemap_set, 0, offscreen),
            // The height-fog set: binding 0 = the offscreen color (storage, GENERAL), binding 2 =
            // the scene depth (point-sampled; the graph transitions it to ShaderReadOnly). Binding 1
            // (the fog params UBO) is a dynamic-offset write below; binding 3 (the sky-view LUT) is
            // written once by the renderer.
            Binding::storage(self.fog_set, 0, offscreen),
            Binding::sampled(self.fog_set, 2, nearest, depth),
            Binding::sampled(self.fog_set, 8, linear, cloud_full_color),
            Binding::sampled(self.fog_set, 9, nearest, cloud_full_depth),
        ];
        // gi-resolve view-local image bindings, into every per-frame set (the shared IBL cube +
        // DDGI atlases at b3/b5/b6 are written from the renderer, which owns those sub-states; the
        // b2 params UBO is a buffer write, done after this image update). dfao is linear-sampled so
        // the half-res sky-visibility upsamples cleanly.
        for &set in &self.gi_resolve_sets {
            plan.push(Binding::sampled(set, 0, nearest, g_normal));
            plan.push(Binding::storage(set, 1, gi_indirect));
            // b4 = the temporally accumulated DFAO, linear-sampled for the half-res upsample. The
            // trace rotates its cone ring every frame, so the spatial result carries that rotation
            // variance indefinitely and only the EMA converges; gi-resolve rides the sky-visibility
            // at full weight wherever DDGI is absent, which would put the variance straight into
            // the indirect diffuse. The accum pass runs earlier in the same frame, so this is this
            // frame's result, not last frame's.
            plan.push(Binding::sampled(set, 4, linear, dfao_resolved));
        }
        // b2: each slot binds its own mapped GiParams UBO (a buffer write, its own update call).
        for (i, set) in self.gi_resolve_sets.iter().enumerate() {
            descriptors.write_uniform_buffer(
                *set,
                2,
                self.gi_params_ubos[i].handle(),
                self.gi_params_ubos[i].size(),
            );
        }
        // ssgi-accum parities: parity p reads ssgi_history[1-p], writes ssgi_history[p].
        // Without motion, binding 2 (motion) gets the denoised map as a neutral placeholder
        // so the set is complete (the accum pass is off without motion).
        for p in 0..2usize {
            let set = self.ssgi_accum_sets[p];
            plan.push(Binding::sampled(set, 0, linear, ssgi_denoised));
            plan.push(Binding::sampled(set, 1, linear, self.history_view(1 - p)));
            plan.push(Binding::sampled(set, 2, linear, ssgi_denoised));
            plan.push(Binding::storage(set, 3, ssgi_resolved));
            plan.push(Binding::storage(set, 4, self.history_view(p)));

            // dfao-accum parities: parity p reads dfao_history[1-p], writes dfao_history[p].
            // Binding 2 (motion) is the denoised placeholder here; write_aa_sets rebinds it to
            // the real motion target once it is built.
            let dfao = self.dfao_accum_sets[p];
            plan.push(Binding::sampled(dfao, 0, linear, dfao_denoised));
            plan.push(Binding::sampled(
                dfao,
                1,
                linear,
                self.dfao_history_view(1 - p),
            ));
            plan.push(Binding::sampled(dfao, 2, linear, dfao_denoised));
            plan.push(Binding::storage(dfao, 3, dfao_resolved));
            plan.push(Binding::storage(dfao, 4, self.dfao_history_view(p)));
        }

        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: the ash seam. The sets/views/samplers outlive the renderer; host
        // access to these per-view sets is single-threaded at the (idle) build point.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        // The tonemap set's binding 1: the grade UBO as one dynamic-offset slice (the per-frame slice
        // is chosen by the dispatch's dynamic offset, so this is written once and reused every frame).
        descriptors.write_dynamic_uniform_buffer(
            self.tonemap_set,
            1,
            self.grade_ubo.handle(),
            self.grade_ubo_stride,
        );

        // The fog set's binding 1: the fog params UBO as one dynamic-offset slice (the per-frame
        // slice is chosen by the dispatch's dynamic offset, so this is written once and reused).
        descriptors.write_dynamic_uniform_buffer(
            self.fog_set,
            1,
            self.fog_ubo.handle(),
            self.fog_ubo_stride,
        );
    }

    /// Writes the *shared* gi-resolve bindings into every per-frame set — b3 the sky SH buffer,
    /// b5 the DDGI irradiance atlas, b6 the DDGI distance-moment atlas. Separate
    /// from [`ViewTarget::write_screen_space_sets`] because the IBL + DDGI sub-states are owned by
    /// the renderer (not visible at view build). The renderer calls this once they are ready, and
    /// re-calls it on IBL rebake / DDGI rebuild (the views change) — the same triggers the mesh
    /// sets use. All three are sampled `SHADER_READ_ONLY`, matching how the mesh samples them.
    #[allow(clippy::too_many_arguments)]
    pub fn write_gi_resolve_shared(
        &self,
        device: &Device,
        frame: usize,
        sky_sh: vk::Buffer,
        sky_sh_size: vk::DeviceSize,
        ddgi_irradiance: vk::ImageView,
        ddgi_distance: vk::ImageView,
        ddgi_sampler: vk::Sampler,
    ) {
        // Write ONLY this frame's slot — the other slots may be bound by an in-flight command buffer
        // (writing them would trip VUID-vkUpdateDescriptorSets-None-03047). This frame's slot was last
        // used `MAX_FRAMES_IN_FLIGHT` frames ago, whose fence `begin_frame` already waited, so it is
        // free. Every slot converges to the current views over that many frames.
        let set = self.gi_resolve_sets[frame];
        if set == vk::DescriptorSet::null() {
            return; // sets not allocated yet (screen-space not built)
        }
        let plan: [Binding; 2] = [
            Binding::sampled(set, 5, ddgi_sampler, ddgi_irradiance),
            Binding::sampled(set, 6, ddgi_sampler, ddgi_distance),
        ];
        let infos: Vec<vk::DescriptorImageInfo> = plan.iter().map(Binding::info).collect();
        let sh_info = [vk::DescriptorBufferInfo::default()
            .buffer(sky_sh)
            .offset(0)
            .range(sky_sh_size)];
        let mut writes: Vec<vk::WriteDescriptorSet> = plan
            .iter()
            .zip(infos.iter())
            .map(|(binding, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(binding.set)
                    .dst_binding(binding.binding)
                    .descriptor_type(binding.kind())
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&sh_info),
        );
        // SAFETY: the ash seam. Sets/views/samplers are valid; the renderer calls this at a
        // post-fence point where the sets are not in use by an in-flight frame.
        unsafe { device.raw().update_descriptor_sets(&writes, &[]) };
    }

    /// The view handle of an `Option<Image>`, or null if it is not built (the
    /// screen-space set writes never run before `build_screen_space`, so this is
    /// always populated when used).
    pub(super) fn view_of(&self, image: &Option<Image>) -> vk::ImageView {
        image.as_ref().map_or(vk::ImageView::null(), Image::view)
    }

    /// The view handle of SSGI history slot `i`.
    pub(super) fn history_view(&self, i: usize) -> vk::ImageView {
        self.ssgi_history[i]
            .as_ref()
            .map_or(vk::ImageView::null(), Image::view)
    }

    /// The view handle of DFAO history slot `i`.
    pub(super) fn dfao_history_view(&self, i: usize) -> vk::ImageView {
        self.dfao_history[i]
            .as_ref()
            .map_or(vk::ImageView::null(), Image::view)
    }

    /// Whether the screen-space chain is built (the G-buffer image exists).
    pub fn screen_space_ready(&self) -> bool {
        self.g_normal.is_some()
    }
}

/// One `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` init transition over `images` so their
/// descriptors are valid before any pass runs. A one-shot submit + wait at the
/// (idle) build point.
pub(super) fn initialize_screen_space_layouts(device: &Device, images: &[&Image]) -> Result<()> {
    use crate::checked;
    let raw = device.raw();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "ssao init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = checked(
        unsafe { raw.allocate_command_buffers(&alloc) },
        "ssao init cmd",
    )?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "ssao init fence",
    )?;

    let result = (|| -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let barriers: Vec<vk::ImageMemoryBarrier2> = images
            .iter()
            .map(|image| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                    .src_access_mask(vk::AccessFlags2::empty())
                    .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image.handle())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
            })
            .collect();
        // SAFETY: the ash seam. The barriers reference images this device created.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "ssao init begin")?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "ssao init end")?;
        }
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. The queue is touched single-threaded at the build point.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "ssao init submit")?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "ssao init wait",
            )?;
        }
        Ok(())
    })();

    // SAFETY: the ash seam. The fence was waited (or the submit never happened), so the
    // pool/fence are idle and destroyed exactly once.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}
