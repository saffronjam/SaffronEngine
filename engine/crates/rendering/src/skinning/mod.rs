//! The compute skinning pre-pass apparatus.
//!
//! [`Skinning`] owns, per frame-in-flight, a deformed-vertex buffer (base 32-byte
//! [`saffron_geometry::Vertex`] layout, `STORAGE|VERTEX`, grow-only) that every skinned
//! mesh-instance deforms into, plus a parallel prev-deformed buffer the motion pass reads as the
//! prev-position stream. Every geometry pass then reads the result as an ordinary static vertex
//! stream. The per-instance descriptor sets come from a per-frame pool reset wholesale each frame.
//!
//! It also owns the cross-frame motion caches keyed by entity uuid: last frame's palette slice
//! (deformation motion) and last frame's world matrix (object motion). A new entity reads back
//! current == previous, so its first frame emits zero motion.
//!
//! The joint + prev-joint palettes live in [`crate::Instancing`] (set 2, binding 1).

mod morph;
mod skin;

use std::collections::HashMap;
use std::sync::Arc;

use ash::vk;
use saffron_geometry::Vertex;
use saffron_geometry::glam::Mat4;

use crate::draw_list::{MorphDispatch, SkinDispatch};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::pipelines::Pipelines;
use crate::resources::{Buffer, DeviceResources, GpuMesh};
use crate::{Device, Result, checked};

pub use morph::*;
pub use skin::*;

/// Per-frame ceiling on compute-skinning descriptor sets — one per skinned mesh-instance.
/// The skin pool is reset and re-allocated each frame; instances past this are skipped
/// (logged), not an error.
pub const SKIN_MAX_SETS_PER_FRAME: u32 = 64;

/// Initial deformed-vertex-buffer capacity in [`Vertex`] (32-byte) elements.
const INITIAL_DEFORMED_CAPACITY: u32 = 4096;

/// The descriptor-set capacity of each per-frame skin pool. Each skinned instance takes
/// one set for the current dispatch and one for its prev-pose sibling, so the pool holds
/// twice the per-frame instance budget.
const SKIN_POOL_SET_CAPACITY: u32 = SKIN_MAX_SETS_PER_FRAME * 2;

/// One frame-in-flight's skinning storage: the descriptor pool (reset each frame) plus the
/// grow-only deformed + prev-deformed vertex buffers.
struct FrameSkinning {
    pool: vk::DescriptorPool,
    deformed: Option<Buffer>,
    deformed_capacity: u32,
    prev_deformed: Option<Buffer>,
    prev_deformed_capacity: u32,
    /// The morph scatter scratch: 6 × `i32` per vertex (position + normal accumulators),
    /// grow-only, reused across morph instances within a frame (each instance clears its
    /// own `[0, vertexCount)` span, so morph dispatches run serially with barriers).
    accum: Option<Buffer>,
    accum_capacity: u32,
}

/// Fixed-point scale shared with `morph.slang`: `weight·delta` quantizes to `i32` at
/// ~1/65536 precision. Mirrors the shader's `MorphFixedScale`.
pub const MORPH_FIXED_SCALE: f32 = 65536.0;

/// Bytes per vertex in the morph accumulator (6 × `i32`: position xyz + normal xyz).
const ACCUM_STRIDE: u64 = 24;

/// The per-dispatch buffer handles the set wiring binds, resolved once per frame from the
/// frame's deformed buffers and the instance palette buffers.
#[derive(Clone, Copy)]
pub struct SkinBufferSet {
    /// The current joint palette (set 2, binding 1 of [`crate::Instancing`]).
    pub palette: vk::Buffer,
    /// The current palette's bound size in bytes.
    pub palette_size: vk::DeviceSize,
    /// The previous joint palette (the prev-pose dispatch's bound palette).
    pub prev_palette: vk::Buffer,
    /// The previous palette's bound size in bytes.
    pub prev_palette_size: vk::DeviceSize,
}

/// Per-skinned-bucket metadata the deformation gather assembles before wiring: the mesh
/// (its static + skin streams + vertex count) and where the bucket lands in the palette /
/// deformed buffer.
pub struct SkinBucket {
    /// The mesh supplying the static vertex + skin streams (binding 0/1) and the index
    /// stream for the RT refit BLAS.
    pub mesh: Arc<GpuMesh>,
    /// The base of this bucket's joints in the frame palette.
    pub joint_offset: u32,
    /// The base vertex of this bucket's instance in the deformed buffer.
    pub deformed_offset: u32,
}

/// The compute skinning pre-pass apparatus: the per-frame deformed buffers, descriptor pools, and
/// cross-frame motion caches. Each [`Buffer`] holds the allocator `Arc` so it frees without a live
/// `&Device`; [`Drop`] destroys the device-borrowing pools and set layout, which is safe because
/// the run loop idles the GPU before any teardown.
pub struct Skinning {
    resources: Arc<DeviceResources>,
    set_layout: vk::DescriptorSetLayout,
    /// The morph compute set layout (6 storage buffers), matching `morph.slang`.
    morph_set_layout: vk::DescriptorSetLayout,
    frames: Vec<FrameSkinning>,
    peak_vertices: u32,
    /// Last frame's joint palette slice per entity (deformation motion); a missing entry
    /// means "uncached" → the prev palette copies the current one (zero deformation
    /// motion on the first frame).
    prev_palette_by_entity: HashMap<u64, Vec<Mat4>>,
    /// Last frame's morph weights per entity (deformation motion for blend shapes); a
    /// missing entry — or a length change from a different mesh binding — means "uncached"
    /// → the prev weights copy the current ones (zero deformation motion on the first
    /// frame). The twin of [`Self::prev_palette_by_entity`].
    prev_morph_weights_by_entity: HashMap<u64, Vec<f32>>,
    /// Whether RT is supported — when set, the deformed buffer also feeds the per-frame
    /// skinned BLAS refit, so it carries shader-device-address + AS-build-input usage.
    rt_supported: bool,
}

impl Skinning {
    /// Creates the skin descriptor-set layout (four storage buffers: static vertices,
    /// skin, palette, deformed output) and the per-frame descriptor pools.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the layout or a pool cannot be created.
    pub fn new(device: &Device) -> Result<Self> {
        let raw = device.resources().device();
        let set_layout = create_skin_set_layout(raw)?;
        let morph_set_layout = match create_morph_set_layout(raw) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The skin layout was created above; freed once.
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };
        let mut frames: Vec<FrameSkinning> = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let pool = match create_skin_pool(raw) {
                Ok(pool) => pool,
                Err(err) => {
                    // Destroy what was built before propagating (no Drop runs on `Self`).
                    for frame in &frames {
                        // SAFETY: the ash seam. Each pool was created above; freed once.
                        unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
                    }
                    // SAFETY: the ash seam. Both layouts were created above; freed once.
                    unsafe {
                        raw.destroy_descriptor_set_layout(set_layout, None);
                        raw.destroy_descriptor_set_layout(morph_set_layout, None);
                    }
                    return Err(err);
                }
            };
            frames.push(FrameSkinning {
                pool,
                deformed: None,
                deformed_capacity: 0,
                prev_deformed: None,
                prev_deformed_capacity: 0,
                accum: None,
                accum_capacity: 0,
            });
        }
        Ok(Self {
            resources: Arc::clone(device.resources()),
            set_layout,
            morph_set_layout,
            frames,
            peak_vertices: 0,
            prev_palette_by_entity: HashMap::new(),
            prev_morph_weights_by_entity: HashMap::new(),
            rt_supported: device.rt_supported(),
        })
    }

    /// The frame's deformed-vertex buffer handle, or `None` before its first grow. The
    /// scene / depth / shadow passes bind it as binding 0 for a skinned batch.
    pub fn deformed_buffer(&self, frame: usize) -> Option<vk::Buffer> {
        self.frames[frame].deformed.as_ref().map(Buffer::handle)
    }

    /// The frame's prev-deformed-vertex buffer handle, or `None` before its first grow.
    /// The motion pass binds it as binding 1 for a skinned batch (the prev-position
    /// stream).
    pub fn prev_deformed_buffer(&self, frame: usize) -> Option<vk::Buffer> {
        self.frames[frame]
            .prev_deformed
            .as_ref()
            .map(Buffer::handle)
    }

    /// The peak deformed-buffer capacity ever allocated (the grow-only high-water mark in
    /// [`Vertex`] elements). Never shrinks.
    pub fn peak_vertices(&self) -> u32 {
        self.peak_vertices
    }

    /// Ensures both the deformed and prev-deformed buffers hold at least `vertex_count` vertices
    /// (growing if needed) and returns their handles — for a **non-skin deformation** (displacement)
    /// that writes the same shared buffers at its own `deformed_offset` slice. Idempotent: a no-op
    /// when the skin path already sized them for the frame's full deformed cursor.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if a buffer allocation fails.
    pub fn ensure_deformed_buffers(
        &mut self,
        frame: usize,
        vertex_count: u32,
    ) -> Result<(vk::Buffer, vk::Buffer)> {
        self.ensure_deformed_capacity(frame, vertex_count)?;
        self.ensure_prev_deformed_capacity(frame, vertex_count)?;
        let f = &self.frames[frame];
        Ok((
            f.deformed.as_ref().expect("deformed buffer").handle(),
            f.prev_deformed
                .as_ref()
                .expect("prev deformed buffer")
                .handle(),
        ))
    }

    /// Replaces an entity's cached previous palette slice with `current`, then returns the
    /// slice to seed the prev palette: the entity's cached slice when its length matches,
    /// else a copy of `current` (uncached or length-changed → zero deformation motion).
    ///
    /// Committing the cache here (rather than after the loop) is sound because each entity
    /// appears once per deformation frame; the read-then-overwrite order preserves last-frame
    /// semantics for this frame's prev palette.
    pub fn swap_palette(&mut self, entity: u64, current: &[Mat4]) -> Vec<Mat4> {
        let prev = match self.prev_palette_by_entity.get(&entity) {
            Some(cached) if cached.len() == current.len() => cached.clone(),
            _ => current.to_vec(),
        };
        self.prev_palette_by_entity.insert(entity, current.to_vec());
        prev
    }

    /// Replaces an entity's cached previous morph weights with `current`, then returns the
    /// slice to seed the prev-pose morph dispatch: the entity's cached weights when their
    /// length matches, else a copy of `current` (uncached, or a length change from a
    /// different mesh binding → prev == cur → zero deformation motion). The morph twin of
    /// [`Self::swap_palette`]; committing the cache here is sound for the same reason (one
    /// appearance per deformation frame, read-then-overwrite).
    pub fn swap_morph_weights(&mut self, entity: u64, current: &[f32]) -> Vec<f32> {
        let prev = match self.prev_morph_weights_by_entity.get(&entity) {
            Some(cached) if cached.len() == current.len() => cached.clone(),
            _ => current.to_vec(),
        };
        self.prev_morph_weights_by_entity
            .insert(entity, current.to_vec());
        prev
    }

    /// Sizes the frame's deformed + prev-deformed buffers to `vertex_count`, resets the
    /// frame's descriptor pool, and allocates + writes one descriptor set per bucket for
    /// the current dispatch and one for its prev-pose sibling.
    ///
    /// On success the `set` field of each [`SkinDispatch`] in `dispatches` /
    /// `prev_dispatches` (which run parallel to `buckets`) is filled. A wiring failure
    /// clears both lists (so the `skin` pass is skipped) and returns `Ok` — the
    /// dispatches are dropped rather than failing the frame.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if growing a deformed buffer fails.
    /// The frame slot's deformed + prev-deformed buffer device addresses for the
    /// GPU-scene address block (0 when the buffer is not yet built).
    pub fn frame_deformed_addresses(&self, frame: usize, device: &Device) -> (u64, u64) {
        let address = |buffer: &Option<Buffer>| {
            buffer
                .as_ref()
                .map_or(0, |buffer| device.buffer_device_address(buffer.handle()))
        };
        (
            address(&self.frames[frame].deformed),
            address(&self.frames[frame].prev_deformed),
        )
    }

    pub fn wire_dispatches(
        &mut self,
        frame: usize,
        vertex_count: u32,
        buffers: SkinBufferSet,
        buckets: &[SkinBucket],
        dispatches: &mut [SkinDispatch],
        prev_dispatches: &mut [SkinDispatch],
    ) -> Result<bool> {
        debug_assert_eq!(buckets.len(), dispatches.len());
        debug_assert_eq!(buckets.len(), prev_dispatches.len());

        self.ensure_deformed_capacity(frame, vertex_count)?;
        self.ensure_prev_deformed_capacity(frame, vertex_count)?;

        let raw = self.resources.device();
        // SAFETY: the ash seam. The slot's prior GPU work was waited (the caller resets the
        // command pool under the frame fence), so every set in the pool is idle.
        if let Err(result) = unsafe {
            raw.reset_descriptor_pool(
                self.frames[frame].pool,
                vk::DescriptorPoolResetFlags::empty(),
            )
        } {
            tracing::error!("skinning: resetDescriptorPool failed: {result:?}");
            return Ok(false);
        }

        let deformed = self.frames[frame]
            .deformed
            .as_ref()
            .expect("deformed buffer grown");
        let prev_deformed = self.frames[frame]
            .prev_deformed
            .as_ref()
            .expect("prev-deformed buffer grown");
        let deformed_buffer = deformed.handle();
        let deformed_size = deformed.size();
        let prev_deformed_buffer = prev_deformed.handle();
        let prev_deformed_size = prev_deformed.size();

        let pool = self.frames[frame].pool;
        for (i, bucket) in buckets.iter().enumerate() {
            let cur = wire_set(
                raw,
                pool,
                self.set_layout,
                &bucket.mesh,
                buffers.palette,
                buffers.palette_size,
                deformed_buffer,
                deformed_size,
            );
            let prev = wire_set(
                raw,
                pool,
                self.set_layout,
                &bucket.mesh,
                buffers.prev_palette,
                buffers.prev_palette_size,
                prev_deformed_buffer,
                prev_deformed_size,
            );
            match (cur, prev) {
                (Some(cur), Some(prev)) => {
                    dispatches[i].set = cur;
                    prev_dispatches[i].set = prev;
                }
                _ => {
                    // The deformed buffer is unwritten; drop every dispatch so the skin
                    // pass is skipped and the batches read the undeformed bind pose.
                    for d in dispatches.iter_mut() {
                        d.set = vk::DescriptorSet::null();
                    }
                    for d in prev_dispatches.iter_mut() {
                        d.set = vk::DescriptorSet::null();
                    }
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Ensures the frame's deformed buffer holds at least `vertex_count` [`Vertex`]
    /// elements, growing to the next power of two (never shrinking). Logs the new peak.
    fn ensure_deformed_capacity(&mut self, frame: usize, vertex_count: u32) -> Result<()> {
        if self.frames[frame].deformed.is_some()
            && self.frames[frame].deformed_capacity >= vertex_count
        {
            return Ok(());
        }
        let capacity = grow_capacity(self.frames[frame].deformed_capacity, vertex_count);
        let buffer = make_deformed_buffer(&self.resources, capacity, self.rt_supported)?;
        self.frames[frame].deformed = Some(buffer);
        self.frames[frame].deformed_capacity = capacity;
        if capacity > self.peak_vertices {
            self.peak_vertices = capacity;
            tracing::debug!(
                "skinning: deformed-vertex buffer grew to {} vertices ({} KiB)",
                capacity,
                u64::from(capacity) * size_of::<Vertex>() as u64 / 1024
            );
        }
        Ok(())
    }

    /// The prev-deformed sibling of [`Skinning::ensure_deformed_capacity`] (same grow-only
    /// policy, laid out identically so the per-instance offset matches).
    fn ensure_prev_deformed_capacity(&mut self, frame: usize, vertex_count: u32) -> Result<()> {
        if self.frames[frame].prev_deformed.is_some()
            && self.frames[frame].prev_deformed_capacity >= vertex_count
        {
            return Ok(());
        }
        let capacity = grow_capacity(self.frames[frame].prev_deformed_capacity, vertex_count);
        let buffer = make_deformed_buffer(&self.resources, capacity, self.rt_supported)?;
        self.frames[frame].prev_deformed = Some(buffer);
        self.frames[frame].prev_deformed_capacity = capacity;
        Ok(())
    }

    /// Ensures the frame's morph accumulator scratch holds at least `vertex_count` vertices
    /// (`ACCUM_STRIDE` bytes each), growing to the next power of two (never shrinking).
    fn ensure_accum_capacity(&mut self, frame: usize, vertex_count: u32) -> Result<()> {
        if self.frames[frame].accum.is_some() && self.frames[frame].accum_capacity >= vertex_count {
            return Ok(());
        }
        let capacity = grow_capacity(self.frames[frame].accum_capacity, vertex_count);
        let buffer = make_accum_buffer(&self.resources, capacity)?;
        self.frames[frame].accum = Some(buffer);
        self.frames[frame].accum_capacity = capacity;
        Ok(())
    }

    /// Wires two morph descriptor sets per mesh in `meshes`: a cur set (parallel to
    /// `dispatches`, output = the shared `deformed` buffer, read by every geometry pass) and
    /// a prev set (parallel to `prev_dispatches`, output = the `prev_deformed` buffer, read
    /// only by the motion pass). Both bind each mesh's base + delta + range buffers, the
    /// per-frame `active` target buffer (cur/prev `active_base` index its disjoint regions),
    /// and the frame's grown accumulator scratch (reused serially under the per-pass
    /// barriers). Fills each [`MorphDispatch::set`]; clears them all and returns `Ok(false)`
    /// on a wiring failure so the morph pass is skipped.
    ///
    /// `deformed_vertices` is the frame's total deformed-buffer span (skin + morph
    /// slices); `accum_vertices` is the largest single morph mesh (the accumulator is
    /// reused serially per instance). Pass `reset_pool = true` only when the skin wiring
    /// did **not** run this frame (morph-only) — the pool must be reset exactly once before
    /// any set is allocated.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if growing the deformed/accumulator buffers fails.
    #[allow(clippy::too_many_arguments)]
    pub fn wire_morph_dispatches(
        &mut self,
        frame: usize,
        deformed_vertices: u32,
        accum_vertices: u32,
        reset_pool: bool,
        active: vk::Buffer,
        active_size: vk::DeviceSize,
        meshes: &[Arc<GpuMesh>],
        dispatches: &mut [MorphDispatch],
        prev_dispatches: &mut [MorphDispatch],
    ) -> Result<bool> {
        debug_assert_eq!(meshes.len(), dispatches.len());
        debug_assert_eq!(meshes.len(), prev_dispatches.len());
        if meshes.is_empty() {
            return Ok(true);
        }
        self.ensure_deformed_capacity(frame, deformed_vertices)?;
        self.ensure_prev_deformed_capacity(frame, deformed_vertices)?;
        self.ensure_accum_capacity(frame, accum_vertices)?;
        let raw = self.resources.device();
        if reset_pool {
            // SAFETY: the ash seam. The slot's prior GPU work was waited under the frame
            // fence, so every set in the pool is idle.
            if let Err(result) = unsafe {
                raw.reset_descriptor_pool(
                    self.frames[frame].pool,
                    vk::DescriptorPoolResetFlags::empty(),
                )
            } {
                tracing::error!("morph: resetDescriptorPool failed: {result:?}");
                return Ok(false);
            }
        }
        let deformed = self.frames[frame]
            .deformed
            .as_ref()
            .expect("deformed buffer grown");
        let deformed_buffer = deformed.handle();
        let deformed_size = deformed.size();
        let prev_deformed = self.frames[frame]
            .prev_deformed
            .as_ref()
            .expect("prev-deformed buffer grown");
        let prev_deformed_buffer = prev_deformed.handle();
        let prev_deformed_size = prev_deformed.size();
        let accum = self.frames[frame].accum.as_ref().expect("accum grown");
        let accum_buffer = accum.handle();
        let accum_size = accum.size();
        let pool = self.frames[frame].pool;
        // The cur set writes the deformed buffer (read by every geometry pass); the prev
        // set writes the prev-deformed buffer (read only by the motion pass). Both bind the
        // same active-target buffer — cur/prev `active_base` index its disjoint regions.
        for (i, mesh) in meshes.iter().enumerate() {
            let cur = wire_morph_set(
                raw,
                pool,
                self.morph_set_layout,
                mesh,
                active,
                active_size,
                accum_buffer,
                accum_size,
                deformed_buffer,
                deformed_size,
            );
            let prev = wire_morph_set(
                raw,
                pool,
                self.morph_set_layout,
                mesh,
                active,
                active_size,
                accum_buffer,
                accum_size,
                prev_deformed_buffer,
                prev_deformed_size,
            );
            match (cur, prev) {
                (Some(cur), Some(prev)) => {
                    dispatches[i].set = cur;
                    prev_dispatches[i].set = prev;
                }
                _ => {
                    for d in dispatches.iter_mut() {
                        d.set = vk::DescriptorSet::null();
                    }
                    for d in prev_dispatches.iter_mut() {
                        d.set = vk::DescriptorSet::null();
                    }
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Replays the frame's skin dispatches on `cmd`: bind the skin PSO, then per dispatch
    /// bind its set + push (vertexCount / jointOffset / deformedOffset) and dispatch one
    /// group of 64 invocations per vertex. The current pose into the deformed buffer, the
    /// previous pose into the prev-deformed buffer (the kernel is identical).
    pub fn record_skin(
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        dispatches: &[SkinDispatch],
        prev_dispatches: &[SkinDispatch],
    ) {
        if dispatches.is_empty() {
            return;
        }
        // SAFETY: the ash seam. The PSO is valid this frame; each set wires the bucket's
        // static + skin streams, palette, and deformed output; the dispatch covers the
        // vertex count (64 per group).
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
        }
        for d in dispatches.iter().chain(prev_dispatches) {
            if d.set == vk::DescriptorSet::null() {
                continue;
            }
            let push = SkinPush {
                vertex_count: d.vertex_count,
                joint_offset: d.joint_offset,
                deformed_offset: d.deformed_offset,
                pad: 0,
            };
            // SAFETY: the ash seam. As above; the push spans the declared 16-byte range.
            unsafe {
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
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

impl Drop for Skinning {
    fn drop(&mut self) {
        // The per-frame pools + the set layout borrow the device; the `Arc<DeviceResources>`
        // keeps it alive for this call. The run loop idled the GPU before any teardown
        //, so no set in a pool is still in flight. The deformed buffers (the
        // `Option<Buffer>` fields) Drop themselves after this, freeing through the allocator.
        let raw = self.resources.device();
        for frame in &self.frames {
            // SAFETY: the ash seam. The GPU was idled; each pool is freed exactly once.
            unsafe { raw.destroy_descriptor_pool(frame.pool, None) };
        }
        // SAFETY: the ash seam. Both layouts are freed exactly once after the pools.
        unsafe {
            raw.destroy_descriptor_set_layout(self.set_layout, None);
            raw.destroy_descriptor_set_layout(self.morph_set_layout, None);
        }
    }
}

/// The skin kernel's 16-byte push constant — `vertexCount / jointOffset / deformedOffset`
/// plus a pad, matching `skin.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkinPush {
    vertex_count: u32,
    joint_offset: u32,
    deformed_offset: u32,
    pad: u32,
}

#[cfg(test)]
mod tests;
