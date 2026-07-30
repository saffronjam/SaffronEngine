use crate::{PlantId, WorldCellDto};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which resident plant to capture the wind prepass record of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationWindRecordParams {
    pub cell: WorldCellDto,
    pub plant: PlantId,
}

/// One plant's wind prepass record, read back from the GPU, beside the authored family response
/// that produced it. Every raster pass applies these stored words rather than re-evaluating wind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VegetationWindRecordResult {
    /// The GPU-scene instance slot the plant mirrors to.
    pub slot: u32,
    /// World-space sway displacement at this frame's time.
    pub sway_current_m: [f32; 3],
    /// World-space sway displacement at the previous frame's time, which motion vectors read.
    pub sway_previous_m: [f32; 3],
    /// Interaction-field displacement at this frame's time.
    pub interaction_current_m: [f32; 3],
    /// Interaction-field displacement at the previous frame's time.
    pub interaction_previous_m: [f32; 3],
    /// True when the interaction cascade covering this plant changed since the previous frame, so
    /// the displacement above jumped rather than moved and the plant is not reprojected.
    pub interaction_reset: bool,
    /// The branch mode's `sin`/`cos` at the current time then the previous time; the vertex path
    /// applies a per-use phase offset through this quadrature.
    pub branch_quadrature: [f32; 4],
    /// Branch-mode bend amplitude in metres at the pivot lever.
    pub branch_amplitude_m: f32,
    /// Leaf flutter amplitude in metres.
    pub flutter_amplitude_m: f32,
    /// Reciprocal of the plant's local top height — the vertex path's height weight.
    pub height_scale: f32,
    /// World-space slack the visibility cull adds to the instance sphere and to every
    /// node box, covering the sway, interaction, and mode amplitudes above.
    pub bounds_inflation_m: f32,
    /// The family's authored response, or `None` when the prototype carries none and the
    /// prepass derived everything from the plant's height.
    pub mechanics: Option<VegetationMechanicsDto>,
}

/// A plant family's authored wind and bend response, as the GPU reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VegetationMechanicsDto {
    /// Bending stiffness: raises the branch frequency as its square root and divides the
    /// amplitude.
    pub stiffness: f32,
    /// Aerodynamic drag: scales how hard the field pushes.
    pub drag: f32,
    /// High-frequency flutter response, scaling the leaf term.
    pub flutter: f32,
    /// Damping in `[0, 1]`, bleeding amplitude.
    pub damping: f32,
    /// Maximum bend as a `[0, 1]` fraction; zero is unlimited.
    pub bend_limit: f32,
}

/// Params of `vegetation-budgets`: the resident-population budgets whose breaches raise alarms
/// naming the cell or family that broke them. Omit a field to leave it alone; omit all to read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBudgetsParams {
    /// Mirrored plants one cell may hold; zero disables the budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_plants: Option<u32>,
    /// Mirrored instances one family may hold across every resident cell; zero disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_instances: Option<u32>,
    /// Cooked blade-candidate upper bound one family may contribute; zero disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_micro_predicted: Option<String>,
}

/// Reply of `vegetation-budgets`: the budgets now in force.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBudgetsResult {
    /// Mirrored plants one cell may hold.
    pub cell_plants: u32,
    /// Mirrored instances one family may hold.
    pub family_instances: u32,
    /// Cooked blade-candidate upper bound one family may contribute.
    pub family_micro_predicted: String,
}
