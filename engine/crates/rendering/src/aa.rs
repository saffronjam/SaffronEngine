//! Anti-aliasing mode selection + the motion-vector prepass the temporal modes need.
//!
//! The three AA modes are mutually exclusive: MSAA (multisampled scene targets resolved
//! into the offscreen), FXAA (scene → scratch, a compute edge-blur → offscreen), and TAA
//! (motion-vector reprojection + a compute resolve with two ping-pong history images).
//! [`Aa`] is the single selector — [`Aa::set`] enforces the exclusivity in one place
//! (MSAA wins if `samples > 1`) and clamps the requested count to what the color + depth
//! formats actually support. There is one AA state, not three independent toggles that can
//! contradict (the phase's NO-LEGACY note).
//!
//! The motion prepass push + recorder ([`MotionPush`], [`record_motion`]) live here
//! too, since the motion vectors are the temporal AA's (and SSGI's) shared dependency.

use ash::vk;
use saffron_geometry::glam::Mat4;

use crate::draw_list::SceneDrawList;
use crate::scene_pass::record_batch_submeshes;

/// The screen-space motion-vector target format (rg16f): the per-pixel `prevUv - curUv`
/// offset TAA / SSGI reproject through.
pub const MOTION_FORMAT: vk::Format = vk::Format::R16G16_SFLOAT;

/// The TAA reactive-coverage mask format (r8): per-input-pixel `[0, 1]` translucency coverage
/// the resolve reads to bias alpha-blended pixels toward the current frame.
pub const REACTIVE_FORMAT: vk::Format = vk::Format::R8_UNORM;

/// Number of jitter phases in the Halton(2,3) cycle. 8 is the balanced native-resolution
/// default (the follow-on upsampling set scales this with the upscale ratio).
pub const TAA_JITTER_PHASES: u32 = 8;

/// Radical-inverse Halton sample in `[0, 1)` for 1-based index `i` in base `b`.
fn halton(mut i: u32, b: u32) -> f32 {
    let mut f = 1.0f32;
    let mut r = 0.0f32;
    while i > 0 {
        f /= b as f32;
        r += f * (i % b) as f32;
        i /= b;
    }
    r
}

/// The sub-pixel jitter offset in NDC for Halton phase `index` at a render extent:
/// `(2·halton − 1) / dim`, i.e. up to ±0.5 px (1 px == `2/dim` in NDC). The scene view-
/// projection applies it as a clip-space translation (`clip.xy += offset · clip.w`); the motion
/// prepass reprojects with the un-jittered matrices so velocity stays exact.
pub fn jitter_offset(index: u32, width: u32, height: u32) -> saffron_geometry::glam::Vec2 {
    let w = width.max(1) as f32;
    let h = height.max(1) as f32;
    saffron_geometry::glam::Vec2::new(
        (2.0 * halton(index + 1, 2) - 1.0) / w,
        (2.0 * halton(index + 1, 3) - 1.0) / h,
    )
}

/// Halton jitter phases for an input→display upscale (FSR2 `ceil(8·n²)`, `n = displayW /
/// inputW`). More phases at heavier upscale so every display pixel is eventually covered by a
/// jittered input sample; degenerates to [`TAA_JITTER_PHASES`] (8) at 1:1. Keyed off width alone
/// because `scaled_render_extent` scales both axes by the one render scale, so `displayW/inputW ==
/// displayH/inputH` — one source of truth.
pub fn jitter_phase_count(input: vk::Extent2D, display: vk::Extent2D) -> u32 {
    let n = display.width.max(1) as f32 / input.width.max(1) as f32;
    ((TAA_JITTER_PHASES as f32 * n * n).ceil() as u32).max(TAA_JITTER_PHASES)
}

/// The anti-aliasing selection: the device's supported sample counts (a fact), the chosen
/// MSAA count, and the FXAA / TAA toggles. Mutually exclusive by construction — only
/// [`Aa::set`] mutates it, and it never leaves more than one mode active.
///
/// Plain data — the GPU targets the modes drive live on [`crate::ViewTarget`]; this is the
/// selector the frame-graph build reads to branch the scene output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aa {
    /// The chosen MSAA sample count (`TYPE_1` = MSAA off).
    sample_count: vk::SampleCountFlags,
    /// The largest sample count the color + depth formats both support (the device cap).
    max_sample_count: vk::SampleCountFlags,
    /// The counts the color + depth MSAA formats both accept (clamp target).
    supported: vk::SampleCountFlags,
    /// FXAA post-process (mutually exclusive with MSAA / TAA).
    fxaa: bool,
    /// TAA resolve (mutually exclusive with MSAA / FXAA).
    taa: bool,
}

impl Aa {
    /// Builds the AA selector against the device's `supported` sample counts, off (1×, no
    /// FXAA/TAA). `max_sample_count` is the largest supported count for inspection.
    pub fn new(supported: vk::SampleCountFlags) -> Self {
        Self {
            sample_count: vk::SampleCountFlags::TYPE_1,
            max_sample_count: largest_supported(supported),
            supported,
            fxaa: false,
            taa: false,
        }
    }

    /// Selects the AA mode, enforcing mutual exclusivity in one place: MSAA wins if
    /// `msaa_samples >= 2` (the count clamped to what the formats support), else FXAA if
    /// requested, else TAA if requested, else off. Returns `true` if the *MSAA sample
    /// count* changed — the caller then clears the sample-count-baked PSO cache.
    pub fn set(&mut self, msaa_samples: u32, fxaa: bool, taa: bool) -> bool {
        let requested = if msaa_samples >= 8 {
            vk::SampleCountFlags::TYPE_8
        } else if msaa_samples >= 4 {
            vk::SampleCountFlags::TYPE_4
        } else if msaa_samples >= 2 {
            vk::SampleCountFlags::TYPE_2
        } else {
            vk::SampleCountFlags::TYPE_1
        };
        let count = clamp_sample_count(self.supported, requested);
        let msaa = count != vk::SampleCountFlags::TYPE_1;
        let old_count = self.sample_count;
        // The three modes are mutually exclusive: MSAA wins if a count > 1 was requested,
        // then FXAA, then TAA. MSAA forces FXAA + TAA off; FXAA forces TAA off.
        self.sample_count = count;
        self.fxaa = !msaa && fxaa;
        self.taa = !msaa && !fxaa && taa;
        old_count != self.sample_count
    }

    /// Selects the AA mode by name (the `sa` CLI / control wire): `"off"`, `"fxaa"`,
    /// `"taa"`, `"msaa2"`, `"msaa4"`, `"msaa8"`. Returns whether the sample count changed.
    pub fn set_mode(&mut self, mode: &str) -> bool {
        let (samples, fxaa, taa) = match mode {
            "fxaa" => (1, true, false),
            "taa" => (1, false, true),
            "msaa2" => (2, false, false),
            "msaa4" => (4, false, false),
            "msaa8" => (8, false, false),
            _ => (1, false, false),
        };
        self.set(samples, fxaa, taa)
    }

    /// The current mode as a name (`"off"` / `"fxaa"` / `"taa"` / `"msaaN"`).
    pub fn mode(&self) -> String {
        if self.fxaa {
            return "fxaa".to_string();
        }
        if self.taa {
            return "taa".to_string();
        }
        match sample_count_value(self.sample_count) {
            n if n <= 1 => "off".to_string(),
            n => format!("msaa{n}"),
        }
    }

    /// The chosen MSAA sample count (`TYPE_1` when MSAA is off).
    pub fn sample_count(&self) -> vk::SampleCountFlags {
        self.sample_count
    }

    /// Whether MSAA is active (sample count > 1).
    pub fn msaa(&self) -> bool {
        self.sample_count != vk::SampleCountFlags::TYPE_1
    }

    /// Whether FXAA is active.
    pub fn fxaa(&self) -> bool {
        self.fxaa
    }

    /// Whether TAA is active.
    pub fn taa(&self) -> bool {
        self.taa
    }

    /// The largest MSAA count the device supports (for inspection / UI clamping).
    pub fn max_sample_count(&self) -> vk::SampleCountFlags {
        self.max_sample_count
    }
}

/// The largest MSAA sample count not exceeding `requested` that `supported` accepts (`1×`
/// if none) — a count valid as a framebuffer limit can still be unsupported for a specific
/// format, and creating an image with it is invalid.
pub fn clamp_sample_count(
    supported: vk::SampleCountFlags,
    requested: vk::SampleCountFlags,
) -> vk::SampleCountFlags {
    let want = sample_count_value(requested);
    for candidate in [
        vk::SampleCountFlags::TYPE_8,
        vk::SampleCountFlags::TYPE_4,
        vk::SampleCountFlags::TYPE_2,
    ] {
        if sample_count_value(candidate) <= want && supported.contains(candidate) {
            return candidate;
        }
    }
    vk::SampleCountFlags::TYPE_1
}

/// The largest count in a supported set (for `max_sample_count` reporting).
fn largest_supported(supported: vk::SampleCountFlags) -> vk::SampleCountFlags {
    for candidate in [
        vk::SampleCountFlags::TYPE_8,
        vk::SampleCountFlags::TYPE_4,
        vk::SampleCountFlags::TYPE_2,
    ] {
        if supported.contains(candidate) {
            return candidate;
        }
    }
    vk::SampleCountFlags::TYPE_1
}

/// The integer sample count a `SampleCountFlags` bit names (the bit value is the count:
/// `TYPE_4` == `0b100` == 4). `TYPE_1` for any unrecognized bit.
fn sample_count_value(flags: vk::SampleCountFlags) -> u32 {
    for (bit, n) in [
        (vk::SampleCountFlags::TYPE_8, 8),
        (vk::SampleCountFlags::TYPE_4, 4),
        (vk::SampleCountFlags::TYPE_2, 2),
    ] {
        if flags.contains(bit) {
            return n;
        }
    }
    1
}

/// The motion-vector prepass push: this frame's + last frame's camera viewProj. The vertex
/// shader reprojects each surface point through both to write `prevUv - curUv`. Two mat4s,
/// vertex stage, matching `motion.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MotionPush {
    /// This frame's world → clip.
    pub cur_view_proj: Mat4,
    /// Last frame's world → clip (this view's own previous frame).
    pub prev_view_proj: Mat4,
}

const _: () = assert!(size_of::<MotionPush>() == 128);

/// The runtime TAA resolve tuning (replaces the old fixed history-weight constant). All are
/// live-tunable over the control plane; the defaults are the balanced reference values
/// (Karis/Lottes/Playdead).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TaaParams {
    /// History weight under fast motion / shading change (the floor). ~0.88.
    pub feedback_min: f32,
    /// History weight when still with stable luma (the ceiling). ~0.97.
    pub feedback_max: f32,
    /// How hard screen-space velocity pulls feedback toward `feedback_min`
    /// (`saturate(velPx * velocity_rejection)`; ~0.025 ≈ full rejection near 40 px/frame).
    pub velocity_rejection: f32,
    /// The YCoCg variance-clip half-extent multiplier `gamma`. ~1.0 (0.75..1.25).
    pub clip_gamma: f32,
    /// RCAS-style sharpen strength (0 = off). Carried here so the push layout is final.
    pub sharpness: f32,
    /// Frames a freshly created lock survives before it must be renewed (FSR2 ≈ a few frames).
    /// 0 disables locking. ~4.0.
    pub lock_lifetime: f32,
    /// How hard the reactive mask pulls the blend toward the current frame
    /// (`saturate(mask * reactive_scale)`). ~1.0.
    pub reactive_scale: f32,
    /// Relative reprojected-depth mismatch counted as a disocclusion (`|d - dPrev| > k · d`). ~0.10.
    pub disocclusion_threshold: f32,
    /// Luma disagreement (vs the stored lock luma) that breaks a lock, as a fraction. ~0.25.
    pub lock_break_luma: f32,
}

impl Default for TaaParams {
    fn default() -> Self {
        Self {
            feedback_min: 0.88,
            feedback_max: 0.97,
            velocity_rejection: 0.025,
            clip_gamma: 1.0,
            sharpness: 0.0,
            lock_lifetime: 4.0,
            reactive_scale: 1.0,
            disocclusion_threshold: 0.10,
            lock_break_luma: 0.25,
        }
    }
}

/// The TAA resolve push (56 bytes, matching `taa.slang`'s `Push`). Seven `vec2`s so the std430
/// layout is unambiguous: the adaptive feedback range, the current + previous NDC jitter, the
/// input render extent in pixels, the clip gamma + history-valid flag, the velocity-rejection +
/// sharpen knobs, and the upscale mapping (ratio + accumulation target).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TaaPush {
    /// `x` = feedback_min, `y` = feedback_max.
    pub feedback: saffron_geometry::glam::Vec2,
    /// This frame's NDC jitter offset (`ViewTarget::jitter`).
    pub jitter: saffron_geometry::glam::Vec2,
    /// Last frame's NDC jitter offset (`ViewTarget::prev_jitter`).
    pub prev_jitter: saffron_geometry::glam::Vec2,
    /// The input/render extent in pixels (`velPx = length(mv * screen_size)`, and the source grid
    /// the resolve reconstructs from — its reciprocal is the input texel size).
    pub screen_size: saffron_geometry::glam::Vec2,
    /// `x` = clip gamma (variance clip), `y` = 1.0 if history is valid this frame.
    pub gamma_valid: saffron_geometry::glam::Vec2,
    /// `x` = velocity_rejection, `y` = sharpness.
    pub reject_sharp: saffron_geometry::glam::Vec2,
    /// `x` = upscale ratio `n = displayW / inputW` (1.0 at native); `y` = accumulation target
    /// (`sampleTarget`) the per-output confidence saturates against.
    pub upscale: saffron_geometry::glam::Vec2,
    /// `x` = lock initial lifetime (frames), `y` = reactive_scale.
    pub lock_reactive: saffron_geometry::glam::Vec2,
    /// `x` = disocclusion_threshold (relative), `y` = lock_break_luma.
    pub disoccl: saffron_geometry::glam::Vec2,
    /// Camera near/far for linearizing `motionDepth` in the disocclusion test (`x` = near, `y` = far).
    pub depth_params: saffron_geometry::glam::Vec2,
}

const _: () = assert!(size_of::<TaaPush>() == 80);

/// Records the motion-vector prepass: bind the instance set (2) + the cur/prev camera
/// viewProj push, then draw every batch's submeshes with both vertex bindings pointing at
/// the same static stream (so `prevPosition == position` and object motion comes from
/// `inst.prevModel`). The skinned deform-motion path uses distinct cur/prev deformed
/// buffers.
#[allow(clippy::too_many_arguments)]
pub fn record_motion(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    list: &SceneDrawList,
    motion_pipeline: vk::Pipeline,
    motion_layout: vk::PipelineLayout,
    instance_set: vk::DescriptorSet,
    push: &MotionPush,
    deformed: Option<vk::Buffer>,
    prev_deformed: Option<vk::Buffer>,
) {
    if !list.valid || list.batches.is_empty() {
        return;
    }
    let push_bytes = bytemuck::bytes_of(push);
    // SAFETY: the ash seam. The PSO/layout/set are valid this frame; the push spans the
    // declared two-mat4 vertex range.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, motion_pipeline);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            motion_layout,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            motion_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            push_bytes,
        );
    }
    for batch in &list.batches {
        // A tessellated batch binds its amplified transient VB as the current position stream and its
        // double-buffered prev micro-vertex VB as the previous stream (the emit kernel wrote the latter
        // with last frame's per-edge factors, so the geomorph slide reprojects), plus its generated index
        // stream; `record_batch_submeshes` then takes the shared indirect branch.
        let (cur, prev, index_buffer) = if let Some(t) = &batch.tessellated {
            (t.vertex_buffer, t.prev_vertex_buffer, t.index_buffer)
        } else {
            let (cur, prev) = select_motion_streams(
                batch.deformed,
                deformed,
                prev_deformed,
                batch.mesh.vertex_buffer(),
            );
            (cur, prev, batch.mesh.index_buffer())
        };
        // SAFETY: the ash seam. The bound streams outlive the recorded command (pinned by
        // the batch `Arc` / the frame's `Skinning` / `TransientResources`); the index buffer + draw
        // cover the batch.
        unsafe {
            raw.cmd_bind_vertex_buffers(cmd, 0, &[cur, prev], &[0, 0]);
            raw.cmd_bind_index_buffer(cmd, index_buffer, 0, vk::IndexType::UINT32);
        }
        record_batch_submeshes(raw, cmd, batch, None);
    }
}

/// Selects the motion pass's (current, previous) position streams for one batch. Binding 0
/// is this frame's position, binding 1 the previous frame's. A deforming batch (skin or
/// morph) with both deformed buffers present reads the current + prev deformed buffers; every
/// other batch binds the same static stream to both (prev == cur, so motion comes purely from
/// `prev_model`). A morph-only batch (`deformed == true`) takes the first arm exactly like a
/// skinned batch — only the buffers the host wired differ.
fn select_motion_streams(
    deformed_flag: bool,
    deformed: Option<vk::Buffer>,
    prev_deformed: Option<vk::Buffer>,
    static_buffer: vk::Buffer,
) -> (vk::Buffer, vk::Buffer) {
    match (deformed_flag, deformed, prev_deformed) {
        (true, Some(deformed), Some(prev_deformed)) => (deformed, prev_deformed),
        _ => (static_buffer, static_buffer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deforming batch (skin or morph) with both deformed buffers present binds the current
    /// and previous deformed streams to motion bindings 0/1; a static batch — or a deforming
    /// batch missing a deformed buffer — binds the static stream to both. A morph-only batch
    /// with `deformed == true` takes the deforming arm exactly like a skinned batch.
    #[test]
    fn select_motion_streams_keys_on_the_deform_flag() {
        use ash::vk::Handle;
        let static_buf = vk::Buffer::from_raw(0x5747);
        let deformed = vk::Buffer::from_raw(0xDEF0);
        let prev = vk::Buffer::from_raw(0xDEF1);

        // Morph-only / skinned: both deformed buffers present → cur + prev deformed.
        assert_eq!(
            select_motion_streams(true, Some(deformed), Some(prev), static_buf),
            (deformed, prev),
            "a deforming batch binds the current + prev deformed buffers"
        );
        // Static batch: the static stream on both bindings.
        assert_eq!(
            select_motion_streams(false, Some(deformed), Some(prev), static_buf),
            (static_buf, static_buf),
            "a static batch binds the static stream to both"
        );
        // Deforming batch but the deformed buffers were never grown this frame → static.
        assert_eq!(
            select_motion_streams(true, None, None, static_buf),
            (static_buf, static_buf),
            "a deforming batch with no deformed buffers falls back to the static stream"
        );
    }

    /// `clamp_sample_count` returns the largest supported count ≤ requested, `TYPE_1` when
    /// none. Pure logic, runs on any host.
    #[test]
    fn clamp_sample_count_picks_largest_supported_le_requested() {
        let all = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_4
            | vk::SampleCountFlags::TYPE_8;
        // The exact requested count when supported.
        assert_eq!(
            clamp_sample_count(all, vk::SampleCountFlags::TYPE_4),
            vk::SampleCountFlags::TYPE_4
        );
        // Requesting 8 with only ≤4 supported clamps down to 4.
        let upto4 = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_4;
        assert_eq!(
            clamp_sample_count(upto4, vk::SampleCountFlags::TYPE_8),
            vk::SampleCountFlags::TYPE_4
        );
        // Requesting 8 with a hole at 4 falls to 2.
        let two_eight = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_8;
        assert_eq!(
            clamp_sample_count(two_eight, vk::SampleCountFlags::TYPE_4),
            vk::SampleCountFlags::TYPE_2
        );
        // Nothing above 1× supported → 1×.
        assert_eq!(
            clamp_sample_count(vk::SampleCountFlags::TYPE_1, vk::SampleCountFlags::TYPE_8),
            vk::SampleCountFlags::TYPE_1
        );
    }

    /// Requesting MSAA + FXAA + TAA together yields MSAA only (MSAA wins); `set(0, true,
    /// true)` yields FXAA only (FXAA beats TAA when no MSAA); `set(0, false, true)` yields
    /// TAA. Mutual exclusivity in one place.
    #[test]
    fn set_aa_enforces_mutual_exclusivity() {
        let all = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_4
            | vk::SampleCountFlags::TYPE_8;

        // MSAA + FXAA + TAA all requested → MSAA only.
        let mut aa = Aa::new(all);
        let changed = aa.set(4, true, true);
        assert!(changed, "selecting 4× from off changes the sample count");
        assert!(aa.msaa());
        assert_eq!(aa.sample_count(), vk::SampleCountFlags::TYPE_4);
        assert!(!aa.fxaa(), "MSAA forces FXAA off");
        assert!(!aa.taa(), "MSAA forces TAA off");
        assert_eq!(aa.mode(), "msaa4");

        // FXAA + TAA both requested, no MSAA → FXAA only.
        let mut aa = Aa::new(all);
        let changed = aa.set(0, true, true);
        assert!(!changed, "staying at 1× does not change the sample count");
        assert!(!aa.msaa());
        assert!(aa.fxaa());
        assert!(!aa.taa(), "FXAA beats TAA when no MSAA");
        assert_eq!(aa.mode(), "fxaa");

        // TAA only.
        let mut aa = Aa::new(all);
        aa.set(0, false, true);
        assert!(aa.taa());
        assert!(!aa.fxaa());
        assert!(!aa.msaa());
        assert_eq!(aa.mode(), "taa");

        // Off.
        let mut aa = Aa::new(all);
        aa.set(0, false, false);
        assert_eq!(aa.mode(), "off");
        assert!(!aa.msaa() && !aa.fxaa() && !aa.taa());
    }

    /// Switching MSAA → MSAA at a different count reports a sample-count change (the caller
    /// clears the PSO cache); switching MSAA → FXAA reports a change (8× → 1×); FXAA → TAA
    /// reports none (both 1×). The cache-clear signal is exactly the sample-count delta.
    #[test]
    fn set_aa_reports_sample_count_change_for_pso_cache_clear() {
        let all = vk::SampleCountFlags::TYPE_1
            | vk::SampleCountFlags::TYPE_2
            | vk::SampleCountFlags::TYPE_4
            | vk::SampleCountFlags::TYPE_8;
        let mut aa = Aa::new(all);
        assert!(aa.set(2, false, false), "off → 2× changes the count");
        assert!(aa.set(8, false, false), "2× → 8× changes the count");
        assert!(!aa.set(8, false, false), "8× → 8× does not");
        assert!(aa.set(0, true, false), "8× → FXAA drops to 1× (a change)");
        assert!(
            !aa.set(0, false, true),
            "FXAA → TAA stays 1× (no count change)"
        );
    }

    /// The motion format (rg16f) and the std430 push sizes are pinned.
    #[test]
    fn push_layouts_match_shaders() {
        assert_eq!(MOTION_FORMAT, vk::Format::R16G16_SFLOAT);
        assert_eq!(size_of::<MotionPush>(), 128);
        assert_eq!(size_of::<TaaPush>(), 80);
    }

    /// The resolution-aware jitter phase count: `8` at 1:1, `ceil(8·n²)` under upscale
    /// (`32` at n = 2, a 0.5 render scale), and monotonic non-decreasing as the input shrinks.
    #[test]
    fn jitter_phase_count_scales_with_upscale() {
        let display = vk::Extent2D {
            width: 1920,
            height: 1080,
        };
        let at = |w: u32, h: u32| {
            jitter_phase_count(
                vk::Extent2D {
                    width: w,
                    height: h,
                },
                display,
            )
        };
        assert_eq!(at(1920, 1080), TAA_JITTER_PHASES); // 1:1 → 8
        assert_eq!(at(960, 540), 32); // n = 2 → ceil(8·4) = 32
        let mut prev = 0;
        for w in [1920u32, 1440, 1280, 960, 640, 480] {
            let count = at(w, w * 1080 / 1920);
            assert!(
                count >= prev,
                "phase count must not decrease as input shrinks"
            );
            prev = count;
        }
    }

    /// The Halton radical-inverse matches the known base-2 (1/2, 1/4, 3/4, 1/8) and base-3
    /// (1/3, 2/3, 1/9) sequences for 1-based indices.
    #[test]
    fn halton_matches_known_radical_inverse() {
        for (i, expected) in [(1u32, 0.5), (2, 0.25), (3, 0.75), (4, 0.125)] {
            assert!((halton(i, 2) - expected).abs() < 1e-6, "halton({i}, 2)");
        }
        for (i, expected) in [(1u32, 1.0 / 3.0), (2, 2.0 / 3.0), (3, 1.0 / 9.0)] {
            assert!((halton(i, 3) - expected).abs() < 1e-6, "halton({i}, 3)");
        }
    }

    /// Every jitter phase stays within ±1/dim (±0.5 px) on each axis, for the whole cycle.
    #[test]
    fn jitter_offset_stays_within_half_pixel() {
        let (w, h) = (1920u32, 1080u32);
        for index in 0..TAA_JITTER_PHASES {
            let o = jitter_offset(index, w, h);
            assert!(o.x.abs() <= 1.0 / w as f32 + 1e-6, "phase {index} x");
            assert!(o.y.abs() <= 1.0 / h as f32 + 1e-6, "phase {index} y");
        }
    }
}
