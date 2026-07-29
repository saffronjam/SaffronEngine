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

use std::sync::Arc;

use ash::vk;

use crate::descriptors::Descriptors;
use crate::device::Device;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::nested_scopes::NestedScopeRecorder;
use crate::render_graph::{RenderGraph, RgPass, RgResource, RgUsage};
use crate::resources::{Buffer, DeviceResources};
use crate::{Result, checked};

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

/// One CPU-enumerated draw bucket: the dense identity the kernels scatter into and the
/// draw site selects a PSO for.
#[derive(Clone, Copy, Debug)]
pub struct ExecutorBucket {
    /// Registered executor shader index (0 = the übershader).
    pub shader_index: u32,
    /// The bucket's full psoBin (representation + material class + deformation).
    pub pso_bin: u32,
    /// First command slot of the bucket's slice.
    pub base: u32,
    /// Slice capacity in commands.
    pub capacity: u32,
}

/// The executor [`crate::Material`] a draw bucket's identity decodes to (the PSO
/// request key for the bucket's draws).
#[must_use]
pub fn bucket_material(
    shaders: &crate::ExecutorShaderRegistry,
    bucket: ExecutorBucket,
) -> crate::Material {
    let class = crate::GpuMaterialClass::from_bits(
        (bucket.pso_bin >> crate::GPU_PSO_MATERIAL_SHIFT) & 0x3f,
    )
    .unwrap_or_default();
    crate::Material {
        shader: shaders.get(bucket.shader_index).to_owned(),
        unlit: class.unlit(),
        blend: class.transparency() == crate::GpuTransparency::AlphaBlended,
        masked: class.coverage() == saffron_material::AlphaClassification::Masked,
    }
}

/// Builds the frame's dense bucket set from the live (shader, material-class) pairs:
/// each pair expands over both representations (rigid deformation), buckets sort by
/// key, and the command buffer partitions into equal slices. Returns the buckets plus
/// the GPU table bytes (`SceneBucketTable` in `scene_bin_common.slang`).
#[must_use]
pub fn build_executor_buckets(
    live: &[(u32, u32)],
    record_capacity: u32,
) -> (Vec<ExecutorBucket>, Vec<u8>) {
    let mut keyed: Vec<(u32, u32, u32)> = Vec::new();
    for (shader_index, class_bits) in live {
        for representation in [
            crate::GpuRepresentation::TriangleCluster as u32,
            crate::GpuRepresentation::AggregateVoxel as u32,
        ] {
            let pso_bin = (representation << crate::GPU_PSO_REPRESENTATION_SHIFT)
                | (class_bits << crate::GPU_PSO_MATERIAL_SHIFT);
            let key = (shader_index << 16) | (pso_bin & 0xFFFF);
            keyed.push((key, *shader_index, pso_bin));
        }
    }
    keyed.sort_unstable_by_key(|entry| entry.0);
    keyed.dedup_by_key(|entry| entry.0);
    keyed.truncate(SCENE_EXECUTOR_BUCKET_CAPACITY as usize);
    let count = keyed.len() as u32;
    let slice = record_capacity.checked_div(count).unwrap_or(0).max(1);

    let mut buckets = Vec::with_capacity(keyed.len());
    let mut table = vec![0_u8; 16 + 512 * 8 + 512 * 4 + 512 * 4];
    table[0..4].copy_from_slice(&count.to_le_bytes());
    for (bucket, (key, shader_index, pso_bin)) in keyed.into_iter().enumerate() {
        let base = bucket as u32 * slice;
        let capacity = slice.min(record_capacity.saturating_sub(base));
        buckets.push(ExecutorBucket {
            shader_index,
            pso_bin,
            base,
            capacity,
        });
        let lookup = 16 + bucket * 8;
        table[lookup..lookup + 4].copy_from_slice(&key.to_le_bytes());
        table[lookup + 4..lookup + 8].copy_from_slice(&(bucket as u32).to_le_bytes());
        let bases = 16 + 512 * 8 + bucket * 4;
        table[bases..bases + 4].copy_from_slice(&base.to_le_bytes());
        let ends = 16 + 512 * 8 + 512 * 4 + bucket * 4;
        table[ends..ends + 4].copy_from_slice(&(base + capacity).to_le_bytes());
    }
    (buckets, table)
}
/// Counter word: transparent sort pairs (also the transparent draw count).
pub const SCENE_VISIBILITY_COUNTER_TRANSPARENT: usize = 5;
/// Overflow flag: the transparent pair list filled.
pub const SCENE_TRANSPARENT_OVERFLOW: u32 = 8;
/// Counter word: triangles the traversal's emitted records rasterize (indices / 3,
/// survivor records included — they draw again).
pub const SCENE_VISIBILITY_COUNTER_TRIANGLES: usize = 8;
/// Pair elements per radix workgroup.
pub const SCENE_RADIX_WORKGROUP: u32 = 256;

/// The traversal push: the view plus error and capacity dimensions.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SceneTraversalPush {
    /// World → clip for the node cull: a node whose swept world bounds leave this
    /// frustum is rejected along with its whole subtree.
    pub view_proj: [f32; 16],
    /// World-space camera position.
    pub eye: [f32; 3],
    /// Projected pixels per metre at 1 m.
    pub proj_scale: f32,
    /// Refine while the projected appearance error exceeds this many pixels.
    pub error_threshold_px: f32,
    /// Element capacity of the record stream.
    pub record_capacity: u32,
    /// Element capacity of the visible list.
    pub list_capacity: u32,
    /// Nonzero: walk only the retest-survivor tail (counter word 6 base).
    pub survivor: u32,
    /// Nonzero: skip records whose material displaces (the tessellation seam draws
    /// the amplified transient geometry instead).
    pub tess_seam: u32,
    /// Crossfade length in frames for this view; zero disables representation
    /// transitions (the view draws settled cuts and never touches the state table).
    pub transition_frames: u32,
    /// Monotonic frame counter for transition-state staleness and once-per-frame
    /// phase advancement.
    pub frame_stamp: u32,
    /// Debug: pin the hierarchy cut instead of letting projected error choose it.
    /// [`SCENE_CUT_AUTO`], [`SCENE_CUT_FORCE_COARSE`], or [`SCENE_CUT_FORCE_FINE`].
    ///
    /// A representation comparison needs the cut to move while the camera holds still.
    /// Reaching the aggregate form by flying the camera out shrinks the subject at the same
    /// time, so the resulting image difference conflates the two.
    pub representation_override: u32,
    /// Nonzero: reject a node whose swept world bounds leave [`Self::view_proj`],
    /// dropping its subtree with it. Zero walks every node of every visible instance.
    ///
    /// The cull is a pure reduction over provably out-of-view geometry, so a host with
    /// it off renders the identical frame — which is what
    /// `tests/e2e/node-cull-parity.test.ts` asserts, boothing two hosts that differ in
    /// exactly this.
    pub node_cull: u32,
    /// Nonzero: walk for page demand only and emit no draw records.
    pub demand_only: u32,
    /// [`SceneViewClass::ordinal`] of the view this walk serves; rides every
    /// missing-page request so the CPU prices the demand by who missed.
    pub view_class: u32,
}

/// Follow the projected-error threshold (production).
pub const SCENE_CUT_AUTO: u32 = 0;
/// Never refine: the coarsest resident cut, which is where aggregate voxels live.
pub const SCENE_CUT_FORCE_COARSE: u32 = 1;
/// Always refine: the finest resident cut, which is triangle clusters.
pub const SCENE_CUT_FORCE_FINE: u32 = 2;

const _: () = assert!(size_of::<SceneTraversalPush>() == 124);

/// What a view wants from the scene, which is what decides how finely its traversal
/// refines and how urgently the pages it misses are streamed.
///
/// A shadow page and a global-illumination gather both read the same scene as the camera
/// and neither needs it at the camera's fidelity; pricing all three alike either wastes
/// the refinement budget on geometry nobody resolves or lets the cheapest reader evict
/// the pages the image is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneViewClass {
    /// The camera: its cut is the image, so it refines to pixel-scale error.
    Camera,
    /// One shadow-atlas page.
    ShadowPage,
    /// The global-illumination reach view.
    Gi,
}

/// Distinct [`SceneViewClass`] values, for per-class tables.
pub const SCENE_VIEW_CLASSES: usize = 3;

impl SceneViewClass {
    /// Every class, in ordinal order.
    pub const ALL: [Self; SCENE_VIEW_CLASSES] = [Self::Camera, Self::ShadowPage, Self::Gi];

    /// The class's dense index — the value the traversal push carries and every
    /// per-class table is keyed on.
    #[must_use]
    pub fn ordinal(self) -> u32 {
        match self {
            Self::Camera => 0,
            Self::ShadowPage => 1,
            Self::Gi => 2,
        }
    }

    /// The class an ordinal names, or [`Self::Camera`] for a value no class claims — the
    /// only safe reading of a word the GPU wrote into a request record.
    #[must_use]
    pub fn from_ordinal(ordinal: u32) -> Self {
        match ordinal {
            1 => Self::ShadowPage,
            2 => Self::Gi,
            _ => Self::Camera,
        }
    }

    /// The class's bit in an overflow mask.
    #[must_use]
    pub fn bit(self) -> u32 {
        1 << self.ordinal()
    }

    /// The page-demand priority a missing-page request from this class carries.
    ///
    /// A miss is a hole in a read that already happened, so every band sits above the
    /// CPU prioritizer's predicted scores; the bands then order the classes against each
    /// other, which is what keeps a gather from evicting the image.
    #[must_use]
    pub fn page_demand_priority(self) -> u64 {
        match self {
            Self::Camera => u64::MAX / 2,
            Self::ShadowPage => u64::MAX / 4,
            Self::Gi => u64::MAX / 8,
        }
    }
}

/// Refinement threshold for a view whose cut is an image: refine until the projected
/// appearance error is under a pixel.
pub const SCENE_ERROR_THRESHOLD_IMAGE_PX: f32 = 1.0;
/// Refinement threshold for the global-illumination reach view. A gather resolves
/// geometry through a distance field at cascade-voxel scale, so silhouette error a pixel
/// wide is far below anything it can express.
pub const SCENE_ERROR_THRESHOLD_GI_PX: f32 = 16.0;

/// The GI occluder-scatter push: the reach window plus the two capacities.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GiOccluderScatterPush {
    /// Minimum corner of the reach window; `w` unused.
    pub reach_min: [f32; 4],
    /// Maximum corner of the reach window; `w` unused.
    pub reach_max: [f32; 4],
    /// Element capacity of the occluder output region.
    pub capacity: u32,
    /// Element capacity of the reach view's visible list.
    pub list_capacity: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 2],
}

const _: () = assert!(size_of::<GiOccluderScatterPush>() == 48);

/// How one view's hierarchy walk refines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraversalTuning {
    /// Refine while the projected appearance error exceeds this many pixels.
    pub error_threshold_px: f32,
    /// The cut pin: [`SCENE_CUT_AUTO`], [`SCENE_CUT_FORCE_COARSE`], or
    /// [`SCENE_CUT_FORCE_FINE`].
    pub representation_override: u32,
}

/// Texels one directory entry's workgroup covers; a larger tile raises the
/// record-pressure flag rather than silently thinning.
pub const SCENE_MICRO_TEXEL_BUDGET: u32 = 4_096;
/// Entries in the cross-frame representation-transition state table (a power of
/// two; four `u32` words per entry, keyed on the flip node's instance slot + page).
pub const SCENE_TRANSITION_STATE_CAPACITY: u32 = 65_536;
/// Pressure flag: the transition state table had no slot for a flip node.
pub const SCENE_TRANSITION_PRESSURE: u32 = 32;
/// Counter word: records mid representation-crossfade this frame.
pub const SCENE_VISIBILITY_COUNTER_TRANSITIONING: usize = 10;

/// Directory entries the per-tile micro scratch covers; more raises pressure.
pub const SCENE_MICRO_DIRECTORY_CAPACITY: u32 = 1_024;
/// Scratch words before the per-tile count/base tables (emitted total, record base,
/// processed tile count, reserved).
pub const SCENE_MICRO_SCRATCH_HEADER: u32 = 4;
/// Total u32 words in the micro scratch buffer: the header plus one count and one
/// base word per directory slot.
pub const SCENE_MICRO_SCRATCH_WORDS: u64 =
    SCENE_MICRO_SCRATCH_HEADER as u64 + 2 * SCENE_MICRO_DIRECTORY_CAPACITY as u64;

/// The micro-field generation push: the view for per-texel culling plus directory
/// and capacity dimensions.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SceneMicroFieldPush {
    /// World → clip for candidate frustum tests.
    pub view_proj: [f32; 16],
    /// World-space camera position.
    pub eye: [f32; 3],
    /// Candidates beyond this camera distance are skipped (micro is near-field).
    pub max_distance: f32,
    /// The resident-tile directory's byte offset within the fields arena.
    pub directory_offset: u32,
    /// Directory entries.
    pub directory_count: u32,
    /// Element capacity of the record stream.
    pub record_capacity: u32,
    /// Element capacity of one frame slot's candidate region.
    pub candidate_capacity: u32,
    /// The frame slot's first candidate index within the global buffer.
    pub frame_base: u32,
    /// Padding words (match the shader push block).
    pub reserved: [u32; 3],
    /// Wind direction (x, z), speed, and gust fraction for the blade bend bake.
    pub wind_dir_speed_gust: [f32; 4],
    /// Wind roughness, gust frequency, reference height, height exponent.
    pub wind_params: [f32; 4],
    /// Turbulence octave count.
    pub wind_octaves: u32,
    /// Phase seed.
    pub wind_seed: u32,
    /// Simulation seconds this frame.
    pub wind_time_current: f32,
    /// Simulation seconds the previous frame.
    pub wind_time_previous: f32,
    /// Device address of this frame's local wind-source list.
    pub wind_sources: u64,
    /// Sources in the list.
    pub wind_source_count: u32,
    /// Reserved ABI word.
    pub wind_reserved: u32,
}

const _: () = assert!(size_of::<SceneMicroFieldPush>() == 176);

/// The micro-field push byte size for pipeline creation.
pub const SCENE_MICRO_FIELD_PUSH_SIZE: u32 = 176;

/// The byte size of the traversal push.
pub const SCENE_TRAVERSAL_PUSH_SIZE: u32 = size_of::<SceneTraversalPush>() as u32;

/// The visibility push: view matrices plus pyramid and list dimensions.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SceneVisibilityPush {
    /// Current world → clip.
    pub view_proj: [f32; 16],
    /// Previous frame's world → clip.
    pub prev_view_proj: [f32; 16],
    /// HZB mip-0 extent.
    pub hzb_extent: [u32; 2],
    /// HZB mip count.
    pub hzb_mip_count: u32,
    /// [`SCENE_VISIBILITY_PASS_CULL`], [`SCENE_VISIBILITY_PASS_RETEST`], or
    /// [`SCENE_VISIBILITY_PASS_REACH`].
    pub pass_kind: u32,
    /// Whether the previous pyramid and history words are trustworthy.
    pub history_valid: u32,
    /// Element capacity of the visible/retest/history lists.
    pub list_capacity: u32,
    /// Alignment padding ahead of the reach corners, which are 16-byte aligned.
    pub reserved: [u32; 2],
    /// Minimum corner of the reach pass's world box; `w` is unused.
    pub reach_min: [f32; 4],
    /// Maximum corner of the reach pass's world box; `w` is unused.
    pub reach_max: [f32; 4],
}

const _: () = assert!(size_of::<SceneVisibilityPush>() == 192);

/// The byte size of the visibility push.
pub const SCENE_VISIBILITY_PUSH_SIZE: u32 = size_of::<SceneVisibilityPush>() as u32;

/// The wind deformation prepass push: the frame's wind words and the current +
/// previous simulation seconds.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WindDeformPush {
    /// Direction (x, z), speed, and gust fraction.
    pub dir_speed_gust: [f32; 4],
    /// Roughness, gust frequency, reference height, height exponent.
    pub params: [f32; 4],
    /// Turbulence octave count.
    pub octaves: u32,
    /// Phase seed.
    pub seed: u32,
    /// Simulation seconds this frame.
    pub time_current: f32,
    /// Simulation seconds the previous frame.
    pub time_previous: f32,
    /// Device address of this frame's local wind-source list.
    pub sources: u64,
    /// Sources in the list.
    pub source_count: u32,
    /// Reserved ABI word.
    pub reserved: u32,
    /// Cascade-0 centre the previous frame's interaction step integrated, in absolute
    /// world texel coordinates.
    pub prev_center0: [i32; 2],
    /// Cascade-1 centre the previous frame's interaction step integrated.
    pub prev_center1: [i32; 2],
}

const _: () = assert!(size_of::<WindDeformPush>() == 80);

/// The byte size of the wind deformation push.
pub const WIND_DEFORM_PUSH_SIZE: u32 = size_of::<WindDeformPush>() as u32;

/// One world-space interaction impulse: an XZ disc pushing along `direction` and
/// pressing the ground down, with a smooth falloff over `radius`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InteractionImpulse {
    /// World-space XZ centre.
    pub position: [f32; 2],
    /// Falloff radius in metres.
    pub radius: f32,
    /// Velocity change at the centre in metres per second.
    pub strength: f32,
    /// Horizontal push direction (zero = radial from the centre).
    pub direction: [f32; 2],
    /// Ground-depression velocity change at the centre.
    pub depress: f32,
    /// Reserved ABI word.
    pub reserved: f32,
}

const _: () = assert!(size_of::<InteractionImpulse>() == 32);

/// One local wind source as the frame's ring uploads it (mirrors
/// `saffron_wind::LocalWindSource`; kind tags: 0 directional, 1 point, 2 vortex,
/// 3 wake, 4 volume).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuWindSourceRecord {
    /// World position in metres.
    pub position: [f32; 3],
    /// Source kind tag.
    pub kind: u32,
    /// Unit forward axis (directional sources).
    pub direction: [f32; 3],
    /// Peak speed in metres per second (volume: the global scale factor).
    pub strength: f32,
    /// Influence radius in metres.
    pub radius: f32,
    /// Edge-falloff fraction of the radius.
    pub falloff: f32,
    /// Reserved ABI words.
    pub reserved: [f32; 2],
}

const _: () = assert!(size_of::<GpuWindSourceRecord>() == 48);

/// The interaction-field step push: the field and impulse device addresses, the
/// cascade centres, and the frame's clock step.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WindInteractPush {
    /// Device address of the field buffer (header + texel cascades).
    pub field: u64,
    /// Device address of this frame's impulse list.
    pub impulses: u64,
    /// Cascade-0 centre in absolute world texel coordinates.
    pub center0: [i32; 2],
    /// Cascade-1 centre in absolute world texel coordinates.
    pub center1: [i32; 2],
    /// Impulses in the list.
    pub impulse_count: u32,
    /// Simulation seconds since the previous frame.
    pub dt: f32,
    /// Reserved ABI words.
    pub reserved: [u32; 2],
}

const _: () = assert!(size_of::<WindInteractPush>() == 48);

/// The byte size of the interaction-field push.
pub const WIND_INTERACT_PUSH_SIZE: u32 = size_of::<WindInteractPush>() as u32;

/// Device-shared visibility scaffolding: the set layout and the HZB sampler.
pub struct SceneVisibility {
    resources: Arc<DeviceResources>,
    sampler: vk::Sampler,
    layout: vk::DescriptorSetLayout,
    traversal_layout: vk::DescriptorSetLayout,
    bin_count_layout: vk::DescriptorSetLayout,
    bin_seed_layout: vk::DescriptorSetLayout,
    bin_scatter_layout: vk::DescriptorSetLayout,
    executor_layout: vk::DescriptorSetLayout,
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
        let executor_layout = make(
            &[sb, ub, sb],
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::MESH_EXT,
        )?;
        let micro_layout = make(&[sb, sb, ub, sb], compute)?;
        let transparent_keys_layout = make(&[sb, sb, sb, ub], compute)?;
        let radix_histogram_layout = make(&[sb, sb, sb], compute)?;
        let radix_scan_layout = make(&[sb], compute)?;
        let radix_scatter_layout = make(&[sb, sb, sb, sb], compute)?;
        let transparent_reorder_layout = make(&[sb, sb, sb, sb, ub], compute)?;
        Ok(Self {
            resources: Arc::clone(device.resources()),
            sampler,
            layout,
            traversal_layout,
            bin_count_layout,
            bin_seed_layout,
            bin_scatter_layout,
            executor_layout,
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

    /// The executor vertex-stage set layout (records + address block).
    pub fn executor_layout(&self) -> vk::DescriptorSetLayout {
        self.executor_layout
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
            raw.destroy_descriptor_set_layout(self.executor_layout, None);
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

/// Bytes of one `VkDrawMeshTasksIndirectCommandEXT` (three u32 group counts).
pub const MESH_TASK_COMMAND_STRIDE: u64 = 12;

/// Triangles one mesh workgroup emits. Three vertices per triangle are emitted without
/// deduplication — a cluster carries no local vertex table, only a flat index range — so this
/// is bounded by the vertex limit rather than the primitive one: 62 x 3 = 186 output vertices,
/// inside the 256 every supported tier reports, and 2 x 62 covers a full 124-triangle cluster.
pub const MESH_TRIANGLES_PER_GROUP: u32 = 62;

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
    commands: Buffer,
    /// The mesh executor's per-command dispatch arguments, filled by the same scatter that
    /// writes `commands`: one `VkDrawMeshTasksIndirectCommandEXT` per draw, at the same slot.
    mesh_args: Buffer,
    pairs: [Buffer; 2],
    histograms: Buffer,
    transparent_commands: Buffer,
    micro_scratch: Buffer,
    cull_set: vk::DescriptorSet,
    retest_set: vk::DescriptorSet,
    traversal_set: vk::DescriptorSet,
    bin_count_set: vk::DescriptorSet,
    bin_seed_set: vk::DescriptorSet,
    bin_scatter_set: vk::DescriptorSet,
    executor_set: vk::DescriptorSet,
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

impl SceneVisibilityView {
    /// Builds lists for `capacity` instance slots. `transparent_group_capacity` sizes
    /// the sorted transparent stream: one full-length command slice per live blend
    /// bucket (the caller rebuilds the view when more blend buckets go live).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] on buffer or set allocation failure.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        visibility: &SceneVisibility,
        capacity: u32,
        record_capacity: u32,
        transparent_group_capacity: u32,
    ) -> Result<Self> {
        let capacity = capacity.max(1);
        let record_capacity = record_capacity.max(1);
        let transparent_group_capacity = transparent_group_capacity.max(1);
        let list_bytes = u64::from(capacity) * 4;
        let record_bytes = u64::from(record_capacity) * size_of::<crate::GpuDrawRecord>() as u64;
        let storage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC
            // The counters are also reached by DEVICE ADDRESS: the geometry fragment shaders
            // increment the covered-sample word through the scene address block rather than
            // through a descriptor set, so no raster pass needs a binding it otherwise would not.
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let device_local = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let history = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
        let transitions = Buffer::new(
            device.resources(),
            u64::from(SCENE_TRANSITION_STATE_CAPACITY) * 16,
            storage,
            &device_local,
        )?;
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let counters = Buffer::new(
                device.resources(),
                SCENE_VISIBILITY_COUNTER_WORDS * 4,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let readback = Buffer::new(
                device.resources(),
                SCENE_VISIBILITY_COUNTER_WORDS * 4,
                vk::BufferUsageFlags::TRANSFER_DST,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            // SAFETY: HOST_VISIBLE + MAPPED, zeroed before any read.
            unsafe {
                std::ptr::write_bytes(readback.mapped_ptr(), 0, readback.size() as usize);
            }
            let visible = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
            let retest = Buffer::new(device.resources(), list_bytes, storage, &device_local)?;
            let records = Buffer::new(device.resources(), record_bytes, storage, &device_local)?;
            let bin_bytes = u64::from(SCENE_EXECUTOR_BUCKET_CAPACITY) * 4;
            let bin_counts = Buffer::new(
                device.resources(),
                bin_bytes,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let bin_cursors = Buffer::new(device.resources(), bin_bytes, storage, &device_local)?;
            let bucket_table = Buffer::new(
                device.resources(),
                (16 + 512 * 8 + 512 * 4 + 512 * 4) as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            // The table must never be read uninitialized: a garbage liveCount would
            // send the kernels' bucket indices out of bounds.
            // SAFETY: HOST_VISIBLE + MAPPED, zeroed before any submit reads it.
            unsafe {
                std::ptr::write_bytes(bucket_table.mapped_ptr(), 0, bucket_table.size() as usize);
            }
            let commands = Buffer::new(
                device.resources(),
                u64::from(record_capacity) * 20,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            // One `VkDrawMeshTasksIndirectCommandEXT` (three u32s) per draw slot, parallel to
            // `commands`. The scatter knows each draw's triangle count, so it writes the group
            // count directly and no conversion pass is needed.
            let mesh_args = Buffer::new(
                device.resources(),
                u64::from(record_capacity) * MESH_TASK_COMMAND_STRIDE,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let workgroups = record_capacity.div_ceil(SCENE_RADIX_WORKGROUP);
            let pairs = [
                Buffer::new(
                    device.resources(),
                    u64::from(record_capacity) * 8,
                    storage,
                    &device_local,
                )?,
                Buffer::new(
                    device.resources(),
                    u64::from(record_capacity) * 8,
                    storage,
                    &device_local,
                )?,
            ];
            let histograms = Buffer::new(
                device.resources(),
                u64::from(workgroups) * 256 * 4,
                storage,
                &device_local,
            )?;
            let transparent_commands = Buffer::new(
                device.resources(),
                u64::from(transparent_group_capacity) * u64::from(record_capacity) * 20,
                storage | vk::BufferUsageFlags::INDIRECT_BUFFER,
                &device_local,
            )?;
            let micro_scratch = Buffer::new(
                device.resources(),
                SCENE_MICRO_SCRATCH_WORDS * 4,
                storage,
                &device_local,
            )?;
            let cull_set = descriptors.allocate_set(visibility.layout)?;
            let retest_set = descriptors.allocate_set(visibility.layout)?;
            let traversal_set = descriptors.allocate_set(visibility.traversal_layout)?;
            let bin_count_set = descriptors.allocate_set(visibility.bin_count_layout)?;
            let bin_seed_set = descriptors.allocate_set(visibility.bin_seed_layout)?;
            let bin_scatter_set = descriptors.allocate_set(visibility.bin_scatter_layout)?;
            let executor_set = descriptors.allocate_set(visibility.executor_layout)?;
            let micro_set = descriptors.allocate_set(visibility.micro_layout)?;
            let raw = device.raw();
            for set in [cull_set, retest_set] {
                write_storage(raw, set, 0, &counters);
                write_storage(raw, set, 1, &visible);
                write_storage(raw, set, 2, &retest);
                write_storage(raw, set, 3, &history);
            }
            write_storage(raw, traversal_set, 0, &counters);
            write_storage(raw, traversal_set, 1, &visible);
            write_storage(raw, traversal_set, 2, &records);
            write_storage(raw, traversal_set, 4, &transitions);
            write_storage(raw, bin_count_set, 0, &counters);
            write_storage(raw, bin_count_set, 1, &records);
            write_storage(raw, bin_count_set, 2, &bin_counts);
            write_storage(raw, bin_count_set, 3, &bucket_table);
            write_storage(raw, bin_seed_set, 0, &bucket_table);
            write_storage(raw, bin_seed_set, 1, &bin_cursors);
            write_storage(raw, bin_scatter_set, 0, &counters);
            write_storage(raw, bin_scatter_set, 1, &records);
            write_storage(raw, bin_scatter_set, 2, &bin_cursors);
            write_storage(raw, bin_scatter_set, 3, &commands);
            write_storage(raw, bin_scatter_set, 5, &bucket_table);
            write_storage(raw, bin_scatter_set, 6, &mesh_args);
            write_storage(raw, executor_set, 0, &records);
            write_storage(raw, executor_set, 2, &commands);
            write_storage(raw, micro_set, 0, &counters);
            write_storage(raw, micro_set, 1, &records);
            write_storage(raw, micro_set, 3, &micro_scratch);
            let transparent_keys_set =
                descriptors.allocate_set(visibility.transparent_keys_layout)?;
            let radix_histogram_sets = [
                descriptors.allocate_set(visibility.radix_histogram_layout)?,
                descriptors.allocate_set(visibility.radix_histogram_layout)?,
            ];
            let radix_scan_set = descriptors.allocate_set(visibility.radix_scan_layout)?;
            let radix_scatter_sets = [
                descriptors.allocate_set(visibility.radix_scatter_layout)?,
                descriptors.allocate_set(visibility.radix_scatter_layout)?,
            ];
            let transparent_reorder_set =
                descriptors.allocate_set(visibility.transparent_reorder_layout)?;
            write_storage(raw, transparent_keys_set, 0, &counters);
            write_storage(raw, transparent_keys_set, 1, &records);
            write_storage(raw, transparent_keys_set, 2, &pairs[0]);
            for (direction, set) in radix_histogram_sets.iter().enumerate() {
                write_storage(raw, *set, 0, &counters);
                write_storage(raw, *set, 1, &pairs[direction]);
                write_storage(raw, *set, 2, &histograms);
            }
            write_storage(raw, radix_scan_set, 0, &histograms);
            for (direction, set) in radix_scatter_sets.iter().enumerate() {
                write_storage(raw, *set, 0, &counters);
                write_storage(raw, *set, 1, &pairs[direction]);
                write_storage(raw, *set, 2, &pairs[direction ^ 1]);
                write_storage(raw, *set, 3, &histograms);
            }
            write_storage(raw, transparent_reorder_set, 0, &counters);
            write_storage(raw, transparent_reorder_set, 1, &pairs[0]);
            write_storage(raw, transparent_reorder_set, 2, &records);
            write_storage(raw, transparent_reorder_set, 3, &transparent_commands);
            frames.push(VisibilityFrame {
                counters,
                readback,
                visible,
                retest,
                records,
                bin_counts,
                bin_cursors,
                bucket_table,
                commands,
                mesh_args,
                pairs,
                histograms,
                transparent_commands,
                micro_scratch,
                cull_set,
                retest_set,
                traversal_set,
                bin_count_set,
                bin_seed_set,
                bin_scatter_set,
                executor_set,
                micro_set,
                transparent_keys_set,
                radix_histogram_sets,
                radix_scan_set,
                radix_scatter_sets,
                transparent_reorder_set,
            });
        }
        Ok(Self {
            frames,
            history,
            transitions,
            transitions_cleared: std::sync::atomic::AtomicBool::new(false),
            capacity,
            record_capacity,
            transparent_group_capacity,
        })
    }

    /// Element capacity of the semantic record stream.
    pub fn record_capacity(&self) -> u32 {
        self.record_capacity
    }

    /// Allocated blend-bucket slices in the sorted transparent stream.
    pub fn transparent_group_capacity(&self) -> u32 {
        self.transparent_group_capacity
    }

    /// The frame slot's semantic record stream.
    pub fn records(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].records.handle()
    }

    /// Element capacity of the lists.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// The cross-frame per-slot history buffer (cleared on history invalidation).
    pub fn history(&self) -> vk::Buffer {
        self.history.handle()
    }

    /// The frame slot's counters buffer (visible / retest / overflow words).
    pub fn counters(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].counters.handle()
    }

    /// The frame slot's counters buffer as a device address, for shaders with no binding for it.
    pub fn counters_address(&self, device: &Device, frame: usize) -> u64 {
        device.buffer_device_address(self.frames[frame].counters.handle())
    }

    /// The frame slot's visible slot list.
    pub fn visible(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].visible.handle()
    }

    /// The frame slot's retest slot list.
    pub fn retest(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].retest.handle()
    }

    /// Returns the sets to the pool before replacement (capacity growth under an idle
    /// wait).
    pub fn free_sets(&mut self, descriptors: &Descriptors) {
        for frame in &mut self.frames {
            descriptors.free_sets(&[
                frame.cull_set,
                frame.retest_set,
                frame.traversal_set,
                frame.bin_count_set,
                frame.bin_seed_set,
                frame.bin_scatter_set,
                frame.executor_set,
                frame.transparent_keys_set,
                frame.radix_histogram_sets[0],
                frame.radix_histogram_sets[1],
                frame.radix_scan_set,
                frame.radix_scatter_sets[0],
                frame.radix_scatter_sets[1],
                frame.transparent_reorder_set,
            ]);
            frame.cull_set = vk::DescriptorSet::null();
            frame.retest_set = vk::DescriptorSet::null();
            frame.traversal_set = vk::DescriptorSet::null();
            frame.bin_count_set = vk::DescriptorSet::null();
            frame.bin_seed_set = vk::DescriptorSet::null();
            frame.bin_scatter_set = vk::DescriptorSet::null();
            frame.executor_set = vk::DescriptorSet::null();
        }
    }

    /// Writes the frame slot's per-frame bindings: the HZB pyramids (previous for the
    /// cull set, current for the retest set) and the frame's address-block slice.
    pub fn write_frame_bindings(
        &self,
        device: &Device,
        visibility: &SceneVisibility,
        frame: usize,
        previous_hzb: vk::ImageView,
        current_hzb: vk::ImageView,
        address_block: (vk::Buffer, u64, u64),
    ) {
        let raw = device.raw();
        let slot = &self.frames[frame];
        for (set, binding) in [
            (slot.traversal_set, 3),
            (slot.bin_scatter_set, 4),
            (slot.executor_set, 1),
            (slot.micro_set, 2),
            (slot.transparent_keys_set, 3),
            (slot.transparent_reorder_set, 4),
        ] {
            let buffer = [vk::DescriptorBufferInfo {
                buffer: address_block.0,
                offset: address_block.1,
                range: address_block.2,
            }];
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(binding)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&buffer);
            // SAFETY: the ash seam. Written at the fence-waited frame build point.
            unsafe { raw.update_descriptor_sets(&[write], &[]) };
        }
        for (set, view) in [
            (slot.cull_set, previous_hzb),
            (slot.retest_set, current_hzb),
        ] {
            let image = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::GENERAL)
                .sampler(visibility.sampler)];
            let buffer = [vk::DescriptorBufferInfo {
                buffer: address_block.0,
                offset: address_block.1,
                range: address_block.2,
            }];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(4)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&image),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(5)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(&buffer),
            ];
            // SAFETY: the ash seam. Written at the fence-waited frame build point.
            unsafe { raw.update_descriptor_sets(&writes, &[]) };
        }
    }

    /// Records the interaction-field step for `frame`: reset scrolled texels, splat
    /// this frame's impulses, and integrate the damped oscillators — before the
    /// wind prepass and the micro scatter sample the field.
    pub fn add_wind_interact_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        interaction_field: RgResource,
        push: WindInteractPush,
    ) {
        let slot = &self.frames[frame];
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = (crate::GPU_INTERACTION_CASCADES
            * crate::GPU_INTERACTION_TEXELS
            * crate::GPU_INTERACTION_TEXELS)
            .div_ceil(64);
        graph.add_pass(
            RgPass::compute("wind-interact")
                .access(interaction_field, RgUsage::StorageReadWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the wind deformation prepass for `frame`: one dispatch over the
    /// world's instance slots writes each wind-flagged instance's sway record
    /// before the cull and every raster pass read it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_wind_deform_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        wind_records: RgResource,
        interaction_field: RgResource,
        instance_capacity: u32,
        push: WindDeformPush,
    ) {
        let slot = &self.frames[frame];
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = instance_capacity.max(1).div_ceil(64);
        graph.add_pass(
            RgPass::compute("wind-deform")
                .access(wind_records, RgUsage::StorageWriteCompute)
                .access(interaction_field, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the counter clear + cull passes for `frame`. `hzb` is the previous
    /// pyramid resource the cull samples (GENERAL layout); `wind_records` is the
    /// sway-record buffer whose slack the sphere compose reads.
    #[allow(clippy::too_many_arguments)]
    pub fn add_cull_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        hzb: RgResource,
        wind_records: RgResource,
        instance_capacity: u32,
        push: SceneVisibilityPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let commands_res = graph.import_buffer(slot.commands.handle(), None);
        let mesh_args_res = graph.import_buffer(slot.mesh_args.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let bin_counts = slot.bin_counts.handle();
        let commands = slot.commands.handle();
        let mesh_args = slot.mesh_args.handle();
        // The transition state table is cross-frame: zero it exactly once, on the
        // view's first recorded frame, so stale allocations never alias live keys.
        let transitions = (!self
            .transitions_cleared
            .swap(true, std::sync::atomic::Ordering::Relaxed))
        .then(|| self.transitions.handle());
        let mut clear = RgPass::compute("visibility-clear")
            .access(counters_res, RgUsage::TransferWrite)
            .access(bin_counts_res, RgUsage::TransferWrite)
            .access(commands_res, RgUsage::TransferWrite)
            .access(mesh_args_res, RgUsage::TransferWrite);
        if transitions.is_some() {
            let transitions_res = graph.import_buffer(self.transitions.handle(), None);
            clear = clear.access(transitions_res, RgUsage::TransferWrite);
        }
        graph.add_pass(clear.body({
            let raw = raw.clone();
            move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Every filled buffer is TRANSFER_DST.
                unsafe {
                    raw.cmd_fill_buffer(cmd, counters, 0, SCENE_VISIBILITY_COUNTER_WORDS * 4, 0);
                    raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    raw.cmd_fill_buffer(cmd, commands, 0, vk::WHOLE_SIZE, 0);
                    // Unwritten mesh-task slots must dispatch nothing, exactly as unwritten
                    // indexed commands draw nothing.
                    raw.cmd_fill_buffer(cmd, mesh_args, 0, vk::WHOLE_SIZE, 0);
                    if let Some(transitions) = transitions {
                        raw.cmd_fill_buffer(cmd, transitions, 0, vk::WHOLE_SIZE, 0);
                    }
                };
            }
        }));
        let pipeline = Arc::clone(pipeline);
        let set = slot.cull_set;
        let groups = instance_capacity.max(1).div_ceil(64);
        graph.add_pass(
            RgPass::compute("instance-cull")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(hzb, RgUsage::StorageImageRwCompute)
                .access(wind_records, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the retest pass for `frame` after the current pyramid's build. The
    /// dispatch covers the whole list capacity; the shader bounds itself by the retest
    /// counter.
    #[allow(clippy::too_many_arguments)]
    pub fn add_retest_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        hzb: RgResource,
        wind_records: RgResource,
        push: SceneVisibilityPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.retest_set;
        let groups = self.capacity.div_ceil(64);
        graph.add_pass(
            RgPass::compute("instance-retest")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(hzb, RgUsage::StorageImageRwCompute)
                .access(wind_records, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_dispatch(&raw, cmd, &pipeline, set, push, groups);
                }),
        );
    }

    /// Records the traversal pass for `frame` over the merged visible list: refine by
    /// projected appearance error where children are resident, request missing pages,
    /// and emit the semantic record stream.
    pub fn add_traversal_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipeline: &Arc<crate::Pipeline>,
        frame: usize,
        push: SceneTraversalPush,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let transitions_res = graph.import_buffer(self.transitions.handle(), None);
        let raw = device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let set = slot.traversal_set;
        let groups = self.capacity.div_ceil(64);
        graph.add_pass(
            RgPass::compute("scene-traversal")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(records_res, RgUsage::StorageWriteCompute)
                .access(transitions_res, RgUsage::StorageReadWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the push
                    // spans the declared range; the dispatch covers the list capacity.
                    unsafe {
                        raw.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            pipeline.handle(),
                        );
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
                        raw.cmd_dispatch(cmd, groups, 1, 1);
                    }
                }),
        );
    }

    /// Records the micro-field count → scan → scatter chain for `frame`: count the
    /// post-cull blade survivors per resident tile, scan the counts into exact
    /// exclusive bases under the frame's candidate and record budgets, then scatter
    /// each survivor's candidate and [`crate::GpuDrawRecord`] to its exact slot —
    /// no atomics order the stream, so record order is stable frame to frame.
    #[allow(clippy::too_many_arguments)]
    pub fn add_micro_field_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        frame: usize,
        candidates: vk::Buffer,
        interaction_field: RgResource,
        push: SceneMicroFieldPush,
    ) {
        let (count, scan, scatter) = pipelines;
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let candidates_res = graph.import_buffer(candidates, None);
        let scratch_res = graph.import_buffer(slot.micro_scratch.handle(), None);
        let raw = device.raw().clone();
        let set = slot.micro_set;
        // One workgroup per directory entry; its threads stride the tile's texels.
        let tile_groups = push
            .directory_count
            .clamp(1, SCENE_MICRO_DIRECTORY_CAPACITY);

        let record_micro_dispatch = move |cmd: vk::CommandBuffer,
                                          raw: &ash::Device,
                                          pipeline: &crate::Pipeline,
                                          groups: (u32, u32)| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the push
            // spans the declared range; the dispatch covers the declared groups.
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
                raw.cmd_push_constants(
                    cmd,
                    pipeline.layout(),
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                raw.cmd_dispatch(cmd, groups.0, groups.1, 1);
            }
        };

        let pipeline = Arc::clone(count);
        graph.add_pass(
            RgPass::compute("scene-micro-count")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(scratch_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_micro_dispatch(cmd, &raw, &pipeline, (1, tile_groups));
                    }
                }),
        );
        let pipeline = Arc::clone(scan);
        graph.add_pass(
            RgPass::compute("scene-micro-scan")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(scratch_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_micro_dispatch(cmd, &raw, &pipeline, (1, 1));
                    }
                }),
        );
        let pipeline = Arc::clone(scatter);
        graph.add_pass(
            RgPass::compute("scene-micro-scatter")
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(records_res, RgUsage::StorageReadWriteCompute)
                .access(candidates_res, RgUsage::StorageWriteCompute)
                .access(scratch_res, RgUsage::StorageReadCompute)
                .access(interaction_field, RgUsage::ShaderDeviceAddressRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_micro_dispatch(cmd, &raw, &pipeline, (1, tile_groups));
                }),
        );
    }

    /// Records the three binning passes for `frame`: per-bin counts, the exclusive
    /// scan, and the scatter that builds the indirect command stream.
    pub fn add_binning_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        frame: usize,
        survivor: bool,
        micro_template: (u32, u32),
    ) {
        let (count, seed, scatter) = pipelines;
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let records_res = graph.import_buffer(slot.records.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let bin_cursors_res = graph.import_buffer(slot.bin_cursors.handle(), None);
        let commands_res = graph.import_buffer(slot.commands.handle(), None);
        let raw = device.raw().clone();
        // Words 2-3: the shared blade-template index count + first index for
        // micro-blade commands.
        let push = [
            self.record_capacity,
            u32::from(survivor),
            micro_template.0,
            micro_template.1,
        ];
        let groups = self.record_capacity.div_ceil(64);

        let pipeline = Arc::clone(count);
        let set = slot.bin_count_set;
        graph.add_pass(
            RgPass::compute("scene-bucket-count")
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(records_res, RgUsage::StorageReadCompute)
                .access(bin_counts_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&push)),
                            groups,
                        );
                    }
                }),
        );
        let pipeline = Arc::clone(seed);
        let set = slot.bin_seed_set;
        graph.add_pass(
            RgPass::compute("scene-bucket-seed")
                .access(bin_cursors_res, RgUsage::StorageReadWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            None,
                            SCENE_EXECUTOR_BUCKET_CAPACITY.div_ceil(64),
                        );
                    }
                }),
        );
        let pipeline = Arc::clone(scatter);
        let set = slot.bin_scatter_set;
        graph.add_pass(
            RgPass::compute("scene-bucket-scatter")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(records_res, RgUsage::StorageReadCompute)
                .access(bin_cursors_res, RgUsage::StorageReadWriteCompute)
                .access(commands_res, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_binning_dispatch(
                        &raw,
                        cmd,
                        &pipeline,
                        set,
                        Some(bytemuck::cast_slice(&push)),
                        groups,
                    );
                }),
        );
    }

    /// The value captures a graphics pass body needs to issue the executor draw.
    #[must_use]
    pub fn executor_draw_inputs(&self, frame: usize, draw_records: u32) -> ExecutorDrawInputs {
        let slot = &self.frames[frame];
        ExecutorDrawInputs {
            executor_set: slot.executor_set,
            commands: slot.commands.handle(),
            mesh_args: slot.mesh_args.handle(),
            counters: slot.counters.handle(),
            bucket_counts: slot.bin_counts.handle(),
            record_capacity: self.record_capacity,
            draw_bound: draw_records.clamp(1, self.record_capacity),
        }
    }

    /// The frame slot's indirect command stream (for graph usage declarations).
    pub fn mesh_args(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].mesh_args.handle()
    }

    /// The frame slot's indexed indirect command stream.
    pub fn commands(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].commands.handle()
    }

    /// The frame slot's per-bucket record counts (the draws' indirect count buffer).
    pub fn bucket_counts(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].bin_counts.handle()
    }

    /// Publishes the frame's bucket table (`build_executor_buckets` bytes).
    pub fn write_bucket_table(&self, frame: usize, table: &[u8]) {
        let buffer = &self.frames[frame].bucket_table;
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's prior GPU reads completed with its
        // fence before this frame reused the slot.
        unsafe {
            std::ptr::copy_nonoverlapping(
                table.as_ptr(),
                buffer.mapped_ptr(),
                table.len().min(buffer.size() as usize),
            );
        }
    }

    /// Records the provisional-count snapshot: copies the visible and record counts
    /// into counter words 6/7 and clears the bucket counts, so the survivor
    /// traversal/binning process only the retest tail. Record after the provisional
    /// raster and before the retest pass.
    pub fn add_survivor_snapshot_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let bin_counts = slot.bin_counts.handle();
        graph.add_pass(
            RgPass::compute("survivor-snapshot")
                .access(counters_res, RgUsage::TransferWrite)
                .access(bin_counts_res, RgUsage::TransferWrite)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Word copies within the counters buffer +
                    // the bucket-count clear, ordered by the graph's transfer usage.
                    unsafe {
                        raw.cmd_copy_buffer(
                            cmd,
                            counters,
                            counters,
                            &[
                                vk::BufferCopy {
                                    src_offset: 0,
                                    dst_offset: 24,
                                    size: 4,
                                },
                                vk::BufferCopy {
                                    src_offset: 12,
                                    dst_offset: 28,
                                    size: 4,
                                },
                            ],
                        );
                        raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    }
                }),
        );
    }

    /// Records a bucket-count clear (fill 0) — the prologue for a fresh binning run
    /// over an already-drawn command stream (the full re-bin after the survivor
    /// raster restores the complete cut for later consumers).
    pub fn add_bucket_count_clear_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let bin_counts_res = graph.import_buffer(slot.bin_counts.handle(), None);
        let raw = device.raw().clone();
        let bin_counts = slot.bin_counts.handle();
        graph.add_pass(
            RgPass::compute("bucket-count-clear")
                .access(bin_counts_res, RgUsage::TransferWrite)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The clear is ordered by the graph's
                    // transfer usage against every earlier count read.
                    unsafe {
                        raw.cmd_fill_buffer(cmd, bin_counts, 0, vk::WHOLE_SIZE, 0);
                    }
                }),
        );
    }

    /// Records the counters → readback copy; the CPU folds the slot's words into
    /// stats once its fence completes.
    pub fn add_counters_readback_pass(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        frame: usize,
    ) {
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let raw = device.raw().clone();
        let counters = slot.counters.handle();
        let readback = slot.readback.handle();
        graph.add_pass(
            RgPass::compute("visibility-readback")
                .access(counters_res, RgUsage::TransferRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Both buffers are valid this frame; the
                    // graph ordered the copy after the chain's writes.
                    unsafe {
                        raw.cmd_copy_buffer(
                            cmd,
                            counters,
                            readback,
                            &[vk::BufferCopy {
                                src_offset: 0,
                                dst_offset: 0,
                                size: SCENE_VISIBILITY_COUNTER_WORDS * 4,
                            }],
                        );
                    }
                }),
        );
    }

    /// The fence-completed slot's counter words (visible/retest/overflow/records/
    /// record-pressure/transparent).
    #[must_use]
    pub fn read_counters(&self, frame: usize) -> [u32; SCENE_VISIBILITY_COUNTER_WORDS as usize] {
        let slot = &self.frames[frame];
        let mut words = [0_u32; SCENE_VISIBILITY_COUNTER_WORDS as usize];
        // SAFETY: HOST_VISIBLE + MAPPED; the slot's copy completed with its fence
        // before this frame reused the slot.
        unsafe {
            std::ptr::copy_nonoverlapping(
                slot.readback.mapped_ptr(),
                words.as_mut_ptr().cast::<u8>(),
                words.len() * 4,
            );
        }
        words
    }

    /// The frame slot's back-to-front transparent command stream (counter word 5 is
    /// its draw count).
    pub fn transparent_commands(&self, frame: usize) -> vk::Buffer {
        self.frames[frame].transparent_commands.handle()
    }

    /// Records the transparent back-to-front sort for `frame`: key collection, four
    /// stable LSD radix passes (histogram → scan → scatter, ping-ponging the pair
    /// buffers), and the reverse-order command reorder into the transparent stream.
    /// `view_row2` is the camera view matrix's third row (view-space depth).
    pub fn add_transparent_sort_passes(
        &self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: TransparentSortPipelines<'_>,
        frame: usize,
        view_row2: [f32; 4],
        blend_bucket_keys: &[u32],
    ) {
        debug_assert!(
            blend_bucket_keys.len() as u32 <= self.transparent_group_capacity,
            "blend buckets exceed the allocated transparent stream slices"
        );
        let slot = &self.frames[frame];
        let counters_res = graph.import_buffer(slot.counters.handle(), None);
        let pairs_res = [
            graph.import_buffer(slot.pairs[0].handle(), None),
            graph.import_buffer(slot.pairs[1].handle(), None),
        ];
        let histograms_res = graph.import_buffer(slot.histograms.handle(), None);
        let transparent_res = graph.import_buffer(slot.transparent_commands.handle(), None);
        let raw = device.raw().clone();
        let workgroups = self.record_capacity.div_ceil(SCENE_RADIX_WORKGROUP);
        let record_groups = self.record_capacity.div_ceil(64);

        let mut keys_push = [0_u32; 8];
        keys_push[..4].copy_from_slice(bytemuck::cast_slice(&view_row2));
        keys_push[4] = self.record_capacity;
        let pipeline = Arc::clone(pipelines.keys);
        let set = slot.transparent_keys_set;
        graph.add_pass(
            RgPass::compute("transparent-keys")
                .access(counters_res, RgUsage::StorageReadWriteCompute)
                .access(pairs_res[0], RgUsage::StorageWriteCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&keys_push)),
                            record_groups,
                        );
                    }
                }),
        );

        for pass in 0..4_u32 {
            let direction = (pass % 2) as usize;
            let shift_push = [pass * 8, self.record_capacity, workgroups, 0];
            let pipeline = Arc::clone(pipelines.histogram);
            let set = slot.radix_histogram_sets[direction];
            graph.add_pass(
                RgPass::compute("radix-histogram")
                    .access(pairs_res[direction], RgUsage::StorageReadCompute)
                    .access(histograms_res, RgUsage::StorageReadWriteCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&shift_push)),
                                workgroups,
                            );
                        }
                    }),
            );
            let pipeline = Arc::clone(pipelines.scan);
            let set = slot.radix_scan_set;
            let entries = workgroups * 256;
            graph.add_pass(
                RgPass::compute("radix-scan")
                    .access(histograms_res, RgUsage::StorageReadWriteCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&[entries, workgroups, 0, 0])),
                                1,
                            );
                        }
                    }),
            );
            let pipeline = Arc::clone(pipelines.scatter);
            let set = slot.radix_scatter_sets[direction];
            graph.add_pass(
                RgPass::compute("radix-scatter")
                    .access(pairs_res[direction], RgUsage::StorageReadCompute)
                    .access(pairs_res[direction ^ 1], RgUsage::StorageWriteCompute)
                    .access(histograms_res, RgUsage::StorageReadCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            record_binning_dispatch(
                                &raw,
                                cmd,
                                &pipeline,
                                set,
                                Some(bytemuck::cast_slice(&shift_push)),
                                workgroups,
                            );
                        }
                    }),
            );
        }

        // One reorder dispatch per live blend bucket: each writes that bucket's
        // full-length command slice (zero-masking other buckets' pairs), so every
        // blend PSO replays the whole back-to-front order. The dispatches write
        // disjoint slices — no barrier between them.
        let pipeline = Arc::clone(pipelines.reorder);
        let set = slot.transparent_reorder_set;
        let reorder_capacity = self.record_capacity;
        let groups: Vec<u32> = blend_bucket_keys.to_vec();
        graph.add_pass(
            RgPass::compute("transparent-reorder")
                .access(pairs_res[0], RgUsage::StorageReadCompute)
                .access(counters_res, RgUsage::StorageReadCompute)
                .access(transparent_res, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    for (group_slot, key) in groups.iter().enumerate() {
                        record_binning_dispatch(
                            &raw,
                            cmd,
                            &pipeline,
                            set,
                            Some(bytemuck::cast_slice(&[
                                reorder_capacity,
                                *key,
                                group_slot as u32 * reorder_capacity,
                                0,
                            ])),
                            record_groups,
                        );
                    }
                }),
        );
    }
}

fn record_binning_dispatch(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: Option<&[u8]>,
    groups: u32,
) {
    // SAFETY: the ash seam. The PSO/set are valid this frame; any push spans the
    // declared range; the dispatch covers the record capacity or the bin table.
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
        if let Some(push) = push {
            raw.cmd_push_constants(
                cmd,
                pipeline.layout(),
                vk::ShaderStageFlags::COMPUTE,
                0,
                push,
            );
        }
        raw.cmd_dispatch(cmd, groups, 1, 1);
    }
}

/// The five transparent-sort pipelines, borrowed for one record call.
pub struct TransparentSortPipelines<'a> {
    /// Key collection over the record stream.
    pub keys: &'a Arc<crate::Pipeline>,
    /// Per-workgroup radix histograms.
    pub histogram: &'a Arc<crate::Pipeline>,
    /// The global exclusive scan over the histogram table.
    pub scan: &'a Arc<crate::Pipeline>,
    /// The stable radix scatter.
    pub scatter: &'a Arc<crate::Pipeline>,
    /// The reverse-order command reorder.
    pub reorder: &'a Arc<crate::Pipeline>,
}

/// Value captures for [`record_executor_draw`] inside a `'static` pass body.
#[derive(Clone, Copy)]
pub struct ExecutorDrawInputs {
    /// The frame slot's executor set (records + address block).
    pub executor_set: vk::DescriptorSet,
    /// The frame slot's indirect command stream.
    pub commands: vk::Buffer,
    /// The frame slot's mesh-task dispatch arguments, parallel to `commands`.
    pub mesh_args: vk::Buffer,
    /// The frame slot's counters buffer (overflow/pressure words).
    pub counters: vk::Buffer,
    /// The frame slot's per-bucket count buffer (the draws' indirect counts).
    pub bucket_counts: vk::Buffer,
    /// Element capacity of the command stream.
    pub record_capacity: u32,
    /// Upper bound on the draws any one bucket slice can hold: the mirror's live draw-record
    /// count, clamped to the slice capacity.
    ///
    /// A device with `drawIndirectCount` reads the exact count on the GPU and treats this as a
    /// ceiling. A device without it must name a count host-side, and the slice capacity is the
    /// wrong one: it issues one command per *slot*, tens of thousands of no-op draws per frame
    /// whatever is on screen. Bounding by the records that actually exist keeps the pass
    /// proportional to the scene while never asking for fewer draws than the GPU wrote.
    pub draw_bound: u32,
}

/// Which bucket a recorder draws, where its count word sits, and whether the device can read
/// that count on the GPU. Shared by both executors so the two draw calls stay the same shape.
#[derive(Clone, Copy)]
pub struct ExecutorBucketDraw {
    /// The bucket's command-slice base and capacity.
    pub bucket: ExecutorBucket,
    /// Its index into the per-bucket count buffer.
    pub index: u32,
    /// Whether `drawIndirectCount` is available; without it the draw count is named host-side.
    pub draw_indirect_count: bool,
}

/// Issues one draw bucket's counted indirect draw into a live graphics pass body. The
/// caller binds the mesh set roster, the push, and the pages-arena index buffer once
/// per pass ([`record_executor_pass_prefix`]); each bucket then binds its PSO and
/// draws its command slice with the bucket's count word (or the fixed-slice variant
/// when the device lacks `drawIndirectCount`; unwritten commands are zero-filled
/// no-ops).
pub fn record_executor_bucket_draw(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    inputs: ExecutorDrawInputs,
    draw: ExecutorBucketDraw,
) {
    let ExecutorBucketDraw {
        bucket,
        index: bucket_index,
        draw_indirect_count,
    } = draw;
    // SAFETY: the ash seam. The PSO/buffers are valid this frame; the indirect stream,
    // counts, and slice bases were built by the bucket passes this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        let draws = bucket.capacity.min(inputs.draw_bound);
        if draw_indirect_count {
            raw.cmd_draw_indexed_indirect_count(
                cmd,
                inputs.commands,
                u64::from(bucket.base) * 20,
                inputs.bucket_counts,
                u64::from(bucket_index) * 4,
                draws,
                20,
            );
        } else {
            raw.cmd_draw_indexed_indirect(
                cmd,
                inputs.commands,
                u64::from(bucket.base) * 20,
                draws,
                20,
            );
        }
    }
}

/// Issues one draw bucket's counted mesh-task dispatch — the mesh executor's counterpart to
/// [`record_executor_bucket_draw`], reading the *same* per-bucket count word against the
/// mesh-args stream the scatter filled beside the indexed commands. One workgroup covers
/// [`MESH_TRIANGLES_PER_GROUP`] triangles of one draw; the shader recovers which draw from
/// `DrawIndex` and which block from its group id.
pub fn record_executor_bucket_draw_mesh(
    raw: &ash::Device,
    dispatch: &ash::ext::mesh_shader::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    inputs: ExecutorDrawInputs,
    draw: ExecutorBucketDraw,
) {
    let ExecutorBucketDraw {
        bucket,
        index: bucket_index,
        draw_indirect_count,
    } = draw;
    // SAFETY: the ash seam. The PSO/buffers are valid this frame; the mesh-args stream, counts,
    // and slice bases were written by the bucket passes this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        let draws = bucket.capacity.min(inputs.draw_bound);
        if draw_indirect_count {
            dispatch.cmd_draw_mesh_tasks_indirect_count(
                cmd,
                inputs.mesh_args,
                u64::from(bucket.base) * MESH_TASK_COMMAND_STRIDE,
                inputs.bucket_counts,
                u64::from(bucket_index) * 4,
                draws,
                MESH_TASK_COMMAND_STRIDE as u32,
            );
        } else {
            dispatch.cmd_draw_mesh_tasks_indirect(
                cmd,
                inputs.mesh_args,
                u64::from(bucket.base) * MESH_TASK_COMMAND_STRIDE,
                draws,
                MESH_TASK_COMMAND_STRIDE as u32,
            );
        }
    }
}

/// Binds the executor set, the viewProj push, and the pages-arena index buffer — the
/// shared prefix every bucket draw in a pass rides on.
pub fn record_executor_pass_prefix(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    inputs: ExecutorDrawInputs,
    page_index_buffer: vk::Buffer,
    view_proj: [f32; 16],
) {
    // SAFETY: the ash seam. The set/buffers are valid this frame.
    unsafe {
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            0,
            &[inputs.executor_set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            bytemuck::bytes_of(&view_proj),
        );
        raw.cmd_bind_index_buffer(cmd, page_index_buffer, 0, vk::IndexType::UINT32);
    }
}

/// Binds the executor set and the mesh executor's push — the view-projection plus the bucket's
/// command-slice base, which `SV_DrawIndex` is relative to. No index buffer: the mesh stage
/// reads indices through the address block rather than through a bound stream.
pub fn record_executor_mesh_prefix(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    inputs: ExecutorDrawInputs,
    view_proj: [f32; 16],
    slice_base: u32,
) {
    let mut push = [0_u8; crate::MESH_EXECUTOR_PUSH_SIZE as usize];
    push[..64].copy_from_slice(bytemuck::bytes_of(&view_proj));
    push[64..].copy_from_slice(&slice_base.to_ne_bytes());
    // SAFETY: the ash seam. The set is valid this frame; the push spans the declared range.
    unsafe {
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            0,
            &[inputs.executor_set],
            &[],
        );
        raw.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::MESH_EXT, 0, &push);
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

fn write_storage(raw: &ash::Device, set: vk::DescriptorSet, binding: u32, buffer: &Buffer) {
    let info = [vk::DescriptorBufferInfo {
        buffer: buffer.handle(),
        offset: 0,
        range: buffer.size(),
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set and buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

fn record_dispatch<P: bytemuck::Pod>(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: P,
    groups: u32,
) {
    // SAFETY: the ash seam. The PSO/set are valid this frame; the push spans the
    // declared range; the dispatch covers the slot capacity.
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
        raw.cmd_push_constants(
            cmd,
            pipeline.layout(),
            vk::ShaderStageFlags::COMPUTE,
            0,
            bytemuck::bytes_of(&push),
        );
        raw.cmd_dispatch(cmd, groups, 1, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_gpu_data::{GlobalGpuData, GpuHandle};
    use crate::gpu_scene_upload::GpuSceneUploader;
    use crate::persistent_gpu_scene::GpuSceneWorldId;
    use crate::persistent_gpu_scene::{
        GpuSceneDynamicTransform, GpuSceneMaterialRecord, GpuScenePageRecord, GpuSceneSharedDelta,
        GpuSceneSharedDeltaResult, GpuSceneTransform, GpuSceneUploadLimits, GpuSceneWorldDelta,
        GpuSceneWorldDeltaResult, PersistentGpuScene,
    };
    use crate::resources::{BindlessFreeList, Image, ImageDesc};
    use crate::{
        Device, GpuSceneInstanceRecord, GpuScenePrototypeRecord, Pipelines, SurfaceSource,
        validation_issue_count,
    };
    use saffron_geometry::glam::{Mat4, Vec3};
    use std::sync::Mutex;

    const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

    /// A device-local wind sway record buffer sized for the tests' 64-slot views.
    fn wind_records_buffer(device: &Device) -> Buffer {
        Buffer::new(
            device.resources(),
            64 * size_of::<crate::GpuWindInstanceRecord>() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            },
        )
        .expect("wind records buffer")
    }

    fn one_shot<F: FnOnce(vk::CommandBuffer)>(device: &Device, record: F) {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Everything is destroyed after the fence wait.
        unsafe {
            let pool = raw.create_command_pool(&pool_info, None).expect("pool");
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = raw.allocate_command_buffers(&alloc).expect("cmd")[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("fence");
            raw.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("begin");
            record(cmd);
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "visibility test")
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
    }

    fn read_words(device: &Device, buffer: vk::Buffer, words: usize) -> Vec<u32> {
        let staging = Buffer::new(
            device.resources(),
            (words * 4) as u64,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("staging");
        let raw = device.raw().clone();
        one_shot(device, |cmd| {
            let barrier = vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
            let barriers = [barrier];
            // SAFETY: the ash seam. One-off copy under the fence below.
            unsafe {
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().memory_barriers(&barriers),
                );
                raw.cmd_copy_buffer(
                    cmd,
                    buffer,
                    staging.handle(),
                    &[vk::BufferCopy {
                        src_offset: 0,
                        dst_offset: 0,
                        size: (words * 4) as u64,
                    }],
                );
            }
        });
        let mut out = vec![0_u8; words * 4];
        // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
        unsafe {
            std::ptr::copy_nonoverlapping(staging.mapped_ptr(), out.as_mut_ptr(), out.len());
        }
        out.chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect()
    }

    fn clear_image(device: &Device, image: vk::Image, layout_from_undefined: bool, value: f32) {
        let raw = device.raw().clone();
        one_shot(device, |cmd| {
            let old_layout = if layout_from_undefined {
                vk::ImageLayout::UNDEFINED
            } else {
                vk::ImageLayout::GENERAL
            };
            let barrier = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let barriers = [barrier];
            // SAFETY: the ash seam. One-off clear under the fence below.
            unsafe {
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&barriers),
                );
                raw.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [value, value, value, value],
                    },
                    &[vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: 1,
                    }],
                );
                let back = vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .dst_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                let backs = [back];
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&backs),
                );
            }
        });
    }

    fn hzb_image(device: &Device, value: f32) -> Image {
        let image = Image::new(
            device.resources(),
            &ImageDesc {
                extent: vk::Extent2D {
                    width: 64,
                    height: 64,
                },
                format: vk::Format::R32_SFLOAT,
                usage: vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_DST,
                aspect: vk::ImageAspectFlags::COLOR,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 7,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )
        .expect("hzb image");
        clear_image(device, image.handle(), true, value);
        image
    }

    fn create_instance(
        gpu_scene: &mut PersistentGpuScene,
        prototype: crate::GpuScenePrototypeHandle,
        translation: Vec3,
    ) -> GpuHandle {
        let transform =
            GpuSceneDynamicTransform::new(Mat4::from_translation(translation), Mat4::IDENTITY)
                .expect("transform");
        match gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateInstance(GpuSceneInstanceRecord {
                    prototype,
                    transform: GpuSceneTransform::Dynamic(transform),
                    material_overrides: std::sync::Arc::from([]),
                    deformation: None,
                    source_generation: 1,
                    flags: 0,
                    combination: 0,
                    vegetation: None,
                }),
            )
            .expect("instance")
        {
            GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle.raw(),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn cooked_quad() -> saffron_geometry::PortableVirtualHierarchy {
        use saffron_geometry::glam::{Vec2, Vec3 as GVec3};
        use saffron_geometry::{Mesh, PortableHierarchyInput, Submesh, Vertex};
        let vert = |x: f32, y: f32| Vertex {
            position: GVec3::new(x, y, 0.0),
            normal: GVec3::Z,
            uv0: Vec2::new(x, y),
            ..Default::default()
        };
        let mesh = Mesh {
            vertices: vec![
                vert(0.0, 0.0),
                vert(1.0, 0.0),
                vert(0.0, 1.0),
                vert(1.0, 1.0),
            ],
            indices: vec![0, 1, 2, 1, 3, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let input = PortableHierarchyInput::from_mesh(&mesh, &[]).expect("input");
        saffron_geometry::cook_portable_virtual_hierarchy(&input).expect("cook")
    }

    #[test]
    fn traversal_emits_cut_records_and_requests_missing_children() {
        use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
        use crate::page_residency::{PageResidency, PageResidencyBudgets};
        use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord, GpuRepresentation};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let visibility = SceneVisibility::new(&device).expect("visibility");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let cull = pipelines
                .request_scene_visibility(visibility.layout())
                .expect("cull pso");
            let traversal = pipelines
                .request_scene_traversal(visibility.traversal_layout())
                .expect("traversal pso");

            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut pending = GpuScenePendingUploads::default();
            let mut residency = PageResidency::new(PageResidencyBudgets::default());
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            // Resident material (for the psoBin material class) and the cooked page
            // hierarchy's resident page records.
            let resident_material = gpu_data
                .materials
                .insert(GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: GpuHandle::INVALID,
                    parameter_index: 0,
                    material_class: crate::GpuMaterialClass::new(
                        saffron_material::AlphaClassification::Masked,
                        crate::GpuSidedness::Single,
                        saffron_material::SurfaceModel::Standard,
                        crate::GpuTransparency::Opaque,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("resident material");
            pending.stage_record(GlobalGpuTableKind::Material, resident_material);

            let hierarchy = cooked_quad();
            let mut device_pages = Vec::new();
            for page in &hierarchy.pages {
                let parent = page
                    .dependency
                    .map(|dependency| device_pages[dependency as usize]);
                let handle = gpu_data
                    .page_table
                    .insert(GpuPageRecord {
                        parent: parent.unwrap_or(GpuHandle::INVALID),
                        dependencies: crate::GpuArenaRange::default(),
                        byte_offset: 0,
                        byte_length: 0,
                        resident_generation: 0,
                        flags: if page.guaranteed_root {
                            crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                        } else {
                            0
                        },
                        reserved: 0,
                    })
                    .expect("page record");
                pending.stage_record(GlobalGpuTableKind::Page, handle);
                residency.register_page(handle, parent, page.guaranteed_root);
                device_pages.push(handle);
            }
            let root_cook = hierarchy
                .pages
                .iter()
                .find(|page| page.guaranteed_root)
                .expect("root page")
                .id;
            let root_handle = device_pages[root_cook as usize];

            // Publish ONLY the root payload; its children stay unresident.
            let mut payload =
                crate::build_page_payload(&hierarchy, root_cook).expect("root payload");
            for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                payload
                    .patch_child(index, device_pages[*cook_child as usize])
                    .expect("patch");
            }
            for handle in residency.take_load_requests(16) {
                if handle == root_handle {
                    residency.complete_load(handle, payload.bytes.clone());
                }
            }
            residency
                .publish_ready(&mut gpu_data, &mut pending)
                .expect("publish root");

            // Scene chain: material -> prototype (root page) -> one instance at origin.
            let material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: resident_material,
                        source_revision: 1,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let scene_root = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: root_handle,
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("scene page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    GpuScenePrototypeRecord {
                        geometry: GpuHandle {
                            index: 3,
                            generation: 1,
                        },
                        materials: std::sync::Arc::from([material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: scene_root,
                        bounds: [0.0, 0.0, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let front = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

            gpu_data.begin_frame(0).expect("gpu data");
            uploader.begin_frame(0).expect("uploader");
            gpu_scene.begin_frame(0).expect("scene");
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain pending");
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            let address_ubo = Buffer::new(
                device.resources(),
                size_of::<crate::GpuSceneAddressBlock>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("address ubo");
            // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&block).as_ptr(),
                    address_ubo.mapped_ptr(),
                    size_of::<crate::GpuSceneAddressBlock>(),
                );
            }
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );

            let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            let open = hzb_image(&device, 1.0);
            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
                * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

            // Cull + traverse with a zero threshold: the root wants to refine, its
            // children are missing, so it emits itself and requests every child.
            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &cull,
                0,
                hzb_res,
                wind_records_res,
                64,
                SceneVisibilityPush {
                    view_proj: view_proj.to_cols_array(),
                    prev_view_proj: view_proj.to_cols_array(),
                    hzb_extent: [64, 64],
                    hzb_mip_count: 7,
                    pass_kind: SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: 64,
                    reserved: [0; 2],
                    reach_min: [0.0; 4],
                    reach_max: [0.0; 4],
                },
            );
            view.add_traversal_pass(
                &device,
                &mut graph,
                &traversal,
                0,
                SceneTraversalPush {
                    view_proj: view_proj.to_cols_array(),
                    eye: [0.0, 0.0, 5.0],
                    proj_scale: 1000.0,
                    error_threshold_px: 0.0,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    tess_seam: 0,
                    transition_frames: 0,
                    frame_stamp: 0,
                    representation_override: SCENE_CUT_AUTO,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let counters = read_words(&device, view.counters(0), 8);
            assert_eq!(counters[0], 1, "the instance is visible");
            assert_eq!(counters[3], 1, "the resident root emits one record");
            assert_eq!(counters[4], 0, "no record overflow");
            let record_words = read_words(
                &device,
                view.records(0),
                size_of::<crate::GpuDrawRecord>() / 4,
            );
            let record: crate::GpuDrawRecord =
                *bytemuck::from_bytes(bytemuck::cast_slice(&record_words));
            assert_eq!(record.content_index, root_handle.index);
            assert_eq!(
                record.representation,
                GpuRepresentation::AggregateVoxel as u32,
                "the cooked root is the aggregate voxel node"
            );
            assert_eq!(record.instance.index, front.index);

            let requested = uploader.drain_page_requests(0);
            let root_node = &hierarchy.nodes[hierarchy
                .pages
                .iter()
                .find(|page| page.id == root_cook)
                .expect("root")
                .node as usize];
            assert_eq!(
                requested.requests.len(),
                root_node.children.len(),
                "every missing child page is requested exactly once"
            );
            for child in &root_node.children {
                let child_page = hierarchy.nodes[*child as usize].page;
                assert!(
                    requested.requests.iter().any(|(slot, class)| {
                        *slot == device_pages[child_page as usize].index
                            && *class == SceneViewClass::Camera
                    }),
                    "child page {child_page} requested, priced as the camera's"
                );
            }

            device.wait_idle().expect("idle");
            drop(view);
            drop(open);
            drop(address_ubo);
            drop(cull);
            drop(traversal);
            drop(pipelines);
            drop(residency);
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(visibility);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    /// The flip-node state machine: a threshold flip crossfades parent ↔ children
    /// over `transition_frames` frames — both sides emit with complementary
    /// transition words, the phase advances once per frame, and the cut settles with
    /// zero transitioning records afterward, in both directions.
    #[test]
    fn representation_flip_crossfades_and_settles_in_both_directions() {
        use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
        use crate::page_residency::{PageResidency, PageResidencyBudgets};
        use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let visibility = SceneVisibility::new(&device).expect("visibility");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let cull = pipelines
                .request_scene_visibility(visibility.layout())
                .expect("cull pso");
            let traversal = pipelines
                .request_scene_traversal(visibility.traversal_layout())
                .expect("traversal pso");

            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut pending = GpuScenePendingUploads::default();
            let mut residency = PageResidency::new(PageResidencyBudgets::default());
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            let resident_material = gpu_data
                .materials
                .insert(GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: GpuHandle::INVALID,
                    parameter_index: 0,
                    material_class: crate::GpuMaterialClass::new(
                        saffron_material::AlphaClassification::Masked,
                        crate::GpuSidedness::Single,
                        saffron_material::SurfaceModel::Standard,
                        crate::GpuTransparency::Opaque,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("resident material");
            pending.stage_record(GlobalGpuTableKind::Material, resident_material);

            // Every cooked page resident so the walk can refine and coarsen freely.
            let hierarchy = cooked_quad();
            let mut device_pages = Vec::new();
            for page in &hierarchy.pages {
                let parent = page
                    .dependency
                    .map(|dependency| device_pages[dependency as usize]);
                let handle = gpu_data
                    .page_table
                    .insert(GpuPageRecord {
                        parent: parent.unwrap_or(GpuHandle::INVALID),
                        dependencies: crate::GpuArenaRange::default(),
                        byte_offset: 0,
                        byte_length: 0,
                        resident_generation: 0,
                        flags: if page.guaranteed_root {
                            crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                        } else {
                            0
                        },
                        reserved: 0,
                    })
                    .expect("page record");
                pending.stage_record(GlobalGpuTableKind::Page, handle);
                residency.register_page(handle, parent, page.guaranteed_root);
                residency.demand(handle, 1);
                device_pages.push(handle);
            }
            let mut payloads = std::collections::HashMap::new();
            for page in &hierarchy.pages {
                let mut payload = crate::build_page_payload(&hierarchy, page.id).expect("payload");
                for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                    payload
                        .patch_child(index, device_pages[*cook_child as usize])
                        .expect("patch");
                }
                payloads.insert(device_pages[page.id as usize], payload.bytes);
            }
            for handle in residency.take_load_requests(64) {
                residency.complete_load(handle, payloads[&handle].clone());
            }
            residency
                .publish_ready(&mut gpu_data, &mut pending)
                .expect("publish all");
            let root_cook = hierarchy
                .pages
                .iter()
                .find(|page| page.guaranteed_root)
                .expect("root page")
                .id;
            let root_children = hierarchy.nodes[hierarchy
                .pages
                .iter()
                .find(|page| page.id == root_cook)
                .expect("root")
                .node as usize]
                .children
                .len();
            assert!(root_children > 0, "the cooked quad root must have children");

            let material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: resident_material,
                        source_revision: 1,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let scene_root = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: device_pages[root_cook as usize],
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("scene page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    GpuScenePrototypeRecord {
                        geometry: GpuHandle {
                            index: 3,
                            generation: 1,
                        },
                        materials: std::sync::Arc::from([material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: scene_root,
                        bounds: [0.0, 0.0, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

            gpu_data.begin_frame(0).expect("gpu data");
            uploader.begin_frame(0).expect("uploader");
            gpu_scene.begin_frame(0).expect("scene");
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain pending");
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            let address_ubo = Buffer::new(
                device.resources(),
                size_of::<crate::GpuSceneAddressBlock>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("address ubo");
            // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&block).as_ptr(),
                    address_ubo.mapped_ptr(),
                    size_of::<crate::GpuSceneAddressBlock>(),
                );
            }
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );

            let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            let open = hzb_image(&device, 1.0);
            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
                * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

            // One simulated frame: clear + cull + traverse at the given refinement
            // threshold with a 3-frame crossfade, then read back the counter words.
            let frames = 3_u32;
            let simulate = |stamp: u32, threshold: f32| -> Vec<u32> {
                let mut graph = RenderGraph::new();
                let hzb_res = graph.import_image(
                    open.handle(),
                    open.view(),
                    vk::ImageAspectFlags::COLOR,
                    vk::ImageLayout::GENERAL,
                    None,
                );
                let wind_buffer = wind_records_buffer(&device);
                let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
                view.add_cull_pass(
                    &device,
                    &mut graph,
                    &cull,
                    0,
                    hzb_res,
                    wind_records_res,
                    64,
                    SceneVisibilityPush {
                        view_proj: view_proj.to_cols_array(),
                        prev_view_proj: view_proj.to_cols_array(),
                        hzb_extent: [64, 64],
                        hzb_mip_count: 7,
                        pass_kind: SCENE_VISIBILITY_PASS_CULL,
                        history_valid: 0,
                        list_capacity: 64,
                        reserved: [0; 2],
                        reach_min: [0.0; 4],
                        reach_max: [0.0; 4],
                    },
                );
                view.add_traversal_pass(
                    &device,
                    &mut graph,
                    &traversal,
                    0,
                    SceneTraversalPush {
                        view_proj: view_proj.to_cols_array(),
                        eye: [0.0, 0.0, 5.0],
                        proj_scale: 1000.0,
                        error_threshold_px: threshold,
                        record_capacity: 256,
                        list_capacity: 64,
                        survivor: 0,
                        tess_seam: 0,
                        transition_frames: frames,
                        frame_stamp: stamp,
                        representation_override: SCENE_CUT_AUTO,
                        node_cull: 1,
                        demand_only: 0,
                        view_class: SceneViewClass::Camera.ordinal(),
                    },
                );
                one_shot(&device, |cmd| graph.execute(&device, cmd));
                read_words(&device, view.counters(0), 12)
            };

            // Settled on the root, then flip to the children: the flip frame and the
            // two after it emit both sides transitioning; the fourth frame settles.
            let settled_root = simulate(1, 1.0e9);
            assert_eq!(settled_root[3], 1, "the settled far cut is the root alone");
            assert_eq!(settled_root[10], 0, "no crossfade while settled");
            let flip_down = simulate(2, 0.0);
            assert!(flip_down[10] > 1, "both sides emit during the refine flip");
            assert_eq!(
                flip_down[10], flip_down[3],
                "every record is transitioning on the flip frame"
            );
            for stamp in 3..=(frames + 1) {
                let mid = simulate(stamp, 0.0);
                assert!(mid[10] > 0, "the crossfade spans {frames} frames");
            }
            let settled_children = simulate(frames + 2, 0.0);
            assert_eq!(settled_children[10], 0, "the refine crossfade settles");
            assert!(
                settled_children[3] >= root_children as u32,
                "the settled near cut is the children"
            );

            // Flip back: the coarsen crossfade emits both sides, then settles on the
            // root alone.
            let flip_up = simulate(frames + 3, 1.0e9);
            assert!(flip_up[10] > 1, "both sides emit during the coarsen flip");
            for stamp in (frames + 4)..=(2 * frames + 2) {
                let mid = simulate(stamp, 1.0e9);
                assert!(mid[10] > 0, "the coarsen crossfade spans {frames} frames");
            }
            let resettled = simulate(2 * frames + 3, 1.0e9);
            assert_eq!(resettled[10], 0, "the coarsen crossfade settles");
            assert_eq!(resettled[3], 1, "the settled far cut is the root again");
            for words in [&settled_root, &flip_down, &settled_children, &resettled] {
                assert_eq!(words[4] & SCENE_TRANSITION_PRESSURE, 0, "no table pressure");
                assert_eq!(
                    words[4] & SCENE_TRAVERSAL_OVERFLOW_RECORDS,
                    0,
                    "no record overflow"
                );
            }

            device.wait_idle().expect("idle");
            drop(view);
            drop(open);
            drop(address_ubo);
            drop(cull);
            drop(traversal);
            drop(pipelines);
            drop(residency);
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(visibility);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    /// Depth texels a 64x64 target holds that are nearer than the clear — the "did it
    /// rasterize" measure both executors are scored by.
    fn count_written_depth(device: &Device, target: &Image) -> usize {
        let staging = Buffer::new(
            device.resources(),
            64 * 64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("staging");
        let raw = device.raw().clone();
        one_shot(device, |cmd| {
            let barrier = vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .old_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(target.handle())
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::DEPTH,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let barriers = [barrier];
            // SAFETY: the ash seam. One-off readback under the fence below.
            unsafe {
                raw.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().image_memory_barriers(&barriers),
                );
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    target.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    staging.handle(),
                    &[vk::BufferImageCopy {
                        buffer_offset: 0,
                        buffer_row_length: 0,
                        buffer_image_height: 0,
                        image_subresource: vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::DEPTH,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        },
                        image_offset: vk::Offset3D::default(),
                        image_extent: vk::Extent3D {
                            width: 64,
                            height: 64,
                            depth: 1,
                        },
                    }],
                );
            }
        });
        let mut depth_bytes = vec![0_u8; 64 * 64 * 4];
        // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
        unsafe {
            std::ptr::copy_nonoverlapping(
                staging.mapped_ptr(),
                depth_bytes.as_mut_ptr(),
                depth_bytes.len(),
            );
        }
        let written = depth_bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .filter(|depth| *depth < 0.999)
            .count();
        device.wait_idle().expect("idle after readback");
        written
    }

    #[test]
    fn executor_draws_the_binned_cut_depth_only() {
        use crate::gpu_scene_upload::{
            GpuArenaUploadRequest, GpuScenePendingUploads, record_pending_global_uploads,
        };
        use crate::page_residency::{PageResidency, PageResidencyBudgets};
        use crate::render_graph::RgAttachment;
        use crate::{GlobalGpuTableKind, GpuGeometryRecord, GpuPageRecord};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let visibility = SceneVisibility::new(&device).expect("visibility");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let cull = pipelines
                .request_scene_visibility(visibility.layout())
                .expect("cull pso");
            let traversal = pipelines
                .request_scene_traversal(visibility.traversal_layout())
                .expect("traversal pso");
            let bin_count = pipelines
                .request_scene_bin_count(visibility.bin_count_layout())
                .expect("bin count pso");
            let bin_scan = pipelines
                .request_scene_bin_seed(visibility.bin_seed_layout())
                .expect("bin scan pso");
            let bin_scatter = pipelines
                .request_scene_bin_scatter(visibility.bin_scatter_layout())
                .expect("bin scatter pso");
            let executor = pipelines
                .request_scene_executor_depth(visibility.executor_layout())
                .expect("executor pso");
            // The mesh executor is optional: a device without `VK_EXT_mesh_shader` runs the
            // indexed path alone, which is the supported configuration on MoltenVK.
            let executor_mesh = device.capabilities.mesh_shader.then(|| {
                pipelines
                    .request_scene_executor_depth_mesh(visibility.executor_layout())
                    .expect("mesh executor pso")
            });

            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut pending = GpuScenePendingUploads::default();
            let mut residency = PageResidency::new(PageResidencyBudgets::default());
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            // Real quad vertices in the global vertex arena for the BDA pull.
            let hierarchy = cooked_quad();
            let vertices: std::sync::Arc<[saffron_geometry::Vertex]> = {
                use saffron_geometry::Vertex;
                use saffron_geometry::glam::{Vec2, Vec3 as GVec3};
                let vert = |x: f32, y: f32| Vertex {
                    position: GVec3::new(x, y, 0.0),
                    normal: GVec3::Z,
                    uv0: Vec2::new(x, y),
                    ..Default::default()
                };
                std::sync::Arc::from([
                    vert(0.0, 0.0),
                    vert(1.0, 0.0),
                    vert(0.0, 1.0),
                    vert(1.0, 1.0),
                ])
            };
            let vertex_bytes = (vertices.len() * size_of::<saffron_geometry::Vertex>()) as u32;
            let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
            pending.upload_arena(GpuArenaUploadRequest::Vertices {
                range: vertex_range,
                data: std::sync::Arc::clone(&vertices),
            });
            let geometry = gpu_data
                .geometries
                .insert(GpuGeometryRecord {
                    vertices: vertex_range,
                    indices: crate::GpuArenaRange::default(),
                    clusters: crate::GpuArenaRange::default(),
                    parts: crate::GpuArenaRange::default(),
                    voxels: crate::GpuArenaRange::default(),
                    submeshes: crate::GpuArenaRange::default(),
                    flags: 0,
                    vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                    index_stride: 4,
                    reserved: 0,
                })
                .expect("geometry");
            pending.stage_record(GlobalGpuTableKind::Geometry, geometry);

            // Every cooked page resident: register, load, publish (parents first).
            let mut device_pages = Vec::new();
            for page in &hierarchy.pages {
                let parent = page
                    .dependency
                    .map(|dependency| device_pages[dependency as usize]);
                let handle = gpu_data
                    .page_table
                    .insert(GpuPageRecord {
                        parent: parent.unwrap_or(GpuHandle::INVALID),
                        dependencies: crate::GpuArenaRange::default(),
                        byte_offset: 0,
                        byte_length: 0,
                        resident_generation: 0,
                        flags: if page.guaranteed_root {
                            crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                        } else {
                            0
                        },
                        reserved: 0,
                    })
                    .expect("page record");
                pending.stage_record(GlobalGpuTableKind::Page, handle);
                residency.register_page(handle, parent, page.guaranteed_root);
                residency.demand(handle, 1);
                device_pages.push(handle);
            }
            let mut payloads = std::collections::HashMap::new();
            for page in &hierarchy.pages {
                let mut payload = crate::build_page_payload(&hierarchy, page.id).expect("payload");
                for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                    payload
                        .patch_child(index, device_pages[*cook_child as usize])
                        .expect("patch");
                }
                payloads.insert(device_pages[page.id as usize], payload.bytes);
            }
            for handle in residency.take_load_requests(64) {
                residency.complete_load(handle, payloads[&handle].clone());
            }
            residency
                .publish_ready(&mut gpu_data, &mut pending)
                .expect("publish all");

            let root_cook = hierarchy
                .pages
                .iter()
                .find(|page| page.guaranteed_root)
                .expect("root page")
                .id;
            let resident_material = gpu_data
                .materials
                .insert(crate::GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: GpuHandle::INVALID,
                    parameter_index: 0,
                    material_class: crate::GpuMaterialClass::new(
                        saffron_material::AlphaClassification::Masked,
                        crate::GpuSidedness::Single,
                        saffron_material::SurfaceModel::Standard,
                        crate::GpuTransparency::Opaque,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("resident material");
            pending.stage_record(GlobalGpuTableKind::Material, resident_material);
            let scene_material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: resident_material,
                        source_revision: 1,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let scene_root = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: device_pages[root_cook as usize],
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("scene page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    GpuScenePrototypeRecord {
                        geometry,
                        materials: std::sync::Arc::from([scene_material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: scene_root,
                        bounds: [0.5, 0.5, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            create_instance(&mut gpu_scene, prototype, Vec3::ZERO);

            gpu_data.begin_frame(0).expect("gpu data");
            uploader.begin_frame(0).expect("uploader");
            gpu_scene.begin_frame(0).expect("scene");
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain pending");
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            let address_ubo = Buffer::new(
                device.resources(),
                size_of::<crate::GpuSceneAddressBlock>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("address ubo");
            // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&block).as_ptr(),
                    address_ubo.mapped_ptr(),
                    size_of::<crate::GpuSceneAddressBlock>(),
                );
            }
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );

            let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            let open = hzb_image(&device, 1.0);
            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
                * Mat4::look_at_rh(Vec3::new(0.5, 0.5, 5.0), Vec3::new(0.5, 0.5, 0.0), Vec3::Y);
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

            let depth_target = Image::new(
                device.resources(),
                &ImageDesc {
                    extent: vk::Extent2D {
                        width: 64,
                        height: 64,
                    },
                    format: vk::Format::D32_SFLOAT,
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSFER_SRC,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                },
            )
            .expect("depth target");
            let depth_mesh = Image::new(
                device.resources(),
                &ImageDesc {
                    extent: vk::Extent2D {
                        width: 64,
                        height: 64,
                    },
                    format: vk::Format::D32_SFLOAT,
                    usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSFER_SRC,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                },
            )
            .expect("mesh depth target");

            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &cull,
                0,
                hzb_res,
                wind_records_res,
                64,
                SceneVisibilityPush {
                    view_proj: view_proj.to_cols_array(),
                    prev_view_proj: view_proj.to_cols_array(),
                    hzb_extent: [64, 64],
                    hzb_mip_count: 7,
                    pass_kind: SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: 64,
                    reserved: [0; 2],
                    reach_min: [0.0; 4],
                    reach_max: [0.0; 4],
                },
            );
            view.add_traversal_pass(
                &device,
                &mut graph,
                &traversal,
                0,
                SceneTraversalPush {
                    view_proj: view_proj.to_cols_array(),
                    eye: [0.5, 0.5, 5.0],
                    proj_scale: 100_000.0,
                    error_threshold_px: 0.0,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    tess_seam: 0,
                    transition_frames: 0,
                    frame_stamp: 0,
                    representation_override: SCENE_CUT_AUTO,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            let class_bits = crate::GpuMaterialClass::new(
                saffron_material::AlphaClassification::Masked,
                crate::GpuSidedness::Single,
                saffron_material::SurfaceModel::Standard,
                crate::GpuTransparency::Opaque,
                false,
            )
            .bits();
            let (buckets, table) = build_executor_buckets(&[(0, class_bits)], 256);
            view.write_bucket_table(0, &table);
            view.add_binning_passes(
                &device,
                &mut graph,
                (&bin_count, &bin_scan, &bin_scatter),
                0,
                false,
                (0, 0),
            );
            let depth_res = graph.import_image(
                depth_target.handle(),
                depth_target.view(),
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let commands_res = graph.import_buffer(view.commands(0), None);
            let counters_res = graph.import_buffer(view.counters(0), None);
            let vp = view_proj.to_cols_array();
            let pages_buffer = gpu_data.pages.buffer();
            let draw_inputs = view.executor_draw_inputs(0, 256);
            let executor_handle = executor.handle();
            let executor_layout = executor.layout();
            let draw_count_supported = device.capabilities.draw_indirect_count;
            let raw_draw = device.raw().clone();
            // The indexed pass body takes ownership of `buckets`; the mesh pass below draws the
            // same list.
            let mesh_buckets = buckets.clone();
            graph.add_pass(
                RgPass::graphics(
                    "executor-depth",
                    vk::Extent2D {
                        width: 64,
                        height: 64,
                    },
                )
                .depth_attachment(RgAttachment {
                    resource: depth_res,
                    load_op: vk::AttachmentLoadOp::CLEAR,
                    store_op: vk::AttachmentStoreOp::STORE,
                    clear_value: vk::ClearValue {
                        depth_stencil: vk::ClearDepthStencilValue {
                            depth: 1.0,
                            stencil: 0,
                        },
                    },
                    resolve: None,
                })
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_executor_pass_prefix(
                        &raw_draw,
                        cmd,
                        executor_layout,
                        draw_inputs,
                        pages_buffer,
                        vp,
                    );
                    for (bucket_index, bucket) in buckets.iter().enumerate() {
                        record_executor_bucket_draw(
                            &raw_draw,
                            cmd,
                            (executor_handle, executor_layout),
                            draw_inputs,
                            ExecutorBucketDraw {
                                bucket: *bucket,
                                index: bucket_index as u32,
                                draw_indirect_count: draw_count_supported,
                            },
                        );
                    }
                }),
            );
            // The mesh executor over the SAME records, into its own depth target. It reads the
            // command stream as data rather than as draw arguments, so any divergence means one
            // executor ignored records the binner emitted.
            if let Some(mesh_pso) = executor_mesh.as_ref() {
                let mesh_args_res = graph.import_buffer(view.mesh_args(0), None);
                let depth_mesh_res = graph.import_image(
                    depth_mesh.handle(),
                    depth_mesh.view(),
                    vk::ImageAspectFlags::DEPTH,
                    vk::ImageLayout::UNDEFINED,
                    None,
                );
                let mesh_handle = mesh_pso.handle();
                let mesh_layout = mesh_pso.layout();
                let raw_mesh = device.raw().clone();
                let mesh_dispatch = device
                    .mesh_shader_dispatch()
                    .expect("mesh dispatch")
                    .clone();
                graph.add_pass(
                    RgPass::graphics(
                        "executor-depth-mesh",
                        vk::Extent2D {
                            width: 64,
                            height: 64,
                        },
                    )
                    .depth_attachment(RgAttachment {
                        resource: depth_mesh_res,
                        load_op: vk::AttachmentLoadOp::CLEAR,
                        store_op: vk::AttachmentStoreOp::STORE,
                        clear_value: vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.0,
                                stencil: 0,
                            },
                        },
                        resolve: None,
                    })
                    .access(mesh_args_res, RgUsage::IndirectCommandRead)
                    .access(counters_res, RgUsage::IndirectCountRead)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        for (bucket_index, bucket) in mesh_buckets.iter().enumerate() {
                            record_executor_mesh_prefix(
                                &raw_mesh,
                                cmd,
                                mesh_layout,
                                draw_inputs,
                                vp,
                                bucket.base,
                            );
                            record_executor_bucket_draw_mesh(
                                &raw_mesh,
                                &mesh_dispatch,
                                cmd,
                                (mesh_handle, mesh_layout),
                                draw_inputs,
                                ExecutorBucketDraw {
                                    bucket: *bucket,
                                    index: bucket_index as u32,
                                    draw_indirect_count: draw_count_supported,
                                },
                            );
                        }
                    }),
                );
            }
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let counters = read_words(&device, view.counters(0), 8);
            assert!(counters[3] > 0, "the cut emits records");
            assert_eq!(counters[4], 0, "no record overflow");

            // The quad must have written depth: count texels nearer than the clear.
            let written = count_written_depth(&device, &depth_target);
            assert!(
                written > 100,
                "the executor rasterized the quad ({written} depth texels written)"
            );

            // Both executors consume one binned cut, so they must cover the same pixels. The
            // bound is a proportion rather than an exact match: the mesh path emits three
            // vertices per triangle without deduplication, so a shared edge is rasterized from
            // two independently interpolated triangles and a boundary texel may resolve either
            // way.
            if executor_mesh.is_some() {
                let mesh_written = count_written_depth(&device, &depth_mesh);
                assert!(
                    mesh_written > 100,
                    "the mesh executor rasterized the quad ({mesh_written} depth texels written)"
                );
                let spread = written.abs_diff(mesh_written);
                assert!(
                    spread * 50 <= written.max(mesh_written),
                    "indexed and mesh executors disagree on coverage \
                     (indexed {written}, mesh {mesh_written})"
                );
            }

            device.wait_idle().expect("idle");
            drop(depth_target);
            drop(depth_mesh);
            drop(view);
            drop(open);
            drop(address_ubo);
            drop(cull);
            drop(traversal);
            drop(bin_count);
            drop(bin_scan);
            drop(bin_scatter);
            drop(executor);
            drop(pipelines);
            drop(residency);
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(visibility);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn transparent_records_sort_back_to_front() {
        use crate::gpu_scene_upload::{GpuScenePendingUploads, record_pending_global_uploads};
        use crate::page_residency::{PageResidency, PageResidencyBudgets};
        use crate::{GlobalGpuTableKind, GpuMaterialTableRecord, GpuPageRecord};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let visibility = SceneVisibility::new(&device).expect("visibility");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let cull = pipelines
                .request_scene_visibility(visibility.layout())
                .expect("cull pso");
            let traversal = pipelines
                .request_scene_traversal(visibility.traversal_layout())
                .expect("traversal pso");
            let keys = pipelines
                .request_transparent_keys(visibility.transparent_keys_layout())
                .expect("keys pso");
            let histogram = pipelines
                .request_radix_histogram(visibility.radix_histogram_layout())
                .expect("histogram pso");
            let scan = pipelines
                .request_radix_scan(visibility.radix_scan_layout())
                .expect("scan pso");
            let scatter = pipelines
                .request_radix_scatter(visibility.radix_scatter_layout())
                .expect("scatter pso");
            let reorder = pipelines
                .request_transparent_reorder(visibility.transparent_reorder_layout())
                .expect("reorder pso");

            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut pending = GpuScenePendingUploads::default();
            let mut residency = PageResidency::new(PageResidencyBudgets::default());
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            // An alpha-blended resident material so leaf clusters bin transparent.
            let resident_material = gpu_data
                .materials
                .insert(GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: GpuHandle::INVALID,
                    parameter_index: 0,
                    material_class: crate::GpuMaterialClass::new(
                        saffron_material::AlphaClassification::Opaque,
                        crate::GpuSidedness::Single,
                        saffron_material::SurfaceModel::Standard,
                        crate::GpuTransparency::AlphaBlended,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("resident material");
            pending.stage_record(GlobalGpuTableKind::Material, resident_material);

            let hierarchy = cooked_quad();
            let mut device_pages = Vec::new();
            for page in &hierarchy.pages {
                let parent = page
                    .dependency
                    .map(|dependency| device_pages[dependency as usize]);
                let handle = gpu_data
                    .page_table
                    .insert(GpuPageRecord {
                        parent: parent.unwrap_or(GpuHandle::INVALID),
                        dependencies: crate::GpuArenaRange::default(),
                        byte_offset: 0,
                        byte_length: 0,
                        resident_generation: 0,
                        flags: if page.guaranteed_root {
                            crate::GPU_PAGE_FLAG_GUARANTEED_ROOT
                        } else {
                            0
                        },
                        reserved: 0,
                    })
                    .expect("page record");
                pending.stage_record(GlobalGpuTableKind::Page, handle);
                residency.register_page(handle, parent, page.guaranteed_root);
                residency.demand(handle, 1);
                device_pages.push(handle);
            }
            let mut payloads = std::collections::HashMap::new();
            for page in &hierarchy.pages {
                let mut payload = crate::build_page_payload(&hierarchy, page.id).expect("payload");
                for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                    payload
                        .patch_child(index, device_pages[*cook_child as usize])
                        .expect("patch");
                }
                payloads.insert(device_pages[page.id as usize], payload.bytes);
            }
            for handle in residency.take_load_requests(64) {
                residency.complete_load(handle, payloads[&handle].clone());
            }
            residency
                .publish_ready(&mut gpu_data, &mut pending)
                .expect("publish all");

            let root_cook = hierarchy
                .pages
                .iter()
                .find(|page| page.guaranteed_root)
                .expect("root page")
                .id;
            let scene_material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: resident_material,
                        source_revision: 1,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let scene_root = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: device_pages[root_cook as usize],
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("scene page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    GpuScenePrototypeRecord {
                        geometry: GpuHandle {
                            index: 3,
                            generation: 1,
                        },
                        materials: std::sync::Arc::from([scene_material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: scene_root,
                        bounds: [0.5, 0.5, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            // Three instances at increasing distance from the camera at z = 5.
            let near = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);
            let middle = create_instance(&mut gpu_scene, prototype, Vec3::new(0.0, 0.0, -3.0));
            let far = create_instance(&mut gpu_scene, prototype, Vec3::new(0.0, 0.0, -6.0));

            gpu_data.begin_frame(0).expect("gpu data");
            uploader.begin_frame(0).expect("uploader");
            gpu_scene.begin_frame(0).expect("scene");
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain pending");
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            let address_ubo = Buffer::new(
                device.resources(),
                size_of::<crate::GpuSceneAddressBlock>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("address ubo");
            // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&block).as_ptr(),
                    address_ubo.mapped_ptr(),
                    size_of::<crate::GpuSceneAddressBlock>(),
                );
            }
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );

            let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            let open = hzb_image(&device, 1.0);
            let camera_view =
                Mat4::look_at_rh(Vec3::new(0.5, 0.5, 5.0), Vec3::new(0.5, 0.5, 0.0), Vec3::Y);
            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0) * camera_view;
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);

            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &cull,
                0,
                hzb_res,
                wind_records_res,
                64,
                SceneVisibilityPush {
                    view_proj: view_proj.to_cols_array(),
                    prev_view_proj: view_proj.to_cols_array(),
                    hzb_extent: [64, 64],
                    hzb_mip_count: 7,
                    pass_kind: SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: 64,
                    reserved: [0; 2],
                    reach_min: [0.0; 4],
                    reach_max: [0.0; 4],
                },
            );
            view.add_traversal_pass(
                &device,
                &mut graph,
                &traversal,
                0,
                SceneTraversalPush {
                    view_proj: view_proj.to_cols_array(),
                    eye: [0.5, 0.5, 5.0],
                    proj_scale: 100_000.0,
                    error_threshold_px: 0.0,
                    record_capacity: 256,
                    list_capacity: 64,
                    survivor: 0,
                    tess_seam: 0,
                    transition_frames: 0,
                    frame_stamp: 0,
                    representation_override: SCENE_CUT_AUTO,
                    node_cull: 1,
                    demand_only: 0,
                    view_class: SceneViewClass::Camera.ordinal(),
                },
            );
            // The view matrix's third row measures view-space depth.
            let row2 = camera_view.row(2);
            // The blend material's two representation buckets, in bucket-key order —
            // the reorder writes one masked full-length slice per bucket.
            let material_class = crate::GpuMaterialClass::new(
                saffron_material::AlphaClassification::Opaque,
                crate::GpuSidedness::Single,
                saffron_material::SurfaceModel::Standard,
                crate::GpuTransparency::AlphaBlended,
                false,
            );
            let blend_keys: Vec<u32> = [
                crate::GpuRepresentation::TriangleCluster as u32,
                crate::GpuRepresentation::AggregateVoxel as u32,
            ]
            .iter()
            .map(|representation| {
                (representation << crate::GPU_PSO_REPRESENTATION_SHIFT)
                    | (material_class.bits() << crate::GPU_PSO_MATERIAL_SHIFT)
            })
            .collect();
            view.add_transparent_sort_passes(
                &device,
                &mut graph,
                TransparentSortPipelines {
                    keys: &keys,
                    histogram: &histogram,
                    scan: &scan,
                    scatter: &scatter,
                    reorder: &reorder,
                },
                0,
                [row2.x, row2.y, row2.z, row2.w],
                &blend_keys,
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let counters = read_words(&device, view.counters(0), 8);
            let record_count = counters[3] as usize;
            let pair_count = counters[5] as usize;
            assert!(record_count >= 3, "each instance emits at least one record");
            assert_eq!(
                pair_count, record_count,
                "every alpha-blended record collects a sort pair"
            );
            assert_eq!(counters[4], 0, "no overflow");

            let record_words = read_words(
                &device,
                view.records(0),
                record_count * size_of::<crate::GpuDrawRecord>() / 4,
            );
            let records: &[crate::GpuDrawRecord] = bytemuck::cast_slice(&record_words);
            // Both bucket slices, full length: exactly one bucket owns each sorted
            // slot with a live draw; the other masks it to a zero draw.
            let command_words = read_words(&device, view.transparent_commands(0), 2 * 256 * 5);
            let order: Vec<u32> = (0..pair_count)
                .map(|slot| {
                    let live: Vec<&[u32]> = (0..2)
                        .map(|group| {
                            let base = (group * 256 + slot) * 5;
                            &command_words[base..base + 5]
                        })
                        .filter(|command| command[0] != 0)
                        .collect();
                    assert_eq!(live.len(), 1, "exactly one bucket owns slot {slot}");
                    records[live[0][4] as usize].instance.index
                })
                .collect();

            // Back-to-front: every far record precedes every middle record, which
            // precede every near record.
            let position = |slot: u32| order.iter().position(|entry| *entry == slot);
            let last = |slot: u32| order.iter().rposition(|entry| *entry == slot);
            let far_last = last(far.index).expect("far drawn");
            let middle_first = position(middle.index).expect("middle drawn");
            let middle_last = last(middle.index).expect("middle drawn");
            let near_first = position(near.index).expect("near drawn");
            assert!(
                far_last < middle_first,
                "far draws before middle: {order:?}"
            );
            assert!(
                middle_last < near_first,
                "middle draws before near: {order:?}"
            );

            device.wait_idle().expect("idle");
            drop(view);
            drop(open);
            drop(address_ubo);
            drop(cull);
            drop(traversal);
            drop(keys);
            drop(histogram);
            drop(scan);
            drop(scatter);
            drop(reorder);
            drop(pipelines);
            drop(residency);
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(visibility);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn cull_frustum_occlusion_and_retest_classify_instances() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let visibility = SceneVisibility::new(&device).expect("visibility");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let pipeline = pipelines
                .request_scene_visibility(visibility.layout())
                .expect("visibility pso");

            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            let material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: GpuHandle {
                            index: 1,
                            generation: 1,
                        },
                        source_revision: 1,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let page = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: GpuHandle {
                        index: 2,
                        generation: 1,
                    },
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    GpuScenePrototypeRecord {
                        geometry: GpuHandle {
                            index: 3,
                            generation: 1,
                        },
                        materials: std::sync::Arc::from([material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: page,
                        bounds: [0.0, 0.0, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let front = create_instance(&mut gpu_scene, prototype, Vec3::ZERO);
            let aside = create_instance(&mut gpu_scene, prototype, Vec3::new(1000.0, 0.0, 0.0));
            let second = create_instance(&mut gpu_scene, prototype, Vec3::new(0.5, 0.0, 0.0));

            gpu_data.begin_frame(0).expect("gpu data");
            uploader.begin_frame(0).expect("uploader");
            gpu_scene.begin_frame(0).expect("scene");
            let mut graph = RenderGraph::new();
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            one_shot(&device, |cmd| graph.execute(&device, cmd));

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            let address_ubo = Buffer::new(
                device.resources(),
                size_of::<crate::GpuSceneAddressBlock>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("address ubo");
            // SAFETY: HOST_VISIBLE + MAPPED, written before any submit that reads it.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytemuck::bytes_of(&block).as_ptr(),
                    address_ubo.mapped_ptr(),
                    size_of::<crate::GpuSceneAddressBlock>(),
                );
            }

            let view = SceneVisibilityView::new(&device, &descriptors, &visibility, 64, 256, 2)
                .expect("view lists");
            let open = hzb_image(&device, 1.0);
            let wall = hzb_image(&device, 0.05);

            let view_proj = Mat4::perspective_rh(1.0, 1.0, 0.1, 100.0)
                * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
            let push = |pass_kind: u32, history_valid: u32| SceneVisibilityPush {
                view_proj: view_proj.to_cols_array(),
                prev_view_proj: view_proj.to_cols_array(),
                hzb_extent: [64, 64],
                hzb_mip_count: 7,
                pass_kind,
                history_valid,
                list_capacity: 64,
                reserved: [0; 2],
                reach_min: [0.0; 4],
                reach_max: [0.0; 4],
            };
            let address = (
                address_ubo.handle(),
                0,
                size_of::<crate::GpuSceneAddressBlock>() as u64,
            );

            // Round 1: open pyramid, no history — the two on-screen instances are
            // visible, the far-off one frustum-culls.
            view.write_frame_bindings(&device, &visibility, 0, open.view(), open.view(), address);
            let mut graph = RenderGraph::new();
            let hzb_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &pipeline,
                0,
                hzb_res,
                wind_records_res,
                64,
                push(0, 0),
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));
            let counters = read_words(&device, view.counters(0), 3);
            assert_eq!(counters[0], 2, "front + second visible");
            assert_eq!(counters[1], 0, "no retest without history");
            assert_eq!(counters[2], 0, "no overflow");
            let mut visible = read_words(&device, view.visible(0), 2);
            visible.sort_unstable();
            let mut expected = vec![front.index, second.index];
            expected.sort_unstable();
            assert_eq!(visible, expected);
            assert!(
                !read_words(&device, view.visible(0), 2).contains(&aside.index),
                "the far-off instance frustum-culls"
            );

            // Round 2 + 3: established instances hit the wall pyramid and wait on the
            // retest list; the retest against the open pyramid merges them back.
            view.write_frame_bindings(&device, &visibility, 0, wall.view(), open.view(), address);
            let mut graph = RenderGraph::new();
            let wall_res = graph.import_image(
                wall.handle(),
                wall.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let open_res = graph.import_image(
                open.handle(),
                open.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                None,
            );
            let wind_buffer = wind_records_buffer(&device);
            let wind_records_res = graph.import_buffer(wind_buffer.handle(), None);
            view.add_cull_pass(
                &device,
                &mut graph,
                &pipeline,
                0,
                wall_res,
                wind_records_res,
                64,
                push(0, 1),
            );
            view.add_retest_pass(
                &device,
                &mut graph,
                &pipeline,
                0,
                open_res,
                wind_records_res,
                push(1, 1),
            );
            one_shot(&device, |cmd| graph.execute(&device, cmd));
            let counters = read_words(&device, view.counters(0), 3);
            assert_eq!(counters[1], 2, "both established instances retested");
            assert_eq!(
                counters[0], 2,
                "the open current pyramid merges both survivors"
            );
            assert_eq!(counters[2], 0, "no overflow");

            device.wait_idle().expect("idle");
            drop(view);
            drop(open);
            drop(wall);
            drop(address_ubo);
            drop(pipeline);
            drop(pipelines);
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(visibility);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
