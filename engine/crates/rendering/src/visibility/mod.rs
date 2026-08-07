//! Instance visibility for the persistent GPU scene.
//!
//! One parameterized compute pipeline (`scene_visibility.slang`) classifies every
//! occupied instance slot for a view. The cull pass frustum-tests current world bounds
//! and occlusion-tests established instances (per-slot history words) against the
//! previous HZB with previous transforms — occluded established instances wait on a
//! retest list, new or history-invalid instances bypass. The retest pass re-tests that
//! list against the current HZB after the provisional raster and merges survivors.
//! Lists and counters are per frame slot (a frame in flight never shares GPU-written
//! buffers); the history buffer is cross-frame and clears on invalidation. Overflow
//! never truncates silently: the counters carry overflow flags.

mod buckets;
mod dispatch;
mod executor;
mod passes;
mod push;
mod view;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use ash::vk;

use crate::device::Device;
use crate::resources::{Buffer, DeviceResources};
use crate::{Result, checked};

pub use buckets::{ExecutorBucket, bucket_material, build_executor_buckets};
pub use executor::{
    ExecutorBucketDraw, ExecutorDrawInputs, MESH_TASK_COMMAND_STRIDE, MESH_TRIANGLES_PER_GROUP,
    TransparentSortPipelines, mesh_executor_supported, record_executor_bucket_draw,
    record_executor_bucket_draw_mesh, transparent_slice_base,
};
pub use push::*;

/// Cull pass kind (full instance sweep).
pub const SCENE_VISIBILITY_PASS_CULL: u32 = 0;
/// Retest pass kind (re-test the retest list against the current pyramid).
pub const SCENE_VISIBILITY_PASS_RETEST: u32 = 1;
/// Reach pass kind (keep every instance whose world sphere meets the reach box).
///
/// Global illumination is not a camera: a march gathers from behind the eye and a
/// reflection shows what the camera cannot, so neither the frustum nor the depth pyramid
/// is a sound rejection for it. The reach box is the only one that is.
pub const SCENE_VISIBILITY_PASS_REACH: u32 = 2;

/// Counter word: visible count.
pub const SCENE_VISIBILITY_COUNTER_VISIBLE: usize = 0;
/// Counter word: retest count.
pub const SCENE_VISIBILITY_COUNTER_RETEST: usize = 1;
/// Counter word: overflow flags.
pub const SCENE_VISIBILITY_COUNTER_OVERFLOW: usize = 2;
/// Counter word: emitted semantic records.
pub const SCENE_VISIBILITY_COUNTER_RECORDS: usize = 3;
/// Counter word: record overflow flags.
pub const SCENE_VISIBILITY_COUNTER_RECORD_OVERFLOW: usize = 4;
/// Total counter words per frame slot. Words ride the one fence-gated readback the
/// frame already copies, so a new counter costs a wider block and never a second copy.
pub const SCENE_VISIBILITY_COUNTER_WORDS: u64 = 24;

/// Counter word: generated micro-blade candidates this frame.
pub const SCENE_VISIBILITY_COUNTER_MICRO_CANDIDATES: usize = 9;
/// Counter word: aggregate-voxel records emitted by the traversal.
pub const SCENE_VISIBILITY_COUNTER_VOXEL_RECORDS: usize = 11;
/// Counter word: the deepest hierarchy level on the emitted cut (a running max).
pub const SCENE_VISIBILITY_COUNTER_MAX_CUT_DEPTH: usize = 12;
/// Counter word: instances the frustum culled.
pub const SCENE_VISIBILITY_COUNTER_CULLED_FRUSTUM: usize = 13;
/// Counter word: instances the occlusion retest kept hidden.
pub const SCENE_VISIBILITY_COUNTER_CULLED_OCCLUSION: usize = 14;
/// Counter word: emitted triangles whose whole record projects under a 2×2 quad —
/// the quad-utilization pressure the rasterizer pays for sub-quad geometry.
pub const SCENE_VISIBILITY_COUNTER_SUB_QUAD_TRIANGLES: usize = 15;
/// Counter word: hierarchy nodes the traversal rejected on their swept bounds, each
/// dropping the whole subtree beneath it.
pub const SCENE_VISIBILITY_COUNTER_CULLED_NODES: usize = 16;
/// Counter word: hierarchy nodes the traversal reached with a resolved assembly use —
/// the denominator the culled count is a fraction of.
pub const SCENE_VISIBILITY_COUNTER_VISITED_NODES: usize = 17;
/// Counter word: executor buckets that received at least one record — the indirect draws the
/// frame issues, counted on the pass that already touches every record.
pub const SCENE_VISIBILITY_COUNTER_BINS: usize = 18;
/// Counter word: deformed instances this view composed bounds for.
pub const SCENE_VISIBILITY_COUNTER_DEFORMED: usize = 19;

/// Samples a geometry fragment shader actually covered, counted at word 20.
///
/// A helper invocation's atomics are discarded by the spec, so this counts REAL lanes while the
/// `FRAGMENT_SHADER_INVOCATIONS` pipeline statistic counts every lane including helpers. Their
/// ratio is quad utilization — the fraction of a shaded 2x2 quad that was not wasted — which no
/// pipeline statistic reports on its own and which foliage, all thin slivers, destroys.
pub const SCENE_VISIBILITY_COUNTER_COVERED_SAMPLES: usize = 20;

/// Counter word: deformed instances the interaction field's re-centring scroll reset this
/// frame — their stored displacement jumped rather than moved, so the reactive-coverage
/// pass marks them and TAA stops reprojecting them. Counted beside
/// [`SCENE_VISIBILITY_COUNTER_DEFORMED`], so the two share a denominator.
pub const SCENE_VISIBILITY_COUNTER_INTERACTION_RESET: usize = 21;

/// Counter word: instances the reach pass rejected — outside the window any march can
/// read, so no gather can be missing them.
pub const SCENE_VISIBILITY_COUNTER_CULLED_REACH: usize = 22;

/// Counter word: triangle clusters on a surviving node the traversal rejected on their own
/// swept bounds. A node keeps its subtree, so this is the finer granularity underneath
/// [`SCENE_VISIBILITY_COUNTER_CULLED_NODES`]: one assembly part leaves the view while its
/// siblings draw.
pub const SCENE_VISIBILITY_COUNTER_CULLED_CLUSTERS: usize = 23;

/// Micro-blade candidates a frame slot can hold.
pub const SCENE_MICRO_CANDIDATE_CAPACITY: u32 = 65_536;
/// Overflow flag: the visible list filled.
pub const SCENE_VISIBILITY_OVERFLOW_VISIBLE: u32 = 1;
/// Overflow flag: the retest list filled.
pub const SCENE_VISIBILITY_OVERFLOW_RETEST: u32 = 2;
/// Overflow flag: the semantic record stream filled.
pub const SCENE_TRAVERSAL_OVERFLOW_RECORDS: u32 = 4;
/// Draw-bucket capacity (live (shader, psoBin) combos per frame; more is pressure).
pub const SCENE_EXECUTOR_BUCKET_CAPACITY: u32 = 512;
/// Element capacity of every view's semantic record stream (and so of each frame's
/// indirect command buffer and each blend bucket's transparent slice).
pub const SCENE_VISIBILITY_RECORD_CAPACITY: u32 = 65_536;
/// Pressure flag: a record's (shader, psoBin) had no bucket or its slice filled.
pub const SCENE_BUCKET_PRESSURE: u32 = 16;

/// Counter word: transparent sort pairs (also the transparent draw count).
pub const SCENE_VISIBILITY_COUNTER_TRANSPARENT: usize = 5;
/// Overflow flag: the transparent pair list filled.
pub const SCENE_TRANSPARENT_OVERFLOW: u32 = 8;
/// Counter word: triangles the traversal's emitted records rasterize (indices / 3,
/// survivor records included — they draw again).
pub const SCENE_VISIBILITY_COUNTER_TRIANGLES: usize = 8;
/// Pair elements per radix workgroup.
pub const SCENE_RADIX_WORKGROUP: u32 = 256;
/// Eight-bit digit passes one radix level takes over a 32-bit key word.
pub const SCENE_RADIX_PASSES: u32 = 4;
/// Key words the transparent sort orders by, least significant first: cluster, page,
/// instance slot, view depth. The lower three make the order a function of the record
/// set rather than of the traversal's atomic emission order.
pub const TRANSPARENT_SORT_LEVELS: u32 = 4;

/// Device-shared visibility scaffolding: the set layout and the HZB sampler.
pub struct SceneVisibility {
    resources: Arc<DeviceResources>,
    sampler: vk::Sampler,
    layout: vk::DescriptorSetLayout,
    traversal_layout: vk::DescriptorSetLayout,
    bin_count_layout: vk::DescriptorSetLayout,
    bin_seed_layout: vk::DescriptorSetLayout,
    bin_scatter_layout: vk::DescriptorSetLayout,
    micro_layout: vk::DescriptorSetLayout,
    transparent_keys_layout: vk::DescriptorSetLayout,
    radix_histogram_layout: vk::DescriptorSetLayout,
    radix_scan_layout: vk::DescriptorSetLayout,
    radix_scatter_layout: vk::DescriptorSetLayout,
    transparent_reorder_layout: vk::DescriptorSetLayout,
}

impl SceneVisibility {
    /// Creates the sampler and the six-binding compute set layout.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] for any failing Vulkan call.
    pub fn new(device: &Device) -> Result<Self> {
        let raw = device.raw();
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .max_lod(vk::LOD_CLAMP_NONE)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        // SAFETY: the ash seam. Freed in `Drop`.
        let sampler = checked(
            unsafe { raw.create_sampler(&sampler_info, None) },
            "visibility sampler",
        )?;
        let types = [
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            vk::DescriptorType::UNIFORM_BUFFER,
        ];
        let bindings: Vec<vk::DescriptorSetLayoutBinding> = types
            .iter()
            .enumerate()
            .map(|(index, &ty)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(index as u32)
                    .descriptor_type(ty)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the ash seam. The bindings outlive the call; freed in `Drop`.
        let layout = match checked(
            unsafe { raw.create_descriptor_set_layout(&info, None) },
            "visibility layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. Free the sampler on this partial-failure path.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };
        let traversal_types = [
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::DescriptorType::STORAGE_BUFFER,
        ];
        let traversal_bindings: Vec<vk::DescriptorSetLayoutBinding> = traversal_types
            .iter()
            .enumerate()
            .map(|(index, &ty)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(index as u32)
                    .descriptor_type(ty)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        let traversal_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(&traversal_bindings);
        // SAFETY: the ash seam. The bindings outlive the call; freed in `Drop`.
        let traversal_layout = match checked(
            unsafe { raw.create_descriptor_set_layout(&traversal_info, None) },
            "traversal layout",
        ) {
            Ok(traversal_layout) => traversal_layout,
            Err(err) => {
                // SAFETY: the ash seam. Free prior handles on this partial-failure path.
                unsafe {
                    raw.destroy_descriptor_set_layout(layout, None);
                    raw.destroy_sampler(sampler, None);
                }
                return Err(err);
            }
        };
        let sb = vk::DescriptorType::STORAGE_BUFFER;
        let ub = vk::DescriptorType::UNIFORM_BUFFER;
        let compute = vk::ShaderStageFlags::COMPUTE;
        let mut built: Vec<vk::DescriptorSetLayout> = vec![layout, traversal_layout];
        let mut make = |types: &[vk::DescriptorType], stages: vk::ShaderStageFlags| {
            match make_stage_layout(raw, types, stages) {
                Ok(created) => {
                    built.push(created);
                    Ok(created)
                }
                Err(err) => {
                    // SAFETY: the ash seam. Free everything built so far plus the
                    // sampler on this partial-failure path.
                    unsafe {
                        for stale in built.drain(..) {
                            raw.destroy_descriptor_set_layout(stale, None);
                        }
                        raw.destroy_sampler(sampler, None);
                    }
                    Err(err)
                }
            }
        };
        let bin_count_layout = make(&[sb, sb, sb, sb], compute)?;
        let bin_seed_layout = make(&[sb, sb], compute)?;
        let bin_scatter_layout = make(&[sb, sb, sb, sb, ub, sb, sb], compute)?;
        let micro_layout = make(&[sb, sb, ub, sb], compute)?;
        let transparent_keys_layout = make(&[sb, sb, sb, ub], compute)?;
        let radix_histogram_layout = make(&[sb, sb, sb], compute)?;
        let radix_scan_layout = make(&[sb], compute)?;
        let radix_scatter_layout = make(&[sb, sb, sb, sb], compute)?;
        let transparent_reorder_layout = make(&[sb, sb, sb, sb, ub, sb], compute)?;
        Ok(Self {
            resources: Arc::clone(device.resources()),
            sampler,
            layout,
            traversal_layout,
            bin_count_layout,
            bin_seed_layout,
            bin_scatter_layout,
            micro_layout,
            transparent_keys_layout,
            radix_histogram_layout,
            radix_scan_layout,
            radix_scatter_layout,
            transparent_reorder_layout,
        })
    }

    /// The transparent key-collection set layout.
    pub fn transparent_keys_layout(&self) -> vk::DescriptorSetLayout {
        self.transparent_keys_layout
    }

    /// The radix histogram set layout.
    pub fn radix_histogram_layout(&self) -> vk::DescriptorSetLayout {
        self.radix_histogram_layout
    }

    /// The radix scan set layout.
    pub fn radix_scan_layout(&self) -> vk::DescriptorSetLayout {
        self.radix_scan_layout
    }

    /// The radix scatter set layout.
    pub fn radix_scatter_layout(&self) -> vk::DescriptorSetLayout {
        self.radix_scatter_layout
    }

    /// The transparent command-reorder set layout.
    pub fn transparent_reorder_layout(&self) -> vk::DescriptorSetLayout {
        self.transparent_reorder_layout
    }

    /// The bin-count compute set layout.
    pub fn bin_count_layout(&self) -> vk::DescriptorSetLayout {
        self.bin_count_layout
    }

    /// The bin-seed compute set layout.
    pub fn bin_seed_layout(&self) -> vk::DescriptorSetLayout {
        self.bin_seed_layout
    }

    /// The bin-scatter compute set layout.
    pub fn bin_scatter_layout(&self) -> vk::DescriptorSetLayout {
        self.bin_scatter_layout
    }

    /// The micro-field pass family's set layout (counters + records + addresses +
    /// address block).
    pub fn micro_layout(&self) -> vk::DescriptorSetLayout {
        self.micro_layout
    }

    /// The visibility (cull/retest) compute set layout.
    /// The nearest-filter pyramid sampler (shared by depth-sourced compute).
    pub fn hzb_sampler(&self) -> vk::Sampler {
        self.sampler
    }

    pub fn layout(&self) -> vk::DescriptorSetLayout {
        self.layout
    }

    /// The traversal compute set layout.
    pub fn traversal_layout(&self) -> vk::DescriptorSetLayout {
        self.traversal_layout
    }
}

impl Drop for SceneVisibility {
    fn drop(&mut self) {
        let raw = self.resources.device();
        // SAFETY: the ash seam. Teardown after `wait_gpu_idle`.
        unsafe {
            raw.destroy_descriptor_set_layout(self.transparent_reorder_layout, None);
            raw.destroy_descriptor_set_layout(self.radix_scatter_layout, None);
            raw.destroy_descriptor_set_layout(self.radix_scan_layout, None);
            raw.destroy_descriptor_set_layout(self.radix_histogram_layout, None);
            raw.destroy_descriptor_set_layout(self.transparent_keys_layout, None);
            raw.destroy_descriptor_set_layout(self.micro_layout, None);
            raw.destroy_descriptor_set_layout(self.bin_scatter_layout, None);
            raw.destroy_descriptor_set_layout(self.bin_seed_layout, None);
            raw.destroy_descriptor_set_layout(self.bin_count_layout, None);
            raw.destroy_descriptor_set_layout(self.traversal_layout, None);
            raw.destroy_descriptor_set_layout(self.layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
    }
}

fn make_stage_layout(
    raw: &ash::Device,
    types: &[vk::DescriptorType],
    stages: vk::ShaderStageFlags,
) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = types
        .iter()
        .enumerate()
        .map(|(index, &ty)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(index as u32)
                .descriptor_type(ty)
                .descriptor_count(1)
                .stage_flags(stages)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the caller owns the layout.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "visibility stage layout",
    )
}

/// One frame slot's lists and its cull/retest sets.
struct VisibilityFrame {
    counters: Buffer,
    readback: Buffer,
    visible: Buffer,
    retest: Buffer,
    records: Buffer,
    bin_counts: Buffer,
    bin_cursors: Buffer,
    bucket_table: Buffer,
    /// The executor command arena: the binner's per-bucket slices in the first
    /// `record_capacity` slots, then one full-length sorted slice per blend bucket.
    commands: Buffer,
    /// The mesh executor's per-command dispatch arguments, parallel to `commands`: one
    /// `VkDrawMeshTasksIndirectCommandEXT` per draw, written at the same slot by whichever
    /// kernel filled that slot.
    mesh_args: Buffer,
    pairs: [Buffer; 2],
    histograms: Buffer,
    micro_scratch: Buffer,
    cull_set: vk::DescriptorSet,
    retest_set: vk::DescriptorSet,
    traversal_set: vk::DescriptorSet,
    bin_count_set: vk::DescriptorSet,
    bin_seed_set: vk::DescriptorSet,
    bin_scatter_set: vk::DescriptorSet,
    micro_set: vk::DescriptorSet,
    transparent_keys_set: vk::DescriptorSet,
    radix_histogram_sets: [vk::DescriptorSet; 2],
    radix_scan_set: vk::DescriptorSet,
    radix_scatter_sets: [vk::DescriptorSet; 2],
    transparent_reorder_set: vk::DescriptorSet,
}

/// A view's visibility buffers: per-frame-slot lists plus the cross-frame history.
pub struct SceneVisibilityView {
    frames: Vec<VisibilityFrame>,
    history: Buffer,
    transitions: Buffer,
    transitions_cleared: std::sync::atomic::AtomicBool,
    capacity: u32,
    record_capacity: u32,
    transparent_group_capacity: u32,
}
