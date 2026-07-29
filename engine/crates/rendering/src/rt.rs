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
use crate::resources::{AccelerationStructure, Buffer, DeviceResources, GpuMesh, Micromap};
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
    scratch: Option<Buffer>,
    scratch_capacity: u32,
    mesh_set: vk::DescriptorSet,
    skinned_blas: HashMap<u64, SkinnedBlas>,
    /// Per-entity tessellated BLAS (full rebuild each frame; keyed by entity in this v1, by
    /// `(mesh, lod_bucket)` once Phase-7 bucketing collapses same-bucket instances onto one).
    tessellated_blas: HashMap<u64, TessellatedBlas>,
    blas_scratch: Option<Buffer>,
    blas_scratch_capacity: u32,
}

/// The captured per-frame static RT scene: parallel model transforms + meshes, set by
/// `set_rt_scene` and consumed by the `tlas-build` pass. Skinned instances ride the
/// [`crate::SceneDrawList`] (their deformed offsets are authoritative there), not here.
#[derive(Default)]
pub struct RtScene {
    /// This frame's static TLAS instance inputs.
    pub instances: Vec<RtInstanceInput>,
}

/// The GPU-scene instance slot value of a TLAS instance with no mirrored identity; ray
/// candidates on such an instance commit without record resolution.
pub const RT_UNMIRRORED_INSTANCE: u32 = 0x00FF_FFFF;

/// One static TLAS instance: its world transform, the mesh supplying the BLAS, the stable
/// GPU-scene instance slot packed as `instanceCustomIndex`, and its opacity class.
#[derive(Clone)]
pub struct RtInstanceInput {
    /// Column-major world transform (transposed to a row-major 3×4 at packing).
    pub model: Mat4,
    /// The mesh whose BLAS the instance references.
    pub mesh: Arc<GpuMesh>,
    /// The GPU-scene instance slot, or [`RT_UNMIRRORED_INSTANCE`].
    pub custom_index: u32,
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
    pub fn set_rt_scene(&mut self, instances: Vec<RtInstanceInput>) {
        self.scene.instances = instances;
        self.build_pending = self.supported && (self.use_rt_shadows || self.use_rt_reflections);
    }

    /// Whether this frame has any RT instances (static or deforming) to build a TLAS over.
    pub fn has_instances(&self, deformed: &[DeformedRtInstance]) -> bool {
        !self.scene.instances.is_empty() || !deformed.is_empty()
    }

    /// Clears the per-frame static-scene capture + the ready/pending flags at the top of a
    /// frame, before the host repopulates via [`Rt::set_rt_scene`]. The static meshes pin
    /// `Arc<GpuMesh>` across the frame; clearing here releases them for the next frame. The
    /// per-slot skinned-BLAS maps are intentionally *not* cleared — they are grow-only across
    /// frames (an entity keeps its AS and refits in place).
    pub fn begin_frame(&mut self) {
        self.scene.instances.clear();
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
        cut_view: RtCutView,
    ) -> Option<TlasBuildPlan> {
        self.tlas_ready = false;
        self.skinned_blas_count = 0;
        self.tessellated_blas_count = 0;
        if !self.supported || (self.scene.instances.is_empty() && deformed.is_empty()) {
            return None;
        }
        let dispatch = self.dispatch.clone()?;

        // Plan each deforming BLAS: skinned/morph refit (create-once then in-place `UPDATE`) +
        // tessellated full `BUILD` (variable topology), both sizing the shared build scratch.
        let mut blas_ops =
            self.plan_skinned_blas_refits(device, &dispatch, frame, deformed, deformed_buffer);
        let skinned_op_count = blas_ops.len() as u32;
        let tess_ops = self.plan_tessellated_blas_builds(device, &dispatch, frame, deformed);
        let tess_op_count = tess_ops.len() as u32;
        blas_ops.extend(tess_ops);

        // One placement per static mesh that has a BLAS, then one per deforming instance.
        // Placements are the single description both top-level forms derive from: the KHR
        // path packs them into its instance array, the partitioned path diffs them against
        // the structure's placed state. Deriving each independently would let the two
        // top-levels disagree about the same scene.
        let mut placements: Vec<Placement> =
            Vec::with_capacity(self.scene.instances.len() + deformed.len());
        let mut aggregate_count = 0_u32;
        for (scene_index, input) in self.scene.instances.iter().enumerate() {
            let primary = placement_primary(input.custom_index, scene_index);
            // Representation selection, by the raster traversal's own refine test: project
            // the root cut's appearance error through the instance scale and eye distance,
            // and when it fits under the threshold place the single family-space aggregate
            // structure instead of the fine expansion — the same swap the traversal makes
            // when it draws the root's voxel bricks.
            if let Some(blas) = aggregate_stands_in(input, &cut_view) {
                placements.push(Placement {
                    key: crate::rt_ptlas::PtlasKey { primary, sub: 0 },
                    rows: transform_rows(&input.model),
                    custom_index: input.custom_index.min(RT_UNMIRRORED_INSTANCE),
                    // The aggregate is built opaque; an opacity override targets the fine
                    // geometry's coverage classes, which this representation merged away.
                    opacity: vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE,
                    blas: crate::RtBlas::Khr(blas),
                });
                aggregate_count += 1;
                continue;
            }
            // An assembly is not one structure: KHR acceleration structures cannot nest
            // micro-instance parts, so a family expands into one TLAS instance per active
            // placed use, each referencing its prototype's structure.
            if let Some(assembly) = input.mesh.assembly.as_ref() {
                if input.mesh.assembly_blas.len() != assembly.prototypes.len() {
                    continue;
                }
                let words = assembly.mask_words();
                let base = input.combination as usize * words;
                for (use_index, use_record) in assembly.uses.iter().enumerate() {
                    let word = assembly.masks.get(base + use_index / 32).copied();
                    if word.is_none_or(|bits| bits & (1 << (use_index % 32)) == 0) {
                        continue;
                    }
                    let Some(blas) = input.mesh.assembly_blas.get(use_record.prototype as usize)
                    else {
                        continue;
                    };
                    placements.push(Placement {
                        // The use index distinguishes a family's placements under one scene
                        // slot; +1 keeps it clear of the single-placement zero.
                        key: crate::rt_ptlas::PtlasKey {
                            primary,
                            sub: use_index as u32 + 1,
                        },
                        rows: transform_rows(&(input.model * assembly_use_matrix(use_record))),
                        custom_index: input.custom_index.min(RT_UNMIRRORED_INSTANCE),
                        opacity: instance_opacity_flags(input.opacity_override),
                        blas: blas.clone(),
                    });
                }
                continue;
            }
            let Some(blas) = input.mesh.blas.as_ref() else {
                continue;
            };
            placements.push(Placement {
                key: crate::rt_ptlas::PtlasKey { primary, sub: 0 },
                rows: transform_rows(&input.model),
                custom_index: input.custom_index.min(RT_UNMIRRORED_INSTANCE),
                opacity: instance_opacity_flags(input.opacity_override),
                blas: crate::RtBlas::Khr(Arc::clone(blas)),
            });
        }
        // A deforming instance references its BLAS at its `world_transform`: identity for a skinned
        // (or skin+morph) instance — the deformed vertices are already in world space — and the node
        // world matrix for an unskinned-morph / tessellated instance, whose vertices are mesh-local.
        // A tessellated instance resolves to its full-rebuild `tessellated_blas`; every other to its
        // refit `skinned_blas`.
        for inst in deformed {
            let accel = if inst.tess.is_some() {
                self.frames[frame]
                    .tessellated_blas
                    .get(&inst.entity)
                    .map(|slot| Arc::clone(&slot.accel))
            } else {
                self.frames[frame]
                    .skinned_blas
                    .get(&inst.entity)
                    .map(|slot| Arc::clone(&slot.accel))
            };
            let Some(accel) = accel else {
                continue;
            };
            placements.push(Placement {
                key: crate::rt_ptlas::PtlasKey {
                    primary: DEFORMED_PRIMARY_BASE | inst.entity,
                    sub: 0,
                },
                rows: transform_rows(&inst.world_transform),
                custom_index: RT_UNMIRRORED_INSTANCE,
                opacity: vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE,
                blas: crate::RtBlas::Khr(accel),
            });
        }

        let count = placements.len() as u32;
        if count == 0 {
            return None;
        }
        let retained: Vec<crate::RtBlas> = placements
            .iter()
            .map(|placement| placement.blas.clone())
            .collect();
        // The partitioned structure is the whole top level where it exists: it consumes the
        // placements directly and retains what it places, so none of the KHR instance array,
        // its buffer, or its per-frame TLAS is reached on such a device.
        if self.ptlas.is_some() {
            return self.plan_ptlas_build(
                device,
                frame,
                &placements,
                &retained,
                PlanContext {
                    dispatch,
                    blas_ops,
                    count,
                    aggregate_count,
                    skinned_op_count,
                    tess_op_count,
                },
            );
        }
        let instances: Vec<vk::AccelerationStructureInstanceKHR> = placements
            .iter()
            .map(|placement| {
                make_instance(
                    placement.rows,
                    placement.custom_index,
                    placement.opacity,
                    placement.blas.address(),
                )
            })
            .collect();
        let mut retained = retained;
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
        self.blas_count = distinct_blas_count(&retained);
        self.skinned_blas_count = skinned_op_count;
        self.tessellated_blas_count = tess_op_count;
        self.aggregate_instance_count = aggregate_count;
        // Sum the bottom-level storage before the TLAS joins `retained`, so the two tiers stay
        // separable in the telemetry. Structures are deduplicated by device address for the same
        // reason `distinct_blas_count` is: instances of one mesh share a structure, and counting
        // its bytes once per instance would report sharing as growth.
        let (blas_bytes, blas_built_bytes) = distinct_blas_bytes(&retained);
        self.blas_bytes = blas_bytes;
        self.blas_built_bytes = blas_built_bytes;
        let (cluster_blas_count, clas_count) = distinct_cluster_blas(&retained);
        self.cluster_blas_count = cluster_blas_count;
        self.clas_count = clas_count;
        let (omm_micromaps, omm_classes) = distinct_micromap_classes(&self.scene.instances);
        self.omm_micromaps = omm_micromaps;
        self.omm_classes = omm_classes;
        self.tlas_bytes = self.frames[frame]
            .tlas
            .as_ref()
            .map_or(0, |tlas| tlas.size());
        self.scratch_bytes = self.frames[frame]
            .scratch
            .as_ref()
            .map_or(0, |scratch| scratch.size())
            + self.frames[frame]
                .blas_scratch
                .as_ref()
                .map_or(0, |scratch| scratch.size());
        self.tlas_ready = true;
        // Retain the TLAS too (it is referenced only through `self` otherwise, but holding
        // it in the plan keeps the replay self-contained).
        retained.push(crate::RtBlas::Khr(Arc::clone(
            self.frames[frame].tlas.as_ref().expect("TLAS present"),
        )));
        Some(TlasBuildPlan {
            dispatch,
            blas_ops,
            blas_scratch_addr,
            top: TopLevelBuild::Khr(tlas_op),
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
            // A tessellated instance takes the full-BUILD path (`plan_tessellated_blas_builds`);
            // variable topology every frame forbids the in-place `UPDATE` this refit path uses.
            if skinned_refit_skips(
                inst.vertex_count,
                inst.index_count,
                inst.entity,
                inst.tess.is_some(),
            ) {
                continue;
            }
            let triangle_count = inst.index_count / 3;
            let vertex_data =
                deformed_base + vk::DeviceAddress::from(inst.deformed_offset) * vertex_stride;
            let index_data = device.buffer_device_address(inst.mesh.index_buffer());

            let inputs = GeometryInputs::new(
                vertex_data,
                vertex_stride,
                inst.vertex_count,
                index_data,
                true,
                None,
            );
            let geom = inputs.geometry();
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

    /// Plans a full `MODE_BUILD` for each tessellated instance's BLAS over its slice of the amplified
    /// transient VB/IB. Unlike the skinned refit there is no create-once/`UPDATE` gate: variable topology
    /// every frame demands a full rebuild, so the AS is sized to the worst-case primitive count and
    /// recreated only when that bound changes (never on the per-frame GPU-packed count). The build range
    /// runs the worst-case count too — the emit kernel degenerate-pads the index tail, so the extra
    /// triangles collapse to points the builder discards, giving a watertight, portable floor with no
    /// GPU-count readback. Shares the frame's build scratch (grown to the max, serialized by the recorder).
    fn plan_tessellated_blas_builds(
        &mut self,
        device: &Device,
        dispatch: &accel::Device,
        frame: usize,
        instances: &[DeformedRtInstance],
    ) -> Vec<BlasRefitOp> {
        let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
        let mut ops: Vec<BlasRefitOp> = Vec::new();
        let mut scratch_needed: vk::DeviceSize = 0;
        for inst in instances {
            let Some(tess) = inst.tess else {
                continue;
            };
            if inst.entity == 0 || tess.worst_case_prims == 0 || tess.worst_case_verts == 0 {
                continue;
            }
            let vertex_data = device.buffer_device_address(tess.vertex_buffer)
                + vk::DeviceAddress::from(tess.vertex_base) * vertex_stride;
            let index_data = device.buffer_device_address(tess.index_buffer)
                + vk::DeviceAddress::from(tess.index_base) * size_of::<u32>() as vk::DeviceSize;

            let inputs = GeometryInputs::new(
                vertex_data,
                vertex_stride,
                tess.worst_case_verts,
                index_data,
                true,
                None,
            );
            let geom = inputs.geometry();
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
                    &[tess.worst_case_prims],
                    &mut sizes,
                );
            }

            // Reuse the AS while its worst-case bound holds; recreate it (never `UPDATE`) otherwise.
            let accel = match self.frames[frame].tessellated_blas.get(&inst.entity) {
                Some(slot)
                    if tess_blas_reuse(Some(slot.worst_case_prims), tess.worst_case_prims) =>
                {
                    Arc::clone(&slot.accel)
                }
                _ => match AccelerationStructure::create(
                    &self.resources,
                    dispatch,
                    sizes.acceleration_structure_size,
                    vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
                ) {
                    Ok(accel) => {
                        let accel = Arc::new(accel);
                        self.frames[frame].tessellated_blas.insert(
                            inst.entity,
                            TessellatedBlas {
                                accel: Arc::clone(&accel),
                                worst_case_prims: tess.worst_case_prims,
                            },
                        );
                        accel
                    }
                    Err(err) => {
                        tracing::error!("rt: tessellated BLAS create failed: {err}");
                        continue;
                    }
                },
            };
            scratch_needed = scratch_needed.max(sizes.build_scratch_size);
            ops.push(BlasRefitOp {
                dst: accel.handle(),
                vertex_data,
                vertex_stride,
                max_vertex: tess.worst_case_verts - 1,
                index_data,
                triangle_count: tess.worst_case_prims,
                update: false,
            });
        }
        if ops.is_empty() {
            return Vec::new();
        }
        if let Err(err) = self.ensure_blas_scratch(frame, scratch_needed) {
            tracing::error!("rt: tessellated BLAS scratch grow failed: {err}");
            return Vec::new();
        }
        ops
    }

    /// Plans the partitioned structure's advance: assigns each placement its stable slot,
    /// diffs against what the structure already holds, and points set 6 at the result.
    ///
    /// The whole top level, so it reports the same counters the KHR path does — with the
    /// partition and op counts beside them, which is where the difference shows.
    fn plan_ptlas_build(
        &mut self,
        device: &Device,
        frame: usize,
        placements: &[Placement],
        retained: &[crate::RtBlas],
        context: PlanContext,
    ) -> Option<TlasBuildPlan> {
        let instances: Vec<crate::rt_ptlas::PtlasInstance> = placements
            .iter()
            .map(|placement| crate::rt_ptlas::PtlasInstance {
                key: placement.key,
                transform: placement.rows,
                custom_index: placement.custom_index,
                mask: 0xFF,
                flags: crate::rt_ptlas::instance_flags(
                    vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE | placement.opacity,
                ),
                blas: placement.blas.clone(),
                // A deforming instance has no fixed cell — its structure is refit every
                // frame anyway — so it goes global rather than churning partitions.
                partition: if placement.key.primary & DEFORMED_PRIMARY_BASE != 0 {
                    crate::rt_ptlas::GLOBAL_PARTITION
                } else {
                    crate::rt_ptlas::partition_for_translation([
                        placement.rows[3],
                        placement.rows[7],
                        placement.rows[11],
                    ])
                },
            })
            .collect();
        let resources = Arc::clone(&self.resources);
        let op = self.ptlas.as_mut()?.plan(&resources, frame, &instances)?;
        let ptlas = self.ptlas.as_ref()?;
        let address = ptlas.current_address()?;
        let structure_bytes = ptlas.structure_bytes();
        let ptlas_scratch_bytes = ptlas.scratch_bytes();
        self.write_mesh_set_ptlas(device, frame, address);

        let blas_scratch_addr = self.frames[frame]
            .blas_scratch
            .as_ref()
            .map(|b| device.buffer_device_address(b.handle()))
            .unwrap_or(0);
        self.frame_instance_count = context.count;
        self.skinned_blas_count = context.skinned_op_count;
        self.tessellated_blas_count = context.tess_op_count;
        self.aggregate_instance_count = context.aggregate_count;
        self.blas_count = distinct_blas_count(retained);
        let (blas_bytes, blas_built_bytes) = distinct_blas_bytes(retained);
        self.blas_bytes = blas_bytes;
        self.blas_built_bytes = blas_built_bytes;
        let (cluster_blas_count, clas_count) = distinct_cluster_blas(retained);
        self.cluster_blas_count = cluster_blas_count;
        self.clas_count = clas_count;
        let (omm_micromaps, omm_classes) = distinct_micromap_classes(&self.scene.instances);
        self.omm_micromaps = omm_micromaps;
        self.omm_classes = omm_classes;
        self.tlas_bytes = structure_bytes;
        self.scratch_bytes = ptlas_scratch_bytes
            + self.frames[frame]
                .blas_scratch
                .as_ref()
                .map_or(0, |scratch| scratch.size());
        self.tlas_ready = true;
        Some(TlasBuildPlan {
            dispatch: context.dispatch,
            blas_ops: context.blas_ops,
            blas_scratch_addr,
            top: TopLevelBuild::Partitioned(op),
            // The structure retains what it places for as long as it places it, so this
            // holds only what the recording itself touches.
            _retained: retained.to_vec(),
        })
    }

    /// Writes the partitioned structure's device address into `frame`'s set 6.
    ///
    /// Unlike the KHR TLAS, which is rewritten only when its capacity changes, the
    /// partitioned structure alternates frame slots — each build writes the slot the other
    /// frame is not tracing — so the binding moves every frame.
    fn write_mesh_set_ptlas(&self, device: &Device, frame: usize, address: vk::DeviceAddress) {
        let addresses = [address];
        let mut accel_write =
            crate::vk_nv_ptlas::WriteDescriptorSetPartitionedAccelerationStructureNV {
                acceleration_structure_count: 1,
                p_acceleration_structures: addresses.as_ptr(),
                ..Default::default()
            };
        let mut write = vk::WriteDescriptorSet::default()
            .dst_set(self.frames[frame].mesh_set)
            .dst_binding(0)
            .descriptor_type(
                crate::vk_nv_ptlas::DESCRIPTOR_TYPE_PARTITIONED_ACCELERATION_STRUCTURE_NV,
            );
        write.descriptor_count = 1;
        // The transcribed payload cannot ride ash's typed `push_next`, so it is chained by
        // hand; nothing else is in this write's chain.
        write.p_next = (&raw mut accel_write).cast();
        // SAFETY: the ash seam. The set + layout are this renderer's; written on the render
        // thread after the slot's fence is waited (no concurrent host access), and both the
        // chained payload and the address array outlive the call.
        unsafe { device.raw().update_descriptor_sets(&[write], &[]) };
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
        // A partitioned device's set 6 takes a partitioned structure — the descriptor type
        // admits nothing else — so the seed is one inert-instance build per frame slot.
        if self.ptlas.is_some() {
            let frames = self.ptlas.as_ref().map_or(0, |ptlas| ptlas.frame_count());
            let mut seeds = Vec::with_capacity(frames);
            for frame in 0..frames {
                let Some(op) = self.ptlas.as_mut().and_then(|ptlas| ptlas.plan_seed(frame)) else {
                    return Err(crate::Error::InvalidUploadData(
                        "partitioned structure seed could not be planned".to_owned(),
                    ));
                };
                seeds.push(op);
            }
            record_and_submit_oneoff(device, |cmd| {
                for op in &seeds {
                    // SAFETY: the extension seam. The buffer is recording and each seed's
                    // addresses reference the structure's own live per-frame buffers.
                    unsafe { op.record(cmd) };
                }
            })?;
            for frame in 0..frames {
                let Some(address) = self
                    .ptlas
                    .as_ref()
                    .and_then(|ptlas| ptlas.frame_address(frame))
                else {
                    continue;
                };
                self.write_mesh_set_ptlas(device, frame, address);
            }
            return Ok(());
        }
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
    top: TopLevelBuild,
    _retained: Vec<crate::RtBlas>,
}

/// The frame's top-level build, in whichever form the device's structure takes. Which arm a
/// device uses is fixed at descriptor-layout creation, so a plan never mixes them.
enum TopLevelBuild {
    /// The `VK_KHR_acceleration_structure` TLAS, rebuilt whole.
    Khr(TlasBuildOp),
    /// The partitioned structure, advanced by an op stream naming only what changed.
    Partitioned(crate::rt_ptlas::PtlasBuildOp),
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
pub fn record_tlas_build_plan(
    raw: &ash::Device,
    plan: &TlasBuildPlan,
    scopes: &mut crate::nested_scopes::NestedScopeRecorder<'_>,
) {
    // Two named child scopes rather than one pass timing. The graph already brackets the pass, so
    // the split costs nothing and answers the question the pass total cannot: whether a frame got
    // slower from refitting deformed geometry or from rebuilding the instance table.
    scopes.scope("blas-refit", |cmd| record_blas_refits(raw, cmd, plan));
    scopes.scope("tlas-build", |cmd| record_tlas_build(raw, cmd, plan));
}

/// The per-frame BLAS refits, split out so they carry their own timestamp scope.
///
/// Refits and the TLAS build share one graph pass and so shared one timing until now; they are
/// different work with different scaling — refits grow with deforming instances, the TLAS with
/// total instance count — and one number could not say which moved.
fn record_blas_refits(raw: &ash::Device, cmd: vk::CommandBuffer, plan: &TlasBuildPlan) {
    let dispatch = &plan.dispatch;
    let scratch_barrier = accel_scratch_barrier();
    for (i, op) in plan.blas_ops.iter().enumerate() {
        if i > 0 {
            let dep = vk::DependencyInfo::default()
                .memory_barriers(std::slice::from_ref(&scratch_barrier));
            // SAFETY: the ash seam. A memory barrier on the active command buffer.
            unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
        }
        let inputs = GeometryInputs::new(
            op.vertex_data,
            op.vertex_stride,
            op.max_vertex + 1,
            op.index_data,
            true,
            None,
        );
        let geom = inputs.geometry();
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
}

/// The top-level build over the packed instance buffer, plus the barrier handing it to ray queries.
fn record_tlas_build(raw: &ash::Device, cmd: vk::CommandBuffer, plan: &TlasBuildPlan) {
    let tlas = match &plan.top {
        TopLevelBuild::Khr(tlas) => tlas,
        TopLevelBuild::Partitioned(ptlas) => {
            // SAFETY: the extension seam. The command buffer is recording and every address
            // in the plan references a live per-frame buffer of the structure that produced
            // it (the frame's fence was waited before the slot was reused).
            unsafe { ptlas.record(cmd) };
            let barrier = accel_build_to_fragment_barrier();
            let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
            // SAFETY: the ash seam. A memory barrier on the active command buffer.
            unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
            return;
        }
    };
    let dispatch = &plan.dispatch;
    let geom = instances_geometry(tlas.instance_address);
    let geoms = [geom];
    let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(tlas.dst)
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: tlas.scratch_address,
        });
    let range = vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(tlas.count);
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

/// One placed instance, in the form both top-level structures derive from.
struct Placement {
    key: crate::rt_ptlas::PtlasKey,
    rows: [f32; 12],
    custom_index: u32,
    opacity: vk::GeometryInstanceFlagsKHR,
    blas: crate::RtBlas,
}

/// Marks a deforming instance's key, whose primary is an entity rather than a scene slot.
const DEFORMED_PRIMARY_BASE: u64 = 1 << 62;

/// Marks a static instance the GPU scene does not mirror, whose primary is its position in
/// the gather rather than a stable slot.
const UNMIRRORED_PRIMARY_BASE: u64 = 1 << 61;

/// The stable half of a static placement's key.
///
/// A mirrored instance keys on its GPU-scene slot, which is stable for as long as the
/// instance exists — the property the partitioned diff rests on. An unmirrored one has no
/// such identity and falls back to its position in the gather; if that order shifts, its
/// placements are rewritten rather than left alone, which costs incrementality and not
/// correctness.
fn placement_primary(custom_index: u32, scene_index: usize) -> u64 {
    if custom_index < RT_UNMIRRORED_INSTANCE {
        u64::from(custom_index)
    } else {
        UNMIRRORED_PRIMARY_BASE | scene_index as u64
    }
}

/// Everything the top-level plan needs beyond the placements themselves: the dispatch it
/// records through, the bottom-level work it carries, and the per-frame counts it reports.
struct PlanContext {
    dispatch: accel::Device,
    blas_ops: Vec<BlasRefitOp>,
    count: u32,
    aggregate_count: u32,
    skinned_op_count: u32,
    tess_op_count: u32,
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
            tessellated_blas: HashMap::new(),
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
    /// The build scratch — cleared by the caller once the build submit completes.
    pub scratch: Option<Buffer>,
    /// The size the build reserved, before compaction (BLAS telemetry).
    pub built_size: vk::DeviceSize,
}

/// The buffers and index range one bottom-level structure covers. A plain mesh passes its
/// whole stream; an assembly prototype passes the slice it owns.
#[derive(Clone, Copy)]
pub struct MeshBlasGeometry<'a> {
    /// The micromap refining this geometry's coverage, when one was derived.
    pub micromap: Option<&'a Micromap>,
    /// Whether every triangle in this slice classifies opaque. A non-opaque geometry surfaces
    /// ray candidates for the coverage classifier, and is the only kind a micromap refines.
    pub opaque: bool,
    /// The device-local vertex buffer.
    pub vertex_buffer: vk::Buffer,
    /// Vertices in the buffer (the build's `max_vertex` bound).
    pub vertex_count: u32,
    /// The device-local index buffer.
    pub index_buffer: vk::Buffer,
    /// First index of this structure's slice.
    pub first_index: u32,
    /// Indices in the slice.
    pub index_count: u32,
}

/// Records a BLAS build over `geometries` into `cmd` and returns the AS + scratch.
///
/// One geometry per material-homogeneous submesh, each with its own opacity flag. That
/// granularity is the point: a BLAS built as a single geometry can only carry one opacity class,
/// so one masked submesh forces the coverage classifier onto every other submesh in the mesh — and
/// an opacity micromap, which refines a single geometry's coverage, has nothing to attach to.
///
/// Built [`mesh_blas_build_flags`]. The caller submits `cmd` and waits, then drops the returned
/// [`MeshBlasBuild::scratch`]. The mesh's vertex + index buffers must carry
/// `SHADER_DEVICE_ADDRESS` + AS-build-input usage.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if the AS or scratch buffer cannot be created, or
/// [`crate::Error::Message`] if `geometries` is empty.
pub fn record_mesh_blas_build(
    resources: &Arc<DeviceResources>,
    dispatch: &accel::Device,
    cmd: vk::CommandBuffer,
    geometries: &[MeshBlasGeometry<'_>],
    omm_supported: bool,
) -> Result<MeshBlasBuild> {
    if geometries.is_empty() {
        return Err(crate::Error::EmptyMesh);
    }
    let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
    // Each micromap chain is boxed so its address is stable for the whole build: `push_next`
    // stores a RAW POINTER into the triangles descriptor, so growing a `Vec` of these in place
    // would leave every already-chained geometry pointing at freed memory.
    let mut omm_chains: Vec<Option<Box<vk::AccelerationStructureTrianglesOpacityMicromapEXT<'_>>>> =
        geometries
            .iter()
            .map(|geometry| {
                geometry.micromap.map(|micromap| {
                    Box::new(
                        vk::AccelerationStructureTrianglesOpacityMicromapEXT::default()
                            .index_type(vk::IndexType::UINT32)
                            .index_buffer(vk::DeviceOrHostAddressConstKHR {
                                device_address: micromap.index_address(),
                            })
                            .index_stride(size_of::<i32>() as vk::DeviceSize)
                            .base_triangle(0)
                            .usage_counts(micromap.usage())
                            .micromap(micromap.handle()),
                    )
                })
            })
            .collect();
    let inputs: Vec<GeometryInputs<'_>> = geometries
        .iter()
        .zip(omm_chains.iter_mut())
        .map(|(geometry, chain)| {
            GeometryInputs::new(
                resources.buffer_device_address(geometry.vertex_buffer),
                vertex_stride,
                geometry.vertex_count,
                resources.buffer_device_address(geometry.index_buffer),
                geometry.opaque,
                chain.as_deref_mut(),
            )
        })
        .collect();
    let geoms: Vec<vk::AccelerationStructureGeometryKHR<'_>> =
        inputs.iter().map(GeometryInputs::geometry).collect();
    let triangle_counts: Vec<u32> = geometries
        .iter()
        .map(|geometry| geometry.index_count / 3)
        .collect();
    let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
        .flags(mesh_blas_build_flags(omm_supported))
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .geometries(&geoms);
    let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
    // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()`, both `geometries.len()`.
    unsafe {
        dispatch.get_acceleration_structure_build_sizes(
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &size_info,
            &triangle_counts,
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
        .flags(mesh_blas_build_flags(omm_supported))
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(blas.handle())
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: scratch_addr,
        });
    // A submesh owns a slice of the shared index stream, so its build starts at its own first
    // index rather than at zero. `primitive_offset` counts BYTES.
    let ranges: Vec<vk::AccelerationStructureBuildRangeInfoKHR> = geometries
        .iter()
        .zip(&triangle_counts)
        .map(|(geometry, &triangle_count)| {
            vk::AccelerationStructureBuildRangeInfoKHR::default()
                .primitive_count(triangle_count)
                .primitive_offset(geometry.first_index * size_of::<u32>() as u32)
        })
        .collect();
    // SAFETY: the ash seam. One build info; the range slice length equals its `geometry_count`.
    // The vertex/index addresses are valid for the device lifetime.
    unsafe {
        dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
    }
    Ok(MeshBlasBuild {
        blas,
        scratch: Some(scratch),
        built_size: sizes.acceleration_structure_size,
    })
}

/// Records a micromap build for `derived` into `cmd` and returns the structure plus its build
/// scratch, which the caller drops once the submit completes.
///
/// The state data, the per-triangle block descriptors, and the per-triangle index stream are
/// uploaded to device-local buffers first: a micromap build reads all three by device address,
/// exactly as an acceleration-structure build reads vertices and indices.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if any buffer or the micromap cannot be created.
pub fn record_micromap_build(
    resources: &Arc<DeviceResources>,
    dispatch: &ash::ext::opacity_micromap::Device,
    cmd: vk::CommandBuffer,
    derived: &saffron_geometry::OpacityMicromapBuild,
) -> Result<(Micromap, Buffer, Buffer, Buffer)> {
    let usage: Vec<vk::MicromapUsageEXT> = derived
        .usage
        .iter()
        .map(|row| {
            vk::MicromapUsageEXT::default()
                .count(row.count)
                .subdivision_level(row.subdivision_level)
                .format(row.format)
        })
        .collect();

    let input_usage = vk::BufferUsageFlags::MICROMAP_BUILD_INPUT_READ_ONLY_EXT
        | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
    let data = Buffer::from_slice_with_usage(resources, &derived.data, input_usage)?;
    // `VkMicromapTriangleEXT` is `{u32 dataOffset, u16 subdivisionLevel, u16 format}` — eight
    // bytes, which is also the minimum stride the build accepts.
    let triangle_bytes: Vec<u8> = derived
        .blocks
        .iter()
        .flat_map(|block| {
            let mut row = [0_u8; 8];
            row[0..4].copy_from_slice(&block.data_offset.to_ne_bytes());
            row[4..6].copy_from_slice(&block.subdivision_level.to_ne_bytes());
            row[6..8].copy_from_slice(&block.format.to_ne_bytes());
            row
        })
        .collect();
    let triangles = Buffer::from_slice_with_usage(resources, &triangle_bytes, input_usage)?;
    let index_bytes: Vec<u8> = derived
        .indices
        .iter()
        .flat_map(|index| index.to_ne_bytes())
        .collect();
    let indices = Buffer::from_slice_with_usage(
        resources,
        &index_bytes,
        vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
    )?;

    let mut sizes = vk::MicromapBuildSizesInfoEXT::default();
    let size_info = vk::MicromapBuildInfoEXT::default()
        .ty(vk::MicromapTypeEXT::OPACITY_MICROMAP)
        .mode(vk::BuildMicromapModeEXT::BUILD)
        .usage_counts(&usage);
    // SAFETY: the ash seam, through the raw table. The usage rows outlive the call.
    unsafe {
        (dispatch.fp().get_micromap_build_sizes_ext)(
            dispatch.device(),
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &size_info,
            &mut sizes,
        );
    }

    let micromap = Micromap::create(
        resources,
        dispatch,
        sizes.micromap_size,
        indices,
        usage.clone(),
        (
            derived.classes.opaque,
            derived.classes.transparent,
            derived.classes.unknown,
        ),
    )?;
    let scratch = make_scratch_buffer(resources, sizes.build_scratch_size.max(1))?;

    let build_info = vk::MicromapBuildInfoEXT::default()
        .ty(vk::MicromapTypeEXT::OPACITY_MICROMAP)
        .mode(vk::BuildMicromapModeEXT::BUILD)
        .dst_micromap(micromap.handle())
        .usage_counts(&usage)
        .data(vk::DeviceOrHostAddressConstKHR {
            device_address: resources.buffer_device_address(data.handle()),
        })
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: resources.buffer_device_address(scratch.handle()),
        })
        .triangle_array(vk::DeviceOrHostAddressConstKHR {
            device_address: resources.buffer_device_address(triangles.handle()),
        })
        .triangle_array_stride(8);
    // SAFETY: the ash seam. One build info; every referenced buffer outlives the submit the
    // caller waits on.
    unsafe {
        (dispatch.fp().cmd_build_micromaps_ext)(cmd, 1, &build_info);
    }
    Ok((micromap, data, triangles, scratch))
}

/// Instance flags for an entity's opacity decision.
///
/// `None` leaves the geometry's own per-submesh flags in charge, which is what a micromap needs:
/// a per-instance `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` overrides an attached micromap outright per
/// spec, so forcing unconditionally would make every micromap inert. `Some` additionally disables
/// the micromap, because a micromap derived for the cooked material describes coverage this
/// instance's material does not have.
fn instance_opacity_flags(opacity_override: Option<bool>) -> vk::GeometryInstanceFlagsKHR {
    match opacity_override {
        None => vk::GeometryInstanceFlagsKHR::empty(),
        Some(true) => {
            vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE
                | vk::GeometryInstanceFlagsKHR::DISABLE_OPACITY_MICROMAPS_EXT
        }
        Some(false) => {
            vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE
                | vk::GeometryInstanceFlagsKHR::DISABLE_OPACITY_MICROMAPS_EXT
        }
    }
}

/// The build flags every per-mesh BLAS uses. `ALLOW_COMPACTION` is what makes the
/// compacted-size query legal, and compaction is not optional here: a static mesh's
/// structure lives for the whole session, so the memory the build over-reserves is held
/// for the whole session too.
///
/// `ALLOW_DISABLE_OPACITY_MICROMAPS_EXT` is a build-time permission rather than a runtime choice:
/// an instance may only carry `DISABLE_OPACITY_MICROMAPS_EXT` if the structure it references was
/// built allowing it, and an entity binding a material that disagrees with the cooked class needs
/// exactly that bit. It is gated on the device advertising `VK_EXT_opacity_micromap` — the flag is
/// not merely useless without the extension, it is an invalid enum value, so a software adapter
/// that never sees a micromap would fail the build outright.
fn mesh_blas_build_flags(omm_supported: bool) -> vk::BuildAccelerationStructureFlagsKHR {
    let base = vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
        | vk::BuildAccelerationStructureFlagsKHR::ALLOW_COMPACTION;
    if omm_supported {
        base | vk::BuildAccelerationStructureFlagsKHR::ALLOW_DISABLE_OPACITY_MICROMAPS_EXT
    } else {
        base
    }
}

/// Records the compacted copy of `source` into a freshly sized structure and returns it.
/// The caller must have waited the build submit and read `compacted_size` from a
/// `ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR` query.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if the destination structure cannot be created.
pub fn record_blas_compaction(
    resources: &Arc<DeviceResources>,
    dispatch: &accel::Device,
    cmd: vk::CommandBuffer,
    source: &AccelerationStructure,
    compacted_size: vk::DeviceSize,
) -> Result<AccelerationStructure> {
    let mut compacted = AccelerationStructure::create(
        resources,
        dispatch,
        compacted_size,
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
    )?;
    compacted.note_compacted_from(source.size());
    let copy = vk::CopyAccelerationStructureInfoKHR::default()
        .src(source.handle())
        .dst(compacted.handle())
        .mode(vk::CopyAccelerationStructureModeKHR::COMPACT);
    // SAFETY: the ash seam. Both structures are live; the source was built with
    // `ALLOW_COMPACTION` and its build has completed on the device.
    unsafe { dispatch.cmd_copy_acceleration_structure(cmd, &copy) };
    Ok(compacted)
}

/// Whether a cached tessellated BLAS whose backing store fits `cached_prims` worst-case triangles can be
/// reused for a build wanting `wanted_prims`. A tessellated BLAS is a full `MODE_BUILD` every frame, so
/// the *contents* never persist — only the AS backing-store *capacity* does. Reuse exactly when the
/// worst-case bound is unchanged; a bound change (a new factor cap / LOD) forces a fresh, larger AS.
/// `None` (no cached AS) is never reusable.
fn tess_blas_reuse(cached_prims: Option<u32>, wanted_prims: u32) -> bool {
    cached_prims == Some(wanted_prims)
}

/// Whether the skinned-refit planner skips an instance: a degenerate / untracked instance (no
/// geometry, no triangle, or entity 0), or a **tessellated** one — the latter takes the full-BUILD
/// path (`plan_tessellated_blas_builds`) because variable topology forbids the in-place `UPDATE` the
/// refit relies on. This is the sole discriminator between the two BLAS paths.
fn skinned_refit_skips(
    vertex_count: u32,
    index_count: u32,
    entity: u64,
    tessellated: bool,
) -> bool {
    vertex_count == 0 || index_count < 3 || entity == 0 || tessellated
}

/// A triangle-geometry descriptor over a device-address vertex + index stream
/// (`R32G32B32_SFLOAT` positions, `UINT32` indices, opaque). The vertex/index addresses
/// must reference live buffers for the build's duration.
/// Owns everything a triangles geometry descriptor borrows: the triangles struct, the optional
/// micromap chain, and the usage rows that chain points at.
///
/// `push_next` stores a raw pointer, so these must outlive the descriptor built from them —
/// which is why this is a struct rather than a function returning `<'static>`.
struct GeometryInputs<'a> {
    triangles: vk::AccelerationStructureGeometryTrianglesDataKHR<'a>,
    opaque: bool,
}

impl<'a> GeometryInputs<'a> {
    /// Builds the inputs, chaining `micromap` onto the triangles when one is supplied.
    fn new(
        vertex_data: vk::DeviceAddress,
        vertex_stride: vk::DeviceSize,
        vertex_count: u32,
        index_data: vk::DeviceAddress,
        opaque: bool,
        omm: Option<&'a mut vk::AccelerationStructureTrianglesOpacityMicromapEXT<'a>>,
    ) -> Self {
        let mut triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
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
        if let Some(omm) = omm {
            triangles = triangles.push_next(omm);
        }
        Self { triangles, opaque }
    }

    /// The descriptor, borrowing this value.
    fn geometry(&self) -> vk::AccelerationStructureGeometryKHR<'_> {
        // Opacity lives on the geometry, not the instance. An instance-level
        // `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` overrides any micromap outright per spec, so a
        // micromap under instance-level opacity would be inert.
        let flags = if self.opaque {
            vk::GeometryFlagsKHR::OPAQUE
        } else {
            vk::GeometryFlagsKHR::empty()
        };
        vk::AccelerationStructureGeometryKHR::default()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .flags(flags)
            .geometry(vk::AccelerationStructureGeometryDataKHR {
                triangles: self.triangles,
            })
    }
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

/// The AS-storage bytes the distinct structures in `retained` occupy, as
/// `(current, as_built)`. The second figure is what the same set would occupy had no
/// compaction copy run, so their difference is the saving compaction realized.
///
/// Deduplicates by device address: one mesh's structure appears once per instance that
/// references it, and charging it each time would report sharing as growth.
fn distinct_blas_bytes(retained: &[crate::RtBlas]) -> (u64, u64) {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    let mut current = 0;
    let mut as_built = 0;
    for structure in retained {
        if seen.contains(&structure.address()) {
            continue;
        }
        seen.push(structure.address());
        current += structure.size();
        as_built += structure.built_size();
    }
    (current, as_built)
}

/// `(structures, composed CLAS)` across the distinct cluster-composed bottom levels in
/// `retained`, deduplicated by device address for the same sharing reason as the byte sums.
fn distinct_cluster_blas(retained: &[crate::RtBlas]) -> (u32, u32) {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    let mut clusters = 0;
    for structure in retained {
        let crate::RtBlas::Cluster(blas) = structure else {
            continue;
        };
        if seen.contains(&blas.address()) {
            continue;
        }
        seen.push(blas.address());
        clusters += blas.cluster_count();
    }
    (seen.len() as u32, clusters)
}

/// A use record's family-local transform as a matrix. The record stores rows 0-2 of the
/// row-major 3×4; the implicit last row is `[0, 0, 0, 1]`.
fn assembly_use_matrix(use_record: &crate::GpuAssemblyUseRecord) -> Mat4 {
    let t = &use_record.transform;
    Mat4::from_cols_array(&[
        t[0], t[4], t[8], 0.0, //
        t[1], t[5], t[9], 0.0, //
        t[2], t[6], t[10], 0.0, //
        t[3], t[7], t[11], 1.0,
    ])
}

/// The number of distinct bottom-level structures a packed instance array references.
/// Instances of one mesh share a BLAS, so this is the count of unique AS device addresses.
fn distinct_blas_count(retained: &[crate::RtBlas]) -> u32 {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    for structure in retained {
        if !seen.contains(&structure.address()) {
            seen.push(structure.address());
        }
    }
    seen.len() as u32
}

/// Packs one `VkAccelerationStructureInstanceKHR`: a row-major 3×4 transform, the custom
/// index, a 0xFF mask, the triangle-cull-disable flag, and the referenced AS device address.
fn make_instance(
    rows: [f32; 12],
    custom_index: u32,
    opacity: vk::GeometryInstanceFlagsKHR,
    accel_reference: vk::DeviceAddress,
) -> vk::AccelerationStructureInstanceKHR {
    vk::AccelerationStructureInstanceKHR {
        transform: vk::TransformMatrixKHR { matrix: rows },
        instance_custom_index_and_mask: vk::Packed24_8::new(custom_index, 0xFF),
        instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(
            0,
            (vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE | opacity).as_raw() as u8,
        ),
        acceleration_structure_reference: vk::AccelerationStructureReferenceKHR {
            device_handle: accel_reference,
        },
    }
}

/// A device-local AS build/refit scratch buffer (`STORAGE | SHADER_DEVICE_ADDRESS`),
/// allocated at the device's `minAccelerationStructureScratchOffsetAlignment` — every
/// scratch address here is a buffer base address, and a misaligned one loses the device.
fn make_scratch_buffer(resources: &Arc<DeviceResources>, bytes: vk::DeviceSize) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::with_alignment(
        resources,
        bytes.max(256),
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &alloc_info,
        resources.scratch_alignment(),
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
        device.graphics_queue.submit2(
            raw,
            &submits,
            vk::Fence::null(),
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
    /// A neutral cut view: auto override at close range, so every input keeps its fine
    /// representation and the assertions about per-use expansion hold.
    fn test_cut_view() -> RtCutView {
        RtCutView {
            eye: [0.0; 3],
            proj_scale: 1_000.0,
            error_threshold_px: 1.0,
            representation_override: crate::SCENE_CUT_AUTO,
        }
    }

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

    /// A tessellated BLAS reuses its backing store only when the worst-case bound is unchanged: a full
    /// `MODE_BUILD` refills the geometry every frame, so nothing but the AS *capacity* persists. A first
    /// sight (no cache) and any bound change both force a fresh AS; never an in-place `UPDATE`.
    #[test]
    fn tessellated_blas_reuses_only_on_unchanged_worst_case() {
        assert!(!tess_blas_reuse(None, 100), "no cache is never reusable");
        assert!(tess_blas_reuse(Some(100), 100), "same bound reuses the AS");
        assert!(
            !tess_blas_reuse(Some(64), 256),
            "a grown bound (higher factor cap) forces a fresh AS"
        );
        assert!(
            !tess_blas_reuse(Some(256), 64),
            "a shrunk bound also recreates (never keep an oversized AS around)"
        );
    }

    /// The tessellated BUILD's CPU range is the worst-case closed form: `worst_case_prims` triangles and
    /// `max_vertex = worst_case_verts - 1`, matching the dice contract the emit kernel reserves. The IB
    /// tail past the GPU-packed triangles is degenerate-padded, so building the full range is watertight.
    #[test]
    fn tessellated_blas_build_range_matches_worst_case_reservation() {
        use crate::tessellation::tess_worst_case;
        // Two base triangles at a factor cap of 8: L=8 ⇒ verts=(9)(10)/2=45, tris=64 per triangle.
        let (verts, indices) = tess_worst_case(2, 8);
        assert_eq!(verts, 2 * 45);
        assert_eq!(indices, 2 * 64 * 3);
        // The planner's BUILD range: primitive_count = indices/3, max_vertex = verts - 1.
        let worst_case_prims = (indices / 3) as u32;
        let max_vertex = verts as u32 - 1;
        assert_eq!(worst_case_prims, 2 * 64);
        assert_eq!(max_vertex, 2 * 45 - 1);
    }

    /// A tessellated instance (`tess: Some`) takes the full-BUILD path, never the skinned in-place refit:
    /// `tessellated` is the sole discriminator, so a healthy instance marked tessellated is skipped by the
    /// refit and planned once by the BUILD planner. Degenerate / untracked instances are skipped by both.
    #[test]
    fn tess_flag_selects_the_build_path_over_the_skinned_refit() {
        // A healthy, non-tessellated instance takes the skinned refit (not skipped).
        assert!(!skinned_refit_skips(100, 300, 7, false));
        // The same instance marked tessellated is skipped by the refit → the BUILD planner claims it.
        assert!(skinned_refit_skips(100, 300, 7, true));
        // Degenerate / untracked instances are skipped regardless of the tessellation flag.
        assert!(skinned_refit_skips(0, 300, 7, false), "no vertices");
        assert!(
            skinned_refit_skips(100, 2, 7, false),
            "sub-triangle index count"
        );
        assert!(
            skinned_refit_skips(100, 300, 0, false),
            "untracked (entity 0)"
        );
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

    /// A two-buffer [`GpuMesh`] with no BLAS, enough for scene-capture bookkeeping tests.
    fn test_mesh(device: &Device) -> Arc<crate::GpuMesh> {
        use vk_mem::Alloc;
        let make_buffer = |size: vk::DeviceSize, usage: vk::BufferUsageFlags| {
            let alloc_info = vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            };
            let info = vk::BufferCreateInfo::default().size(size).usage(usage);
            // SAFETY: the VMA seam. Ownership passes into the GpuMesh below.
            unsafe {
                device
                    .resources()
                    .allocator()
                    .create_buffer(&info, &alloc_info)
            }
            .expect("create_buffer")
        };
        Arc::new(crate::GpuMesh::from_parts(
            device.resources(),
            crate::GpuMeshParts {
                cooked_opaque: true,
                micromaps: Vec::new(),
                vertex: make_buffer(96, vk::BufferUsageFlags::VERTEX_BUFFER),
                index: make_buffer(48, vk::BufferUsageFlags::INDEX_BUFFER),
                skin: None,
                morph: None,
                conditioning: None,
                index_count: 3,
                vertex_count: 3,
                submeshes: Vec::new(),
                bounds_min: saffron_geometry::glam::Vec3::ZERO,
                bounds_max: saffron_geometry::glam::Vec3::ONE,
                cpu_vertices: Vec::new(),
                cpu_indices: Vec::new(),
                cpu_skin: Vec::new(),
                blas: None,
                assembly_blas: Vec::new(),
                aggregate_blas: None,
                sdfs: Vec::new(),
                hierarchy_pages: Vec::new(),
                assembly: None,
            },
        ))
    }

    /// `make_instance` packs the custom index + 0xFF mask, the triangle-cull-disable flag
    /// plus the per-instance opacity flag, and the referenced AS device address into a
    /// `VkAccelerationStructureInstanceKHR`.
    #[test]
    fn make_instance_packs_index_mask_flags_and_reference() {
        let inst = make_instance(
            IDENTITY_ROWS,
            7,
            vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE,
            0xDEAD_BEEF,
        );
        assert_eq!(inst.instance_custom_index_and_mask.low_24(), 7);
        assert_eq!(inst.instance_custom_index_and_mask.high_8(), 0xFF);
        assert_eq!(
            inst.instance_shader_binding_table_record_offset_and_flags
                .high_8(),
            (vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE
                | vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE)
                .as_raw() as u8
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
        rt.set_rt_scene(Vec::new());
        assert!(!rt.build_pending());

        // The build path is a no-op: it produces no plan and leaves tlas_ready false.
        let plan = rt.prepare_tlas_build(&device, 0, &[], None, test_cut_view());
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
        rt.set_rt_scene(Vec::new());
        assert!(!rt.build_pending());

        rt.set_rt_shadows(true);
        rt.set_rt_scene(Vec::new());
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
        let mesh = test_mesh(&device);
        rt.set_rt_scene(vec![
            RtInstanceInput {
                model: Mat4::IDENTITY,
                mesh: Arc::clone(&mesh),
                custom_index: RT_UNMIRRORED_INSTANCE,
                opacity_override: Some(true),
                combination: 0,
            },
            RtInstanceInput {
                model: Mat4::IDENTITY,
                mesh,
                custom_index: 5,
                opacity_override: Some(false),
                combination: 0,
            },
        ]);
        assert!(rt.has_instances(&[]));
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
        use crate::upload::Uploader;
        use saffron_geometry::glam::{Vec2, Vec3};
        use saffron_geometry::{Mesh, Submesh, Vertex};

        let Some((device, descriptors, mut rt, before)) = rt_or_skip() else {
            return;
        };
        if !rt.supported() {
            // No RT extensions: the build path is a verified no-op (covered above). The GPU
            // TLAS build is DEFERRED-NEEDS-HARDWARE on a software device.
            assert!(
                rt.prepare_tlas_build(&device, 0, &[], None, test_cut_view())
                    .is_none()
            );
            drop(rt);
            device.wait_idle().expect("wait_idle");
            return;
        }

        // Upload a unit triangle; on an RT device this builds its BLAS at upload time.
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader");
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
            ..Vertex::default()
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
        let hierarchy = crate::upload::hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
        let gpu_mesh = uploader
            .upload_mesh(
                &descriptors,
                &mesh,
                &hierarchy,
                &[],
                None,
                crate::SdfSource::None,
            )
            .expect("upload_mesh");
        assert!(
            gpu_mesh.blas.is_some(),
            "RT device builds the mesh BLAS at upload"
        );

        // Arm RT shadows + capture one static instance, then prepare the per-frame TLAS build.
        rt.set_rt_shadows(true);
        rt.set_rt_scene(vec![RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: Arc::clone(&gpu_mesh),
            custom_index: RT_UNMIRRORED_INSTANCE,
            opacity_override: Some(true),
            combination: 0,
        }]);
        assert!(rt.build_pending());
        let plan = rt
            .prepare_tlas_build(&device, 0, &[], None, test_cut_view())
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
            // Unarmed recorders: `scope` is a transparent wrapper, so the test records exactly
            // the same commands the profiled path does.
            let mut scopes =
                crate::nested_scopes::NestedScopeRecorder::new(device.raw(), cmd, None, None);
            record_tlas_build_plan(device.raw(), &plan, &mut scopes);
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

    /// A derived micromap builds on the device, validation-clean, with storage allocated.
    ///
    /// This is the seam between the CPU derivation and Vulkan: the state bytes, the per-triangle
    /// block descriptors, and the index stream all have to land in the exact layout the build
    /// expects, and a wrong stride or a missing usage row shows up here rather than as a subtly
    /// wrong shadow much later.
    #[test]
    fn a_derived_micromap_builds_validation_clean() {
        use saffron_geometry::{CoverageRule, CoverageSourcePlane, derive_opacity_micromap};
        use saffron_material::{AlphaClassification, OpacityMicromapDerivation, SurfaceUnit};

        let Some((device, _descriptors, _rt, _before)) = rt_or_skip() else {
            return;
        };
        let Some(dispatch) = device.omm_dispatch().cloned() else {
            eprintln!("skipping: no VK_EXT_opacity_micromap on this device");
            return;
        };

        // A gradient so the derivation settles both extremes and leaves a middle band unknown —
        // a uniform plane would emit only special indices and build nothing at all.
        const EXTENT: u32 = 32;
        let alpha: Vec<u8> = (0..EXTENT * EXTENT)
            .map(|i| ((i % EXTENT) * 255 / (EXTENT - 1)) as u8)
            .collect();
        let derived = derive_opacity_micromap(
            &[0, 1, 2],
            &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            CoverageSourcePlane {
                alpha: &alpha,
                width: EXTENT,
                height: EXTENT,
                rule: CoverageRule::new(AlphaClassification::Masked, false, 0.5),
                base_alpha: 1.0,
            },
            &OpacityMicromapDerivation {
                enabled: true,
                max_subdivision: 3,
                transparent_threshold: SurfaceUnit::from_bits(0),
                opaque_threshold: SurfaceUnit::from_bits(u16::MAX),
            },
        );
        assert!(
            !derived.blocks.is_empty(),
            "the gradient must produce a block"
        );

        let before = validation_issue_count();
        let resources = std::sync::Arc::clone(device.resources());
        let raw = device.raw();
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(device.graphics_queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: the ash seam. The pool is destroyed below on every path.
        let pool = unsafe { raw.create_command_pool(&pool_info, None) }.expect("command pool");
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the pool just created.
        let cmd = unsafe { raw.allocate_command_buffers(&alloc) }.expect("command buffer")[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begin/end bracket the recording below.
        unsafe { raw.begin_command_buffer(cmd, &begin) }.expect("begin");
        let built = record_micromap_build(&resources, &dispatch, cmd, &derived)
            .expect("micromap build records");
        // SAFETY: the ash seam. Ends the recording opened above.
        unsafe { raw.end_command_buffer(cmd) }.expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submits, vk::Fence::null(), "micromap build test")
            .expect("submit");
        device.wait_idle().expect("idle after the build submit");

        assert!(built.0.size() > 0, "the micromap reserved storage");
        assert_eq!(
            built.0.classes(),
            (
                derived.classes.opaque,
                derived.classes.transparent,
                derived.classes.unknown
            )
        );
        drop(built);
        // SAFETY: the ash seam. The queue is idle, so the pool is free to destroy.
        unsafe { raw.destroy_command_pool(pool, None) };
        device.wait_idle().expect("idle before teardown");
        assert_eq!(
            validation_issue_count(),
            before,
            "the micromap build must be validation-clean"
        );
    }
}

/// Counts the distinct micromaps the frame's instances reference and sums what they settled.
///
/// Deduplicated by micromap handle for the same reason the BLAS bytes are: instances of one mesh
/// share its structures, and counting a micromap once per instance would report sharing as work.
fn distinct_micromap_classes(instances: &[RtInstanceInput]) -> (u32, (u64, u64, u64)) {
    let mut seen = std::collections::BTreeSet::new();
    let mut classes = (0_u64, 0_u64, 0_u64);
    for input in instances {
        for micromap in &input.mesh.micromaps {
            if !seen.insert(micromap.handle()) {
                continue;
            }
            let (opaque, transparent, unknown) = micromap.classes();
            classes.0 += opaque;
            classes.1 += transparent;
            classes.2 += unknown;
        }
    }
    (u32::try_from(seen.len()).unwrap_or(u32::MAX), classes)
}
