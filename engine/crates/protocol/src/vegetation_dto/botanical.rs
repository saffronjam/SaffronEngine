use crate::{PlantGraftSourceDto, PlantSourceSelectorDto, Uuid, VegetationGuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A botanical element class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BotanicalElementDto {
    Trunk,
    Branch,
    Root,
    Vine,
    Frond,
    Leaf,
    Needle,
    Blade,
    Flower,
    Fruit,
    Bud,
    Scar,
    DeadPart,
}

/// How child attachments are arranged around a parent axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PhyllotaxisPatternDto {
    Alternate,
    Opposite,
    Whorled,
    Spiral,
}

/// Which way a tropism bends an axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TropismKindDto {
    Phototropism,
    Gravitropism,
    Thigmotropism,
}

/// Which axes a prune rule removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PruneRuleDto {
    BelowHeight,
    ShorterThan,
    KeepStrongest,
}

/// One point of a taper curve: where along the axis, and the radius factor there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalCurvePointDto {
    /// Position along the axis, as `UnitInterval` bits.
    pub at: u16,
    /// Radius factor, as Q15.16 bits.
    pub factor_bits: i32,
}

/// One point of a hand-drawn spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalDrawnPointDto {
    /// Position in family-local metres, as Q15.16 bits.
    pub position_bits: [i32; 3],
    /// Radius there, as Q15.16 bits.
    pub radius_bits: i32,
}

/// One typed botanical operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum BotanicalOperatorDto {
    Drawn {
        element: BotanicalElementDto,
        points: Vec<BotanicalDrawnPointDto>,
    },
    Trunk {
        element: BotanicalElementDto,
        length_bits: i32,
        base_radius_bits: i32,
        taper: Vec<BotanicalCurvePointDto>,
        segments: u32,
    },
    Branch {
        element: BotanicalElementDto,
        length_ratio: u16,
        radius_ratio: u16,
        declination: u16,
        jitter: u16,
        segments: u32,
    },
    Phyllotaxis {
        pattern: PhyllotaxisPatternDto,
        count: u32,
        nodes: u32,
        start: u16,
        end: u16,
        divergence: u16,
    },
    Tropism {
        kind_of: TropismKindDto,
        strength: u16,
    },
    Prune {
        rule: PruneRuleDto,
        threshold_bits: i32,
        count: u32,
    },
    Roots {
        depth_ratio: u16,
        spread_ratio: u16,
        count: u32,
    },
    Shell {
        material_slot: u32,
        sides: u32,
    },
    Instance {
        element: BotanicalElementDto,
        material_slot: u32,
        size_bits: i32,
        jitter: u16,
    },
    /// Grows another `.splant` module at each incoming frame; the call GUID names the family's
    /// module reference that says which module, which of its variations, and what scale.
    ModuleCall {
        /// Call-site GUID as a canonical 32-hex-digit string.
        call_guid: VegetationGuid,
    },
    Family,
}

/// One node of a botanical graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalNodeDto {
    /// Stable node GUID as a decimal `u128`.
    pub guid: String,
    pub version: u32,
    pub semantic_revision: u32,
    pub operator: BotanicalOperatorDto,
}

/// One directed typed edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalEdgeDto {
    pub from_node: String,
    pub from_pin: String,
    pub to_node: String,
    pub to_pin: String,
}

/// What one manual edit does to its target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum BotanicalEditActionDto {
    Transform {
        /// Translation in family-local metres, as Q15.16 bits.
        offset_bits: [i32; 3],
        /// Turn about the target's base, as `UnitInterval` bits.
        roll: u16,
        /// Uniform scale as Q15.16 bits, where 65536 is unchanged.
        scale_bits: i32,
    },
    Trim {
        /// Where along the axis the cut falls, as `UnitInterval` bits.
        at: u16,
    },
    Remove,
    Graft {
        /// The family graft source supplying the geometry.
        source: VegetationGuid,
        /// Which of that source's elements to take.
        selector: PlantSourceSelectorDto,
    },
}

/// One manual edit laid over what the graph grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalManualEditDto {
    /// Element identity the edit addresses, as a decimal `u128`.
    pub target: String,
    pub action: BotanicalEditActionDto,
}

/// Why an authored edit found nothing to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BotanicalEditOrphanReasonDto {
    TargetMissing,
    TargetKind,
    TargetRemoved,
}

/// One edit that did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalEditOrphanDto {
    /// Element identity the edit addressed, as a decimal `u128`.
    pub target: String,
    pub action: BotanicalEditActionDto,
    pub reason: BotanicalEditOrphanReasonDto,
}

/// One individual a graph grows: a seed, an intrinsic age, and a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalVariationDto {
    /// Seed selecting the individual, as a decimal `u128`.
    pub seed: String,
    /// Intrinsic age as `UnitInterval` bits, where 65535 is fully grown.
    pub age: u16,
    /// Artist-facing name.
    pub name: String,
}

/// One native plant family's botanical graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalGraphDto {
    /// The individuals the graph grows, in authored order; the first is the representative one and
    /// each becomes a family variation.
    pub variations: Vec<BotanicalVariationDto>,
    pub nodes: Vec<BotanicalNodeDto>,
    pub edges: Vec<BotanicalEdgeDto>,
    /// Manual edits laid over what the nodes grow, in canonical target order.
    #[serde(default)]
    pub edits: Vec<BotanicalManualEditDto>,
}

/// Replaces one native plant family's botanical graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraphSetParams {
    pub plant: crate::AssetSelector,
    pub graph: BotanicalGraphDto,
    /// External hero meshes the graph's grafts name, in canonical identity order.
    #[serde(default)]
    pub grafts: Vec<PlantGraftSourceDto>,
}

/// The graph a plant family carries, and what it grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraphResult {
    pub plant: Uuid,
    pub graph: BotanicalGraphDto,
    /// External hero meshes the graph's grafts name.
    pub grafts: Vec<PlantGraftSourceDto>,
    pub growth: BotanicalGrowthDto,
}

/// What one grown botanical graph produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalGrowthDto {
    /// Exact graph identity.
    pub graph: String,
    /// Which declared variation the report describes.
    pub variation: u32,
    /// Seed the individual grew from.
    pub seed: String,
    /// Intrinsic age it grew at, as `UnitInterval` bits.
    pub age: u16,
    /// Variations the graph declares.
    pub variations: u32,
    /// Grown axes.
    pub axes: u32,
    /// Attachment frames carrying a placed element.
    pub frames: u32,
    /// Swept shells.
    pub shells: u32,
    /// Instanced elements.
    pub elements: u32,
    /// Generated vertices.
    pub vertices: u32,
    /// Generated triangles.
    pub triangles: u32,
    /// Whether a preview bound stopped the walk before the graph finished. A truncated report
    /// describes a prefix of the plant, never a different one, and never what a cook would write.
    pub truncated: bool,
    /// Semantic parts the family declares.
    pub parts: u32,
    /// Structural spines.
    pub spines: u32,
    /// Family height in Q15.16 metres.
    pub height_bits: i32,
    /// Hero meshes grafted over generated elements. Their geometry is resolved by the cooker, so
    /// the vertex and triangle counts above are the generated surface alone.
    pub grafts: u32,
    /// Manual edits that found their target.
    pub applied_edits: u32,
    /// Manual edits that did not, and why.
    pub orphans: Vec<BotanicalEditOrphanDto>,
}

/// One grown axis, as the authoring surface addresses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalAxisDto {
    /// Stable element identity as a decimal `u128`, which an edit targets.
    pub id: String,
    /// Parent axis identity, absent for a trunk or a drawn spine.
    pub parent: Option<String>,
    /// Attachment frame it grew from, absent when it grew from a base rather than a frame.
    pub frame: Option<String>,
    pub element: BotanicalElementDto,
    /// Base position in family-local metres, as Q15.16 bits.
    pub base_bits: [i32; 3],
    /// Tip position in family-local metres, as Q15.16 bits.
    pub tip_bits: [i32; 3],
    /// Radius at the base, as Q15.16 bits.
    pub base_radius_bits: i32,
    /// Rest points along the axis.
    pub points: u32,
}

/// One placed instanced element, as the authoring surface addresses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalPlacementDto {
    /// Stable element identity as a decimal `u128`, which an edit targets.
    pub id: String,
    /// Frame it sits on, as a decimal `u128`.
    pub frame: String,
    pub element: BotanicalElementDto,
    pub material_slot: u32,
    /// Position in family-local metres, as Q15.16 bits.
    pub position_bits: [i32; 3],
    /// Size in metres, as Q15.16 bits.
    pub size_bits: i32,
    /// Roll about the frame, as `UnitInterval` bits.
    pub roll: u16,
}
