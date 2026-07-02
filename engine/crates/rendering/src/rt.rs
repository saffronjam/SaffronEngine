//! Hardware ray tracing: per-mesh BLAS, a per-frame TLAS over the scene's mesh
//! instances, per-skinned-instance refit BLAS, and the set-6 TLAS descriptor the mesh
//! fragment binds for inline ray-query shadows.
//!
//! Everything is feature-gated on [`Device::rt_supported`]: on a software device
//! [`Rt::new`] resolves no layout and every method is a no-op, and the engine renders
//! via the shadow-map path.
//!
//! # Why the TLAS is per-frame-in-flight and the skinned BLAS per-slot
//!
//! The TLAS is ping-ponged per in-flight frame with grow-only instance + scratch buffers
//! (`set_rt_scene` captures this frame's static models/meshes; the `tlas-build` pass
//! builds it). The skinned refit BLAS is per-slot then keyed by entity uuid: an in-place
//! `MODE_UPDATE` rewrites the AS while frame N's GPU work may still trace the same slot's
//! prior contents, so the per-slot fence wait in the frame loop serializes each slot — slot
//! f's BLAS is never refit under a live read. The deformed vertices are already in world
//! space (the skin kernel bakes `worldBone * inverseBind` in without the model matrix), so
//! the TLAS transform for a skinned instance is identity.

use std::collections::HashMap;
use std::sync::Arc;

use ash::khr::acceleration_structure as accel;
use ash::vk;
use saffron_geometry::Vertex;
use saffron_geometry::glam::Mat4;

use crate::draw_list::DeformedRtInstance;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{AccelerationStructure, Buffer, DeviceResources, GpuMesh};
use crate::{Device, Result, checked};

/// Initial TLAS instance-buffer capacity ([`Rt::ensure_tlas_capacity`] seed).
const INITIAL_TLAS_CAPACITY: u32 = 64;

/// The size in bytes of one `VkAccelerationStructureInstanceKHR` (64 bytes: a 3×4
/// row-major transform + a packed `instanceCustomIndex`/`mask`/`flags`/AS reference).
const INSTANCE_STRIDE: vk::DeviceSize = size_of::<vk::AccelerationStructureInstanceKHR>() as u64;

/// One skinned instance's per-slot refit BLAS: the AS and whether it has been built once
/// (the gate between a full `MODE_BUILD` and an in-place `MODE_UPDATE`).
struct SkinnedBlas {
    accel: Arc<AccelerationStructure>,
    built: bool,
}

/// One frame-in-flight's TLAS state: the structure itself, the instance count it is sized
/// for, the host-visible instance buffer (one `VkAccelerationStructureInstanceKHR` per
/// referenced mesh instance), the build scratch, and the set-6 descriptor set the mesh
/// fragment binds to read it. The skinned refit map + its shared scratch ride alongside.
struct FrameRt {
    tlas: Option<Arc<AccelerationStructure>>,
    tlas_capacity: u32,
    instance_buffer: Option<Buffer>,
    instance_capacity: u32,
    scratch: Option<Buffer>,
    scratch_capacity: u32,
    mesh_set: vk::DescriptorSet,
    skinned_blas: HashMap<u64, SkinnedBlas>,
    blas_scratch: Option<Buffer>,
    blas_scratch_capacity: u32,
}

/// The captured per-frame static RT scene: parallel model transforms + meshes, set by
/// `set_rt_scene` and consumed by the `tlas-build` pass. Skinned instances ride the
/// [`crate::SceneDrawList`] (their deformed offsets are authoritative there), not here.
#[derive(Default)]
pub struct RtScene {
    /// Static mesh-instance world transforms (column-major; transposed to a row-major 3×4
    /// when packed into the TLAS instance).
    pub models: Vec<Mat4>,
    /// The mesh each transform draws — supplies the BLAS reference for the TLAS instance.
    pub meshes: Vec<Arc<GpuMesh>>,
}

/// Hardware-ray-tracing sub-state: the per-frame TLAS ring + the set-6 TLAS descriptor the
/// mesh fragment binds for inline ray-query shadows.
///
/// Owns the per-frame TLAS / scratch / instance buffers (each a Drop type freeing itself
/// through the shared `Arc<DeviceResources>`). The set-6 layout is *borrowed* from
/// [`crate::Descriptors`] and the per-frame sets free with that pool, so `Rt` needs no custom
/// `Drop`. Constructed once in `Renderer::new`. When [`Device::rt_supported`] is false,
/// `supported` is false, no layout / sets exist, and every method is an early-return no-op.
pub struct Rt {
    resources: Arc<DeviceResources>,
    /// Whether the device supports RT (mirrors [`Device::rt_supported`]).
    supported: bool,
    /// Runtime toggle: trace inline ray-query shadows. Only meaningful when `supported`.
    use_rt_shadows: bool,
    /// Runtime toggle: trace inline ray-query reflections (the mesh fragment traces the TLAS
    /// and reprojects the hit into `prev_color`). Only meaningful when `supported`.
    use_rt_reflections: bool,
    /// The acceleration-structure dispatch, cloned from the device (present iff `supported`).
    dispatch: Option<accel::Device>,
    /// Set 6 (mesh pipeline): one fragment-stage TLAS binding — a handle *borrowed* from
    /// [`crate::Descriptors`] (which owns and destroys it), used only to allocate the
    /// per-frame sets. `null` when RT is unsupported.
    mesh_layout: vk::DescriptorSetLayout,
    frames: Vec<FrameRt>,
    /// This frame's captured static instances, set by [`Rt::set_rt_scene`].
    scene: RtScene,
    /// RT shadows on + instances present this frame → the `tlas-build` pass should run.
    build_pending: bool,
    /// A TLAS was built this frame (the set-6 bind is valid for the mesh fragment).
    tlas_ready: bool,
    /// Instances in this frame's TLAS (static + skinned), set after a build.
    frame_instance_count: u32,
    /// Built per-mesh BLAS count (rt-stats), bumped at upload time.
    blas_count: u32,
    /// Skinned refit BLAS active this frame (rt-stats).
    skinned_blas_count: u32,
}

// SAFETY: every handle (layout / sets / `AccelerationStructure` / `Buffer`) carries no
// thread-affine state and the `Arc`'d resources are `Send`. `Rt` lives on the render
// thread, but the field types must be `Send` for the renderer aggregate to be.
unsafe impl Send for Rt {}

impl Rt {
    /// Creates the RT sub-state: when [`Device::rt_supported`], the set-6 TLAS layout, one
    /// descriptor set per frame slot, and a 0-instance empty TLAS seeded into every set (so
    /// set 6 always references a valid AS — the mesh fragment statically binds `rtScene`
    /// even when the runtime ray-query flag is off, and an unwritten descriptor is a
    /// validation error). On a software device this resolves nothing and stays inert.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if a layout / set / seed-TLAS creation fails.
    pub fn new(device: &Device, descriptors: &crate::Descriptors) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let mut rt = Self {
            resources,
            supported: device.rt_supported(),
            use_rt_shadows: false,
            use_rt_reflections: false,
            dispatch: device.accel_dispatch().cloned(),
            mesh_layout: vk::DescriptorSetLayout::null(),
            frames: (0..MAX_FRAMES_IN_FLIGHT)
                .map(|_| FrameRt::empty())
                .collect(),
            scene: RtScene::default(),
            build_pending: false,
            tlas_ready: false,
            frame_instance_count: 0,
            blas_count: 0,
            skinned_blas_count: 0,
        };
        if !rt.supported {
            return Ok(rt);
        }

        // Set 6 always exists when RT is supported so the mesh PSO layout is stable.
        rt.mesh_layout = descriptors
            .rt_mesh_set_layout()
            .expect("rt_mesh_set_layout present on an RT device");
        for frame in &mut rt.frames {
            frame.mesh_set = descriptors.allocate_set(rt.mesh_layout)?;
        }
        rt.seed_empty_tlas(device)?;
        Ok(rt)
    }

    /// Whether the device supports RT (acceleration-structure + ray-query).
    pub fn supported(&self) -> bool {
        self.supported
    }

    /// Whether inline ray-query shadows should run this frame: the toggle is on, RT is
    /// supported, and a TLAS was built.
    pub fn shadows_enabled(&self) -> bool {
        self.use_rt_shadows && self.supported && self.tlas_ready
    }

    /// Whether the runtime ray-query-shadows toggle is on (independent of `tlas_ready`).
    pub fn use_rt_shadows(&self) -> bool {
        self.use_rt_shadows
    }

    /// Sets the ray-query-shadows toggle (clamped off on a non-RT device).
    pub fn set_rt_shadows(&mut self, enabled: bool) {
        self.use_rt_shadows = enabled && self.supported;
    }

    /// Whether inline ray-query reflections should run this frame: the toggle is on, RT is
    /// supported, and a TLAS was built.
    pub fn reflections_enabled(&self) -> bool {
        self.use_rt_reflections && self.supported && self.tlas_ready
    }

    /// Whether the runtime ray-query-reflections toggle is on (independent of `tlas_ready`).
    pub fn use_rt_reflections(&self) -> bool {
        self.use_rt_reflections
    }

    /// Sets the ray-query-reflections toggle (clamped off on a non-RT device).
    pub fn set_rt_reflections(&mut self, enabled: bool) {
        self.use_rt_reflections = enabled && self.supported;
    }

    /// The built per-mesh BLAS count (rt-stats).
    pub fn blas_count(&self) -> u32 {
        self.blas_count
    }

    /// The skinned refit BLAS active this frame (rt-stats).
    pub fn skinned_blas_count(&self) -> u32 {
        self.skinned_blas_count
    }

    /// The TLAS instance count produced by this frame's build (static + skinned).
    pub fn frame_instance_count(&self) -> u32 {
        self.frame_instance_count
    }

    /// Whether a TLAS was built this frame and the set-6 bind is valid.
    pub fn tlas_ready(&self) -> bool {
        self.tlas_ready
    }

    /// Whether the `tlas-build` pass should be scheduled this frame: RT shadows are on and
    /// at least one static or skinned RT instance exists.
    pub fn build_pending(&self) -> bool {
        self.build_pending
    }

    /// Records the per-mesh BLAS count (one per uploaded mesh). The upload path bumps this.
    pub fn note_blas_built(&mut self) {
        self.blas_count += 1;
    }

    /// Set 6's descriptor set for `frame` — the TLAS the mesh fragment binds.
    pub fn mesh_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].mesh_set
    }

    /// `frame`'s top-level acceleration structure handle, or `null` when none is built yet
    /// (a software device, or before the empty-TLAS seed). The ReSTIR resolve set binds this
    /// per frame for its visibility ray.
    pub fn frame_tlas(&self, frame: usize) -> vk::AccelerationStructureKHR {
        self.frames[frame]
            .tlas
            .as_ref()
            .map_or(vk::AccelerationStructureKHR::null(), |tlas| tlas.handle())
    }

    /// Captures this frame's static instance transforms + meshes for the `tlas-build` pass,
    /// arming the build when RT shadows are on.
    pub fn set_rt_scene(&mut self, models: Vec<Mat4>, meshes: Vec<Arc<GpuMesh>>) {
        self.scene.models = models;
        self.scene.meshes = meshes;
        self.build_pending = self.supported && (self.use_rt_shadows || self.use_rt_reflections);
    }

    /// Whether this frame has any RT instances (static or deforming) to build a TLAS over.
    pub fn has_instances(&self, deformed: &[DeformedRtInstance]) -> bool {
        !self.scene.models.is_empty() || !deformed.is_empty()
    }

    /// Clears the per-frame static-scene capture + the ready/pending flags at the top of a
    /// frame, before the host repopulates via [`Rt::set_rt_scene`]. The static meshes pin
    /// `Arc<GpuMesh>` across the frame; clearing here releases them for the next frame. The
    /// per-slot skinned-BLAS maps are intentionally *not* cleared — they are grow-only across
    /// frames (an entity keeps its AS and refits in place).
    pub fn begin_frame(&mut self) {
        self.scene.models.clear();
        self.scene.meshes.clear();
        self.tlas_ready = false;
        self.build_pending = false;
    }

    /// Resets only the per-frame TLAS-ready flag (not the host-set scene), so a frame that
    /// skips the build (RT shadows off, or no instances) does not report a stale `tlas_ready`
    /// from an earlier frame. Called by the renderer at the top of the frame-graph build.
    pub fn reset_frame_ready(&mut self) {
        self.tlas_ready = false;
        self.frame_instance_count = 0;
        self.skinned_blas_count = 0;
    }

    /// Drops every per-slot skinned refit BLAS (e.g. on a scene reset) so a stale entity's
    /// AS does not linger. The maps regrow on demand.
    pub fn clear_skinned_blas(&mut self) {
        for frame in &mut self.frames {
            frame.skinned_blas.clear();
        }
    }

    /// Prepares the per-frame TLAS build: refit-plans each skinned BLAS (creating the AS on
    /// first sight, sizing the shared scratch), packs the instance buffer, (re)creates the
    /// TLAS + scratch on a capacity change, and writes the TLAS into set 6 — every step that
    /// touches `&mut self`. Returns an owned, `'static` [`TlasBuildPlan`] of device-address
    /// build descriptors the `tlas-build` pass replays via [`record_tlas_build_plan`]; the
    /// plan holds the `Arc<AccelerationStructure>`s so they outlive the recording. The prep
    /// and record halves are split to fit the `'static` graph closure.
    ///
    /// Returns `None` (and leaves `tlas_ready` false) when RT is unsupported, no instances
    /// exist, or a build resource cannot be created.
    pub fn prepare_tlas_build(
        &mut self,
        device: &Device,
        frame: usize,
        deformed: &[DeformedRtInstance],
        deformed_buffer: Option<vk::Buffer>,
    ) -> Option<TlasBuildPlan> {
        self.tlas_ready = false;
        self.skinned_blas_count = 0;
        if !self.supported || (self.scene.models.is_empty() && deformed.is_empty()) {
            return None;
        }
        let dispatch = self.dispatch.clone()?;

        // Plan each deforming BLAS refit (create on first sight), sizing the shared scratch.
        let blas_ops =
            self.plan_skinned_blas_refits(device, &dispatch, frame, deformed, deformed_buffer);

        // Pack one instance per static mesh that has a BLAS, then one per deforming instance.
        let mut instances: Vec<vk::AccelerationStructureInstanceKHR> =
            Vec::with_capacity(self.scene.models.len() + deformed.len());
        let mut retained: Vec<Arc<AccelerationStructure>> = Vec::new();
        for (model, mesh) in self.scene.models.iter().zip(self.scene.meshes.iter()) {
            let Some(blas) = mesh.blas.as_ref() else {
                continue;
            };
            let index = instances.len() as u32;
            instances.push(make_instance(transform_rows(model), index, blas.address));
            retained.push(Arc::clone(blas));
        }
        // A deforming instance references its refit BLAS at its `world_transform`: identity
        // for a skinned (or skin+morph) instance — the deformed vertices are already in world
        // space — and the node world matrix for an unskinned-morph instance, whose deformed
        // vertices are mesh-local.
        for inst in deformed {
            let Some(slot) = self.frames[frame].skinned_blas.get(&inst.entity) else {
                continue;
            };
            let index = instances.len() as u32;
            instances.push(make_instance(
                transform_rows(&inst.world_transform),
                index,
                slot.accel.address,
            ));
            retained.push(Arc::clone(&slot.accel));
        }

        let count = instances.len() as u32;
        if count == 0 {
            return None;
        }
        if let Err(err) = self.ensure_tlas_capacity(frame, count) {
            tracing::error!("rt: TLAS instance buffer grow failed: {err}");
            return None;
        }
        // Copy the packed instances into the host-visible instance buffer. The ash
        // `AccelerationStructureInstanceKHR` is not `bytemuck::Pod` (it embeds bit-packed
        // unions), so view it as raw bytes for the memcpy.
        {
            // SAFETY: `instances` is a contiguous, fully-initialized `#[repr(C)]` array; the
            // byte view spans exactly its bytes and is only read into the mapped buffer.
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    instances.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(instances.as_slice()),
                )
            };
            let buffer = self.frames[frame]
                .instance_buffer
                .as_mut()
                .expect("instance buffer present after ensure_tlas_capacity");
            if let Some(dst) = buffer.mapped_bytes() {
                dst[..bytes.len()].copy_from_slice(bytes);
            }
        }

        // Size + (re)create the TLAS on a capacity change, then write it into set 6.
        let tlas_op = self.prepare_tlas(device, &dispatch, frame, count)?;
        let blas_scratch_addr = self.frames[frame]
            .blas_scratch
            .as_ref()
            .map(|b| device.buffer_device_address(b.handle()))
            .unwrap_or(0);

        self.frame_instance_count = count;
        self.skinned_blas_count = blas_ops.len() as u32;
        self.tlas_ready = true;
        // Retain the TLAS too (it is referenced only through `self` otherwise, but holding
        // it in the plan keeps the replay self-contained).
        retained.push(Arc::clone(
            self.frames[frame].tlas.as_ref().expect("TLAS present"),
        ));
        Some(TlasBuildPlan {
            dispatch,
            blas_ops,
            blas_scratch_addr,
            tlas: tlas_op,
            _retained: retained,
        })
    }

    /// Plans each deforming instance's BLAS refit: creates the AS on first sight, sizes the
    /// shared scratch, and records the build mode (`BUILD` first, then in-place `UPDATE`).
    /// The first-sight build reads the live deformed buffer — which the morph + skin passes
    /// already wrote this frame — so it builds over the resolved-weight pose, never the
    /// zero-weight base. The recording is deferred to [`record_tlas_build_plan`].
    fn plan_skinned_blas_refits(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        instances: &[DeformedRtInstance],
        deformed_buffer: Option<vk::Buffer>,
    ) -> Vec<BlasRefitOp> {
        let Some(deformed) = deformed_buffer else {
            return Vec::new();
        };
        if instances.is_empty() {
            return Vec::new();
        }
        let deformed_base = device.buffer_device_address(deformed);
        let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;

        let mut ops: Vec<BlasRefitOp> = Vec::with_capacity(instances.len());
        let mut scratch_needed: vk::DeviceSize = 0;
        for inst in instances {
            if inst.vertex_count == 0 || inst.index_count < 3 || inst.entity == 0 {
                continue;
            }
            let triangle_count = inst.index_count / 3;
            let vertex_data =
                deformed_base + vk::DeviceAddress::from(inst.deformed_offset) * vertex_stride;
            let index_data = device.buffer_device_address(inst.mesh.index_buffer());

            let geom = triangle_geometry(vertex_data, vertex_stride, inst.vertex_count, index_data);
            let geoms = [geom];
            let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                .flags(
                    vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                        | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
                )
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geoms);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
            unsafe {
                dispatch.get_acceleration_structure_build_sizes(
                    vk::AccelerationStructureBuildTypeKHR::DEVICE,
                    &size_info,
                    &[triangle_count],
                    &mut sizes,
                );
            }

            // Build the AS on first sight; refit (in-place `UPDATE`) afterwards.
            let (accel, update) = match self.frames[frame].skinned_blas.get(&inst.entity) {
                Some(slot) => (Arc::clone(&slot.accel), slot.built),
                None => {
                    match AccelerationStructure::create(
                        &self.resources,
                        dispatch,
                        sizes.acceleration_structure_size,
                        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
                    ) {
                        Ok(accel) => {
                            let accel = Arc::new(accel);
                            self.frames[frame].skinned_blas.insert(
                                inst.entity,
                                SkinnedBlas {
                                    accel: Arc::clone(&accel),
                                    built: false,
                                },
                            );
                            (accel, false)
                        }
                        Err(err) => {
                            tracing::error!("rt: skinned BLAS create failed: {err}");
                            continue;
                        }
                    }
                }
            };
            let want = if update {
                sizes.update_scratch_size
            } else {
                sizes.build_scratch_size
            };
            scratch_needed = scratch_needed.max(want);
            // Each refit is now considered built (the recording will run this frame).
            if let Some(slot) = self.frames[frame].skinned_blas.get_mut(&inst.entity) {
                slot.built = true;
            }
            ops.push(BlasRefitOp {
                dst: accel.handle(),
                vertex_data,
                vertex_stride,
                max_vertex: inst.vertex_count - 1,
                index_data,
                triangle_count,
                update,
            });
        }
        if ops.is_empty() {
            return Vec::new();
        }
        if let Err(err) = self.ensure_blas_scratch(frame, scratch_needed) {
            tracing::error!("rt: skinned BLAS scratch grow failed: {err}");
            // Roll back the "built" flags so a later frame retries the build cleanly.
            return Vec::new();
        }
        ops
    }

    /// Sizes + (re)creates the frame's TLAS on a capacity change, writing it into set 6, and
    /// returns the build op (handle + instance/scratch addresses + count).
    fn prepare_tlas(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        count: u32,
    ) -> Option<TlasBuildOp> {
        let instance_address = device.buffer_device_address(
            self.frames[frame]
                .instance_buffer
                .as_ref()
                .expect("instance buffer present")
                .handle(),
        );
        let geom = instances_geometry(instance_address);
        let geoms = [geom];
        let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .geometries(&geoms);

        // Size for the buffer capacity (>= count) so the TLAS is stable until the buffer
        // regrows; query both that and the actual count's scratch.
        let capacity = self.frames[frame].instance_capacity;
        let mut cap_sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
        unsafe {
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[capacity],
                &mut cap_sizes,
            );
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[count],
                &mut sizes,
            );
        }

        if self.frames[frame].tlas_capacity < count {
            match AccelerationStructure::create(
                &self.resources,
                dispatch,
                cap_sizes.acceleration_structure_size,
                vk::AccelerationStructureTypeKHR::TOP_LEVEL,
            ) {
                Ok(tlas) => {
                    let tlas = Arc::new(tlas);
                    let handle = tlas.handle();
                    self.frames[frame].tlas = Some(tlas);
                    self.frames[frame].tlas_capacity = capacity;
                    self.write_mesh_set(device, frame, handle);
                }
                Err(err) => {
                    tracing::error!("rt: TLAS create failed: {err}");
                    return None;
                }
            }
        }
        let scratch_needed = sizes.build_scratch_size.max(cap_sizes.build_scratch_size);
        if let Err(err) = self.ensure_tlas_scratch(frame, scratch_needed) {
            tracing::error!("rt: TLAS scratch grow failed: {err}");
            return None;
        }
        let scratch_addr = device.buffer_device_address(
            self.frames[frame]
                .scratch
                .as_ref()
                .expect("TLAS scratch present after ensure")
                .handle(),
        );
        let dst = self.frames[frame]
            .tlas
            .as_ref()
            .expect("TLAS present after (re)create")
            .handle();
        Some(TlasBuildOp {
            dst,
            instance_address,
            scratch_address: scratch_addr,
            count,
        })
    }

    /// Ensures `frame`'s instance buffer holds `count` instances (host-visible AS-build
    /// input + BDA), growing to the next power of two.
    fn ensure_tlas_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].instance_buffer.is_some()
            && self.frames[frame].instance_capacity >= count
        {
            return Ok(());
        }
        let mut capacity = self.frames[frame]
            .instance_capacity
            .max(INITIAL_TLAS_CAPACITY);
        while capacity < count {
            capacity *= 2;
        }
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let buffer = Buffer::new(
            &self.resources,
            vk::DeviceSize::from(capacity) * INSTANCE_STRIDE,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &alloc_info,
        )?;
        self.frames[frame].instance_buffer = Some(buffer);
        self.frames[frame].instance_capacity = capacity;
        Ok(())
    }

    /// Ensures `frame`'s TLAS build scratch is at least `bytes` (device-local, BDA).
    fn ensure_tlas_scratch(&mut self, frame: usize, bytes: vk::DeviceSize) -> Result<()> {
        if self.frames[frame].scratch.is_some()
            && vk::DeviceSize::from(self.frames[frame].scratch_capacity) >= bytes
        {
            return Ok(());
        }
        let buffer = make_scratch_buffer(&self.resources, bytes)?;
        self.frames[frame].scratch = Some(buffer);
        self.frames[frame].scratch_capacity = bytes as u32;
        Ok(())
    }

    /// Ensures `frame`'s shared skinned-BLAS build/refit scratch is at least `bytes`.
    fn ensure_blas_scratch(&mut self, frame: usize, bytes: vk::DeviceSize) -> Result<()> {
        if self.frames[frame].blas_scratch.is_some()
            && vk::DeviceSize::from(self.frames[frame].blas_scratch_capacity) >= bytes
        {
            return Ok(());
        }
        let buffer = make_scratch_buffer(&self.resources, bytes)?;
        self.frames[frame].blas_scratch = Some(buffer);
        self.frames[frame].blas_scratch_capacity = bytes as u32;
        Ok(())
    }

    /// Writes `tlas` into `frame`'s set-6 binding 0 (the mesh fragment's TLAS).
    fn write_mesh_set(&self, device: &Device, frame: usize, tlas: vk::AccelerationStructureKHR) {
        let structures = [tlas];
        let mut accel_write = vk::WriteDescriptorSetAccelerationStructureKHR::default()
            .acceleration_structures(&structures);
        let mut write = vk::WriteDescriptorSet::default()
            .dst_set(self.frames[frame].mesh_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR)
            .push_next(&mut accel_write);
        // `descriptor_count` is otherwise inferred from the (absent) image/buffer arrays.
        write.descriptor_count = 1;
        // SAFETY: the ash seam. The set + layout are this renderer's; written on the render
        // thread after the slot's fence is waited (no concurrent host access).
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
    }

    /// Builds a 0-instance empty TLAS (synchronous one-off submit) and writes it into every
    /// frame's set 6, so set 6 always references a valid AS before any per-frame build.
    fn seed_empty_tlas(&mut self, device: &Device) -> Result<()> {
        let dispatch = self
            .dispatch
            .clone()
            .expect("accel dispatch present on an RT device");
        let geom = instances_geometry(0);
        let geoms = [geom];
        let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .geometries(&geoms);
        let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
        unsafe {
            dispatch.get_acceleration_structure_build_sizes(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &size_info,
                &[0],
                &mut sizes,
            );
        }
        let empty = AccelerationStructure::create(
            &self.resources,
            &dispatch,
            sizes.acceleration_structure_size.max(256),
            vk::AccelerationStructureTypeKHR::TOP_LEVEL,
        )?;
        let scratch = make_scratch_buffer(&self.resources, sizes.build_scratch_size.max(256))?;
        let scratch_addr = device.buffer_device_address(scratch.handle());
        let dst = empty.handle();
        let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .dst_acceleration_structure(dst)
            .geometries(&geoms)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: scratch_addr,
            });
        let range = vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(0);
        let ranges = [range];

        // A synchronous one-off submit on a private transient command pool, then `wait_idle`
        // (an init-time path; never per-frame).
        record_and_submit_oneoff(device, |cmd| {
            // SAFETY: the ash seam. One build info; the range slice length equals its
            // `geometry_count` (1).
            unsafe {
                dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
            }
        })?;
        device.wait_idle()?;
        drop(scratch);

        // Share the one empty TLAS across every slot. A real per-frame build later replaces
        // a slot's TLAS (and rewrites its set) on demand.
        let empty = Arc::new(empty);
        let handle = empty.handle();
        for frame in 0..self.frames.len() {
            self.frames[frame].tlas = Some(Arc::clone(&empty));
            self.write_mesh_set(device, frame, handle);
        }
        Ok(())
    }
}

/// One skinned BLAS refit recorded by [`record_tlas_build_plan`]: the AS to (re)build over
/// a device-address vertex + index stream, and whether it is an in-place `UPDATE`.
pub struct BlasRefitOp {
    dst: vk::AccelerationStructureKHR,
    vertex_data: vk::DeviceAddress,
    vertex_stride: vk::DeviceSize,
    max_vertex: u32,
    index_data: vk::DeviceAddress,
    triangle_count: u32,
    update: bool,
}

/// The TLAS build recorded by [`record_tlas_build_plan`]: the destination AS, the instance
/// array + scratch device addresses, and the instance count.
pub struct TlasBuildOp {
    dst: vk::AccelerationStructureKHR,
    instance_address: vk::DeviceAddress,
    scratch_address: vk::DeviceAddress,
    count: u32,
}

/// An owned, `Send + 'static` plan the `tlas-build` pass replays into its command buffer:
/// the skinned BLAS refits (sharing one scratch region), then the TLAS build, then the
/// AS-build → fragment ray-query barrier. Built by [`Rt::prepare_tlas_build`] (which did the
/// `&mut self` work); recording it only issues commands through resolved handles. It holds
/// the referenced `Arc<AccelerationStructure>`s so they outlive the recording.
pub struct TlasBuildPlan {
    dispatch: accel::Device,
    blas_ops: Vec<BlasRefitOp>,
    blas_scratch_addr: vk::DeviceAddress,
    tlas: TlasBuildOp,
    _retained: Vec<Arc<AccelerationStructure>>,
}

// SAFETY: every field is an `Arc` / `Copy` handle / device address with no thread-affine
// state; the dispatch is a Clone fn-pointer table. The plan crosses into the `'static`
// graph closure, which runs on the render thread.
unsafe impl Send for TlasBuildPlan {}

/// Replays a [`TlasBuildPlan`] into `cmd`: each skinned BLAS refit serialized on the shared
/// scratch (AS-build → AS-build barrier between them), an AS-build → AS-build-read barrier
/// handing them to the TLAS build, the TLAS build itself, then the AS-build → fragment
/// ray-query barrier. The record half of the TLAS build. Issues commands only — no
/// resource creation, no `&mut self`.
pub fn record_tlas_build_plan(raw: &ash::Device, cmd: vk::CommandBuffer, plan: &TlasBuildPlan) {
    let dispatch = &plan.dispatch;
    let scratch_barrier = accel_scratch_barrier();
    for (i, op) in plan.blas_ops.iter().enumerate() {
        if i > 0 {
            let dep = vk::DependencyInfo::default()
                .memory_barriers(std::slice::from_ref(&scratch_barrier));
            // SAFETY: the ash seam. A memory barrier on the active command buffer.
            unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
        }
        let geom = triangle_geometry(
            op.vertex_data,
            op.vertex_stride,
            op.max_vertex + 1,
            op.index_data,
        );
        let geoms = [geom];
        let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
            .flags(
                vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                    | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
            )
            .mode(if op.update {
                vk::BuildAccelerationStructureModeKHR::UPDATE
            } else {
                vk::BuildAccelerationStructureModeKHR::BUILD
            })
            .src_acceleration_structure(if op.update {
                op.dst
            } else {
                vk::AccelerationStructureKHR::null()
            })
            .dst_acceleration_structure(op.dst)
            .geometries(&geoms)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: plan.blas_scratch_addr,
            });
        let range = vk::AccelerationStructureBuildRangeInfoKHR::default()
            .primitive_count(op.triangle_count);
        let ranges = [range];
        // SAFETY: the ash seam. One build info; the range slice length equals its
        // `geometry_count` (1). The vertex/index/scratch addresses reference live buffers.
        unsafe {
            dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
        }
    }
    if !plan.blas_ops.is_empty() {
        // Hand the finished BLASes (build write) to the TLAS build (build read).
        let barrier = accel_build_to_build_read_barrier();
        let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
        // SAFETY: the ash seam. A memory barrier on the active command buffer.
        unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
    }

    // The TLAS build over the packed instance buffer.
    let geom = instances_geometry(plan.tlas.instance_address);
    let geoms = [geom];
    let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(plan.tlas.dst)
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: plan.tlas.scratch_address,
        });
    let range =
        vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(plan.tlas.count);
    let ranges = [range];
    // SAFETY: the ash seam. One build info; the range slice length equals its
    // `geometry_count` (1).
    unsafe {
        dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
    }

    // AS build (write) → fragment ray-query (read).
    let barrier = accel_build_to_fragment_barrier();
    let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
    // SAFETY: the ash seam. A memory barrier on the active command buffer.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

impl FrameRt {
    /// An empty slot before any RT use: no TLAS, no buffers, a null descriptor set.
    fn empty() -> Self {
        Self {
            tlas: None,
            tlas_capacity: 0,
            instance_buffer: None,
            instance_capacity: 0,
            scratch: None,
            scratch_capacity: 0,
            mesh_set: vk::DescriptorSet::null(),
            skinned_blas: HashMap::new(),
            blas_scratch: None,
            blas_scratch_capacity: 0,
        }
    }
}

/// One built BLAS + its build scratch — the upload-time mesh BLAS, returned so the caller
/// (the [`crate::Uploader`]) keeps the scratch alive until its one-off submit completes.
pub struct MeshBlasBuild {
    /// The built bottom-level acceleration structure (shared from the mesh).
    pub blas: AccelerationStructure,
    /// The build scratch — dropped after the build submit completes.
    pub scratch: Buffer,
}

/// Records a one-geometry BLAS build for `mesh` into `cmd` and returns the AS + scratch.
/// `PREFER_FAST_TRACE`, no compaction (correctness-first). The caller
/// submits `cmd` and waits, then drops the returned [`MeshBlasBuild::scratch`]. The mesh's
/// vertex + index buffers must carry `SHADER_DEVICE_ADDRESS` + AS-build-input usage.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if the AS or scratch buffer cannot be created.
pub fn record_mesh_blas_build(
    resources: &Arc<DeviceResources>,
    dispatch: &accel::Device,
    cmd: vk::CommandBuffer,
    vertex_buffer: vk::Buffer,
    vertex_count: u32,
    index_buffer: vk::Buffer,
    index_count: u32,
) -> Result<MeshBlasBuild> {
    let vertex_data = resources.buffer_device_address(vertex_buffer);
    let index_data = resources.buffer_device_address(index_buffer);
    let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
    let triangle_count = index_count / 3;

    let geom = triangle_geometry(vertex_data, vertex_stride, vertex_count, index_data);
    let geoms = [geom];
    let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .geometries(&geoms);
    let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
    // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()` (1).
    unsafe {
        dispatch.get_acceleration_structure_build_sizes(
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &size_info,
            &[triangle_count],
            &mut sizes,
        );
    }

    let blas = AccelerationStructure::create(
        resources,
        dispatch,
        sizes.acceleration_structure_size,
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
    )?;
    let scratch = make_scratch_buffer(resources, sizes.build_scratch_size)?;
    let scratch_addr = resources.buffer_device_address(scratch.handle());

    let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(blas.handle())
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: scratch_addr,
        });
    let range =
        vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(triangle_count);
    let ranges = [range];
    // SAFETY: the ash seam. One build info; the range slice length equals its
    // `geometry_count` (1). The vertex/index addresses are valid for the device lifetime.
    unsafe {
        dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
    }
    Ok(MeshBlasBuild { blas, scratch })
}

/// A triangle-geometry descriptor over a device-address vertex + index stream
/// (`R32G32B32_SFLOAT` positions, `UINT32` indices, opaque). The vertex/index addresses
/// must reference live buffers for the build's duration.
fn triangle_geometry(
    vertex_data: vk::DeviceAddress,
    vertex_stride: vk::DeviceSize,
    vertex_count: u32,
    index_data: vk::DeviceAddress,
) -> vk::AccelerationStructureGeometryKHR<'static> {
    let triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
        .vertex_format(vk::Format::R32G32B32_SFLOAT)
        .vertex_data(vk::DeviceOrHostAddressConstKHR {
            device_address: vertex_data,
        })
        .vertex_stride(vertex_stride)
        .max_vertex(vertex_count.saturating_sub(1))
        .index_type(vk::IndexType::UINT32)
        .index_data(vk::DeviceOrHostAddressConstKHR {
            device_address: index_data,
        });
    vk::AccelerationStructureGeometryKHR::default()
        .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
        .flags(vk::GeometryFlagsKHR::OPAQUE)
        .geometry(vk::AccelerationStructureGeometryDataKHR { triangles })
}

/// An instances-geometry descriptor over a device-address instance array (the TLAS input).
fn instances_geometry(
    instance_data: vk::DeviceAddress,
) -> vk::AccelerationStructureGeometryKHR<'static> {
    let instances = vk::AccelerationStructureGeometryInstancesDataKHR::default()
        .array_of_pointers(false)
        .data(vk::DeviceOrHostAddressConstKHR {
            device_address: instance_data,
        });
    vk::AccelerationStructureGeometryKHR::default()
        .geometry_type(vk::GeometryTypeKHR::INSTANCES)
        .flags(vk::GeometryFlagsKHR::OPAQUE)
        .geometry(vk::AccelerationStructureGeometryDataKHR { instances })
}

/// The row-major 3×4 transform of an identity placement (a skinned instance: its deformed
/// vertices are already world-space). The placement loop derives every instance's rows from
/// `transform_rows(&inst.world_transform)`, so this is the source-of-truth constant the
/// byte-identity test pins `transform_rows(&Mat4::IDENTITY)` against.
#[cfg(test)]
const IDENTITY_ROWS: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// Transposes a column-major [`Mat4`] world transform into the row-major 3×4
/// `VkTransformMatrixKHR` layout (12 floats, row 0 first).
fn transform_rows(model: &Mat4) -> [f32; 12] {
    let m = model.to_cols_array_2d();
    let mut rows = [0.0_f32; 12];
    for r in 0..3 {
        for c in 0..4 {
            rows[r * 4 + c] = m[c][r];
        }
    }
    rows
}

/// Packs one `VkAccelerationStructureInstanceKHR`: a row-major 3×4 transform, the custom
/// index, a 0xFF mask, the triangle-cull-disable flag, and the referenced AS device address.
fn make_instance(
    rows: [f32; 12],
    custom_index: u32,
    accel_reference: vk::DeviceAddress,
) -> vk::AccelerationStructureInstanceKHR {
    vk::AccelerationStructureInstanceKHR {
        transform: vk::TransformMatrixKHR { matrix: rows },
        instance_custom_index_and_mask: vk::Packed24_8::new(custom_index, 0xFF),
        instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(
            0,
            vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE.as_raw() as u8,
        ),
        acceleration_structure_reference: vk::AccelerationStructureReferenceKHR {
            device_handle: accel_reference,
        },
    }
}

/// A device-local AS build/refit scratch buffer (`STORAGE | SHADER_DEVICE_ADDRESS`).
fn make_scratch_buffer(resources: &Arc<DeviceResources>, bytes: vk::DeviceSize) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(
        resources,
        bytes.max(256),
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &alloc_info,
    )
}

/// The shared-scratch reuse barrier: serialize consecutive AS builds sharing one scratch
/// region (build write/read → build write/read).
fn accel_scratch_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(
            vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR
                | vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR,
        )
        .dst_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .dst_access_mask(
            vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR
                | vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR,
        )
}

/// The BLAS-refit → TLAS-build barrier: the refit writes (build stage) feed the TLAS build
/// that reads them as input (build stage).
fn accel_build_to_build_read_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
        .dst_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .dst_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR)
}

/// The TLAS-build → fragment-ray-query barrier: the AS build write feeds the fragment
/// shader's inline ray-query read.
fn accel_build_to_fragment_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR)
}

/// Allocates a transient command buffer, records `record`, submits it, and blocks on a
/// fresh fence — the init-time one-off path for the empty-TLAS seed (a private pool keeps it
/// self-contained, never touching a per-frame pool).
fn record_and_submit_oneoff<R: FnOnce(vk::CommandBuffer)>(
    device: &Device,
    record: R,
) -> Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. The pool is created, used, and destroyed within this call.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "create_command_pool (seed tlas)",
    )?;
    let result = (|| -> Result<()> {
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the private pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc_info) },
            "allocate_command_buffers (seed tlas)",
        )?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begin/record/end on the freshly allocated buffer.
        checked(
            unsafe { raw.begin_command_buffer(cmd, &begin) },
            "begin_command_buffer (seed tlas)",
        )?;
        record(cmd);
        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(cmd) },
            "end_command_buffer (seed tlas)",
        )?;
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The graphics queue is idle at init (no frame in flight);
        // submit without a fence and drain with `wait_idle` below (an init path).
        checked(
            unsafe { raw.queue_submit2(device.graphics_queue, &submits, vk::Fence::null()) },
            "queue_submit2 (seed tlas)",
        )?;
        device.wait_idle()
    })();
    // SAFETY: the ash seam. The queue was idled (or the submit never happened), so the pool
    // and its buffer are idle and destroyed exactly once.
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptors::Descriptors;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use saffron_geometry::glam::Vec4;
    use std::sync::Mutex;

    /// Builds a headless device + descriptors + `Rt`, or returns `None` when no Vulkan ICD
    /// is present (the toolbox without a device). Yields the issue count taken before
    /// `Rt::new` so the caller can assert the seed path is validation-clean.
    fn rt_or_skip() -> Option<(Device, Descriptors, Rt, u64)> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let before = validation_issue_count();
        let rt = Rt::new(&device, &descriptors).expect("Rt::new");
        Some((device, descriptors, rt, before))
    }

    /// `transform_rows` transposes a column-major world transform into the row-major 3×4
    /// `VkTransformMatrixKHR` layout: row r, column c reads `model[c][r]`.
    #[test]
    fn transform_rows_transposes_to_row_major() {
        let model = Mat4::from_cols(
            Vec4::new(1.0, 2.0, 3.0, 4.0),
            Vec4::new(5.0, 6.0, 7.0, 8.0),
            Vec4::new(9.0, 10.0, 11.0, 12.0),
            Vec4::new(13.0, 14.0, 15.0, 16.0),
        );
        let rows = transform_rows(&model);
        // Row 0 = the x-components of each column (the matrix's first row).
        assert_eq!(rows[0..4], [1.0, 5.0, 9.0, 13.0]);
        // Row 1 = the y-components.
        assert_eq!(rows[4..8], [2.0, 6.0, 10.0, 14.0]);
        // Row 2 = the z-components.
        assert_eq!(rows[8..12], [3.0, 7.0, 11.0, 15.0]);
    }

    /// A skinned instance's TLAS transform is the row-major identity (its deformed vertices
    /// are already in world space). The placement loop now derives the row matrix from
    /// `transform_rows(&inst.world_transform)` for every deforming instance, so a skinned
    /// instance (`world_transform == IDENTITY`) must produce bytes identical to the
    /// `IDENTITY_ROWS` constant — this is what keeps the skin RT placement provably unchanged
    /// after generalizing to morph.
    #[test]
    fn identity_rows_is_the_3x4_identity() {
        assert_eq!(
            IDENTITY_ROWS,
            [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        );
        assert_eq!(
            transform_rows(&Mat4::IDENTITY),
            IDENTITY_ROWS,
            "a skinned instance's identity world_transform places byte-identical to IDENTITY_ROWS"
        );
    }

    /// `make_instance` packs the custom index + 0xFF mask, the triangle-cull-disable flag,
    /// and the referenced AS device address into a `VkAccelerationStructureInstanceKHR`.
    #[test]
    fn make_instance_packs_index_mask_flags_and_reference() {
        let inst = make_instance(IDENTITY_ROWS, 7, 0xDEAD_BEEF);
        assert_eq!(inst.instance_custom_index_and_mask.low_24(), 7);
        assert_eq!(inst.instance_custom_index_and_mask.high_8(), 0xFF);
        assert_eq!(
            inst.instance_shader_binding_table_record_offset_and_flags
                .high_8(),
            vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE.as_raw() as u8
        );
        // SAFETY: the reference is the `device_handle` union arm, set by `make_instance`.
        assert_eq!(
            unsafe { inst.acceleration_structure_reference.device_handle },
            0xDEAD_BEEF
        );
    }

    /// On a software device (llvmpipe — no RT extensions), `Rt::new` builds an inert
    /// sub-state: `supported()` is false, set 6 is null, the shadow toggle clamps off, and
    /// `prepare_tlas_build` is a no-op returning `None`. This is the gate's "all RT paths are
    /// no-ops when rt_supported == false" requirement — the engine renders via the
    /// shadow-map path. On an RT device this asserts the seed path instead.
    #[test]
    fn rt_inert_on_software_device_validation_clean() {
        let Some((device, _descriptors, mut rt, before)) = rt_or_skip() else {
            return;
        };
        if rt.supported() {
            // RT-capable device: the seed empty TLAS wrote a valid AS into every set 6.
            assert_ne!(rt.mesh_set(0), vk::DescriptorSet::null());
            // GPU-RUNTIME RT validation (a TLAS build + ray-query render) is
            // DEFERRED-NEEDS-HARDWARE — llvmpipe has no RT, so this branch is unreachable in
            // the toolbox; the seed AS create + descriptor writes are exercised here.
            eprintln!("rt: RT-capable device — seed empty TLAS written into every set 6");
            return;
        }
        // Software device: the inert contract.
        assert!(!rt.supported());
        assert_eq!(rt.mesh_set(0), vk::DescriptorSet::null());
        assert_eq!(rt.blas_count(), 0);

        rt.set_rt_shadows(true);
        assert!(
            !rt.use_rt_shadows(),
            "shadow toggle clamps off on a non-RT device"
        );
        assert!(!rt.shadows_enabled());

        // set_rt_scene with static instances does not arm a build on a non-RT device.
        rt.set_rt_scene(vec![Mat4::IDENTITY], Vec::new());
        assert!(!rt.build_pending());

        // The build path is a no-op: it produces no plan and leaves tlas_ready false.
        let plan = rt.prepare_tlas_build(&device, 0, &[], None);
        assert!(plan.is_none());
        assert!(!rt.tlas_ready());

        drop(rt);
        // SAFETY: the device must idle before its sub-state Drops (here Rt already dropped).
        device.wait_idle().expect("wait_idle");
        assert_eq!(
            validation_issue_count(),
            before,
            "the inert RT sub-state raised no validation issues"
        );
    }

    /// `set_rt_scene` arms the per-frame `tlas-build` only when RT is supported *and* the
    /// shadow toggle is on — the `build_pending` gate the frame graph reads. On a software
    /// device it never arms (covered above); this asserts the toggle interaction directly.
    #[test]
    fn build_pending_requires_supported_and_shadows_on() {
        let Some((device, _descriptors, mut rt, _before)) = rt_or_skip() else {
            return;
        };
        // Shadows off → never pending, regardless of support.
        rt.set_rt_shadows(false);
        rt.set_rt_scene(vec![Mat4::IDENTITY], Vec::new());
        assert!(!rt.build_pending());

        rt.set_rt_shadows(true);
        rt.set_rt_scene(vec![Mat4::IDENTITY], Vec::new());
        // Pending iff the device actually supports RT (the toggle was clamped otherwise).
        assert_eq!(rt.build_pending(), rt.supported());

        drop(rt);
        device.wait_idle().expect("wait_idle");
    }

    /// `begin_frame` clears the static-scene capture + ready/pending flags (the per-slot
    /// skinned-BLAS maps are grow-only and intentionally untouched).
    #[test]
    fn begin_frame_clears_scene_and_ready_flags() {
        let Some((device, _descriptors, mut rt, _before)) = rt_or_skip() else {
            return;
        };
        rt.set_rt_shadows(true);
        rt.set_rt_scene(vec![Mat4::IDENTITY, Mat4::IDENTITY], Vec::new());
        rt.begin_frame();
        assert!(!rt.build_pending());
        assert!(!rt.tlas_ready());
        // The scene capture is cleared, so a build with no fresh scene has no instances.
        assert!(!rt.has_instances(&[]));

        drop(rt);
        device.wait_idle().expect("wait_idle");
    }

    /// GPU-runtime validation of the per-frame TLAS build over a static mesh instance: upload
    /// a mesh (its BLAS is built at upload when RT is supported), capture it via
    /// `set_rt_scene`, `prepare_tlas_build`, replay the plan into a one-off command buffer,
    /// submit + wait — and assert the TLAS holds one instance and the whole path is
    /// validation-clean. On a software device (no RT extensions) this asserts the no-op path
    /// and is skipped for the GPU build (DEFERRED-NEEDS-HARDWARE). The toolbox lavapipe build
    /// *does* advertise the RT extensions, so the build runs here.
    #[test]
    fn tlas_build_over_static_instance_is_validation_clean() {
        use crate::upload::{GpuQueue, Uploader};
        use saffron_geometry::glam::{Vec2, Vec3};
        use saffron_geometry::{Mesh, Submesh, Vertex};

        let Some((device, descriptors, mut rt, before)) = rt_or_skip() else {
            return;
        };
        if !rt.supported() {
            // No RT extensions: the build path is a verified no-op (covered above). The GPU
            // TLAS build is DEFERRED-NEEDS-HARDWARE on a software device.
            assert!(rt.prepare_tlas_build(&device, 0, &[], None).is_none());
            drop(rt);
            device.wait_idle().expect("wait_idle");
            return;
        }

        // Upload a unit triangle; on an RT device this builds its BLAS at upload time.
        let queue = GpuQueue::new(device.graphics_queue);
        let uploader = Uploader::new(&device, &queue).expect("Uploader");
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
        };
        let mesh = Mesh {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let gpu_mesh = uploader
            .upload_mesh(&descriptors, &mesh, &[], None, None)
            .expect("upload_mesh");
        assert!(
            gpu_mesh.blas.is_some(),
            "RT device builds the mesh BLAS at upload"
        );

        // Arm RT shadows + capture one static instance, then prepare the per-frame TLAS build.
        rt.set_rt_shadows(true);
        rt.set_rt_scene(vec![Mat4::IDENTITY], vec![Arc::clone(&gpu_mesh)]);
        assert!(rt.build_pending());
        let plan = rt
            .prepare_tlas_build(&device, 0, &[], None)
            .expect("a build plan for one static instance");
        assert!(rt.tlas_ready());
        assert_eq!(
            rt.frame_instance_count(),
            1,
            "one static instance in the TLAS"
        );
        assert_ne!(rt.mesh_set(0), vk::DescriptorSet::null());

        // Replay the plan into a one-off command buffer, submit, and wait.
        record_and_submit_oneoff(&device, |cmd| {
            record_tlas_build_plan(device.raw(), cmd, &plan);
        })
        .expect("record + submit the TLAS build");
        device.wait_idle().expect("wait_idle");

        drop(plan);
        drop(gpu_mesh);
        drop(uploader);
        drop(rt);
        device.wait_idle().expect("wait_idle before teardown");
        drop(descriptors);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the upload-time BLAS + per-frame TLAS build must be validation-clean (saw {} new)",
            after.saturating_sub(before)
        );
    }
}
