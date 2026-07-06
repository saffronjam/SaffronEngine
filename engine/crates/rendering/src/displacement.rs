//! GPU **displacement** compute pre-pass. For a displacement-enabled mesh-instance it displaces each
//! base vertex once along its normal by the material's height field into the shared deformed-vertex
//! buffer — the very buffer [`crate::skinning::Skinning`] writes for skinned meshes — so every graphics
//! pass (scene, depth, all shadow maps, GBuffer, motion) and the RT BLAS read one already-displaced
//! static mesh. This is the in-scene analogue of the übershader's per-vertex displacement, but baked
//! into a buffer so it is *consistent across every pass* (the übershader path missed point-shadow
//! cubes) and can back an acceleration structure (phase C1).
//!
//! It mirrors the skinning subsystem exactly, minus buffer ownership: displacement writes into the
//! deformed buffer owned by `Skinning` (both stamp non-overlapping `deformed_offset` slices from the
//! same per-frame cursor), so it owns only its descriptor-set layout (set 1: base vertices in,
//! deformed out) + a per-frame pool. The height map is sampled from the bindless albedo array (set 0,
//! shared with the übershader) by the index carried in the push — no per-instance image descriptor.

use std::sync::Arc;

use ash::vk;

use crate::checked;
use crate::device::Device;
use crate::draw_list::DisplaceDispatch;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{DeviceResources, GpuMesh};

/// Per-frame cap on displaced instances (one descriptor set each), matching the skinning budget.
pub const DISPLACE_MAX_SETS_PER_FRAME: u32 = 64;

/// A displaced bucket the wiring turns into one dispatch: which mesh, where in the shared deformed
/// buffer, and the height-map index + amplitude + uv transform the kernel needs (from the material).
pub struct DisplaceBucket {
    /// The base (undisplaced) mesh whose vertices the kernel reads.
    pub mesh: Arc<GpuMesh>,
    /// This instance's base vertex in the shared deformed buffer.
    pub deformed_offset: u32,
    /// Bindless index of the height map.
    pub height_index: u32,
    /// Local-space displacement amplitude (`MaterialParams.emissive.w`).
    pub height_scale: f32,
    /// `tiling.xy, offset.xy` (`MaterialParams.uv`).
    pub uv_transform: [f32; 4],
    /// Bindless index of the vector-displacement map (`0` = scalar-only along the normal).
    pub vector_index: u32,
}

struct FrameDisplace {
    pool: vk::DescriptorPool,
}

/// The compute-displacement subsystem: a set-1 layout + per-frame descriptor pools. The deformed
/// output buffer belongs to [`crate::skinning::Skinning`]; this only wires dispatches that write it.
pub struct Displacement {
    resources: Arc<DeviceResources>,
    set_layout: vk::DescriptorSetLayout,
    frames: Vec<FrameDisplace>,
}

impl Displacement {
    /// Creates the displace set layout (two compute storage buffers: base vertices in, deformed out)
    /// and one descriptor pool per frame-in-flight.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the layout or a pool cannot be created.
    pub fn new(device: &Device) -> crate::Result<Self> {
        let raw = device.resources().device();
        let set_layout = create_displace_set_layout(raw)?;
        let mut frames: Vec<FrameDisplace> = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let pool = match create_displace_pool(raw) {
                Ok(pool) => pool,
                Err(err) => {
                    for frame in &frames {
                        // SAFETY: the ash seam. Each pool was created above; freed once.
                        unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
                    }
                    // SAFETY: the ash seam. The layout was created above; freed once.
                    unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                    return Err(err);
                }
            };
            frames.push(FrameDisplace { pool });
        }
        Ok(Self {
            resources: Arc::clone(device.resources()),
            set_layout,
            frames,
        })
    }

    /// The displace set-1 layout, for building the compute PSO (`request_displace_pipeline`).
    pub fn set_layout(&self) -> vk::DescriptorSetLayout {
        self.set_layout
    }

    /// Wires the displace dispatches for `frame`: resets the pool, then per bucket allocates a
    /// *current* set (base vertices in → `deformed` out) and a *previous* set (→ `prev_deformed`).
    /// Displacement is static, so both write the same displaced vertices — the prev stream gives the
    /// motion pass a zero deformation delta, leaving pure object motion (`inst.model`/`prevModel`),
    /// exactly like a static mesh. Returns `(current, previous)` dispatch lists, both empty on any
    /// allocation failure (the whole displace pass is then skipped).
    pub fn wire_dispatches(
        &mut self,
        frame: usize,
        deformed: vk::Buffer,
        prev_deformed: vk::Buffer,
        buckets: &[DisplaceBucket],
    ) -> (Vec<DisplaceDispatch>, Vec<DisplaceDispatch>) {
        let raw = self.resources.device();
        let pool = self.frames[frame].pool;
        // SAFETY: the ash seam. This frame slot's prior GPU work was awaited at frame begin, so its
        // sets are free to recycle.
        if let Err(result) =
            unsafe { raw.reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty()) }
        {
            tracing::error!("displacement: reset pool failed: {result:?}");
            return (Vec::new(), Vec::new());
        }
        let mut cur = Vec::with_capacity(buckets.len());
        let mut prev = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            let dispatch = |out| {
                wire_set(raw, pool, self.set_layout, &bucket.mesh, out).map(|set| {
                    DisplaceDispatch {
                        set,
                        vertex_count: bucket.mesh.vertex_count,
                        deformed_offset: bucket.deformed_offset,
                        height_index: bucket.height_index,
                        height_scale: bucket.height_scale,
                        uv_transform: bucket.uv_transform,
                        vector_index: bucket.vector_index,
                    }
                })
            };
            let (Some(c), Some(p)) = (dispatch(deformed), dispatch(prev_deformed)) else {
                return (Vec::new(), Vec::new());
            };
            cur.push(c);
            prev.push(p);
        }
        (cur, prev)
    }

    /// Replays the displace dispatches: binds the PSO + the bindless set (set 0), then per dispatch
    /// binds its buffer set (set 1), pushes the kernel params, and dispatches one group per 64
    /// vertices. A no-op when there are no dispatches.
    pub fn record_displace(
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        bindless_set: vk::DescriptorSet,
        dispatches: &[DisplaceDispatch],
    ) {
        if dispatches.is_empty() {
            return;
        }
        // SAFETY: the ash seam. The PSO + bindless set are valid this frame; set 0 (bindless) is
        // constant across the dispatches, each set 1 wires one bucket's streams.
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[bindless_set],
                &[],
            );
        }
        for d in dispatches {
            let push = DisplacePush {
                vertex_count: d.vertex_count,
                deformed_offset: d.deformed_offset,
                height_index: d.height_index,
                height_scale: d.height_scale,
                uv_transform: d.uv_transform,
                vector_index: d.vector_index,
                _pad: [0; 3],
            };
            // SAFETY: the ash seam. As above; the push spans the declared 48-byte range and the
            // dispatch covers the vertex count (64 per group).
            unsafe {
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    1,
                    &[d.set],
                    &[],
                );
                raw.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                raw.cmd_dispatch(cmd, d.vertex_count.div_ceil(64), 1, 1);
            }
        }
    }
}

/// Requests the `displace` compute PSO: the bindless albedo set 0 (from `descriptors`, so the kernel
/// samples the height map by index) + the displace buffer set 1 (from `displacement`). `None` on a
/// build failure (logged).
pub fn request_displace_pipeline(
    pipelines: &mut crate::Pipelines,
    descriptors: &crate::Descriptors,
    displacement: &Displacement,
) -> Option<Arc<crate::Pipeline>> {
    pipelines.request_displace(descriptors.bindless_set_layout(), displacement.set_layout())
}

impl Drop for Displacement {
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

/// The displace kernel's 48-byte push — matches `displace.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplacePush {
    vertex_count: u32,
    deformed_offset: u32,
    height_index: u32,
    height_scale: f32,
    uv_transform: [f32; 4],
    vector_index: u32,
    _pad: [u32; 3],
}

/// Allocates one displace set from `pool` and writes its two storage-buffer bindings: the mesh's
/// static base vertices (in) and the shared deformed buffer (out). `None` on allocation failure.
fn wire_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    mesh: &GpuMesh,
    deformed: vk::Buffer,
) -> Option<vk::DescriptorSet> {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is reset.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("displacement: allocate set failed: {result:?}");
            return None;
        }
    };
    let infos = [
        vk::DescriptorBufferInfo {
            buffer: mesh.vertex_buffer(),
            offset: 0,
            range: vk::WHOLE_SIZE,
        },
        vk::DescriptorBufferInfo {
            buffer: deformed,
            offset: 0,
            range: vk::WHOLE_SIZE,
        },
    ];
    let writes: Vec<vk::WriteDescriptorSet> = (0..2)
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

/// The displace set-1 layout: two compute-stage storage buffers matching `displace.slang` bindings
/// `(0,1)` and `(1,1)`.
fn create_displace_set_layout(raw: &ash::Device) -> crate::Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..2)
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "displaceSetLayout",
    )
}

/// A per-frame displace pool: two sets per instance (current + previous pose), each two buffers.
fn create_displace_pool(raw: &ash::Device) -> crate::Result<vk::DescriptorPool> {
    let sets = DISPLACE_MAX_SETS_PER_FRAME * 2;
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(sets * 2)];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(sets)
        .pool_sizes(&sizes);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "displacePool",
    )
}
