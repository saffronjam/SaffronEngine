//! The `VK_EXT_mesh_shader` meshlet raster path (phase C2): an alternative opaque front end that
//! draws each mesh-instance's submesh with `cmd_draw_mesh_tasks` over its meshlet decomposition,
//! reusing the übershader `fragmentMain` (so only the geometry stage changes).
//!
//! Capability- **and** opt-in-gated: the renderer engages it only when the device advertises
//! `VK_EXT_mesh_shader` *and* `SAFFRON_MESH_SHADER` is set, so the validated index-draw path stays the
//! default. It mirrors [`crate::displacement::Displacement`]'s per-frame descriptor-pool discipline: one
//! pool per frame-in-flight, reset at wire time, one set-8 (meshlet geometry + base vertex stream) per
//! (submesh, instance) draw. A batch's meshlet vertices are global indices into the mesh's own vertex
//! buffer; a skinned/displaced batch reads the shared deformed buffer instead, with `vertex_base`
//! shifting each index into that batch's deformed slice — exactly the buffer choice the index path makes.

use std::sync::Arc;

use ash::vk;

use crate::checked;
use crate::device::Device;
use crate::draw_list::SceneDrawList;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::DeviceResources;

/// The mesh/task push — matches `meshlet.slang`'s `MeshPush` (80 bytes: a `float4x4` + four `uint`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshletPush {
    /// World → clip (this path's own copy; the vertex path uses the übershader `camera` push).
    pub view_proj: [[f32; 4]; 4],
    /// Row into the shared `instances` buffer (set 2).
    pub instance_index: u32,
    /// First meshlet of this submesh in the mesh's descriptor array.
    pub meshlet_base: u32,
    /// Meshlets in this submesh (the task dispatch size).
    pub meshlet_count: u32,
    /// Added to each global vertex index — the deformed slice for a skinned/displaced batch, else 0.
    pub vertex_base: u32,
}

/// One meshlet-raster draw: its wired set 8 (meshlet + vertex buffers) and the push the mesh/task
/// stages read. Replayed by [`record_meshlet_draws`].
#[derive(Clone, Copy)]
pub struct MeshletDraw {
    set: vk::DescriptorSet,
    push: MeshletPush,
}

struct FrameMeshlet {
    pool: vk::DescriptorPool,
}

/// The meshlet raster subsystem: a set-8 layout + per-frame descriptor pools + the mesh-shader
/// dispatch. The graphics PSO lives in [`crate::Pipelines`] (built from [`MeshletRaster::set_layout`]).
pub struct MeshletRaster {
    resources: Arc<DeviceResources>,
    dispatch: ash::ext::mesh_shader::Device,
    set_layout: vk::DescriptorSetLayout,
    frames: Vec<FrameMeshlet>,
}

impl MeshletRaster {
    /// Creates the set-8 layout (four mesh/task storage buffers) and one descriptor pool per
    /// frame-in-flight. Returns `None` when the device lacks `VK_EXT_mesh_shader` (no dispatch).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the layout or a pool cannot be created.
    pub fn new(device: &Device) -> crate::Result<Option<Self>> {
        let Some(dispatch) = device.mesh_shader_dispatch().cloned() else {
            return Ok(None);
        };
        let raw = device.resources().device();
        let set_layout = create_meshlet_set_layout(raw)?;
        let mut frames: Vec<FrameMeshlet> = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            match create_meshlet_pool(raw) {
                Ok(pool) => frames.push(FrameMeshlet { pool }),
                Err(err) => {
                    for frame in &frames {
                        // SAFETY: the ash seam. Each pool was created above; freed once.
                        unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
                    }
                    // SAFETY: the ash seam. The layout was created above; freed once.
                    unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                    return Err(err);
                }
            }
        }
        Ok(Some(Self {
            resources: Arc::clone(device.resources()),
            dispatch,
            set_layout,
            frames,
        }))
    }

    /// The set-8 layout, for building the meshlet PSO (`Pipelines::request_meshlet`).
    pub fn set_layout(&self) -> vk::DescriptorSetLayout {
        self.set_layout
    }

    /// The `cmd_draw_mesh_tasks` dispatch (cloned into the scene-pass closure that replays the draws).
    pub fn dispatch(&self) -> ash::ext::mesh_shader::Device {
        self.dispatch.clone()
    }

    /// Wires the opaque meshlet draws for `frame`: resets the pool, then per opaque batch → submesh →
    /// instance allocates a set 8 (the mesh's meshlet buffers + its vertex stream — the deformed
    /// buffer for a skinned/displaced batch) and records the push. Returns the wired draws, or `None`
    /// when **any** opaque batch lacks a meshlet decomposition (upload built none) or a set allocation
    /// fails, so the renderer falls back to the whole-list index path rather than dropping a batch.
    pub fn wire(
        &mut self,
        frame: usize,
        list: &SceneDrawList,
        deformed: Option<vk::Buffer>,
    ) -> Option<Vec<MeshletDraw>> {
        if !list.valid || list.batches.is_empty() {
            return None;
        }
        let raw = self.resources.device();
        let pool = self.frames[frame].pool;
        // SAFETY: the ash seam. This frame slot's prior GPU work was awaited at frame begin.
        if let Err(result) =
            unsafe { raw.reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty()) }
        {
            tracing::error!("meshlet: reset pool failed: {result:?}");
            return None;
        }

        let mut draws: Vec<MeshletDraw> = Vec::new();
        let view_proj = list.view_proj.to_cols_array_2d();
        for batch in &list.batches {
            // Any opaque batch without a meshlet decomposition falls the whole list back to the
            // index path — the meshlet path is all-or-nothing, never dropping a batch.
            let meshlets = batch.mesh.meshlets()?;
            let vertex_buffer = match (batch.deformed, deformed) {
                (true, Some(deformed)) => deformed,
                _ => batch.mesh.vertex_buffer(),
            };
            let vertex_base = if batch.deformed {
                batch.deformed_vertex_offset
            } else {
                0
            };

            // The submesh indices this batch draws (a submesh-less mesh is one implicit submesh 0).
            let submesh_indices: Vec<u32> = if batch.mesh.submeshes.is_empty() {
                vec![0]
            } else {
                batch.submeshes.clone()
            };
            for &s in &submesh_indices {
                let Some(&(meshlet_base, meshlet_count)) = meshlets.submesh_ranges.get(s as usize)
                else {
                    continue;
                };
                if meshlet_count == 0 {
                    continue;
                }
                for i in 0..batch.instance_count {
                    let instance_index = batch.base_instance + s * batch.instance_count + i;
                    let set = wire_set(raw, pool, self.set_layout, meshlets, vertex_buffer)?;
                    draws.push(MeshletDraw {
                        set,
                        push: MeshletPush {
                            view_proj,
                            instance_index,
                            meshlet_base,
                            meshlet_count,
                            vertex_base,
                        },
                    });
                }
            }
        }

        Some(draws)
    }
}

/// Replays the wired opaque meshlet draws in the scene-opaque scope: binds the mesh PSO, rebinds the
/// frame's übershader sets 0–7 under the mesh pipeline layout (its push range differs, so the index
/// path's binds do not carry), then per draw binds set 8, pushes, and dispatches one mesh task group
/// per 32 meshlets. A no-op when nothing was wired. A free function (not a method) so the scene-pass
/// `move` closure can call it without borrowing the renderer's [`MeshletRaster`].
#[allow(clippy::too_many_arguments)]
pub fn record_meshlet_draws(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    dispatch: &ash::ext::mesh_shader::Device,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    bindless_set: vk::DescriptorSet,
    light_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    ibl_set: vk::DescriptorSet,
    ssao_mesh_set: vk::DescriptorSet,
    ddgi_mesh_set: vk::DescriptorSet,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
    draws: &[MeshletDraw],
) {
    if draws.is_empty() {
        return;
    }
    let bp = vk::PipelineBindPoint::GRAPHICS;
    // SAFETY: the ash seam. The PSO + sets belong to this frame; each set index matches the meshlet
    // pipeline layout (übershader 0–7 + geometry 8). Optional RT sets bind only when present, exactly
    // as the index path does.
    unsafe {
        raw.cmd_bind_pipeline(cmd, bp, pipeline);
        raw.cmd_bind_descriptor_sets(cmd, bp, layout, 0, &[bindless_set], &[]);
        raw.cmd_bind_descriptor_sets(cmd, bp, layout, 1, &[light_set, instance_set], &[]);
        raw.cmd_bind_descriptor_sets(cmd, bp, layout, 3, &[ibl_set], &[]);
        if ssao_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(cmd, bp, layout, 4, &[ssao_mesh_set], &[]);
        }
        if ddgi_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(cmd, bp, layout, 5, &[ddgi_mesh_set], &[]);
        }
        if rt_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(cmd, bp, layout, 6, &[rt_mesh_set], &[]);
        }
        if restir_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(cmd, bp, layout, 7, &[restir_mesh_set], &[]);
        }
        for draw in draws {
            raw.cmd_bind_descriptor_sets(cmd, bp, layout, 8, &[draw.set], &[]);
            raw.cmd_push_constants(
                cmd,
                layout,
                vk::ShaderStageFlags::MESH_EXT | vk::ShaderStageFlags::TASK_EXT,
                0,
                bytemuck::bytes_of(&draw.push),
            );
            // One task workgroup covers up to 32 meshlets (the task shader's local size).
            dispatch.cmd_draw_mesh_tasks(cmd, draw.push.meshlet_count.div_ceil(32), 1, 1);
        }
    }
}

impl Drop for MeshletRaster {
    fn drop(&mut self) {
        // The run loop idles the GPU before teardown, so no set is still in flight.
        let raw = self.resources.device();
        for frame in &self.frames {
            // SAFETY: the ash seam. Each pool was created in `new`; freed exactly once.
            unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
        }
        // SAFETY: the ash seam. The layout is freed exactly once, after the pools.
        unsafe { raw.destroy_descriptor_set_layout(self.set_layout, None) };
    }
}

/// Allocates one set 8 from `pool` and writes its four bindings: the mesh's meshlet descriptors,
/// meshlet-vertex indices, packed triangle bytes, and the base vertex stream (deformed or static).
/// `None` on allocation failure.
fn wire_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    meshlets: &crate::resources::MeshletBuffers,
    vertex_buffer: vk::Buffer,
) -> Option<vk::DescriptorSet> {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is reset.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("meshlet: allocate set failed: {result:?}");
            return None;
        }
    };
    let buffers = [
        meshlets.descriptors.0,
        meshlets.vertices.0,
        meshlets.triangles.0,
        vertex_buffer,
    ];
    let infos: Vec<vk::DescriptorBufferInfo> = buffers
        .iter()
        .map(|&buffer| vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range: vk::WHOLE_SIZE,
        })
        .collect();
    let writes: Vec<vk::WriteDescriptorSet> = (0..4)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets one binding.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// The set-8 layout: four mesh/task-stage storage buffers matching `meshlet.slang` bindings `(0..3,8)`.
fn create_meshlet_set_layout(raw: &ash::Device) -> crate::Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..4)
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::MESH_EXT | vk::ShaderStageFlags::TASK_EXT)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "meshletSetLayout",
    )
}

/// A per-frame meshlet pool: one set (four storage buffers) per (submesh, instance) draw.
fn create_meshlet_pool(raw: &ash::Device) -> crate::Result<vk::DescriptorPool> {
    let sets = MESHLET_MAX_SETS_PER_FRAME;
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(sets * 4)];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(sets)
        .pool_sizes(&sizes);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "meshletPool",
    )
}

/// Per-frame set budget: one meshlet set per (submesh, instance) opaque draw.
const MESHLET_MAX_SETS_PER_FRAME: u32 = 4096;
