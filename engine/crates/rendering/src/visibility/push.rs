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
    /// Nonzero: an instance holding a row in the frame's displacement arena emits one
    /// record for its amplified micro-geometry instead of walking its base hierarchy.
    /// Zero: every instance walks its base pages — what a view reading the undisplaced
    /// surface (shadow pages, the reach walk) wants.
    pub displaced_records: u32,
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
