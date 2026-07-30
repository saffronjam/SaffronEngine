//! The per-frame deformation state ([`FrameDeformation`]), the resolved material
//! vocabulary ([`SubmeshMaterial`]), and the [`RenderStats`] counters.
//!
//! A skinned instance deforms into its slice of the frame's deformed-vertex buffer
//! (the [`SkinDispatch`] the `skin` compute pass replays); the executor vertex path
//! then pulls the slice through its device address. The [`DeformedRtInstance`] list
//! rides for the RT refit BLAS.

use std::sync::Arc;

use ash::vk;
use saffron_core::{BlendMode, HeightMode};
use saffron_geometry::glam::{Mat4, Vec2, Vec3, Vec4};
use saffron_material::AlphaClassification;

use crate::resources::{GpuMesh, GpuTexture};

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
/// An instance carries one per mesh submesh, indexed by `Submesh::material_slot`
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
    /// or translucent (routed to the GPU-sorted, blended translucent stream).
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

/// One skinned mesh-instance's frame deformation facts: where its palette and deformed
/// output landed this frame, keyed by entity for the provider-params patch.
#[derive(Clone, Copy, Debug)]
pub struct SkinnedDeformation {
    /// The skinned entity (the mirror's instance key).
    pub entity: u64,
    /// The base of this instance's joints in the frame palette.
    pub joint_offset: u32,
    /// This instance's joint count.
    pub joint_count: u32,
    /// The base vertex of this instance in the frame's deformed buffers.
    pub deformed_offset: u32,
    /// The instance's vertex count.
    pub vertex_count: u32,
}

/// One skinned mesh-instance's compute work for the frame: the descriptor set wiring its
/// static + skin streams, the joint palette, and the deformed output, plus the push the
/// `skin` kernel reads. Built by the deformation wiring and replayed in
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
/// by the deformation wiring and replayed in the `morph` pass before skin.
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

/// A tessellated ([`HeightMode::Displacement`]) instance's per-frame slice into the amplified
/// transient VB/IB, for building its BLAS. A tessellated instance mints variable topology every
/// frame, so its BLAS is a full rebuild over these buffers rather than the skinned path's in-place
/// refit. Filled mid-render, once the transients exist.
///
/// These point at the **coarse** (secondary-ray) amplified geometry — a separate, lower-density run
/// of the tessellation chain — not the fine buffers the raster passes draw. The coarse mesh is still
/// Phong-smoothed, displaced, and watertight; the smaller worst case makes the per-frame BLAS build
/// far cheaper for shadow / GI / reflection rays.
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

/// One deforming mesh-instance (skinned, morph, or tessellated) the TLAS references via its own
/// per-frame BLAS. The geometry is the post-deform vertex slice; `world_transform` places it in
/// the TLAS.
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

/// The frame's deformation state, built by
/// [`crate::Renderer::submit_gpu_scene_deformations`] and read by the deform, raster,
/// and RT passes (the draws themselves come from the GPU scene's visibility traversal).
#[derive(Default)]
pub struct FrameDeformation {
    /// The camera view-projection (the per-frame vertex push constant).
    pub view_proj: Mat4,
    /// Per skinned mesh-instance: the compute work the `skin` pass dispatches before any
    /// geometry pass reads the deformed buffer. Empty when no skinned instances exist.
    pub skin_dispatches: Vec<SkinDispatch>,
    /// Per skinned mesh-instance: the frame's deformation-provider parameter values
    /// (the GPU-scene mirror's provider params patch to these offsets each frame).
    pub skinned_deformations: Vec<SkinnedDeformation>,
    /// The parallel dispatches that deform the previous pose into the prev-deformed
    /// buffer (previous palette + previous-deformed output), read only by the motion pass.
    pub prev_skin_dispatches: Vec<SkinDispatch>,
    /// Per morph mesh-instance: the compute work the `morph` pass dispatches (before skin)
    /// to write the morphed base into the deformed buffer. Empty when no morph instances.
    pub morph_dispatches: Vec<MorphDispatch>,
    /// The parallel prev-pose morph dispatches (prev weights → prev-deformed), read only
    /// by the motion pass.
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
    /// `true` once this frame's deformation state has been built.
    pub valid: bool,
}

impl FrameDeformation {
    /// A recording-only copy (the `Arc`s cloned cheaply) with no `live_textures` — the
    /// texture pins stay on the owning list until the frame's fence is waited next, so
    /// they outlive the GPU read.
    pub fn shallow_clone(&self) -> Self {
        Self {
            view_proj: self.view_proj,
            skin_dispatches: self.skin_dispatches.clone(),
            prev_skin_dispatches: self.prev_skin_dispatches.clone(),
            morph_dispatches: self.morph_dispatches.clone(),
            prev_morph_dispatches: self.prev_morph_dispatches.clone(),
            deformed_rt_instances: self.deformed_rt_instances.clone(),
            // The prep passes run in the deform scope off the owning list, never a recording copy.
            tess_buckets: Vec::new(),
            skinned_deformations: Vec::new(),
            live_textures: Vec::new(),
            valid: self.valid,
        }
    }
}

/// Per-frame scene draw counters, derived from the visibility chain's GPU readback
/// (the executor decides the actual draws) and inspectable to verify render
/// preparation stays O(changes), not O(draws).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// GPU-emitted indirect draw commands (the traversal's record count).
    pub draw_calls: u32,
    /// Live executor draw buckets (distinct shader + PSO-bin combos).
    pub batches: u32,
    /// Instances the visibility cull kept.
    pub instances: u32,
    /// Triangles the emitted records rasterize (sum of `index_count / 3`).
    pub triangles: u32,
    /// Descriptor-set binds recorded in the scene pass.
    pub descriptor_binds: u32,
    /// Primary command buffers submitted this frame.
    pub command_buffers: u32,
    /// `vkQueueSubmit2` calls this frame.
    pub queue_submits: u32,
    /// PSOs compiled this frame (non-zero on a steady-state frame = a compile hitch).
    pub pipelines_created: u32,
    /// GPU-scene table bytes staged this frame ((near-)zero on a steady scene).
    pub instance_upload_bytes: u64,
    /// CPU bytes retained by unique drawn meshes for exact surface queries.
    pub retained_mesh_cpu_bytes: u64,
    /// Actual indexed draw invocations recorded across directional, spot, and point shadows.
    pub shadow_draw_calls: u32,
}
