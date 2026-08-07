use crate::{Uuid, VegetationGuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Plant-family source kind shown in catalog summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantSourceKindDto {
    Imported,
    Native,
}

/// Severity of one authored-asset or derived-artifact validation diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationValidationSeverityDto {
    Info,
    Warning,
    Error,
}

/// One stable machine-readable validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationValidationIssueDto {
    pub severity: VegetationValidationSeverityDto,
    pub code: String,
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_selector: Option<String>,
}

/// Complete validation state for one vegetation asset or artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationValidationSummaryDto {
    pub valid: bool,
    pub issues: Vec<VegetationValidationIssueDto>,
}

/// Exact licensing and attribution retained from an imported source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSourceProvenanceDto {
    pub source: String,
    pub source_uri: String,
    pub license_id: String,
    pub license_uri: String,
    pub author: String,
    pub attribution: String,
    pub requires_attribution: bool,
}

/// Durable location of one imported plant-family source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSourceLocatorDto {
    Asset { asset: Uuid },
    File { uri: String },
}

/// Semantic contribution supplied by one imported plant-family source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantSourceRoleDto {
    Geometry,
    Material,
    Skeleton,
    Collision,
    Navigation,
}

/// Stable selection within one imported source snapshot.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSourceSelectorDto {
    #[default]
    Whole,
    Element {
        id: VegetationGuid,
        path: String,
    },
    Submesh {
        element: VegetationGuid,
        index: u32,
    },
}

/// Authored semantic destination of one stable imported-source selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSemanticDestinationDto {
    Part { id: VegetationGuid },
    Spine { id: VegetationGuid },
    MaterialSlot { slot: u32 },
    CollisionProxy { id: VegetationGuid },
    NavigationProxy { id: VegetationGuid },
    Phenotype { id: u32 },
}

/// Stable diagnostic category emitted by the single plant compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantCompileDiagnosticCodeDto {
    MissingSource,
    DuplicateSource,
    EmptySelection,
    InvalidGeometry,
    MissingMaterial,
    InvalidMaterial,
    InvalidSkeleton,
    MissingCoverageUv,
    InvalidLeafOrientation,
    BoundsMismatch,
    LimitExceeded,
    SourceChanged,
    OrphanedEdit,
}

/// One exact source-normalization diagnostic from the plant compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCompileDiagnosticDto {
    pub severity: VegetationValidationSeverityDto,
    pub code: PlantCompileDiagnosticCodeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<VegetationGuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_selector: Option<PlantSourceSelectorDto>,
    pub path: String,
    pub message: String,
}

/// Why one manual semantic target cannot survive plant reimport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantReimportConflictReasonDto {
    MissingSource,
    MissingElement,
}

/// One source identity update observed by the plant compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSourceHashUpdateDto {
    pub source: VegetationGuid,
    pub previous: String,
    pub current: String,
}

/// Exact deterministic counts produced by plant source normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCompileStatisticsDto {
    pub sources: String,
    pub meshes: String,
    pub vertices: String,
    pub indices: String,
    pub joints: String,
    pub materials: String,
    pub rejected: String,
}

/// Source coordinate units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceUnitsDto {
    #[default]
    Meters,
    Centimeters,
    Millimeters,
    Feet,
}

/// A signed coordinate axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceAxisDto {
    PositiveX,
    NegativeX,
    #[default]
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

/// Source coordinate-system handedness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceHandednessDto {
    #[default]
    Right,
    Left,
}

/// Source front-face winding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceWindingDto {
    #[default]
    CounterClockwise,
    Clockwise,
}

/// Source UV vertical origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceUvOriginDto {
    #[default]
    TopLeft,
    BottomLeft,
}

/// Tangent-frame normalization policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantTangentPolicyDto {
    Require,
    #[default]
    GenerateMissing,
    Regenerate,
}

/// Family origin policy after coordinate normalization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
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
pub enum PlantPivotDto {
    #[default]
    SourceOrigin,
    BoundsBaseCenter,
    Explicit {
        /// Position in source metres, as Q15.16 bits.
        position_bits: [i32; 3],
    },
    SemanticPart {
        /// The part identity the origin follows.
        part: VegetationGuid,
    },
}

/// How one external source is read into a plant family. Every field defaults, so a caller states
/// only what differs from metres, Y-up, right-handed, counter-clockwise geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
#[ts(export)]
pub struct PlantImportSettingsDto {
    pub units: SourceUnitsDto,
    pub up_axis: SourceAxisDto,
    pub forward_axis: SourceAxisDto,
    pub handedness: SourceHandednessDto,
    /// Uniform post-unit scale, as Q15.16 bits, where 65536 is unchanged.
    pub scale_bits: i32,
    pub pivot: PlantPivotDto,
    pub winding: SourceWindingDto,
    pub uv_origin: SourceUvOriginDto,
    /// UV scale, as Q15.16 bits.
    pub uv_scale_bits: [i32; 2],
    /// UV offset applied after scaling, as Q15.16 bits.
    pub uv_offset_bits: [i32; 2],
    pub tangent_policy: PlantTangentPolicyDto,
}

impl Default for PlantImportSettingsDto {
    fn default() -> Self {
        Self {
            units: SourceUnitsDto::default(),
            up_axis: SourceAxisDto::PositiveY,
            forward_axis: SourceAxisDto::PositiveZ,
            handedness: SourceHandednessDto::default(),
            scale_bits: 1 << 16,
            pivot: PlantPivotDto::default(),
            winding: SourceWindingDto::default(),
            uv_origin: SourceUvOriginDto::default(),
            uv_scale_bits: [1 << 16, 1 << 16],
            uv_offset_bits: [0, 0],
            tangent_policy: PlantTangentPolicyDto::default(),
        }
    }
}

/// One external hero mesh a native family may graft over a generated element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraftSourceDto {
    /// Source identity a graft edit names, as 32 hex digits.
    pub id: VegetationGuid,
    pub locator: PlantSourceLocatorDto,
    /// Which of the source's elements the graft takes.
    #[serde(default)]
    pub selector: PlantSourceSelectorDto,
    #[serde(default)]
    pub settings: PlantImportSettingsDto,
    pub provenance: VegetationSourceProvenanceDto,
}

/// One exact source snapshot read by plant validation and recooking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSourceReferenceDto {
    pub id: VegetationGuid,
    pub locator: PlantSourceLocatorDto,
    pub role: PlantSourceRoleDto,
    pub selector: PlantSourceSelectorDto,
    pub content_hash: String,
    pub provenance: VegetationSourceProvenanceDto,
}
