//! The renderer-coupled half: the scene render, the shared-memory publish, and the preview
//! thumbnail render.

use saffron_assets::{
    AssetServer, GpuSceneMirror, PREVIEW_THUMBNAIL_MATERIAL_ID, PreviewRenderKind,
    RenderSceneOptions, RendererScene, RendererUploader, render_scene, write_thumbnail_cache,
};
use saffron_control::{PreviewSubject, build_preview_scene_for_thumbnail};

use saffron_rendering::{Renderer, Uploader};
use saffron_scene::{CameraView, Scene};
use saffron_sceneedit::PlayState;
use saffron_window::Window;

use crate::viewport_shm::ShmView;

use super::*;

impl HostLayer {
    /// Renders a small budget of queued material / texture preview tiles through the main forward+
    /// graph (the interactive previewer's path) on the offscreen thumbnail view, writing each to the
    /// disk cache the editor repolls for. The worker cannot drive the main graph (it lives on the
    /// render thread), so [`saffron_assets::request_thumbnail`] enqueues these and this drains them.
    /// A small per-tick budget offsets the multi-frame converge cost of each tile.
    pub(super) fn drive_preview_render_queue(&mut self, renderer: &mut Renderer) {
        /// Tiles rendered per `on_update` tick — each converges several frames, so keep it small.
        const MAX_PREVIEW_RENDERS_PER_TICK: usize = 2;

        if !self.assets.preview_render_pending() {
            return;
        }
        self.ensure_uploader(renderer);
        if self.uploader.is_none() {
            return;
        }
        let skinning = renderer.skinning_enabled();
        let jobs = self
            .assets
            .take_preview_render_jobs(MAX_PREVIEW_RENDERS_PER_TICK);
        let uploader = self.uploader.as_ref().expect("uploader present");
        let assets = &mut self.assets;
        let mirror = &mut self.gpu_scene_mirror;
        for (index, job) in jobs.iter().enumerate() {
            let subject = match job.kind {
                PreviewRenderKind::Material(id) => PreviewSubject::Material(id),
                PreviewRenderKind::TextureRole { tid, role } => {
                    PreviewSubject::TextureRole { tid, role }
                }
                PreviewRenderKind::Mesh(id) => PreviewSubject::Mesh(id),
                PreviewRenderKind::Model(id) => PreviewSubject::Model(id),
                PreviewRenderKind::Hdri(tid) => PreviewSubject::Hdri(tid),
                PreviewRenderKind::Plant(id) => PreviewSubject::Plant(id),
            };
            // Build the furnished scene (transient uploader over the renderer's descriptors), then
            // render it (the uploader borrow ends with the block, freeing the renderer).
            let (mut scene, _root, camera) = {
                let gpu = RendererUploader::new(uploader, renderer.descriptors(), skinning);
                build_preview_scene_for_thumbnail(
                    assets,
                    &gpu,
                    subject,
                    PREVIEW_THUMBNAIL_MATERIAL_ID,
                )
            };
            let view = camera.view();
            match render_preview_scene_to_png(
                renderer, uploader, mirror, &mut scene, assets, &view, job.size,
            ) {
                Ok(png) => {
                    if let Err(err) =
                        write_thumbnail_cache(std::path::Path::new(&job.cache_path), &png.bytes)
                    {
                        tracing::warn!("preview thumbnail cache write: {err}");
                    }
                }
                Err(err) => {
                    tracing::error!("preview thumbnail render: {err}");
                    // The failed render left the frame ring mid-flight; the rest of the budget
                    // would drive it further before the loop closes the slot. Release every
                    // remaining job's in-flight marker so a later request re-enqueues it, and
                    // give the tick back.
                    for abandoned in &jobs[index..] {
                        assets.finish_preview_render(&abandoned.cache_path);
                    }
                    return;
                }
            }
            assets.finish_preview_render(&job.cache_path);
        }
    }

    /// Renders the scene through the active camera and submits the native gizmo overlay: track
    /// the viewport size in present mode, sync the gizmo, render the scene, then build + submit
    /// the edit overlay geometry.
    pub(super) fn render_ui(&mut self, window: Option<&Window>, renderer: &mut Renderer) -> bool {
        // Publish mode: the editor owns the render size (set-viewport-size); the hidden
        // window's size is meaningless. Present mode tracks the window.
        if !self.shm_publish
            && let Some(window) = window
        {
            let view = renderer.active_view_id();
            let _ = renderer.set_viewport_desired_size(view, window.width(), window.height());
        }

        self.editor.sync_native_gizmo();
        let cam = self.editor.render_camera_view();
        let (view_width, view_height) = (renderer.viewport_width(), renderer.viewport_height());
        if view_width == 0 || view_height == 0 {
            return false;
        }

        let options = RenderSceneOptions {
            show_editor_camera_models: self.editor.play_state == PlayState::Edit,
            show_grid: self.editor.debug_overlays.grid && self.editor.editor_chrome_visible(),
        };

        let skinning = renderer.skinning_enabled();
        self.ensure_uploader(renderer);
        // Fold the shared wind field's frame words (authored settings + the monotonic
        // clock) before render_scene writes the light UBO.
        {
            let sources = self.editor.active_scene().local_wind_source_field();
            let wind = self.editor.active_scene().environment.wind;
            if let Err(err) = renderer.set_wind(
                &saffron_rendering::SceneWind {
                    orientation: wind.orientation,
                    speed: wind.speed,
                    gust: wind.gust,
                    turbulence_octaves: wind.turbulence_octaves,
                    turbulence_roughness: wind.turbulence_roughness,
                    gust_frequency: wind.gust_frequency,
                    reference_height: wind.reference_height,
                    height_exponent: wind.height_exponent,
                    seed: wind.seed,
                    time_s: self.editor.simulation_time_s,
                },
                &sources,
            ) {
                tracing::error!("set_wind: {err}");
            }
        }
        if !self.pending_interaction_impulses.is_empty() {
            renderer.submit_interaction_impulses(&self.pending_interaction_impulses);
            self.pending_interaction_impulses.clear();
        }
        let mut vegetation_mutated = false;
        let vegetation_cell = self.runtime.vegetation_cell();
        if let Some(uploader) = self.uploader.as_ref() {
            let world = renderer.active_view_id().gpu_scene_world();
            let vegetation = vegetation_cell.borrow();
            match self.gpu_scene_mirror.sync_renderer_world(
                world,
                self.editor.active_scene(),
                vegetation.as_ref(),
                &mut self.assets,
                renderer,
                uploader,
            ) {
                Ok(mutated) => vegetation_mutated = mutated,
                Err(error) => tracing::error!("gpu scene mirror sync: {error}"),
            }
            drop(vegetation);
            let mut driver = RendererScene::new(renderer, uploader, skinning);
            let scene: &mut Scene = self.editor.active_scene();
            render_scene(
                &mut driver,
                scene,
                &mut self.assets,
                &mut self.gpu_scene_mirror,
                &cam,
                options,
            );
        }

        self.submit_scene_edit_overlay(renderer, &cam, view_width, view_height);

        // Tell the renderer whether to fold the active view's BGRA8 shm readback into this
        // frame's command buffer, so it records the blit/copy inline — no separate submit, no
        // synchronous stall.
        if self.shm_publish {
            self.arm_active_view_shm(renderer);
        }

        // Execute the offscreen scene graph (pass 1: scene → offscreen). The editor/headless
        // host never presents a swapchain — the BGRA8 read-back into the shared-memory ring is
        // its frame transport. The copy is recorded into the frame command buffer above; this
        // submits it. A failure is logged, not fatal.
        if let Err(err) = renderer.render_scene_offscreen() {
            tracing::error!("render_scene_offscreen: {err}");
            return vegetation_mutated;
        }
        if self.shm_publish {
            self.publish_pipelined_view(renderer);
        }
        vegetation_mutated
    }

    /// Arms the renderer's per-view shm-publish flags from the host's segment wiring, so
    /// [`Renderer::render_scene_offscreen`] knows whether to fold the active view's readback
    /// into the frame command buffer.
    pub(super) fn arm_active_view_shm(&mut self, renderer: &mut Renderer) {
        for (view, shm_view) in [
            (saffron_rendering::ViewId::Scene, ShmView::Scene),
            (
                saffron_rendering::ViewId::AssetPreview,
                ShmView::AssetPreview,
            ),
        ] {
            renderer.set_shm_publish_enabled(view, self.shm.is_enabled(shm_view));
        }
    }

    /// Drains the renderer's pipelined BGRA8 bytes (a frame whose GPU work completed a few
    /// frames ago, read back stall-free at the begin-frame fence wait) and publishes them
    /// into the view's shm segment. A no-op when no completed slot is staged.
    pub(super) fn publish_pipelined_view(&mut self, renderer: &mut Renderer) {
        let Some((view_id, width, height, pixels)) = renderer.pending_shm_view() else {
            return;
        };
        let view = match view_id {
            saffron_rendering::ViewId::Scene => ShmView::Scene,
            saffron_rendering::ViewId::AssetPreview => ShmView::AssetPreview,
            // The Thumbnail view is never shm-published — its readback goes straight to a PNG, so
            // it never stages a pipelined slot here.
            saffron_rendering::ViewId::Thumbnail => return,
        };
        if !self.shm.is_enabled(view) {
            return;
        }
        self.shm.publish(view, width, height, pixels);
    }
}

/// Renders a throwaway preview `scene` through the main forward+ graph on the offscreen
/// [`saffron_rendering::ViewId::Thumbnail`] view and returns the encoded PNG — the one render
/// primitive both the async Assets-tile queue and the sync `preview-render` seam drive. Converges
/// temporal effects and waits for the preview environment's asynchronous IBL refresh and derived
/// lighting capture before readback. Restores the prior active view *without* resetting its temporal
/// state, so a `Scene → Thumbnail → Scene` excursion never wipes the live viewport's accumulated
/// history.
pub(crate) fn render_preview_scene_to_png(
    renderer: &mut Renderer,
    uploader: &Uploader,
    mirror: &mut GpuSceneMirror,
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    size: u32,
) -> saffron_rendering::Result<saffron_rendering::ThumbnailPng> {
    /// Minimum frames rendered before readback so temporal effects converge to the previewer's look.
    const MIN_CONVERGE_FRAMES: u32 = 8;
    /// Safety bound for a failed asynchronous environment refresh.
    const MAX_CONVERGE_FRAMES: u32 = 256;

    let skinning = renderer.skinning_enabled();
    let prev_view = renderer.active_view_id();
    renderer.set_active_view(saffron_rendering::ViewId::Thumbnail);
    let result = (|| {
        renderer.set_viewport_desired_size(saffron_rendering::ViewId::Thumbnail, size, size)?;
        let options = RenderSceneOptions {
            show_editor_camera_models: false,
            show_grid: false,
        };
        let mut converged = false;
        for frame in 0..MAX_CONVERGE_FRAMES {
            let world = saffron_rendering::ViewId::Thumbnail.gpu_scene_world();
            if let Err(error) =
                mirror.sync_renderer_world(world, scene, None, assets, renderer, uploader)
            {
                tracing::error!("gpu scene mirror sync (thumbnail): {error}");
            }
            {
                let mut driver = RendererScene::new(renderer, uploader, skinning);
                render_scene(&mut driver, scene, assets, mirror, camera, options);
            }
            renderer.render_scene_offscreen()?;
            if frame + 1 >= MIN_CONVERGE_FRAMES && renderer.active_environment_converged() {
                converged = true;
                break;
            }
        }
        if !converged {
            return Err(saffron_rendering::Error::ShaderLoad(
                "thumbnail environment did not converge".to_owned(),
            ));
        }
        renderer.encode_active_offscreen_png()
    })();
    renderer.restore_active_view_no_reset(prev_view);
    result
}
