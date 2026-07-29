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
mod canonical_coverage;
mod checkpoints;
mod clouds;
mod compute_dispatch;
mod conformance;
mod count_scan_scatter;
mod ddgi;
mod descriptors;
mod device;
mod draw_list;
mod frame;
mod frame_history;
mod froxel_fog;
mod global_gpu_data;
mod global_sdf;
mod gpu_scene_upload;
mod gpu_types;
mod hzb;
mod ibl;
mod instancing;
mod lighting;
mod nested_scopes;
mod overlay;
mod page_payload;
mod page_residency;
mod persistent_gpu_scene;
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
mod rt_cluster;
mod rt_ptlas;
mod scene_pass;
mod shader_artifact;
mod shm_publish;
mod skinning;
#[cfg(test)]
mod spatial_numeric;
mod ssao;
mod stars;
mod swapchain;
mod tessellation;
mod thin_sheet;
mod thumbnail;
mod transient;
mod upload;
mod view_target;
mod visibility;
mod vk_nv_cluster;
mod vk_nv_ptlas;
mod vsm;
mod watchdog;

pub use aa::{
    Aa, MOTION_FORMAT, MotionPush, REACTIVE_FORMAT, TAA_JITTER_PHASES, TaaParams, TaaPush,
    clamp_sample_count, jitter_offset, jitter_phase_count,
};
pub use canonical_coverage::*;
pub use clouds::{CloudRenderSettings, Clouds};
pub use compute_dispatch::{
    ComputeBuffer, ComputeDispatch, ComputeDispatchAbort, ComputeDispatchLimits,
    ComputeDispatchOutcome, compute_dispatch_limits,
};
pub use conformance::{
    ShaderArtifactEvidence, SpatialNumericEvidence, ValidationEvidence, VulkanProfileEvidence,
    capture_spatial_numeric, shader_artifact_evidence, vulkan_profile_evidence,
};
pub use count_scan_scatter::{
    CountScanScatterError, CountScanScatterOutcome, CountScanScatterOverflow, CountScanScatterPlan,
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
pub use device::{
    Capabilities, Device, ProfilerFacts, SurfaceSource, VulkanDeviceIdentity,
    validation_issue_count,
};
pub use draw_list::{
    AggregateMaterialMoments, CoverageSourceKind, DeformedRtInstance, MorphDispatch, RenderStats,
    SceneDrawList, SkinDispatch, SkinnedDeformation, SubmeshMaterial, TessDraw, TessRtSlice,
    TessSceneDraw, ThinSheetMaterial, ThinSheetNormalMode, normal_matrix,
};
pub use frame::MAX_FRAMES_IN_FLIGHT;
pub use frame_history::{
    ALARM_EVENT_RING_CAPACITY, ALARM_RESUME_SETTLE_FRAMES, ActiveAlarm, AlarmDrain, AlarmEvent,
    AlarmEventKind, AlarmInputs, AlarmSeverity, AlarmState, FRAME_HISTORY_CAPACITY, FrameHistory,
    FrameHistoryStats, FrameSample, OwnedBudgetBreach, PerfConfig,
};
pub use froxel_fog::{
    AP_FAR_M, AP_GRID, AerialParamsUbo, AerialPerspective, FOG_SHAPE_BOX, FOG_SHAPE_SPHERE,
    FROXEL_FAR, FROXEL_FORMAT, FROXEL_GRID_X, FROXEL_GRID_Y, FROXEL_GRID_Z, FogGridParams,
    FogVolumeGpu, FogVolumeUpload, FroxelFog, FroxelQuality, MAX_FOG_VOLUMES, ap_slice_view_z,
    froxel_slice_view_z, froxel_to_cluster,
};
pub use global_gpu_data::ExecutorShaderRegistry;
pub use global_gpu_data::{
    ClusterArena, CoverageTable, DeformationParameterArena, DeformationProviderArena, FieldArena,
    FrameUploadRing, GLOBAL_GPU_DATA_ABI_VERSION, GPU_ASSEMBLY_NO_USE,
    GPU_DEFORMATION_PROVIDER_DISPLACEMENT, GPU_DEFORMATION_PROVIDER_INTERACTION,
    GPU_DEFORMATION_PROVIDER_MORPH, GPU_DEFORMATION_PROVIDER_SKINNING,
    GPU_DEFORMATION_PROVIDER_WIND, GPU_INTERACTION_CASCADES, GPU_INTERACTION_FIELD_BYTES,
    GPU_INTERACTION_HEADER_SIZE, GPU_INTERACTION_TEXEL_SIZE, GPU_INTERACTION_TEXELS,
    GPU_MATERIAL_COVERAGE_SHIFT, GPU_MATERIAL_SIDEDNESS_SHIFT, GPU_MATERIAL_SURFACE_MODEL_SHIFT,
    GPU_MATERIAL_TABLE_FLAG_TESSELLATED, GPU_MATERIAL_TRANSPARENCY_SHIFT,
    GPU_PAGE_FLAG_GUARANTEED_ROOT, GPU_PSO_COVERAGE_SHIFT, GPU_PSO_DEFORMATION_SHIFT,
    GPU_PSO_MATERIAL_SHIFT, GPU_PSO_PASS_SHIFT, GPU_PSO_REPRESENTATION_SHIFT,
    GPU_PSO_SIDEDNESS_SHIFT, GPU_PSO_SURFACE_MODEL_SHIFT, GPU_PSO_TRANSPARENCY_SHIFT,
    GPU_SCENE_INSTANCE_FLAG_ATTACHED, GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS,
    GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD, GPU_SCENE_INSTANCE_FLAG_WIND,
    GPU_SCENE_INSTANCE_POLICY_SHIFT, GPU_SCENE_TRANSFORM_DYNAMIC, GPU_SCENE_TRANSFORM_STATIC,
    GPU_TRANSITION_FRAMES, GeometryTable, GlobalGpuArena, GlobalGpuData, GlobalGpuTableDescriptors,
    GlobalGpuTableKind, GpuArenaGrowth, GpuArenaRange, GpuAssemblyHeaderRecord,
    GpuAssemblyPrototypeRecord, GpuAssemblyUseRecord, GpuBufferUpload, GpuCoverageRecord,
    GpuDeformation, GpuDeformationProviderRecord, GpuDrawRecord, GpuFieldDirectoryEntry,
    GpuFieldTileRecord, GpuGeometryRecord, GpuHandle, GpuInverseBindRecord, GpuMaterialClass,
    GpuMaterialTableRecord, GpuMicroCandidate, GpuPageRecord, GpuPassClass, GpuPrototypeRecord,
    GpuPsoBin, GpuRangeAllocator, GpuRecordRetirement, GpuRepresentation,
    GpuSceneInstanceGpuRecord, GpuSceneLightGpuRecord, GpuSceneOverrideGpuRecord,
    GpuScenePageGpuRecord, GpuScenePrototypeGpuRecord, GpuSceneReferenceGpuRecord,
    GpuSdfTableRecord, GpuSidedness, GpuSkeletonJointRecord, GpuSkeletonRecord, GpuSubmeshRecord,
    GpuTableDescriptor, GpuTableSlotHeader, GpuTextureTableRecord, GpuTransparency,
    GpuWindInstanceRecord, ImmutableGpuTable, IndexArena, InverseBindArena,
    MICRO_BLADE_INDEX_COUNT, MICRO_BLADE_VERTEX_COUNT, MaterialParameterArena, MaterialTable,
    PageArena, PageDependencyArena, PageTable, PartArena, PrototypeMaterialArena, PrototypeTable,
    ResidentGpuTable, SceneDeformationTable, SceneInstanceTable, SceneLightTable,
    SceneMaterialTable, SceneOverrideArena, ScenePageTable, ScenePrototypeTable, SceneSdfTable,
    SkeletonJointArena, SkeletonTable, SubmeshArena, TextureTable, UploadSlice, VertexArena,
    VoxelArena, micro_blade_template_indices,
};
pub use global_sdf::{
    GDF_BAND_FRACTION, GDF_CASCADE0_EXTENT, GDF_CASCADES, GDF_EXPONENT, GDF_FORMAT, GDF_MAX_CULLED,
    GDF_NEAR_HANDOFF, GDF_RES, GdfCompositePush, GdfCullPush, GdfParamsUbo, GdfRegion, GlobalSdf,
    gi_occluder_bounds,
};
pub use gpu_scene_upload::{
    GpuArenaUploadRequest, GpuSceneAddressBlock, GpuScenePendingUploads, GpuSceneTableDescriptors,
    GpuSceneTableStorage, GpuSceneUploadRunStats, GpuSceneUploader, GpuSceneWorldDescriptors,
    GpuSceneWorldTables, PAGE_REQUEST_CAPACITY, PageRequestDrain, record_pending_global_uploads,
};
pub use gpu_types::{GpuLight, InstanceData, Material, MaterialParamsData, SdfInstance};
pub use hzb::{HZB_MAX_MIPS, HZB_PUSH_SIZE, Hzb, HzbPyramid};
pub use ibl::{
    ATMOS_MULTI_SCATTER_SIZE, ATMOS_SKY_VIEW_H, ATMOS_SKY_VIEW_W, ATMOS_TRANSMITTANCE_H,
    ATMOS_TRANSMITTANCE_W, AtmosphereParams, EnvSource, IBL_COLOR_FORMAT, IBL_ENV_SIZE,
    IBL_LUT_SIZE, IBL_PREFILTER_MIPS, IBL_PREFILTER_SIZE, Ibl, NightSkyParams, ProbeMetaGpu,
    ReflectionProbe, ReflectionProbeUpload, ReflectionProbes, SKY_SH_COEFFICIENTS, Sky, SkyDraw,
    SkyRenderSettings, SkygenParams, record_sky,
};
pub use instancing::{
    DeformationGather, DeformationWork, Instancing, TessGatherParams, displace_info_from,
    gather_instance_deformation, resolve_material_params,
};
pub use lighting::{
    CLUSTER_COUNT, CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, ClusterCamera, ClusterParams,
    LightUbo, Lighting, MAX_LIGHTS_PER_CLUSTER, SceneLighting, SceneWind, cull_clusters_cpu,
    point_shadow_face_matrices,
};
pub use overlay::{
    BloomPush, ColorGrade, GradeRange, GradeUniform, GridPush, LUT_BAKE_SIZE, LUT_SHAPER_EV_MAX,
    LUT_SHAPER_EV_MIN, OverlayDraw, OverlayState, OverlayVertex, TonemapMode, TonemapPush,
    record_grid, record_overlay,
};
pub use page_payload::{
    GPU_PAGE_NODE_NO_PROTOTYPE, GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT, GpuPageClusterRecord,
    GpuPageNodeRecord, GpuPageVoxelVertex, PagePayload, build_page_payload,
};
pub use page_residency::{
    PAGE_DEMAND_PREDICTED_CEILING, PageDemandView, PageResidency, PageResidencyBudgets,
    PageResidencyStats,
};
pub use persistent_gpu_scene::*;
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
    ProfileRecorders, RenderGraph, RgAccess, RgAttachment, RgBatchCommandBuffers, RgBufferDesc,
    RgBufferLifetime, RgBufferRange, RgBufferRangeError, RgBufferResource, RgExternalBufferState,
    RgExternalState, RgPass, RgPassBarriers, RgPassBatch, RgPassKind, RgQueueAssignment,
    RgQueueFamilies, RgQueuePreference, RgRecordedBatch, RgResource, RgSubmissionPlan, RgUsage,
};
pub use renderer::{FogRenderSettings, RenderStatsFull, Renderer, VIEW_COUNT, ViewId, ViewMode};
pub use resources::{
    AccelerationStructure, BindlessFreeList, Buffer, DefaultHeightMinMax, DeviceResources, GpuLut,
    GpuMesh, GpuMeshParts, GpuSdf, GpuSdfParts, GpuTexture, GpuTextureParts, Image, Image3D,
    ImageDesc, MeshAssembly, Micromap, MinMaxPyramid, Pipeline, RtBlas,
};
pub use restir::{
    InitialPush as RestirInitialPush, RESTIR_CANDIDATE_COUNT, RESTIR_INITIAL_PUSH_SIZE,
    RESTIR_MAX_M, RESTIR_RADIANCE_FORMAT, RESTIR_RESOLVE_PUSH_SIZE, RESTIR_REUSE_PUSH_SIZE,
    RESTIR_SPATIAL_RADIUS, Reservoir, ResolvePush as RestirResolvePush, Restir, RestirView,
    ReusePush as RestirReusePush, reservoir_bytes as restir_reservoir_bytes, wants_restir,
};
pub use rt::{
    BlasRefitOp, MeshBlasBuild, MeshBlasGeometry, RT_UNMIRRORED_INSTANCE, Rt, RtCutView,
    RtInstanceInput, RtScene, TlasBuildOp, TlasBuildPlan, record_blas_compaction,
    record_mesh_blas_build, record_micromap_build, record_tlas_build_plan,
};
pub use rt_cluster::ClusterBlas;
pub use scene_pass::{
    MeshPassSets, record_executor_buckets, record_executor_depth_family,
    record_executor_transparent_stream, record_tess_depth_draws, record_tess_scene_draws,
};
pub use shader_artifact::{
    ShaderArtifactContract, ShaderArtifactError, ShaderArtifactIdentity, ShaderSha256,
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
pub use stars::{StarCatalog, StarDraw, record_stars};
pub use swapchain::Swapchain;
pub use tessellation::{
    TESS_CLAS_MAX_TRIS, TESS_CLAS_MAX_VERTS, TESS_DEFAULT_EDGE_LENGTH_TARGET,
    TESS_DEFAULT_FACTOR_CAP, TESS_DEFAULT_MIN_FACTOR, TESS_MAX_DICE_FACTOR, TESS_MAX_INSTANCES,
    TESS_MICRO_VERTEX_BUDGET, TessBucket, TessCamera, TessEmitPush, TessFactorPush,
    TessFinalizePush, TessInstanceLayout, TessScanPush, Tessellation, budget_scaled_caps,
    coarse_parent_bary, displacement_aware_factor, factor_push, geomorph_weight,
    project_world_to_pixels, smoothstep01, split_recursion, tess_worst_case, wire_storage_set,
};
pub use thin_sheet::{ThinSheetEnergyPartition, thin_sheet_energy_partition};
pub use thumbnail::{
    PngTransfer, ThumbnailPng, convert_to_rgb, encode_to_png, format_pixel_bytes, write_png_file,
};
pub use transient::{FROXEL_VOLUME_KEYS, RenderGraphResources};
pub use upload::{GpuQueue, SdfBake, SdfSource, TextureMipLevel, Uploader};
pub use view_target::ViewTarget;
pub use visibility::{
    ExecutorBucket, ExecutorDrawInputs, GpuWindSourceRecord, InteractionImpulse,
    MESH_TASK_COMMAND_STRIDE, MESH_TRIANGLES_PER_GROUP, SCENE_BUCKET_PRESSURE,
    SCENE_EXECUTOR_BUCKET_CAPACITY, SCENE_MICRO_CANDIDATE_CAPACITY, SCENE_MICRO_FIELD_PUSH_SIZE,
    SCENE_MICRO_TEXEL_BUDGET, SCENE_RADIX_WORKGROUP, SCENE_TRANSITION_PRESSURE,
    SCENE_TRANSITION_STATE_CAPACITY, SCENE_TRANSPARENT_OVERFLOW, SCENE_TRAVERSAL_OVERFLOW_RECORDS,
    SCENE_TRAVERSAL_PUSH_SIZE, SCENE_VISIBILITY_COUNTER_BINS,
    SCENE_VISIBILITY_COUNTER_COVERED_SAMPLES, SCENE_VISIBILITY_COUNTER_CULLED_FRUSTUM,
    SCENE_VISIBILITY_COUNTER_CULLED_NODES, SCENE_VISIBILITY_COUNTER_CULLED_OCCLUSION,
    SCENE_VISIBILITY_COUNTER_DEFORMED, SCENE_VISIBILITY_COUNTER_INTERACTION_RESET,
    SCENE_VISIBILITY_COUNTER_MAX_CUT_DEPTH, SCENE_VISIBILITY_COUNTER_MICRO_CANDIDATES,
    SCENE_VISIBILITY_COUNTER_OVERFLOW, SCENE_VISIBILITY_COUNTER_RECORD_OVERFLOW,
    SCENE_VISIBILITY_COUNTER_RECORDS, SCENE_VISIBILITY_COUNTER_RETEST,
    SCENE_VISIBILITY_COUNTER_SUB_QUAD_TRIANGLES, SCENE_VISIBILITY_COUNTER_TRANSITIONING,
    SCENE_VISIBILITY_COUNTER_TRANSPARENT, SCENE_VISIBILITY_COUNTER_TRIANGLES,
    SCENE_VISIBILITY_COUNTER_VISIBLE, SCENE_VISIBILITY_COUNTER_VISITED_NODES,
    SCENE_VISIBILITY_COUNTER_VOXEL_RECORDS, SCENE_VISIBILITY_COUNTER_WORDS,
    SCENE_VISIBILITY_OVERFLOW_RETEST, SCENE_VISIBILITY_OVERFLOW_VISIBLE,
    SCENE_VISIBILITY_PASS_CULL, SCENE_VISIBILITY_PASS_RETEST, SCENE_VISIBILITY_PUSH_SIZE,
    SCENE_VISIBILITY_RECORD_CAPACITY, SceneMicroFieldPush, SceneTraversalPush, SceneVisibility,
    SceneVisibilityPush, SceneVisibilityView, TransparentSortPipelines, WIND_DEFORM_PUSH_SIZE,
    WIND_INTERACT_PUSH_SIZE, WindDeformPush, WindInteractPush, bucket_material,
    build_executor_buckets, record_executor_bucket_draw, record_executor_bucket_draw_mesh,
    record_executor_mesh_prefix, record_executor_pass_prefix,
};
pub use visibility::{
    ExecutorBucketDraw, GiOccluderScatterPush, SCENE_CUT_AUTO, SCENE_CUT_FORCE_COARSE,
    SCENE_CUT_FORCE_FINE, SCENE_ERROR_THRESHOLD_GI_PX, SCENE_ERROR_THRESHOLD_IMAGE_PX,
    SCENE_VIEW_CLASSES, SCENE_VISIBILITY_COUNTER_CULLED_REACH, SCENE_VISIBILITY_PASS_REACH,
    SceneViewClass, TraversalTuning,
};

/// Bytes of the mesh executor's push block: the view-projection matrix plus the bucket's
/// command-slice base, which `SV_DrawIndex` is relative to.
pub const MESH_EXECUTOR_PUSH_SIZE: u32 = 68;
pub use vsm::{
    VSM_ATLAS_SIZE, VSM_ATLAS_TILES, VSM_COMPACT_PUSH_SIZE, VSM_DEFAULT_PAGE_BUDGET,
    VSM_DEMAND_CAPACITY, VSM_DEMAND_PUSH_SIZE, VSM_DIRECTIONAL_LEVELS, VSM_LEVEL_PAGES,
    VSM_LEVEL0_EXTENT_M, VSM_PAGE_SIZE, VSM_TABLE_RESIDENT, VsmCompactPush, VsmCounters, VsmDemand,
    VsmDemandPush, VsmDirectionalSpace, VsmPageKey, VsmRenderPage, VsmResidency, vsm_table_entry,
};

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

    /// The acquire-to-present state machine received an operation out of order.
    #[error("invalid present state: {0}")]
    PresentState(&'static str),

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

    /// A CPU upload payload does not match the declared GPU resource shape.
    #[error("invalid upload data: {0}")]
    InvalidUploadData(String),

    /// A SPIR-V shader module could not be read or is malformed (size not a
    /// multiple of 4, or unreadable).
    #[error("shader load failed: {0}")]
    ShaderLoad(String),

    /// A generated shader artifact or its compiler/source manifest is missing or stale.
    #[error(transparent)]
    ShaderArtifact(#[from] ShaderArtifactError),

    /// A persistent GPU-scene delta or upload contract was invalid.
    #[error(transparent)]
    GpuScene(#[from] GpuSceneError),

    /// The GPU signed-distance-field bake could not run or its sidecar was malformed
    /// (no bake pipelines, or a decode/IO failure on the cache).
    #[error("sdf bake failed: {0}")]
    SdfBake(String),

    /// A bindless array (albedo textures or per-mesh SDF fields) is full — every slot up
    /// to its capacity is occupied — so the upload was skipped rather than writing an
    /// out-of-range `dstArrayElement`.
    #[error("bindless array full: {0}")]
    BindlessFull(&'static str),

    /// The creative-look bake could not run (its compute PSO was unavailable, or a Vulkan/VMA call
    /// on the one-off dispatch/readback failed).
    #[error("look bake failed: {0}")]
    LutBake(String),

    /// A per-queue timeline semaphore exhausted its monotonic value space.
    #[error("render-graph timeline semaphore value overflowed")]
    TimelineValueOverflow,
}

impl Error {
    /// Whether this is a Vulkan `ERROR_DEVICE_LOST` — the paths that observe one report the
    /// device's last-reached diagnostic checkpoints before propagating.
    #[must_use]
    pub fn is_device_loss(&self) -> bool {
        matches!(
            self,
            Self::Vk {
                result: vk::Result::ERROR_DEVICE_LOST,
                ..
            }
        )
    }
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
