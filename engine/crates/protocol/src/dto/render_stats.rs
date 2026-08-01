use crate::{
    AaModeDto, AnamorphicParams, CreativeLutStat, ProfilerModeDto, SetColorGradingParams, Uuid,
    ViewModeDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One frame's virtual-shadow residency activity (page counts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VsmStatsDto {
    /// Pages demanded (receiver requests + bootstrap).
    pub requested: i32,
    /// Demands answered by an already-resident page.
    pub hits: i32,
    /// Fresh page-to-tile allocations.
    pub allocated: i32,
    /// Pages the frame rasterized.
    pub rendered: i32,
    /// Resident pages re-marked dirty.
    pub dirtied: i32,
    /// LRU evictions.
    pub evicted: i32,
    /// Demands the full atlas could not satisfy.
    pub overflow: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderStatsDto {
    pub draw_calls: i32,
    pub batches: i32,
    pub instances: i32,
    /// CPU time spent deriving this frame's deformation work and ray instances.
    pub scene_gather_ms: f32,
    /// Instances that derivation touched this frame — (near-)zero on a steady scene, and
    /// independent of how many instances the scene holds.
    pub scene_gather_entities: i32,
    /// GPU-scene table bytes staged this frame — (near-)zero on a steady scene.
    pub instance_upload_bytes: u64,
    /// Host bytes retained by unique drawn meshes for exact surface queries.
    pub retained_mesh_cpu_bytes: u64,
    /// Indirect draw invocations recorded across the frame's virtual-shadow pages.
    pub shadow_draw_calls: i32,
    /// Whether the device exposes an independent compute queue family, which is what lets a
    /// pass that prefers the async lane actually take it.
    pub async_compute_queue: bool,
    /// Command buffers the frame's render graph submitted on that independent compute queue.
    /// Zero where the device has no such family — the same passes then run on graphics.
    pub async_compute_batches: u32,
    /// Virtual-shadow residency activity (requests, allocations, evictions, …).
    pub vsm: VsmStatsDto,
    /// Instances published into the active frame TLAS.
    pub rt_instances: i32,
    /// TLAS instances placed through the aggregate-representation structure — a family
    /// packed as one coarse instance instead of its per-use expansion.
    pub rt_aggregate_instances: i32,
    /// TLAS instances whose identity record names a live GPU-scene slot, so a non-opaque
    /// candidate on them runs through the canonical coverage classifier. The rest commit
    /// unconditionally — conservative, but not coverage agreement.
    pub rt_resolvable_instances: i32,
    pub frame_ms: f32,
    pub fps: f32,
    pub gpu_ms: f32,
    pub cpu_frame_ms: f32,
    pub gpu_frame_ms: f32,
    pub cpu_wait_ms: f32,
    pub triangles: i32,
    pub descriptor_binds: i32,
    pub command_buffers: i32,
    pub queue_submits: i32,
    pub pipelines_created: i32,
    pub vram_usage_bytes: u64,
    pub vram_budget_bytes: u64,
    pub software_gpu: bool,
    pub profiler_mode: ProfilerModeDto,
    pub clustered: bool,
    pub depth_prepass: bool,
    pub shadows: bool,
    pub ibl: bool,
    pub ssao: bool,
    pub contact_shadows: bool,
    pub ssgi: bool,
    /// The active view's dynamic-resolution factor (`(0, 1]`; `1.0` = native). The frame-budget
    /// controller lowers it below the `Low` tier floor to hold the budget; the present blit upscales.
    pub render_scale: f32,
    /// Whether the Global SDF reflection-occlusion cone is occluding the reflected skybox
    /// (specular only; indirect diffuse occlusion is DDGI ray-miss + contact GTAO).
    pub sky_occlusion: bool,
    /// The active render-quality tier (`low`/`medium`/`high`/`ultra`/`custom`) — the knob the
    /// `ssao`/`contact_shadows`/`ssgi` flags above derive from.
    pub quality: String,
    /// The active tonemap operator (`reinhard`/`aces`/`agx`/`pbr-neutral`).
    pub tonemap: String,
    /// The reactive loop is idling (skipping renders) — a static, converged, or hidden viewport.
    pub idle: bool,
    /// The temporal effects (TAA / SSGI history) have converged to their final image.
    pub converged: bool,
    /// The reasons continuous render is currently held (empty when idle), for the stats readout.
    pub redraw_reasons: Vec<String>,
    /// The editor viewport power state (`focused`/`unfocused`/`occluded`).
    pub power_state: String,
    pub ddgi: bool,
    /// Whether the Global Distance Field (the camera-centered cascade clipmap) backs the far-field
    /// cone-march tap.
    pub gdf: bool,
    pub rt_supported: bool,
    pub rt_shadows: bool,
    pub restir: bool,
    pub ssr: bool,
    pub rt_reflections: bool,
    /// Whether `VK_EXT_mesh_shader` is enabled, making the mesh executor reachable on this device.
    pub mesh_shader: bool,
    /// Whether the shaded executor is running through the mesh stage.
    pub mesh_executor: bool,
    /// Occluders dropped from this frame's SDF list for want of capacity; nonzero means the
    /// global-illumination inputs are incomplete.
    pub sdf_instances_dropped: i32,
    /// Occluders the distance-field cascade window excluded this frame, which no march can reach.
    pub sdf_instances_culled: i32,
    /// Ray instances excluded because they sit outside the window a GI or reflection ray reaches.
    pub rt_instances_culled: i32,
    /// Whether `VK_EXT_opacity_micromap` is enabled on this device.
    pub omm_supported: bool,
    pub blas_count: i32,
    /// Skinned refit structures active this frame, as opposed to the static builds in `blasCount`.
    pub skinned_blas_count: i32,
    /// Tessellated structures active this frame; variable topology forbids an in-place refit.
    pub tessellated_blas_count: i32,
    /// Placed uses whose wind-deformed geometry this frame materialized into the deformed arena
    /// for their bottom-level structures, so ray shadows and reflections show the pose every
    /// raster pass draws instead of the rest pose.
    pub wind_deformed_instances: i32,
    /// Whether `VK_NV_cluster_acceleration_structure` is enabled: an assembly prototype's
    /// bottom-level structure then composes from its cooked triangle clusters.
    pub cluster_as_supported: bool,
    /// Distinct cluster-composed bottom-level structures referenced this frame,
    /// deduplicated by device address so shared structures are counted once.
    pub cluster_blas_count: i32,
    /// Cluster acceleration structures those bottom levels compose.
    pub clas_count: i32,
    /// Whether the top-level structure is partitioned: instances live in partitions and a
    /// frame rewrites only what changed, rather than the table being rebuilt whole.
    pub ptlas_supported: bool,
    /// Partitions this frame's instances occupy. Zero where the top level is the KHR TLAS.
    pub ptlas_partitions: i32,
    /// Instances the frame placed whole — appeared, or moved.
    pub ptlas_writes: i32,
    /// Instances whose structure address changed under an unmoved placement, the cheaper op.
    pub ptlas_updates: i32,
    /// Session-cumulative GPU microseconds in acceleration-structure builds outside the render
    /// graph — the static build and its compaction, on the uploader's private pool.
    pub accel_build_us: String,
    /// Distinct opacity micromaps this frame's structures reference, deduplicated by handle so
    /// a micromap shared across instances is charged once.
    pub omm_micromaps: i32,
    /// Micro-triangles a derivation proved wholly covered, so traversal commits without the
    /// coverage classifier.
    pub omm_opaque: String,
    /// Micro-triangles proved wholly cut out, so traversal rejects without the classifier.
    pub omm_transparent: String,
    /// Micro-triangles left unresolved, where the coverage classifier still runs.
    pub omm_unknown: String,
    /// Cooked opacity micromaps uploaded meshes carried this session, counted from the cooked
    /// hierarchy before any device gate — so it reports what the derivation produced even where
    /// the extension is absent and nothing can be attached.
    pub omm_derived_micromaps: i32,
    /// Micro-triangles that derivation proved wholly covered.
    pub omm_derived_opaque: String,
    /// Micro-triangles that derivation proved wholly cut out.
    pub omm_derived_transparent: String,
    /// Micro-triangles that derivation left unresolved.
    pub omm_derived_unknown: String,
    /// AS-storage bytes the distinct bottom-level structures occupy, deduplicated by device
    /// address so shared structures are charged once.
    pub blas_bytes: String,
    /// What those structures would occupy uncompacted; the gap to `blasBytes` is what compaction saved.
    pub blas_built_bytes: String,
    /// AS-storage bytes this frame's top-level structure occupies.
    pub tlas_bytes: String,
    /// Build-scratch bytes held for this frame's structure builds; grow-only and shared.
    pub rt_scratch_bytes: String,
    pub pipelines: i32,
    pub bindless_textures: i32,
    pub bindless_free: i32,
    pub hdr: bool,
    pub exposure_ev: f32,
    /// The scene-linear color grade folded into the tonemap pass (the panel reads live grade state
    /// from here, not the `set-color-grading` echo).
    pub color_grading: SetColorGradingParams,
    /// The resolved creative look-up table state (asset + intensity + size), `None` when no look is
    /// assigned. The panel's size/interp readout resolves from here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creative_lut: Option<CreativeLutStat>,
    /// Whether the pre-tonemap scene-linear bloom pyramid is enabled.
    pub bloom_enabled: bool,
    /// The energy-conserving bloom composite weight.
    pub bloom_intensity: f32,
    /// The bloom tent-upsample scatter radius (UV units).
    pub bloom_scatter: f32,
    /// The bloom tint (multiplies the composited bloom).
    pub bloom_tint: [f32; 3],
    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    pub bloom_threshold: f32,
    /// The lens-dirt mask asset id (`0` = none).
    pub bloom_dirt_texture: Uuid,
    /// The lens-dirt mix fraction.
    pub bloom_dirt_intensity: f32,
    /// The lens-dirt tint.
    pub bloom_dirt_tint: [f32; 3],
    /// The anamorphic streak block (a horizontally-squeezed blur added over the radial bloom).
    pub bloom_anamorphic: AnamorphicParams,
    /// The per-upsample-step tint stack (empty when off).
    pub bloom_per_mip_tint: Vec<[f32; 3]>,
    pub aa: AaModeDto,
    pub view_mode: ViewModeDto,
}

/// Population and rebuild counters for the journal-driven persistent GPU-scene mirror.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GpuSceneMirrorStatsDto {
    /// Why the active view's temporal history was last invalidated.
    pub history_invalidation: String,
    /// Mirrored mesh assets (prototypes).
    pub meshes: u32,
    /// Interned resolved material variants.
    pub materials: u32,
    /// Interned texture-table records.
    pub textures: u32,
    /// Instances across every synced world.
    pub instances: u32,
    /// Punctual lights across every synced world.
    pub lights: u32,
    /// Entities whose referenced mesh is currently unresolvable.
    pub unresolved_instances: u32,
    /// Host bytes the mirrored meshes retain for surface queries.
    pub retained_mesh_bytes: u64,
    /// Complete shared-record rebuilds (asset journal overflow or catalog replacement).
    pub shared_rebuilds: u64,
    /// Complete world rebuilds (scene journal overflow or a rebound scene instance).
    pub world_rebuilds: u64,
    /// Cooked density upper bound of micro blade candidates across the resident-tile
    /// directory; the per-frame generated count never exceeds it.
    pub micro_predicted: u64,
    /// Hierarchy page-payload residency counters.
    pub page_residency: PageResidencyStatsDto,
    /// GPU visibility counters from the latest completed frame.
    pub visibility: SceneVisibilityStatsDto,
}

/// GPU instance-visibility counters for one completed frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SceneVisibilityStatsDto {
    /// Instances on the visible list.
    pub visible: u32,
    /// Occlusion-retested instances.
    pub retested: u32,
    /// Semantic draw records emitted by the traversal.
    pub records: u32,
    /// Back-to-front transparent draws.
    pub transparent: u32,
    /// Generated micro-blade candidates.
    pub micro_candidates: u32,
    /// Records mid representation-crossfade.
    pub transitioning: u32,
    /// Aggregate-voxel records on the cut.
    pub voxel_records: u32,
    /// The deepest hierarchy level on the emitted cut.
    pub max_cut_depth: u32,
    /// Instances the frustum culled.
    pub culled_frustum: u32,
    /// Instances the occlusion retest kept hidden.
    pub culled_occlusion: u32,
    /// Emitted triangles whose whole record projects under one 2x2 quad.
    pub sub_quad_triangles: u32,
    /// Hierarchy nodes the traversal reached with a resolved assembly use.
    pub visited_nodes: u32,
    /// Executor buckets that received a record — the indirect draws the frame issues.
    pub bins: u32,
    /// Deformed instances the view composed bounds for.
    pub deformed: u32,
    /// Deformed instances the interaction field's re-centring scroll has reset, counted since boot
    /// rather than per frame.
    pub interaction_resets: u64,
    /// Samples the geometry fragment shaders covered, counted only while the profiler is armed
    /// (zero otherwise). Over `fragmentInvocations` from the same capture it is quad utilization.
    pub covered_samples: u32,
    /// Hierarchy nodes rejected on their swept world bounds, each dropping the subtree beneath it.
    pub culled_nodes: u32,
    /// Triangle clusters on a surviving node rejected on their own swept world bounds — the
    /// per-part granularity beneath `culledNodes`.
    pub culled_clusters: u32,
    /// Instances the global-illumination reach view kept: those a march or reflection ray can
    /// read, whether or not the camera sees them. Zero while the distance field is off.
    pub gi_reach_visible: u32,
    /// Instances the reach view rejected as unreachable.
    pub gi_reach_culled: u32,
    /// List overflow flags (visible/retest).
    pub overflow_flags: u32,
    /// Record-stream and draw-bucket pressure flags.
    pub pressure_flags: u32,
}

/// Hierarchy page-payload residency counters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageResidencyStatsDto {
    /// Registered pages.
    pub registered: u64,
    /// Pages whose payload is resident.
    pub resident: u64,
    /// Resident payload bytes.
    pub resident_bytes: u64,
    /// Resident byte budget.
    pub budget_bytes: u64,
    /// Pages awaiting the load worker.
    pub requested: u64,
    /// Pages at the load worker.
    pub loading: u64,
    /// Loaded pages awaiting publication.
    pub ready: u64,
    /// Cumulative evictions.
    pub evictions: u64,
    /// Pages that went from requested to resident, which is what a fault costs.
    pub faults: u64,
    /// Microseconds those faults took, summed; over `faults` it is the mean fault latency.
    pub fault_latency_us: u64,
    /// Missing-page requests no request region had room for, counted since boot rather than per
    /// frame. A dropped request is latency, not lost geometry: the page faults again next frame.
    pub requests_dropped: u64,
    /// Bit per view class whose request region has filled since boot (camera 1, shadow 2, GI 4).
    pub request_overflow_classes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderPassTimingDto {
    pub name: String,
    pub gpu_ms: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderPassTimingsDto {
    pub passes: Vec<RenderPassTimingDto>,
    pub gpu_total_ms: f32,
    pub software_gpu: bool,
    pub profiler_mode: ProfilerModeDto,
}
