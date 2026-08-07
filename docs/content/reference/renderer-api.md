+++
title = 'Renderer API'
weight = 4
math = false
+++

# Renderer API

The public surface of `saffron-rendering`. The `Renderer` owns the instance/device/swapchain/allocator, the descriptor sets, the IBL bake, and the per-view offscreen targets; nearly every method takes `&mut self` and fallible ones return `Result<T>`. Vulkan handles are the `ash` `vk::` bindings. Mesh/texture upload runs through a separate `Uploader` (the host constructs one per drain), not through the `Renderer`.

| What | File | Symbols |
|---|---|---|
| The renderer | `renderer.rs` | `Renderer`, `Renderer::new`, `ViewId`, `ViewMode`, `RenderStatsFull` |
| GPU-facing data types | `gpu_types.rs` | `Material`, `GpuLight`, `MaterialParamsData`, `SdfInstance` |
| Frame deformation state | `draw_list.rs` | `FrameDeformation`, `SubmeshMaterial`, `RenderStats` |
| Lighting inputs | `lighting.rs` | `SceneLighting`, `ClusterCamera` |
| Upload | `upload.rs` | `Uploader`, `GpuQueue` |

## Lifecycle

| Symbol | Effect |
|---|---|
| `Renderer::new(surface_source: &SurfaceSource, width: u32, height: u32) -> Result<Renderer>` | build instance/device/swapchain/allocator + descriptors + IBL bake |
| `device().wait_idle() -> Result<()>` | block until all submitted work finishes (also run by `Renderer`'s `Drop`) |

The renderer holds an explicit `Drop` that waits idle before tearing down, so resources free in order.

## Per-frame

| Symbol | Effect |
|---|---|
| `begin_offscreen_frame() -> Result<()>` | start the offscreen scene frame for the active view |
| `render_scene_offscreen() -> Result<()>` | build + execute the frame graph (cull + scene + AA + tonemap) into the offscreen target |
| `submit(body: impl FnOnce(vk::CommandBuffer) + 'static)` | record a closure into the scene pass after the executor draws (the gizmo / native overlay seam) |
| `render_frame() -> Result<bool>` | the full present-only loop: acquire → render → present; `false` if it recreated the swapchain |
| `begin_present_frame() -> Result<bool>` / `present_active_view_to_swapchain() -> Result<()>` | the split acquire / present steps |

The submit closure type is `RenderFn = Box<dyn FnOnce(vk::CommandBuffer)>`; it captures resolved handles and runs once on the render thread.

## Views and viewport target

The renderer keeps `VIEW_COUNT` views (`ViewId::Scene` and `ViewId::AssetPreview`).

| Symbol | Effect |
|---|---|
| `set_active_view(view: ViewId)` / `active_view_id() -> ViewId` | the view subsequent calls address |
| `view(view: ViewId) -> &ViewTarget` | a view's offscreen target |
| `set_viewport_desired_size(width, height) -> Result<()>` | desired offscreen size in device pixels |
| `viewport_width() -> u32` / `viewport_height() -> u32` | current offscreen size |
| `reset_view_temporal(view: ViewId)` | drop a view's TAA history |

## Frame draws

| Symbol | Effect |
|---|---|
| `submit_gpu_scene_deformations(view_proj: Mat4, work: &[DeformationWork], joints: &[Mat4]) -> Result<()>` | wire the frame's skin/morph/tessellation dispatches + the joint palette (the executor draws come from the visibility traversal) |
| `stats() -> RenderStats` / `render_stats() -> RenderStatsFull` | last frame's draw counters / counters + timing + flags |
| `pipeline_count() -> u32` | distinct cached mesh PSOs |

`pipelines()` returns `&mut Pipelines`; `Pipelines::request_executor_mesh_pipeline(material, …)` is the PSO-cache front door (build-and-cache on first request).

## Lighting

| Symbol | Effect |
|---|---|
| `set_scene_lighting(scene: &SceneLighting) -> Result<()>` | directional + ambient + eye + the per-frame punctual `GpuLight` list |
| `set_cluster_camera(camera: ClusterCamera)` | view / projection / size / z-planes for froxel culling |
| `set_clustered(bool)` / `clustered_enabled() -> bool` | toggle / query clustered culling |

`SceneLighting { direction, color, intensity, ambient: Vec3, eye_position, lights: Vec<GpuLight> }`. `ClusterCamera { view, projection: Mat4, width, height: u32, near, far: f32 }`.

## Feature toggles (paired set/query)

Each is a `set_*(&mut self, bool)` with a `*_enabled(&self) -> bool` query:

| Feature | Methods |
|---|---|
| IBL ambient | `set_ibl` / `ibl_enabled` |
| GTAO | `set_ssao` / `ssao_enabled` |
| Contact shadows | `set_contact_shadows` / `contact_shadows_enabled` |
| Screen-space GI | `set_ssgi` / `ssgi_enabled` |
| DDGI probe GI | `set_ddgi` / `ddgi_enabled` |
| Directional shadows | `set_shadows` / `shadows_enabled` |
| Depth pre-pass | `set_depth_prepass` / `depth_prepass_enabled` |
| GPU skinning | `set_skinning` / `skinning_enabled` |
| Reflection probes | `set_reflection_probes` / `reflection_probes_enabled` |
| RT shadows | `set_rt_shadows` / `rt_shadows_enabled` (with `rt_supported() -> bool`) |
| ReSTIR direct | `set_restir` / `restir_enabled` |

Tonemap exposure is `set_exposure(ev: f32)` / `exposure_ev() -> f32`. Anti-aliasing is `set_aa(msaa_samples: u32, fxaa: bool, taa: bool) -> Result<()>` (or `set_aa_mode(&str)`) with `aa_mode() -> String` returning `"off"` / `"fxaa"` / `"taa"` / `"msaa2|4|8"`. The debug view channel is `set_view_mode(ViewMode)` / `view_mode() -> ViewMode`.

## Shadow / screen-space arming (per frame)

| Symbol | Effect |
|---|---|
| `set_directional_shadow(light_view_proj: Mat4, casting: bool)` | arm the directional shadow map |
| `set_spot_shadow(light_view_proj: Mat4, light_index: u32, casting: bool)` | arm a spot shadow |
| `set_point_shadow(light_view_proj: Mat4, light_index: u32, casting: bool)` | arm an omnidirectional point shadow |
| `set_ssao_camera(camera: ClusterCamera)` | the camera SSAO/GTAO and motion need |
| `set_ddgi_scene(models, meshes, extent) -> Result<()>` | the DDGI volume box geometry |

## Capture and thumbnails

| Symbol | Effect |
|---|---|
| `capture_viewport(path: &Path) -> Result<()>` | synchronous PNG of the active view's offscreen color |
| `request_window_capture(path: &Path) -> Result<()>` | arm a PNG of the next presented frame; `window_capture_pending() -> bool` |
| `render_mesh_thumbnail(mesh: &Arc<GpuMesh>, size: u32) -> Result<Arc<GpuTexture>>` | render a mesh to a thumbnail texture |

Mesh and texture upload go through `Uploader::upload_mesh(mesh, skin) -> Result<Arc<GpuMesh>>` and `Uploader::upload_texture(...) -> Result<Arc<GpuTexture>>`, constructed over the device + a `GpuQueue`.

## Constants

| Symbol | Value | File |
|---|---|---|
| `MAX_FRAMES_IN_FLIGHT` | `2` | `frame.rs` |
| `MAX_BINDLESS_TEXTURES` | `1024` | `descriptors.rs` |
| `VIEW_COUNT` | `2` | `renderer.rs` |
| `OFFSCREEN_COLOR_FORMAT` | `vk::Format::R16G16B16A16_SFLOAT` | `pipelines.rs` |
| `DEPTH_FORMAT` | `vk::Format::D32_SFLOAT` | `pipelines.rs` |

## Key data structs

| Type | Fields (abridged) |
|---|---|
| `Material` | `shader: String` (default `"shaders/mesh.spv"`); `unlit`, `blend`, `masked: bool` |
| `DeformationWork` | `mesh: Arc<GpuMesh>`; `model: Mat4`; `skinned: bool`; `joint_offset`, `joint_count: u32`; `morph_weights: Vec<f32>`; `displace: Option<DisplaceInfo>`; `instance_slot: u32`; `entity: u64` |
| `SubmeshMaterial` | the per-submesh textures (`Option<Arc<GpuTexture>>`) + `base_color`, `metallic`, `roughness`, `emissive`, UV / normal / alpha factors |
| `RenderStats` | `draw_calls`, `batches`, `instances`, `triangles`, `descriptor_binds`, `command_buffers`, `queue_submits`, `pipelines_created: u32` |
| `GpuLight` | `position_range`, `color_intensity`, `direction_type` (.w: 0 = point, 1 = spot), `spot_cos: Vec4` |

## Related

- [Render seams](../../explanations/app-lifecycle-and-window/the-submit-and-rendergraph-seams/) — how `submit` feeds the frame
- [Material and PSO selection](../../explanations/materials-and-pipelines/material-and-pso-selection/) — what `request_executor_mesh_pipeline` keys on
- [Meta-layer resources](../../explanations/vulkan-foundation/meta-layer-resources/) — `Arc<GpuMesh>` / `GpuTexture` ownership
