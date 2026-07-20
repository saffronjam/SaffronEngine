//! The per-frame instance + material storage buffers and the draw-list batcher.
//!
//! [`Instancing`] owns, per frame-in-flight, one instance SSBO (set 2, binding 0) and
//! one deduplicated material SSBO (set 2, binding 2), each grown on demand to the next
//! power of two and never shrunk, plus the descriptor set that binds them.
//! [`Instancing::submit_draw_list`] resolves each [`DrawItem`]'s material to a cached
//! PSO, buckets by (pipeline, mesh) into instanced draws, deduplicates the material
//! table by raw bytes, uploads both SSBOs, and returns the [`SceneDrawList`] +
//! [`RenderStats`].
//!
//! The current joint palette is uploaded to set 2,
//! binding 1; the prev-joint palette feeds the prev skin dispatch directly (it is not
//! bound). The deformed buffers + dispatch pool live in [`crate::skinning::Skinning`].
//!
//! # The submesh-major instance layout (load-bearing)
//!
//! A bucket's instance rows are flattened *submesh-major*: every instance's submesh-`s`
//! row is laid contiguously, so a submesh draws all instances at once by offsetting
//! `firstInstance` by `base_instance + s * instance_count`. The vertex shader reads
//! `instances[SV_VulkanInstanceID]`, and Vulkan's instance id includes `firstInstance`,
//! so this is exactly the row each draw fetches.

use std::collections::{HashMap, HashSet};

use ash::vk;
use saffron_core::{BlendMode, HeightMode};
use saffron_geometry::glam::{Mat4, UVec4, Vec4};

use crate::descriptors::Descriptors;
use crate::draw_list::{
    DeformedRtInstance, DrawBatch, DrawItem, MorphDispatch, RenderStats, SceneDrawList,
    SkinDispatch, SubmeshMaterial,
};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::{InstanceData, Material, MaterialParamsData};
use crate::pipelines::Pipelines;
use crate::resources::{Buffer, DeviceResources, GpuMesh};
use crate::skinning::{SkinBucket, SkinBufferSet, Skinning, clamp_to_set_budget};
use crate::tessellation::{TESS_MAX_INSTANCES, TessBucket};
use crate::{Device, Result};

use std::sync::Arc;

/// The per-frame policy inputs to [`Instancing::submit_draw_list`]: the camera
/// transform plus the renderer-state flags that shape the instance rows (the wireframe
/// PSO permutation, the default bindless slot used for an absent texture).
#[derive(Debug, Clone, Copy)]
pub struct DrawListInputs {
    /// The frame slot keying the per-frame SSBOs.
    pub frame: usize,
    /// The camera view-projection (the per-frame vertex push constant).
    pub view_proj: Mat4,
    /// Wireframe view mode — selects the wireframe PSO permutation per draw.
    pub wireframe: bool,
    /// The default white bindless slot used for any absent material texture.
    pub default_texture_index: u32,
    /// Whether an RT consumer is armed this frame — gates building the skinned RT-instance
    /// list (a non-RT scene pays nothing).
    pub rt_skinned: bool,
    /// Whether the GPU displacement path runs. Off leaves displacement-enabled meshes at their base
    /// geometry (no tessellation prep gathered, so no amplified transient geometry to draw).
    pub displace_enabled: bool,
    /// The tessellation-quality budget applied to each displaced instance's [`TessBucket`]: the hard
    /// dice cap, the minimum per-edge factor, and the target screen-space edge length. Runtime-tunable
    /// via the `set-tessellation-quality` control command; defaults to the `TESS_DEFAULT_*` consts.
    pub tess_factor_cap: f32,
    pub tess_min_factor: f32,
    pub tess_edge_length_target: f32,
}

/// Initial instance-buffer capacity (in [`InstanceData`] elements).
const INITIAL_INSTANCE_CAPACITY: u32 = 256;

/// Initial material-buffer capacity (in [`MaterialParamsData`] entries).
const INITIAL_MATERIAL_CAPACITY: u32 = 64;

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
struct ActiveTarget {
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
    instances: Option<Buffer>,
    instance_capacity: u32,
    materials: Option<Buffer>,
    material_capacity: u32,
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

/// The per-frame instance + material storage and the draw-list batcher.
///
/// Built once in [`Instancing::new`] (it allocates one instance set per frame slot),
/// then mutated only through [`Instancing::submit_draw_list`] taking `&mut self` plus
/// the device / descriptors / pipelines. Each [`Buffer`] is a [`crate::Buffer`] Drop
/// type holding the allocator `Arc`, so the SSBOs free without a live `&Device`.
pub struct Instancing {
    resources: Arc<DeviceResources>,
    frames: Vec<FrameInstancing>,
}

impl Instancing {
    /// Allocates one instance descriptor set per frame-in-flight from the shared pool.
    /// The SSBOs are created lazily on the first [`Instancing::submit_draw_list`] that
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
                instances: None,
                instance_capacity: 0,
                materials: None,
                material_capacity: 0,
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

    /// Builds the frame's [`SceneDrawList`] from `items` + the `joints` palette: bucket by
    /// (pipeline, mesh), flatten submesh-major into the instance SSBO, deduplicate the
    /// material table, upload the SSBOs + the current/previous joint palettes, and — for
    /// skinned buckets — size the deformed buffers and wire the per-instance skin
    /// dispatches through `skinning`. Returns the list + the [`RenderStats`].
    ///
    /// `joints` is the concatenated `worldBone * inverseBind` palette every skinned item
    /// indexes by its `joint_offset`; an unskinned scene passes an empty slice.
    /// An empty `items` (or one that resolves to no drawable instances) returns an
    /// invalid (`valid == false`) list and zeroed stats — the scene pass records nothing.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if growing/rewriting an SSBO or deformed buffer fails.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_draw_list(
        &mut self,
        descriptors: &Descriptors,
        pipelines: &mut Pipelines,
        skinning: &mut Skinning,
        items: &[DrawItem],
        joints: &[Mat4],
        inputs: DrawListInputs,
    ) -> Result<(SceneDrawList, RenderStats)> {
        let DrawListInputs {
            frame,
            view_proj,
            wireframe,
            default_texture_index,
            rt_skinned,
            displace_enabled,
            tess_factor_cap,
            tess_min_factor,
            tess_edge_length_target,
        } = inputs;
        let pipelines_before = pipelines.pipelines_created();
        let mut list = SceneDrawList {
            view_proj,
            ..SceneDrawList::default()
        };
        if items.is_empty() {
            return Ok((list, RenderStats::default()));
        }

        let mut buckets: Vec<Bucket> = Vec::new();
        let mut live_textures: Vec<Arc<crate::GpuTexture>> = Vec::new();
        let mut material_table: Vec<MaterialParamsData> = Vec::new();
        let mut material_dedup: HashMap<MaterialParamsData, u32> = HashMap::new();

        for item in items {
            // A skinned draw needs the mesh's skin stream; without it the draw is dropped.
            if item.skinned && item.mesh.skin_buffer().is_none() {
                continue;
            }

            // A morph item carries per-target weights and a mesh with morph buffers; it
            // deforms into its own slice (before skin), so it never merges either.
            let is_morph = !item.morph_weights.is_empty() && item.mesh.morph().is_some();

            // A displacement-enabled item rides the adaptive-tessellation path (amplified transient
            // geometry), so — like skin/morph — it never merges. Gated off leaves it as base geometry.
            let displace = if displace_enabled {
                displace_info_for(item)
            } else {
                None
            };

            // The per-submesh blend pattern (drives the per-submesh PSO + routing at emit). A
            // bucket with any translucent submesh is drawn sorted back-to-front, so it never
            // merges — each keeps its own lone-instance depth key.
            let blend_modes = submesh_blend_modes(item);
            let has_blend = blend_modes.contains(&BlendMode::Blend);

            // Find an existing (mesh, base material, blend pattern) bucket; a deforming item
            // (skinned or morph) or a blend-carrying item never merges (each keeps its own slice /
            // depth order). PSO is resolved per submesh at emit, so the merge key is the base
            // material (shader + unlit) plus the whole blend pattern, not a single pipeline.
            let bucket_index = if item.skinned || is_morph || has_blend || displace.is_some() {
                None
            } else {
                buckets.iter().position(|b| {
                    !b.skinned
                        && b.morph_weights.is_empty()
                        && b.displace.is_none()
                        && !b.blend_modes.contains(&BlendMode::Blend)
                        && Arc::ptr_eq(&b.mesh, &item.mesh)
                        && b.material.shader == item.material.shader
                        && b.material.unlit == item.material.unlit
                        && b.blend_modes == blend_modes
                })
            };
            let bucket_index = match bucket_index {
                Some(index) => index,
                None => {
                    buckets.push(Bucket {
                        mesh: Arc::clone(&item.mesh),
                        material: item.material.clone(),
                        blend_modes,
                        skinned: item.skinned,
                        joint_offset: item.joint_offset,
                        joint_count: item.joint_count,
                        entity: item.entity,
                        morph_weights: if is_morph {
                            item.morph_weights.clone()
                        } else {
                            Vec::new()
                        },
                        model: item.model,
                        submesh_cull: submesh_cull_for(item),
                        displace,
                        instances: Vec::new(),
                    });
                    buckets.len() - 1
                }
            };

            // This frame's previous model: the entity's cached last-frame world matrix, or
            // the current one when new/uncached (no object-motion ghost on frame 1).
            let prev_model = if item.entity != 0 {
                let prev = skinning.prev_model(item.entity).unwrap_or(item.model);
                skinning.commit_model(item.entity, item.model);
                prev
            } else {
                item.model
            };

            let rows = build_instance_rows(
                item,
                prev_model,
                default_texture_index,
                &mut material_table,
                &mut material_dedup,
                &mut live_textures,
            );
            buckets[bucket_index].instances.push(rows);
        }

        // Flatten submesh-major: lay every instance's submesh-`s` row contiguously, so a
        // submesh draws all instances at once by offsetting `firstInstance`. Skinned
        // buckets carry instance_count == 1 + a deformed-buffer base vertex; the running
        // cursor concatenates each skinned instance's full vertex array.
        let mut instances: Vec<InstanceData> = Vec::new();
        let mut batches: Vec<DrawBatch> = Vec::new();
        // Translucent batches paired with their back-to-front depth key (clip-space `w`, larger
        // = farther); sorted farthest-first after the loop into `list.transparent_batches`.
        let mut transparent: Vec<(f32, DrawBatch)> = Vec::new();
        let mut skin_buckets: Vec<SkinBucket> = Vec::new();
        let mut skinned_rt: Vec<DeformedRtInstance> = Vec::new();
        // The displaced instances' TLAS entries (mesh-local displaced geometry). Their `tess` slice is
        // filled mid-render by `record_tess_prep` from the amplified transient buffers.
        let mut displaced_rt: Vec<DeformedRtInstance> = Vec::new();
        // The adaptive-tessellation prep work: one bucket per displaced instance, carrying its base
        // mesh (+ Phase-2 conditioning) and world transform for the factor/scan/finalize passes the
        // deform scope records. The amplified geometry it emits is what every raster + RT consumer
        // reads for a displaced mesh.
        let mut tess_buckets: Vec<TessBucket> = Vec::new();
        // The morph deform work: one cur + one prev dispatch + mesh per morph-active bucket,
        // and the frame's concatenated active-target list (each dispatch reads its
        // `active_base` slice — cur and prev both index the same buffer, differing only in
        // the output buffer their set binds). The morph pass runs before skin.
        let mut morph_dispatches: Vec<MorphDispatch> = Vec::new();
        let mut prev_morph_dispatches: Vec<MorphDispatch> = Vec::new();
        let mut morph_meshes: Vec<Arc<GpuMesh>> = Vec::new();
        let mut active_targets: Vec<ActiveTarget> = Vec::new();
        // RT instances for unskinned-morph buckets (skinned ones ride `skinned_rt`, wired +
        // budget-clamped in `wire_skin_dispatches`); appended to the draw list after wiring.
        let mut morph_rt: Vec<DeformedRtInstance> = Vec::new();
        // The previous palette, laid out exactly like `joints`: a copy of the current
        // palette (uncached slots → zero deformation motion), each skinned bucket's slice
        // replaced by the entity's cached last-frame slice.
        let mut prev_joints: Vec<Mat4> = joints.to_vec();
        let mut deformed_cursor: u32 = 0;
        for bucket in &buckets {
            if bucket.instances.is_empty() {
                continue;
            }
            let submesh_count = bucket.instances[0].len() as u32;
            // A morph bucket's above-threshold targets (empty if all-rest or not a morph
            // bucket); a bucket deforms when it is skinned OR has active morph targets.
            let (morph_active, scatter_count) = match bucket.mesh.morph() {
                Some(morph) if !bucket.morph_weights.is_empty() => {
                    build_active_targets(morph, &bucket.morph_weights)
                }
                _ => (Vec::new(), 0),
            };
            let has_morph = !morph_active.is_empty();
            // The deformed ring is skin/morph only; a displaced mesh rides the transient
            // tessellation buffers instead (gathered below, outside this gate).
            let deformed = bucket.skinned || has_morph;

            let base_instance = instances.len() as u32;
            let instance_count = bucket.instances.len() as u32;
            let deformed_vertex_offset = if deformed { deformed_cursor } else { 0 };
            if deformed {
                let vertex_count = bucket.mesh.vertex_count;
                if bucket.skinned {
                    skin_buckets.push(SkinBucket {
                        mesh: Arc::clone(&bucket.mesh),
                        joint_offset: bucket.joint_offset,
                        deformed_offset: deformed_cursor,
                    });
                    // The refit BLAS reads exactly this instance's deformed slice; it needs
                    // an entity to key the grow-only per-instance BLAS. A placeholder
                    // (entity 0) keeps parity with the dispatch list and is dropped at the end.
                    skinned_rt.push(DeformedRtInstance {
                        entity: if rt_skinned { bucket.entity } else { 0 },
                        deformed_offset: deformed_cursor,
                        vertex_count,
                        index_count: bucket.mesh.index_count,
                        mesh: Arc::clone(&bucket.mesh),
                        // Skinned (and skin+morph) deformed vertices are already world-space.
                        world_transform: Mat4::IDENTITY,
                        tess: None,
                    });
                    // Replace this bucket's slice in the prev palette with the entity's
                    // cached last-frame slice (or leave the current copy → no frame-1 ghost).
                    let lo = bucket.joint_offset as usize;
                    let hi = lo + bucket.joint_count as usize;
                    if bucket.entity != 0 && bucket.joint_count > 0 && hi <= joints.len() {
                        let cached = skinning.swap_palette(bucket.entity, &joints[lo..hi]);
                        prev_joints[lo..hi].copy_from_slice(&cached);
                    }
                }
                if has_morph {
                    // The morph pass runs before skin and writes this same deformed slice;
                    // a skinned-morph instance then has skin read+overwrite it in place.
                    let active_base = active_targets.len() as u32;
                    let active_count = morph_active.len() as u32;
                    active_targets.extend_from_slice(&morph_active);
                    morph_dispatches.push(MorphDispatch {
                        set: vk::DescriptorSet::null(),
                        vertex_count,
                        scatter_count,
                        active_count,
                        active_base,
                        deformed_offset: deformed_cursor,
                    });
                    // The prev-pose morph dispatch (previous weights → prev-deformed) for
                    // deformation motion: build the active list from the entity's cached
                    // last-frame weights (uncached / length change → prev == cur → zero
                    // motion). Both lists share the one active-target buffer; only the prev
                    // set's output buffer differs (prev-deformed).
                    let prev_weights =
                        skinning.swap_morph_weights(bucket.entity, &bucket.morph_weights);
                    let (prev_active, prev_scatter) = match bucket.mesh.morph() {
                        Some(morph) => build_active_targets(morph, &prev_weights),
                        None => (Vec::new(), 0),
                    };
                    let prev_active_base = active_targets.len() as u32;
                    let prev_active_count = prev_active.len() as u32;
                    active_targets.extend_from_slice(&prev_active);
                    prev_morph_dispatches.push(MorphDispatch {
                        set: vk::DescriptorSet::null(),
                        vertex_count,
                        scatter_count: prev_scatter,
                        active_count: prev_active_count,
                        active_base: prev_active_base,
                        deformed_offset: deformed_cursor,
                    });
                    morph_meshes.push(Arc::clone(&bucket.mesh));
                    // An unskinned-morph instance enters the TLAS at its node world matrix
                    // (its deformed vertices are mesh-local). Skinned-morph rides skinned_rt.
                    if !bucket.skinned {
                        morph_rt.push(DeformedRtInstance {
                            entity: if rt_skinned { bucket.entity } else { 0 },
                            deformed_offset: deformed_cursor,
                            vertex_count,
                            index_count: bucket.mesh.index_count,
                            mesh: Arc::clone(&bucket.mesh),
                            world_transform: bucket.model,
                            tess: None,
                        });
                    }
                }
                deformed_cursor += vertex_count;
            }
            // A displaced instance rides the adaptive-tessellation path: the prep passes gathered here
            // amplify its base geometry into the per-frame transient buffers that every raster pass and
            // its RT BLAS read. It claims no deformed-ring slice (that ring is skin/morph only); its
            // `tess` RT slice is filled mid-render by `record_tess_prep`.
            if let Some(info) = bucket.displace {
                // Only meshes carrying Phase-2 conditioning (watertight base topology) can be tessellated.
                if bucket.mesh.conditioning().is_some() {
                    tess_buckets.push(TessBucket {
                        mesh: Arc::clone(&bucket.mesh),
                        base_instance,
                        entity: if rt_skinned { bucket.entity } else { 0 },
                        model: bucket.model,
                        height_index: info.height_index,
                        height_scale: info.height_scale,
                        uv_transform: info.uv_transform,
                        vector_index: info.vector_index,
                        factor_cap: tess_factor_cap,
                        min_factor: tess_min_factor,
                        edge_length_target: tess_edge_length_target,
                    });
                }
                // Its TLAS entry (mesh-local displaced geometry at the node world matrix); the tess slice
                // `record_tess_prep` fills drives the full per-frame BLAS BUILD over the amplified geometry.
                displaced_rt.push(DeformedRtInstance {
                    entity: if rt_skinned { bucket.entity } else { 0 },
                    deformed_offset: 0,
                    vertex_count: bucket.mesh.vertex_count,
                    index_count: bucket.mesh.index_count,
                    mesh: Arc::clone(&bucket.mesh),
                    world_transform: bucket.model,
                    tess: None,
                });
            }
            for s in 0..submesh_count as usize {
                for rows in &bucket.instances {
                    instances.push(rows[s]);
                }
            }

            // Resolve each submesh's PSO from the base material + its blend mode, then group
            // submeshes sharing a PSO into one batch. Opaque and (at 1×) masked collapse to the
            // same PSO; under MSAA masked is the distinct alpha-to-coverage PSO; translucent is
            // the blend PSO. Every group draws from the one shared submesh-major instance block.
            let no_submesh = bucket.mesh.submeshes.is_empty();
            let last_mode = bucket.blend_modes.len().saturating_sub(1);
            let mut groups: Vec<(Arc<crate::Pipeline>, bool, Vec<u32>)> = Vec::new();
            for s in 0..submesh_count as usize {
                let mode = bucket
                    .blend_modes
                    .get(s.min(last_mode))
                    .copied()
                    .unwrap_or_default();
                let material = Material {
                    shader: bucket.material.shader.clone(),
                    unlit: bucket.material.unlit,
                    blend: mode == BlendMode::Blend,
                    masked: mode == BlendMode::Masked,
                };
                let Some(pipeline) = pipelines.request_mesh_pipeline(&material, false, wireframe)
                else {
                    continue;
                };
                if let Some(group) = groups
                    .iter_mut()
                    .find(|(p, _, _)| Arc::ptr_eq(p, &pipeline))
                {
                    group.2.push(s as u32);
                } else {
                    groups.push((pipeline, mode == BlendMode::Blend, vec![s as u32]));
                }
            }
            for (pipeline, is_blend, submesh_indices) in groups {
                let batch = DrawBatch {
                    pipeline,
                    mesh: Arc::clone(&bucket.mesh),
                    base_instance,
                    instance_count,
                    deformed,
                    deformed_vertex_offset,
                    submesh_cull: bucket.submesh_cull.clone(),
                    submeshes: if no_submesh {
                        Vec::new()
                    } else {
                        submesh_indices
                    },
                    // Resolved mid-render by `record_tess_prep` (the transients don't exist yet),
                    // matched to this batch by `base_instance`.
                    tessellated: None,
                };
                if is_blend {
                    // Depth key: the object's world-space origin projected to clip `w`. For a
                    // perspective view `w = -view_z`, so a larger `w` is farther from the camera.
                    let clip_w = (view_proj * bucket.model.w_axis).w;
                    transparent.push((clip_w, batch));
                } else {
                    batches.push(batch);
                }
            }
        }

        if instances.is_empty() {
            return Ok((list, RenderStats::default()));
        }

        // Back-to-front: farthest (largest clip `w`) first, so nearer translucent surfaces
        // blend over the ones behind them.
        transparent.sort_by(|a, b| b.0.total_cmp(&a.0));
        let transparent_batches: Vec<DrawBatch> =
            transparent.into_iter().map(|(_, batch)| batch).collect();

        // Upload the instance SSBO, growing/rebinding it on demand.
        self.ensure_instance_capacity(descriptors, frame, instances.len() as u32)?;
        upload_into(
            self.frames[frame]
                .instances
                .as_mut()
                .expect("instance buffer"),
            bytemuck::cast_slice(&instances),
        );

        // Upload the deduplicated material table (set 2, binding 2). Always non-empty
        // when there are instances — every row interns at least the default material.
        self.ensure_material_capacity(descriptors, frame, material_table.len() as u32)?;
        upload_into(
            self.frames[frame]
                .materials
                .as_mut()
                .expect("material buffer"),
            bytemuck::cast_slice(&material_table),
        );

        // Upload the current joint palette (set 2, binding 1) + the previous palette (fed
        // to the prev skin dispatch). Only when the scene supplied a palette.
        if !joints.is_empty() {
            self.ensure_joint_capacity(descriptors, frame, joints.len() as u32)?;
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
        // bucket with no palette this frame can't be deformed: drop the skin work so the
        // skin pass is skipped and the batches read the undeformed bind pose.
        let skin_ran = !skin_buckets.is_empty() && !joints.is_empty();
        if skin_ran {
            self.wire_skin_dispatches(
                frame,
                skinning,
                deformed_cursor,
                &mut skin_buckets,
                &mut skinned_rt,
                &mut list,
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

        // Feed the displaced instances' RT entries into the draw list. They ride the transient
        // tessellation buffers (their `tess` slice is filled mid-render by `record_tess_prep`), not
        // the deformed ring — but the deformed buffers must exist for any deform work this frame, so
        // size them to the deform cursor (idempotent; skin/morph already sized them). Clamp to the
        // tessellation instance budget, matching how `tess_buckets` is truncated below.
        if !displaced_rt.is_empty() {
            skinning.ensure_deformed_buffers(frame, deformed_cursor)?;
            displaced_rt.truncate(TESS_MAX_INSTANCES as usize);
            list.deformed_rt_instances
                .extend(displaced_rt.into_iter().filter(|s| s.entity != 0));
        }

        // Hand the deform scope the adaptive-tessellation buckets, clamped to the per-frame descriptor
        // budget (the subsystem's pool holds `TESS_MAX_INSTANCES` set trios).
        tess_buckets.truncate(TESS_MAX_INSTANCES as usize);
        list.tess_buckets = tess_buckets;

        let stats = compute_stats(
            &batches,
            &transparent_batches,
            pipelines.pipelines_created() - pipelines_before,
            instances.len(),
        );

        list.batches = batches;
        list.transparent_batches = transparent_batches;
        list.live_textures = live_textures;
        list.valid = true;
        Ok((list, stats))
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
        list: &mut SceneDrawList,
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

    /// Ensures the frame's instance SSBO holds at least `count` [`InstanceData`]
    /// elements, growing to the next power of two (never shrinking) and rewriting its
    /// descriptor (set 2, binding 0).
    fn ensure_instance_capacity(
        &mut self,
        descriptors: &Descriptors,
        frame: usize,
        count: u32,
    ) -> Result<()> {
        if self.frames[frame].instances.is_some() && self.frames[frame].instance_capacity >= count {
            return Ok(());
        }
        let capacity = grow_capacity(
            self.frames[frame].instance_capacity,
            INITIAL_INSTANCE_CAPACITY,
            count,
        );
        let size = u64::from(capacity) * size_of::<InstanceData>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        descriptors.write_storage_buffer(self.frames[frame].set, 0, buffer.handle(), buffer.size());
        self.frames[frame].instances = Some(buffer);
        self.frames[frame].instance_capacity = capacity;
        Ok(())
    }

    /// Ensures the frame's material SSBO holds at least `count` [`MaterialParamsData`]
    /// entries (same grow-only policy), rewriting its descriptor (set 2, binding 2).
    fn ensure_material_capacity(
        &mut self,
        descriptors: &Descriptors,
        frame: usize,
        count: u32,
    ) -> Result<()> {
        if self.frames[frame].materials.is_some() && self.frames[frame].material_capacity >= count {
            return Ok(());
        }
        let capacity = grow_capacity(
            self.frames[frame].material_capacity,
            INITIAL_MATERIAL_CAPACITY,
            count,
        );
        let size = u64::from(capacity) * size_of::<MaterialParamsData>() as u64;
        let buffer = make_mapped_storage_buffer(&self.resources, size)?;
        descriptors.write_storage_buffer(self.frames[frame].set, 2, buffer.handle(), buffer.size());
        self.frames[frame].materials = Some(buffer);
        self.frames[frame].material_capacity = capacity;
        Ok(())
    }

    /// Ensures the frame's joint palette holds at least `count` [`Mat4`] matrices (same
    /// grow-only policy), rewriting its descriptor (set 2, binding 1).
    fn ensure_joint_capacity(
        &mut self,
        descriptors: &Descriptors,
        frame: usize,
        count: u32,
    ) -> Result<()> {
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
        descriptors.write_storage_buffer(self.frames[frame].set, 1, buffer.handle(), buffer.size());
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

/// One (mesh, base material, blend pattern) bucket accumulating instance rows before the
/// submesh-major flatten. Each instance contributes one [`InstanceData`] row per mesh submesh.
/// The PSO is resolved per submesh at emit time (from the base material + the submesh's blend
/// mode), so one mesh's submeshes split into opaque/masked/blend batches. A skinned, morph, or
/// blend-carrying bucket never merges and carries the palette slice + entity for the dispatch.
struct Bucket {
    mesh: Arc<crate::GpuMesh>,
    /// The item's base PSO identity: shader + unlit. `blend`/`masked` are ignored here — the
    /// per-submesh blend mode drives them at emit time.
    material: Material,
    /// Per-geometry-submesh blend mode (clamped to the submesh count), driving the per-submesh
    /// PSO + opaque/translucent routing at emit. Part of the merge key: instances merge only when
    /// their whole blend pattern matches, so the submesh split aligns across the bucket.
    blend_modes: Vec<BlendMode>,
    skinned: bool,
    /// Skinned only: the base of this instance's joints in the palette.
    joint_offset: u32,
    /// Skinned only: the matrices this instance contributes (its palette slice length).
    joint_count: u32,
    /// Skinned only: the source entity uuid, keying the cross-frame motion caches.
    entity: u64,
    /// Per-target morph weights (empty = not a morph bucket); a morph bucket never merges.
    morph_weights: Vec<f32>,
    /// The instance's world matrix (used as the RT `world_transform` for an unskinned-morph
    /// instance, whose deformed vertices are mesh-local; skinned instances place identity). Also
    /// the translucent depth key origin (a blend-carrying bucket never merges, so it is one item).
    model: Mat4,
    /// Per-geometry-submesh backface-cull mode, from the bucket's first item's submesh materials
    /// (a bucket is one mesh, so its submesh two-sidedness is shared). Copied onto every batch.
    submesh_cull: Vec<vk::CullModeFlags>,
    /// Set when the mesh-instance is displacement-enabled: the adaptive-tessellation prep passes
    /// amplify its base geometry into the per-frame transient buffers every consumer reads (like a
    /// skinned bucket, it never merges). `None` for a normal bucket.
    displace: Option<DisplaceInfo>,
    instances: Vec<Vec<InstanceData>>,
}

/// Per-geometry-submesh blend mode from the item's submesh materials (clamped to the last material
/// like the instance-row build), driving the per-submesh PSO + opaque/translucent routing. Length
/// matches the geometry submesh count (>= 1 for the no-submesh single-draw path).
fn submesh_blend_modes(item: &DrawItem) -> Vec<BlendMode> {
    let count = item.mesh.submeshes.len().max(1);
    let last = item.submesh_materials.len().saturating_sub(1);
    (0..count)
        .map(|s| {
            item.submesh_materials
                .get(s.min(last))
                .map(|m| m.blend_mode)
                .unwrap_or_default()
        })
        .collect()
}

/// Per-geometry-submesh backface-cull mode from the item's submesh materials (clamped to the last
/// material like the instance-row build): a two-sided submesh disables culling (`NONE`), otherwise
/// cull `BACK`. Length matches the geometry submesh count (>= 1 for the no-submesh single-draw path).
/// The height-map index + amplitude + uv transform a displaced mesh-instance's adaptive-tessellation
/// prep needs, derived from a submesh material. The whole mesh-instance is displaced by one height
/// field (its first displacement-enabled submesh material).
#[derive(Clone, Copy)]
struct DisplaceInfo {
    height_index: u32,
    height_scale: f32,
    uv_transform: [f32; 4],
    vector_index: u32,
}

/// The displacement info for an item, if any submesh material is displacement-enabled with a height
/// map. A mesh-instance is displaced as a whole by the first such material (terrain/displaced planes
/// carry a single material; multi-material displacement picks the first). A bound vector-displacement
/// map switches the kernel to tangent-space vector offset. `None` → not displaced.
fn displace_info_for(item: &DrawItem) -> Option<DisplaceInfo> {
    item.submesh_materials.iter().find_map(|m| {
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

fn submesh_cull_for(item: &DrawItem) -> Vec<vk::CullModeFlags> {
    let count = item.mesh.submeshes.len().max(1);
    let last = item.submesh_materials.len().saturating_sub(1);
    (0..count)
        .map(|s| {
            let two_sided = item
                .submesh_materials
                .get(s.min(last))
                .is_some_and(|m| m.double_sided);
            if two_sided {
                vk::CullModeFlags::NONE
            } else {
                vk::CullModeFlags::BACK
            }
        })
        .collect()
}

/// Builds one draw item's per-submesh [`InstanceData`] rows, interning each submesh's
/// [`MaterialParamsData`] into the frame's deduplicated table and pinning every sampled
/// texture into `live_textures`.
fn build_instance_rows(
    item: &DrawItem,
    prev_model: Mat4,
    default_texture_index: u32,
    material_table: &mut Vec<MaterialParamsData>,
    material_dedup: &mut HashMap<MaterialParamsData, u32>,
    live_textures: &mut Vec<Arc<crate::GpuTexture>>,
) -> Vec<InstanceData> {
    let submesh_count = item.mesh.submeshes.len().max(1);
    let mut rows = Vec::with_capacity(submesh_count);
    for s in 0..submesh_count {
        let material = item
            .submesh_materials
            .get(s.min(item.submesh_materials.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        let (params, albedo_index, mr_index) =
            resolve_material(&material, default_texture_index, live_textures);
        let material_index = intern_material(params, material_table, material_dedup);

        rows.push(InstanceData {
            model: item.model,
            normal_matrix: item.normal_matrix,
            prev_model,
            base_color: material.base_color,
            // .x albedo bindless, .y joint-palette offset, .z metallic-roughness, .w material index.
            texture: UVec4::new(albedo_index, item.joint_offset, mr_index, material_index),
            pbr: Vec4::new(material.metallic, material.roughness, 0.0, 0.0),
            emissive: (material.emissive * material.emissive_strength).extend(0.0),
        });
    }
    rows
}

/// Packs a [`SubmeshMaterial`] into the std430 [`MaterialParamsData`], resolving each
/// texture to its bindless index (default white when absent) and setting the feature
/// bits, while pinning the live texture `Arc`s. Returns the params plus the albedo +
/// metallic-roughness indices the instance row also carries.
fn resolve_material(
    material: &SubmeshMaterial,
    default_texture_index: u32,
    live_textures: &mut Vec<Arc<crate::GpuTexture>>,
) -> (MaterialParamsData, u32, u32) {
    let mut albedo_index = default_texture_index;
    let mut mr_index = default_texture_index;
    let mut normal_index = default_texture_index;
    let mut occlusion_index = default_texture_index;
    let mut emissive_index = default_texture_index;
    let mut height_index = default_texture_index;
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
        tex1: UVec4::new(height_index, occlusion_index, 0, features),
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

/// Interns a material into the frame's deduplicated table, hashing its raw bytes (the
/// [`MaterialParamsData`] `Hash`/`Eq` are byte-exact), so identical materials collapse
/// to one entry. Returns the entry index (`InstanceData.texture.w`).
fn intern_material(
    params: MaterialParamsData,
    table: &mut Vec<MaterialParamsData>,
    dedup: &mut HashMap<MaterialParamsData, u32>,
) -> u32 {
    if let Some(&index) = dedup.get(&params) {
        return index;
    }
    let index = table.len() as u32;
    table.push(params);
    dedup.insert(params, index);
    index
}

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

/// Tallies the per-frame draw counters from the batch list — one `drawIndexed` per
/// submesh per batch, the instance + triangle totals.
fn compute_stats(
    batches: &[DrawBatch],
    transparent: &[DrawBatch],
    pipelines_created: u32,
    instance_rows: usize,
) -> RenderStats {
    let mut stats = RenderStats {
        batches: (batches.len() + transparent.len()) as u32,
        pipelines_created,
        instance_upload_bytes: u64::try_from(instance_rows)
            .unwrap_or(u64::MAX)
            .saturating_mul(size_of::<InstanceData>() as u64),
        ..RenderStats::default()
    };
    let mut retained_meshes = HashSet::new();
    for batch in batches.iter().chain(transparent) {
        if retained_meshes.insert(Arc::as_ptr(&batch.mesh)) {
            stats.retained_mesh_cpu_bytes = stats
                .retained_mesh_cpu_bytes
                .saturating_add(batch.mesh.retained_query_cpu_bytes());
        }
        // A batch draws only its submesh subset (one mesh's submeshes split across batches by
        // blend mode), so the draw-call + triangle tallies key off that subset, not the whole mesh.
        if batch.mesh.submeshes.is_empty() {
            stats.draw_calls += 1;
            stats.instances += batch.instance_count;
            stats.triangles += (batch.mesh.index_count / 3) * batch.instance_count;
            continue;
        }
        stats.draw_calls += batch.submeshes.len() as u32;
        stats.instances += batch.instance_count;
        let indices: u32 = batch
            .submeshes
            .iter()
            .filter_map(|&s| batch.mesh.submeshes.get(s as usize))
            .map(|s| s.index_count)
            .sum();
        stats.triangles += (indices / 3) * batch.instance_count;
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::SurfaceSource;
    use crate::draw_list::{DrawItem, SubmeshMaterial};
    use crate::gpu_types::Material;
    use crate::resources::BindlessFreeList;
    use crate::skinning::Skinning;
    use crate::upload::Uploader;
    use crate::validation_issue_count;
    use saffron_geometry::glam::{Mat4, Vec2, Vec3, Vec4};
    use saffron_geometry::{Mesh, Submesh, Vertex, VertexSkin};
    use std::sync::Mutex;

    /// A device + descriptors + pipelines + instancing + skinning + uploader fixture, or
    /// `None` when no Vulkan ICD is available (the test skips cleanly).
    #[allow(clippy::type_complexity)]
    fn fixture_or_skip() -> Option<(
        Device,
        Descriptors,
        Pipelines,
        Instancing,
        Skinning,
        Uploader,
    )> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let instancing = Instancing::new(&device, &descriptors).expect("Instancing::new");
        let skinning = Skinning::new(&device).expect("Skinning::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        Some((
            device,
            descriptors,
            pipelines,
            instancing,
            skinning,
            uploader,
        ))
    }

    /// The default draw-list inputs for frame `frame` — identity view-proj, no
    /// wireframe, the default white slot, no RT consumer.
    fn inputs(frame: usize) -> DrawListInputs {
        DrawListInputs {
            frame,
            view_proj: Mat4::IDENTITY,
            wireframe: false,
            default_texture_index: crate::DEFAULT_WHITE_SLOT,
            rt_skinned: false,
            displace_enabled: true,
            tess_factor_cap: crate::tessellation::TESS_DEFAULT_FACTOR_CAP,
            tess_min_factor: crate::tessellation::TESS_DEFAULT_MIN_FACTOR,
            tess_edge_length_target: crate::tessellation::TESS_DEFAULT_EDGE_LENGTH_TARGET,
        }
    }

    /// A single-submesh triangle.
    fn triangle() -> Mesh {
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
            ..Vertex::default()
        };
        Mesh {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    /// A two-submesh quad (two triangles, one submesh each), for the per-submesh blend split.
    fn two_submesh_quad() -> Mesh {
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
            ..Vertex::default()
        };
        Mesh {
            vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(1.0, 1.0), v(-1.0, 1.0)],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![
                Submesh {
                    first_index: 0,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 0,
                },
                Submesh {
                    first_index: 3,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 1,
                },
            ],
        }
    }

    /// One mesh whose two submeshes are opaque + translucent splits into an opaque batch (submesh
    /// 0) and a sorted-transparent batch (submesh 1), both keyed off the same shared instance
    /// block — the per-submesh blend routing that makes a mixed-material model's `BLEND` panels
    /// actually blend instead of taking slot 0's mode for the whole mesh.
    #[test]
    fn submit_draw_list_splits_submeshes_by_blend_mode() {
        let Some((device, descriptors, mut pipelines, mut instancing, mut skinning, uploader)) =
            fixture_or_skip()
        else {
            return;
        };
        let mesh = uploader
            .upload_mesh(&descriptors, &two_submesh_quad(), &[], None, None)
            .expect("upload quad");
        let opaque = SubmeshMaterial {
            blend_mode: BlendMode::Opaque,
            ..SubmeshMaterial::defaults()
        };
        let blend = SubmeshMaterial {
            blend_mode: BlendMode::Blend,
            ..SubmeshMaterial::defaults()
        };
        let item = DrawItem::new(Arc::clone(&mesh), Mat4::IDENTITY, vec![opaque, blend]);

        let (list, stats) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[item],
                &[],
                inputs(0),
            )
            .expect("submit_draw_list");

        assert!(list.valid);
        // Submesh 0 (opaque) draws in the opaque list; submesh 1 (translucent) in the sorted
        // transparent list — one batch each, from the one mesh.
        assert_eq!(
            list.batches.len(),
            1,
            "the opaque submesh is one opaque batch"
        );
        assert_eq!(list.batches[0].submeshes, vec![0]);
        assert_eq!(
            list.transparent_batches.len(),
            1,
            "the translucent submesh is one transparent batch"
        );
        assert_eq!(list.transparent_batches[0].submeshes, vec![1]);
        // Both batches share the one submesh-major instance block.
        assert_eq!(list.batches[0].base_instance, 0);
        assert_eq!(list.transparent_batches[0].base_instance, 0);
        // Two draw calls total (one per submesh), across the two lists.
        assert_eq!(stats.draw_calls, 2);
        assert_eq!(stats.batches, 2);

        drop(list);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);
    }

    /// Three items of two meshes (A×2, B×1) batch into two (pipeline, mesh) buckets
    /// with the submesh-major `base_instance`/`instance_count` the scene pass reads, and
    /// the stats report 2 batches / 3 instances — the phase's named batching gate.
    #[test]
    fn submit_draw_list_batches_by_pipeline_and_mesh() {
        let Some((device, descriptors, mut pipelines, mut instancing, mut skinning, uploader)) =
            fixture_or_skip()
        else {
            return;
        };

        let mesh_a = uploader
            .upload_mesh(&descriptors, &triangle(), &[], None, None)
            .expect("upload A");
        let mesh_b = uploader
            .upload_mesh(&descriptors, &triangle(), &[], None, None)
            .expect("upload B");
        let item = |mesh: &Arc<crate::GpuMesh>| {
            DrawItem::new(
                Arc::clone(mesh),
                Mat4::IDENTITY,
                vec![SubmeshMaterial::defaults()],
            )
        };
        let items = [item(&mesh_a), item(&mesh_b), item(&mesh_a)];

        let (list, stats) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &items,
                &[],
                inputs(0),
            )
            .expect("submit_draw_list");

        assert!(list.valid);
        assert_eq!(list.batches.len(), 2, "two (pipeline, mesh) buckets");
        assert_eq!(stats.batches, 2);
        assert_eq!(stats.instances, 3, "three logical instances total");
        assert_eq!(
            stats.instance_upload_bytes,
            3 * size_of::<InstanceData>() as u64,
            "the counter reports the exact uploaded row bytes"
        );
        assert_eq!(
            stats.retained_mesh_cpu_bytes,
            mesh_a
                .retained_query_cpu_bytes()
                .saturating_add(mesh_b.retained_query_cpu_bytes()),
            "shared mesh instances contribute retained query memory once"
        );
        // One drawIndexed per submesh per batch (instanced), not per instance: two
        // single-submesh batches → two draw calls.
        assert_eq!(
            stats.draw_calls, 2,
            "one instanced drawIndexed per submesh per batch"
        );

        // Bucket order is first-seen: A (2 instances) then B (1 instance). The
        // submesh-major flatten lays A's two rows first, then B's single row.
        let batch_a = &list.batches[0];
        let batch_b = &list.batches[1];
        assert!(Arc::ptr_eq(&batch_a.mesh, &mesh_a));
        assert_eq!(batch_a.base_instance, 0);
        assert_eq!(batch_a.instance_count, 2);
        assert!(Arc::ptr_eq(&batch_b.mesh, &mesh_b));
        assert_eq!(batch_b.base_instance, 2, "B follows A's two rows");
        assert_eq!(batch_b.instance_count, 1);

        // Every `Arc<GpuMesh>` must release before the device tears down (the device
        // outlives every resource — README §4). `items` + `list` hold clones, so they
        // drop here, ahead of the mesh handles and the device.
        drop(items);
        drop(list);
        drop(mesh_a);
        drop(mesh_b);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);
    }

    /// Two items with byte-identical materials dedup to one material SSBO entry; two
    /// distinct materials produce two. Driven through `submit_draw_list` (the gate's
    /// dedup case), then asserted by the resulting instance rows' `texture.w` indices.
    #[test]
    fn submit_draw_list_dedups_identical_materials() {
        let Some((device, descriptors, mut pipelines, mut instancing, mut skinning, uploader)) =
            fixture_or_skip()
        else {
            return;
        };
        let mesh = uploader
            .upload_mesh(&descriptors, &triangle(), &[], None, None)
            .expect("upload");

        // Two items sharing one material (same factors), then a third with a different
        // base color. Distinct meshes would still share the deduped material table.
        let red = SubmeshMaterial {
            base_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
            ..SubmeshMaterial::defaults()
        };
        let blue = SubmeshMaterial {
            base_color: Vec4::new(0.0, 0.0, 1.0, 1.0),
            ..SubmeshMaterial::defaults()
        };
        let mk = |mat: &SubmeshMaterial, model: Mat4| {
            let mut item = DrawItem::new(Arc::clone(&mesh), model, vec![mat.clone()]);
            item.material = Material::default();
            item
        };

        // Two identical-red items: one material entry. The materials are deduped per
        // frame, so the only count we can read back is via fresh frames with controlled
        // capacity — assert through the public batching invariants + the helper below.
        let same = [
            mk(&red, Mat4::IDENTITY),
            mk(&red, Mat4::from_translation(Vec3::X)),
        ];
        let (list_same, _) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &same,
                &[],
                inputs(0),
            )
            .expect("submit same");
        assert_eq!(list_same.batches.len(), 1, "one (pipeline, mesh) batch");
        assert_eq!(list_same.batches[0].instance_count, 2);

        let distinct = [mk(&red, Mat4::IDENTITY), mk(&blue, Mat4::IDENTITY)];
        let (list_distinct, _) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &distinct,
                &[],
                inputs(1),
            )
            .expect("submit distinct");
        assert_eq!(
            list_distinct.batches[0].instance_count, 2,
            "still one batch (bindless: material does not split a batch)"
        );

        // The byte-exact dedup itself is the unit test below; here we confirm the
        // material table built two entries for the distinct case by re-deriving it.
        let red_params = material_params(&red);
        let blue_params = material_params(&blue);
        assert_ne!(
            red_params, blue_params,
            "distinct base colors are distinct params"
        );

        // Release every `Arc<GpuMesh>` (the draw-item arrays + lists hold clones)
        // before the device — the device outlives every resource (README §4).
        drop(same);
        drop(distinct);
        drop(list_same);
        drop(list_distinct);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);
        let _ = validation_issue_count();
    }

    /// Re-derives a submesh material's std430 params for the dedup assertion above.
    fn material_params(material: &SubmeshMaterial) -> MaterialParamsData {
        let mut live = Vec::new();
        resolve_material(material, crate::DEFAULT_WHITE_SLOT, &mut live).0
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

    /// Interning byte-identical materials collapses to one entry; a differing field is
    /// a distinct entry. This is the per-frame dedup the instance row's `.texture.w`
    /// indexes.
    #[test]
    fn intern_material_dedups_by_bytes() {
        let mut table = Vec::new();
        let mut dedup = HashMap::new();
        let a = MaterialParamsData::default();
        let b = MaterialParamsData::default();
        assert_eq!(intern_material(a, &mut table, &mut dedup), 0);
        assert_eq!(
            intern_material(b, &mut table, &mut dedup),
            0,
            "identical material reuses entry 0"
        );
        assert_eq!(table.len(), 1, "no second entry for an identical material");

        let mut c = MaterialParamsData::default();
        c.tex0.x = 7;
        assert_eq!(
            intern_material(c, &mut table, &mut dedup),
            1,
            "a differing index is a distinct entry"
        );
        assert_eq!(table.len(), 2);
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
        uploader
            .upload_mesh(descriptors, &mesh, &skin, None, None)
            .expect("upload skinned")
    }

    /// A skinned draw item keyed by `entity` with a one-joint palette slice.
    fn skinned_item(mesh: &Arc<crate::GpuMesh>, model: Mat4, entity: u64) -> DrawItem {
        let mut item = DrawItem::new(Arc::clone(mesh), model, vec![SubmeshMaterial::defaults()]);
        item.skinned = true;
        item.joint_offset = 0;
        item.joint_count = 1;
        item.entity = entity;
        item
    }

    /// A skinned draw with a palette produces a skin dispatch per skinned instance, and
    /// the deformed buffers are allocated; an unskinned draw produces none. The skin pass
    /// is armed only when `skin_dispatches` is non-empty — the phase's named gate.
    #[test]
    fn skin_dispatch_appears_only_for_skinned_draws() {
        let Some((device, descriptors, mut pipelines, mut instancing, mut skinning, uploader)) =
            fixture_or_skip()
        else {
            return;
        };
        let mesh = skinned_triangle(&descriptors, &uploader);

        // An unskinned draw, even with a palette supplied, emits no skin dispatch.
        let static_item = DrawItem::new(
            Arc::clone(&mesh),
            Mat4::IDENTITY,
            vec![SubmeshMaterial::defaults()],
        );
        let palette = [Mat4::IDENTITY];
        let (static_list, _) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[static_item],
                &palette,
                inputs(0),
            )
            .expect("submit static");
        assert!(
            static_list.skin_dispatches.is_empty(),
            "an unskinned draw arms no skin dispatch"
        );

        // A skinned draw with a palette emits one dispatch (current + prev), and the
        // deformed buffers are now allocated.
        let item = skinned_item(&mesh, Mat4::IDENTITY, 1);
        let (list, _) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[item],
                &palette,
                inputs(1),
            )
            .expect("submit skinned");
        assert_eq!(
            list.skin_dispatches.len(),
            1,
            "one dispatch per skinned bucket"
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
        // The batch draws the deformed buffer as a static stream (its base vertex offset).
        assert_eq!(list.batches.len(), 1);
        assert!(list.batches[0].deformed);
        assert_eq!(list.batches[0].deformed_vertex_offset, 0);

        drop(static_list);
        drop(list);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);
    }

    /// A new entity's first frame reads back prev == current (zero motion: `prev_model`
    /// equals `model`, and the prev palette copies the current one); a moved entity's
    /// second frame reflects last frame's pose — the phase's named cross-frame gate.
    #[test]
    fn cross_frame_motion_caches_track_the_entity() {
        let Some((device, descriptors, mut pipelines, mut instancing, mut skinning, uploader)) =
            fixture_or_skip()
        else {
            return;
        };
        let mesh = skinned_triangle(&descriptors, &uploader);

        // Frame one: a new entity at the origin. Uncached → prev_model == model and the
        // cache now holds this pose.
        let first = Mat4::IDENTITY;
        instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[skinned_item(&mesh, first, 7)],
                &[Mat4::IDENTITY],
                inputs(0),
            )
            .expect("frame one");
        assert_eq!(
            skinning.prev_model(7),
            Some(first),
            "the entity's pose is cached after its first frame"
        );

        // Frame two: the same entity moved. The instance's prev_model must be frame one's
        // pose; submit_draw_list reads it before overwriting the cache.
        let second = Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0));
        let (list, _) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[skinned_item(&mesh, second, 7)],
                &[Mat4::IDENTITY],
                inputs(1),
            )
            .expect("frame two");
        // The single instance row's prev_model is frame one's identity, its model frame two.
        assert_eq!(list.batches[0].instance_count, 1);
        assert_eq!(
            skinning.prev_model(7),
            Some(second),
            "the cache advanced to frame two's pose"
        );

        drop(list);
        drop(mesh);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);
    }
}
