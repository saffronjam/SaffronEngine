//! The per-frame scene draw list: the inputs ([`DrawItem`] + [`SubmeshMaterial`]),
//! the batched output ([`DrawBatch`] / [`SceneDrawList`]), and the [`RenderStats`]
//! counters.
//!
//! [`crate::Instancing::submit_draw_list`] resolves each item's material to a cached
//! PSO, buckets by (pipeline, mesh) into instanced draws, deduplicates the per-frame
//! material table, and produces the [`SceneDrawList`] the scene + depth passes record.
//!
//! Skinned items carry a joint palette: [`crate::Instancing::submit_draw_list`] deforms
//! each into its slice of the frame's deformed-vertex buffer (the [`SkinDispatch`] the
//! `skin` compute pass replays), then draws it as a static instance reading that slice.
//! The [`DeformedRtInstance`] list rides for the RT refit BLAS.

use std::sync::Arc;

use ash::vk;
use saffron_core::{BlendMode, HeightMode};
use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};
use saffron_vegetation::AlphaClassification;

use crate::gpu_types::Material;
use crate::resources::{GpuMesh, GpuTexture, Pipeline};

/// Canonical coverage source sampled by every foliage raster path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum CoverageSourceKind {
    /// Alpha from the material's base-color texture.
    #[default]
    AlbedoAlpha = 0,
    /// Alpha from a dedicated coverage texture.
    Texture = 1,
    /// The modeled silhouette is fully covered.
    ModeledGeometry = 2,
}

/// Normal orientation policy for the two faces of a thin sheet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ThinSheetNormalMode {
    /// Keep the authored geometric/tangent orientation on both faces.
    Preserve = 0,
    /// Flip the back face toward the observer while retaining tangent detail.
    #[default]
    FaceForwardBack = 1,
    /// Use an observer-facing symmetric lobe on both faces.
    Symmetric = 2,
}

/// Coverage/material statistics consumed by aggregate virtual-geometry clusters.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AggregateMaterialMoments {
    /// Occupied projected-area fraction.
    pub occupancy: f32,
    /// Coverage-weighted albedo mean.
    pub albedo_mean: Vec3,
    /// Coverage-weighted roughness mean.
    pub roughness_mean: f32,
    /// Coverage-weighted transmitted-energy mean.
    pub transmission_mean: Vec3,
    /// Coverage-weighted physical thickness mean in metres.
    pub thickness_mean: f32,
    /// Normal second moments in XX/YY/ZZ/XY/XZ/YZ order.
    pub normal_second_moments: [f32; 6],
}

/// Render-ready optical and coverage parameters for one thin foliage sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThinSheetMaterial {
    /// Front-face reflected albedo response.
    pub front_albedo_response: f32,
    /// Back-face reflected albedo response.
    pub back_albedo_response: f32,
    /// Physical sheet thickness in metres.
    pub thickness: f32,
    /// Beer-Lambert absorption coefficients.
    pub absorption: Vec3,
    /// Transmitted-light tint.
    pub transmission: Vec3,
    /// Thin-sheet reflection/transmission lobe roughness.
    pub roughness: f32,
    /// Two-sided shading-normal policy.
    pub normal_mode: ThinSheetNormalMode,
    /// Where canonical coverage alpha comes from.
    pub coverage_source: CoverageSourceKind,
    /// How sampled alpha is classified.
    pub coverage_classification: AlphaClassification,
    /// Stable object-space stochastic-coverage salt.
    pub coverage_hash_salt: u64,
    /// Source dimensions used to anchor stochastic coverage to texels.
    pub coverage_source_extent: [u32; 2],
    /// Upper bound for reflected plus transmitted energy.
    pub energy_limit: f32,
    /// Aggregate-cluster material statistics derived from this response.
    pub aggregate: AggregateMaterialMoments,
}

/// One submesh's material: its textures (each `None` → the default white slot) plus
/// the PBR factors that fold into the per-frame [`crate::MaterialParamsData`].
///
/// A [`DrawItem`] carries one per mesh submesh, indexed by `Submesh::material_slot`
/// order; a single entry applies to every submesh (clamped).
#[derive(Clone)]
pub struct SubmeshMaterial {
    /// Base-color / albedo texture (`None` → default white, factors unchanged).
    pub albedo_texture: Option<Arc<GpuTexture>>,
    /// Metallic-roughness / ORM texture (`None` → default white).
    pub metallic_roughness_texture: Option<Arc<GpuTexture>>,
    /// Tangent-space normal map (sets the `NORMAL` feature bit when present).
    pub normal_texture: Option<Arc<GpuTexture>>,
    /// Ambient-occlusion map (AO in R; sets the `OCCLUSION` feature bit).
    pub occlusion_texture: Option<Arc<GpuTexture>>,
    /// Emissive map (modulates the emissive factor; sets `EMISSIVE_TEX`).
    pub emissive_texture: Option<Arc<GpuTexture>>,
    /// Height map (the technique is [`SubmeshMaterial::height_mode`]: `HEIGHT_BUMP` shading bump,
    /// `HEIGHT` parallax, or `DISPLACE` real geometry).
    pub height_texture: Option<Arc<GpuTexture>>,
    /// Vector-displacement map (tangent-space XYZ). When set under [`HeightMode::Displacement`], the
    /// `displace` pre-pass offsets each vertex through its TBN by this field instead of scalar height,
    /// so overhangs/undercuts become real geometry. `None` → scalar displacement along the normal.
    pub vector_displacement_texture: Option<Arc<GpuTexture>>,
    /// Coverage-preserving mip chain for thin-sheet alpha classification.
    pub coverage_texture: Option<Arc<GpuTexture>>,
    /// Base color (RGBA), multiplied with the albedo texture.
    pub base_color: Vec4,
    /// Metallic factor.
    pub metallic: f32,
    /// Roughness factor.
    pub roughness: f32,
    /// Emissive radiance factor.
    pub emissive: Vec3,
    /// Emissive strength multiplier on [`SubmeshMaterial::emissive`].
    pub emissive_strength: f32,
    /// Normal-map strength.
    pub normal_strength: f32,
    /// UV tiling (multiplied into the sampled UV).
    pub uv_tiling: Vec2,
    /// UV offset (added to the tiled UV).
    pub uv_offset: Vec2,
    /// Height scale — parallax march depth ([`HeightMode::Parallax`]), or the OBJECT-space
    /// displacement amplitude ([`HeightMode::Displacement`]): the relief rides the instance's
    /// transform like its geometry, so scaling the object scales the bumps proportionally.
    pub height_scale: f32,
    /// The height-map technique: [`HeightMode::Bump`] (shading bump), [`HeightMode::Parallax`]
    /// (parallax-occlusion mapping), or [`HeightMode::Displacement`] (real displaced geometry via the
    /// adaptive-tessellation passes, which amplify each base triangle into a displaced micro-grid).
    pub height_mode: HeightMode,
    /// Alpha/blend mode: opaque, masked (alpha-clip discard below [`SubmeshMaterial::alpha_cutoff`]),
    /// or translucent (routed to the sorted, blended translucent draw list).
    pub blend_mode: BlendMode,
    /// The alpha-clip cutoff threshold (used by [`BlendMode::Masked`]).
    pub alpha_cutoff: f32,
    /// Two-sided (glTF `doubleSided`): the scene pass disables backface culling for this submesh so
    /// both faces shade (curtains, foliage); single-sided submeshes cull `BACK`.
    pub double_sided: bool,
    /// Physically defined thin-sheet response. `None` selects the standard PBR model.
    pub thin_sheet: Option<ThinSheetMaterial>,
}

impl SubmeshMaterial {
    /// The default glTF metallic-roughness defaults the draw path applies when an item
    /// carries no per-submesh material — white base color, dielectric, fully rough.
    pub fn defaults() -> Self {
        Self {
            albedo_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            vector_displacement_texture: None,
            coverage_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            height_texture: None,
            base_color: Vec4::ONE,
            metallic: 0.0,
            roughness: 1.0,
            emissive: Vec3::ZERO,
            emissive_strength: 1.0,
            normal_strength: 1.0,
            uv_tiling: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            height_scale: 0.05,
            height_mode: HeightMode::Bump,
            blend_mode: BlendMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
            thin_sheet: None,
        }
    }
}

impl Default for SubmeshMaterial {
    fn default() -> Self {
        Self::defaults()
    }
}

/// One renderable submitted to the scene draw list: a mesh, its world transform, the
/// per-submesh materials, and the PSO-selecting [`Material`].
///
/// `submit_draw_list` resolves the material to a cached PSO and batches by
/// (pipeline, mesh) into instanced draws.
#[derive(Clone)]
pub struct DrawItem {
    /// The mesh to draw.
    pub mesh: Arc<GpuMesh>,
    /// World matrix.
    pub model: Mat4,
    /// `transpose(inverse(mat3(model)))` for correct normals under non-uniform scale.
    pub normal_matrix: Mat4,
    /// One entry per mesh submesh; a single entry applies to all submeshes (clamped).
    pub submesh_materials: Vec<SubmeshMaterial>,
    /// Selects the PSO (the übershader permutation), shared by all submeshes.
    pub material: Material,
    /// GPU skinning: when set the item is deformed once by the `skin` compute pass into
    /// its slice of the frame's deformed-vertex buffer, then drawn as a lone static
    /// instance reading that slice. A skinned item with no mesh skin stream is dropped.
    pub skinned: bool,
    /// Skinning: the base of this instance's joints in the frame palette.
    pub joint_offset: u32,
    /// Skinning: matrices this instance contributes (its palette slice length).
    pub joint_count: u32,
    /// Per-target morph weights driving this instance (empty = not a morph draw; canonical
    /// `0..1`). The instancing pass compacts the above-threshold targets into the frame's
    /// active-target buffer and dispatches the morph deform before skin; the mesh must
    /// carry morph buffers (`GpuMesh::morph`).
    pub morph_weights: Vec<f32>,
    /// Source entity id (0 = none), keying the cross-frame motion caches (TAA + skin).
    pub entity: u64,
}

impl DrawItem {
    /// A static draw item: a mesh + transform + materials with the default (lit
    /// übershader) [`Material`] and no skinning.
    pub fn new(mesh: Arc<GpuMesh>, model: Mat4, submesh_materials: Vec<SubmeshMaterial>) -> Self {
        Self {
            mesh,
            model,
            normal_matrix: normal_matrix(model),
            submesh_materials,
            material: Material::default(),
            skinned: false,
            joint_offset: 0,
            joint_count: 0,
            morph_weights: Vec::new(),
            entity: 0,
        }
    }
}

/// `transpose(inverse(mat3(model)))` extended to a `Mat4` — the normal matrix the
/// instance row carries so non-uniform scale leaves normals orthogonal to the surface.
pub fn normal_matrix(model: Mat4) -> Mat4 {
    Mat4::from_mat3(Mat3::from_mat4(model).inverse().transpose())
}

/// The per-frame handles a tessellated (`HeightMode::Displacement`) batch draws through — the
/// amplified geometry the Phase-4 emit kernel wrote into `RenderGraphResources`. Filled by
/// [`crate::Renderer`]'s `record_tess_prep` once the per-frame transients are acquired (the base
/// instance links a batch to its tessellated slice), then read by the raster passes' indirect draw.
#[derive(Clone, Copy)]
pub struct TessDraw {
    /// The transient VB holding this frame's amplified micro-vertices (48 B stride).
    pub vertex_buffer: vk::Buffer,
    /// The transient VB holding the PREVIOUS frame's micro-vertex positions (same layout + stride), for
    /// the motion prepass's prev-position stream — the geomorph slide the emit kernel wrote with last
    /// frame's per-edge factors. Equal to `vertex_buffer` only when the two carry identical positions.
    pub prev_vertex_buffer: vk::Buffer,
    /// The transient IB holding the generated index stream (u32).
    pub index_buffer: vk::Buffer,
    /// The indirect-args buffer (one `VkDrawIndexedIndirectCommand` per instance, 20 B stride).
    pub args_buffer: vk::Buffer,
    /// Byte offset of this batch's command in `args_buffer` (= `instance_row * 20`).
    pub args_offset: u64,
}

/// A batch of instances sharing a pipeline + mesh, drawn as one instanced draw per
/// submesh. Bindless means the per-instance texture indices live in the instance SSBO,
/// not a per-batch descriptor — so texture differences never split a batch.
/// `base_instance` offsets into the frame's instance buffer.
#[derive(Clone)]
pub struct DrawBatch {
    /// The PSO resolved from the material via the cache.
    pub pipeline: Arc<Pipeline>,
    /// The mesh whose vertex/index streams the batch binds and draws.
    pub mesh: Arc<GpuMesh>,
    /// The base offset into the frame's instance buffer (submesh 0, instance 0).
    pub base_instance: u32,
    /// The number of logical instances in the batch.
    pub instance_count: u32,
    /// When set the batch draws the frame's compute-deformed buffer as its binding-0
    /// vertex stream (the static stream otherwise) — true for a skinned OR a
    /// morph-active batch; a deformed batch is always one instance.
    pub deformed: bool,
    /// The base vertex of this batch's instance in the deformed buffer (0 for the static
    /// path), added to each submesh's `vertex_offset` in the deformed draw.
    pub deformed_vertex_offset: u32,
    /// Per-geometry-submesh backface-cull mode (aligned with `mesh.submeshes`; a single entry backs
    /// the no-submesh single-draw path): `NONE` for a two-sided submesh material, `BACK` otherwise.
    /// The scene pass applies it via dynamic state; other passes keep their baked cull mode.
    pub submesh_cull: Vec<vk::CullModeFlags>,
    /// The original mesh-submesh indices this batch draws (into `mesh.submeshes`), letting one
    /// mesh's submeshes split across batches by blend mode: opaque/masked submeshes draw with the
    /// opaque PSO here, translucent ones with the blend PSO in `transparent_batches` — every batch
    /// from the same mesh shares one submesh-major instance block (`base_instance`), so a subset
    /// just picks its `s` slices. Empty for a submesh-less mesh (the whole index buffer draws once).
    pub submeshes: Vec<u32>,
    /// When set the batch is a Phase-4 tessellated (`HeightMode::Displacement`) instance: it draws
    /// the amplified transient VB/IB via one indirect draw sourced from [`TessDraw::args_buffer`]
    /// instead of the fixed-count `cmd_draw_indexed`. Filled mid-render (the transients don't exist at
    /// draw-list build time); `None` for a static / skinned / morph batch.
    pub tessellated: Option<TessDraw>,
}

/// One skinned mesh-instance's compute work for the frame: the descriptor set wiring its
/// static + skin streams, the joint palette, and the deformed output, plus the push the
/// `skin` kernel reads. Built by [`crate::Instancing::submit_draw_list`] and replayed in
/// the `skin` pass.
#[derive(Clone, Copy)]
pub struct SkinDispatch {
    /// The per-dispatch descriptor set (static vertices, skin, palette, deformed output).
    pub set: vk::DescriptorSet,
    /// The skinned mesh-instance's vertex count (one compute invocation each).
    pub vertex_count: u32,
    /// The base of this instance's joints in the bound palette.
    pub joint_offset: u32,
    /// The base of this instance's vertices in the deformed output buffer.
    pub deformed_offset: u32,
}

/// One morph mesh-instance's compute work for the frame: the descriptor set wiring its
/// base + delta + range + active-target + accumulator + deformed-output buffers, plus the
/// counts the `morph` kernel's three passes (clear/scatter/resolve) dispatch over. Built
/// by [`crate::Instancing::submit_draw_list`] and replayed in the `morph` pass before skin.
#[derive(Clone, Copy)]
pub struct MorphDispatch {
    /// The per-dispatch descriptor set (base, deltas, ranges, active list, accum, output).
    pub set: vk::DescriptorSet,
    /// The morph mesh-instance's vertex count (clear/resolve dispatch size).
    pub vertex_count: u32,
    /// The total active deltas across active targets (the scatter dispatch size).
    pub scatter_count: u32,
    /// The number of active (above-threshold) morph targets.
    pub active_count: u32,
    /// The base of this instance's active targets in the frame's shared active buffer.
    pub active_base: u32,
    /// The base of this instance's vertices in the deformed output buffer.
    pub deformed_offset: u32,
}

/// One deforming mesh-instance (skinned or morph) the TLAS references via its own per-frame
/// refit BLAS. The BLAS geometry is the post-deform vertex slice; `world_transform` places
/// it in the TLAS. For a skinned (or skin+morph) instance the deformed vertices are already
/// in world space (the palette is `worldBone * inverseBind` and the skin kernel omits the
/// model matrix), so `world_transform` is identity; for an unskinned-morph instance the
/// deformed vertices are in mesh-local space, so `world_transform` is the node world matrix.
/// A tessellated (`HeightMode::Displacement`) instance's per-frame slice into the amplified transient
/// VB/IB, for building its BLAS. Unlike the skinned path (a 1:1 remap of the base mesh, refit in place),
/// a tessellated instance mints variable topology every frame, so its BLAS is a full rebuild over these
/// buffers. Filled mid-render by `record_tess_prep` (the transients don't exist at draw-list build time).
///
/// These point at the **coarse** (secondary-ray) amplified geometry — a separate, lower-density run of the
/// tessellation chain (Phase 10, Q2 RT coarsening: coarser LOD target + smaller dice cap) — not the fine
/// buffers the raster passes draw. The coarse mesh is still Phong-smoothed, displaced, and watertight; the
/// smaller worst case makes the per-frame BLAS BUILD far cheaper for shadow / GI / reflection rays.
#[derive(Clone, Copy)]
pub struct TessRtSlice {
    /// The transient VB holding this frame's amplified micro-vertices (48 B stride).
    pub vertex_buffer: vk::Buffer,
    /// The transient IB holding the generated index stream (u32), degenerate-padded past the real tail.
    pub index_buffer: vk::Buffer,
    /// This instance's reserved vertex slice base (micro-vertices).
    pub vertex_base: u32,
    /// This instance's reserved index slice base (indices).
    pub index_base: u32,
    /// Worst-case reserved vertices (`max_vertex + 1` for the BLAS size query + build).
    pub worst_case_verts: u32,
    /// Worst-case reserved triangles (the CPU `maxPrimitiveCount` for the portable BUILD floor).
    pub worst_case_prims: u32,
}

#[derive(Clone)]
pub struct DeformedRtInstance {
    /// Keys the grow-only per-instance refit / rebuild BLAS.
    pub entity: u64,
    /// The instance's base vertex in the frame's deformed buffer.
    pub deformed_offset: u32,
    /// The deformed vertex count.
    pub vertex_count: u32,
    /// The index count (the BLAS geometry's triangle source).
    pub index_count: u32,
    /// The mesh supplying the index stream for the BLAS geometry.
    pub mesh: Arc<GpuMesh>,
    /// The TLAS placement: identity for a skinned / skin+morph instance (already
    /// world-space), the node world matrix for an unskinned-morph / tessellated instance.
    pub world_transform: Mat4,
    /// When set the instance is tessellated: its BLAS is a full per-frame `MODE_BUILD` over the
    /// amplified transient VB/IB (variable topology forbids the skinned in-place `UPDATE`). `None` for a
    /// skinned / morph instance, which keeps the create-once-then-refit fast path.
    pub tess: Option<TessRtSlice>,
}

/// The frame's structured draw list, built by `submit_draw_list` and recorded by the
/// scene pass (shaded) and the optional depth pre-pass (depth only).
#[derive(Default)]
pub struct SceneDrawList {
    /// The camera view-projection (the per-frame vertex push constant).
    pub view_proj: Mat4,
    /// The batched instanced opaque + masked draws, in first-seen bucket order.
    pub batches: Vec<DrawBatch>,
    /// The translucent draws, each a lone-instance batch, sorted back-to-front (farthest
    /// first) by clip-space depth. Recorded by the scene pass's trailing translucent scope
    /// with the blend PSO (depth-test on, depth-write off), never in the depth pre-pass.
    pub transparent_batches: Vec<DrawBatch>,
    /// Per skinned mesh-instance: the compute work the `skin` pass dispatches before any
    /// geometry pass reads the deformed buffer. Empty when no skinned instances exist.
    pub skin_dispatches: Vec<SkinDispatch>,
    /// The parallel dispatches that deform the previous pose into the prev-deformed
    /// buffer (previous palette + previous-deformed output), read only by the motion pass.
    pub prev_skin_dispatches: Vec<SkinDispatch>,
    /// Per morph mesh-instance: the compute work the `morph` pass dispatches (before skin)
    /// to write the morphed base into the deformed buffer. Empty when no morph instances.
    pub morph_dispatches: Vec<MorphDispatch>,
    /// The parallel prev-pose morph dispatches (prev weights → prev-deformed), read only
    /// by the motion pass. Wired in Phase 5; the field lands here so the shape is complete.
    pub prev_morph_dispatches: Vec<MorphDispatch>,
    /// Per deforming instance (skin or morph): the entity + deformed offset the RT refit
    /// BLAS reads + the TLAS placement. Empty unless an RT consumer is armed.
    pub deformed_rt_instances: Vec<DeformedRtInstance>,
    /// Per displaced mesh-instance: the base mesh + transform + budget the adaptive-tessellation
    /// prep passes (factor/scan/finalize) consume in the deform scope. The amplified transient
    /// geometry they emit is what every raster + RT consumer reads for a displaced mesh.
    pub tess_buckets: Vec<crate::tessellation::TessBucket>,
    /// Textures pinned live for the frame (their bindless indices are referenced by
    /// the instance SSBO, so the `Arc`s must outlive the GPU read).
    pub live_textures: Vec<Arc<GpuTexture>>,
    /// `true` once a draw list has been built this frame.
    pub valid: bool,
}

impl SceneDrawList {
    /// A recording-only copy: the batches (their `Arc`s cloned cheaply) plus the
    /// view-projection and validity, with no `live_textures`. Both the depth pre-pass
    /// and scene-pass bodies take one; the texture pins stay on the owning list until
    /// the frame's fence is waited next, so they outlive the GPU read.
    pub fn shallow_clone(&self) -> Self {
        Self {
            view_proj: self.view_proj,
            batches: self.batches.clone(),
            transparent_batches: self.transparent_batches.clone(),
            skin_dispatches: self.skin_dispatches.clone(),
            prev_skin_dispatches: self.prev_skin_dispatches.clone(),
            morph_dispatches: self.morph_dispatches.clone(),
            prev_morph_dispatches: self.prev_morph_dispatches.clone(),
            deformed_rt_instances: self.deformed_rt_instances.clone(),
            // The prep passes run in the deform scope off the owning list, never a recording copy.
            tess_buckets: Vec::new(),
            live_textures: Vec::new(),
            valid: self.valid,
        }
    }
}

/// Per-frame scene draw counters, refreshed each `submit_draw_list` and inspectable to
/// verify the batching is not O(draws).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// `drawIndexed` calls (one per submesh per batch).
    pub draw_calls: u32,
    /// Distinct (pipeline, mesh) buckets.
    pub batches: u32,
    /// Total logical instances drawn.
    pub instances: u32,
    /// Triangles submitted (sum of `index_count / 3` over instances).
    pub triangles: u32,
    /// Descriptor-set binds recorded in the scene pass.
    pub descriptor_binds: u32,
    /// Primary command buffers submitted this frame.
    pub command_buffers: u32,
    /// `vkQueueSubmit2` calls this frame.
    pub queue_submits: u32,
    /// PSOs compiled this frame (non-zero on a steady-state frame = a compile hitch).
    pub pipelines_created: u32,
    /// Bytes uploaded to the frame's `InstanceData` storage buffer.
    pub instance_upload_bytes: u64,
    /// CPU bytes retained by unique drawn meshes for exact surface queries.
    pub retained_mesh_cpu_bytes: u64,
    /// Actual indexed draw invocations recorded across directional, spot, and point shadows.
    pub shadow_draw_calls: u32,
}
