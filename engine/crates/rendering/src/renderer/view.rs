use super::*;

/// The debug render-output mode. Transient — never persisted with the scene.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Full PBR shading (the default).
    #[default]
    Lit,
    /// Albedo + emissive, no lighting.
    Unlit,
    /// Wireframe (the `PolygonMode::LINE` permutation, gated on `fill_mode_non_solid`).
    Wireframe,
    /// Shaded scene with wireframe edges overlaid (an extra wireframe-overlay pass).
    LitWireframe,
    /// Full lighting on a neutral-grey material (normals preserved).
    DetailLighting,
    /// Full lighting on a flat white diffuse material (normals/specular flattened).
    LightingOnly,
    /// IBL specular reflection only (mirror-like).
    Reflections,
    /// Albedo / base color only.
    Albedo,
    /// World-space normal.
    Normal,
    Roughness,
    Metallic,
    Emissive,
    /// Linearized view-space depth as grayscale.
    Depth,
    /// Screen-space ambient occlusion factor (white when SSAO is off).
    AmbientOcclusion,
    /// Indirect/ambient lighting only (IBL diffuse + SSGI + DDGI).
    Gi,
    /// Per-cluster light count as a heatmap.
    LightComplexity,
    /// Motion vectors, colorized by a dedicated fullscreen pass.
    MotionVectors,
    /// The froxel volumetric-fog volume, drawn by the fog composite pass (volumetric mode only).
    Fog,
    /// Raw volumetric cloud density integrated by the dedicated cloud debug pass.
    CloudDensity,
    /// Virtual-shadow page visualization: the resolved (level, page) as a stable colour.
    ShadowPages,
}

impl ViewMode {
    /// The debug-shading channel the mesh fragment outputs instead of full shading, folded into
    /// the light UBO's `point_shadow_meta.w`. `0` is full shading or a mode with its own pass.
    pub(super) fn debug_channel(self) -> u32 {
        match self {
            ViewMode::Lit
            | ViewMode::Wireframe
            | ViewMode::LitWireframe
            | ViewMode::MotionVectors
            | ViewMode::Fog
            | ViewMode::CloudDensity => 0,
            ViewMode::Albedo => 1,
            ViewMode::Normal => 2,
            ViewMode::Roughness => 3,
            ViewMode::Metallic => 4,
            ViewMode::Emissive => 5,
            ViewMode::Unlit => 6,
            ViewMode::DetailLighting => 7,
            ViewMode::LightingOnly => 8,
            ViewMode::Reflections => 9,
            ViewMode::Depth => 10,
            ViewMode::AmbientOcclusion => 11,
            ViewMode::Gi => 12,
            ViewMode::LightComplexity => 13,
            ViewMode::ShadowPages => 14,
        }
    }
}

/// Which editor pane a render view targets.
///
/// The discriminant is the dense slot index into the renderer's `views` array and the host's
/// per-view shm segments, so the indices and the wire tokens are frozen end-to-end with the
/// presenter's reader ordering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ViewId {
    /// The main scene viewport (slot `0`, the default).
    #[default]
    Scene,
    /// The asset-preview viewport (slot `1`).
    AssetPreview,
    /// The offscreen thumbnail-render view (slot `2`): never shm-published, not wire-selectable,
    /// and isolated from the other views' size + temporal state. Its readback goes to a PNG.
    Thumbnail,
}

/// The number of editor render views (scene + asset-preview + offscreen thumbnail).
pub const VIEW_COUNT: usize = 3;

impl ViewId {
    /// The dense slot index into the renderer's `views` array (`Scene = 0`).
    pub fn index(self) -> usize {
        match self {
            ViewId::Scene => 0,
            ViewId::AssetPreview => 1,
            ViewId::Thumbnail => 2,
        }
    }

    /// Stable world identity reserved for this renderer-owned view.
    pub fn gpu_scene_world(self) -> crate::GpuSceneWorldId {
        crate::GpuSceneWorldId(self.index() as u64)
    }

    /// Stable temporal-view identity reserved for this renderer-owned view.
    pub fn gpu_scene_view(self) -> crate::GpuSceneViewId {
        crate::GpuSceneViewId(self.index() as u64)
    }

    /// The [`ViewId`] for a dense slot index, the inverse of [`ViewId::index`].
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => ViewId::AssetPreview,
            2 => ViewId::Thumbnail,
            _ => ViewId::Scene,
        }
    }

    /// The control-plane / shm wire token, frozen end-to-end with the presenter's reader.
    pub fn wire(self) -> &'static str {
        match self {
            ViewId::Scene => "scene",
            ViewId::AssetPreview => "assetPreview",
            ViewId::Thumbnail => "thumbnail",
        }
    }

    /// Parses a wire token into a [`ViewId`]; `None` for an unknown token. `Thumbnail` is
    /// offscreen-only and deliberately not parseable.
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "scene" => Some(ViewId::Scene),
            "assetPreview" => Some(ViewId::AssetPreview),
            _ => None,
        }
    }
}

impl Renderer {
    /// The active view's offscreen scene-color image handle + view + extent.
    pub fn active_view(&self) -> &ViewTarget {
        &self.views[self.active_view.index()]
    }

    /// Which editor pane is currently rendered/presented.
    pub fn active_view_id(&self) -> ViewId {
        self.active_view
    }

    /// A view's render targets by id, for the paths that read a non-active view's state.
    pub fn view(&self, view: ViewId) -> &ViewTarget {
        &self.views[view.index()]
    }

    /// Selects which editor pane is rendered/presented. Switching resets the newly-shown view's
    /// temporal accumulators rather than reprojecting them against another view's history.
    pub fn set_active_view(&mut self, view: ViewId) {
        if self.active_view == view {
            return;
        }
        self.active_view = view;
        self.reset_view_temporal(view, crate::GpuSceneHistoryInvalidation::NewView);
    }

    /// This frame's active-view sub-pixel jitter offset (NDC) while TAA is active, else zero.
    pub fn active_view_jitter(&self) -> saffron_geometry::glam::Vec2 {
        if self.aa.taa() {
            self.active_view().jitter
        } else {
            saffron_geometry::glam::Vec2::ZERO
        }
    }

    /// The scene view-projection with the TAA sub-pixel jitter removed. Jitter is a clip-space
    /// `clip.xy += offset·clip.w` translation, so it inverts by the opposite translation.
    pub(super) fn scene_view_proj_unjittered(&self) -> Mat4 {
        let j = self.active_view_jitter();
        Mat4::from_translation(saffron_geometry::glam::Vec3::new(-j.x, -j.y, 0.0))
            * self.frame_deformation.view_proj
    }

    /// Sets a view's desired render size and resizes its offscreen targets to match,
    /// Sets a view's desired render size and resizes its offscreen targets to match, idling the
    /// GPU first. The desired size is recorded even when the extent already matches, so
    /// [`ViewTarget::desired_width`] tracks whether the view has been sized at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the targets cannot be recreated.
    pub fn set_viewport_desired_size(
        &mut self,
        view: ViewId,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let i = view.index();
        self.views[i].desired_width = width;
        self.views[i].desired_height = height;
        self.apply_render_extent(i)
    }

    /// Sets the dynamic-resolution factor for a view and re-sizes its render targets to
    /// `round(desired * scale)`, holding the published (native) size constant — the present
    /// blit upscales. Clamped to `(0.1, 1.0]`. A no-op if unchanged or the view is unsized.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if recreating the render targets fails.
    pub fn set_render_scale(&mut self, view: ViewId, scale: f32) -> Result<()> {
        let i = view.index();
        let clamped = scale.clamp(0.1, 1.0);
        if (self.views[i].render_scale - clamped).abs() < f32::EPSILON {
            return Ok(());
        }
        self.views[i].render_scale = clamped;
        if self.views[i].desired_width == 0 || self.views[i].desired_height == 0 {
            return Ok(());
        }
        self.apply_render_extent(i)
    }

    /// The active view's dynamic-resolution factor.
    #[must_use]
    pub fn render_scale(&self, view: ViewId) -> f32 {
        self.views[view.index()].render_scale
    }

    /// The dynamic-resolution factor of the currently-active view (the one `render-stats` reports).
    #[must_use]
    pub fn active_render_scale(&self) -> f32 {
        self.views[self.active_view.index()].render_scale
    }

    /// Sets the dynamic-resolution factor of the currently-active view (the manual-override path;
    /// `auto_quality` overrides it each frame when on).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if recreating the render targets fails.
    pub fn set_active_render_scale(&mut self, scale: f32) -> Result<()> {
        let view = self.active_view;
        self.set_render_scale(view, scale)
    }

    /// Reconciles view `i`'s two extent classes: the INPUT class (`scaled_render_extent` — scene /
    /// depth / motion / G-buffer / ReSTIR) and the DISPLAY class (`published_extent` — the resolve
    /// output + TAA history + overlay depth). A desired-size change moves both, a render-scale
    /// change only the input class. A no-op when neither moved.
    fn apply_render_extent(&mut self, i: usize) -> Result<()> {
        let input = self.views[i].scaled_render_extent();
        let display = self.views[i].published_extent();
        let cur_input = self.views[i].scratch.as_ref().map(|s| s.extent);
        let cur_display = self.views[i].offscreen.extent;
        let input_changed = cur_input != Some(input);
        let display_changed = cur_display != display;
        if !input_changed && !display_changed {
            return Ok(());
        }
        // A render-scale-only change must not flush the display-extent TAA history: the resolve
        // resamples the newly-sized input into the fixed display grid every frame, so the
        // accumulator rides through.
        let scale_only = input_changed && !display_changed;
        self.device.wait_idle()?;
        self.views[i].resize(&self.device, input, display)?;
        if let Some(mut old_pyramid) = self.views[i].hzb_pyramid.take() {
            old_pyramid.free_sets(&self.descriptors);
        }
        self.views[i].hzb_pyramid =
            match crate::HzbPyramid::new(&self.device, &self.descriptors, &self.hzb, input) {
                Ok(pyramid) => Some(pyramid),
                Err(err) => {
                    tracing::error!("hzb pyramid rebuild: {err}");
                    None
                }
            };
        self.views[i].build_screen_space(&self.device, &self.descriptors, &self.ssao)?;
        if scale_only {
            self.views[i].build_aa_targets_preserving_temporal(
                &self.device,
                &self.descriptors,
                self.aa,
            )?;
        } else {
            self.views[i].build_aa_targets(&self.device, &self.descriptors, self.aa)?;
        }
        // The ReSTIR reservoirs + radiance are INPUT-extent; the rebuild arms a temporal reset.
        self.views[i].restir.reset_history();
        self.views[i]
            .restir
            .build(&self.device, &self.descriptors, &self.restir, input)?;
        // The shm capture ring is display-extent, resized here under this function's idle wait.
        if self.shm_publish_enabled[i] {
            self.views[i].size_shm_capture(&self.device, display)?;
        }
        self.clouds.bind_view(
            i,
            crate::clouds::CloudViewBindings {
                color: self.views[i].offscreen.view(),
                depth: self.views[i].depth.view(),
                motion: self.views[i]
                    .motion
                    .as_ref()
                    .expect("cloud motion built")
                    .view(),
                reduced: [
                    self.views[i].cloud_reduced[0]
                        .as_ref()
                        .expect("cloud reduced 0 built")
                        .view(),
                    self.views[i].cloud_reduced[1]
                        .as_ref()
                        .expect("cloud reduced 1 built")
                        .view(),
                ],
                reduced_depth: self.views[i]
                    .cloud_reduced_depth
                    .as_ref()
                    .expect("cloud reduced depth built")
                    .view(),
                full_color: self.views[i]
                    .cloud_full_color
                    .as_ref()
                    .expect("cloud full color built")
                    .view(),
                full_depth: self.views[i]
                    .cloud_full_depth
                    .as_ref()
                    .expect("cloud full depth built")
                    .view(),
            },
        );
        Ok(())
    }

    /// A view's last-requested render width in device pixels (`0` until the view is sized).
    pub fn view_desired_width(&self, view: ViewId) -> u32 {
        self.views[view.index()].desired_width
    }

    /// A view's last-requested render height in device pixels.
    pub fn view_desired_height(&self, view: ViewId) -> u32 {
        self.views[view.index()].desired_height
    }

    /// The active view's INPUT (scene render) width in device pixels.
    pub fn viewport_width(&self) -> u32 {
        self.views[self.active_view.index()]
            .scaled_render_extent()
            .width
    }

    /// The active view's INPUT (scene render) height in device pixels.
    pub fn viewport_height(&self) -> u32 {
        self.views[self.active_view.index()]
            .scaled_render_extent()
            .height
    }

    /// Resets a view's temporal state: motion reprojection, SSGI/TAA history, ReSTIR reservoirs,
    /// and the scene-global DDGI probes.
    pub fn reset_view_temporal(
        &mut self,
        view: ViewId,
        reason: crate::GpuSceneHistoryInvalidation,
    ) {
        let target = &mut self.views[view.index()];
        target.prev_view_proj_valid = false;
        target.history_valid = false;
        target.restir.reset_history();
        self.ddgi.reset_history();
        // The persistent scene keeps its own per-view history generation, and it is what the
        // visibility pass consults: both must move together, or a view whose reprojection was
        // blanked still advertises a valid history to the GPU.
        if let Ok(state) = self
            .persistent_gpu_scene
            .view_mut(view.gpu_scene_view())
            .map_err(|err| tracing::warn!("view history invalidation: {err}"))
            && let Err(err) = state.invalidate(reason)
        {
            tracing::warn!("view history invalidation: {err}");
        }
    }

    /// Restores the active view without resetting its temporal state, unlike
    /// [`Renderer::set_active_view`]: a background thumbnail render makes a brief
    /// `Scene → Thumbnail → Scene` excursion each drained frame and must not wipe the Scene
    /// view's accumulated history.
    pub fn restore_active_view_no_reset(&mut self, view: ViewId) {
        self.active_view = view;
    }

    /// Why the active view's temporal history was last invalidated.
    #[must_use]
    pub fn view_history_invalidation(&self) -> &'static str {
        self.persistent_gpu_scene
            .view(self.active_view.gpu_scene_view())
            .map_or("unknown", |state| state.invalidation.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view-id wire tokens + dense slot indices are FROZEN end-to-end with the
    /// presenter's reader and the host's per-view shm segments (`Scene = 0`).
    #[test]
    fn view_id_wire_tokens_and_indices_are_frozen() {
        assert_eq!(ViewId::default(), ViewId::Scene);
        assert_eq!(ViewId::Scene.index(), 0);
        assert_eq!(ViewId::AssetPreview.index(), 1);
        assert_eq!(ViewId::Thumbnail.index(), 2);
        assert_eq!(ViewId::Scene.wire(), "scene");
        assert_eq!(ViewId::AssetPreview.wire(), "assetPreview");
        assert_eq!(ViewId::Thumbnail.wire(), "thumbnail");
        assert_eq!(ViewId::from_wire("scene"), Some(ViewId::Scene));
        assert_eq!(
            ViewId::from_wire("assetPreview"),
            Some(ViewId::AssetPreview)
        );
        assert_eq!(ViewId::from_wire("nope"), None);
        assert_eq!(ViewId::from_wire("thumbnail"), None);
        for view in [ViewId::Scene, ViewId::AssetPreview] {
            assert_eq!(ViewId::from_wire(view.wire()), Some(view));
        }
        for view in [ViewId::Scene, ViewId::AssetPreview, ViewId::Thumbnail] {
            assert_eq!(ViewId::from_index(view.index()), view);
        }
        assert_eq!(VIEW_COUNT, 3);
    }
}
