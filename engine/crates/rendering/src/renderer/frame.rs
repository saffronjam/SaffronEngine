use super::*;

impl Renderer {
    /// Stashes an ad-hoc record closure replayed inside the scene pass after the batched draw
    /// list — the editor gizmo / native overlay seam. The closure captures resolved handles.
    pub fn submit(&mut self, body: impl FnOnce(vk::CommandBuffer) + 'static) {
        self.submissions.push(Box::new(body));
    }

    /// Submits the editor-overlay geometry for the next render: the `depth_tested` range
    /// (occluded by scene geometry) then the `on_top` range. Composited into the post-tonemap
    /// color, so the present-only path blits it too.
    ///
    /// Like the [`Renderer::submit`] closures, the geometry is consumed by the render that draws
    /// it: a render nothing submits an overlay for — a thumbnail excursion onto another view —
    /// draws none.
    pub fn submit_overlay(&mut self, depth_tested: Vec<OverlayVertex>, on_top: Vec<OverlayVertex>) {
        self.overlay.submit(depth_tested, on_top);
    }

    /// Begins the offscreen frame: waits + resets the current slot's in-flight fence and resets
    /// its command pool, so the slot is idle before any per-frame state reset (notably the layers'
    /// deformation submit, which resets the per-frame skinning descriptor pool). Arms
    /// [`Renderer::slot_fence_armed`] so the following [`Renderer::render_scene_offscreen`] does
    /// not re-wait the now-unsignaled fence.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence/pool call.
    pub fn begin_offscreen_frame(&mut self) -> Result<()> {
        // An armed slot's fence is reset and unsignaled, so the wait below would never return.
        // Close that slot first — the ring then begins on the next one.
        self.finish_unsubmitted_frame()?;
        let raw = self.device.raw();
        let in_flight = self.frames.in_flight();
        // The wait is unbounded, so a frame whose GPU work never completes blocks here forever.
        // Registering the slot lets the watchdog thread read the record that slot's last submission
        // published and name the batch the GPU is inside, instead of hanging silently.
        let _watch = crate::watchdog::watch_frame(self.frames.index());
        // SAFETY: the ash seam. The fence belongs to this device; the wait blocks until this
        // slot's prior GPU work completes, so its per-frame buffers/sets are free to reuse.
        let waited = checked(
            unsafe { raw.wait_for_fences(&[in_flight], true, u64::MAX) },
            "wait_for_fences (begin)",
        );
        if waited.as_ref().is_err_and(crate::Error::is_device_loss) {
            self.device.log_device_loss_checkpoints();
        }
        waited?;
        // The slot's prior GPU work is done, so its transient scratch allocations are free to
        // recycle: rewind the pool's acquire cursors for this frame index.
        self.transient.begin_frame(self.frames.index());
        // The tessellation prep descriptor pool recycles on the same fence: reset this slot's sets
        // before the deform scope wires the frame's factor/scan/finalize passes.
        self.tessellation.begin_frame(self.frames.index());
        // This slot's GPU work is complete, so its timestamp pool reads back without blocking:
        // fold the prior frame's per-pass GPU spans into `gpu_frame_ms` + `last_timings` here.
        let slot = self.frames.index();
        self.global_gpu_data.begin_frame(slot)?;
        self.gpu_scene_uploader.begin_frame(slot)?;
        self.persistent_gpu_scene.begin_frame(slot)?;
        // Re-sample the GPU↔CPU clock offset before the read-back so this frame's spans decode
        // onto the CPU axis (ordering: calibrate → readback). The profiler self-gates to ~1 Hz.
        self.frame_serial = self.frame_serial.wrapping_add(1);
        if self.gpu_profiler.mode != ProfilerMode::Off && self.gpu_profiler.pools_ready {
            self.gpu_profiler.calibrate(&self.device, self.frame_serial);
        }
        self.gpu_frame_ms = self
            .gpu_profiler
            .readback(&self.device, slot, self.gpu_frame_ms);
        // Drain this slot's merged spans into an in-flight capture BEFORE the upcoming
        // `render_scene_offscreen` resets the slot's CPU buffer, so both lanes describe the same
        // frame. Ticking after the reset would pair this frame's CPU spans with the older GPU
        // read-back (ordering: readback → tick-capture → reset).
        self.capture
            .tick(&self.cpu_profiler, slot, &self.gpu_profiler);
        // This slot's frame fence just signalled, so its recorded shm readback is host-visible:
        // stage those bytes for the host to publish without a stall.
        self.stage_pending_shm_publish(slot);
        // SAFETY: the ash seam. The waited fence is unsignaled and reset before resubmit.
        let raw = self.device.raw();
        checked(
            unsafe { raw.reset_fences(&[in_flight]) },
            "reset_fences (begin)",
        )?;
        // Armed the instant the fence is reset, ahead of the pool reset that can still fail: from
        // here until a submit signals it, only this flag makes the slot recoverable.
        self.slot_fence_armed = true;
        self.frames.reset_command_pools(&self.device)?;
        // Nothing is recorded into this slot yet, so this is the one point where an idle-and-
        // reallocate is free of an in-flight frame. It runs after the shm staging above, so this
        // slot's read-back is drained at the size it was recorded at.
        self.reconcile_pending_view_targets()?;
        Ok(())
    }

    /// Applies the view-target changes deferred to this frame boundary: the requested desired sizes
    /// and the budget controller's render scale. Both idle the GPU and reallocate a view's targets.
    fn reconcile_pending_view_targets(&mut self) -> Result<()> {
        for i in 0..crate::VIEW_COUNT {
            let Some((width, height)) = self.pending_view_size[i].take() else {
                continue;
            };
            self.views[i].desired_width = width;
            self.views[i].desired_height = height;
            self.apply_render_extent(i)?;
        }
        if let Some(scale) = self.pending_render_scale.take() {
            self.set_render_scale(self.active_view, scale)?;
        }
        Ok(())
    }

    /// Closes an armed frame slot — one whose in-flight fence [`Renderer::begin_offscreen_frame`]
    /// reset but no submit signalled — with an empty submit, and advances the ring.
    ///
    /// The slot is usable again only once something signals its fence, so every way out of the
    /// armed window routes through here: a layer that draws nothing never reaches
    /// `render_scene_offscreen`, and a frame that fails anywhere between the begin and the tail
    /// submit returns early with the fence still reset. The loop calls this at the end of every
    /// frame and `begin_offscreen_frame` calls it before it waits, so the next wait is always on a
    /// fence something will signal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the empty submit fails; the slot stays armed for a later retry.
    pub fn finish_unsubmitted_frame(&mut self) -> Result<()> {
        if !self.slot_fence_armed {
            return Ok(());
        }
        let fence = self.frames.in_flight();
        // A frame that failed part-way through its submit sequence can still have async-compute
        // work reading this slot's pools, and the fence is what gates resetting them — so the
        // closing signal waits the last compute point the slot actually submitted. The point is
        // recorded only after its own submit succeeded, so it is always signalled.
        let waits: Vec<vk::SemaphoreSubmitInfo<'_>> = self
            .pending_compute_signal
            .iter()
            .map(|point| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(point.semaphore)
                    .value(point.value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            })
            .collect();
        let submit = vk::SubmitInfo2::default().wait_semaphore_infos(&waits);
        let submits = [submit];
        self.device.graphics_queue.submit2(
            self.device.raw(),
            &submits,
            fence,
            "queue_submit2 (empty frame)",
        )?;
        // The closing submit reserves no timeline point, so the slot's next fence wait has nothing
        // to name: replace the previous submission's record rather than let it read as current.
        crate::watchdog::publish_frame(
            self.frames.index(),
            self.device.raw().handle(),
            self.frame_serial,
            &[],
        );
        self.slot_fence_armed = false;
        self.pending_compute_signal = None;
        self.frames.advance();
        Ok(())
    }

    /// Records and submits the scene + optional depth-prepass into the active view's offscreen
    /// target through the render graph. Call after [`Renderer::submit_gpu_scene_deformations`];
    /// submit-seam closures replay after the executor draws.
    ///
    /// The graph derives the UNDEFINED → COLOR/DEPTH attachment barriers and the depth WAW barrier
    /// from the declared usages. The offscreen image is left in `COLOR_ATTACHMENT_OPTIMAL` for a
    /// later post/capture.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing Vulkan call.
    pub fn render_scene_offscreen(&mut self) -> Result<()> {
        // Advance fence-owned IBL refreshes at the frame boundary. A completed back set is
        // descriptor-committed here; in-flight frames continue sampling the front set.
        match self.ibl.update_refresh(&self.device) {
            Ok(true) => {
                self.sky.bind_env_cube(&self.ibl);
                self.reflection.refresh_fallbacks(&self.ibl);
            }
            Ok(false) => {}
            Err(err) => tracing::error!("ibl refresh failed: {err}"),
        }
        match self.preview_ibl.update_refresh(&self.device) {
            Ok(true) => self
                .reflection
                .refresh_secondary_fallbacks(&self.preview_ibl),
            Ok(false) => {}
            Err(err) => tracing::error!("preview ibl refresh failed: {err}"),
        }

        // Wait + reset this slot's fence and command pool. The run loop calls
        // [`Renderer::begin_offscreen_frame`] in `begin_frame`, and an already-armed slot skips
        // the re-wait here; a standalone caller still gets a self-contained begin.
        if !self.slot_fence_armed {
            self.begin_offscreen_frame()?;
        }
        let raw = self.device.raw().clone();
        let frame = self.frames.index();
        let command_buffer = self.frames.command_buffer();
        self.reflection.prepare_frame(frame);

        // Reset this slot's CPU span buffer when the profiler is active; when `Off` the buffer
        // stays empty and every CPU scope below is a no-op.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        if profile_cpu {
            self.cpu_profiler.buffers[frame].reset();
        }

        // Resolve every PSO this frame needs up front (each takes `&mut self.pipelines`),
        // so the graph build below borrows the rest of `self` immutably. A `None` arms
        // nothing — a build failure (logged once) degrades to the unlit/unshadowed path.
        let depth_prepass = if self.use_depth_prepass {
            self.pipelines.request_depth_prepass_executor()
        } else {
            None
        };
        let cull_pipeline = if self.lighting.take_cluster_dispatch_pending() {
            self.pipelines.request_light_cull()
        } else {
            None
        };
        // The compute skinning PSO, resolved only when the frame built skin dispatches. The skin
        // pass deforms each instance once before every geometry pass reads it as a static stream.
        let skin_pipeline = if !self.frame_deformation.skin_dispatches.is_empty() {
            crate::skinning::request_skin_pipeline(&mut self.pipelines, &self.skinning)
        } else {
            None
        };
        // The morph compute PSO, resolved only when the frame built morph dispatches. The
        // morph pass deforms each morph instance into the deformed buffer before skin.
        let morph_pipeline = if !self.frame_deformation.morph_dispatches.is_empty() {
            crate::skinning::request_morph_pipeline(&mut self.pipelines, &self.skinning)
        } else {
            None
        };
        let shadow_pipeline = if self.vsm_render_pages.is_empty() {
            None
        } else {
            self.pipelines.request_shadow_depth_executor()
        };

        // Screen-space effects ride a thin G-buffer prepass that runs when ANY of GTAO / contact /
        // SSGI is on. Every PSO is resolved up front (each takes `&mut self.pipelines`) so the
        // graph build below borrows the rest of `self` immutably; a `None` skips that pass.
        let gbuf_ready =
            self.ssao.ready && self.views[self.active_view.index()].screen_space_ready();
        let want_ssao = gbuf_ready && self.ssao.use_ssao;
        let want_contact = gbuf_ready && self.ssao.use_contact;
        let want_ssgi = gbuf_ready && self.ssao.use_ssgi;
        let want_ssr = gbuf_ready && self.ssao.use_ssr;
        // RT reflections gather from prev_color (the screen-space chain's history copy), so
        // they force the chain on + the prev-color copy even with no other screen effect.
        let want_rt_reflections = gbuf_ready && self.rt.use_rt_reflections();
        // ReSTIR needs the thin G-buffer (it reconstructs world pos/normal from it), so it
        // forces the prepass on even with no screen-space effect, then ANDs G-buffer
        // readiness into the ReSTIR enable.
        let want_restir = self.restir.use_restir()
            && self.restir.supported()
            && self.views[self.active_view.index()].restir.ready()
            && gbuf_ready;
        // DFAO reconstructs world pos/normal from the thin G-buffer, so it forces the prepass on.
        // It runs whenever sky occlusion is active this frame (IBL + the toggle + the GDF ready).
        let want_dfao = gbuf_ready && self.want_sky_occlusion();
        let want_screen = want_restir
            || want_rt_reflections
            || want_dfao
            || crate::ssao::wants_gbuffer_prepass(
                gbuf_ready,
                self.ssao.use_ssao,
                self.ssao.use_contact,
                self.ssao.use_ssgi,
                self.ssao.use_ssr,
            );
        let compute2 = self.ssao.compute2_layout();
        let compute3 = self.ssao.compute3_layout();
        let gi_resolve_layout = self.ssao.gi_resolve_layout();
        let (gbuffer, gtao, ao_blur, contact, ssgi, ssgi_blur, ssr, copy_color) = if want_screen {
            let gbuffer = self.pipelines.request_gbuffer_executor();
            let (gtao, ao_blur) = if want_ssao {
                (
                    self.pipelines.request_gtao(compute2),
                    self.pipelines.request_ao_blur(compute3),
                )
            } else {
                (None, None)
            };
            let contact = if want_contact {
                self.pipelines.request_contact(compute2)
            } else {
                None
            };
            let (ssgi, ssgi_blur) = if want_ssgi {
                (
                    self.pipelines.request_ssgi(compute3),
                    self.pipelines.request_ssgi_blur(compute3),
                )
            } else {
                (None, None)
            };
            let ssr = if want_ssr {
                self.pipelines.request_ssr(compute3)
            } else {
                None
            };
            // SSGI, SSR, and RT reflections all gather from the previous frame's color, so
            // the prev-color copy runs when any is on.
            let copy_color = if want_ssgi || want_ssr || want_rt_reflections {
                self.pipelines.request_copy_color(compute2)
            } else {
                None
            };
            (
                gbuffer, gtao, ao_blur, contact, ssgi, ssgi_blur, ssr, copy_color,
            )
        } else {
            (None, None, None, None, None, None, None, None)
        };
        // Bump the monotonic SSGI/SSR frame indices (decorrelating the trace noise) here,
        // where `&mut self.ssao` is live; the `&self` graph build reads the snapshot below.
        let ssgi_push = self.ssao.next_ssgi_push();
        let ssr_push = self.ssao.next_ssr_push();
        // Screen-space indirect-diffuse resolve PSO — runs whenever the screen chain does, since
        // it reads the G-buffer.
        let gi_resolve = if want_screen {
            self.pipelines.request_gi_resolve(gi_resolve_layout)
        } else {
            None
        };
        // Bump the monotonic DFAO frame index (rotating the cone ring) here, where
        // `&mut self.ssao` is live; the `&self` graph build reads the snapshot below.
        let dfao_push = self.ssao.next_dfao_push();
        // Specular occlusion shares the sky-occlusion gate with DFAO. The trace is a three-set PSO
        // and the blur reuses the ssgi-blur PSO, bound with the specocc blur set.
        let (specocc, specocc_blur) = if want_dfao {
            (
                self.pipelines.request_specocc(compute3),
                self.pipelines.request_ssgi_blur(compute3),
            )
        } else {
            (None, None)
        };
        // Bump the monotonic specocc frame index here, where `&mut self.ssao` is live.
        let specocc_push = self.ssao.next_specocc_push();

        // DDGI: the four trace/blend/border PSOs, resolved together (a partial set skips the whole
        // chain). Resolved here so the `&self` graph build borrows `self.ddgi` immutably.
        let ddgi = if self.ddgi.use_ddgi && self.ddgi.ready {
            let trace = self.pipelines.request_ddgi_trace(self.ddgi.trace_layout());
            let blend_irr = self
                .pipelines
                .request_ddgi_blend_irr(self.ddgi.blend_irr_layout());
            let blend_dist = self
                .pipelines
                .request_ddgi_blend_dist(self.ddgi.blend_dist_layout());
            let border = self
                .pipelines
                .request_ddgi_border(self.ddgi.border_layout());
            match (trace, blend_irr, blend_dist, border) {
                (Some(trace), Some(blend_irr), Some(blend_dist), Some(border)) => {
                    Some(DdgiPipelines {
                        trace,
                        blend_irr,
                        blend_dist,
                        border,
                    })
                }
                _ => None,
            }
        } else {
            None
        };

        // Global SDF: the cull + composite PSOs, resolved together (both, or the chain is skipped)
        // so the `&self` graph build can borrow `self.global_sdf` immutably.
        let gdf = if self.global_sdf.use_gdf && self.global_sdf.ready {
            let cull = self
                .pipelines
                .request_gdf_cull(self.global_sdf.cull_layout());
            let composite = self
                .pipelines
                .request_gdf_composite(self.global_sdf.composite_layout());
            match (cull, composite) {
                (Some(cull), Some(composite)) => Some(GdfPipelines { cull, composite }),
                _ => None,
            }
        } else {
            None
        };

        // ReSTIR DI: the three compute PSOs, resolved together (a partial set skips the whole
        // chain). RT-only — the resolve traces a visibility ray. The runtime gate (cull +
        // G-buffer + TLAS ran) is applied in the graph build, where `tlas_ready` is known.
        let restir = if want_restir {
            let initial = self
                .pipelines
                .request_restir_initial(self.restir.initial_layout());
            let reuse = self
                .pipelines
                .request_restir_reuse(self.restir.reuse_layout());
            let resolve = self
                .pipelines
                .request_restir_resolve(self.restir.resolve_layout());
            match (initial, reuse, resolve) {
                (Some(initial), Some(reuse), Some(resolve)) => Some(RestirPipelines {
                    initial,
                    reuse,
                    resolve,
                }),
                _ => None,
            }
        } else {
            None
        };

        // The motion-vector prepass runs when TAA or SSGI is on (both reproject through it); the
        // TAA / FXAA resolves run when that mode is active and its scratch is built. The per-view
        // target checks read the active view first so no immutable borrow spans the
        // `&mut self.pipelines` requests.
        let have_motion_targets = {
            let view = &self.views[self.active_view.index()];
            view.motion.is_some() && view.motion_depth.is_some()
        };
        let have_scratch = self.views[self.active_view.index()].scratch.is_some();
        // DFAO also reprojects through the motion target, so it forces motion on (like TAA/SSGI).
        let want_cloud_motion =
            self.clouds.settings().enabled && self.view_mode != ViewMode::CloudDensity;
        let want_motion =
            (self.aa.taa() || want_ssgi || want_dfao || want_cloud_motion) && have_motion_targets;
        let motion = if want_motion {
            self.pipelines.request_motion_executor()
        } else {
            None
        };
        // The SSGI temporal accumulator PSO runs when SSGI is on AND motion ran (it reprojects
        // through the motion target), independent of the final-image AA mode.
        let ssgi_accum = if want_ssgi && have_motion_targets {
            self.pipelines
                .request_ssgi_accum(self.descriptors.taa_set_layout())
        } else {
            None
        };
        // DFAO: the three-set trace, the shared bilateral upsample (the ssgi-blur PSO bound with
        // the DFAO blur set), and the clamp-free `dfao_accum` accumulator, resolved together — a
        // partial set skips the whole chain, since the map every consumer samples is the
        // accumulated one. A neighborhood clamp there would re-inject the per-frame cone-rotation
        // variance and never converge; specular occlusion is spatial-only (its view-dependent term
        // must not be reprojected by surface motion), so it has no accumulator. The accumulator
        // reprojects through the motion target, so the chain also requires motion.
        let dfao = if want_dfao && motion.is_some() {
            let trace = self.pipelines.request_dfao(compute2);
            let blur = self.pipelines.request_ssgi_blur(compute3);
            let accum = self
                .pipelines
                .request_dfao_accum(self.descriptors.taa_set_layout());
            match (trace, blur, accum) {
                (Some(trace), Some(blur), Some(accum)) => {
                    Some(DfaoPipelines { trace, blur, accum })
                }
                _ => None,
            }
        } else {
            None
        };
        let taa_layout = self.descriptors.taa_set_layout();
        let fxaa_layout = self.descriptors.fxaa_set_layout();
        let taa = if self.aa.taa() && have_scratch {
            self.pipelines.request_taa(taa_layout)
        } else {
            None
        };
        let fxaa = if self.aa.fxaa() && have_scratch {
            self.pipelines.request_fxaa(fxaa_layout)
        } else {
            None
        };
        // Bloom composites into scene-linear `color` before the tonemap; resolve its PSO only when
        // enabled so a disabled bloom pays nothing.
        let bloom = if self.bloom_enabled {
            self.pipelines
                .request_bloom(self.descriptors.bloom_set_layout())
        } else {
            None
        };
        // The scene-resolve copy (input scratch -> display offscreen, normalized-UV upscale) runs
        // on the no-AA / MSAA paths; FXAA / TAA resolve to the offscreen themselves. Memoized, so
        // requesting it unconditionally is cheap.
        let scene_resolve = self.pipelines.request_copy_color(compute2);
        // The depth-upscale graphics pass fills the display-extent overlay depth from the
        // input-extent scene depth so the grid / gizmo occlude correctly under upsampling.
        let depth_upscale = self
            .pipelines
            .request_depth_upscale(self.descriptors.depth_upscale_layout());
        // The reactive-coverage pass (marks translucent geometry into the r8 reactive mask) arms
        // only under TAA — the mask is a TAA-resolve input. Memoized, so the request is cheap.
        let (reactive_coverage, reactive_transition) = if self.aa.taa() {
            (
                self.pipelines.request_reactive_coverage(),
                self.pipelines.request_reactive_transition(),
            )
        } else {
            (None, None)
        };

        // The final post chain: the tonemap is mandatory; the grid arms only when shown; the
        // overlay PSOs arm only when this render has submitted geometry. The overlay's per-frame
        // vertex buffer is grown + uploaded here, before the graph build, so the pass captures the
        // resolved handle.
        let tonemap = self.pipelines.request_tonemap();
        // Aerial perspective fills + composites only when authored AND the atmosphere baked LUTs.
        let ap_active = self.fog.aerial_perspective && self.scene_ibl().atmosphere_live();
        let cloud_active = self.clouds.settings().enabled;
        // Height fog is the one composite for fog, aerial perspective, and lit clouds.
        let fog = if self.view_mode != ViewMode::CloudDensity
            && (self.fog.enabled || ap_active || cloud_active)
        {
            self.pipelines.request_fog()
        } else {
            None
        };
        let cloud_weather = if cloud_active && self.clouds.weather_dirty() {
            self.pipelines
                .request_cloud_weather(self.clouds.weather_layout())
        } else {
            None
        };
        let cloud_debug = if cloud_active && self.view_mode == ViewMode::CloudDensity {
            self.pipelines
                .request_cloud_debug(self.clouds.debug_layout())
        } else {
            None
        };
        let (cloud_raymarch, cloud_reconstruct, cloud_upscale) =
            if cloud_active && self.view_mode != ViewMode::CloudDensity {
                (
                    self.pipelines
                        .request_cloud_raymarch(self.clouds.raymarch_layout()),
                    self.pipelines
                        .request_cloud_reconstruct(self.clouds.reconstruct_layout()),
                    self.pipelines
                        .request_cloud_upscale(self.clouds.upscale_layout()),
                )
            } else {
                (None, None, None)
            };
        let cloud_shadow = if cloud_active && self.clouds.settings().cast_cloud_shadows {
            self.pipelines
                .request_cloud_shadow(self.clouds.shadow_layout())
        } else {
            None
        };
        // The froxel inject/integrate PSOs arm only when volumetric fog is authored this frame.
        let (fog_inject, fog_integrate) = if self.fog.enabled && self.fog.volumetric {
            let volume_layout = self.froxel.inject_volume_layout();
            let integrate_layout = self.froxel.integrate_layout();
            (
                self.pipelines.request_fog_inject(volume_layout),
                self.pipelines.request_fog_integrate(integrate_layout),
            )
        } else {
            (None, None)
        };
        // The aerial-perspective fill PSO arms only while AP is live this frame.
        let aerial = if ap_active {
            let fill_layout = self.aerial.fill_layout();
            self.pipelines.request_aerial(fill_layout)
        } else {
            None
        };
        let grid = if self.show_grid {
            self.pipelines.request_grid()
        } else {
            None
        };
        // Draining here is what keeps the overlay to the render it was submitted for: this frame
        // draws the gizmo, the next one draws it only if the host submits it again.
        let overlay_draw = match self.overlay.take_draw(frame) {
            Ok(draw) => draw,
            Err(err) => {
                tracing::error!("overlay upload failed: {err}");
                None
            }
        };
        let (overlay, overlay_depth) = if overlay_draw.is_some() {
            (
                self.pipelines.request_overlay(),
                self.pipelines.request_overlay_depth(),
            )
        } else {
            (None, None)
        };

        // View-mode-specific post passes: the Lit Wireframe overlay and the motion-vector
        // visualization, resolved only for their active mode.
        let wireframe_overlay = if self.view_mode == ViewMode::LitWireframe {
            self.pipelines.request_wireframe_overlay()
        } else {
            None
        };
        let motion_visualize = if self.view_mode == ViewMode::MotionVectors {
            self.pipelines
                .request_motion_visualize(self.ssao.compute2_layout())
        } else {
            None
        };

        let frame_pipelines = FramePipelines {
            depth_prepass,
            cull: cull_pipeline,
            skin: skin_pipeline,
            morph: morph_pipeline,
            shadow: shadow_pipeline,
            gbuffer,
            gtao,
            ao_blur,
            contact,
            ssgi,
            ssgi_blur,
            ssgi_accum,
            gi_resolve,
            dfao,
            dfao_push,
            specocc,
            specocc_blur,
            specocc_push,
            ssr,
            copy_color,
            ddgi,
            gdf,
            restir,
            ssgi_push,
            ssr_push,
            motion,
            taa,
            fxaa,
            bloom,
            tonemap,
            fog,
            cloud_weather,
            cloud_debug,
            cloud_raymarch,
            cloud_reconstruct,
            cloud_upscale,
            cloud_shadow,
            fog_inject,
            fog_integrate,
            aerial,
            scene_resolve,
            depth_upscale,
            reactive_coverage,
            reactive_transition,
            grid,
            overlay,
            overlay_depth,
            overlay_draw,
            wireframe_overlay,
            motion_visualize,
        };

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begins recording on the freshly reset buffer.
        checked(
            unsafe { raw.begin_command_buffer(command_buffer, &begin_info) },
            "begin_command_buffer (scene)",
        )?;

        // The validation-clean gate's regression probe: when armed, record one deliberately
        // invalid command so a planted error surfaces through the debug messenger.
        plant_validation_error(&raw, command_buffer);

        // Timestamp queries are uninitialized until reset; reset this slot's pool(s) before the
        // graph writes into it — reading an unreset pool risks device loss.
        self.reset_profiler_pools(command_buffer, frame);

        // This prefix owns query-pool resets and the validation probe. Every async-compute
        // batch waits for its timeline point before writing timestamps.
        checked(
            unsafe { raw.end_command_buffer(command_buffer) },
            "end_command_buffer (scene prefix)",
        )?;

        let recorded = self.record_scene_graph(frame, frame_pipelines)?;

        checked(
            unsafe { raw.begin_command_buffer(recorded.tail, &begin_info) },
            "begin_command_buffer (scene tail)",
        )?;

        // Fold the active view's BGRA8 shm-publish readback into THIS frame's command buffer, so
        // one submit covers it with no separate submit and no synchronous wait.
        if self.shm_publish_enabled[self.active_view.index()] {
            self.record_shm_copy(recorded.tail, frame)?;
        }

        // Re-borrow the device after the `&mut self` graph build above.
        let raw = self.device.raw();
        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(recorded.tail) },
            "end_command_buffer (scene tail)",
        )?;

        // CPU span over the frame's queue submit. A no-op when the profiler is `Off`.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let submit_span = if profile_cpu {
            let CpuProfiler { registry, buffers } = &mut self.cpu_profiler;
            Some(buffers[frame].begin_span(registry, "submit-present", cpu_now_ns()))
        } else {
            None
        };
        let queue_submits = self.submit_scene_graph(frame, command_buffer, recorded)?;
        if let Some(index) = submit_span {
            let CpuProfiler { buffers, .. } = &mut self.cpu_profiler;
            buffers[frame].end_span(index, cpu_now_ns());
        }
        self.stats.command_buffers = queue_submits;
        self.stats.queue_submits = queue_submits;
        Ok(())
    }

    fn submit_scene_graph(
        &mut self,
        frame: usize,
        prefix: vk::CommandBuffer,
        recorded: RecordedSceneGraph,
    ) -> Result<u32> {
        let prefix_point = self.frames.reserve_timeline(RgQueueAssignment::Graphics)?;
        submit_graph_command(
            &self.device,
            GraphCommandSubmission {
                queue: RgQueueAssignment::Graphics,
                command_buffer: prefix,
                waits: &[],
                signals: &[prefix_point],
                binary_signal: None,
                fence: vk::Fence::null(),
                context: "queue_submit2 (scene prefix)",
            },
        )?;

        let mut batch_points = Vec::with_capacity(recorded.batches.len());
        for batch in &recorded.batches {
            let point = self.frames.reserve_timeline(batch.queue)?;
            let mut waits = Vec::new();
            if batch.queue == RgQueueAssignment::AsyncCompute {
                merge_timeline_point(&mut waits, prefix_point);
            }
            for &source_batch in &batch.wait_for_batches {
                let source = *batch_points.get(source_batch).ok_or_else(|| {
                    Error::InvalidUploadData(
                        "render-graph batch dependency does not precede its consumer".into(),
                    )
                })?;
                merge_timeline_point(&mut waits, source);
            }
            submit_graph_command(
                &self.device,
                GraphCommandSubmission {
                    queue: batch.queue,
                    command_buffer: batch.command_buffer,
                    waits: &waits,
                    signals: &[point],
                    binary_signal: None,
                    fence: vk::Fence::null(),
                    context: "queue_submit2 (render-graph batch)",
                },
            )?;
            // Recorded after the submit succeeded: a slot closed before the tail waits this point
            // so its fence cannot signal while the compute batch still reads the slot's pools.
            if batch.queue == RgQueueAssignment::AsyncCompute {
                self.pending_compute_signal = Some(point);
            }
            batch_points.push(point);
        }

        let mut tail_waits = Vec::new();
        if let Some(point) =
            recorded
                .batches
                .iter()
                .zip(&batch_points)
                .rev()
                .find_map(|(batch, point)| {
                    (batch.queue == RgQueueAssignment::AsyncCompute).then_some(*point)
                })
        {
            merge_timeline_point(&mut tail_waits, point);
        }
        let present_signal = match self.present_sync.as_ref() {
            Some(present_sync) => present_sync.scene_finished_to_signal(frame)?,
            None => None,
        };
        // The tail signals a point of its own alongside the slot fence, so every submission this
        // frame makes is one the hang watchdog can name by comparing counters.
        let tail_point = self.frames.reserve_timeline(RgQueueAssignment::Graphics)?;
        self.publish_frame_submission(frame, prefix_point, &recorded, &batch_points, tail_point);
        submit_graph_command(
            &self.device,
            GraphCommandSubmission {
                queue: RgQueueAssignment::Graphics,
                command_buffer: recorded.tail,
                waits: &tail_waits,
                signals: &[tail_point],
                binary_signal: present_signal,
                fence: self.frames.in_flight(),
                context: "queue_submit2 (scene tail)",
            },
        )?;
        // The tail owns this slot's fence signal and waits the last async-compute point, so the
        // slot needs no closing submit and the ring moves on.
        self.slot_fence_armed = false;
        self.pending_compute_signal = None;
        self.frames.advance();
        if present_signal.is_some()
            && let Some(present_sync) = self.present_sync.as_mut()
        {
            present_sync.mark_scene_finished_signaled(frame)?;
        }

        u32::try_from(recorded.batches.len().saturating_add(2)).map_err(|_| {
            Error::InvalidUploadData("render-graph submit count exceeds u32".to_owned())
        })
    }

    /// Publishes this frame's submissions — prefix, every render-graph batch, tail — in submission
    /// order, so a wedged fence wait on this slot names the batch whose point never signalled.
    fn publish_frame_submission(
        &self,
        frame: usize,
        prefix: FrameTimelinePoint,
        recorded: &RecordedSceneGraph,
        batch_points: &[FrameTimelinePoint],
        tail: FrameTimelinePoint,
    ) {
        let mut submitted = Vec::with_capacity(recorded.batches.len() + 2);
        submitted.push(crate::watchdog::SubmittedBatch {
            label: "scene-prefix",
            semaphore: prefix.semaphore,
            value: prefix.value,
        });
        submitted.extend(
            recorded
                .batches
                .iter()
                .zip(batch_points)
                .map(|(batch, point)| crate::watchdog::SubmittedBatch {
                    label: &batch.label,
                    semaphore: point.semaphore,
                    value: point.value,
                }),
        );
        submitted.push(crate::watchdog::SubmittedBatch {
            label: "scene-tail",
            semaphore: tail.semaphore,
            value: tail.value,
        });
        crate::watchdog::publish_frame(
            frame,
            self.device.raw().handle(),
            self.frame_serial,
            &submitted,
        );
    }
}
