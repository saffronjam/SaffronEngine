use crate::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetExposureParams {
    pub ev: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetExposureResult {
    pub exposure_ev: f32,
}

/// One masked correction range (Shadows / Midtones / Highlights) of the grade: an ASC-CDL SOP triplet
/// plus a saturation and a contrast, blended by a smooth luma mask. Neutral is slope `[1, 1, 1]`,
/// offset `[0, 0, 0]`, power `[1, 1, 1]`, saturation/contrast `1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GradeRangeDto {
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

impl Default for GradeRangeDto {
    /// The identity range (a zeroed derive would crush the range to black).
    fn default() -> Self {
        Self {
            slope: [1.0, 1.0, 1.0],
            offset: [0.0, 0.0, 0.0],
            power: [1.0, 1.0, 1.0],
            saturation: 1.0,
            contrast: 1.0,
        }
    }
}

/// The split-tone block of the grade: a shadow tint + a highlight tint blended by luma, biased by a
/// balance knob. Neutral tints are `[0.5, 0.5, 0.5]` (a 1× multiply) with balance `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SplitToneDto {
    /// Shadow tint; `[0.5, 0.5, 0.5]` neutral.
    pub shadow: [f32; 3],
    /// Highlight tint; `[0.5, 0.5, 0.5]` neutral.
    pub highlight: [f32; 3],
    /// Luma pivot bias; `0.0` neutral.
    pub balance: f32,
}

impl Default for SplitToneDto {
    /// The neutral split (0.5 tints multiply by 1).
    fn default() -> Self {
        Self {
            shadow: [0.5, 0.5, 0.5],
            highlight: [0.5, 0.5, 0.5],
            balance: 0.0,
        }
    }
}

/// The scene-linear grade folded into the tonemap pass before the view transform: global ASC-CDL
/// slope/offset/power, three masked tonal ranges, a row-major 3x3 channel mixer, and split-toning.
/// Flat and `#[serde(default)]` over a neutral identity, so unspecified fields stay neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct SetColorGradingParams {
    /// White-balance temperature in Kelvin; `6500` neutral.
    pub temperature: f32,
    /// White-balance tint: green (`-`) / magenta (`+`).
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
    pub shadows: GradeRangeDto,
    /// The midtones correction range.
    pub midtones: GradeRangeDto,
    /// The highlights correction range.
    pub highlights: GradeRangeDto,
    /// Luma where the shadow mask reaches zero (`~0.09`).
    pub shadows_max: f32,
    /// Luma where the highlight mask begins to rise (`~0.5`).
    pub highlights_min: f32,
    /// Row-major 3×3 channel mixer; identity by default.
    pub channel_mixer: [f32; 9],
    /// The split-tone block.
    pub split_tone: SplitToneDto,
    /// The display-space creative `.cube` look asset (`0` = none), sampled tetrahedrally after the
    /// view transform. Matches the bloom-dirt convention (`0` clears).
    pub creative_lut_asset: Uuid,
    /// The creative-look intensity in `[0, 1]` (`0` = neutral).
    pub creative_lut_intensity: f32,
}

impl Default for SetColorGradingParams {
    /// The neutral identity grade — the passthrough that renders an ungraded frame unchanged.
    fn default() -> Self {
        Self {
            temperature: 6500.0,
            tint: 0.0,
            contrast: 1.0,
            pivot: 0.18,
            saturation: 1.0,
            slope: [1.0, 1.0, 1.0],
            offset: [0.0, 0.0, 0.0],
            power: [1.0, 1.0, 1.0],
            shadows: GradeRangeDto::default(),
            midtones: GradeRangeDto::default(),
            highlights: GradeRangeDto::default(),
            shadows_max: 0.09,
            highlights_min: 0.5,
            channel_mixer: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            split_tone: SplitToneDto::default(),
            creative_lut_asset: Uuid(0),
            creative_lut_intensity: 0.0,
        }
    }
}

/// The applied color grade, echoed by `set-color-grading` (the same flat fields the panel reads back
/// from `render-stats`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetColorGradingResult {
    pub temperature: f32,
    pub tint: f32,
    pub contrast: f32,
    pub pivot: f32,
    pub saturation: f32,
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub shadows: GradeRangeDto,
    pub midtones: GradeRangeDto,
    pub highlights: GradeRangeDto,
    pub shadows_max: f32,
    pub highlights_min: f32,
    pub channel_mixer: [f32; 9],
    pub split_tone: SplitToneDto,
    pub creative_lut_asset: Uuid,
    pub creative_lut_intensity: f32,
}

/// The resolved creative-look read-back on `render-stats`: the assigned `.cube`/`.slut` asset, its
/// look intensity, and the table resolution the tonemap pass samples (`17`/`33`/`65`). `None` when no
/// look is assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreativeLutStat {
    /// The creative-LUT asset id.
    pub asset: Uuid,
    /// The look intensity in `[0, 1]`.
    pub intensity: f32,
    /// The table resolution per axis.
    pub size: u32,
}

/// Params for `bake-look`: fold the current grade + view transform + creative LUT into one `33³`
/// log2-shaper `.slut` for the exported player. Optional `name` for the baked asset row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct BakeLookParams {
    /// The baked LUT asset's display name (defaults to `"Baked Look"`).
    pub name: Option<String>,
}

/// The `bake-look` result: the written `.slut`'s asset id, project-relative path, and resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BakeLookResult {
    /// The baked LUT asset id.
    pub asset: Uuid,
    /// The project-relative `.slut` path.
    pub path: String,
    /// The baked table resolution per axis (`33`).
    pub size: u32,
}

/// The anamorphic streak: a horizontally-squeezed blur of the bright pyramid added over the radial
/// bloom, with `ratio` the horizontal squeeze and `intensity` the add weight.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnamorphicParams {
    pub enabled: bool,
    pub ratio: f32,
    pub tint: [f32; 3],
    pub intensity: f32,
}

/// The pre-tonemap bloom pyramid plus its art-direction layers. `intensity` is the
/// `lerp(hdr, bloom, intensity)` mix, `scatter` the tent-upsample radius in UV units, `threshold`
/// the soft-knee prefilter (`0.0` = off). The optional fields are a patch: an absent one is
/// unchanged, and `dirtTexture: 0` clears the lens-dirt mask.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetBloomParams {
    pub enabled: bool,
    pub intensity: f32,
    pub scatter: f32,
    pub tint: [f32; 3],
    pub threshold: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirt_texture: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirt_intensity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirt_tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anamorphic: Option<AnamorphicParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_mip_tint: Option<Vec<[f32; 3]>>,
}

/// The applied bloom state, echoed by `set-bloom`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetBloomResult {
    pub enabled: bool,
    pub intensity: f32,
    pub scatter: f32,
    pub tint: [f32; 3],
    pub threshold: f32,
    /// The lens-dirt mask asset id (`0` = none).
    pub dirt_texture: Uuid,
    pub dirt_intensity: f32,
    pub dirt_tint: [f32; 3],
    pub anamorphic: AnamorphicParams,
    pub per_mip_tint: Vec<[f32; 3]>,
}
