//! The renderer-coupled half: the scene render, the shared-memory publish, and the preview
//! thumbnail job.

use saffron_assets::{
    PREVIEW_THUMBNAIL_MATERIAL_ID, PreviewRenderJob, PreviewRenderKind, RenderSceneOptions,
    RendererScene, RendererUploader, render_scene, write_thumbnail_cache,
};
use saffron_control::{PreviewSubject, build_preview_scene_for_thumbnail};

use saffron_rendering::{Renderer, ThumbnailPng, ViewId};
use saffron_scene::{CameraView, Scene};
use saffron_sceneedit::PlayState;

use crate::viewport_shm::ShmView;

use super::*;

/// Frames a preview tile renders before its read-back, so the temporal effects settle to the
/// interactive previewer's look.
const MIN_CONVERGE_FRAMES: u32 = 8;
/// Safety bound for a preview environment whose asynchronous refresh never completes.
const MAX_CONVERGE_FRAMES: u32 = 256;

/// A preview tile converging on the offscreen [`ViewId::Thumbnail`] view, one frame per tick.
///
/// The subject scene is furnished once when the job starts and re-rendered each tick, so the
/// thumbnail view accumulates its temporal history across ticks exactly as a viewport does.
pub(super) struct PreviewRenderState {
    /// What to render and where its PNG is cached.
    job: PreviewRenderJob,
    /// The furnished subject scene.
    scene: Scene,
    /// The framed preview camera.
    camera: CameraView,
    /// Converge frames rendered so far.
    frames: u32,
}

/// The preview subject a queued job renders.
fn preview_subject(kind: PreviewRenderKind) -> PreviewSubject {
    match kind {
        PreviewRenderKind::Material(id) => PreviewSubject::Material(id),
        PreviewRenderKind::TextureRole { tid, role } => PreviewSubject::TextureRole { tid, role },
        PreviewRenderKind::Mesh(id) => PreviewSubject::Mesh(id),
        PreviewRenderKind::Model(id) => PreviewSubject::Model(id),
        PreviewRenderKind::Hdri(tid) => PreviewSubject::Hdri(tid),
        PreviewRenderKind::Plant(id) => PreviewSubject::Plant(id),
    }
}

impl HostLayer {
    /// Advances the queued material / texture / mesh preview tiles by **one** converge frame per
    /// tick, through the main forward+ graph (the interactive previewer's path) on the offscreen
    /// thumbnail view, writing each finished tile to the disk cache the editor repolls for. The
    /// worker cannot drive the main graph (it lives on the render thread), so
    /// [`saffron_assets::request_thumbnail`] enqueues these and this drains them.
    ///
    /// One tile is in flight and one frame is rendered per tick, so a tile costs the frame loop the
    /// same as an ordinary viewport rather than a burst of frames inside a single update.
    pub(super) fn drive_preview_render_queue(&mut self, renderer: &mut Renderer) {
        // A project load swaps the scene and clears the asset caches across frames, so a tile
        // converging against the outgoing catalog would render into a torn scene. Abandon it; the
        // editor's next poll re-enqueues against the loaded project.
        if self.editor.project_phase == ProjectPhase::Loading {
            if let Some(state) = self.preview_job.take() {
                self.assets.finish_preview_render(&state.job.cache_path);
            }
            return;
        }
        if self.preview_job.is_some() {
            self.advance_preview_job(renderer);
        } else {
            self.start_preview_job(renderer);
        }
    }

    /// Whether a preview tile is queued or converging — a render-activity reason, so the reactive
    /// loop holds full cadence until every tile has landed.
    pub(super) fn preview_render_active(&self) -> bool {
        self.preview_job.is_some() || self.assets.preview_render_pending()
    }

    /// Takes the next queued tile, furnishes its subject scene, and requests the thumbnail view's
    /// size. The size lands at the next frame boundary, so the first converge frame runs on the
    /// tick after this one.
    fn start_preview_job(&mut self, renderer: &mut Renderer) {
        if !self.assets.preview_render_pending() {
            return;
        }
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return;
        };
        let Some(job) = self.assets.take_preview_render_job() else {
            return;
        };
        let skinning = renderer.skinning_enabled();
        // The transient uploader borrows the renderer's descriptors; the borrow ends with the
        // block, freeing the renderer for the converge frames.
        let (scene, _root, camera) = {
            let gpu = RendererUploader::new(uploader, renderer.descriptors(), skinning);
            build_preview_scene_for_thumbnail(
                &mut self.assets,
                &gpu,
                preview_subject(job.kind),
                PREVIEW_THUMBNAIL_MATERIAL_ID,
            )
        };
        renderer.set_viewport_desired_size(ViewId::Thumbnail, job.size, job.size);
        self.preview_job = Some(PreviewRenderState {
            job,
            scene,
            camera: camera.view(),
            frames: 0,
        });
    }

    /// Renders one converge frame of the active tile and, once the preview environment's
    /// asynchronous IBL refresh and derived lighting capture have landed, reads it back to a PNG
    /// and writes the disk cache.
    fn advance_preview_job(&mut self, renderer: &mut Renderer) {
        let outcome = {
            let Some(state) = self.preview_job.as_mut() else {
                return;
            };
            let Some(uploader) = self.uploader.as_ref() else {
                return;
            };
            let assets = &mut self.assets;
            let mirror = &mut self.gpu_scene_mirror;
            let skinning = renderer.skinning_enabled();
            let prev_view = renderer.active_view_id();
            if state.frames == 0 {
                // A fresh subject starts from a clean history rather than the last tile's.
                renderer.set_active_view(ViewId::Thumbnail);
            } else {
                renderer.set_active_view_no_reset(ViewId::Thumbnail);
            }
            let result = (|| {
                let world = ViewId::Thumbnail.gpu_scene_world();
                if let Err(error) = mirror.sync_renderer_world(
                    world,
                    &mut state.scene,
                    None,
                    assets,
                    renderer,
                    uploader,
                ) {
                    tracing::error!("gpu scene mirror sync (thumbnail): {error}");
                }
                {
                    let mut driver = RendererScene::new(renderer, uploader, skinning);
                    render_scene(
                        &mut driver,
                        &mut state.scene,
                        assets,
                        &mut *mirror,
                        &state.camera,
                        RenderSceneOptions {
                            show_editor_camera_models: false,
                            show_grid: false,
                        },
                    );
                }
                renderer.render_scene_offscreen()?;
                state.frames += 1;
                if state.frames >= MIN_CONVERGE_FRAMES && renderer.active_environment_converged() {
                    return renderer.encode_active_offscreen_png().map(Some);
                }
                if state.frames >= MAX_CONVERGE_FRAMES {
                    return Err(saffron_rendering::Error::NotConverged {
                        frames: MAX_CONVERGE_FRAMES,
                    });
                }
                Ok(None)
            })();
            // Leave the excursion without resetting the viewport's accumulated history.
            renderer.set_active_view_no_reset(prev_view);
            result
        };

        let finished: Option<ThumbnailPng> = match outcome {
            Ok(None) => return,
            Ok(Some(png)) => Some(png),
            Err(err) => {
                tracing::error!("preview thumbnail render: {err}");
                None
            }
        };
        let Some(state) = self.preview_job.take() else {
            return;
        };
        if let Some(png) = finished
            && let Err(err) =
                write_thumbnail_cache(std::path::Path::new(&state.job.cache_path), &png.bytes)
        {
            tracing::warn!("preview thumbnail cache write: {err}");
        }
        // Release the in-flight marker either way, so a later request re-enqueues the tile.
        self.assets.finish_preview_render(&state.job.cache_path);
    }

    /// Renders the scene through the active camera and submits the native gizmo overlay: sync the
    /// gizmo, render the scene, then build + submit the edit overlay geometry.
    pub(super) fn render_ui(&mut self, renderer: &mut Renderer) -> bool {
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
            let t = cpu_now_ns();
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
            mark(renderer, "gpu-scene-mirror-sync", t);
            let t = cpu_now_ns();
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
            mark(renderer, "render-scene-gather", t);
        }

        let t = cpu_now_ns();
        self.submit_scene_edit_overlay(renderer, &cam, view_width, view_height);
        mark(renderer, "scene-edit-overlay", t);

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
            let t = cpu_now_ns();
            self.publish_pipelined_view(renderer);
            mark(renderer, "publish-view", t);
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
