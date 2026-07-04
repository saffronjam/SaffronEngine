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
use saffron_core::BlendMode;
use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};

use crate::gpu_types::Material;
use crate::resources::{GpuMesh, GpuTexture, Pipeline};

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
    /// Height / displacement map (sets the `HEIGHT` feature bit for parallax, or `DISPLACE` when
    /// [`SubmeshMaterial::displacement`] routes it through vertex-shader displacement instead).
    pub height_texture: Option<Arc<GpuTexture>>,
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
    /// Height scale — parallax march depth, or the world-space displacement amplitude when
    /// [`SubmeshMaterial::displacement`] is set.
    pub height_scale: f32,
    /// Route the height map through **vertex-shader displacement** (real geometry, true silhouette)
    /// rather than parallax-occlusion mapping. Requires a densely-tessellated mesh to look right;
    /// the interactive preview sphere is seeded dense for exactly this.
    pub displacement: bool,
    /// Alpha/blend mode: opaque, masked (alpha-clip discard below [`SubmeshMaterial::alpha_cutoff`]),
    /// or translucent (routed to the sorted, blended translucent draw list).
    pub blend_mode: BlendMode,
    /// The alpha-clip cutoff threshold (used by [`BlendMode::Masked`]).
    pub alpha_cutoff: f32,
    /// Two-sided (glTF `doubleSided`): the scene pass disables backface culling for this submesh so
    /// both faces shade (curtains, foliage); single-sided submeshes cull `BACK`.
    pub double_sided: bool,
}

impl SubmeshMaterial {
    /// The default glTF metallic-roughness defaults the draw path applies when an item
    /// carries no per-submesh material — white base color, dielectric, fully rough.
    pub fn defaults() -> Self {
        Self {
            albedo_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
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
            displacement: false,
            blend_mode: BlendMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
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

/// One displaced mesh-instance's compute work for the frame: the descriptor set wiring its base
/// vertices (in) + the shared deformed buffer (out), plus the height-map index / amplitude / uv
/// transform the `displace` kernel pushes. Built by [`crate::Instancing::submit_draw_list`] and
/// replayed in the `displace` pass (which writes the same deformed buffer as skin/morph).
#[derive(Clone, Copy)]
pub struct DisplaceDispatch {
    /// The per-dispatch descriptor set (base vertices in, deformed out).
    pub set: vk::DescriptorSet,
    /// The displaced mesh-instance's vertex count (one compute invocation each).
    pub vertex_count: u32,
    /// The base of this instance's vertices in the deformed output buffer.
    pub deformed_offset: u32,
    /// Bindless index of the height map (sampled from the shared set-0 albedo array).
    pub height_index: u32,
    /// Local-space displacement amplitude (`MaterialParams.emissive.w`).
    pub height_scale: f32,
    /// `tiling.xy, offset.xy` (`MaterialParams.uv`).
    pub uv_transform: [f32; 4],
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
#[derive(Clone)]
pub struct DeformedRtInstance {
    /// Keys the grow-only per-instance refit BLAS (built once, then updated).
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
    /// world-space), the node world matrix for an unskinned-morph instance.
    pub world_transform: Mat4,
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
    /// Per displaced mesh-instance: the compute work the `displace` pass dispatches (in the same
    /// deform scope as skin/morph) to write the height-displaced base into the deformed buffer.
    /// Empty when no displacement-enabled instances exist.
    pub displace_dispatches: Vec<DisplaceDispatch>,
    /// The parallel displace dispatches writing the prev-deformed buffer (identical displacement —
    /// a zero deformation delta), read only by the motion pass. Empty when no displaced instances.
    pub prev_displace_dispatches: Vec<DisplaceDispatch>,
    /// Per deforming instance (skin or morph): the entity + deformed offset the RT refit
    /// BLAS reads + the TLAS placement. Empty unless an RT consumer is armed.
    pub deformed_rt_instances: Vec<DeformedRtInstance>,
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
            displace_dispatches: self.displace_dispatches.clone(),
            prev_displace_dispatches: self.prev_displace_dispatches.clone(),
            deformed_rt_instances: self.deformed_rt_instances.clone(),
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
}
