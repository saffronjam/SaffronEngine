//! The per-view anti-aliasing targets: the MSAA attachments, the FXAA scratch, and the TAA
//! ping-pong history plus its jitter and reprojection state.

use super::*;

impl ViewTarget {
    /// (Re)creates the AA targets for the active mode — the motion-vector target + its
    /// depth scratch (built when TAA *or* SSGI is on, since both need it), TAA's two
    /// ping-pong history images (when TAA is on), the FXAA/TAA 1× scratch (when either is
    /// on), and the MSAA multisampled scene color + depth (when MSAA is on) — then rewrites
    /// the FXAA + TAA sets and repoints the ssgi-accum binding 2 + mesh set-4 SSGI sampler
    /// for the new mode. Resets the temporal validity (a mode change / resize invalidates
    /// reprojection). Call after [`ViewTarget::build_screen_space`] (it depends on the
    /// freshly built SSGI maps) at init, every resize, and every AA change.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_aa_targets(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
    ) -> Result<()> {
        self.build_aa_targets_impl(device, descriptors, aa, false)
    }

    /// Rebuilds the AA targets for a render-scale-only change (dynamic resolution): recreates the
    /// INPUT-extent members (motion, its depth, the scene scratch, the reactive mask) while
    /// PRESERVING the DISPLAY-extent TAA history + lock ping-pong and `history_valid`. The resolve
    /// resamples the (now differently-sized) input into the fixed display grid every frame and
    /// motion reprojects in resolution-independent UV space, so the accumulator rides an input
    /// change — flushing it would flicker at every budget step (the bug this variant prevents).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing image creation or init transition.
    pub fn build_aa_targets_preserving_temporal(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
    ) -> Result<()> {
        self.build_aa_targets_impl(device, descriptors, aa, true)
    }

    pub(super) fn build_aa_targets_impl(
        &mut self,
        device: &Device,
        descriptors: &Descriptors,
        aa: crate::Aa,
        preserve_temporal: bool,
    ) -> Result<()> {
        // Two extent classes: the scene / motion / MSAA targets rasterise at INPUT extent; the
        // history + overlay depth live at DISPLAY extent (where the resolve reconstructs).
        let input = self.scaled_render_extent();
        let display = self.published_extent();
        // Drop the previous mode's INPUT-extent targets; rebuilt below for the active mode. The
        // DISPLAY-extent history + lock are preserved on a scale-only change (see below).
        self.motion = None;
        self.motion_depth = None;
        self.reactive = None;
        self.depth_display = None;
        self.scratch = None;
        self.msaa_color = None;
        self.msaa_depth = None;
        if !preserve_temporal {
            // A mode change / resize invalidates the temporal reprojection + ping-pong parity, and
            // restarts the jitter cycle (seed phase 0 so the first frame is already jittered). The
            // jitter is a fraction of an INPUT pixel — the scene renders at input extent.
            self.history = [None, None];
            self.lock = [None, None];
            self.history_valid = false;
            self.history_index = 0;
            self.prev_view_proj_valid = false;
            self.jitter_index = 0;
            self.jitter = crate::jitter_offset(0, input.width, input.height);
            self.prev_jitter = self.jitter;
        }
        if input.width == 0 || input.height == 0 || display.width == 0 || display.height == 0 {
            return Ok(());
        }
        let resources = device.resources();
        let storage_sampled = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::STORAGE;

        // The motion target is built whenever the screen-space chain exists,
        // unconditionally: both TAA and SSGI reproject
        // through it, and which one runs is gated per frame, not by the target's existence.
        let need_motion = self.ssgi_resolved.is_some();
        if need_motion {
            self.motion = Some(Image::new(
                resources,
                &ImageDesc::color_2d(
                    input,
                    crate::MOTION_FORMAT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                ),
            )?);
            self.motion_depth = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: DEPTH_FORMAT,
                    // SAMPLED so the TAA resolve can read it for closest-depth velocity dilation.
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                },
            )?);
        }

        // TAA's two DISPLAY-extent ping-pong history images + the lock ping-pong (storage +
        // sampled) — the reconstruction accumulator + reconstruction state live at display
        // resolution. On a render-scale-only change these are PRESERVED (`preserve_temporal`): the
        // display extent is unchanged, so the accumulated history rides the input-extent change.
        if aa.taa() {
            if !preserve_temporal {
                let mut history_0 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                let mut history_1 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                // The history images rest ShaderReadOnly so their sampler bindings are valid
                // before the first TAA write (`history_valid` gates the actual blend).
                initialize_screen_space_layouts(device, &[&history_0, &history_1])?;
                history_0.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                history_1.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                self.history = [Some(history_0), Some(history_1)];

                // The pixel-lock ping-pong (DISPLAY extent, same parity as history): storage +
                // sampled, resting ShaderReadOnly so slot 7's binding is valid before the first write.
                let mut lock_0 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                let mut lock_1 = Image::new(
                    resources,
                    &ImageDesc::color_2d(display, OFFSCREEN_COLOR_FORMAT, storage_sampled),
                )?;
                initialize_screen_space_layouts(device, &[&lock_0, &lock_1])?;
                lock_0.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                lock_1.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
                self.lock = [Some(lock_0), Some(lock_1)];
            }

            // The reactive coverage mask (INPUT extent, r8): the coverage pass writes it, the
            // resolve samples it. Rest ShaderReadOnly so slot 6's binding is valid before the
            // first coverage write (a scene with no translucent content leaves it cleared to 0).
            let mut reactive = Image::new(
                resources,
                &ImageDesc::color_2d(
                    input,
                    crate::REACTIVE_FORMAT,
                    vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
                ),
            )?;
            initialize_screen_space_layouts(device, &[&reactive])?;
            reactive.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            self.reactive = Some(reactive);
        }

        // The INPUT-extent scene-color scratch: the scene always rasterises into it, and the
        // resolve stage (FXAA / TAA, or the no-AA copy) always writes the DISPLAY-extent offscreen
        // — so a graphics pass never mixes attachment extents. Allocated unconditionally.
        self.scratch = Some(Image::new(
            resources,
            &ImageDesc::color_2d(
                input,
                OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            ),
        )?);

        // The DISPLAY-extent overlay depth: point-upscaled from the input-extent scene depth each
        // frame (the depth-upscale pass) so the grid / gizmo depth-test on the display grid. Built
        // whenever AA targets exist (the overlays run in every AA mode).
        self.depth_display = Some(Image::new(
            resources,
            &ImageDesc {
                extent: display,
                format: DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?);

        // The MSAA multisampled scene color + depth (INPUT extent, resolved into scratch / depth).
        if aa.msaa() {
            self.msaa_color = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: OFFSCREEN_COLOR_FORMAT,
                    usage: vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    aspect: vk::ImageAspectFlags::COLOR,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: aa.sample_count(),
                },
            )?);
            self.msaa_depth = Some(Image::new(
                resources,
                &ImageDesc {
                    extent: input,
                    format: DEPTH_FORMAT,
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: aa.sample_count(),
                },
            )?);
        }

        self.write_aa_sets(device, descriptors, aa);
        Ok(())
    }

    /// Writes the FXAA + TAA sets and repoints the ssgi-accum binding 2 (motion) + the mesh
    /// set-4 SSGI sampler for the active AA mode. The TAA / FXAA scene input is the scratch
    /// image when built, else the offscreen as a valid placeholder (the set is unused until
    /// that mode turns on + rebinds).
    pub(super) fn write_aa_sets(&self, device: &Device, descriptors: &Descriptors, aa: crate::Aa) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let scene_input = self
            .scratch
            .as_ref()
            .map_or(self.offscreen.view(), Image::view);
        let offscreen = self.offscreen.view();
        let motion = self.aa_view(&self.motion);
        let motion_depth = self.aa_view(&self.motion_depth);
        let reactive = self.aa_view(&self.reactive);
        let ssgi_denoised = self.view_of(&self.ssgi_denoised);
        let ssgi_resolved = self.view_of(&self.ssgi_resolved);
        let dfao_denoised = self.view_of(&self.dfao_denoised);
        let dfao_resolved = self.view_of(&self.dfao_resolved);

        let mut plan: Vec<Binding> = vec![
            // fxaa: scratch source sampler -> offscreen storage.
            Binding::sampled(self.fxaa_set, 0, linear, scene_input),
            Binding::storage(self.fxaa_set, 1, offscreen),
            // motion-vector visualization: motion sampler -> offscreen storage. `motion` is
            // the offscreen placeholder when not built; the visualize pass runs only when the
            // real target exists, so the placeholder is never sampled.
            Binding::sampled(self.motion_vis_set, 0, linear, motion),
            Binding::storage(self.motion_vis_set, 1, offscreen),
            // mesh set 4 binding 2: the SSGI map the scene samples — the temporally
            // resolved map when TAA is on, the spatially denoised map otherwise.
            Binding::sampled_image(
                self.mesh_set,
                2,
                if aa.taa() {
                    ssgi_resolved
                } else {
                    ssgi_denoised
                },
            ),
            // Scene-resolve copy (no-AA / MSAA path): the input-extent scratch sampler -> the
            // display-extent offscreen storage, a normalized-UV upscale dispatched at display.
            Binding::sampled(self.scene_resolve_set, 0, linear, scene_input),
            Binding::storage(self.scene_resolve_set, 1, offscreen),
            // Depth-upscale: the input-extent scene depth sampler feeding the display-extent
            // overlay depth (the fragment point-samples it per display pixel).
            Binding::sampled(self.depth_upscale_set, 0, linear, self.depth.view()),
        ];
        // TAA parities: parity p reads scratch/history[1-p]/motion, writes offscreen +
        // history[p]. The ssgi-accum binding 2 (motion) is rebound to the real motion
        // target now that it exists (build_screen_space seeded denoised as a placeholder).
        for p in 0..2usize {
            let taa = self.taa_sets[p];
            plan.push(Binding::sampled(taa, 0, linear, scene_input));
            plan.push(Binding::sampled(
                taa,
                1,
                linear,
                self.taa_history_view(1 - p),
            ));
            plan.push(Binding::sampled(taa, 2, linear, motion));
            plan.push(Binding::storage(taa, 3, offscreen));
            plan.push(Binding::storage(taa, 4, self.taa_history_view(p)));
            // Motion-prepass depth for closest-depth velocity dilation (placeholder when TAA
            // is off, never sampled until the mode turns on and rebinds).
            plan.push(Binding::sampled(taa, 5, linear, motion_depth));
            // Reactive coverage (6), the previous lock read at the reprojected UV (7, the opposite
            // parity), and this frame's lock write (8, this parity).
            plan.push(Binding::sampled(taa, 6, linear, reactive));
            plan.push(Binding::sampled(taa, 7, linear, self.taa_lock_view(1 - p)));
            plan.push(Binding::storage(taa, 8, self.taa_lock_view(p)));

            let accum = self.ssgi_accum_sets[p];
            plan.push(Binding::sampled(accum, 0, linear, ssgi_denoised));
            plan.push(Binding::sampled(accum, 1, linear, self.history_view(1 - p)));
            plan.push(Binding::sampled(accum, 2, linear, motion));
            plan.push(Binding::storage(accum, 3, ssgi_resolved));
            plan.push(Binding::storage(accum, 4, self.history_view(p)));

            // The dfao-accum motion binding (2) is likewise rebound to the real motion target.
            let dfao = self.dfao_accum_sets[p];
            plan.push(Binding::sampled(dfao, 0, linear, dfao_denoised));
            plan.push(Binding::sampled(
                dfao,
                1,
                linear,
                self.dfao_history_view(1 - p),
            ));
            plan.push(Binding::sampled(dfao, 2, linear, motion));
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
        // SAFETY: the ash seam. The sets/views/samplers outlive the renderer; host access
        // to these per-view sets is single-threaded at the (idle) build point.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// Rewrites this frame slot's bloom sets to bind the passed `(source, target)` view pairs — one
    /// pair per pyramid pass, in graph order (downsamples, then upsamples, then the composite). The
    /// bloom mip images come from the per-frame-in-flight transient pool, so only slot `frame`'s
    /// sets are touched; that slot's previous GPU use finished `MAX_FRAMES_IN_FLIGHT` frames ago
    /// (its fence was waited at frame begin), so the update is hazard-free. The source binds through
    /// the linear clamp sampler (binding 0, `SHADER_READ_ONLY`), the target as a storage image
    /// (binding 1, `GENERAL`).
    pub fn write_bloom_sets(
        &self,
        device: &Device,
        descriptors: &Descriptors,
        frame: usize,
        pairs: &[(vk::ImageView, vk::ImageView)],
        composite: BloomCompositeBindings,
    ) {
        let raw = device.raw();
        let linear = descriptors.linear_sampler();
        let base = frame * BLOOM_PASSES_PER_FRAME;
        let last = pairs.len().saturating_sub(1);
        let mut plan: Vec<Binding> = Vec::with_capacity(pairs.len() * 4);
        for (i, &(source, target)) in pairs.iter().enumerate() {
            let set = self.bloom_sets[base + i];
            plan.push(Binding::sampled(set, 0, linear, source));
            plan.push(Binding::storage(set, 1, target));
            // Only the composite (last) pass samples the dirt mask + streak; the earlier passes bind
            // the harmless fallback so every set is complete (the shader statically references both).
            let (dirt, streak) = if i == last {
                (composite.dirt, composite.streak)
            } else {
                (composite.fallback, composite.fallback)
            };
            plan.push(Binding::sampled(set, 2, linear, dirt));
            plan.push(Binding::sampled(set, 3, linear, streak));
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
        // SAFETY: the ash seam. Only slot `frame`'s sets are written; that slot's prior use has
        // signalled its fence, so no in-flight command buffer references these sets.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
    }

    /// The bloom set for pass `pass` in this frame slot (`slot * BLOOM_PASSES_PER_FRAME + pass`).
    pub fn bloom_set(&self, frame: usize, pass: usize) -> vk::DescriptorSet {
        self.bloom_sets[frame * BLOOM_PASSES_PER_FRAME + pass]
    }

    /// The view handle of a built AA `Option<Image>` (motion), or — when not built (the
    /// mode is off) — the offscreen as a valid placeholder so the set is complete.
    pub(super) fn aa_view(&self, image: &Option<Image>) -> vk::ImageView {
        image.as_ref().map_or(self.offscreen.view(), Image::view)
    }

    /// The view handle of TAA history slot `i`, or the offscreen as a placeholder when TAA
    /// is off (the set is unused until TAA turns on + rebinds).
    pub(super) fn taa_history_view(&self, i: usize) -> vk::ImageView {
        self.history[i]
            .as_ref()
            .map_or(self.offscreen.view(), Image::view)
    }

    /// The view handle of TAA pixel-lock slot `i`, or the offscreen as a placeholder when TAA
    /// is off (same lifetime + parity as the history ping-pong).
    pub(super) fn taa_lock_view(&self, i: usize) -> vk::ImageView {
        self.lock[i]
            .as_ref()
            .map_or(self.offscreen.view(), Image::view)
    }

    /// Flips the temporal ping-pong parity + marks the history valid after a frame's TAA /
    /// SSGI accumulation consumed this frame's parity. The next frame reprojects through the
    /// buffer just written.
    pub fn flip_history(&mut self) {
        self.history_valid = true;
        self.history_index = 1 - self.history_index;
    }

    /// Records this frame's camera viewProj as the per-view previous frame for next frame's
    /// motion reprojection.
    pub fn store_prev_view_proj(&mut self, view_proj: saffron_geometry::glam::Mat4) {
        self.prev_view_proj = view_proj;
        self.prev_view_proj_valid = true;
    }

    /// Advances the Halton jitter cycle for the next frame: rolls this frame's offset into
    /// `prev_jitter`, steps the phase index, and recomputes `jitter` for the new index at the
    /// current extent. Called at the frame tail only while TAA is active — mirroring
    /// `store_prev_view_proj`, which records this frame's matrix as next frame's previous.
    pub fn advance_jitter(&mut self) {
        self.prev_jitter = self.jitter;
        // The jitter is a fraction of an INPUT pixel — the scene renders at input extent — and the
        // cycle length scales with the upscale ratio so a heavier upscale eventually covers every
        // display pixel with a jittered input sample.
        let input = self.scaled_render_extent();
        let display = self.published_extent();
        let phases = crate::jitter_phase_count(input, display);
        self.jitter_index = (self.jitter_index + 1) % phases;
        self.jitter = crate::jitter_offset(self.jitter_index, input.width, input.height);
    }
}
