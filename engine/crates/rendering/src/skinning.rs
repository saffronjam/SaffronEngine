//! The compute skinning pre-pass apparatus.
//!
//! [`Skinning`] owns, per frame-in-flight, a deformed-vertex buffer (base 32-byte
//! [`saffron_geometry::Vertex`] layout, `STORAGE|VERTEX`, grow-only) that every skinned
//! mesh-instance deforms into, plus a parallel prev-deformed buffer the motion pass reads
//! as the prev-position stream. The `skin` compute PSO deforms once per vertex; every
//! geometry pass then reads the result as an ordinary static vertex stream — the
//! deform-once win. The per-instance descriptor sets are allocated from a per-frame pool
//! that is reset wholesale each frame.
//!
//! It also owns the cross-frame motion caches keyed by entity uuid: last frame's palette
//! slice (deformation motion) + last frame's world matrix (object motion). A new entity
//! reads back current == previous, so its first frame emits zero motion (no velocity
//! flash). These are single-threaded host state mutated through [`Skinning`]'s methods.
//!
//! The joint + prev-joint palettes live in [`crate::Instancing`] (set 2, binding 1).

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

/// Per-skinned-bucket metadata the draw-list batcher assembles before wiring: the mesh
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

/// The compute skinning pre-pass apparatus: the per-frame deformed buffers + descriptor
/// pools + the cross-frame motion caches. Built once in [`Skinning::new`], then mutated
/// only through [`Skinning::wire_dispatches`] taking `&mut self`. Each [`Buffer`] is a
/// Drop type holding the allocator `Arc`, so the deformed buffers free without a live
/// `&Device`; [`Drop`] destroys the device-borrowing pools + set layout (the run loop
/// idles the GPU before any teardown, README §4).
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
    /// Last frame's world matrix per entity (object motion); a missing entry means the
    /// instance reprojects against itself (zero object motion on the first frame).
    prev_model_by_entity: HashMap<u64, Mat4>,
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
            prev_model_by_entity: HashMap::new(),
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

    /// Looks up the entity's cached previous world matrix, or `None` when uncached (a new
    /// entity reprojects against its current pose → zero object motion on frame one). The
    /// draw-list batcher reads this when building each instance's `prev_model`.
    pub fn prev_model(&self, entity: u64) -> Option<Mat4> {
        self.prev_model_by_entity.get(&entity).copied()
    }

    /// Replaces an entity's cached previous palette slice with `current`, then returns the
    /// slice to seed the prev palette: the entity's cached slice when its length matches,
    /// else a copy of `current` (uncached or length-changed → zero deformation motion).
    ///
    /// Committing the cache here (rather than after the loop) is sound because each entity
    /// appears once per draw list; the read-then-overwrite order preserves last-frame
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
    /// appearance per draw list, read-then-overwrite).
    pub fn swap_morph_weights(&mut self, entity: u64, current: &[f32]) -> Vec<f32> {
        let prev = match self.prev_morph_weights_by_entity.get(&entity) {
            Some(cached) if cached.len() == current.len() => cached.clone(),
            _ => current.to_vec(),
        };
        self.prev_morph_weights_by_entity
            .insert(entity, current.to_vec());
        prev
    }

    /// Commits an entity's current world matrix into the cross-frame cache, so next frame
    /// reprojects from this pose. Called after the prev matrix was read for this frame.
    pub fn commit_model(&mut self, entity: u64, model: Mat4) {
        self.prev_model_by_entity.insert(entity, model);
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
        // (README §4), so no set in a pool is still in flight. The deformed buffers (the
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

/// Allocates one skin descriptor set from `pool` and writes its four storage-buffer
/// bindings (static vertices, skin, palette, deformed output). Returns `None` on an
/// allocation failure (logged), so the caller drops the dispatches.
#[allow(clippy::too_many_arguments)]
fn wire_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    mesh: &GpuMesh,
    palette: vk::Buffer,
    palette_size: vk::DeviceSize,
    deformed: vk::Buffer,
    deformed_size: vk::DeviceSize,
) -> Option<vk::DescriptorSet> {
    let skin = mesh.skin_buffer()?;
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is
    // reset (next frame) or destroyed.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("skinning: allocate skin set failed: {result:?}");
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
            buffer: skin,
            offset: 0,
            range: vk::WHOLE_SIZE,
        },
        vk::DescriptorBufferInfo {
            buffer: palette,
            offset: 0,
            range: palette_size,
        },
        vk::DescriptorBufferInfo {
            buffer: deformed,
            offset: 0,
            range: deformed_size,
        },
    ];
    let writes: Vec<vk::WriteDescriptorSet> = (0..4)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets a
    // single binding the layout declares.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// Grows `current` (a [`Vertex`]-element capacity) to the next power of two that holds
/// `count`, seeding from the initial capacity when empty and never shrinking.
fn grow_capacity(current: u32, count: u32) -> u32 {
    let mut capacity = if current == 0 {
        INITIAL_DEFORMED_CAPACITY
    } else {
        current
    };
    while capacity < count {
        capacity *= 2;
    }
    capacity
}

/// Allocates a device-local deformed-vertex buffer of `capacity` [`Vertex`] elements with
/// `STORAGE|VERTEX` usage. When `rt_supported`, the buffer also feeds the per-frame
/// skinned BLAS refit, so it adds shader-device-address + AS-build-input usage.
fn make_deformed_buffer(
    resources: &Arc<DeviceResources>,
    capacity: u32,
    rt_supported: bool,
) -> Result<Buffer> {
    let size = u64::from(capacity) * size_of::<Vertex>() as u64;
    let mut usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::VERTEX_BUFFER;
    if rt_supported {
        usage |= vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
    }
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(resources, size, usage, &alloc_info)
}

/// The skin set layout: four compute-stage storage buffers (static vertices, skin,
/// palette, deformed output) matching `skin.slang`'s bindings 0-3.
fn create_skin_set_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..4)
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
        "skinSetLayout",
    )
}

/// Creates a per-frame skin descriptor pool sized for [`SKIN_POOL_SET_CAPACITY`] sets,
/// each with four storage buffers.
fn create_skin_pool(raw: &ash::Device) -> Result<vk::DescriptorPool> {
    // Each set holds up to 6 storage buffers (a morph set; a skin set uses 4) out of the
    // shared cur+prev budget, so the descriptor count covers the morph worst case.
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER)
        .descriptor_count(SKIN_POOL_SET_CAPACITY * 6)];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(SKIN_POOL_SET_CAPACITY)
        .pool_sizes(&sizes);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "skinPool",
    )
}

/// Clamps the parallel dispatch / prev-dispatch / bucket / RT lists to
/// [`SKIN_MAX_SETS_PER_FRAME`], logging the overflow (a clamp, not an error). Returns the
/// retained count.
pub fn clamp_to_set_budget(count: usize) -> usize {
    if count > SKIN_MAX_SETS_PER_FRAME as usize {
        tracing::warn!(
            "skinning: {count} skinned instances exceed the {SKIN_MAX_SETS_PER_FRAME}-set frame budget; clamping"
        );
        SKIN_MAX_SETS_PER_FRAME as usize
    } else {
        count
    }
}

/// Requests the `skin` compute PSO from `pipelines`, returning `None` on a build failure
/// (logged). The PSO binds the skin set layout owned by [`Skinning`] and a 16-byte push.
pub fn request_skin_pipeline(
    pipelines: &mut Pipelines,
    skinning: &Skinning,
) -> Option<Arc<crate::Pipeline>> {
    pipelines.request_skin(skinning.set_layout)
}

/// Requests the `morph` compute PSO from `pipelines`, binding the morph set layout owned by
/// [`Skinning`] and a 20-byte push. Returns `None` on a build failure (logged).
pub fn request_morph_pipeline(
    pipelines: &mut Pipelines,
    skinning: &Skinning,
) -> Option<Arc<crate::Pipeline>> {
    pipelines.request_morph(skinning.morph_set_layout)
}

/// The morph kernel's 20-byte push constant, matching `morph.slang`'s `Push`
/// (`vertexCount / scatterCount / activeCount / deformedOffset / pass`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MorphPush {
    vertex_count: u32,
    scatter_count: u32,
    active_count: u32,
    active_base: u32,
    deformed_offset: u32,
    pass: u32,
}

/// The morph set layout: six compute-stage storage buffers (base vertices, deltas, target
/// ranges, active targets, accumulator, deformed output) matching `morph.slang` bindings 0-5.
fn create_morph_set_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..6)
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
        "morphSetLayout",
    )
}

/// Allocates the morph scatter scratch buffer: `ACCUM_STRIDE` (6 × `i32`) per vertex,
/// `STORAGE` only.
fn make_accum_buffer(resources: &Arc<DeviceResources>, capacity: u32) -> Result<Buffer> {
    let size = u64::from(capacity) * ACCUM_STRIDE;
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        &alloc_info,
    )
}

/// Allocates one morph descriptor set from `pool` and writes its six storage-buffer
/// bindings (base vertices, deltas, ranges, active targets, accumulator, deformed output).
/// Returns `None` if the mesh has no morph buffers or the allocation fails (logged).
#[allow(clippy::too_many_arguments)]
pub fn wire_morph_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    mesh: &GpuMesh,
    active: vk::Buffer,
    active_size: vk::DeviceSize,
    accum: vk::Buffer,
    accum_size: vk::DeviceSize,
    deformed: vk::Buffer,
    deformed_size: vk::DeviceSize,
) -> Option<vk::DescriptorSet> {
    let morph = mesh.morph()?;
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is
    // reset (next frame) or destroyed.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("morph: allocate morph set failed: {result:?}");
            return None;
        }
    };
    let infos = [
        (mesh.vertex_buffer(), vk::WHOLE_SIZE),
        (morph.deltas.0, vk::WHOLE_SIZE),
        (morph.ranges.0, vk::WHOLE_SIZE),
        (active, active_size),
        (accum, accum_size),
        (deformed, deformed_size),
    ];
    let buffer_infos: Vec<vk::DescriptorBufferInfo> = infos
        .iter()
        .map(|&(buffer, range)| vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range,
        })
        .collect();
    let writes: Vec<vk::WriteDescriptorSet> = (0..6)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&buffer_infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets a single
    // binding the layout declares.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// Emits a compute→compute memory barrier over the morph accumulator so the next pass (or
/// the next instance's clear) sees the prior pass's writes — the three morph passes and
/// successive instances share the accumulator region.
fn morph_accum_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_STORAGE_READ,
        )
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_STORAGE_READ,
        );
    let deps = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
    // SAFETY: the ash seam. A global memory barrier on the bound command buffer.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &deps) };
}

/// Replays the frame's morph dispatches on `cmd`: bind the morph PSO, then per instance run
/// the three passes (clear → scatter → resolve), inserting an accumulator barrier between
/// passes and after each instance (the accumulator region is shared and reused serially).
/// The cur dispatches deform the current weights into the deformed buffer; the prev
/// dispatches deform the previous-frame weights into the prev-deformed buffer (read by the
/// motion pass) — the kernel is identical, only each set's output buffer differs.
pub fn record_morph(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    dispatches: &[MorphDispatch],
    prev_dispatches: &[MorphDispatch],
) {
    if dispatches.is_empty() {
        return;
    }
    // SAFETY: the ash seam. The PSO is valid this frame.
    unsafe { raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline) };
    for d in dispatches.iter().chain(prev_dispatches) {
        if d.set == vk::DescriptorSet::null() {
            continue;
        }
        // SAFETY: the ash seam. The set wires the instance's buffers; each push spans the
        // declared 24-byte range; the dispatch covers the relevant count (64 per group).
        unsafe {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[d.set],
                &[],
            );
        }
        for pass in 0u32..3 {
            let push = MorphPush {
                vertex_count: d.vertex_count,
                scatter_count: d.scatter_count,
                active_count: d.active_count,
                active_base: d.active_base,
                deformed_offset: d.deformed_offset,
                pass,
            };
            let groups = if pass == 1 {
                d.scatter_count.div_ceil(64)
            } else {
                d.vertex_count.div_ceil(64)
            };
            // SAFETY: the ash seam. As above.
            unsafe {
                raw.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                if groups > 0 {
                    raw.cmd_dispatch(cmd, groups, 1, 1);
                }
            }
            morph_accum_barrier(raw, cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grow policy seeds from the initial capacity when empty, doubles to cover the
    /// vertex count, and never shrinks below the existing capacity — the deformed buffer
    /// grows to fit peak vertices and is not shrunk.
    #[test]
    fn grow_capacity_doubles_and_never_shrinks() {
        assert_eq!(grow_capacity(0, 1), INITIAL_DEFORMED_CAPACITY);
        assert_eq!(grow_capacity(0, INITIAL_DEFORMED_CAPACITY + 1), 8192);
        assert_eq!(grow_capacity(8192, 100), 8192, "never shrinks");
        assert_eq!(grow_capacity(4096, 4096), 4096, "exact fit holds");
        assert_eq!(grow_capacity(4096, 20000), 32768);
    }

    /// Golden deterministic morph math: mirror `morph.slang`'s fixed-point scatter +
    /// resolve on the CPU and prove (a) the integer atomic accumulation is bit-identical
    /// regardless of scatter order — the property that makes the GPU pass reproducible on
    /// llvmpipe and a hardware GPU alike — and (b) the dequantized blend reproduces the
    /// analytic `base + Σ wᵢ·δᵢ` within the `1/MORPH_FIXED_SCALE` quantization step.
    #[test]
    fn morph_fixed_point_scatter_is_order_independent_and_matches_golden() {
        // One vertex, three active targets contributing position deltas at distinct weights.
        let base = [1.0_f32, -2.0, 0.5];
        let targets: [([f32; 3], f32); 3] = [
            ([0.25, 0.0, -0.5], 0.8), // (δ, weight)
            ([-0.125, 0.75, 0.0], 0.3),
            ([0.0625, -0.25, 1.0], 1.0),
        ];

        // Quantize one weighted delta lane exactly as the shader: round(w·δ·scale) as i32.
        let quant = |w: f32, d: f32| (w * d * MORPH_FIXED_SCALE).round() as i32;

        // Scatter in forward order vs. reverse order: integer adds commute, so the two
        // accumulators must be bit-identical (no float-atomic nondeterminism).
        let scatter = |order: &[usize]| -> [i32; 3] {
            let mut acc = [0_i32; 3];
            for &t in order {
                let (d, w) = targets[t];
                for lane in 0..3 {
                    acc[lane] = acc[lane].wrapping_add(quant(w, d[lane]));
                }
            }
            acc
        };
        let forward = scatter(&[0, 1, 2]);
        let reverse = scatter(&[2, 1, 0]);
        assert_eq!(
            forward, reverse,
            "fixed-point integer scatter must be order-independent"
        );

        // Resolve: dequantize and add the base, as the shader's pass 2 does.
        let resolved: [f32; 3] =
            std::array::from_fn(|lane| base[lane] + forward[lane] as f32 / MORPH_FIXED_SCALE);

        // Analytic reference blend.
        let mut golden = base;
        for (d, w) in targets {
            for lane in 0..3 {
                golden[lane] += w * d[lane];
            }
        }

        // Each lane sums three rounded quantities, so the error is bounded by
        // 3·(0.5/scale). Assert well inside that.
        let eps = 3.0 * 0.5 / MORPH_FIXED_SCALE;
        for lane in 0..3 {
            assert!(
                (resolved[lane] - golden[lane]).abs() <= eps,
                "lane {lane}: resolved {} vs golden {} exceeds {eps}",
                resolved[lane],
                golden[lane]
            );
        }
    }

    /// `swap_morph_weights` mirrors the palette swap: an uncached entity returns `current`
    /// (prev == cur ⇒ zero deformation motion); a length change (a different mesh binding)
    /// returns `current`; a same-length second call returns the previously stored slice; and
    /// the cache holds the latest weights after each call.
    #[test]
    fn swap_morph_weights_mirrors_palette_swap() {
        let device = match Device::new(&crate::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let mut skinning = Skinning::new(&device).expect("skinning");

        // Uncached: prev == cur.
        let first = [0.2_f32, 0.8];
        assert_eq!(
            skinning.swap_morph_weights(7, &first),
            first.to_vec(),
            "uncached entity returns current (zero motion on frame 1)"
        );

        // Same-length second call returns the previously stored slice.
        let second = [0.5_f32, 0.1];
        assert_eq!(
            skinning.swap_morph_weights(7, &second),
            first.to_vec(),
            "the previously stored weights are returned as prev"
        );

        // A length change (different mesh binding) returns current, not the stale slice.
        let third = [0.3_f32, 0.4, 0.5];
        assert_eq!(
            skinning.swap_morph_weights(7, &third),
            third.to_vec(),
            "a length change yields prev == cur"
        );

        // The cache now holds the latest (length-3) weights.
        let fourth = [0.9_f32, 0.0, 0.1];
        assert_eq!(
            skinning.swap_morph_weights(7, &fourth),
            third.to_vec(),
            "the cache held the latest weights after the length change"
        );
    }

    /// Dispatches past the per-frame set budget are clamped (and logged), not an error.
    #[test]
    fn clamp_to_set_budget_caps_at_max() {
        assert_eq!(clamp_to_set_budget(10), 10, "under the cap is unchanged");
        assert_eq!(
            clamp_to_set_budget(SKIN_MAX_SETS_PER_FRAME as usize),
            SKIN_MAX_SETS_PER_FRAME as usize,
            "exactly at the cap is unchanged"
        );
        assert_eq!(
            clamp_to_set_budget(SKIN_MAX_SETS_PER_FRAME as usize + 50),
            SKIN_MAX_SETS_PER_FRAME as usize,
            "past the cap clamps to the budget"
        );
    }

    /// The skin compute kernel deforms a known bind pose with a known palette to a
    /// committed golden, validation-clean — the phase's GPU-runtime gate (llvmpipe runs
    /// compute). A translation joint matrix must shift every vertex position by exactly
    /// that translation (the kernel applies the skin matrix without the model matrix) and
    /// leave the normal + UV untouched, and the run must add no validation issues (the
    /// pipeline / descriptor-set wiring + the dispatch are real-GPU-valid).
    #[test]
    fn skin_kernel_deforms_to_golden_validation_clean() {
        use crate::device::SurfaceSource;
        use crate::pipelines::Pipelines;
        use crate::resources::BindlessFreeList;
        use crate::validation_issue_count;
        use ash::vk;
        use saffron_geometry::glam::{Vec2, Vec3};
        use std::sync::Mutex;

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = crate::Descriptors::new(&device, &free_list).expect("Descriptors");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let skinning = Skinning::new(&device).expect("Skinning");
        let skin_pso = request_skin_pipeline(&mut pipelines, &skinning).expect("skin PSO");

        // Three bind-pose vertices weighted fully on joint 0, a palette translating by
        // (10, 20, 30), and a host-visible deformed output we read back.
        let positions = [
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-1.0, 0.0, 5.0),
            Vec3::new(0.5, -2.5, 1.0),
        ];
        let normals = Vec3::new(0.0, 0.0, 1.0);
        let verts: Vec<Vertex> = positions
            .iter()
            .map(|&p| Vertex {
                position: p,
                normal: normals,
                uv0: Vec2::new(0.25, 0.75),
                ..Vertex::default()
            })
            .collect();
        let skins = vec![
            saffron_geometry::VertexSkin {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            3
        ];
        let translation = Vec3::new(10.0, 20.0, 30.0);
        let palette = [Mat4::from_translation(translation)];

        let resources = device.resources();
        let host = |bytes: &[u8], usage: vk::BufferUsageFlags| -> Buffer {
            let info = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            };
            let mut buffer = Buffer::new(resources, bytes.len() as u64, usage, &info)
                .expect("host-visible buffer");
            buffer.mapped_bytes().expect("mapped")[..bytes.len()].copy_from_slice(bytes);
            buffer
        };
        let storage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let in_verts = host(bytemuck::cast_slice(&verts), storage);
        let in_skins = host(bytemuck::cast_slice(&skins), storage);
        let in_palette = host(bytemuck::cast_slice(&palette), storage);
        let out_bytes = vec![0u8; verts.len() * size_of::<Vertex>()];
        let mut out_verts = host(&out_bytes, storage);

        // Wire the skin set against the four raw host buffers (the production path wires
        // it off a GpuMesh; here the bind-pose lives in plain host buffers), then dispatch.
        let set = alloc_and_wire_raw(
            resources.device(),
            skinning.frames[0].pool,
            skinning.set_layout,
            in_verts.handle(),
            in_skins.handle(),
            in_palette.handle(),
            in_palette.size(),
            out_verts.handle(),
            out_verts.size(),
        );

        dispatch_skin(&device, &skin_pso, set, verts.len() as u32);

        device.wait_idle().expect("idle after dispatch");
        let deformed: &[Vertex] = bytemuck::cast_slice(out_verts.mapped_bytes().expect("mapped"));
        for (i, src) in positions.iter().enumerate() {
            let got = deformed[i].position;
            let want = *src + translation;
            assert!(
                (got - want).length() < 1e-4,
                "vertex {i}: deformed {got:?} != golden {want:?}"
            );
            assert!(
                (deformed[i].normal - normals).length() < 1e-4,
                "vertex {i}: normal must survive the identity-rotation skin"
            );
            assert_eq!(
                deformed[i].uv0,
                Vec2::new(0.25, 0.75),
                "vertex {i}: uv passes through unchanged"
            );
        }

        drop(in_verts);
        drop(in_skins);
        drop(in_palette);
        drop(out_verts);
        drop(skin_pso);
        drop(skinning);
        drop(pipelines);
        drop(descriptors);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the skin dispatch must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Allocates one skin set and writes the four raw storage buffers directly — the
    /// test-only sibling of [`wire_set`] that takes raw handles rather than a `GpuMesh`.
    #[allow(clippy::too_many_arguments)]
    fn alloc_and_wire_raw(
        raw: &ash::Device,
        pool: vk::DescriptorPool,
        layout: vk::DescriptorSetLayout,
        verts: vk::Buffer,
        skins: vk::Buffer,
        palette: vk::Buffer,
        palette_size: vk::DeviceSize,
        out: vk::Buffer,
        out_size: vk::DeviceSize,
    ) -> vk::DescriptorSet {
        let layouts = [layout];
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool
        // is reset / destroyed.
        let set = unsafe { raw.allocate_descriptor_sets(&info) }.expect("alloc skin set")[0];
        let whole = vk::WHOLE_SIZE;
        let infos = [
            vk::DescriptorBufferInfo {
                buffer: verts,
                offset: 0,
                range: whole,
            },
            vk::DescriptorBufferInfo {
                buffer: skins,
                offset: 0,
                range: whole,
            },
            vk::DescriptorBufferInfo {
                buffer: palette,
                offset: 0,
                range: palette_size,
            },
            vk::DescriptorBufferInfo {
                buffer: out,
                offset: 0,
                range: out_size,
            },
        ];
        let writes: Vec<vk::WriteDescriptorSet> = (0..4)
            .map(|b| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(b as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(std::slice::from_ref(&infos[b]))
            })
            .collect();
        // SAFETY: the ash seam. Each write targets a binding the layout declares.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };
        set
    }

    /// Records the skin compute dispatch on a one-off command buffer and submits it,
    /// waiting the fence. The deformed buffer is host-visible (mapped), so no copy-out is
    /// needed; a `SHADER_WRITE → HOST_READ` barrier orders the read after the dispatch.
    fn dispatch_skin(
        device: &Device,
        pipeline: &Arc<crate::Pipeline>,
        set: vk::DescriptorSet,
        vertex_count: u32,
    ) {
        use ash::vk;
        let raw = device.resources().device();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end.
        let pool = unsafe { raw.create_command_pool(&pool_info, None) }.expect("pool");
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool.
        let cmd = unsafe { raw.allocate_command_buffers(&alloc) }.expect("cmd")[0];
        // SAFETY: the ash seam. Default fence.
        let fence =
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) }.expect("fence");

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let push = SkinPush {
            vertex_count,
            joint_offset: 0,
            deformed_offset: 0,
            pad: 0,
        };
        // SAFETY: the ash seam. The PSO/set are valid; the dispatch covers the vertices;
        // the host-read barrier orders the mapped read after the compute write.
        unsafe {
            raw.begin_command_buffer(cmd, &begin).expect("begin");
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                pipeline.layout(),
                0,
                &[set],
                &[],
            );
            raw.cmd_push_constants(
                cmd,
                pipeline.layout(),
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(&push),
            );
            raw.cmd_dispatch(cmd, vertex_count.div_ceil(64), 1, 1);
            let mem = vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ);
            let barriers = [mem];
            let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "submit")
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
    }
}
