//! The per-frame instance set (set 2) storage and the deformation gather/wiring.
//!
//! [`Instancing`] owns, per frame-in-flight, the instance descriptor set: the
//! joint palette, the
//! global material-parameter arena (binding 2, rebound by the renderer each frame),
//! the GPU-scene address block (binding 3), and the frame's semantic record stream
//! (binding 4). [`gather_instance_deformation`] collects each deforming instance's
//! skin/morph/tessellation work; [`Instancing::wire_gathered_deformations`] uploads
//! the palettes and wires the dispatches onto the frame's [`FrameDeformation`]. The
//! deformed buffers + dispatch pool live in [`crate::skinning::Skinning`].

use ash::vk;
use saffron_core::{BlendMode, HeightMode};
use saffron_geometry::glam::{Mat4, UVec4, Vec4};

use crate::descriptors::Descriptors;
use crate::draw_list::{
    DeformedRtInstance, FrameDeformation, MorphDispatch, SkinDispatch, SubmeshMaterial,
};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::MaterialParamsData;
use crate::resources::{Buffer, DeviceResources, GpuMesh};
use crate::skinning::{SkinBucket, SkinBufferSet, Skinning, clamp_to_set_budget};
use crate::tessellation::{TESS_MAX_INSTANCES, TessBucket};
use crate::{Device, Result};

use std::sync::Arc;

/// Initial joint-palette capacity (in [`Mat4`] matrices).
const INITIAL_JOINT_CAPACITY: u32 = 128;

/// Initial active-target capacity (in [`ActiveTarget`] entries).
const INITIAL_ACTIVE_CAPACITY: u32 = 128;

/// Morph weights below this magnitude are dropped during compaction (UE's
/// `GMorphTargetWeightThreshold` analogue), so a rest-pose morph mesh dispatches nothing.
const MORPH_WEIGHT_THRESHOLD: f32 = 1.0e-3;

/// One compacted active morph target, matching `morph.slang`'s `ActiveTarget` (16 bytes):
/// the target index into the mesh's ranges, the cumulative delta count of preceding active
/// targets (the flat scatter base), and the resolved weight.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ActiveTarget {
    target_index: u32,
    scatter_base: u32,
    weight: f32,
    _pad: f32,
}

/// One frame-in-flight's grow-only storage: the instance + material SSBOs, the current +
/// previous joint palettes (and their element capacities), plus the descriptor set
/// binding the instance + material + current-palette SSBOs.
struct FrameInstancing {
    set: vk::DescriptorSet,
    /// The current joint palette (set 2, binding 1): `worldBone * inverseBind` per joint.
    joints: Option<Buffer>,
    joint_capacity: u32,
    /// The previous frame's palette, same layout (same per-instance `joint_offset`), fed
    /// to the prev skin dispatch for motion. Not bound to set 2.
    prev_joints: Option<Buffer>,
    prev_joint_capacity: u32,
    /// The frame's compacted active-target list (all morph instances concatenated), bound
    /// by every morph dispatch's set at binding 3. Each instance reads its slice; the
    /// per-instance `scatter_base` chain is relative to that instance's slice.
    active_targets: Option<Buffer>,
    active_capacity: u32,
}

/// The per-frame material-parameter storage and the instance descriptor sets.
///
/// Built once in [`Instancing::new`] (it allocates one instance set per frame slot),
/// then mutated only through the deformation wiring taking
/// `&mut self` plus the device / descriptors. Each [`Buffer`] is a [`crate::Buffer`]
/// Drop type holding the allocator `Arc`, so the SSBOs free without a live `&Device`.
pub struct Instancing {
    resources: Arc<DeviceResources>,
    frames: Vec<FrameInstancing>,
}

/// One deforming instance's frame facts: the inputs the
/// skin/morph/tessellation wiring needs (animation evaluation stays CPU simulation).
pub struct DeformationWork {
    /// The mesh supplying static + skin/morph streams.
    pub mesh: Arc<GpuMesh>,
    /// Source entity id (0 = none), keying the cross-frame motion caches.
    pub entity: u64,
    /// Whether the instance skins (the mesh must carry a skin stream).
    pub skinned: bool,
    /// The base of this instance's joints in the frame palette.
    pub joint_offset: u32,
    /// This instance's joint count.
    pub joint_count: u32,
    /// Morph weights (empty = not a morph instance).
    pub morph_weights: Vec<f32>,
    /// The instance's world matrix.
    pub model: Mat4,
    /// Displacement-tessellation facts when the instance displaces.
    pub displace: Option<DisplaceInfo>,
    /// The instance's slot in the persistent GPU scene's instance table, or
    /// [`crate::RT_UNMIRRORED_INSTANCE`] when it is not mirrored. The visibility
    /// traversal looks a displaced instance's amplification row up by this slot.
    pub instance_slot: u32,
}

/// The frame's gathered deformation outputs, consumed by the record-driven frame driver.
#[derive(Default)]
pub struct DeformationGather {
    /// Per skinned instance: the palette + deformed-slice wiring.
    pub skin_buckets: Vec<SkinBucket>,
    /// Per skinned instance: the provider-params patch facts.
    pub skinned_deformations: Vec<crate::SkinnedDeformation>,
    /// Skinned TLAS refit entries.
    pub skinned_rt: Vec<DeformedRtInstance>,
    /// Morph deform dispatches (before skin).
    pub morph_dispatches: Vec<MorphDispatch>,
    /// Previous-pose morph dispatches (deformation motion).
    pub prev_morph_dispatches: Vec<MorphDispatch>,
    /// The morph dispatches' meshes, parallel.
    pub morph_meshes: Vec<Arc<GpuMesh>>,
    /// The frame's concatenated active-target list.
    pub active_targets: Vec<ActiveTarget>,
    /// Unskinned-morph TLAS entries.
    pub morph_rt: Vec<DeformedRtInstance>,
    /// Adaptive-tessellation prep buckets.
    pub tess_buckets: Vec<TessBucket>,
    /// Displaced-instance TLAS entries.
    pub displaced_rt: Vec<DeformedRtInstance>,
    /// The running deformed-ring cursor (vertices).
    pub deformed_cursor: u32,
}

/// Tessellation shape parameters for one frame's gather.
#[derive(Clone, Copy)]
pub struct TessGatherParams {
    /// Whether RT consumers track deformed instances this frame.
    pub rt_skinned: bool,
    /// The clamp on per-edge tessellation factors.
    pub factor_cap: f32,
    /// The minimum per-edge factor.
    pub min_factor: f32,
    /// The screen-space edge-length target (pixels).
    pub edge_length_target: f32,
}

/// Gathers one deforming instance's skin/morph/tess work into `gather`, advancing the
/// frame's deformed-ring cursor. Returns the instance's deformed-ring base vertex, or
/// `None` when it claims no slice (not skinned and no above-threshold morph targets).
pub fn gather_instance_deformation(
    gather: &mut DeformationGather,
    skinning: &mut Skinning,
    work: &DeformationWork,
    joints: &[Mat4],
    prev_joints: &mut [Mat4],
    params: TessGatherParams,
) -> Option<u32> {
    let (morph_active, scatter_count) = match work.mesh.morph() {
        Some(morph) if !work.morph_weights.is_empty() => {
            build_active_targets(morph, &work.morph_weights)
        }
        _ => (Vec::new(), 0),
    };
    let has_morph = !morph_active.is_empty();
    let deformed = work.skinned || has_morph;
    let deformed_vertex_offset = deformed.then_some(gather.deformed_cursor);
    if deformed {
        let vertex_count = work.mesh.vertex_count;
        if work.skinned {
            gather.skin_buckets.push(SkinBucket {
                mesh: Arc::clone(&work.mesh),
                joint_offset: work.joint_offset,
                deformed_offset: gather.deformed_cursor,
            });
            gather.skinned_deformations.push(crate::SkinnedDeformation {
                entity: work.entity,
                joint_offset: work.joint_offset,
                joint_count: work.joint_count,
                deformed_offset: gather.deformed_cursor,
                vertex_count,
            });
            gather.skinned_rt.push(DeformedRtInstance {
                entity: if params.rt_skinned { work.entity } else { 0 },
                deformed_offset: gather.deformed_cursor,
                vertex_count,
                index_count: work.mesh.index_count,
                mesh: Arc::clone(&work.mesh),
                world_transform: Mat4::IDENTITY,
                tess: None,
            });
            let lo = work.joint_offset as usize;
            let hi = lo + work.joint_count as usize;
            if work.entity != 0 && work.joint_count > 0 && hi <= joints.len() {
                let cached = skinning.swap_palette(work.entity, &joints[lo..hi]);
                prev_joints[lo..hi].copy_from_slice(&cached);
            }
        }
        if has_morph {
            let active_base = gather.active_targets.len() as u32;
            let active_count = morph_active.len() as u32;
            gather.active_targets.extend_from_slice(&morph_active);
            gather.morph_dispatches.push(MorphDispatch {
                set: vk::DescriptorSet::null(),
                vertex_count,
                scatter_count,
                active_count,
                active_base,
                deformed_offset: gather.deformed_cursor,
            });
            let prev_weights = skinning.swap_morph_weights(work.entity, &work.morph_weights);
            let (prev_active, prev_scatter) = match work.mesh.morph() {
                Some(morph) => build_active_targets(morph, &prev_weights),
                None => (Vec::new(), 0),
            };
            let prev_active_base = gather.active_targets.len() as u32;
            let prev_active_count = prev_active.len() as u32;
            gather.active_targets.extend_from_slice(&prev_active);
            gather.prev_morph_dispatches.push(MorphDispatch {
                set: vk::DescriptorSet::null(),
                vertex_count,
                scatter_count: prev_scatter,
                active_count: prev_active_count,
                active_base: prev_active_base,
                deformed_offset: gather.deformed_cursor,
            });
            gather.morph_meshes.push(Arc::clone(&work.mesh));
            if !work.skinned {
                gather.morph_rt.push(DeformedRtInstance {
                    entity: if params.rt_skinned { work.entity } else { 0 },
                    deformed_offset: gather.deformed_cursor,
                    vertex_count,
                    index_count: work.mesh.index_count,
                    mesh: Arc::clone(&work.mesh),
                    world_transform: work.model,
                    tess: None,
                });
            }
        }
        gather.deformed_cursor += vertex_count;
    }
    if let Some(info) = work.displace {
        if work.mesh.conditioning().is_some() {
            gather.tess_buckets.push(TessBucket {
                mesh: Arc::clone(&work.mesh),
                instance_slot: work.instance_slot,
                entity: if params.rt_skinned { work.entity } else { 0 },
                model: work.model,
                height_index: info.height_index,
                height_scale: info.height_scale,
                uv_transform: info.uv_transform,
                vector_index: info.vector_index,
                factor_cap: params.factor_cap,
                min_factor: params.min_factor,
                edge_length_target: params.edge_length_target,
            });
        }
        gather.displaced_rt.push(DeformedRtInstance {
            entity: if params.rt_skinned { work.entity } else { 0 },
            deformed_offset: 0,
            vertex_count: work.mesh.vertex_count,
            index_count: work.mesh.index_count,
            mesh: Arc::clone(&work.mesh),
            world_transform: work.model,
            tess: None,
        });
    }
    deformed_vertex_offset
}

impl Instancing {
    /// Allocates one instance descriptor set per frame-in-flight from the shared pool.
    /// The SSBOs are created lazily on the first frame upload that
    /// needs them (the buffers start null and grow on demand).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if a descriptor set cannot be allocated.
    pub fn new(device: &Device, descriptors: &Descriptors) -> Result<Self> {
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let set = descriptors.allocate_set(descriptors.instance_set_layout())?;
            frames.push(FrameInstancing {
                set,
                joints: None,
                joint_capacity: 0,
                prev_joints: None,
                prev_joint_capacity: 0,
                active_targets: None,
                active_capacity: 0,
            });
        }
        Ok(Self {
            resources: Arc::clone(device.resources()),
            frames,
        })
    }

    /// The frame slot's instance descriptor set (set 2), bound by the scene + depth
    /// passes. Valid for the renderer's lifetime.
    pub fn instance_set(&self, frame: usize) -> vk::DescriptorSet {
        self.frames[frame].set
    }

    /// Uploads the frame's palettes and wires the gathered skin/morph/tessellation
    /// work into `list`, the deformation state the frame's compute and raster passes read.
    #[allow(clippy::too_many_arguments)]
    pub fn wire_gathered_deformations(
        &mut self,
        skinning: &mut Skinning,
        frame: usize,
        gather: DeformationGather,
        joints: &[Mat4],
        prev_joints: Vec<Mat4>,
        list: &mut FrameDeformation,
    ) -> Result<()> {
        // Upload the current joint palette + the previous one (fed to the prev skin
        // dispatch). Only when the scene supplied a palette.
        if !joints.is_empty() {
            self.ensure_joint_capacity(frame, joints.len() as u32)?;
            upload_into(
                self.frames[frame].joints.as_mut().expect("joint buffer"),
                bytemuck::cast_slice(joints),
            );
            self.ensure_prev_joint_capacity(frame, prev_joints.len() as u32)?;
            upload_into(
                self.frames[frame]
                    .prev_joints
                    .as_mut()
                    .expect("prev joint buffer"),
                bytemuck::cast_slice(&prev_joints),
            );
        }

        // Size the deformed buffers + wire the per-instance skin dispatches. A skinned
        // instance with no palette this frame can't be deformed: drop the skin work so
        // the skin pass is skipped and the geometry reads the undeformed bind pose.
        let DeformationGather {
            mut skin_buckets,
            skinned_deformations,
            mut skinned_rt,
            mut morph_dispatches,
            mut prev_morph_dispatches,
            morph_meshes,
            active_targets,
            morph_rt,
            mut tess_buckets,
            mut displaced_rt,
            deformed_cursor,
        } = gather;
        list.skinned_deformations = skinned_deformations;
        let skin_ran = !skin_buckets.is_empty() && !joints.is_empty();
        if skin_ran {
            self.wire_skin_dispatches(
                frame,
                skinning,
                deformed_cursor,
                &mut skin_buckets,
                &mut skinned_rt,
                list,
            )?;
        } else if !skin_buckets.is_empty() {
            tracing::warn!(
                "skinning: skinned instances present but no joint palette uploaded; skipping"
            );
        }

        // Upload the frame's active-target list + wire the per-instance morph dispatches
        // (recorded before skin). The pool is reset here only when skin didn't run; the
        // accumulator is sized to the largest single morph mesh (reused serially).
        if !morph_dispatches.is_empty() {
            let accum_vertices = morph_meshes
                .iter()
                .map(|m| m.vertex_count)
                .max()
                .unwrap_or(0);
            self.ensure_active_capacity(frame, active_targets.len() as u32)?;
            upload_into(
                self.frames[frame]
                    .active_targets
                    .as_mut()
                    .expect("active buffer"),
                bytemuck::cast_slice(&active_targets),
            );
            let (active_buf, active_size) = {
                let buffer = self.frames[frame]
                    .active_targets
                    .as_ref()
                    .expect("active buffer");
                (buffer.handle(), buffer.size())
            };
            skinning.wire_morph_dispatches(
                frame,
                deformed_cursor,
                accum_vertices,
                !skin_ran,
                active_buf,
                active_size,
                &morph_meshes,
                &mut morph_dispatches,
                &mut prev_morph_dispatches,
            )?;
            list.morph_dispatches = morph_dispatches;
            list.prev_morph_dispatches = prev_morph_dispatches;
            // Unskinned-morph instances enter the TLAS here (skinned ones were added by
            // `wire_skin_dispatches`); drop the non-RT-armed placeholders.
            list.deformed_rt_instances
                .extend(morph_rt.into_iter().filter(|s| s.entity != 0));
        }

        // Feed the displaced instances' RT entries into the deformation frame. They ride the
        // transient tessellation buffers (their `tess` slice is filled mid-render by
        // `record_tess_prep`), not the deformed ring — but the deformed buffers must
        // exist for any deform work this frame, so size them to the deform cursor
        // (idempotent; skin/morph already sized them). Clamp to the tessellation
        // instance budget, matching how `tess_buckets` is truncated below.
        if !displaced_rt.is_empty() {
            skinning.ensure_deformed_buffers(frame, deformed_cursor)?;
            displaced_rt.truncate(TESS_MAX_INSTANCES as usize);
            list.deformed_rt_instances
                .extend(displaced_rt.into_iter().filter(|s| s.entity != 0));
        }

        // Hand the deform scope the adaptive-tessellation buckets, clamped to the
        // per-frame descriptor budget (the subsystem's pool holds `TESS_MAX_INSTANCES`
        // set trios).
        tess_buckets.truncate(TESS_MAX_INSTANCES as usize);
        list.tess_buckets = tess_buckets;
        Ok(())
    }

    /// Clamps the skin work to the per-frame set budget, sizes the deformed buffers, and
    /// wires one descriptor set per dispatch (current + prev pose) through `skinning`,
    /// filling `list.skin_dispatches` / `prev_skin_dispatches` / `deformed_rt_instances`.
    /// A wiring failure leaves the lists empty (the skin pass is skipped).
    fn wire_skin_dispatches(
        &mut self,
        frame: usize,
        skinning: &mut Skinning,
        deformed_cursor: u32,
        skin_buckets: &mut Vec<SkinBucket>,
        skinned_rt: &mut Vec<DeformedRtInstance>,
        list: &mut FrameDeformation,
    ) -> Result<()> {
        let kept = clamp_to_set_budget(skin_buckets.len());
        skin_buckets.truncate(kept);
        skinned_rt.truncate(kept);

        let mut dispatches: Vec<SkinDispatch> = skin_buckets
            .iter()
            .map(|b| SkinDispatch {
                set: vk::DescriptorSet::null(),
                vertex_count: b.mesh.vertex_count,
                joint_offset: b.joint_offset,
                deformed_offset: b.deformed_offset,
            })
            .collect();
        let mut prev_dispatches = dispatches.clone();

        let frame_state = &self.frames[frame];
        let joints = frame_state.joints.as_ref().expect("joints uploaded");
        let prev = frame_state
            .prev_joints
            .as_ref()
            .expect("prev joints uploaded");
        let buffers = SkinBufferSet {
            palette: joints.handle(),
            palette_size: joints.size(),
            prev_palette: prev.handle(),
            prev_palette_size: prev.size(),
        };
        let wired = skinning.wire_dispatches(
            frame,
            deformed_cursor,
            buffers,
            skin_buckets,
            &mut dispatches,
            &mut prev_dispatches,
        )?;
        if wired {
            list.skin_dispatches = dispatches;
            list.prev_skin_dispatches = prev_dispatches;
            // Keep only the real RT skinned instances (drop entity-less placeholders).
            list.deformed_rt_instances = skinned_rt.drain(..).filter(|s| s.entity != 0).collect();
        }
        Ok(())
    }

    /// Ensures the frame's joint palette holds at least `count` [`Mat4`] matrices (same
    /// grow-only policy). The skin dispatch sets bind it per instance.
    fn ensure_joint_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].joints.is_some() && self.frames[frame].joint_capacity >= count {
            return Ok(());
        }
        let capacity = grow_capacity(
            self.frames[frame].joint_capacity,
            INITIAL_JOINT_CAPACITY,
            count,
        );
        let size = u64::from(capacity) * size_of::<Mat4>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        self.frames[frame].joints = Some(buffer);
        self.frames[frame].joint_capacity = capacity;
        Ok(())
    }

    /// The prev-joint sibling of [`Instancing::ensure_joint_capacity`]: same grow-only
    /// policy, NOT bound to set 2 (only the current palette feeds the scene shader); the
    /// prev skin dispatch reads it directly.
    fn ensure_prev_joint_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].prev_joints.is_some()
            && self.frames[frame].prev_joint_capacity >= count
        {
            return Ok(());
        }
        let capacity = grow_capacity(
            self.frames[frame].prev_joint_capacity,
            INITIAL_JOINT_CAPACITY,
            count,
        );
        let size = u64::from(capacity) * size_of::<Mat4>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        self.frames[frame].prev_joints = Some(buffer);
        self.frames[frame].prev_joint_capacity = capacity;
        Ok(())
    }

    /// Ensures the frame's active-target buffer holds at least `count` [`ActiveTarget`]
    /// entries (same grow-only policy). Not bound to set 2 — each morph dispatch's own set
    /// binds it at binding 3, indexed by the dispatch's `active_base`.
    fn ensure_active_capacity(&mut self, frame: usize, count: u32) -> Result<()> {
        if self.frames[frame].active_targets.is_some()
            && self.frames[frame].active_capacity >= count
        {
            return Ok(());
        }
        let capacity = grow_capacity(
            self.frames[frame].active_capacity,
            INITIAL_ACTIVE_CAPACITY,
            count,
        );
        let size = u64::from(capacity) * size_of::<ActiveTarget>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        self.frames[frame].active_targets = Some(buffer);
        self.frames[frame].active_capacity = capacity;
        Ok(())
    }
}

/// Compacts a morph instance's per-target weights into the above-threshold active list,
/// each entry carrying its cumulative scatter base (the running delta count of preceding
/// active targets). Returns the active list + the total scatter count (its dispatch size).
/// An all-rest (every weight below threshold) instance returns an empty list.
fn build_active_targets(
    morph: &crate::resources::MorphBuffers,
    weights: &[f32],
) -> (Vec<ActiveTarget>, u32) {
    let mut active = Vec::new();
    let mut scatter_base = 0u32;
    for (k, &weight) in weights.iter().enumerate() {
        if weight.abs() < MORPH_WEIGHT_THRESHOLD || k >= morph.cpu_ranges.len() {
            continue;
        }
        let delta_count = morph.cpu_ranges[k][1];
        active.push(ActiveTarget {
            target_index: k as u32,
            scatter_base,
            weight,
            _pad: 0.0,
        });
        scatter_base += delta_count;
    }
    (active, scatter_base)
}

/// The height-map index + amplitude + uv transform a displaced mesh-instance's adaptive-tessellation
/// prep needs, derived from a submesh material. The whole mesh-instance is displaced by one height
/// field (its first displacement-enabled submesh material).
#[derive(Clone, Copy)]
pub struct DisplaceInfo {
    height_index: u32,
    height_scale: f32,
    uv_transform: [f32; 4],
    vector_index: u32,
}

/// The displacement info for an instance's resolved submesh materials, if any is
/// displacement-enabled with a height map. A mesh-instance is displaced as a whole by the first
/// such material (terrain/displaced planes carry a single material; multi-material displacement
/// picks the first). A bound vector-displacement map switches the kernel to tangent-space vector
/// offset. `None` → not displaced.
#[must_use]
pub fn displace_info_from(materials: &[crate::draw_list::SubmeshMaterial]) -> Option<DisplaceInfo> {
    materials.iter().find_map(|m| {
        let texture = m.height_texture.as_ref()?;
        (m.height_mode == HeightMode::Displacement).then(|| DisplaceInfo {
            height_index: texture.bindless_index(),
            height_scale: m.height_scale,
            uv_transform: [m.uv_tiling.x, m.uv_tiling.y, m.uv_offset.x, m.uv_offset.y],
            vector_index: m
                .vector_displacement_texture
                .as_ref()
                .map_or(0, |t| t.bindless_index()),
        })
    })
}

/// Packs a [`SubmeshMaterial`] into the std430 [`MaterialParamsData`], resolving each
/// texture to its bindless index (default white when absent) and setting the feature
/// bits, while pinning the live texture `Arc`s. Returns the params plus the albedo +
/// metallic-roughness indices the instance row also carries.
pub fn resolve_material_params(
    material: &SubmeshMaterial,
    default_texture_index: u32,
    coverage_temporal_phase: u32,
    live_textures: &mut Vec<Arc<crate::GpuTexture>>,
) -> (MaterialParamsData, u32, u32) {
    let mut albedo_index = default_texture_index;
    let mut mr_index = default_texture_index;
    let mut normal_index = default_texture_index;
    let mut occlusion_index = default_texture_index;
    let mut emissive_index = default_texture_index;
    let mut height_index = default_texture_index;
    let mut coverage_index = default_texture_index;
    let mut features = 0u32;

    let mut pin = |texture: &Option<Arc<crate::GpuTexture>>, slot: &mut u32| -> bool {
        if let Some(texture) = texture {
            *slot = texture.bindless_index();
            live_textures.push(Arc::clone(texture));
            true
        } else {
            false
        }
    };
    pin(&material.albedo_texture, &mut albedo_index);
    pin(&material.metallic_roughness_texture, &mut mr_index);
    if pin(&material.normal_texture, &mut normal_index) {
        features |= FEATURE_NORMAL;
    }
    if pin(&material.emissive_texture, &mut emissive_index) {
        features |= FEATURE_EMISSIVE_TEX;
    }
    if pin(&material.occlusion_texture, &mut occlusion_index) {
        features |= FEATURE_OCCLUSION;
    }
    if pin(&material.height_texture, &mut height_index) {
        // The mode selects the technique: `Bump` is a fragment shading-normal bump (safe, flat
        // silhouette); `Parallax` is the fragment parallax-occlusion march; `Displacement` moves
        // real geometry via the adaptive-tessellation passes (true silhouette, consistent across
        // passes, BLAS-able) and keeps the shading bump.
        features |= match material.height_mode {
            HeightMode::Bump => FEATURE_HEIGHT_BUMP,
            HeightMode::Parallax => FEATURE_HEIGHT,
            HeightMode::Displacement => FEATURE_DISPLACE,
        };
    }
    if material.blend_mode == BlendMode::Masked {
        features |= FEATURE_ALPHACLIP;
    }

    let (thin_reflection, thin_absorption, thin_transmission, coverage, coverage_hash) =
        if let Some(thin) = material.thin_sheet {
            features |= FEATURE_THIN_SHEET;
            pin(&material.coverage_texture, &mut coverage_index);
            let salt = thin.coverage_hash_salt;
            (
                Vec4::new(
                    thin.front_albedo_response,
                    thin.back_albedo_response,
                    thin.roughness,
                    thin.thickness,
                ),
                thin.absorption.extend(thin.energy_limit),
                thin.transmission.extend(0.0),
                UVec4::new(
                    coverage_index,
                    thin.coverage_source as u32,
                    thin.coverage_classification as u32,
                    thin.normal_mode as u32,
                ),
                UVec4::new(
                    salt as u32,
                    (salt >> 32) as u32,
                    thin.coverage_source_extent[0],
                    thin.coverage_source_extent[1],
                ),
            )
        } else {
            (Vec4::ZERO, Vec4::ZERO, Vec4::ZERO, UVec4::ZERO, UVec4::ZERO)
        };

    let (
        aggregate0,
        aggregate_albedo,
        aggregate_transmission,
        aggregate_normal0,
        aggregate_normal1,
    ) = material.thin_sheet.map_or(
        (Vec4::ZERO, Vec4::ZERO, Vec4::ZERO, Vec4::ZERO, Vec4::ZERO),
        |thin| {
            let moments = thin.aggregate;
            (
                Vec4::new(
                    moments.occupancy,
                    moments.roughness_mean,
                    moments.thickness_mean,
                    0.0,
                ),
                moments.albedo_mean.extend(0.0),
                moments.transmission_mean.extend(0.0),
                Vec4::from_array([
                    moments.normal_second_moments[0],
                    moments.normal_second_moments[1],
                    moments.normal_second_moments[2],
                    moments.normal_second_moments[3],
                ]),
                Vec4::new(
                    moments.normal_second_moments[4],
                    moments.normal_second_moments[5],
                    0.0,
                    0.0,
                ),
            )
        },
    );

    let params = MaterialParamsData {
        base_color: material.base_color,
        pbr: Vec4::new(
            material.metallic,
            material.roughness,
            material.normal_strength,
            material.alpha_cutoff,
        ),
        emissive: (material.emissive * material.emissive_strength).extend(material.height_scale),
        uv: Vec4::new(
            material.uv_tiling.x,
            material.uv_tiling.y,
            material.uv_offset.x,
            material.uv_offset.y,
        ),
        tex0: UVec4::new(albedo_index, mr_index, normal_index, emissive_index),
        tex1: UVec4::new(
            height_index,
            occlusion_index,
            coverage_temporal_phase,
            features,
        ),
        thin_reflection,
        thin_absorption,
        thin_transmission,
        coverage,
        coverage_hash,
        aggregate0,
        aggregate_albedo,
        aggregate_transmission,
        aggregate_normal0,
        aggregate_normal1,
    };
    (params, albedo_index, mr_index)
}

/// `NORMAL` material feature bit (a normal map is present).
const FEATURE_NORMAL: u32 = 1;
/// `EMISSIVE_TEX` feature bit.
const FEATURE_EMISSIVE_TEX: u32 = 2;
/// `OCCLUSION` feature bit.
const FEATURE_OCCLUSION: u32 = 4;
/// `HEIGHT` (parallax) feature bit.
const FEATURE_HEIGHT: u32 = 8;
/// `ALPHACLIP` (masked) feature bit.
const FEATURE_ALPHACLIP: u32 = 16;
/// `DISPLACE` feature bit: the height map drives real per-vertex displacement (the compute
/// pre-pass moved the geometry) plus a fragment shading-normal bump.
const FEATURE_DISPLACE: u32 = 32;
/// `HEIGHT_BUMP` feature bit: the height map perturbs only the fragment shading normal — no
/// parallax march, no geometry — the safe, artifact-free baseline.
const FEATURE_HEIGHT_BUMP: u32 = 64;
/// `THIN_SHEET` feature bit: the material uses the complete two-sided foliage response.
const FEATURE_THIN_SHEET: u32 = 128;

/// Grows `current` (an element capacity) to the next power of two that holds `count`,
/// seeding from `initial` when empty and never shrinking.
fn grow_capacity(current: u32, initial: u32, count: u32) -> u32 {
    let mut capacity = if current == 0 { initial } else { current };
    while capacity < count {
        capacity *= 2;
    }
    capacity
}

/// Copies `bytes` into the head of a mapped storage buffer. The buffer is host-visible
/// + persistently mapped, sized `>= bytes.len()` by the grow path.
fn upload_into(buffer: &mut Buffer, bytes: &[u8]) {
    let dst = buffer
        .mapped_bytes()
        .expect("instance/material buffer is mapped");
    dst[..bytes.len()].copy_from_slice(bytes);
}

/// Allocates a host-visible, persistently-mapped storage buffer of `size` bytes — the
/// backing for the per-frame instance / material SSBOs.
fn make_mapped_storage_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::Auto,
        flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
            | vk_mem::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    Buffer::new(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        &alloc_info,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::resources::BindlessFreeList;
    use crate::skinning::Skinning;
    use crate::upload::Uploader;
    use saffron_geometry::glam::{Mat4, Vec2, Vec3};
    use saffron_geometry::{Mesh, Submesh, Vertex, VertexSkin};
    use std::sync::Mutex;

    /// A device + descriptors + instancing + skinning + uploader fixture, or `None`
    /// when no Vulkan ICD is available (the test skips cleanly).
    #[allow(clippy::type_complexity)]
    fn fixture_or_skip() -> Option<(Device, Descriptors, Instancing, Skinning, Uploader)> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let instancing = Instancing::new(&device, &descriptors).expect("Instancing::new");
        let skinning = Skinning::new(&device).expect("Skinning::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        Some((device, descriptors, instancing, skinning, uploader))
    }

    /// A skinned work item with a palette produces a skin dispatch (current + prev)
    /// through the gather + wiring, and the deformed buffers are allocated; a frame
    /// with no skinned work arms nothing. The skin pass is armed only when
    /// `skin_dispatches` is non-empty.
    #[test]
    fn skin_dispatch_appears_only_for_skinned_work() {
        let Some((device, descriptors, mut instancing, mut skinning, uploader)) = fixture_or_skip()
        else {
            return;
        };
        let mesh = skinned_triangle(&descriptors, &uploader);
        let palette = [Mat4::IDENTITY];
        let params = TessGatherParams {
            rt_skinned: false,
            factor_cap: crate::tessellation::TESS_DEFAULT_FACTOR_CAP,
            min_factor: crate::tessellation::TESS_DEFAULT_MIN_FACTOR,
            edge_length_target: crate::tessellation::TESS_DEFAULT_EDGE_LENGTH_TARGET,
        };

        // No skinned work: wiring an empty gather arms nothing.
        let mut static_list = FrameDeformation::default();
        instancing
            .wire_gathered_deformations(
                &mut skinning,
                0,
                DeformationGather::default(),
                &palette,
                palette.to_vec(),
                &mut static_list,
            )
            .expect("wire static");
        assert!(
            static_list.skin_dispatches.is_empty(),
            "no skinned work arms no skin dispatch"
        );

        // A skinned work item emits one dispatch (current + prev), and the deformed
        // buffers are now allocated.
        let work = DeformationWork {
            mesh: Arc::clone(&mesh),
            entity: 1,
            skinned: true,
            joint_offset: 0,
            joint_count: 1,
            morph_weights: Vec::new(),
            model: Mat4::IDENTITY,
            displace: None,
            instance_slot: 0,
        };
        let mut gather = DeformationGather::default();
        let mut prev = palette.to_vec();
        gather_instance_deformation(
            &mut gather,
            &mut skinning,
            &work,
            &palette,
            &mut prev,
            params,
        );
        let mut list = FrameDeformation::default();
        instancing
            .wire_gathered_deformations(&mut skinning, 1, gather, &palette, prev, &mut list)
            .expect("wire skinned");
        assert_eq!(
            list.skin_dispatches.len(),
            1,
            "one dispatch per skinned instance"
        );
        assert_eq!(
            list.prev_skin_dispatches.len(),
            1,
            "a parallel prev dispatch"
        );
        assert_eq!(list.skin_dispatches[0].vertex_count, 3);
        assert_eq!(list.skin_dispatches[0].deformed_offset, 0);
        assert_ne!(
            list.skin_dispatches[0].set,
            vk::DescriptorSet::null(),
            "the dispatch set is wired"
        );
        assert!(
            skinning.deformed_buffer(1).is_some() && skinning.prev_deformed_buffer(1).is_some(),
            "both deformed buffers are allocated for the skinned frame"
        );

        drop(static_list);
        drop(list);
        drop(work);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(descriptors);
        drop(device);
    }

    /// The grow policy seeds from `initial` when empty, doubles to cover `count`, and
    /// never shrinks below the existing capacity.
    #[test]
    fn grow_capacity_doubles_and_never_shrinks() {
        assert_eq!(grow_capacity(0, 256, 1), 256, "empty seeds the initial");
        assert_eq!(grow_capacity(0, 256, 300), 512, "doubles past the seed");
        assert_eq!(grow_capacity(0, 64, 200), 256);
        assert_eq!(grow_capacity(512, 256, 100), 512, "never shrinks");
        assert_eq!(grow_capacity(256, 256, 256), 256, "exact fit holds");
    }

    /// A single-submesh triangle with a parallel skin stream (one joint, full weight),
    /// the geometry the skinned-path tests deform.
    fn skinned_triangle(descriptors: &Descriptors, uploader: &Uploader) -> Arc<crate::GpuMesh> {
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
        let skin = vec![
            VertexSkin {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            3
        ];
        let hierarchy = crate::upload::hierarchy_for_upload(&mesh, &skin).expect("cook hierarchy");
        uploader
            .upload_mesh(
                descriptors,
                &mesh,
                &hierarchy,
                &skin,
                None,
                crate::SdfSource::None,
            )
            .expect("upload skinned")
    }
}
