use super::coerce;
use crate::{Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The anti-aliasing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AaModeDto {
    Off,
    Fxaa,
    Taa,
    Msaa2,
    Msaa4,
    Msaa8,
}

/// The global-illumination mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GiModeDto {
    Off,
    Ddgi,
}

/// Debug render-output mode (read back via render-stats; transient, not persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ViewModeDto {
    Lit,
    Unlit,
    Wireframe,
    LitWireframe,
    DetailLighting,
    LightingOnly,
    Reflections,
    Albedo,
    Normal,
    Roughness,
    Metallic,
    Emissive,
    Depth,
    AmbientOcclusion,
    Gi,
    LightComplexity,
    MotionVectors,
    Fog,
    CloudDensity,
    ShadowPages,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VsmPageBudgetParams {
    /// Shadow pages a frame may render; clamped to at least one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VsmPageBudgetResult {
    /// The budget now in force.
    pub pages: u32,
}

/// Params of `page-request-budget`: how many missing-page requests one view class may
/// raise in a frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageRequestBudgetParams {
    /// The per-class budget to set, clamped to a usable region. Omit to read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<u32>,
}

/// Reply of `page-request-budget`: the budget now in force and the region it sits in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageRequestBudgetResult {
    /// Requests one class may raise per frame.
    pub entries: u32,
    /// The allocated per-class region, which the budget may be lowered below but never
    /// raised above.
    pub capacity: u32,
}

/// The temporal-upsampling render-scale surface: the fixed input:display ratio, the dynamic
/// toggle + budget the frame controller drives it with, and the live input/display extents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpscaleDto {
    /// Fixed input:display render scale in `(0, 1]`; `1.0` = native (no upscaling).
    pub ratio: f32,
    /// Dynamic resolution: the frame-budget controller drives `ratio` toward `target_ms`.
    pub dynamic: bool,
    /// The per-frame budget (ms) the dynamic driver holds to (`= 1000 / target_fps`).
    pub target_ms: f32,
    /// Current input extent (scene / depth / motion render size) in device pixels.
    pub input_width: u32,
    pub input_height: u32,
    /// Fixed display extent the resolve reconstructs to (the present size), in device pixels.
    pub display_width: u32,
    pub display_height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetUpscaleResult {
    pub upscale: UpscaleDto,
}

/// Partial update of the upscale surface — every field `Option`, so an omitted field keeps its prior
/// value (`{ "ratio": 0.67 }` pins the ratio without touching `dynamic` / `target_ms`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetUpscaleParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_ms: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetUpscaleResult {
    pub upscale: UpscaleDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAaParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AaModeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAaResult {
    pub aa: AaModeDto,
}

/// The runtime TAA resolve tuning (mirrors `saffron_rendering::TaaParams`): the adaptive
/// feedback range, the velocity-rejection scale, the YCoCg variance-clip gamma, and the
/// RCAS sharpen strength.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaaParamsDto {
    pub feedback_min: f32,
    pub feedback_max: f32,
    pub velocity_rejection: f32,
    pub clip_gamma: f32,
    pub sharpness: f32,
}

/// `get-taa-params` reply: the current TAA blend/sharpen parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetTaaParamsResult {
    pub params: TaaParamsDto,
}

/// `set-taa-params` request: a partial update — each present field overwrites, each omitted
/// field keeps its current value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTaaParamsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub velocity_rejection: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_gamma: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<f32>,
}

/// `set-taa-params` reply: the fully-resolved parameters after the merge (echo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTaaParamsResult {
    pub params: TaaParamsDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewModeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ViewModeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewModeResult {
    pub view_mode: ViewModeDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToggleParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetClusteredResult {
    pub clustered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetIblResult {
    pub ibl: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkyOcclusionResult {
    pub sky_occlusion: bool,
}

/// Result of `set-gdf`: the resolved Global-Distance-Field enable state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGdfResult {
    pub gdf: bool,
}

/// Params for `set-render-quality`: the tier name (`low`/`medium`/`high`/`ultra`/`custom`) — the
/// single knob for the SSGI / GTAO / contact-shadow stack (replacing the per-effect toggles).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRenderQualityParams {
    pub tier: String,
}

/// Params for `set-tonemap`: the operator name (`reinhard`/`aces`/`agx`/`pbr-neutral`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTonemapParams {
    pub mode: String,
}

/// The applied tonemap operator, echoed by `set-tonemap`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TonemapResult {
    pub mode: String,
}

/// Params for `set-viewport-power-state`: the editor's window visibility
/// (`focused`/`unfocused`/`occluded`), so the host can suppress rendering a hidden viewport.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportPowerStateParams {
    pub state: String,
}

/// The applied viewport power state, echoed by `set-viewport-power-state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewportPowerStateResult {
    pub state: String,
}

/// The active render-quality tier + the resolved per-effect state, returned by both
/// `set-render-quality` and `get-render-quality`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderQualityResult {
    /// The active tier name.
    pub tier: String,
    /// Whether screen-space one-bounce GI is on at this tier.
    pub ssgi: bool,
    /// Whether GTAO ambient occlusion is on at this tier.
    pub gtao: bool,
    /// Whether screen-space contact shadows are on at this tier.
    pub contact_shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRtShadowsResult {
    pub rt_shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRestirResult {
    pub restir: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSsrResult {
    pub ssr: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRtReflectionsResult {
    pub rt_reflections: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGiParams {
    pub mode: GiModeDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGiResult {
    pub ddgi: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetShadowsResult {
    pub shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkinningResult {
    pub skinning: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetDisplacementResult {
    pub displacement: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetDepthPrepassResult {
    pub depth_prepass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewportNativeInfoResult {
    pub platform: String,
    pub transport: String,
    pub status: String,
    pub control_socket: String,
    pub width: i32,
    pub height: i32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportSizeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportSizeResult {
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetActiveViewParams {
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetActiveViewResult {
    pub view: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetProbesParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetProbesResult {
    pub probes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecaptureProbesResult {
    pub marked: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProbeRef {
    pub slot: u32,
    pub entity: Uuid,
    pub origin: Vec3,
    pub influence_radius: f32,
    pub intensity: f32,
    pub box_projection: bool,
    pub valid: bool,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListProbesResult {
    pub enabled: bool,
    pub count: u32,
    pub probes: Vec<ProbeRef>,
}

/// Params for `set-tessellation-quality` — the runtime displacement-tessellation budget. Every field is
/// optional so a caller can tune one knob without disturbing the others; omitted fields are unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTessellationQualityParams {
    /// Hard per-instance dice cap (max subdivision factor, integer-rounded). Clamped to `[1, 2048]`;
    /// the split pass expresses it and the micro-vertex budget coarsens dense scenes.
    pub factor_cap: Option<f32>,
    /// Minimum per-edge factor (never dice coarser than this). Clamped to `[1, factor_cap]`.
    pub min_factor: Option<f32>,
    /// Target screen-space edge length in pixels (smaller ⇒ denser tessellation). Clamped to `≥ 1`.
    pub edge_length_target: Option<f32>,
}

/// The tessellation-quality budget after applying (and clamping) a `set-tessellation-quality` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTessellationQualityResult {
    pub factor_cap: f32,
    pub min_factor: f32,
    pub edge_length_target: f32,
}

/// Which hierarchy cut a view draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum HierarchyCutDto {
    /// Projected appearance error chooses the cut, which is what a shipped frame does.
    Auto,
    /// Never refine: the coarsest cut, aggregate voxels.
    Coarse,
    /// Always refine: the finest cut, triangle clusters.
    Fine,
}

/// Which view's hierarchy cut a `set-hierarchy-cut` call addresses; each view carries its own pin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum HierarchyCutViewDto {
    /// The camera, whose cut is the image.
    #[default]
    Camera,
    /// The shadow-atlas page views.
    Shadow,
    /// The global-illumination reach view.
    Gi,
}

/// Params of `set-hierarchy-cut`: pin one view's cut, or return it to following
/// projected error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SetHierarchyCutParams {
    /// The cut to pin. Omit to read the current one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<HierarchyCutDto>,
    /// The view to address. Omit for the camera.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<HierarchyCutViewDto>,
}

/// Reply of `set-hierarchy-cut`: the cut the addressed view now draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct HierarchyCutResult {
    /// The view the cut belongs to.
    pub view: HierarchyCutViewDto,
    /// The cut that view now draws.
    pub cut: HierarchyCutDto,
}
