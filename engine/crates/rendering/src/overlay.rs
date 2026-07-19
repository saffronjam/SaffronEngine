//! The final post chain: the mandatory HDR→display tonemap, the analytic ground
//! grid, and the editor overlay — the passes that run every frame after the scene +
//! AA passes and complete the offscreen color the present blit / shm publish consume.
//!
//! - **Tonemap** is mandatory: an in-place compute pass on the offscreen color
//!   (`StorageImageRwCompute`, GENERAL layout) that maps the scene's linear HDR
//!   radiance to display range. Exposure is `exp2(exposure_ev)`.
//! - **Grid** is an optional fullscreen depth-tested debug overlay drawn on the 1×
//!   resolved color after tonemap; its fragment reconstructs the world ray from a
//!   push-constant `inv_view_proj` and writes `SV_Depth` so scene geometry occludes
//!   it.
//! - **Overlay** is the editor gizmo (handles + entity billboards): a plain
//!   [`OverlayVertex`] CPU stream uploaded into a grow-only per-frame vertex buffer
//!   and drawn in two ranges — a depth-tested range (camera frustums, occluded) then
//!   an always-on-top range (handles). Composited into the post-tonemap color so the
//!   present-only blit embeds it too.
//!
//! The geometry itself is the host's native gizmo builder (PP-10); this module owns
//! the vertex contract, the per-frame upload buffer, the pushes, and the recorders.

use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};

use crate::Result;
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::resources::{Buffer, DeviceResources};

/// One editor-overlay vertex: a clip-space NDC position, an RGBA color, a feather
/// `edge` vector, and an NDC depth. The vertex stream the `gizmo_overlay` PSO binds.
///
/// `edge.xy` are signed coordinates per feather direction (±1 at the nominal edge),
/// `edge.zw` the matching half-extents in pixels (a non-positive half-extent disables
/// that direction — lines feather one way, filled quads both). `depth` is the NDC z
/// ([0,1]); only the depth-tested range uses it, the on-top range leaves it 0.
///
/// `#[repr(C)]` + [`bytemuck::Pod`]: the host fills this vertex with a `vec2` position +
/// a `vec4` color that aligns the color to offset 16, so [`Vec4`]'s 16-byte alignment
/// reproduces the layout exactly.
/// The attribute offsets the PSO declares come from [`std::mem::offset_of`] on this
/// struct, so the upload and the bindings are self-consistent.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OverlayVertex {
    /// Clip-space NDC position ([-1, 1]).
    pub position: Vec2,
    /// Padding to align `color` to offset 16.
    _pad: [f32; 2],
    /// RGBA color (straight alpha; the pass alpha-blends).
    pub color: Vec4,
    /// `xy` signed edge coords per feather direction, `zw` the pixel half-extents.
    pub edge: Vec4,
    /// NDC z ([0, 1]); 0 = on the near plane (on top). Only the depth-tested range
    /// reads it.
    pub depth: f32,
    /// Tail padding to the 16-byte struct alignment.
    _pad_tail: [f32; 3],
}

const _: () = assert!(size_of::<OverlayVertex>() == 64);
const _: () = assert!(std::mem::offset_of!(OverlayVertex, position) == 0);
const _: () = assert!(std::mem::offset_of!(OverlayVertex, color) == 16);
const _: () = assert!(std::mem::offset_of!(OverlayVertex, edge) == 32);
const _: () = assert!(std::mem::offset_of!(OverlayVertex, depth) == 48);

impl OverlayVertex {
    /// A vertex with the given clip-space position, color, feather edge, and NDC depth
    /// — the constructor the host's gizmo builder uses (the padding fields are zeroed).
    pub fn new(position: Vec2, color: Vec4, edge: Vec4, depth: f32) -> Self {
        Self {
            position,
            _pad: [0.0; 2],
            color,
            edge,
            depth,
            _pad_tail: [0.0; 3],
        }
    }
}

/// The tonemap compute push: exposure, operator, and low-light adaptation, matching
/// `tonemap.slang`'s `Push`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TonemapPush {
    /// The linear exposure multiplier (`exp2(exposure_ev)`).
    pub exposure: f32,
    /// The tonemap operator ([`TonemapMode`] as `u32`).
    pub mode: u32,
    /// Sun-elevation-derived scotopic adaptation strength.
    pub night_factor: f32,
    _pad: f32,
}

const _: () = assert!(size_of::<TonemapPush>() == 16);

/// One masked correction range (Shadows / Midtones / Highlights): an ASC-CDL SOP triplet plus a
/// saturation and a contrast, blended into the frame by a smooth luma weight. Neutral is slope
/// `[1, 1, 1]`, offset `[0, 0, 0]`, power `[1, 1, 1]`, saturation/contrast `1.0` — an identity that
/// leaves the range untouched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradeRange {
    /// ASC-CDL slope (S); `[1, 1, 1]` neutral.
    pub slope: [f32; 3],
    /// ASC-CDL offset (O); `[0, 0, 0]` neutral.
    pub offset: [f32; 3],
    /// ASC-CDL power (P); `[1, 1, 1]` neutral.
    pub power: [f32; 3],
    /// Saturation around Rec.709 luma; `1.0` neutral.
    pub saturation: f32,
    /// Contrast gain around the middle-grey pivot; `1.0` neutral.
    pub contrast: f32,
}

impl Default for GradeRange {
    /// The identity range. Hand-written because a zeroed `power`/`slope` would crush the range to
    /// black; this passthrough leaves a pixel in the range unchanged.
    fn default() -> Self {
        Self {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            saturation: 1.0,
            contrast: 1.0,
        }
    }
}

/// Scene-linear grade state (canonical ASC-CDL SOP+Sat + white balance, three masked ranges, a 3×3
/// channel mixer, and split-toning), applied in the tonemap pass after exposure and before the
/// view/display transform. Exposure is *not* here — it stays the existing [`TonemapPush::exposure`]
/// multiply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGrade {
    /// White-balance temperature in Kelvin; `6500` = neutral (identity).
    pub temperature: f32,
    /// White-balance tint: green (`-`) / magenta (`+`); `0` = neutral.
    pub tint: f32,
    /// Contrast gain around the log2 pivot; `1.0` neutral.
    pub contrast: f32,
    /// The middle-grey contrast pivot (`0.18`).
    pub pivot: f32,
    /// Saturation around Rec.709 luma; `1.0` neutral.
    pub saturation: f32,
    /// ASC-CDL slope (S); `[1, 1, 1]` neutral.
    pub slope: [f32; 3],
    /// ASC-CDL offset (O); `[0, 0, 0]` neutral.
    pub offset: [f32; 3],
    /// ASC-CDL power (P); `[1, 1, 1]` neutral.
    pub power: [f32; 3],
    /// The shadows correction range.
    pub shadows: GradeRange,
    /// The midtones correction range.
    pub midtones: GradeRange,
    /// The highlights correction range.
    pub highlights: GradeRange,
    /// Luma where the shadow mask reaches zero (`~0.09`).
    pub shadows_max: f32,
    /// Luma where the highlight mask begins to rise (`~0.5`).
    pub highlights_min: f32,
    /// Row-major 3×3 channel mixer; identity by default.
    pub channel_mixer: [f32; 9],
    /// Split-tone shadow tint; `[0.5, 0.5, 0.5]` neutral.
    pub split_shadow: [f32; 3],
    /// Split-tone highlight tint; `[0.5, 0.5, 0.5]` neutral.
    pub split_highlight: [f32; 3],
    /// Split-tone luma pivot bias; `0.0` neutral.
    pub split_balance: f32,
}

impl Default for ColorGrade {
    /// The neutral identity grade. Hand-written (not a `derive`) because a zeroed `pivot`/`power`
    /// would render a black frame — this is the passthrough that keeps an ungraded project unchanged.
    fn default() -> Self {
        Self {
            temperature: 6500.0,
            tint: 0.0,
            contrast: 1.0,
            pivot: 0.18,
            saturation: 1.0,
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            shadows: GradeRange::default(),
            midtones: GradeRange::default(),
            highlights: GradeRange::default(),
            shadows_max: 0.09,
            highlights_min: 0.5,
            channel_mixer: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            split_shadow: [0.5; 3],
            split_highlight: [0.5; 3],
            split_balance: 0.0,
        }
    }
}

/// One masked range as three std140 `vec4` rows: `slope.xyz` + saturation in `.w`, `offset.xyz` +
/// contrast in `.w`, `power.xyz` + pad. Matches the `GradeBlockParams` unpack in `tonemap.slang`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradeBlock {
    slope: [f32; 4],
    offset: [f32; 4],
    power: [f32; 4],
}

impl From<&GradeRange> for GradeBlock {
    fn from(r: &GradeRange) -> Self {
        Self {
            slope: [r.slope[0], r.slope[1], r.slope[2], r.saturation],
            offset: [r.offset[0], r.offset[1], r.offset[2], r.contrast],
            power: [r.power[0], r.power[1], r.power[2], 0.0],
        }
    }
}

/// The std140 image of the shader's grade uniform (`GradeUniform` in `tonemap.slang`): the Bradford
/// white-balance matrix as three `vec4` rows, the global slope/offset/power as `xyz` + pad, the
/// contrast/pivot/saturation scalars in one `vec4`, then the three masked ranges, the row-major 3×3
/// channel mixer as three `vec4` rows, the two split-tone tints, and the range knobs. Bound at
/// binding 1 of the tonemap set.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GradeUniform {
    white_balance: [[f32; 4]; 3],
    slope: [f32; 4],
    offset: [f32; 4],
    power: [f32; 4],
    tonal: [f32; 4],
    shadows: GradeBlock,
    midtones: GradeBlock,
    highlights: GradeBlock,
    channel_mixer: [[f32; 4]; 3],
    split_shadow: [f32; 4],
    split_highlight: [f32; 4],
    range_knobs: [f32; 4],
    /// The creative-look block: `x` intensity, `y` LUT size (as `f32`, an exact `2/17/33/65`), `z` the
    /// frozen-look view-mode flag (`0` live ALU host, `1` baked full-tail player), `w` pad.
    look: [f32; 4],
}

const _: () = assert!(size_of::<GradeUniform>() == 368);

impl From<&ColorGrade> for GradeUniform {
    fn from(g: &ColorGrade) -> Self {
        let m = bradford_white_balance(g.temperature, g.tint);
        // The shader reconstructs `float3x3(row0, row1, row2)` (row-major) and does `mul(M, c)`, so
        // each `vec4` here is a matrix row. glam `Mat3` is column-major, so row `i` gathers component
        // `i` of the three column axes.
        let row = |i: usize| [m.x_axis[i], m.y_axis[i], m.z_axis[i], 0.0];
        // The channel mixer travels row-major; each `vec4` here is one output row (`out = M · rgb`).
        let mixer_row = |i: usize| {
            [
                g.channel_mixer[i * 3],
                g.channel_mixer[i * 3 + 1],
                g.channel_mixer[i * 3 + 2],
                0.0,
            ]
        };
        Self {
            white_balance: [row(0), row(1), row(2)],
            slope: [g.slope[0], g.slope[1], g.slope[2], 0.0],
            offset: [g.offset[0], g.offset[1], g.offset[2], 0.0],
            power: [g.power[0], g.power[1], g.power[2], 0.0],
            tonal: [g.contrast, g.pivot, g.saturation, 0.0],
            shadows: GradeBlock::from(&g.shadows),
            midtones: GradeBlock::from(&g.midtones),
            highlights: GradeBlock::from(&g.highlights),
            channel_mixer: [mixer_row(0), mixer_row(1), mixer_row(2)],
            split_shadow: [g.split_shadow[0], g.split_shadow[1], g.split_shadow[2], 0.0],
            split_highlight: [
                g.split_highlight[0],
                g.split_highlight[1],
                g.split_highlight[2],
                0.0,
            ],
            range_knobs: [g.shadows_max, g.highlights_min, g.split_balance, 0.0],
            look: [0.0, 2.0, 0.0, 0.0],
        }
    }
}

/// The baked look table resolution per axis (`33³`), matching the `lut_bake.slang` dispatch and the
/// player's frozen-look fetch.
pub const LUT_BAKE_SIZE: u32 = 33;
/// The log2-shaper EV span the bake maps its grid against (anchored at 18% grey), shared with
/// `tonemap_ops.slang` and written into the `.slut` header.
pub const LUT_SHAPER_EV_MIN: f32 = -14.0;
/// The upper end of the bake's log2-shaper EV span. See [`LUT_SHAPER_EV_MIN`].
pub const LUT_SHAPER_EV_MAX: f32 = 11.0;

impl GradeUniform {
    /// Fills the creative-look block: the display-space LUT `intensity`, the LUT `size` (`2/17/33/65`),
    /// and the `frozen_look` view-mode flag (`false` = the live ALU host, `true` = the baked full-tail
    /// player). Applied after [`GradeUniform::from`] builds the grade rows, once the renderer has
    /// resolved the bound LUT.
    #[must_use]
    pub fn with_look(mut self, intensity: f32, size: u32, frozen_look: bool) -> Self {
        self.look = [intensity, size as f32, u32::from(frozen_look) as f32, 0.0];
        self
    }
}

/// The Bradford chromatic-adaptation matrix in linear Rec.709 that white-balances the scene: it
/// adapts the working white (the `6500 K` reference on the Planckian locus) to the target white set
/// by `temperature` + `tint`, so lowering the temperature warms the image. Neutral
/// (`6500 K`, tint `0`) returns exactly [`Mat3::IDENTITY`] so the default grade is bit-identity.
fn bradford_white_balance(temperature: f32, tint: f32) -> Mat3 {
    if temperature == 6500.0 && tint == 0.0 {
        return Mat3::IDENTITY;
    }
    // Bradford cone-response matrix (XYZ → LMS) and its inverse.
    let bradford = Mat3::from_cols_array(&[
        0.8951, -0.7502, 0.0389, // column 0
        0.2664, 1.7135, -0.0685, // column 1
        -0.1614, 0.0367, 1.0296, // column 2
    ]);
    // Linear sRGB (Rec.709, D65) ⇄ XYZ.
    let xyz_from_rgb = Mat3::from_cols_array(&[
        0.412_456_4,
        0.212_672_9,
        0.019_333_9, // column 0
        0.357_576_1,
        0.715_152_2,
        0.119_192, // column 1
        0.180_437_5,
        0.072_175,
        0.950_304_1, // column 2
    ]);
    let src = planckian_white_xyz(6500.0, 0.0);
    let dst = planckian_white_xyz(temperature, tint);
    let lms_src = bradford * src;
    let lms_dst = bradford * dst;
    let ratio = Mat3::from_diagonal(Vec3::new(
        lms_dst.x / lms_src.x,
        lms_dst.y / lms_src.y,
        lms_dst.z / lms_src.z,
    ));
    let adapt_xyz = bradford.inverse() * ratio * bradford;
    xyz_from_rgb.inverse() * adapt_xyz * xyz_from_rgb
}

/// The XYZ (Y = 1) of the white point at colour temperature `temp` (Kelvin) with a green/magenta
/// `tint` offset. `temp` follows the Planckian-locus approximation (Kim et al.); `tint` shifts the
/// chromaticity along the green(+)/magenta(−) axis.
fn planckian_white_xyz(temp: f32, tint: f32) -> Vec3 {
    let t = temp.clamp(1667.0, 25000.0) as f64;
    let inv = 1.0 / t;
    let x = if t < 4000.0 {
        -0.266_123_9e9 * inv * inv * inv - 0.234_358_9e6 * inv * inv
            + 0.877_695_6e3 * inv
            + 0.179_910
    } else {
        -3.025_846_9e9 * inv * inv * inv
            + 2.107_037_9e6 * inv * inv
            + 0.222_634_7e3 * inv
            + 0.240_390
    };
    let y = if t < 2222.0 {
        -1.106_381_4 * x * x * x - 1.348_110_2 * x * x + 2.185_558_32 * x - 0.202_196_83
    } else if t < 4000.0 {
        -0.954_947_6 * x * x * x - 1.374_185_93 * x * x + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758_0 * x * x * x - 5.873_386_7 * x * x + 3.751_129_97 * x - 0.370_014_83
    };
    // Tint shifts the y chromaticity toward green (+) / magenta (−). A modest, bounded slope keeps
    // the ±1 range within the perceptual green-magenta span.
    let y = (y + f64::from(tint) * 0.05).clamp(1e-4, 0.9);
    let x = x as f32;
    let y = y as f32;
    Vec3::new(x / y, 1.0, (1.0 - x - y) / y)
}

/// The bloom compute push, matching `bloom.slang`'s `Push`. One struct drives all four passes;
/// `pass`/`karis` select the branch, so a single PSO covers the whole pyramid. Fields are grouped
/// so every `float3` sits on a 16-byte boundary followed by its trailing scalar — that packs
/// identically under `#[repr(C)]` here and Slang's std140 push-constant layout (five vec4 rows, 80
/// bytes, no padding either way).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BloomPush {
    /// The bloom tint (composite pass only).
    pub tint: [f32; 3],
    /// The tent-upsample scatter radius in UV units.
    pub filter_radius: f32,
    /// The energy-conserving lerp weight (composite pass only).
    pub intensity: f32,
    /// The soft-knee prefilter threshold; `0.0` = off (the default, thresholdless).
    pub threshold: f32,
    /// The pass selector: `0` downsample, `1` upsample-add, `2` composite, `3` anamorphic streak.
    pub pass: u32,
    /// `1` on the first downsample only (Karis firefly average), `0` otherwise.
    pub karis: u32,
    /// The lens-dirt tint; composite pass only.
    pub dirt_tint: [f32; 3],
    /// The lens-dirt mix (`0.0` = no dirt); composite pass only.
    pub dirt_intensity: f32,
    /// The per-mip contribution tint (identity `1,1,1` when the stack is off); upsample pass only.
    pub mip_tint: [f32; 3],
    /// The anamorphic streak add weight (`0.0` = no streak); composite pass only.
    pub anamorphic_intensity: f32,
    /// The anamorphic streak tint (cool by default); composite pass only.
    pub anamorphic_tint: [f32; 3],
    /// The anamorphic horizontal squeeze (`~2.0`); streak pass only.
    pub anamorphic_ratio: f32,
}

const _: () = assert!(size_of::<BloomPush>() == 80);

impl BloomPush {
    /// A push with identity art-direction: white tints, no dirt/streak, `pass`/`karis` zero. The
    /// per-pass construction in [`crate::Renderer::add_bloom_pass`] fills it via struct-update
    /// syntax, setting only the fields the selected branch reads.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            tint: [1.0; 3],
            filter_radius: 0.0,
            intensity: 0.0,
            threshold: 0.0,
            pass: 0,
            karis: 0,
            dirt_tint: [1.0; 3],
            dirt_intensity: 0.0,
            mip_tint: [1.0; 3],
            anamorphic_intensity: 0.0,
            anamorphic_tint: [1.0; 3],
            anamorphic_ratio: 1.0,
        }
    }
}

impl TonemapPush {
    /// The tonemap push for exposure, operator, and scotopic adaptation.
    pub fn new(exposure_ev: f32, mode: TonemapMode, night_factor: f32) -> Self {
        Self {
            exposure: exposure_ev.exp2(),
            mode: mode as u32,
            night_factor: night_factor.clamp(0.0, 1.0),
            _pad: 0.0,
        }
    }
}

/// The selectable tonemap operator. Wire-encoded by the kebab-case name pair below.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u32)]
pub enum TonemapMode {
    /// Simple Reinhard (flat; for A/B).
    Reinhard = 0,
    /// ACES filmic (Narkowicz) — the engine default.
    #[default]
    Aces = 1,
    /// AgX (graceful highlight compression, hue-preserving).
    Agx = 2,
    /// Khronos PBR Neutral (material-color-preserving; good for thumbnails).
    PbrNeutral = 3,
}

impl TonemapMode {
    /// The wire / CLI name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TonemapMode::Reinhard => "reinhard",
            TonemapMode::Aces => "aces",
            TonemapMode::Agx => "agx",
            TonemapMode::PbrNeutral => "pbr-neutral",
        }
    }

    /// Parses a tonemap-operator name, `None` on an unknown value.
    #[must_use]
    pub fn from_name(name: &str) -> Option<TonemapMode> {
        match name {
            "reinhard" => Some(TonemapMode::Reinhard),
            "aces" => Some(TonemapMode::Aces),
            "agx" => Some(TonemapMode::Agx),
            "pbr-neutral" => Some(TonemapMode::PbrNeutral),
            _ => None,
        }
    }
}

/// The grid push: the camera world→clip + its inverse, both vertex+fragment stage.
/// Two mat4s (128 bytes), matching `grid.slang`'s `GridPush`. The fragment
/// reconstructs the world view ray from `inv_view_proj` and reprojects the ground
/// point through `view_proj` for `SV_Depth`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GridPush {
    /// World → clip (this frame's camera) — for the fragment `SV_Depth`.
    pub view_proj: Mat4,
    /// Clip → world (the inverse) — for the view-ray reconstruction.
    pub inv_view_proj: Mat4,
}

const _: () = assert!(size_of::<GridPush>() == 128);

impl GridPush {
    /// The grid push for `view_proj`, computing the inverse for the view-ray
    /// reconstruction.
    pub fn new(view_proj: Mat4) -> Self {
        Self {
            view_proj,
            inv_view_proj: view_proj.inverse(),
        }
    }
}

/// Per-frame editor-overlay geometry: the vertex list (depth-tested range first, then
/// the always-on-top range) and a grow-only mapped vertex buffer per frame-in-flight.
///
/// The pass body cannot hold `&mut Renderer`, so the buffer is prepared (grown +
/// uploaded) *before* the graph build via [`OverlayState::prepare`]; the pass then
/// captures only the resolved [`vk::Buffer`] handle + the counts (README §2).
/// Single-thread state — only the render thread touches it.
pub struct OverlayState {
    resources: Arc<DeviceResources>,
    /// The combined vertex list: `[0, depth_tested_count)` then the on-top range.
    vertices: Vec<OverlayVertex>,
    /// How many leading vertices are the depth-tested (occluded) range.
    depth_tested_count: u32,
    /// Grow-only mapped vertex buffer per frame-in-flight.
    buffers: [Option<Buffer>; MAX_FRAMES_IN_FLIGHT],
    /// Current capacity (in vertices) of each per-frame buffer.
    capacity: [u32; MAX_FRAMES_IN_FLIGHT],
}

/// What [`OverlayState::prepare`] hands the graph build: the resolved per-frame vertex
/// buffer plus the two draw-range counts. Captured by the overlay pass body.
#[derive(Clone, Copy)]
pub struct OverlayDraw {
    /// The per-frame vertex buffer the pass binds (this frame's uploaded geometry).
    pub buffer: vk::Buffer,
    /// Total vertices in the buffer (depth-tested + on-top).
    pub vertex_count: u32,
    /// Leading vertices in the depth-tested (occluded) range.
    pub depth_tested_count: u32,
}

impl OverlayState {
    /// An empty overlay state owning a clone of the device resources (for the
    /// per-frame buffers' `Drop`). No buffers are allocated until the first non-empty
    /// frame.
    pub fn new(resources: &Arc<DeviceResources>) -> Self {
        Self {
            resources: Arc::clone(resources),
            vertices: Vec::new(),
            depth_tested_count: 0,
            buffers: [const { None }; MAX_FRAMES_IN_FLIGHT],
            capacity: [0; MAX_FRAMES_IN_FLIGHT],
        }
    }

    /// Replaces this frame's overlay geometry: the `depth_tested` range (occluded by
    /// scene geometry) followed by the `on_top` range (always drawn). The
    /// `depth_tested_count` is recorded so the pass draws each range with its own PSO.
    pub fn submit(&mut self, mut depth_tested: Vec<OverlayVertex>, on_top: Vec<OverlayVertex>) {
        self.depth_tested_count = depth_tested.len() as u32;
        depth_tested.extend(on_top);
        self.vertices = depth_tested;
    }

    /// Whether any overlay geometry is queued this frame (the gate for arming the
    /// overlay pass).
    pub fn has_geometry(&self) -> bool {
        !self.vertices.is_empty()
    }

    /// Grows the `frame` slot's vertex buffer to fit the queued geometry and uploads
    /// it, returning the resolved draw info the graph captures — or `None` when no
    /// geometry is queued (the pass is skipped). Done before the graph build so the
    /// pass body captures only the handle (README §2).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the (re)allocation of the per-frame buffer
    /// fails; the prior buffer is left in place on failure.
    pub fn prepare(&mut self, frame: usize) -> Result<Option<OverlayDraw>> {
        let vertex_count = self.vertices.len() as u32;
        if vertex_count == 0 {
            return Ok(None);
        }
        if self.capacity[frame] < vertex_count {
            let bytes = u64::from(vertex_count) * size_of::<OverlayVertex>() as u64;
            // Drop the prior buffer before allocating the replacement so the old
            // allocation frees first (the slot's prior frame already completed).
            self.buffers[frame] = None;
            let buffer = Buffer::new(
                &self.resources,
                bytes,
                vk::BufferUsageFlags::VERTEX_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferHost,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?;
            self.buffers[frame] = Some(buffer);
            self.capacity[frame] = vertex_count;
        }
        let buffer = self.buffers[frame]
            .as_mut()
            .expect("the buffer was just ensured");
        let src = bytemuck::cast_slice::<OverlayVertex, u8>(&self.vertices);
        let dst = buffer.mapped_bytes().expect("overlay buffer is MAPPED");
        dst[..src.len()].copy_from_slice(src);
        Ok(Some(OverlayDraw {
            buffer: buffer.handle(),
            vertex_count,
            depth_tested_count: self.depth_tested_count.min(vertex_count),
        }))
    }
}

/// The final-post-chain pass names that arm this frame, in graph order. The tonemap is
/// mandatory (always present unless its PSO build failed); the grid arms only when shown,
/// the overlay only when geometry is queued. Pure gate logic so the phase's
/// "tonemap-always / grid-overlay-conditional" acceptance test runs without a device —
/// it mirrors the `if let Some(..)` guards in [`crate::Renderer::add_tonemap_pass`] /
/// [`crate::Renderer::add_grid_overlay_passes`].
#[cfg(test)]
pub(crate) fn final_post_pass_names(
    bloom_armed: bool,
    tonemap_built: bool,
    show_grid: bool,
    grid_built: bool,
    has_overlay: bool,
    overlay_built: bool,
) -> Vec<&'static str> {
    let mut names = Vec::new();
    // Bloom composites into scene-linear `color` immediately before the tonemap pass, so it leads
    // the final-post chain when enabled.
    if bloom_armed {
        names.push("bloom-composite");
    }
    if tonemap_built {
        names.push("tonemap");
    }
    if show_grid && grid_built {
        names.push("grid");
    }
    if has_overlay && overlay_built {
        names.push("editor-overlay");
    }
    names
}

/// Records the ground-grid draw: bind the grid PSO + the `view_proj`/`inv_view_proj`
/// push, draw the fullscreen triangle (no vertex buffer). The graph opened the
/// rendering scope + set the viewport/scissor.
pub fn record_grid(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    push: &GridPush,
) {
    // SAFETY: the ash seam. The PSO/layout are valid this frame; the push spans the
    // declared two-mat4 vertex+fragment range; the fullscreen triangle needs no buffer.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        raw.cmd_push_constants(
            cmd,
            layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytemuck::bytes_of(push),
        );
        raw.cmd_draw(cmd, 3, 1, 0, 0);
    }
}

/// Records the editor overlay: bind the uploaded vertex buffer, then draw the
/// depth-tested range (occluded, `overlay_depth` PSO) followed by the on-top range
/// (always drawn, `overlay` PSO). The graph opened the rendering scope + set the
/// viewport/scissor.
pub fn record_overlay(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    draw: &OverlayDraw,
    overlay: vk::Pipeline,
    overlay_depth: vk::Pipeline,
) {
    // SAFETY: the ash seam. The buffer holds this frame's uploaded vertices; the two
    // ranges partition `[0, vertex_count)`; the PSOs are valid this frame.
    unsafe {
        raw.cmd_bind_vertex_buffers(cmd, 0, &[draw.buffer], &[0]);
        if draw.depth_tested_count > 0 {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, overlay_depth);
            raw.cmd_draw(cmd, draw.depth_tested_count, 1, 0, 0);
        }
        if draw.vertex_count > draw.depth_tested_count {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, overlay);
            raw.cmd_draw(
                cmd,
                draw.vertex_count - draw.depth_tested_count,
                1,
                draw.depth_tested_count,
                0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `OverlayVertex` byte layout the host fills: position@0, color@16,
    /// edge@32, depth@48, size 64. A wrong offset is a
    /// silently corrupted vertex stream, so pin each. Pure layout logic, any host.
    #[test]
    fn overlay_vertex_layout_matches_the_host_contract() {
        use std::mem::offset_of;
        assert_eq!(size_of::<OverlayVertex>(), 64);
        assert_eq!(offset_of!(OverlayVertex, position), 0);
        assert_eq!(offset_of!(OverlayVertex, color), 16);
        assert_eq!(offset_of!(OverlayVertex, edge), 32);
        assert_eq!(offset_of!(OverlayVertex, depth), 48);
    }

    /// The tonemap push is `exp2(ev)`: 0 EV → 1.0×, +1 EV → 2.0×, -1 EV → 0.5×. Pure math,
    /// any host.
    #[test]
    fn tonemap_push_is_exp2_of_the_ev() {
        let m = TonemapMode::Aces;
        assert!((TonemapPush::new(0.0, m, 0.0).exposure - 1.0).abs() < 1e-6);
        assert!((TonemapPush::new(1.0, m, 0.0).exposure - 2.0).abs() < 1e-6);
        assert!((TonemapPush::new(-1.0, m, 0.0).exposure - 0.5).abs() < 1e-6);
        assert!((TonemapPush::new(2.0, m, 0.0).exposure - 4.0).abs() < 1e-6);
        assert_eq!(TonemapPush::new(0.0, TonemapMode::Agx, 0.0).mode, 2);
        assert_eq!(TonemapPush::new(0.0, m, 2.0).night_factor, 1.0);
        assert_eq!(size_of::<TonemapPush>(), 16);
    }

    /// `GridPush::new` records `view_proj` and its mathematical inverse — round-tripping
    /// through both is the identity (within float tolerance). The grid fragment relies
    /// on this for the view-ray reconstruction. Pure math, any host.
    #[test]
    fn grid_push_records_view_proj_and_its_inverse() {
        let view_proj = Mat4::perspective_rh(1.0, 1.6, 0.1, 100.0)
            * Mat4::look_at_rh(
                saffron_geometry::glam::Vec3::new(3.0, 4.0, 5.0),
                saffron_geometry::glam::Vec3::ZERO,
                saffron_geometry::glam::Vec3::Y,
            );
        let push = GridPush::new(view_proj);
        let identity = push.view_proj * push.inv_view_proj;
        let diff = (identity - Mat4::IDENTITY).to_cols_array();
        assert!(
            diff.iter().all(|c| c.abs() < 1e-3),
            "view_proj * inv_view_proj should be the identity, got {identity:?}"
        );
        assert_eq!(size_of::<GridPush>(), 128);
    }

    /// The tonemap is always in the graph (mandatory); the grid arms only when shown +
    /// built; the overlay only when geometry is queued + built — in that graph order. Pure
    /// gate logic, any host.
    #[test]
    fn final_post_chain_arms_tonemap_always_grid_and_overlay_conditionally() {
        // Tonemap only: nothing else armed.
        assert_eq!(
            final_post_pass_names(false, true, false, false, false, false),
            vec!["tonemap"]
        );
        // Grid shown + built arms it after the tonemap.
        assert_eq!(
            final_post_pass_names(false, true, true, true, false, false),
            vec!["tonemap", "grid"]
        );
        // Grid shown but its PSO failed to build → not armed (degrades, no panic).
        assert_eq!(
            final_post_pass_names(false, true, true, false, false, false),
            vec!["tonemap"]
        );
        // Overlay geometry queued + built arms it last.
        assert_eq!(
            final_post_pass_names(false, true, false, false, true, true),
            vec!["tonemap", "editor-overlay"]
        );
        // All three armed, in graph order.
        assert_eq!(
            final_post_pass_names(false, true, true, true, true, true),
            vec!["tonemap", "grid", "editor-overlay"]
        );
        // Overlay geometry present but no PSO → not armed.
        assert_eq!(
            final_post_pass_names(false, true, false, false, true, false),
            vec!["tonemap"]
        );
        // Bloom enabled leads the chain, ahead of the tonemap.
        assert_eq!(
            final_post_pass_names(true, true, false, false, false, false),
            vec!["bloom-composite", "tonemap"]
        );
        assert_eq!(
            final_post_pass_names(true, true, true, true, true, true),
            vec!["bloom-composite", "tonemap", "grid", "editor-overlay"]
        );
    }

    /// `submit` lays the depth-tested range first then the on-top range and records the
    /// split count, so one buffer drives both draws, and [`OverlayState::prepare`] grows +
    /// uploads the per-frame buffer. Needs a device (the per-frame buffer is
    /// VMA-allocated); skips when none is present.
    #[test]
    fn submit_lays_depth_tested_first_then_uploads() {
        let device = match crate::device::Device::new(&crate::device::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device ({err})");
                return;
            }
        };
        let before = crate::validation_issue_count();
        let mut state = OverlayState::new(device.resources());
        assert!(!state.has_geometry());
        assert!(
            state.prepare(0).expect("empty prepare").is_none(),
            "no geometry → no draw"
        );

        let depth = vec![OverlayVertex::default(); 3];
        let on_top = vec![OverlayVertex::default(); 6];
        state.submit(depth, on_top);
        assert!(state.has_geometry());
        assert_eq!(state.depth_tested_count, 3);
        assert_eq!(state.vertices.len(), 9);

        // Prepare grows + uploads the per-frame buffer and reports the two draw ranges.
        let draw = state.prepare(0).expect("prepare").expect("draw");
        assert_eq!(draw.vertex_count, 9);
        assert_eq!(draw.depth_tested_count, 3);

        // An empty submit clears the geometry (the gate disarms the pass).
        state.submit(Vec::new(), Vec::new());
        assert!(!state.has_geometry());

        // Drop the state (freeing its buffer) before the device, then idle + teardown.
        drop(state);
        device.wait_idle().expect("idle");
        drop(device);

        let after = crate::validation_issue_count();
        assert_eq!(
            before,
            after,
            "overlay buffer alloc + teardown must be validation-clean (saw {} new)",
            after.saturating_sub(before)
        );
    }
}
