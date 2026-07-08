//! The Vulkan renderer: ash, the VMA allocator, the swapchain, the render graph,
//! and every pass (one crate, many submodules — the ~80-field renderer aggregate
//! cannot be cut across a crate boundary).
//!
//! Depends on `saffron-core`, `saffron-window`, `saffron-geometry`. One of the
//! three FFI crates in the engine: every other crate denies unsafe.
//!
//! # The `unsafe` seam
//!
//! `#![allow(unsafe_code)]` is set crate-wide because `ash` is a thin, unchecked
//! binding over the Vulkan C API — `vkCreateInstance`, `vkAcquireNextImageKHR`,
//! `vkQueueSubmit2` and the VMA FFI are all `unsafe`. The unsafe is confined to
//! the [`device`] / [`swapchain`] / [`renderer`] modules and wrapped in safe
//! methods ([`Device::new`], [`Swapchain::new`], [`Renderer::render_frame`]), so
//! no caller of this crate ever touches a raw handle.
#![allow(unsafe_code)]

mod aa;
mod budget;
mod ddgi;
mod descriptors;
mod device;
mod draw_list;
mod frame;
mod frame_history;
mod global_sdf;
mod gpu_types;
mod ibl;
mod instancing;
mod lighting;
mod meshlet_raster;
mod nested_scopes;
mod overlay;
mod pipelines;
mod present;
mod profiler;
mod quality;
mod reactive;
mod render_graph;
mod render_settings;
mod renderer;
mod resources;
mod restir;
mod rt;
mod scene_pass;
mod shm_publish;
mod skinning;
mod ssao;
mod swapchain;
mod targets;
mod tessellation;
mod thumbnail;
mod transient;
mod upload;
mod view_target;

pub use aa::{
    Aa, MOTION_FORMAT, MotionPush, REACTIVE_FORMAT, TAA_JITTER_PHASES, TaaParams, TaaPush,
    clamp_sample_count, jitter_offset, jitter_phase_count, record_motion,
};
pub use ddgi::{
    BlendPush as DdgiBlendPush, BorderPush as DdgiBorderPush, DDGI_DIST_FORMAT, DDGI_DIST_INTERIOR,
    DDGI_HYSTERESIS, DDGI_IRR_FORMAT, DDGI_IRR_INTERIOR, DDGI_PROBE_BUDGET, DDGI_PROBE_SPACING,
    DDGI_PROBE_TOTAL, DDGI_PROBES_X, DDGI_PROBES_Y, DDGI_PROBES_Z, DDGI_RAY_FORMAT,
    DDGI_RAYS_PER_PROBE, Ddgi, TracePush as DdgiTracePush,
};
pub use descriptors::{
    DEFAULT_WHITE_SLOT, Descriptors, MAX_BINDLESS_SDF, MAX_BINDLESS_TEXTURES, MAX_REFLECTION_PROBES,
};
pub use device::{Capabilities, Device, ProfilerFacts, SurfaceSource, validation_issue_count};
pub use draw_list::{
    DeformedRtInstance, DrawBatch, DrawItem, MorphDispatch, RenderStats, SceneDrawList,
    SkinDispatch, SubmeshMaterial, TessDraw, TessRtSlice, normal_matrix,
};
pub use frame::MAX_FRAMES_IN_FLIGHT;
pub use frame_history::{
    ALARM_EVENT_RING_CAPACITY, ALARM_RESUME_SETTLE_FRAMES, ActiveAlarm, AlarmDrain, AlarmEvent,
    AlarmEventKind, AlarmInputs, AlarmSeverity, AlarmState, FRAME_HISTORY_CAPACITY, FrameHistory,
    FrameHistoryStats, FrameSample, PerfConfig,
};
pub use global_sdf::{
    GDF_BAND_FRACTION, GDF_CASCADE0_EXTENT, GDF_CASCADES, GDF_EXPONENT, GDF_FORMAT, GDF_MAX_CULLED,
    GDF_NEAR_HANDOFF, GDF_RES, GdfCompositePush, GdfCullPush, GdfParamsUbo, GdfRegion, GlobalSdf,
};
pub use gpu_types::{GpuLight, InstanceData, Material, MaterialParamsData, SdfInstance};
pub use ibl::{
    ATMOS_MULTI_SCATTER_SIZE, ATMOS_SKY_VIEW_H, ATMOS_SKY_VIEW_W, ATMOS_TRANSMITTANCE_H,
    ATMOS_TRANSMITTANCE_W, AtmosphereParams, EnvSource, IBL_COLOR_FORMAT, IBL_ENV_SIZE,
    IBL_IRRADIANCE_SIZE, IBL_LUT_SIZE, IBL_PREFILTER_MIPS, IBL_PREFILTER_SIZE, Ibl, ProbeMetaGpu,
    ReflectionProbe, ReflectionProbeUpload, ReflectionProbes, Sky, SkyDraw, SkyRenderSettings,
    SkygenParams, record_sky,
};
pub use instancing::{DrawListInputs, Instancing};
pub use lighting::{
    CLUSTER_COUNT, CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, ClusterCamera, ClusterParams,
    LightUbo, Lighting, MAX_LIGHTS_PER_CLUSTER, POINT_SHADOW_SIZE, SHADOW_MAP_SIZE, SceneLighting,
    cull_clusters_cpu, point_shadow_face_matrices,
};
pub use overlay::{
    GridPush, OverlayDraw, OverlayState, OverlayVertex, TonemapMode, TonemapPush, record_grid,
    record_overlay,
};
pub use pipelines::{DEPTH_FORMAT, OFFSCREEN_COLOR_FORMAT, Pipelines, PsoKey};
pub use profiler::{
    CaptureMode, CaptureRecorder, CaptureState, CpuMarkerRegistry, CpuProfiler, CpuSpan,
    CpuSpanBuffer, GpuCalibration, GpuProfiler, MAX_CAPTURE_FRAMES, MAX_PROFILED_SCOPES,
    PIPELINE_STATS_COUNT, PassTiming, PipelineStats, ProfileCapture, ProfileCaptureMeta,
    ProfileLane, ProfileSpan, ProfilerMode, RgTimestamps, ScopeRecord, cpu_now_ns,
    pipeline_stats_flags,
};
pub use quality::{QualityTier, RenderQuality};
pub use reactive::{PowerState, ReactiveState};
pub use render_graph::{
    ProfileRecorders, RenderGraph, RgAccess, RgAttachment, RgPass, RgPassKind, RgResource, RgUsage,
};
pub use renderer::{RenderStatsFull, Renderer, VIEW_COUNT, ViewId, ViewMode};
pub use resources::{
    AccelerationStructure, BindlessFreeList, Buffer, DefaultHeightMinMax, DeviceResources, GpuMesh,
    GpuMeshParts, GpuSdf, GpuSdfParts, GpuTexture, GpuTextureParts, Image, Image3D, ImageDesc,
    MinMaxPyramid, Pipeline,
};
pub use restir::{
    InitialPush as RestirInitialPush, RESTIR_CANDIDATE_COUNT, RESTIR_INITIAL_PUSH_SIZE,
    RESTIR_MAX_M, RESTIR_RADIANCE_FORMAT, RESTIR_RESOLVE_PUSH_SIZE, RESTIR_REUSE_PUSH_SIZE,
    RESTIR_SPATIAL_RADIUS, Reservoir, ResolvePush as RestirResolvePush, Restir, RestirView,
    ReusePush as RestirReusePush, reservoir_bytes as restir_reservoir_bytes, wants_restir,
};
pub use rt::{
    BlasRefitOp, MeshBlasBuild, Rt, RtScene, TlasBuildOp, TlasBuildPlan, record_mesh_blas_build,
    record_tlas_build_plan,
};
pub use scene_pass::{
    PointShadowTarget, record_depth_prepass, record_gbuffer, record_point_shadow,
    record_reactive_coverage, record_scene_draw_list, record_shadow_depth,
    record_transparent_draw_list,
};
pub use shm_publish::{
    MIN_SHM_SLOT_CAPACITY, SHM_HEADER_BYTES, SHM_MAGIC, SHM_RING_SLOTS, ShmPublish,
};
pub use skinning::{
    MORPH_FIXED_SCALE, SKIN_MAX_SETS_PER_FRAME, Skinning, record_morph, request_morph_pipeline,
    wire_morph_set,
};
pub use ssao::{
    AO_FORMAT, ContactPush, DfaoPush, G_NORMAL_FORMAT, GbufferPush, GtaoPush, ROUGHNESS_FORMAT,
    SSGI_HISTORY_WEIGHT, SpecoccPush, Ssao, SsgiAccumPush, SsgiPush,
};
pub use swapchain::Swapchain;
pub use targets::{PointShadowCube, Targets};
pub use tessellation::{
    TESS_CLAS_MAX_TRIS, TESS_CLAS_MAX_VERTS, TESS_DEFAULT_EDGE_LENGTH_TARGET,
    TESS_DEFAULT_FACTOR_CAP, TESS_DEFAULT_MIN_FACTOR, TESS_MAX_DICE_FACTOR, TESS_MAX_INSTANCES,
    TESS_MICRO_VERTEX_BUDGET, TessBucket, TessCamera, TessEmitPush, TessFactorPush,
    TessFinalizePush, TessInstanceLayout, TessScanPush, Tessellation, budget_scaled_caps,
    coarse_parent_bary, displacement_aware_factor, factor_push, geomorph_weight,
    project_world_to_pixels, smoothstep01, split_recursion, tess_worst_case, wire_storage_set,
};
pub use thumbnail::{
    PngTransfer, ThumbnailPng, convert_to_rgb, encode_to_png, format_pixel_bytes, write_png_file,
};
pub use transient::TransientResources;
pub use upload::{GpuQueue, SdfBake, Uploader};
pub use view_target::ViewTarget;

use ash::vk;

/// Errors from the Vulkan bring-up and per-frame paths.
///
/// The typed [`Error::Vk`] variant carries the raw [`vk::Result`] so callers can
/// `match` on the exact failure, and `?` over an ash call *is* the check — there is
/// no separate check-then-propagate step.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Vulkan loader could not be initialized (no ICD / no `libvulkan`).
    #[error("failed to load the Vulkan loader: {0}")]
    Loader(String),

    /// A Vulkan call returned a non-success [`vk::Result`].
    #[error("vulkan call '{context}' failed: {result:?}")]
    Vk {
        /// The operation that failed, for the message.
        context: &'static str,
        /// The raw Vulkan result code.
        result: vk::Result,
    },

    /// No physical device satisfied the required feature set.
    #[error("no suitable Vulkan device: {0}")]
    NoDevice(String),

    /// The surface exposed no usable graphics-and-present queue family.
    #[error("no graphics+present queue family on the selected device")]
    NoQueueFamily,

    /// The window could not hand out a surface handle (e.g. headless winit mode
    /// without a headless-surface fallback).
    #[error("the window exposes no surface handle: {0}")]
    NoSurfaceHandle(String),

    /// A mesh upload was handed a mesh with no vertices or no indices.
    #[error("upload_mesh: empty mesh")]
    EmptyMesh,

    /// A skinned mesh upload's skin stream did not parallel the vertex stream.
    #[error("upload_mesh: skin stream ({skin}) does not parallel the vertices ({vertices})")]
    SkinMismatch {
        /// The skin stream length.
        skin: usize,
        /// The vertex count it must match.
        vertices: usize,
    },

    /// A texture upload was handed a zero-width or zero-height image.
    #[error("upload_texture: zero-sized image")]
    ZeroSizedImage,

    /// A SPIR-V shader module could not be read or is malformed (size not a
    /// multiple of 4, or unreadable).
    #[error("shader load failed: {0}")]
    ShaderLoad(String),

    /// The GPU signed-distance-field bake could not run or its sidecar was malformed
    /// (no bake pipelines, or a decode/IO failure on the cache).
    #[error("sdf bake failed: {0}")]
    SdfBake(String),

    /// A bindless array (albedo textures or per-mesh SDF fields) is full — every slot up
    /// to its capacity is occupied — so the upload was skipped rather than writing an
    /// out-of-range `dstArrayElement`.
    #[error("bindless array full: {0}")]
    BindlessFull(&'static str),
}

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Wraps an ash `VkResult<T>` into this crate's typed [`Error::Vk`], tagging the
/// failing operation. This is the single point that maps the ash seam onto the
/// engine error model.
pub(crate) fn checked<T>(
    result: std::result::Result<T, vk::Result>,
    context: &'static str,
) -> Result<T> {
    result.map_err(|result| Error::Vk { context, result })
}
