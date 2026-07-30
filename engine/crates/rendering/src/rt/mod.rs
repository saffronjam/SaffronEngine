//! Hardware ray tracing: per-mesh BLAS, a per-frame TLAS over the scene's mesh instances,
//! per-skinned-instance refit BLAS, and the set-6 TLAS descriptor the mesh fragment binds for
//! inline ray-query shadows. Feature-gated on [`Device::rt_supported`]; inert on a software device.
//!
//! The TLAS is ping-ponged per in-flight frame and the skinned refit BLAS is per-slot: an in-place
//! `MODE_UPDATE` rewrites the structure while frame N's GPU work may still trace the same slot, so
//! the per-slot fence wait in the frame loop is what keeps a refit off a live read. Skinned
//! deformed vertices are already in world space, so a skinned instance's TLAS transform is
//! identity.

mod build;
mod plan;

use std::collections::HashMap;
use std::sync::Arc;

use ash::khr::acceleration_structure as accel;
use ash::vk;
use saffron_geometry::Vertex;
use saffron_geometry::glam::Mat4;

use crate::draw_list::DeformedRtInstance;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{AccelerationStructure, Buffer, DeviceResources, GpuMesh, Micromap};
use crate::{Device, Result, checked};

pub use build::*;

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

/// One tessellated instance's per-slot BLAS: variable topology every frame forbids `MODE_UPDATE`, so
/// this is always a full `MODE_BUILD` over the amplified transient VB/IB. The AS backing store is sized
/// to the worst-case primitive count and recreated **only** when that bound changes (never on per-frame
/// count wobble), so `worst_case_prims` is the reuse key.
struct TessellatedBlas {
    accel: Arc<AccelerationStructure>,
    worst_case_prims: u32,
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
    /// The per-placement identity table the candidate resolver reads, parallel to the
    /// placements this frame packs. Grown with the placement bound before the frame's
    /// address block is published, so the block always names a live allocation.
    ray_instances: Option<Buffer>,
    ray_instance_capacity: u32,
    scratch: Option<Buffer>,
    scratch_capacity: u32,
    mesh_set: vk::DescriptorSet,
    skinned_blas: HashMap<u64, SkinnedBlas>,
    /// Per-entity tessellated BLAS, rebuilt in full each frame.
    tessellated_blas: HashMap<u64, TessellatedBlas>,
    blas_scratch: Option<Buffer>,
    blas_scratch_capacity: u32,
}

/// The captured per-frame static RT scene: parallel model transforms + meshes, set by
/// `set_rt_scene` and consumed by the `tlas-build` pass. Skinned instances ride the
/// [`crate::FrameDeformation`] (their deformed offsets are authoritative there), not here.
#[derive(Default)]
pub struct RtScene {
    /// This frame's static TLAS instance inputs.
    ///
    /// Shared rather than owned: the scene mirror caches the cut and hands the same allocation to
    /// every frame it stays valid for, so republishing an unchanged scene costs one refcount.
    pub instances: Arc<[RtInstanceInput]>,
}

/// The GPU-scene instance slot value of a TLAS instance with no mirrored identity; ray
/// candidates on such an instance commit without record resolution.
pub const RT_UNMIRRORED_INSTANCE: u32 = u32::MAX;

/// One TLAS instance's identity, indexed by the `instanceCustomIndex` the packer writes.
///
/// A traced candidate carries a bottom-level structure and a geometry index; neither names a scene
/// record, so this table is what turns the pair into one. `first_submesh` is the geometry's submesh
/// element for geometry index 0 — zero for a plain mesh, the span start for an assembly prototype,
/// whose structure holds one geometry per submesh of ITS span rather than of the family's table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C, align(4))]
pub struct GpuRayInstanceRecord {
    /// The GPU-scene instance slot, or [`RT_UNMIRRORED_INSTANCE`].
    pub instance_slot: u32,
    /// The referenced structure's geometry-0 submesh element within the prototype geometry.
    pub first_submesh: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 2],
}

const _: () = assert!(size_of::<GpuRayInstanceRecord>() == 16);

/// One static TLAS instance: its world transform, the mesh supplying the BLAS, the stable
/// GPU-scene instance slot, and its opacity class.
#[derive(Clone)]
pub struct RtInstanceInput {
    /// Column-major world transform (transposed to a row-major 3×4 at packing).
    pub model: Mat4,
    /// The mesh whose BLAS the instance references.
    pub mesh: Arc<GpuMesh>,
    /// The GPU-scene instance slot, or [`RT_UNMIRRORED_INSTANCE`].
    pub instance_slot: u32,
    /// Opacity this instance forces, overriding what its BLAS geometry was built with.
    ///
    /// `None` is the normal case and the one that lets a micromap work: the per-submesh flags
    /// baked into the structure govern, and traversal consults any attached micromap. `Some` means
    /// the entity resolved a material whose class DISAGREES with the cooked one — one mesh
    /// instanced with materials chosen at runtime — so the instance forces its own answer and
    /// disables the micromap, which was derived against a different material and would otherwise
    /// refine coverage this instance does not have.
    pub opacity_override: Option<bool>,
    /// For an assembly mesh, the mask-table combination this instance renders — the same
    /// index the raster path resolves. Selects which placed uses are active, and therefore
    /// which TLAS instances this input expands into. Ignored by a plain mesh.
    pub combination: u32,
}

/// The camera cut parameters TLAS packing projects representation selection through — the
/// same eye, projection scale, threshold, and override the raster traversal's refine test
/// uses, so ray and raster swap representations on the same boundary.
#[derive(Clone, Copy, Debug)]
pub struct RtCutView {
    /// The camera eye position, world space.
    pub eye: [f32; 3],
    /// The traversal's projection scale (view height in pixels over the tangent span).
    pub proj_scale: f32,
    /// The refine threshold in projected pixels.
    pub error_threshold_px: f32,
    /// [`crate::SCENE_CUT_AUTO`], [`crate::SCENE_CUT_FORCE_COARSE`], or
    /// [`crate::SCENE_CUT_FORCE_FINE`], so a pinned cut pins both representations.
    pub representation_override: u32,
}

/// Whether `input`'s aggregate structure stands in for its fine expansion this frame:
/// `scene_traversal.slang`'s refine test, evaluated on the root cut. The projected error is
/// `(appearanceTotal / 65536) * instanceScale * projScale / distance`; at or under the
/// threshold the traversal draws the root's voxel bricks, so the ray representation places
/// the matching aggregate structure.
fn aggregate_stands_in(
    input: &RtInstanceInput,
    view: &RtCutView,
) -> Option<Arc<AccelerationStructure>> {
    let (blas, error_total) = input.mesh.aggregate_blas.as_ref()?;
    let want_refine = match view.representation_override {
        crate::SCENE_CUT_FORCE_COARSE => false,
        crate::SCENE_CUT_FORCE_FINE => true,
        _ => {
            let scale = input
                .model
                .x_axis
                .truncate()
                .length()
                .max(input.model.y_axis.truncate().length())
                .max(input.model.z_axis.truncate().length());
            let eye = saffron_geometry::glam::Vec3::from(view.eye);
            let distance = (input.model.w_axis.truncate() - eye).length().max(0.05);
            let projected = (*error_total as f32 / 65_536.0) * scale * view.proj_scale / distance;
            projected > view.error_threshold_px
        }
    };
    (!want_refine).then(|| Arc::clone(blas))
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
    /// Distinct bottom-level structures this frame's TLAS references (rt-stats). Counted
    /// from the packed instances, so instancing shows up as instances > BLAS.
    blas_count: u32,
    /// Skinned refit BLAS active this frame (rt-stats).
    skinned_blas_count: u32,
    /// Tessellated full-rebuild BLAS active this frame (rt-stats).
    tessellated_blas_count: u32,
    /// AS-storage bytes the distinct bottom-level structures occupy this frame (rt-stats).
    blas_bytes: u64,
    /// What those same structures would occupy had none been compacted, so
    /// `blas_built_bytes - blas_bytes` is the saving compaction realized (rt-stats).
    blas_built_bytes: u64,
    /// AS-storage bytes this frame's top-level structure occupies (rt-stats).
    tlas_bytes: u64,
    /// The partitioned top-level structure, on a device that has the extension. `None`
    /// everywhere else, where the KHR per-frame TLAS is the top level.
    ptlas: Option<crate::rt_ptlas::Ptlas>,
    /// Build-scratch bytes held for this frame's TLAS + BLAS builds (rt-stats). Scratch is
    /// grow-only and shared, so it is reported apart from the structures themselves.
    scratch_bytes: u64,
    /// TLAS instances placed through the aggregate-representation structure this frame — a
    /// family that packed one coarse instance instead of its per-use expansion (rt-stats).
    aggregate_instance_count: u32,
    /// Distinct cluster-composed bottom-level structures referenced this frame (rt-stats).
    /// Zero everywhere `VK_NV_cluster_acceleration_structure` is absent.
    cluster_blas_count: u32,
    /// Cluster acceleration structures those bottom levels compose (rt-stats).
    clas_count: u32,
    /// Distinct opacity micromaps the frame's structures reference (rt-stats).
    omm_micromaps: u32,
    /// Micro-triangles those micromaps settled opaque, settled transparent, and left unknown.
    ///
    /// The unknown share is the one that matters operationally: it is the work the classifier
    /// still does, so a derivation that resolved nothing reads as micromaps present and unknown
    /// equal to the total rather than as a healthy zero.
    omm_classes: (u64, u64, u64),
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
            tessellated_blas_count: 0,
            blas_bytes: 0,
            blas_built_bytes: 0,
            aggregate_instance_count: 0,
            cluster_blas_count: 0,
            clas_count: 0,
            omm_micromaps: 0,
            omm_classes: (0, 0, 0),
            tlas_bytes: 0,
            scratch_bytes: 0,
            ptlas: None,
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
        // A partitioned structure replaces the per-frame TLAS entirely on a device that has
        // one — the two never coexist, because set 6's descriptor type is resolved for the
        // device at layout creation and admits exactly one of them.
        rt.ptlas = crate::rt_ptlas::Ptlas::new(device, &Arc::clone(&rt.resources));
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

    /// Whether the runtime ray-query-reflections toggle is on (independent of `tlas_ready`).
    pub fn use_rt_reflections(&self) -> bool {
        self.use_rt_reflections
    }

    /// Sets the ray-query-reflections toggle (clamped off on a non-RT device).
    pub fn set_rt_reflections(&mut self, enabled: bool) {
        self.use_rt_reflections = enabled && self.supported;
    }

    /// Distinct bottom-level structures referenced by this frame's TLAS (rt-stats).
    pub fn blas_count(&self) -> u32 {
        self.blas_count
    }

    /// The skinned refit BLAS active this frame (rt-stats).
    pub fn skinned_blas_count(&self) -> u32 {
        self.skinned_blas_count
    }

    /// The tessellated full-rebuild BLAS active this frame (rt-stats).
    pub fn tessellated_blas_count(&self) -> u32 {
        self.tessellated_blas_count
    }

    /// The TLAS instance count produced by this frame's build (static + skinned).
    pub fn frame_instance_count(&self) -> u32 {
        self.frame_instance_count
    }

    /// TLAS instances placed through the aggregate-representation structure this frame.
    pub fn aggregate_instance_count(&self) -> u32 {
        self.aggregate_instance_count
    }

    /// Distinct cluster-composed bottom-level structures referenced this frame.
    pub fn cluster_blas_count(&self) -> u32 {
        self.cluster_blas_count
    }

    /// Cluster acceleration structures those bottom levels compose.
    pub fn clas_count(&self) -> u32 {
        self.clas_count
    }

    /// The partitioned structure's last build, or `None` on a device without one.
    pub fn ptlas_stats(&self) -> Option<crate::rt_ptlas::PtlasStats> {
        self.ptlas.as_ref().map(crate::rt_ptlas::Ptlas::stats)
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

    /// Captures this frame's static TLAS instance inputs for the `tlas-build` pass,
    /// arming the build when RT shadows are on.
    pub fn set_rt_scene(&mut self, instances: Arc<[RtInstanceInput]>) {
        self.scene.instances = instances;
        self.build_pending = self.supported && (self.use_rt_shadows || self.use_rt_reflections);
    }

    /// Whether this frame has any RT instances (static or deforming) to build a TLAS over.
    pub fn has_instances(&self, deformed: &[DeformedRtInstance]) -> bool {
        !self.scene.instances.is_empty() || !deformed.is_empty()
    }

    /// Grows `frame`'s ray-instance identity table to cover every placement this frame could pack,
    /// and returns its `(address, addressable entries)` for the frame's GPU-scene address block.
    ///
    /// Sized here rather than at packing time because the block is published before the top-level
    /// plan runs: a table that regrew mid-frame would leave the block naming a freed allocation.
    /// The table and the structure that indexes it are written together per frame slot, so a frame
    /// that skips the build leaves a matching pair rather than a mixed one.
    pub fn ensure_frame_ray_instances(
        &mut self,
        device: &Device,
        frame: usize,
        deformed_count: usize,
    ) -> (u64, u32) {
        if !self.supported {
            return (0, 0);
        }
        let wanted = self.placement_upper_bound(deformed_count);
        if let Err(err) = self.ensure_ray_instance_capacity(frame, wanted) {
            tracing::error!("rt: ray instance table grow failed: {err}");
            return (0, 0);
        }
        self.ray_instance_addresses(device, frame)
    }

    /// `frame`'s ray-instance table `(address, addressable entries)`, or `(0, 0)` before the table
    /// exists.
    pub fn ray_instance_addresses(&self, device: &Device, frame: usize) -> (u64, u32) {
        self.frames[frame]
            .ray_instances
            .as_ref()
            .map_or((0, 0), |buffer| {
                (
                    device.buffer_device_address(buffer.handle()),
                    self.frames[frame].ray_instance_capacity,
                )
            })
    }

    /// Clears the per-frame static-scene capture + the ready/pending flags at the top of a
    /// frame, before the host repopulates via [`Rt::set_rt_scene`]. The static meshes pin
    /// `Arc<GpuMesh>` across the frame; clearing here releases them for the next frame. The
    /// per-slot skinned-BLAS maps are intentionally *not* cleared — they are grow-only across
    /// frames (an entity keeps its AS and refits in place).
    pub fn begin_frame(&mut self) {
        self.scene.instances = Arc::from([]);
        self.tlas_ready = false;
        self.build_pending = false;
    }

    /// Resets only the per-frame TLAS-ready flag (not the host-set scene), so a frame that
    /// skips the build (RT shadows off, or no instances) does not report a stale `tlas_ready`
    /// from an earlier frame. Called by the renderer at the top of the frame-graph build.
    pub fn reset_frame_ready(&mut self) {
        self.tlas_ready = false;
        self.frame_instance_count = 0;
        self.blas_count = 0;
        self.skinned_blas_count = 0;
        self.aggregate_instance_count = 0;
        self.cluster_blas_count = 0;
        self.clas_count = 0;
        self.blas_bytes = 0;
        self.omm_micromaps = 0;
        self.omm_classes = (0, 0, 0);
        self.blas_built_bytes = 0;
        self.tlas_bytes = 0;
        self.scratch_bytes = 0;
    }

    /// AS-storage bytes the distinct bottom-level structures occupy this frame.
    pub fn blas_bytes(&self) -> u64 {
        self.blas_bytes
    }

    /// What those structures would occupy had none been compacted.
    pub fn blas_built_bytes(&self) -> u64 {
        self.blas_built_bytes
    }

    /// AS-storage bytes this frame's top-level structure occupies.
    pub fn tlas_bytes(&self) -> u64 {
        self.tlas_bytes
    }

    /// Distinct opacity micromaps the frame's structures reference.
    pub fn omm_micromaps(&self) -> u32 {
        self.omm_micromaps
    }

    /// Micro-triangles settled opaque, settled transparent, and left unknown.
    pub fn omm_classes(&self) -> (u64, u64, u64) {
        self.omm_classes
    }

    /// Build-scratch bytes held for this frame's TLAS + BLAS builds.
    pub fn scratch_bytes(&self) -> u64 {
        self.scratch_bytes
    }

    /// Drops every per-slot skinned refit BLAS (e.g. on a scene reset) so a stale entity's
    /// AS does not linger. The maps regrow on demand.
    pub fn clear_skinned_blas(&mut self) {
        for frame in &mut self.frames {
            frame.skinned_blas.clear();
            frame.tessellated_blas.clear();
        }
    }
}

impl FrameRt {
    /// An empty slot before any RT use: no TLAS, no buffers, a null descriptor set.
    fn empty() -> Self {
        Self {
            tlas: None,
            tlas_capacity: 0,
            instance_buffer: None,
            instance_capacity: 0,
            ray_instances: None,
            ray_instance_capacity: 0,
            scratch: None,
            scratch_capacity: 0,
            mesh_set: vk::DescriptorSet::null(),
            skinned_blas: HashMap::new(),
            tessellated_blas: HashMap::new(),
            blas_scratch: None,
            blas_scratch_capacity: 0,
        }
    }
}

#[cfg(test)]
mod tests;
