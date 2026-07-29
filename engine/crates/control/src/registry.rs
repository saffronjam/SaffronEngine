//! The command registry, the `EngineContext` live-state seam, dispatch, and the
//! two builtin commands (`ping`, `help`).
//!
//! A [`CommandRegistry`] is a `Vec<Command>` (insertion order preserved, so `help`
//! and the generated manifest iterate it in registration order) plus a
//! `HashMap<&'static str, usize>` index. The typed [`CommandRegistry::register`]
//! is the single place the wire encoding (serde-driven, decimal-string ids) is
//! applied, so every later handler inherits it.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use std::path::Path;

use saffron_assets::{AssetServer, GpuUploader};
use saffron_physics::World;
use saffron_protocol::{PingParams, PingResult};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, CaptureMode, CaptureState, FrameHistoryStats, FrameSample, GpuLut,
    GpuTexture, PassTiming, PerfConfig, ProfileCapture, ProfilerMode, ReflectionProbe,
    RenderStatsFull, ViewId, ViewMode,
};
use saffron_sceneedit::SceneEditContext;
use saffron_spatial::ResidencyManager;
use saffron_window::Window;

use crate::error::{Error, Result};

/// A graph-compute backend that has qualified its active hardware profile.
pub type VegetationComputeExecutor = Arc<dyn saffron_vegetation::GraphComputeExecutor>;

/// The renderer seam every render-, scene-, and asset-domain command reaches
/// through.
///
/// The concrete `Renderer` cannot be built headless (its swapchain WSI has no
/// offscreen backing on lavapipe), so the borrow is taken behind this object-safe
/// trait. The live implementation is the host's `HostControlRenderer`, which
/// bundles the renderer with the host-owned one-off `Uploader` (the renderer owns
/// none) so the GPU-upload / scene-render seam below can be handed to the asset
/// loaders; a unit-test stub implements it over plain in-memory state. Growing
/// this trait is the only renderer coupling the control plane has.
///
/// Beyond the render-domain query/toggle methods, three asset/scene-domain seams
/// live here: **view-select** ([`ControlRenderer::set_active_view`] + the
/// desired-size pair), **screenshot** ([`ControlRenderer::capture_viewport`]), and
/// the **GPU-upload** access point ([`ControlRenderer::with_gpu_uploader`]) that
/// hands a transient [`GpuUploader`] to the asset loaders (`import_texture`,
/// `load_mesh_asset`, `ensure_preview_floor_mesh`, `resolve_material_asset`,
/// `pick_entity`, …) the asset/scene handlers drive.
/// One plant's captured wind prepass record joined with the response that produced it.
pub struct PlantWindRecord {
    /// The GPU-scene instance slot the plant mirrors to.
    pub slot: u32,
    /// The record the prepass wrote, which every raster pass then applies.
    pub record: saffron_rendering::GpuWindInstanceRecord,
    /// The family's authored response in its cooked integer form; all zero when the
    /// prototype carries none.
    pub mechanics: [u32; 4],
}

pub trait ControlRenderer {
    /// The full per-frame draw + timing + telemetry snapshot.
    fn render_stats(&self) -> RenderStatsFull;

    /// Population and rebuild counters of the persistent GPU-scene mirror.
    fn gpu_scene_mirror_stats(&self) -> saffron_assets::GpuSceneMirrorStats;
    fn page_residency_stats(&self) -> saffron_rendering::PageResidencyStats;
    fn visibility_counters(
        &self,
    ) -> [u32; saffron_rendering::SCENE_VISIBILITY_COUNTER_WORDS as usize];
    /// The same, for the global-illumination reach view.
    fn gi_visibility_counters(
        &self,
    ) -> [u32; saffron_rendering::SCENE_VISIBILITY_COUNTER_WORDS as usize];
    /// Deformed instances the interaction field's re-centring scroll has reset since boot.
    fn wind_interaction_resets(&self) -> u64;
    /// Per-family and per-cell vegetation render population.
    fn vegetation_breakdown(&self) -> saffron_assets::VegetationRenderBreakdown;
    /// The resident-population budgets whose breaches raise owner-named alarms.
    fn vegetation_budgets(&self) -> saffron_assets::VegetationBudgets;
    /// Replaces those budgets.
    fn set_vegetation_budgets(&mut self, budgets: saffron_assets::VegetationBudgets);
    /// One resident plant's wind prepass record, captured from the GPU on demand, beside
    /// the authored family response the prepass was given. `None` when the plant is not
    /// mirrored or no frame has created the record buffer yet.
    fn capture_plant_wind_record(
        &self,
        cell: saffron_spatial::WorldCellKey,
        plant: saffron_vegetation::PlantId,
    ) -> std::result::Result<Option<PlantWindRecord>, String>;
    /// GPU missing-page requests drained since startup.
    fn page_faults(&self) -> u64;

    /// Whether clustered-forward light culling is on.
    fn clustered_enabled(&self) -> bool;
    /// Toggles clustered-forward culling.
    fn set_clustered(&mut self, enabled: bool);
    /// Stages a world interaction impulse for the next frame's field step.
    fn submit_interaction_impulse(&mut self, impulse: saffron_rendering::InteractionImpulse);
    /// Whether the depth pre-pass is on.
    fn depth_prepass_enabled(&self) -> bool;
    /// Toggles the depth pre-pass.
    fn set_depth_prepass(&mut self, enabled: bool);
    /// Whether the directional shadow map is on.
    fn shadows_enabled(&self) -> bool;
    /// Toggles the directional shadow map.
    fn set_shadows(&mut self, enabled: bool);
    /// Whether image-based ambient lighting is on.
    fn ibl_enabled(&self) -> bool;
    /// Toggles IBL ambient.
    fn set_ibl(&mut self, enabled: bool);
    /// Whether GTAO screen-space ambient occlusion is on (per the active quality tier).
    fn ssao_enabled(&self) -> bool;
    /// Whether screen-space contact shadows are on (per the active quality tier).
    fn contact_shadows_enabled(&self) -> bool;
    /// Whether screen-space one-bounce GI is on (per the active quality tier).
    fn ssgi_enabled(&self) -> bool;
    /// The active render-quality tier name (`low`/`medium`/`high`/`ultra`/`custom`) — the single
    /// knob for the SSGI / GTAO / contact-shadow stack.
    fn render_quality_tier(&self) -> String;
    /// Applies a render-quality tier by name; returns `false` for an unknown name (no change).
    fn set_render_quality(&mut self, tier: &str) -> bool;
    /// The active view's dynamic-resolution factor (`(0, 1]`; `1.0` = native).
    fn render_scale(&self) -> f32;
    /// Sets the active view's dynamic-resolution factor (clamped to `(0, 1]`). A manual override;
    /// when `auto_quality` is on the budget controller resets it each frame.
    fn set_render_scale(&mut self, scale: f32);
    /// The active view's current input (scene-render) extent `(width, height)` in device pixels —
    /// `round(display × render_scale)`, what the TAAU resolve reconstructs from.
    fn input_extent(&self) -> (u32, u32);
    /// The active view's fixed display (present) extent `(width, height)` in device pixels — what
    /// the resolve reconstructs to, independent of the render scale.
    fn display_extent(&self) -> (u32, u32);
    /// The active tonemap operator name (`reinhard`/`aces`/`agx`/`pbr-neutral`).
    fn tonemap_mode(&self) -> String;
    /// Selects the tonemap operator by name; returns `false` for an unknown name.
    fn set_tonemap(&mut self, mode: &str) -> bool;

    /// Whether the reactive loop is idling (skipping renders) per the host's last snapshot.
    fn reactive_idle(&self) -> bool;
    /// Whether the temporal effects have converged per the host's last snapshot.
    fn reactive_converged(&self) -> bool;
    /// The reasons continuous render is currently held (empty when idle).
    fn redraw_reasons(&self) -> Vec<String>;
    /// The editor viewport power state name (`focused`/`unfocused`/`occluded`).
    fn power_state(&self) -> String;
    /// Sets the editor viewport power state by name; returns `false` for an unknown name.
    fn set_viewport_power_state(&mut self, state: &str) -> bool;
    /// Whether DDGI multi-bounce GI is on.
    fn ddgi_enabled(&self) -> bool;
    /// Toggles DDGI.
    fn set_ddgi(&mut self, enabled: bool);
    /// Whether SDF distance-field AO is occluding the analytic IBL.
    fn sky_occlusion_enabled(&self) -> bool;
    /// Toggles SDF distance-field AO occlusion of the analytic IBL.
    fn set_sky_occlusion(&mut self, enabled: bool);
    /// Whether the Global Distance Field (the camera-centered cascade clipmap) is on.
    fn gdf_enabled(&self) -> bool;
    /// Toggles the Global Distance Field — the far-field cone-march tap.
    fn set_gdf(&mut self, enabled: bool);
    /// Whether reflection probes contribute.
    fn reflection_probes_enabled(&self) -> bool;
    /// Toggles reflection probes.
    fn set_reflection_probes(&mut self, enabled: bool);
    /// The captured reflection probes in slot order (the `list-probes` source).
    fn reflection_probes(&self) -> Vec<ReflectionProbe>;
    /// Whether the GPU skinning path is on.
    fn skinning_enabled(&self) -> bool;
    /// Toggles GPU skinning.
    fn set_skinning(&mut self, enabled: bool);
    /// Whether the GPU displacement path is on.
    fn displacement_enabled(&self) -> bool;
    /// Toggles GPU displacement.
    fn set_displacement(&mut self, enabled: bool);
    /// Sets the displacement-tessellation budget (dice cap / minimum factor / target edge length in
    /// pixels); `None` leaves a field unchanged, and the values are clamped to a sane range (cap ≤ 2048, integer).
    fn set_tessellation_quality(
        &mut self,
        factor_cap: Option<f32>,
        min_factor: Option<f32>,
        edge_length_target: Option<f32>,
    );
    /// The current tessellation-quality budget `(factor_cap, min_factor, edge_length_target)`.
    fn tessellation_quality(&self) -> (f32, f32, f32);

    /// Whether the device supports hardware ray tracing.
    fn rt_supported(&self) -> bool;
    /// Whether ray-query shadows ran this frame.
    fn rt_shadows_enabled(&self) -> bool;
    /// Toggles ray-query shadows (the caller gates on [`ControlRenderer::rt_supported`]).
    fn set_rt_shadows(&mut self, enabled: bool);
    /// Whether ReSTIR direct lighting is on.
    fn restir_enabled(&self) -> bool;
    /// Toggles ReSTIR (the caller gates on [`ControlRenderer::rt_supported`]).
    fn set_restir(&mut self, enabled: bool);
    /// Whether screen-space reflections are on.
    fn ssr_enabled(&self) -> bool;
    /// Toggles screen-space reflections.
    fn set_ssr(&mut self, enabled: bool);
    /// Whether ray-traced reflections are on.
    fn rt_reflections_enabled(&self) -> bool;
    /// Toggles ray-traced reflections (the caller gates on [`ControlRenderer::rt_supported`]).
    fn set_rt_reflections(&mut self, enabled: bool);
    /// Whether a mesh stage is available on this device.
    fn mesh_shader_supported(&self) -> bool;
    /// Whether the shaded executor is running through the mesh stage this frame.
    fn mesh_executor_active(&self) -> bool;
    /// Occluders dropped from this frame's SDF list for want of capacity.
    fn sdf_instances_dropped(&self) -> u32;
    /// Occluders the cascade-window gate excluded this frame.
    fn sdf_instances_culled(&self) -> u32;
    /// Ray instances excluded by the reachable-GI gate.
    fn rt_instances_culled(&self) -> u32;
    /// Whether opacity micromaps can be attached to triangle geometry on this device.
    fn rt_omm_supported(&self) -> bool;
    /// The built static-mesh BLAS count.
    fn rt_blas_count(&self) -> u32;
    /// Skinned refit structures active this frame.
    fn rt_skinned_blas_count(&self) -> u32;
    /// Tessellated full-rebuild structures active this frame.
    fn rt_tessellated_blas_count(&self) -> u32;
    /// Whether cluster acceleration structures are enabled on this device.
    fn cluster_as_supported(&self) -> bool;
    /// Distinct cluster-composed bottom-level structures referenced this frame.
    fn rt_cluster_blas_count(&self) -> u32;
    /// Cluster acceleration structures those bottom levels compose.
    fn rt_clas_count(&self) -> u32;
    /// Whether the top-level structure is partitioned on this device.
    fn ptlas_supported(&self) -> bool;
    /// The partitioned structure's last build as `(partitions, writes, updates)`.
    fn rt_ptlas_ops(&self) -> (u32, u32, u32);
    /// Why the active view's temporal history was last invalidated.
    fn view_history_invalidation(&self) -> &'static str;
    /// Shadow pages a frame may render.
    fn vsm_page_budget(&self) -> u32;
    /// Sets the per-frame shadow-page render budget.
    fn set_vsm_page_budget(&mut self, pages: u32);
    /// Missing-page requests one view class may raise per frame.
    fn page_request_budget(&self) -> u32;
    /// Sets that budget.
    fn set_page_request_budget(&mut self, entries: u32);
    /// Cumulative GPU microseconds in out-of-graph structure builds and compactions.
    fn rt_accel_build_us(&self) -> u64;
    /// Distinct opacity micromaps this frame's structures reference.
    /// `view`'s pinned hierarchy cut, or `SCENE_CUT_AUTO`.
    fn cut_override(&self, view: saffron_rendering::SceneViewClass) -> u32;
    /// Pins the cut `view` draws.
    fn set_cut_override(&mut self, view: saffron_rendering::SceneViewClass, cut: u32);
    fn rt_omm_micromaps(&self) -> u32;
    /// Micro-triangles settled opaque, settled transparent, and left unknown.
    fn rt_omm_classes(&self) -> (u64, u64, u64);
    /// AS-storage bytes the distinct bottom-level structures occupy.
    fn rt_blas_bytes(&self) -> u64;
    /// What those structures would occupy uncompacted.
    fn rt_blas_built_bytes(&self) -> u64;
    /// AS-storage bytes this frame's top-level structure occupies.
    fn rt_tlas_bytes(&self) -> u64;
    /// Build-scratch bytes held for this frame's structure builds.
    fn rt_scratch_bytes(&self) -> u64;

    /// The cached PSO count.
    fn pipeline_count(&self) -> u32;
    /// The high-water bindless texture-slot count.
    fn bindless_texture_count(&self) -> u32;
    /// The reclaimed-and-free bindless slot count.
    fn bindless_free_count(&self) -> u32;

    /// The current debug render-output mode.
    fn view_mode(&self) -> ViewMode;
    /// Selects the debug render-output mode.
    fn set_view_mode(&mut self, mode: ViewMode);

    /// The current AA mode name (`off` / `fxaa` / `taa` / `msaaN`).
    fn aa_mode(&self) -> String;
    /// Applies an AA selection by (samples, fxaa, taa); idles + recreates targets.
    ///
    /// # Errors
    ///
    /// Returns the device error message if the GPU cannot idle or the AA targets
    /// cannot be recreated.
    fn set_aa(&mut self, samples: u32, fxaa: bool, taa: bool) -> std::result::Result<(), String>;

    /// The current TAA blend/sharpen parameters. Uses `TaaParamsDto` as the currency so the
    /// trait keeps no dependency on `saffron-rendering` (as `aa_mode` returns `String`, not `Aa`).
    fn taa_params(&self) -> saffron_protocol::TaaParamsDto;
    /// Applies fully-resolved TAA parameters (no idle — it is a per-frame push value).
    fn set_taa_params(&mut self, params: saffron_protocol::TaaParamsDto);

    /// The tonemap exposure in stops.
    fn exposure_ev(&self) -> f32;
    /// Sets the tonemap exposure in stops.
    fn set_exposure(&mut self, ev: f32);

    /// Sets the scene-linear color grade (white balance, contrast, saturation, ASC-CDL) folded into
    /// the tonemap pass. Carries the wire DTO directly; the host maps it to the renderer's grade type.
    fn set_color_grading(&mut self, params: saffron_protocol::SetColorGradingParams);
    /// The current scene-linear color grade.
    fn color_grading(&self) -> saffron_protocol::SetColorGradingParams;

    /// Sets the display-space creative look-up table the tonemap pass samples after the view transform
    /// (`id`/`lut` together; `id == 0` + `None` clears it to the identity default). The caller resolves
    /// the asset id to a live [`GpuLut`]; `intensity` (in `[0, 1]`) rides the grade UBO.
    fn set_creative_lut_texture(&mut self, id: u64, lut: Option<Arc<GpuLut>>, intensity: f32);
    /// The resolved creative-LUT read-back: `(asset id, size, intensity)`, or `None` when none.
    fn creative_lut(&self) -> Option<(u64, u32, f32)>;
    /// Bakes the current grade + view transform + creative LUT into a `33³` display-referred table on
    /// the GPU, returning `(size, red-fastest [r, g, b] f16 bits)` the caller serializes into a `.slut`.
    fn bake_look_lut(&mut self) -> std::result::Result<(u32, Vec<[u16; 3]>), String>;

    /// Sets the pre-tonemap bloom parameters (primitive-typed so the trait keeps no dependency on
    /// rendering internals): the enable flag, the composite `intensity`, the tent-upsample
    /// `scatter` radius, the `tint`, and the soft-knee `threshold`.
    fn set_bloom(
        &mut self,
        enabled: bool,
        intensity: f32,
        scatter: f32,
        tint: [f32; 3],
        threshold: f32,
    );
    /// Whether the scene-linear bloom pyramid runs before the tonemap.
    fn bloom_enabled(&self) -> bool;
    /// The energy-conserving bloom composite weight.
    fn bloom_intensity(&self) -> f32;
    /// The bloom tent-upsample scatter radius (UV units).
    fn bloom_scatter(&self) -> f32;
    /// The bloom tint (multiplies the composited bloom).
    fn bloom_tint(&self) -> [f32; 3];
    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    fn bloom_threshold(&self) -> f32;

    /// Sets the lens-dirt mask texture the bloom composite multiplies the pyramid by (`id`/`texture`
    /// together; `id == 0` + `None` clears it). The caller resolves the asset id to a live GPU
    /// texture; an absent texture binds the renderer's 1×1 white fallback (mask = 1 ⇒ identity).
    fn set_bloom_dirt_texture(&mut self, id: u64, texture: Option<Arc<GpuTexture>>);
    /// Sets the lens-dirt mix (`0.0` = no dirt) and its tint.
    fn set_bloom_dirt_params(&mut self, intensity: f32, tint: [f32; 3]);
    /// The lens-dirt mask asset id (`0` = none).
    fn bloom_dirt_texture(&self) -> u64;
    /// The lens-dirt mix fraction.
    fn bloom_dirt_intensity(&self) -> f32;
    /// The lens-dirt tint.
    fn bloom_dirt_tint(&self) -> [f32; 3];
    /// Sets the anamorphic streak: the `enabled` toggle, the horizontal `ratio` squeeze, the streak
    /// `tint`, and the `intensity` add weight.
    fn set_bloom_anamorphic(&mut self, enabled: bool, ratio: f32, tint: [f32; 3], intensity: f32);
    /// Whether the anamorphic streak runs.
    fn bloom_anamorphic_enabled(&self) -> bool;
    /// The anamorphic horizontal squeeze.
    fn bloom_anamorphic_ratio(&self) -> f32;
    /// The anamorphic streak tint.
    fn bloom_anamorphic_tint(&self) -> [f32; 3];
    /// The anamorphic streak add weight.
    fn bloom_anamorphic_intensity(&self) -> f32;
    /// Sets the per-upsample-step tint stack (empty disables per-mip tinting).
    fn set_bloom_mip_tint(&mut self, tint: Vec<[f32; 3]>);
    /// The per-upsample-step tint stack.
    fn bloom_mip_tint(&self) -> Vec<[f32; 3]>;

    /// The current GPU profiler mode.
    fn profiler_mode(&self) -> ProfilerMode;
    /// Selects the GPU profiler mode.
    fn set_profiler_mode(&mut self, mode: ProfilerMode);
    /// Whether timestamp queries are supported on the graphics queue.
    fn profiler_timestamps_supported(&self) -> bool;
    /// Whether pipeline-statistics queries are supported.
    fn profiler_pipeline_stats_supported(&self) -> bool;
    /// The last frame's per-pass GPU timings.
    fn pass_timings(&self) -> Vec<PassTiming>;
    /// The last frame's total GPU span (ms).
    fn pass_timings_total_ms(&self) -> f32;

    /// Arms a profiler capture, returning its id.
    fn start_profile_capture(
        &mut self,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32;
    /// Finishes the armed capture and returns the spans + metadata.
    fn stop_profile_capture(&mut self) -> ProfileCapture;
    /// The capture's mode.
    fn profile_capture_mode(&self) -> CaptureMode;
    /// The capture state machine's current state.
    fn profile_capture_state(&self) -> CaptureState;
    /// Frames copied into the in-flight capture so far.
    fn profile_capture_captured_frames(&self) -> u32;
    /// The in-flight capture's target frame count.
    fn profile_capture_target_frames(&self) -> u32;

    /// The rolling frame-time percentile / stutter summary.
    fn frame_history_stats(&self) -> FrameHistoryStats;
    /// The most recent `max_samples` frame samples, oldest→newest.
    fn frame_samples(&self, max_samples: u32) -> Vec<FrameSample>;
    /// The shared frame-budget / threshold config.
    fn perf_config(&self) -> PerfConfig;
    /// Replaces the perf config (clamped into sane ranges).
    fn set_perf_config(&mut self, config: PerfConfig);

    /// Drains perf-alarm events with `seq > since`.
    fn drain_alarms(&self, since: u64) -> AlarmDrain;
    /// The currently-firing perf alarms.
    fn active_alarms(&self) -> Vec<ActiveAlarm>;

    /// The active view's offscreen render width (device pixels).
    fn viewport_width(&self) -> u32;
    /// The active view's offscreen render height (device pixels).
    fn viewport_height(&self) -> u32;

    /// Whether the device is a software rasterizer.
    fn software_gpu(&self) -> bool;

    /// Blocks until the GPU has finished every in-flight frame.
    ///
    /// The asset handlers idle before a destructive cache mutation (`scan-assets`,
    /// `reimport-model`, `delete-unused`) so dropping a cached `Arc<GpuMesh>`/
    /// `Arc<GpuTexture>` never frees a resource a frame still reads.
    fn wait_gpu_idle(&mut self);

    /// Switches the active view the engine renders + publishes.
    ///
    /// Routes the per-view render target + temporal state + shm publisher; the
    /// scene/camera swap that follows a preview enter/exit is the `SceneEditContext`'s
    /// concern, driven by the same handler.
    fn set_active_view(&mut self, view: ViewId);
    /// The render size a view last requested (device pixels), `(0, 0)` until it has been
    /// sized at least once. Read to tell whether a not-yet-shown preview pane needs
    /// seeding before a `set-active-view assetPreview`.
    fn view_desired_size(&self, view: ViewId) -> (u32, u32);
    /// Sets a view's desired offscreen render size, recreating its targets.
    ///
    /// # Errors
    ///
    /// Returns the device error message if the GPU cannot idle or the targets cannot be
    /// recreated.
    fn set_view_desired_size(
        &mut self,
        view: ViewId,
        width: u32,
        height: u32,
    ) -> std::result::Result<(), String>;

    /// Captures the active view's offscreen scene color to a PNG file (the
    /// `screenshot {target:viewport}` path). Synchronous: idles, reads back, and
    /// writes the file before returning.
    ///
    /// # Errors
    ///
    /// Returns the device / file error message if the capture cannot be performed.
    fn capture_viewport(&mut self, path: &Path) -> std::result::Result<(), String>;

    /// Requests a window-surface (swapchain) capture written at the end of the current
    /// frame (the `screenshot {target:window}` path). Returns immediately — the
    /// `screenshot` reply carries `pending: true`.
    ///
    /// # Errors
    ///
    /// Returns the device / file error message if the capture cannot be armed.
    fn request_window_capture(&mut self, path: &Path) -> std::result::Result<(), String>;

    /// Runs `with` against a transient [`GpuUploader`] over the live renderer.
    ///
    /// The upload seam borrows the renderer plus the host-owned one-off uploader for the
    /// call's duration; it never escapes the closure. Every asset handler that resolves
    /// or uploads an asset reaches the loaders through it — `import-texture`,
    /// `instantiate-model`, `material-import`, the preview floor, and `pick` (which pairs
    /// it with [`ControlRenderer::viewport_width`] / [`ControlRenderer::viewport_height`]
    /// for the ray-cast aspect).
    fn with_gpu_uploader(&mut self, with: &mut dyn FnMut(&dyn GpuUploader));

    /// Creates the qualified graph-compute backend for this renderer.
    ///
    /// `Ok(None)` explicitly identifies a renderer without compute support, such as the
    /// in-memory control test stub. A live renderer returns an error when qualification fails.
    fn create_vegetation_compute_executor(
        &self,
    ) -> std::result::Result<Option<VegetationComputeExecutor>, String>;

    /// Renders a material or texture-role preview subject through the **main forward+ graph** on the
    /// offscreen thumbnail view (displacement + procedural sky + floor + key light) and returns the
    /// PNG bytes — the sync `preview-render` seam. Live/uncached, so it reflects unsaved edits. The
    /// async Assets tiles use the same render primitive off a queue in the host, not this seam.
    ///
    /// # Errors
    ///
    /// Returns the render/read-back/encode error message.
    fn render_material_preview_png(
        &mut self,
        assets: &mut AssetServer,
        subject: crate::commands_asset::PreviewSubject,
        size: u32,
    ) -> std::result::Result<Vec<u8>, String>;

    /// Serializes the renderer's settings as the project-file `renderSettings` block (the
    /// [`ProjectHost::render_settings_to_json`] seam the project lifecycle commands save).
    fn render_settings_to_json(&self) -> Value;

    /// Applies a saved `renderSettings` block to the renderer (the
    /// [`ProjectHost::apply_render_settings`] seam project load/open/reload drives).
    fn apply_render_settings(&mut self, settings: &Value);

    /// The LuaLS type-def text written to a project's `library/sa.lua` on create/load (the
    /// `sa_lua_defs` the host generates from the script bindings). Empty under the test
    /// stub.
    fn sa_lua_defs(&self) -> String;

    /// Drops the frame-timing distribution + smoothed headlines and holds telemetry off for a
    /// short warm-up. The loader calls this when a project reaches `Ready` so the perf HUD grades
    /// steady-state frames, not the load transition + cold-pipeline warm-up.
    fn reset_frame_telemetry(&mut self);
}

/// The slice of live engine state a command may touch.
///
/// References only, assembled fresh each frame in `poll_control` and dropped at
/// the end of the drain — never stored past it. Because the fields are distinct,
/// a handler that needs `&mut` to two subsystems at once
/// borrows them disjointly through the struct, no `RefCell` required. `physics`
/// is the live play world or `None` in Edit / before the first play.
pub struct EngineContext<'a> {
    /// The OS / windowless window facade.
    pub window: &'a mut Window,
    /// The renderer, behind the [`ControlRenderer`] seam.
    pub renderer: &'a mut dyn ControlRenderer,
    /// The editor's mutable scene/selection/version state.
    pub scene_edit: &'a mut SceneEditContext,
    /// The live asset catalog + caches.
    pub assets: &'a mut AssetServer,
    /// Shared multi-source cell residency state.
    pub spatial: &'a mut ResidencyManager,
    /// The sole runtime vegetation authority, bound to one exact cooked manifest when available.
    pub vegetation: &'a mut Option<saffron_vegetation::VegetationWorld>,
    /// Closed binding state reported by the RuntimeSession scheduler.
    pub vegetation_status: saffron_runtime::VegetationRuntimeBindingStatus,
    /// Cells whose deleted disposable artifact is queued through the shared cooker.
    pub vegetation_regeneration_cells: Vec<saffron_spatial::WorldCellKey>,
    /// Collision-facet residency counters, present only with a live play world.
    pub vegetation_collision: Option<saffron_runtime::VegetationCollisionReport>,
    /// The promotion authority owning transient macro-plant entity views, present only with a
    /// live play world.
    pub vegetation_promotion: Option<&'a mut saffron_runtime::VegetationPromotion>,
    /// The navigation contribution seam: published contributions and dirty regions.
    pub vegetation_navigation: Option<&'a mut saffron_runtime::VegetationNavigationSeam>,
    /// The vegetation runtime's compact telemetry.
    pub vegetation_telemetry: Option<&'a mut saffron_runtime::VegetationTelemetry>,
    /// The live play physics world, or `None` in Edit.
    pub physics: Option<&'a mut World>,
    /// Owned asynchronous vegetation evaluation jobs and retained results.
    pub(crate) vegetation_jobs: &'a mut crate::vegetation_jobs::VegetationEvaluationJobs,
    /// Owned asynchronous vegetation cook jobs and automatic commit handoff.
    pub(crate) vegetation_cook_jobs: &'a mut crate::vegetation_cook_jobs::VegetationCookJobs,
    /// Lazily initialized graph-compute capability for the live renderer.
    pub(crate) vegetation_compute: &'a mut Option<Option<VegetationComputeExecutor>>,
}

impl EngineContext<'_> {
    pub(crate) fn vegetation_compute_executor(
        &mut self,
    ) -> Result<Option<VegetationComputeExecutor>> {
        if self.vegetation_compute.is_none() {
            let executor = self
                .renderer
                .create_vegetation_compute_executor()
                .map_err(Error::command)?;
            *self.vegetation_compute = Some(executor);
        }
        Ok(self.vegetation_compute.as_ref().cloned().flatten())
    }
}

/// The boxed handler type: a closure run on the calling (main) thread that maps
/// `(ctx, params)` to a result `Value` or a typed error. `!Send` and
/// single-thread-confined.
type HandlerFn = Box<dyn Fn(&mut EngineContext<'_>, &Value) -> Result<Value>>;

/// A registered control command: a name, one-line help, and its handler.
pub struct Command {
    /// The wire command name (`cmd` in the request envelope).
    pub name: &'static str,
    /// One-line help, surfaced by the `help` command and the editor palette.
    pub help: &'static str,
    run: HandlerFn,
}

impl Command {
    /// Runs the handler against the given context and params.
    ///
    /// # Errors
    ///
    /// Propagates whatever the handler returns — a [`Error::Command`] business
    /// failure or a [`Error::Params`] deserialize failure.
    pub fn run(&self, ctx: &mut EngineContext<'_>, params: &Value) -> Result<Value> {
        (self.run)(ctx, params)
    }
}

/// The fn-pointer command table: insertion-ordered rows plus a name index.
#[derive(Default)]
pub struct CommandRegistry {
    rows: Vec<Command>,
    by_name: HashMap<&'static str, usize>,
}

impl CommandRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an untyped command whose handler receives the raw params
    /// `Value`. `help` is the one builtin that needs this — it reflects over the
    /// registry.
    pub fn register_raw(
        &mut self,
        name: &'static str,
        help: &'static str,
        run: impl Fn(&mut EngineContext<'_>, &Value) -> Result<Value> + 'static,
    ) {
        let index = self.rows.len();
        self.by_name.insert(name, index);
        self.rows.push(Command {
            name,
            help,
            run: Box::new(run),
        });
    }

    /// Registers a typed command: deserialize `P` from the params `Value`, run the
    /// typed handler, serialize `R` back to a `Value`.
    ///
    /// This is the single site the frozen wire encoding is applied — the typed
    /// DTOs carry the decimal-string-`u64` and kebab-case-enum derives, so every
    /// handler registered this way inherits the contract for free.
    pub fn register<P, R>(
        &mut self,
        name: &'static str,
        help: &'static str,
        handler: impl Fn(&mut EngineContext<'_>, P) -> Result<R> + 'static,
    ) where
        P: DeserializeOwned + JsonSchema + 'static,
        R: Serialize,
    {
        self.register_raw(name, help, move |ctx, params| {
            let folded = fold_positional_args::<P>(params);
            let parsed: P =
                serde_json::from_value(folded).map_err(|e| Error::Params(e.to_string()))?;
            let result = handler(ctx, parsed)?;
            serde_json::to_value(result).map_err(|e| Error::Params(e.to_string()))
        });
    }

    /// Looks up a command by name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Command> {
        self.by_name.get(name).map(|&index| &self.rows[index])
    }

    /// The registered commands in insertion order (`help` iterates this).
    #[must_use]
    pub fn rows(&self) -> &[Command] {
        &self.rows
    }

    /// Dispatches one parsed request envelope to a reply envelope: echo `id`, find
    /// the command, run it, and build `{ id, ok, result | error }`.
    ///
    /// `id` echoes whatever the request carried (any JSON, absent → `null`);
    /// `ok` is always present; exactly one of `result` / `error` accompanies it.
    pub fn dispatch(&self, ctx: &mut EngineContext<'_>, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let command = request.get("cmd").and_then(Value::as_str).unwrap_or("");
        let Some(row) = self.find(command) else {
            return failure_reply(id, Error::command(format!("unknown command '{command}'")));
        };
        // `help` reflects over the live registry, so it is served here rather
        // than from a captured snapshot that would go stale as later phases
        // register their commands. The `help` row still exists (so it lists
        // itself and resolves for the palette), it just has no standalone body.
        if command == "help" {
            return json!({ "id": id, "ok": true, "result": self.help_listing() });
        }
        // While a project load is in flight the scene/catalog are mid-swap, so all but a tiny
        // allow-list of commands are discarded with the busy error — never queued for replay.
        if ctx.scene_edit.project_phase == saffron_sceneedit::ProjectPhase::Loading
            && !is_loading_safe_command(command)
        {
            return failure_reply(id, Error::Busy);
        }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        match row.run(ctx, &params) {
            Ok(result) => json!({ "id": id, "ok": true, "result": result }),
            Err(error) => failure_reply(id, error),
        }
    }

    /// The `{ commands: [{ name, help }] }` listing in registration order — the
    /// body of the `help` command.
    fn help_listing(&self) -> Value {
        let commands: Vec<Value> = self
            .rows
            .iter()
            .map(|command| json!({ "name": command.name, "help": command.help }))
            .collect();
        json!({ "commands": commands })
    }
}

/// Builds the single failure envelope used by parsed and invalid requests alike.
pub(crate) fn failure_reply(id: Value, error: Error) -> Value {
    json!({ "id": id, "ok": false, "error": error.into_failure() })
}

/// `params[name]` if present, else the index-th element of `params["args"]`,
/// else `Null`.
///
/// This is the lenient read every handler shares so a command accepts either
/// `--name value` (an object key) or a bare positional. Domain handlers use it
/// to extract a selector before resolving it; the typed [`CommandRegistry::register`]
/// wrapper consumes the object form directly.
#[must_use]
pub fn positional_or(params: &Value, name: &str, index: usize) -> Value {
    if let Some(value) = params.get(name) {
        return value.clone();
    }
    params
        .get("args")
        .and_then(Value::as_array)
        .and_then(|args| args.get(index))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The per-type cache of a DTO's positional field order, so a typed command resolves the order
/// once (the first dispatch) rather than rebuilding the `schemars` schema each call.
fn field_order_cache() -> &'static Mutex<HashMap<TypeId, &'static [String]>> {
    static CACHE: OnceLock<Mutex<HashMap<TypeId, &'static [String]>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The declaration-ordered wire field names of DTO `P`, cached per type.
fn field_order<P: JsonSchema + 'static>() -> &'static [String] {
    let id = TypeId::of::<P>();
    let mut cache = field_order_cache()
        .lock()
        .expect("field-order cache poisoned");
    cache.entry(id).or_insert_with(|| {
        // Leaked once per DTO type (a bounded, process-lifetime set), so the borrow is `'static`.
        Box::leak(saffron_protocol::positional_field_order::<P>().into_boxed_slice())
    })
}

/// Folds a request's positional `args` array onto DTO `P`'s named fields before deserializing:
/// `args[i]` fills the `i`-th declared field.
///
/// A named key always wins over its positional slot. With no `args` array the params pass
/// through untouched, so the common object-form call costs nothing past the field-order lookup.
/// This is the single site the positional-CLI-argument wire shape (what `sa <cmd> <a> <b>` and
/// the e2e harness send as `{ args: [...] }`) is applied, so every typed command inherits it.
fn fold_positional_args<P: JsonSchema + 'static>(params: &Value) -> Value {
    let Some(args) = params.get("args").and_then(Value::as_array) else {
        return params.clone();
    };
    let mut object = match params.as_object() {
        Some(map) => map.clone(),
        None => return params.clone(),
    };
    for (index, field) in field_order::<P>().iter().enumerate() {
        if let Some(value) = args.get(index)
            && !object.contains_key(field)
        {
            object.insert(field.clone(), value.clone());
        }
    }
    Value::Object(object)
}

/// Registers the builtin commands: `ping` then `help`, in that order, then the
/// domain phases' `register_*_commands`.
pub fn register_builtin_commands(reg: &mut CommandRegistry) {
    reg.register::<PingParams, PingResult>("ping", "liveness + engine info", |_ctx, _params| {
        Ok(PingResult {
            pong: true,
            engine: saffron_core::ENGINE_NAME.to_owned(),
            version: saffron_core::ENGINE_VERSION.to_owned(),
            pid: process_id(),
        })
    });

    // `help` reflects over the live registry, so `dispatch` serves it directly
    // from the registration order; the row registered here exists so `help`
    // lists itself and resolves for the editor palette. Its registered body is
    // never invoked.
    reg.register_raw("help", "list available commands", |_ctx, _params| {
        Ok(json!({ "commands": [] }))
    });

    // The domain groups register in the frozen order render → scene → animation → physics
    // → vegetation → asset. `help` and the manifest-completeness check iterate the registry as a set, so
    // the asset group is the manifest tail (`get-project` … `quit`); the scene group lands
    // between render and animation.
    crate::commands_render::register_render_commands(reg);
    crate::commands_scene::register_scene_commands(reg);
    crate::commands_animation::register_animation_commands(reg);
    crate::commands_physics::register_physics_commands(reg);
    crate::commands_vegetation::register_vegetation_commands(reg);
    crate::commands_asset::register_plant_commands(reg);
    crate::commands_asset::register_interchange_commands(reg);
    crate::commands_asset::register_asset_commands(reg);
}

/// The process id, for the `ping` reply.
fn process_id() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(0)
}

/// Whether a control command leaves the rendered image unchanged — the editor's per-frame
/// reconcile/stats pollers and every pure query.
///
/// The reactive render loop ([`saffron_app::RedrawController`]) renders a frame for any command
/// **not** listed here, so the classification errs toward rendering: a query missing from this set
/// costs at most one redundant frame per poll, while a *mutating* command can never be mislabeled
/// read-only (the default is "mutates"), so a static viewport never shows a stale frame. This is the
/// single source of truth for the read-vs-mutate split; `poll` consults it after each dispatch.
#[must_use]
pub fn is_read_only_command(name: &str) -> bool {
    // Every `get-*` / `list-*` is a query by construction.
    if name.starts_with("get-") || name.starts_with("list-") {
        return true;
    }
    matches!(
        name,
        // liveness + help
        "ping" | "help"
        // telemetry the stats / profiler panels poll each interval
        | "render-stats" | "pass-timings" | "frame-history" | "profiler.capture-status"
        // scene / entity / physics queries the reconcile poll runs every tick
        | "inspect" | "physics-state" | "physics-bodies"
        // ring-buffer drains: advance a read cursor, never the image
        | "drain-alarms" | "drain-contacts" | "drain-script-errors" | "drain-script-logs"
        // asset / material / model introspection
        | "model-info" | "asset-references" | "asset-usages" | "probe-asset"
        | "material-get" | "material-list" | "viewport-native-info" | "thumbnail-cache"
        // spatial queries (cast a ray, read back a hit — no scene change). `pick` is excluded:
        // it sets the selection, which changes the rendered gizmo / skeleton overlay, so a
        // viewport-click select must request a redraw under the reactive loop.
        | "raycast" | "shapecast" | "pick-skeleton-joint"
        | "spatial-cell" | "spatial-providers" | "spatial-sample" | "spatial-residency"
        // biome graph compilation/schema and retained evaluation diagnostics do not change the scene
        | "vegetation-compile-biome" | "vegetation-node-schema"
        | "vegetation-preflight-region" | "vegetation-start-evaluation"
        | "vegetation-evaluation-status"
        | "vegetation-cancel-evaluation" | "vegetation-explain-point"
        | "vegetation-cook-status" | "vegetation-cancel-cook"
        | "vegetation-cell-inspect" | "vegetation-manifest"
        | "vegetation-runtime-status" | "vegetation-runtime-cell"
        | "vegetation-runtime-query" | "vegetation-runtime-inspect"
        | "vegetation-combustion" | "vegetation-ecology-status" | "vegetation-telemetry"
        | "vegetation-state-export"
        | "plant-validate" | "plant-growth" | "plant-graph" | "plant-elements"
        // project-load phase + progress the editor's loading screen polls each tick
        | "project-status"
    )
}

/// Commands that stay serviceable while a project load is in flight. Deliberately tiny: during a
/// load the scene/catalog are mid-swap, so even reads (`inspect`, `list-entities`) could observe a
/// torn state — they get the busy error and the editor retries them once `Ready`. The loading
/// screen polls `project-status` for progress and offers `cancel-load` to abort; both must pass the
/// gate they run under. `viewport-native-info` is here too: it reads only the (swap-independent)
/// viewport surface, and the editor's readiness probe drives it to flip the engine to `ready` — a
/// gate the project-load poll itself runs behind, so blocking it during a load would stall the very
/// progress the load is trying to report.
#[must_use]
pub fn is_loading_safe_command(name: &str) -> bool {
    matches!(
        name,
        "ping"
            | "help"
            | "get-project"
            | "project-status"
            | "cancel-load"
            | "quit"
            | "viewport-native-info"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{StubRenderer, with_stub};

    /// Runs `body` against a fresh `EngineContext` over a default renderer stub.
    fn with_ctx<T>(body: impl FnOnce(&mut EngineContext<'_>) -> T) -> T {
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, body)
    }

    fn builtins() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    #[test]
    fn ping_reports_engine_identity_and_pid() {
        let reg = builtins();
        let reply = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "id": 1, "cmd": "ping" })));
        assert_eq!(reply["id"], json!(1));
        assert_eq!(reply["ok"], json!(true));
        let result = &reply["result"];
        assert_eq!(result["pong"], json!(true));
        assert_eq!(result["engine"], json!(saffron_core::ENGINE_NAME));
        assert_eq!(result["version"], json!(saffron_core::ENGINE_VERSION));
        assert_eq!(result["pid"], json!(process_id()));
    }

    /// Every command in the frozen protocol manifest (`saffron_protocol::COMMANDS`) has a
    /// handler in the builtin registry — except `get-script-schema`, which the host registers
    /// (it needs the Lua schema reader) — and the registry registers nothing the manifest does
    /// not declare. This is the manifest-completeness contract (set equality, not order: the
    /// registry iterates in registration order while the manifest table is the generation
    /// order, and each domain's intra-order is locked by its own per-file order test). A
    /// command added to the protocol table without a matching handler trips here.
    #[test]
    fn registry_covers_the_protocol_manifest() {
        use std::collections::BTreeSet;
        const HOST_REGISTERED: &[&str] = &["get-script-schema"];
        let reg = builtins();
        // `help` is the reflective-registry builtin the manifest skips by design
        // (`HELP_COMMAND`, never in `COMMANDS`), so it is not a manifest row.
        let registered: BTreeSet<&str> = reg
            .rows()
            .iter()
            .map(|c| c.name)
            .filter(|&name| name != saffron_protocol::HELP_COMMAND)
            .collect();
        let expected: BTreeSet<&str> = saffron_protocol::COMMANDS
            .iter()
            .map(|c| c.name)
            .filter(|name| !HOST_REGISTERED.contains(name))
            .collect();
        let missing: Vec<&&str> = expected.difference(&registered).collect();
        let unexpected: Vec<&&str> = registered.difference(&expected).collect();
        assert!(
            missing.is_empty(),
            "protocol commands with no registered handler: {missing:?}"
        );
        assert!(
            unexpected.is_empty(),
            "registered commands not in the protocol manifest: {unexpected:?}"
        );
    }

    #[test]
    fn help_lists_commands_in_registration_order() {
        let reg = builtins();
        let reply = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "cmd": "help" })));
        assert_eq!(reply["ok"], json!(true));
        let commands = reply["result"]["commands"].as_array().unwrap();
        // ping is registered first, help second, then the render domain. The two
        // builtins lead; render-stats opens the render group.
        assert!(commands.len() >= 3);
        assert_eq!(commands[0]["name"], json!("ping"));
        assert_eq!(commands[0]["help"], json!("liveness + engine info"));
        assert_eq!(commands[1]["name"], json!("help"));
        assert_eq!(commands[1]["help"], json!("list available commands"));
        assert_eq!(commands[2]["name"], json!("render-stats"));
    }

    #[test]
    fn unknown_command_is_an_error_with_id_echoed() {
        let reg = builtins();
        let reply = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "cmd": "nope" })));
        // No id in the request → null in the reply.
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["error"]["code"], json!("command"));
        assert_eq!(reply["error"]["message"], json!("unknown command 'nope'"));
        assert!(reply.get("code").is_none());
        assert!(reply.get("result").is_none());
    }

    #[test]
    fn loading_gate_discards_non_allow_listed_commands() {
        let reg = builtins();
        with_ctx(|ctx| {
            ctx.scene_edit.project_phase = saffron_sceneedit::ProjectPhase::Loading;

            // A mutating command is discarded with the busy code while Loading.
            let blocked = reg.dispatch(ctx, &json!({ "id": 1, "cmd": "add-entity" }));
            assert_eq!(blocked["ok"], json!(false));
            assert_eq!(blocked["error"]["code"], json!("busy-loading"));
            assert_eq!(blocked["id"], json!(1));

            // Allow-listed liveness / identity commands stay serviceable.
            assert_eq!(
                reg.dispatch(ctx, &json!({ "cmd": "ping" }))["ok"],
                json!(true)
            );
            assert_eq!(
                reg.dispatch(ctx, &json!({ "cmd": "get-project" }))["ok"],
                json!(true)
            );
        });
    }

    #[test]
    fn error_arm_emits_code_uniformly() {
        // A params deserialize failure carries the `params` code, proving the `Err` arm emits a
        // code for every business/params error, not just the busy gate.
        let reg = builtins();
        let reply = with_ctx(|ctx| {
            reg.dispatch(
                ctx,
                &json!({ "cmd": "set-aa", "params": { "args": ["nonsense"] } }),
            )
        });
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["error"]["code"], json!("params"));
    }

    #[test]
    fn graph_limit_failure_preserves_exact_diagnostic_fields() {
        let reply = failure_reply(
            json!(9),
            Error::from(saffron_vegetation::Error::GraphLimit {
                resource: "candidates",
                requested: 16,
                limit: 4,
            }),
        );
        assert_eq!(
            reply,
            json!({
                "id": 9,
                "ok": false,
                "error": {
                    "code": "diagnostic",
                    "message": "graph candidates limit exceeded: requested 16, limit 4",
                    "diagnostic": {
                        "domain": "vegetation-graph",
                        "detail": {
                            "category": "limit",
                            "resource": "candidates",
                            "requested": "16",
                            "limit": "4",
                        },
                    },
                },
            })
        );
    }

    #[test]
    fn id_echoes_number_string_and_absent() {
        let reg = builtins();
        let number = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "id": 7, "cmd": "ping" })));
        assert_eq!(number["id"], json!(7));

        let string = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "id": "abc", "cmd": "ping" })));
        assert_eq!(string["id"], json!("abc"));

        let absent = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "cmd": "ping" })));
        assert_eq!(absent["id"], Value::Null);
    }

    #[test]
    fn missing_cmd_is_an_unknown_empty_command() {
        let reg = builtins();
        let reply = with_ctx(|ctx| reg.dispatch(ctx, &json!({ "id": 3 })));
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["error"]["message"], json!("unknown command ''"));
    }

    #[test]
    fn typed_register_surfaces_a_params_deserialize_error() {
        let mut reg = CommandRegistry::new();
        reg.register::<Vec3Holder, Vec3Holder>("echo", "echo", |_ctx, p| Ok(p));
        // `x` must be a number; a string fails the typed deserialize and becomes
        // the envelope error rather than a panic.
        let reply =
            with_ctx(|ctx| reg.dispatch(ctx, &json!({ "cmd": "echo", "params": { "x": "nan" } })));
        assert_eq!(reply["ok"], json!(false));
        assert!(reply.get("error").is_some());
    }

    #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct Vec3Holder {
        x: f32,
    }

    #[test]
    fn typed_register_folds_positional_args_onto_named_fields() {
        use saffron_protocol::CreateEntityParams;
        // `create-entity` with a bare positional arg (the `sa create-entity foo` / e2e
        // `{ args: ["foo"] }` form) folds `args[0]` onto the `name` field.
        let folded = fold_positional_args::<CreateEntityParams>(&json!({ "args": ["foo"] }));
        assert_eq!(folded, json!({ "args": ["foo"], "name": "foo" }));
        // A named key wins over its positional slot.
        let named =
            fold_positional_args::<CreateEntityParams>(&json!({ "name": "kept", "args": ["x"] }));
        assert_eq!(named["name"], json!("kept"));
        // No `args` array → untouched.
        let plain = fold_positional_args::<CreateEntityParams>(&json!({ "name": "n" }));
        assert_eq!(plain, json!({ "name": "n" }));
    }

    #[test]
    fn typed_register_rejects_an_invalid_positional_enum_value() {
        // `set-aa nonsense` (`{ args: ["nonsense"] }`) folds onto `mode`, then fails the
        // `AaModeDto` enum deserialize — the negative oracle the e2e asserts (bad input is
        // rejected, not silently accepted as an absent optional).
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        let reply = with_ctx(|ctx| {
            reg.dispatch(
                ctx,
                &json!({ "cmd": "set-aa", "params": { "args": ["nonsense"] } }),
            )
        });
        assert_eq!(reply["ok"], json!(false));
    }

    #[test]
    fn positional_or_prefers_named_then_args_then_null() {
        // Named key wins.
        let named = json!({ "entity": 5, "args": [9] });
        assert_eq!(positional_or(&named, "entity", 0), json!(5));
        // Falls back to the index-th positional arg.
        let positional = json!({ "args": [9, 10] });
        assert_eq!(positional_or(&positional, "entity", 1), json!(10));
        // Neither present → null.
        let empty = json!({});
        assert_eq!(positional_or(&empty, "entity", 0), Value::Null);
        // Out-of-range positional → null.
        assert_eq!(positional_or(&positional, "entity", 5), Value::Null);
    }
}
