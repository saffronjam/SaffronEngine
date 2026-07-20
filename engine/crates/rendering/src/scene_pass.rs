//! Recording the scene draw list into the scene + depth-prepass command buffers.
//!
//! [`record_scene_draw_list`] binds the frame's descriptor sets once (the bindless
//! albedo array on set 0 and the per-instance + material storage on set 2), pushes the
//! viewProj, then per batch binds the PSO + vertex/index streams and issues one
//! instanced `drawIndexed` per submesh. [`record_depth_prepass`] is the vertex-only
//! sibling that lays down depth first. Both consume the same [`SceneDrawList`].
//!
//! # Submesh draws + `firstInstance`
//!
//! The instance buffer is submesh-major, so submesh `s` of a batch reads its rows by
//! offsetting `firstInstance` by `base_instance + s * instance_count`. The vertex
//! shader reads `instances[SV_VulkanInstanceID]`, and Vulkan's instance id is
//! `firstInstance + instance`, so submesh `s`'s instance `i` fetches exactly its row.

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};

use crate::draw_list::{DrawBatch, SceneDrawList};
use crate::lighting::{SHADOW_DEPTH_BIAS_CONSTANT, SHADOW_DEPTH_BIAS_SLOPE};

/// Records one batch's submesh draws on `cmd`: one instanced `drawIndexed` per submesh,
/// the `firstInstance` shifted by the submesh's slice of the submesh-major instance
/// buffer. A submesh-less mesh draws its whole index buffer as a single range.
/// Replays a batch's per-submesh instanced draws. `cull_modes` (the scene pass only) sets the
/// backface-cull mode per submesh via dynamic state — `Some` requires the bound PSO to declare
/// `VK_DYNAMIC_STATE_CULL_MODE`; passes that keep a baked cull mode (shadow / prepass / g-buffer /
/// point-shadow) pass `None` and never touch the state.
pub(crate) fn record_batch_submeshes(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    batch: &DrawBatch,
    cull_modes: Option<&[vk::CullModeFlags]>,
) {
    // A tessellated (`HeightMode::Displacement`) batch draws its amplified transient VB/IB as one
    // indirect draw sourced from the GPU-seeded `VkDrawIndexedIndirectCommand` (index/first-index/
    // vertex-offset/first-instance are all in the buffer). The floor form (`drawCount = 1`) needs no
    // device feature, so it draws on every implementation.
    if let Some(t) = &batch.tessellated {
        if let Some(&mode) = cull_modes.and_then(|m| m.first()) {
            // SAFETY: the ash seam. The scene PSO declares dynamic cull state.
            unsafe { raw.cmd_set_cull_mode(cmd, mode) };
        }
        // SAFETY: the ash seam. `args_buffer` was allocated with `INDIRECT_BUFFER` usage and holds one
        // `VkDrawIndexedIndirectCommand` (20 B) at `args_offset`; the tessellated VB/IB are bound.
        unsafe {
            raw.cmd_draw_indexed_indirect(cmd, t.args_buffer, t.args_offset, 1, 20);
        }
        return;
    }
    if batch.mesh.submeshes.is_empty() {
        if let Some(&mode) = cull_modes.and_then(|m| m.first()) {
            // SAFETY: the ash seam. The scene PSO declares dynamic cull state.
            unsafe { raw.cmd_set_cull_mode(cmd, mode) };
        }
        // SAFETY: the ash seam. The bound vertex/index streams + instance set cover the
        // draw; `cmd` is recording inside the pass's rendering scope.
        unsafe {
            raw.cmd_draw_indexed(
                cmd,
                batch.mesh.index_count,
                batch.instance_count,
                0,
                0,
                batch.base_instance,
            );
        }
        return;
    }
    // The batch draws its subset of the mesh's submeshes (blend mode split one mesh across
    // batches). `s` is the original mesh-submesh index, so it keys both `mesh.submeshes[s]` and
    // the submesh-major instance slice (`base_instance + s * instance_count`) — the same block
    // the sibling batches share.
    for &s in &batch.submeshes {
        let s = s as usize;
        let Some(submesh) = batch.mesh.submeshes.get(s) else {
            continue;
        };
        if let Some(modes) = cull_modes {
            let mode = modes.get(s).copied().unwrap_or(vk::CullModeFlags::BACK);
            // SAFETY: the ash seam. The scene PSO declares dynamic cull state.
            unsafe { raw.cmd_set_cull_mode(cmd, mode) };
        }
        let vertex_offset = batch.deformed_vertex_offset as i32 + submesh.vertex_offset;
        let first_instance = batch.base_instance + s as u32 * batch.instance_count;
        // SAFETY: the ash seam. As above; the submesh range is within the index buffer.
        unsafe {
            raw.cmd_draw_indexed(
                cmd,
                submesh.index_count,
                batch.instance_count,
                submesh.first_index,
                vertex_offset,
                first_instance,
            );
        }
    }
}

/// Binds a batch's vertex (binding 0) + index streams: the frame's compute-deformed
/// buffer for a skinned batch (its vertices start at `deformed_vertex_offset`, applied in
/// [`record_batch_submeshes`]), the static mesh stream otherwise. `deformed` is the
/// frame's deformed buffer handle when the skin pass ran, `None` otherwise.
fn bind_batch_vertices(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    batch: &DrawBatch,
    deformed: Option<vk::Buffer>,
) {
    // A tessellated batch binds the frame's amplified transient VB (binding 0) + generated index
    // stream (both 48 B `Vertex` / u32, matching the static attribute + index state).
    if let Some(t) = &batch.tessellated {
        // SAFETY: the ash seam. The transient VB/IB are pinned by the frame's `TransientResources`
        // until this slot's fence; `cmd` is recording inside the pass.
        unsafe {
            raw.cmd_bind_vertex_buffers(cmd, 0, &[t.vertex_buffer], &[0]);
            raw.cmd_bind_index_buffer(cmd, t.index_buffer, 0, vk::IndexType::UINT32);
        }
        return;
    }
    let vertex_buffer = match (batch.deformed, deformed) {
        (true, Some(deformed)) => deformed,
        _ => batch.mesh.vertex_buffer(),
    };
    // SAFETY: the ash seam. The mesh / deformed buffer outlives the recorded command
    // (pinned by the batch's `Arc` / the frame's `Skinning`); `cmd` is recording.
    unsafe {
        raw.cmd_bind_vertex_buffers(cmd, 0, &[vertex_buffer], &[0]);
        raw.cmd_bind_index_buffer(cmd, batch.mesh.index_buffer(), 0, vk::IndexType::UINT32);
    }
}

/// The number of descriptor-set bind operations the scene pass records this frame — constant
/// in the batch count.
///
/// Sets 0, {1,2}, 3, 4, 5 are five bind operations that hold regardless of batch count
/// (bindless textures + per-instance indices keep the path O(1) in draws); the RT sets 6 + 7
/// add one each when present on an RT device. `0` when the draw list is empty (no draws, no
/// binds). The single source of truth, called by both [`record_scene_draw_list`] (the real
/// bind) and the renderer's `render-stats` accounting so the two never drift.
#[must_use]
pub fn scene_draw_list_bind_count(
    has_draws: bool,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
) -> u32 {
    if !has_draws {
        return 0;
    }
    let mut binds = 5u32;
    if rt_mesh_set != vk::DescriptorSet::null() {
        binds += 1;
    }
    if restir_mesh_set != vk::DescriptorSet::null() {
        binds += 1;
    }
    binds
}

/// Records the shaded scene geometry: bind the bindless set (0), the light set (1), and
/// the instance/material set (2) once, push the viewProj, then per batch bind its PSO +
/// streams and draw. Returns the number of descriptor-set binds recorded (for
/// [`crate::RenderStats`]) — constant in the batch count.
///
/// `bindless_set` is set 0, `light_set` set 1 (directional + punctual + cluster lists +
/// shadow maps), `instance_set` set 2, `ibl_set` set 3, `ssao_mesh_set` set 4 (AO +
/// contact + SSGI samplers), `ddgi_mesh_set` set 5. The übershader's layout always
/// declares sets 4 + 5, so each is bound whenever present (its reads are gated in-shader by
/// the AO/contact/SSGI/DDGI flags, so the lit image is correct even when the maps are the
/// neutral init-transitioned targets); leaving either unbound is a draw-time validation
/// error (`VUID-vkCmdDrawIndexed-None-08600`). Sets 6 (TLAS) / 7 (ReSTIR) are present only
/// on an RT device, where the layout declares them, so they bind when non-`null`.
#[allow(clippy::too_many_arguments)]
pub fn record_scene_draw_list(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    bindless_set: vk::DescriptorSet,
    light_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    ibl_set: vk::DescriptorSet,
    ssao_mesh_set: vk::DescriptorSet,
    ddgi_mesh_set: vk::DescriptorSet,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
    deformed: Option<vk::Buffer>,
) -> u32 {
    if !list.valid || list.batches.is_empty() {
        return 0;
    }
    let binds = scene_draw_list_bind_count(true, rt_mesh_set, restir_mesh_set);
    let layout = list.batches[0].pipeline.layout();
    bind_mesh_descriptor_sets(
        raw,
        cmd,
        layout,
        list.view_proj,
        bindless_set,
        light_set,
        instance_set,
        ibl_set,
        ssao_mesh_set,
        ddgi_mesh_set,
        rt_mesh_set,
        restir_mesh_set,
    );
    record_batches(raw, cmd, &list.batches, deformed);
    binds
}

/// Records the sorted translucent draws into the scene pass's trailing translucent scope:
/// the same descriptor-set binds as [`record_scene_draw_list`], then each back-to-front
/// batch replayed with its blend PSO (depth-test on, depth-write off) so nearer translucent
/// surfaces composite over the ones behind them. Returns the bind count (0 when there are no
/// translucent draws). Shares the scene pass's color + depth attachments — no separate pass.
#[allow(clippy::too_many_arguments)]
pub fn record_transparent_draw_list(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    bindless_set: vk::DescriptorSet,
    light_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    ibl_set: vk::DescriptorSet,
    ssao_mesh_set: vk::DescriptorSet,
    ddgi_mesh_set: vk::DescriptorSet,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
    deformed: Option<vk::Buffer>,
) -> u32 {
    if !list.valid || list.transparent_batches.is_empty() {
        return 0;
    }
    let binds = scene_draw_list_bind_count(true, rt_mesh_set, restir_mesh_set);
    let layout = list.transparent_batches[0].pipeline.layout();
    bind_mesh_descriptor_sets(
        raw,
        cmd,
        layout,
        list.view_proj,
        bindless_set,
        light_set,
        instance_set,
        ibl_set,
        ssao_mesh_set,
        ddgi_mesh_set,
        rt_mesh_set,
        restir_mesh_set,
    );
    record_batches(raw, cmd, &list.transparent_batches, deformed);
    binds
}

/// Binds the mesh descriptor sets (0, {1,2}, 3, 4, 5, and 6/7 when present) and pushes the
/// viewProj constant — the constant prefix both the opaque and translucent scopes share. The
/// set roster is documented on [`record_scene_draw_list`].
#[allow(clippy::too_many_arguments)]
fn bind_mesh_descriptor_sets(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    view_proj: saffron_geometry::glam::Mat4,
    bindless_set: vk::DescriptorSet,
    light_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    ibl_set: vk::DescriptorSet,
    ssao_mesh_set: vk::DescriptorSet,
    ddgi_mesh_set: vk::DescriptorSet,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
) {
    let view_proj = bytemuck::bytes_of(&view_proj);
    // SAFETY: the ash seam. The sets/layout belong to this frame; the push spans the
    // declared vertex range. Sets 1 + 2 bind in one call (consecutive sets). Set 3 = IBL
    // (irradiance + prefiltered + BRDF LUT + reflection probes), baked once, always valid.
    unsafe {
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            0,
            &[bindless_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            1,
            &[light_set, instance_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            3,
            &[ibl_set],
            &[],
        );
        // Set 4 = screen-space AO + contact + SSGI samplers, bound when the chain is
        // built (the maps are neutral init-transitioned targets when the effects are off).
        if ssao_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                4,
                &[ssao_mesh_set],
                &[],
            );
        }
        // Set 5 = the DDGI irradiance + distance atlas samplers, bound when the volume's
        // resources are built. The mesh fragment statically references set 5 (the atlases
        // are the neutral init-transitioned targets when DDGI is off), so bind it whenever
        // present; the sample is gated in-shader by the DDGI `screen_flags.z` flag.
        if ddgi_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                5,
                &[ddgi_mesh_set],
                &[],
            );
        }
        // Set 6 = the ray-tracing TLAS, present only on an RT device (the mesh PSO layout
        // includes it then). The mesh fragment statically binds it for inline ray-query
        // shadows, gated at runtime by the `rtShadows` flag; an unbound set would be a
        // validation error, so bind it whenever the layout has it.
        if rt_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                6,
                &[rt_mesh_set],
                &[],
            );
        }
        // Set 7 = the ReSTIR resolved-radiance sampler, present only on an RT device and
        // bound only when ReSTIR ran this frame; the mesh fragment then samples the resolved
        // direct radiance instead of the clustered-forward direct term. `null` otherwise.
        if restir_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                7,
                &[restir_mesh_set],
                &[],
            );
        }
        raw.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::VERTEX, 0, view_proj);
    }
}

/// Replays a batch list: bind each batch's PSO + vertex streams and record its per-submesh
/// instanced draws, applying the per-submesh dynamic cull mode (two-sided → `NONE`, else `BACK`).
fn record_batches(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    batches: &[DrawBatch],
    deformed: Option<vk::Buffer>,
) {
    for batch in batches {
        // SAFETY: the ash seam. The PSO is pinned by the batch `Arc`.
        unsafe {
            raw.cmd_bind_pipeline(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                batch.pipeline.handle(),
            );
        }
        bind_batch_vertices(raw, cmd, batch, deformed);
        record_batch_submeshes(raw, cmd, batch, Some(&batch.submesh_cull));
    }
}

/// Records the vertex-only shadow depth pass: bind the shadow PSO (depth-biased) + the
/// instance set (2), push the LIGHT's viewProj, and draw every batch's submeshes into the
/// shadow map. Shared by the directional and spot passes (only the push transform + the
/// target map differ).
#[allow(clippy::too_many_arguments)]
pub fn record_shadow_depth(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    shadow_pipeline: vk::Pipeline,
    shadow_layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    light_view_proj: Mat4,
    deformed: Option<vk::Buffer>,
) {
    if !list.valid || list.batches.is_empty() {
        return;
    }
    let push = bytemuck::bytes_of(&light_view_proj);
    // SAFETY: the ash seam. The PSO/layout/set are valid for this frame; the dynamic
    // depth bias is set per pass (the PSO declares `depth_bias_enable`).
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, shadow_pipeline);
        raw.cmd_set_depth_bias(
            cmd,
            SHADOW_DEPTH_BIAS_CONSTANT,
            0.0,
            SHADOW_DEPTH_BIAS_SLOPE,
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            shadow_layout,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(cmd, shadow_layout, vk::ShaderStageFlags::VERTEX, 0, push);
    }
    for batch in &list.batches {
        bind_batch_vertices(raw, cmd, batch, deformed);
        record_batch_submeshes(raw, cmd, batch, None);
    }
}

/// The point-shadow per-face push: the cube face's world→clip transform + the light's
/// world position (with the far plane in `w`). Read in the vertex and fragment stages.
/// Matches `point_shadow.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointShadowPush {
    view_proj: Mat4,
    light_pos: Vec4,
}

/// Records the vertex-only depth pre-pass: bind the instance set (2) + the viewProj
/// push, then draw every batch's submeshes with the single depth PSO. Lays down scene
/// depth so the scene pass loads it and shades only the front-most fragments.
pub fn record_depth_prepass(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    depth_pipeline: vk::Pipeline,
    depth_layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    deformed: Option<vk::Buffer>,
) {
    if !list.valid || list.batches.is_empty() {
        return;
    }
    let view_proj = bytemuck::bytes_of(&list.view_proj);
    // SAFETY: the ash seam. The depth PSO/layout/set are valid for this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, depth_pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            depth_layout,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            depth_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            view_proj,
        );
    }
    for batch in &list.batches {
        bind_batch_vertices(raw, cmd, batch, deformed);
        record_batch_submeshes(raw, cmd, batch, None);
    }
}

/// Records the TAA reactive-coverage pass: the same instance-set (2) bind + viewProj push as
/// [`record_depth_prepass`], then re-draws the **translucent** batches with the coverage PSO
/// (constant-1.0 fragment into the r8 reactive mask, depth-tested read-only against the scene
/// depth). Skinned batches draw from the frame's `deformed` buffer, exactly as the prepass. Does
/// nothing when there are no translucent draws (the mask stays cleared to 0).
pub fn record_reactive_coverage(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    coverage_pipeline: vk::Pipeline,
    coverage_layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    deformed: Option<vk::Buffer>,
) {
    if !list.valid || list.transparent_batches.is_empty() {
        return;
    }
    let view_proj = bytemuck::bytes_of(&list.view_proj);
    // SAFETY: the ash seam. The coverage PSO/layout/set are valid for this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, coverage_pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            coverage_layout,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            coverage_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            view_proj,
        );
    }
    for batch in &list.transparent_batches {
        bind_batch_vertices(raw, cmd, batch, deformed);
        record_batch_submeshes(raw, cmd, batch, None);
    }
}

/// Records the thin G-buffer prepass: bind the instance set (2) + the `viewProj + view`
/// push, then draw every batch's static submeshes with the G-buffer PSO. Writes the
/// view-space normal (rgb) + view-Z (.a) the screen-space chain reads.
#[allow(clippy::too_many_arguments)]
pub fn record_gbuffer(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    gbuffer_pipeline: vk::Pipeline,
    gbuffer_layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    push: &crate::ssao::GbufferPush,
    deformed: Option<vk::Buffer>,
) {
    if !list.valid || list.batches.is_empty() {
        return;
    }
    let push_bytes = bytemuck::bytes_of(push);
    // SAFETY: the ash seam. The PSO/layout/set are valid for this frame; the push spans
    // the declared two-mat4 vertex range.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, gbuffer_pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            gbuffer_layout,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            gbuffer_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            push_bytes,
        );
    }
    for batch in &list.batches {
        bind_batch_vertices(raw, cmd, batch, deformed);
        record_batch_submeshes(raw, cmd, batch, None);
    }
}

/// The resolved point-shadow cube handles a [`record_point_shadow`] body captures: the
/// cube color image + its 6 per-face render views, the shared depth scratch, and the
/// per-face square extent. The renderer fills this from the [`crate::targets::Targets`]
/// point shadow cube before building the pass.
#[derive(Clone, Copy)]
pub struct PointShadowTarget {
    /// The cube color image (all 6 layers barriered together).
    pub cube_image: vk::Image,
    /// The 6 per-face 2D render views (cube layer order +X,−X,+Y,−Y,+Z,−Z).
    pub face_views: [vk::ImageView; 6],
    /// The shared single-layer depth scratch image (reused across faces).
    pub depth_image: vk::Image,
    /// The depth scratch view.
    pub depth_view: vk::ImageView,
    /// The per-face square extent.
    pub extent: vk::Extent2D,
}

/// Renders world distance-to-light into the 6 faces of the point-shadow distance cube.
///
/// Runs as the body of a Compute-kind graph pass (the graph opens no rendering scope),
/// so this opens its own per-face dynamic-rendering scopes and manages the cube layout
/// directly: the cube's 6 layers exceed the graph's single-layer barrier. It transitions
/// all 6 layers `ShaderReadOnly → ColorAttachment`, renders each face (clearing the depth
/// scratch + the color to a beyond-far distance), then transitions back to
/// `ShaderReadOnly` for the scene sample.
///
/// `deformed_only` selects which casters this cube holds: `false` renders the non-deformed (static)
/// batches into the cached static cube, `true` renders the deformed (skinned / morph) batches into
/// the per-frame dynamic cube. The scene samples `min(static, dynamic)`.
#[allow(clippy::too_many_arguments)]
pub fn record_point_shadow(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    target: &PointShadowTarget,
    faces: &[Mat4; 6],
    light_pos: Vec3,
    far_plane: f32,
    deformed: Option<vk::Buffer>,
    deformed_only: bool,
) {
    if !list.valid || list.batches.is_empty() {
        return;
    }
    let extent = target.extent;

    // All 6 cube layers: ShaderReadOnly (entry) → ColorAttachment for rendering. The cube
    // is seeded UNDEFINED on its first frame; the renderer's first-frame path passes
    // UNDEFINED as the old layout so contents are discarded (every face clears anyway).
    cube_barrier(
        raw,
        cmd,
        target.cube_image,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::PipelineStageFlags2::FRAGMENT_SHADER,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
    );
    // The shared depth scratch: Undefined → DepthAttachment (cleared each face).
    depth_barrier(
        raw,
        cmd,
        target.depth_image,
        vk::ImageLayout::UNDEFINED,
        vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
        vk::PipelineStageFlags2::TOP_OF_PIPE,
        vk::AccessFlags2::empty(),
        vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
            | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
        vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
    );

    let viewport = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };
    // SAFETY: the ash seam. The pipeline + instance set are valid for this frame; bound
    // once before the per-face loop.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            2,
            &[instance_set],
            &[],
        );
    }

    for (face, (face_view, &face_view_proj)) in
        target.face_views.iter().zip(faces.iter()).enumerate()
    {
        // The depth scratch is reused across faces; barrier write→write between faces.
        if face > 0 {
            depth_barrier(
                raw,
                cmd,
                target.depth_image,
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
                vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
                vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS,
                vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            );
        }

        let color_attach = [vk::RenderingAttachmentInfo::default()
            .image_view(*face_view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            // Clear to beyond-far distance so untouched texels read "no occluder".
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [far_plane * 2.0, 0.0, 0.0, 0.0],
                },
            })];
        let depth_attach = vk::RenderingAttachmentInfo::default()
            .image_view(target.depth_view)
            .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .clear_value(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            });
        let rendering = vk::RenderingInfo::default()
            .render_area(scissor)
            .layer_count(1)
            .color_attachments(&color_attach)
            .depth_attachment(&depth_attach);

        let push = PointShadowPush {
            view_proj: face_view_proj,
            light_pos: light_pos.extend(far_plane),
        };
        // SAFETY: the ash seam. The attachment views reference the cube/depth images
        // barriered above; the rendering scope is opened and closed in this iteration.
        unsafe {
            raw.cmd_begin_rendering(cmd, &rendering);
            raw.cmd_set_viewport(cmd, 0, &[viewport]);
            raw.cmd_set_scissor(cmd, 0, &[scissor]);
            raw.cmd_push_constants(
                cmd,
                layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytemuck::bytes_of(&push),
            );
        }
        for batch in &list.batches {
            // Static cube renders non-deformed casters; dynamic cube renders deformed (skinned /
            // morph) ones. The scene samples `min(static, dynamic)`, so each cube holds its half.
            if batch.deformed != deformed_only {
                continue;
            }
            bind_batch_vertices(raw, cmd, batch, deformed);
            record_batch_submeshes(raw, cmd, batch, None);
        }
        // SAFETY: the ash seam. Closes the per-face rendering scope opened above.
        unsafe { raw.cmd_end_rendering(cmd) };
    }

    // All 6 layers back to ShaderReadOnly for the scene pass to sample.
    cube_barrier(
        raw,
        cmd,
        target.cube_image,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        vk::PipelineStageFlags2::FRAGMENT_SHADER,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
    );
}

/// One sync2 barrier over all 6 layers of the point-shadow cube color image.
#[allow(clippy::too_many_arguments)]
fn cube_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    layer_barrier(
        raw,
        cmd,
        image,
        vk::ImageAspectFlags::COLOR,
        6,
        old_layout,
        new_layout,
        src_stage,
        src_access,
        dst_stage,
        dst_access,
    );
}

/// One sync2 barrier over the single-layer depth scratch.
#[allow(clippy::too_many_arguments)]
fn depth_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    layer_barrier(
        raw,
        cmd,
        image,
        vk::ImageAspectFlags::DEPTH,
        1,
        old_layout,
        new_layout,
        src_stage,
        src_access,
        dst_stage,
        dst_access,
    );
}

/// One sync2 image-memory barrier over `layer_count` layers of `aspect`.
#[allow(clippy::too_many_arguments)]
fn layer_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    layer_count: u32,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the ash seam. The image outlives the recorded command; `cmd` is recording.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::Descriptors;
    use crate::device::{Device, SurfaceSource};
    use crate::draw_list::{DrawItem, SubmeshMaterial};
    use crate::instancing::{DrawListInputs, Instancing};
    use crate::lighting::{Lighting, point_shadow_face_matrices};
    use crate::pipelines::Pipelines;
    use crate::render_graph::{RenderGraph, RgPass};
    use crate::resources::BindlessFreeList;
    use crate::skinning::Skinning;
    use crate::targets::Targets;
    use crate::upload::Uploader;
    use crate::validation_issue_count;
    use crate::{Result, checked};
    use saffron_geometry::glam::{Vec2, Vec3 as G3};
    use saffron_geometry::{Mesh, Submesh, Vertex};
    use std::sync::{Arc, Mutex};

    /// A clip-space triangle covering the viewport — geometry for the shadow draws.
    fn triangle(descriptors: &Descriptors, uploader: &Uploader) -> Arc<crate::GpuMesh> {
        let v = |x: f32, y: f32| Vertex {
            position: G3::new(x, y, 0.0),
            normal: G3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
            ..Vertex::default()
        };
        let mesh = Mesh {
            vertices: vec![v(-3.0, -3.0), v(3.0, -3.0), v(0.0, 3.0)],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        uploader
            .upload_mesh(descriptors, &mesh, &[], None, None)
            .expect("upload")
    }

    /// The directional shadow depth pass + the point-shadow cube pass run on a real
    /// device against the scene-global shadow targets, validation-clean. This is the
    /// GPU-runtime shadow gate the toolbox can run (depth + cube graphics, no
    /// ray-tracing / present): the shadow depth PSO writes the directional map through
    /// the graph (DepthWrite → its external slot), and the point cube body opens its own
    /// 6 face scopes + barriers. Skips when no Vulkan device is present.
    #[test]
    fn shadow_passes_are_validation_clean() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let targets = Targets::new(&device).expect("Targets");
        let _lighting = Lighting::new(&device, &descriptors, &targets).expect("Lighting");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let mut instancing = Instancing::new(&device, &descriptors).expect("Instancing");
        let mut skinning = Skinning::new(&device).expect("Skinning");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader");

        let mesh = triangle(&descriptors, &uploader);
        let item = DrawItem::new(
            Arc::clone(&mesh),
            Mat4::IDENTITY,
            vec![SubmeshMaterial::defaults()],
        );
        let (list, _stats) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[item],
                &[],
                DrawListInputs {
                    frame: 0,
                    view_proj: Mat4::IDENTITY,
                    wireframe: false,
                    default_texture_index: crate::DEFAULT_WHITE_SLOT,
                    rt_skinned: false,
                    displace_enabled: true,
                    tess_factor_cap: crate::tessellation::TESS_DEFAULT_FACTOR_CAP,
                    tess_min_factor: crate::tessellation::TESS_DEFAULT_MIN_FACTOR,
                    tess_edge_length_target: crate::tessellation::TESS_DEFAULT_EDGE_LENGTH_TARGET,
                },
            )
            .expect("submit_draw_list");
        let instance_set = instancing.instance_set(0);

        let shadow = pipelines.request_shadow_depth().expect("shadow PSO");
        let point = pipelines.request_point_shadow().expect("point-shadow PSO");

        record_shadow_and_point(&device, &targets, &list, &shadow, &point, instance_set)
            .expect("record shadow + point-shadow");

        drop(list);
        drop(shadow);
        drop(point);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(_lighting);
        drop(targets);
        drop(descriptors);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the shadow + point-shadow passes must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Records the directional shadow depth pass (through the graph) + the point-shadow
    /// cube pass (a direct body) on a one-off command buffer and waits — the GPU-runtime
    /// path the renderer's `record_scene_graph` schedules.
    fn record_shadow_and_point(
        device: &Device,
        targets: &Targets,
        list: &SceneDrawList,
        shadow: &Arc<crate::Pipeline>,
        point: &Arc<crate::Pipeline>,
        instance_set: vk::DescriptorSet,
    ) -> Result<()> {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end.
        let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let shadow_extent = vk::Extent2D {
            width: crate::lighting::SHADOW_MAP_SIZE,
            height: crate::lighting::SHADOW_MAP_SIZE,
        };
        let target = PointShadowTarget {
            cube_image: targets.point_shadow.image(),
            face_views: std::array::from_fn(|f| targets.point_shadow.face_view(f)),
            depth_image: targets.point_shadow.depth_image(),
            depth_view: targets.point_shadow.depth_view(),
            extent: targets.point_shadow.extent,
        };
        let faces = point_shadow_face_matrices(G3::new(0.0, 0.0, 5.0), 50.0);

        let record = || -> Result<()> {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the ash seam.
            unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

            // Directional shadow through the graph (DepthWrite → an external slot).
            let mut graph = RenderGraph::new();
            let slot = graph.alloc_external_layout(vk::ImageLayout::UNDEFINED);
            let res = graph.import_image(
                targets.directional_shadow.handle(),
                targets.directional_shadow.view(),
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                Some(slot),
            );
            let shadow_list = list.shallow_clone();
            let raw_body = raw.clone();
            let shadow_pipeline = shadow.handle();
            let shadow_layout = shadow.layout();
            graph.add_pass(
                RgPass::graphics("shadow", shadow_extent)
                    .depth_attachment(crate::RgAttachment {
                        resource: res,
                        load_op: vk::AttachmentLoadOp::CLEAR,
                        store_op: vk::AttachmentStoreOp::STORE,
                        clear_value: vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.0,
                                stencil: 0,
                            },
                        },
                        resolve: None,
                    })
                    .body(move |cmd, _scopes| {
                        record_shadow_depth(
                            &raw_body,
                            cmd,
                            &shadow_list,
                            shadow_pipeline,
                            shadow_layout,
                            instance_set,
                            Mat4::IDENTITY,
                            None,
                        );
                    }),
            );
            graph.execute(device, cmd);

            // The point-shadow cube body (6 face scopes + its own barriers).
            record_point_shadow(
                raw,
                cmd,
                list,
                point.handle(),
                point.layout(),
                instance_set,
                &target,
                &faces,
                G3::new(0.0, 0.0, 5.0),
                50.0,
                None,
                false,
            );

            // SAFETY: the ash seam.
            unsafe { checked(raw.end_command_buffer(cmd), "end")? };
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. Single-threaded queue use in the test.
            unsafe {
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "submit")?;
                checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        };
        let result = record();
        // SAFETY: the ash seam. The fence was waited; the pool/fence are idle.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        result
    }
}
