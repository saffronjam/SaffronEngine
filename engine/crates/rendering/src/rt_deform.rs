//! Materializing wind-deformed geometry for acceleration-structure builds.
//!
//! Every raster pass applies the wind prepass record in its vertex stage. Ray traversal has no
//! vertex stage, so the only way a swaying plant reaches a bottom-level structure is as vertices:
//! this plans one `rt_deform` dispatch per placed use, writing world-space deformed vertices into
//! the shared deformed arena — the same arena the skinning refits build from — and hands the
//! result back as [`DeformedRtInstance`]s so the refit path builds and updates them like any other
//! deforming geometry.
//!
//! The materialized set is a budgeted cache over the static representation, not a replacement for
//! it: a plant that does not fit the budget keeps its shared per-prototype structures at rest pose,
//! which is a smaller error the further away it is, because sway projects to less of a pixel with
//! distance.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::Vertex;
use saffron_geometry::glam::{Mat4, Vec3};

use crate::draw_list::DeformedRtInstance;
use crate::rt::{RT_UNMIRRORED_INSTANCE, RtCutView, RtInstanceInput, wind_blas_key};

/// Placed uses one frame may materialize. The refits are per-frame work on top of the static
/// structures, so the cap is what keeps a dense meadow from turning the frame into
/// acceleration-structure builds.
pub const RT_DEFORM_MAX_JOBS: usize = 64;

/// Deformed-arena vertices one frame may spend on materialized wind geometry, on top of whatever
/// skinning and morph claim.
pub const RT_DEFORM_MAX_VERTICES: u32 = 262_144;

/// One materialization dispatch, pushed whole (96 bytes, inside the 128-byte guaranteed range).
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct RtDeformPush {
    /// The use's family-local transform, row-major 3×4.
    pub use_rows: [f32; 12],
    /// Device address of the mesh's static vertex stream.
    pub source: vk::DeviceAddress,
    /// Device address of the destination slice in the deformed arena.
    pub dest: vk::DeviceAddress,
    /// First vertex of the mesh's stream this slice mirrors.
    pub first_vertex: u32,
    /// Vertices the slice covers.
    pub vertex_count: u32,
    /// The GPU-scene instance slot the wind record and world transform are read from.
    pub instance_slot: u32,
    /// The use's ordinal in the family table (the branch mode's stable phase).
    pub use_index: u32,
    /// The use's structural semantic (trunk, branch, leaf …).
    pub semantic: u32,
    /// Nonzero when `use_rows` applies — a family. Zero for a plain mesh.
    pub assembly: u32,
    reserved: [u32; 2],
}

const _: () = assert!(size_of::<RtDeformPush>() == 96);

/// This frame's materialization work: the dispatches, the deforming instances they feed, the
/// static instances left behind, and the arena high-water the dispatches need.
pub struct RtDeformPlan {
    /// One per entry of [`RtDeformPlan::instances`].
    pub jobs: Vec<RtDeformPush>,
    /// The deforming instances the refit path builds structures for.
    pub instances: Vec<DeformedRtInstance>,
    /// What remains of the captured static scene: the materialized instances are removed, because
    /// leaving them would place the same plant twice — once swaying and once at rest.
    pub statics: Vec<RtInstanceInput>,
    /// Deformed-arena vertices the plan reaches (an exclusive end, in [`Vertex`] elements).
    pub high_water: u32,
}

/// Plans this frame's wind materialization over `scene`, the captured static ray instances.
///
/// `field_speed` is the wind field's mean speed in metres per second. Every deformation term
/// scales with it — the gust is a fraction of the mean, and the branch and flutter modes are
/// driven by the sway — so a field at rest displaces nothing and the rest-pose structures the
/// upload built already are the pose every pass draws. Materializing there would spend a
/// dispatch and a bottom-level build per placed use to reproduce that pose, and would replace
/// a cluster-composed structure with a plain triangle build to do it.
///
/// `deformed_base` is the arena cursor skinning and morph left. Returns `None` when nothing
/// qualifies, so an unwindy frame keeps the captured scene's cached allocation untouched.
pub fn plan_wind_deformation(
    scene: &[RtInstanceInput],
    cut_view: &RtCutView,
    field_speed: f32,
    deformed_base: u32,
) -> Option<RtDeformPlan> {
    if field_speed <= 0.0 {
        return None;
    }
    let eye = Vec3::from(cut_view.eye);
    // Nearest first. Sway is a world-space field displacement rather than a property of the
    // instance, so the projected size of that displacement falls with distance alone — which makes
    // distance the same lever the representation cut uses, and the near set the one where a rest
    // pose is visible.
    let mut candidates: Vec<(usize, f32)> = scene
        .iter()
        .enumerate()
        .filter(|(_, input)| input.wind && input.instance_slot != RT_UNMIRRORED_INSTANCE)
        // A family the cut has already coarsened to its aggregate structure is not materialized:
        // that representation merged the geometry sway would move.
        .filter(|(_, input)| crate::rt::aggregate_stands_in(input, cut_view).is_none())
        .map(|(index, input)| {
            (
                index,
                (input.model.w_axis.truncate() - eye).length_squared(),
            )
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));

    let mut jobs: Vec<RtDeformPush> = Vec::new();
    let mut instances: Vec<DeformedRtInstance> = Vec::new();
    let mut materialized: Vec<usize> = Vec::new();
    let mut cursor = deformed_base;
    let ceiling = deformed_base.saturating_add(RT_DEFORM_MAX_VERTICES);
    for (index, _) in candidates {
        let input = &scene[index];
        let before = (jobs.len(), instances.len(), cursor);
        if !push_instance_jobs(input, &mut jobs, &mut instances, &mut cursor, ceiling) {
            // Partial materialization would sway some of a plant and leave the rest at rest pose,
            // which reads as a broken model rather than as a budget: roll the whole plant back and
            // let it keep its static structures.
            jobs.truncate(before.0);
            instances.truncate(before.1);
            cursor = before.2;
            continue;
        }
        materialized.push(index);
    }
    if instances.is_empty() {
        return None;
    }
    let statics = scene
        .iter()
        .enumerate()
        .filter(|(index, _)| !materialized.contains(index))
        .map(|(_, input)| input.clone())
        .collect();
    Some(RtDeformPlan {
        jobs,
        instances,
        statics,
        high_water: cursor,
    })
}

/// Appends one instance's jobs, or reports `false` when the whole instance does not fit.
fn push_instance_jobs(
    input: &RtInstanceInput,
    jobs: &mut Vec<RtDeformPush>,
    instances: &mut Vec<DeformedRtInstance>,
    cursor: &mut u32,
    ceiling: u32,
) -> bool {
    let mut claim = |first_vertex: u32,
                     vertex_count: u32,
                     first_submesh: u32,
                     submesh_count: u32,
                     use_index: u32,
                     use_record: Option<&crate::GpuAssemblyUseRecord>|
     -> bool {
        if vertex_count == 0 || jobs.len() >= RT_DEFORM_MAX_JOBS {
            return false;
        }
        // The index stream addresses the mesh's vertices absolutely while the slice starts at the
        // run's own base, so the build rebases the vertex address by `first_vertex`. Placing a
        // slice below its own base would put that rebased address before the buffer.
        let offset = (*cursor).max(first_vertex);
        if offset.saturating_add(vertex_count) > ceiling {
            return false;
        }
        *cursor = offset + vertex_count;
        jobs.push(RtDeformPush {
            use_rows: use_record.map_or(
                [
                    1.0, 0.0, 0.0, 0.0, //
                    0.0, 1.0, 0.0, 0.0, //
                    0.0, 0.0, 1.0, 0.0,
                ],
                |record| record.transform,
            ),
            source: 0,
            dest: 0,
            first_vertex,
            vertex_count,
            instance_slot: input.instance_slot,
            use_index,
            semantic: use_record.map_or(0, |record| record.reserved[0]),
            assembly: u32::from(use_record.is_some()),
            reserved: [0; 2],
        });
        instances.push(DeformedRtInstance {
            entity: wind_blas_key(input.instance_slot, use_index),
            deformed_offset: offset,
            vertex_base: first_vertex,
            vertex_count,
            index_count: input.mesh.index_count,
            first_submesh,
            submesh_count,
            instance_slot: input.instance_slot,
            opacity_override: input.opacity_override,
            mesh: Arc::clone(&input.mesh),
            // The materialized vertices are world-space, exactly as compute skinning's are.
            world_transform: Mat4::IDENTITY,
            tess: None,
        });
        true
    };

    match input.mesh.assembly.as_ref() {
        Some(assembly) => {
            if input.mesh.assembly_blas.len() != assembly.prototypes.len() {
                return false;
            }
            let words = assembly.mask_words();
            let base = input.combination as usize * words;
            let mut placed = false;
            for (use_index, use_record) in assembly.uses.iter().enumerate() {
                let word = assembly.masks.get(base + use_index / 32).copied();
                if word.is_none_or(|bits| bits & (1 << (use_index % 32)) == 0) {
                    continue;
                }
                let Some(slice) = assembly.prototype_slices.get(use_record.prototype as usize)
                else {
                    return false;
                };
                if !claim(
                    slice.first_vertex,
                    slice.vertex_count,
                    slice.first_submesh,
                    slice.submesh_count,
                    use_index as u32,
                    Some(use_record),
                ) {
                    return false;
                }
                placed = true;
            }
            placed
        }
        None => {
            if input.mesh.blas.is_none() {
                return false;
            }
            claim(
                0,
                input.mesh.vertex_count,
                0,
                input.mesh.submeshes.len() as u32,
                0,
                None,
            )
        }
    }
}

/// Fills each job's source and destination device addresses, once the arena is sized.
pub fn resolve_job_addresses(
    plan: &mut RtDeformPlan,
    device: &crate::Device,
    deformed: vk::Buffer,
) {
    let stride = size_of::<Vertex>() as vk::DeviceAddress;
    let dest_base = device.buffer_device_address(deformed);
    for (job, instance) in plan.jobs.iter_mut().zip(&plan.instances) {
        job.source = device.buffer_device_address(instance.mesh.vertex_buffer());
        job.dest = dest_base + vk::DeviceAddress::from(instance.deformed_offset) * stride;
    }
}

/// Records the frame's materialization dispatches: one per job, 64 vertices per group.
pub fn record_rt_deform(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    jobs: &[RtDeformPush],
) {
    // SAFETY: the ash seam. The PSO/set are valid this frame; every push spans the declared
    // range and every address references a live buffer of this frame's deformation state.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pipeline.layout(),
            0,
            &[set],
            &[],
        );
        for job in jobs {
            raw.cmd_push_constants(
                cmd,
                pipeline.layout(),
                vk::ShaderStageFlags::COMPUTE,
                0,
                bytemuck::bytes_of(job),
            );
            raw.cmd_dispatch(cmd, job.vertex_count.div_ceil(64), 1, 1);
        }
    }
}
