//! Typed biome-graph IR, module compilation, authority flow, and execution planning.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_json::{Map, Value};
use saffron_spatial::{DecisionScalar, FieldChannel, FieldDerivative, UnitInterval};

use crate::hash::sha256;
use crate::{
    BiomeAsset, BiomePaletteEntry, BiomeParameterType, BiomeRole, CompanionRule, CompetitionRule,
    Error, Result, SuccessionRule, SuitabilityBinding, validate_biome,
};

/// Current typed biome-graph document version.
pub const BIOME_GRAPH_VERSION: u32 = 1;
/// Current strict public-interface schema version.
pub const BIOME_INTERFACE_VERSION: u32 = 1;
/// Current semantic version of every initial graph operator.
pub const BIOME_NODE_VERSION: u32 = 1;

/// A value domain carried by a typed graph pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphDomain {
    /// Canonical scalar samples keyed by candidate identity.
    ScalarField,
    /// Canonical vector samples keyed by candidate identity.
    VectorField,
    /// Canonical symmetric Hessian samples keyed by candidate identity.
    HessianField,
    /// Projected surface samples and stable attachments.
    SurfaceField,
    /// Candidate stream whose identities exist before acceptance.
    Candidates,
    /// Accepted canonical macro-point columns.
    MacroPoints,
    /// Quantized authoritative micro density/attribute tiles.
    MicroField,
    /// Exact regions and bounds.
    Regions,
    /// Quantized spline paths.
    Splines,
    /// Plant-family selection table.
    SpeciesTable,
    /// Community and companion rules.
    CommunityTable,
    /// Typed evaluator diagnostics.
    Diagnostics,
}

impl GraphDomain {
    /// Stable wire/debug spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::ScalarField => "scalar-field",
            Self::VectorField => "vector-field",
            Self::HessianField => "hessian-field",
            Self::SurfaceField => "surface-field",
            Self::Candidates => "candidates",
            Self::MacroPoints => "macro-points",
            Self::MicroField => "micro-field",
            Self::Regions => "regions",
            Self::Splines => "splines",
            Self::SpeciesTable => "species-table",
            Self::CommunityTable => "community-table",
            Self::Diagnostics => "diagnostics",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "scalar-field" => Self::ScalarField,
            "vector-field" => Self::VectorField,
            "hessian-field" => Self::HessianField,
            "surface-field" => Self::SurfaceField,
            "candidates" => Self::Candidates,
            "macro-points" => Self::MacroPoints,
            "micro-field" => Self::MicroField,
            "regions" => Self::Regions,
            "splines" => Self::Splines,
            "species-table" => Self::SpeciesTable,
            "community-table" => Self::CommunityTable,
            "diagnostics" => Self::Diagnostics,
            _ => return None,
        })
    }
}

/// Whether graph output can affect authoritative world state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphAuthority {
    /// Canonical reference semantics; safe for identity and persistent state.
    #[default]
    Authoritative,
    /// GPU semantics carrying profile-specific Rust/Slang byte-equivalence evidence.
    EquivalentGpu,
    /// Spatially stable visual data that cannot feed gameplay or persistence.
    Cosmetic,
}

impl GraphAuthority {
    /// Stable wire spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::EquivalentGpu => "equivalent-gpu",
            Self::Cosmetic => "cosmetic",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "authoritative" => Self::Authoritative,
            "equivalent-gpu" => Self::EquivalentGpu,
            "cosmetic" => Self::Cosmetic,
            _ => return None,
        })
    }

    fn join(self, other: Self) -> Self {
        self.max(other)
    }

    fn can_feed_authority(self) -> bool {
        self != Self::Cosmetic
    }
}

/// Execution domains one compiled node can use without changing semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionCapabilities {
    /// Canonical single-thread reference interpreter.
    pub reference_cpu: bool,
    /// Parallel CPU scheduling over independently owned cells.
    pub parallel_cpu: bool,
    /// Slang compute implementation exists for this operation.
    pub slang_compute: bool,
}

/// Spatial evaluation policy for one node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeSpatialPolicy {
    /// Cell-partitioned evaluation with a finite immutable input halo.
    Partitioned {
        /// Hierarchical output level.
        level: u8,
        /// Maximum support radius in Q15.16 metres.
        influence_radius: DecisionScalar,
    },
    /// Ancestor/global stage that emits immutable tiled output.
    Global {
        /// Hierarchical stage level.
        level: u8,
    },
}

impl NodeSpatialPolicy {
    /// Declared hierarchy level.
    #[must_use]
    pub const fn level(self) -> u8 {
        match self {
            Self::Partitioned { level, .. } | Self::Global { level } => level,
        }
    }

    /// Required halo radius, zero for global stages.
    #[must_use]
    pub const fn influence_radius(self) -> DecisionScalar {
        match self {
            Self::Partitioned {
                influence_radius, ..
            } => influence_radius,
            Self::Global { .. } => DecisionScalar::from_bits(0),
        }
    }
}

/// Whether one operator has finite local support or propagates across its complete solve domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphSpatialRequirement {
    /// The operator is valid in a partitioned stage when its complete support is declared.
    FiniteSupport,
    /// The operator requires one bounded ancestor/global solve domain.
    Propagating,
}

/// An immutable source that invalidates a node when its content changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GraphDependencySource {
    /// Catalog asset dependency.
    Asset(Uuid),
    /// Shared surface/environment field.
    Field(FieldChannel),
    /// Stable surface provider.
    SurfaceProvider(u64),
    /// Sparse authored map layer.
    MapLayer(u128),
}

impl GraphDependencySource {
    /// Canonical tagged bytes used by dependency identities and ordering.
    #[must_use]
    pub fn canonical_bytes(self) -> Vec<u8> {
        dependency_source_bytes(self)
    }
}

impl PartialOrd for GraphDependencySource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GraphDependencySource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        dependency_source_bytes(*self).cmp(&dependency_source_bytes(*other))
    }
}

/// One typed input or output pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPin {
    /// Stable pin name.
    pub name: String,
    /// Value domain.
    pub domain: GraphDomain,
    /// Whether an input edge is mandatory.
    pub required: bool,
}

/// Typed parameter shapes accepted by operator schemas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphParameterType {
    /// Boolean.
    Boolean,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer encoded as a decimal string.
    U64,
    /// Three unsigned 32-bit lanes.
    U32Vec3,
    /// Stable 128-bit GUID encoded as lowercase hexadecimal.
    Guid,
    /// Catalog UUID encoded as a decimal string.
    Asset,
    /// Q15.16 scalar bits.
    Fixed,
    /// Closed normalized `u16` bits.
    Unit,
    /// Three Q15.16 scalar lanes.
    FixedVec3,
    /// Three exact signed global world-tick coordinates.
    WorldPosition,
    /// Shared field channel.
    FieldChannel,
    /// Scalar-field derivative order.
    FieldDerivative,
    /// Closed scalar-field combination operation.
    CombineOperation,
    /// Closed geometric distance source.
    DistanceSource,
    /// Closed cluster expansion mode.
    ClusterMode,
    /// Piecewise-linear fixed curve.
    Curve,
    /// UTF-8 identifier or mode.
    String,
    /// Stable 128-bit GUID list.
    GuidList,
    /// Stable `u64` tag list.
    TagList,
}

impl GraphParameterType {
    /// Stable wire/debug spelling used by authoring schema inspection.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U32Vec3 => "u32-vec3",
            Self::Guid => "guid",
            Self::Asset => "asset",
            Self::Fixed => "fixed",
            Self::Unit => "unit",
            Self::FixedVec3 => "fixed-vec3",
            Self::WorldPosition => "world-position",
            Self::FieldChannel => "field-channel",
            Self::FieldDerivative => "field-derivative",
            Self::CombineOperation => "combine-operation",
            Self::DistanceSource => "distance-source",
            Self::ClusterMode => "cluster-mode",
            Self::Curve => "curve",
            Self::String => "string",
            Self::GuidList => "guid-list",
            Self::TagList => "tag-list",
        }
    }
}

/// One parameter declaration for a graph operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphParameterDescriptor {
    /// Stable parameter key.
    pub name: &'static str,
    /// Typed wire/value shape.
    pub parameter_type: GraphParameterType,
    /// Whether the graph document must provide it.
    pub required: bool,
}

/// One validated typed node parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphParameterValue {
    /// Reference to one typed `.sbiome` root/module parameter, resolved before validation.
    Binding(u128),
    /// Boolean.
    Boolean(bool),
    /// Unsigned 32-bit integer.
    U32(u32),
    /// Unsigned 64-bit integer.
    U64(u64),
    /// Three unsigned 32-bit lanes.
    U32Vec3([u32; 3]),
    /// Stable 128-bit GUID.
    Guid(u128),
    /// Catalog asset UUID.
    Asset(Uuid),
    /// Q15.16 scalar.
    Fixed(DecisionScalar),
    /// Closed normalized value.
    Unit(UnitInterval),
    /// Q15.16 vector.
    FixedVec3([DecisionScalar; 3]),
    /// Three exact signed global world-tick coordinates.
    WorldPosition([i128; 3]),
    /// Shared field channel.
    FieldChannel(FieldChannel),
    /// Scalar-field derivative order.
    FieldDerivative(FieldDerivative),
    /// Closed scalar-field combination operation.
    CombineOperation(GraphCombineOperation),
    /// Closed geometric distance source.
    DistanceSource(GraphDistanceSource),
    /// Closed cluster expansion mode.
    ClusterMode(GraphClusterMode),
    /// Canonical curve `(x unit bits, y fixed bits)`.
    Curve(Vec<(UnitInterval, DecisionScalar)>),
    /// UTF-8 identifier or mode.
    String(String),
    /// Stable GUID list.
    GuidList(Vec<u128>),
    /// Stable tag list.
    TagList(Vec<u64>),
}

/// Closed scalar-field combination operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphCombineOperation {
    /// Checked addition.
    Add,
    /// Checked fixed-point multiplication.
    Multiply,
    /// Component minimum.
    Minimum,
    /// Component maximum.
    Maximum,
}

/// Closed geometric source vocabulary for distance fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDistanceSource {
    /// Water spline network.
    Water,
    /// Authored spline network.
    Spline,
    /// Region/shape bounds.
    Shape,
    /// Exclusion blocker bounds.
    Blocker,
}

/// Closed candidate-expansion mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphClusterMode {
    /// Compact local cluster.
    Cluster,
    /// Broader patch.
    Patch,
    /// Parent-rooted colony.
    Colony,
}

impl GraphCombineOperation {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Multiply => "multiply",
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "add" => Self::Add,
            "multiply" => Self::Multiply,
            "minimum" => Self::Minimum,
            "maximum" => Self::Maximum,
            _ => return None,
        })
    }
}

impl GraphDistanceSource {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::Water => "water",
            Self::Spline => "spline",
            Self::Shape => "shape",
            Self::Blocker => "blocker",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "water" => Self::Water,
            "spline" => Self::Spline,
            "shape" => Self::Shape,
            "blocker" => Self::Blocker,
            _ => return None,
        })
    }
}

impl GraphClusterMode {
    const fn as_wire(self) -> &'static str {
        match self {
            Self::Cluster => "cluster",
            Self::Patch => "patch",
            Self::Colony => "colony",
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "cluster" => Self::Cluster,
            "patch" => Self::Patch,
            "colony" => Self::Colony,
            _ => return None,
        })
    }
}

/// Complete initial biome operator vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphOperator {
    /// Public module input value.
    InterfaceInput,
    /// Root/module region input.
    RegionInput,
    /// Authored spline input.
    SplineInput,
    /// Plant-family palette input.
    SpeciesInput,
    /// Community/companion table input.
    CommunityInput,
    /// Explicit authored anchors.
    ExplicitAnchors,
    /// Stratified coverage with optional deterministic jitter.
    StratifiedCoverage,
    /// Bridson-style deterministic Poisson/blue-noise candidates.
    BlueNoisePoisson,
    /// Canonical surface projection in an arbitrary direction.
    SurfaceProjection,
    /// Shared canonical surface/environment field sampling.
    FieldSample,
    /// Sparse authored image/painted-tile sampling.
    PaintedTile,
    /// Counter-based deterministic noise.
    Noise,
    /// Exact axis/directional gradient.
    Gradient,
    /// Piecewise-linear curve evaluation.
    Curve,
    /// Fixed range remapping.
    Remap,
    /// Scalar combination.
    Combine,
    /// Fixed scalar clamp.
    Clamp,
    /// Water/spline/shape/blocker distance.
    DistanceField,
    /// Weighted candidate elimination.
    WeightedElimination,
    /// Variable prototype-aware spacing.
    VariableSpacing,
    /// Field-importance acceptance.
    FieldImportance,
    /// Cluster, patch, or colony expansion.
    ClusterPatchColony,
    /// Candidate generation along splines and edges.
    SplineFollow,
    /// Recursive child/companion placement.
    RecursiveCompanion,
    /// Canonical transform, surface orientation, yaw, scale, and variation.
    Transform,
    /// Stable priority and exclusion claims.
    PriorityExclusion,
    /// Conservative bounds-aware overlap rejection.
    BoundsOverlap,
    /// Crown/root competition with deterministic claims.
    Competition,
    /// Suitability curve filtering.
    Suitability,
    /// Community blending and shade-tolerant undergrowth selection.
    CommunityBlend,
    /// Typed succession inputs without stateful simulation.
    SuccessionInput,
    /// Canonical macro-point output.
    MacroOutput,
    /// Quantized micro density/attribute tile output.
    MicroOutput,
    /// Typed diagnostic stream output.
    DiagnosticOutput,
    /// Typed call to an ordinary `.sbiome` module.
    ModuleCall,
}

impl GraphOperator {
    /// Complete operator inventory in stable wire order.
    pub const ALL: &'static [Self] = &[
        Self::InterfaceInput,
        Self::RegionInput,
        Self::SplineInput,
        Self::SpeciesInput,
        Self::CommunityInput,
        Self::ExplicitAnchors,
        Self::StratifiedCoverage,
        Self::BlueNoisePoisson,
        Self::SurfaceProjection,
        Self::FieldSample,
        Self::PaintedTile,
        Self::Noise,
        Self::Gradient,
        Self::Curve,
        Self::Remap,
        Self::Combine,
        Self::Clamp,
        Self::DistanceField,
        Self::WeightedElimination,
        Self::VariableSpacing,
        Self::FieldImportance,
        Self::ClusterPatchColony,
        Self::SplineFollow,
        Self::RecursiveCompanion,
        Self::Transform,
        Self::PriorityExclusion,
        Self::BoundsOverlap,
        Self::Competition,
        Self::Suitability,
        Self::CommunityBlend,
        Self::SuccessionInput,
        Self::MacroOutput,
        Self::MicroOutput,
        Self::DiagnosticOutput,
        Self::ModuleCall,
    ];

    /// Stable wire spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::InterfaceInput => "interface-input",
            Self::RegionInput => "region-input",
            Self::SplineInput => "spline-input",
            Self::SpeciesInput => "species-input",
            Self::CommunityInput => "community-input",
            Self::ExplicitAnchors => "explicit-anchors",
            Self::StratifiedCoverage => "stratified-coverage",
            Self::BlueNoisePoisson => "blue-noise-poisson",
            Self::SurfaceProjection => "surface-projection",
            Self::FieldSample => "field-sample",
            Self::PaintedTile => "painted-tile",
            Self::Noise => "noise",
            Self::Gradient => "gradient",
            Self::Curve => "curve",
            Self::Remap => "remap",
            Self::Combine => "combine",
            Self::Clamp => "clamp",
            Self::DistanceField => "distance-field",
            Self::WeightedElimination => "weighted-elimination",
            Self::VariableSpacing => "variable-spacing",
            Self::FieldImportance => "field-importance",
            Self::ClusterPatchColony => "cluster-patch-colony",
            Self::SplineFollow => "spline-follow",
            Self::RecursiveCompanion => "recursive-companion",
            Self::Transform => "transform",
            Self::PriorityExclusion => "priority-exclusion",
            Self::BoundsOverlap => "bounds-overlap",
            Self::Competition => "competition",
            Self::Suitability => "suitability",
            Self::CommunityBlend => "community-blend",
            Self::SuccessionInput => "succession-input",
            Self::MacroOutput => "macro-output",
            Self::MicroOutput => "micro-output",
            Self::DiagnosticOutput => "diagnostic-output",
            Self::ModuleCall => "module-call",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "interface-input" => Self::InterfaceInput,
            "region-input" => Self::RegionInput,
            "spline-input" => Self::SplineInput,
            "species-input" => Self::SpeciesInput,
            "community-input" => Self::CommunityInput,
            "explicit-anchors" => Self::ExplicitAnchors,
            "stratified-coverage" => Self::StratifiedCoverage,
            "blue-noise-poisson" => Self::BlueNoisePoisson,
            "surface-projection" => Self::SurfaceProjection,
            "field-sample" => Self::FieldSample,
            "painted-tile" => Self::PaintedTile,
            "noise" => Self::Noise,
            "gradient" => Self::Gradient,
            "curve" => Self::Curve,
            "remap" => Self::Remap,
            "combine" => Self::Combine,
            "clamp" => Self::Clamp,
            "distance-field" => Self::DistanceField,
            "weighted-elimination" => Self::WeightedElimination,
            "variable-spacing" => Self::VariableSpacing,
            "field-importance" => Self::FieldImportance,
            "cluster-patch-colony" => Self::ClusterPatchColony,
            "spline-follow" => Self::SplineFollow,
            "recursive-companion" => Self::RecursiveCompanion,
            "transform" => Self::Transform,
            "priority-exclusion" => Self::PriorityExclusion,
            "bounds-overlap" => Self::BoundsOverlap,
            "competition" => Self::Competition,
            "suitability" => Self::Suitability,
            "community-blend" => Self::CommunityBlend,
            "succession-input" => Self::SuccessionInput,
            "macro-output" => Self::MacroOutput,
            "micro-output" => Self::MicroOutput,
            "diagnostic-output" => Self::DiagnosticOutput,
            "module-call" => Self::ModuleCall,
            _ => return None,
        })
    }

    /// Whether a portable Slang implementation exists for the operator.
    #[must_use]
    pub const fn has_slang_executor(self) -> bool {
        matches!(
            self,
            Self::Noise
                | Self::Gradient
                | Self::Curve
                | Self::Remap
                | Self::Combine
                | Self::Clamp
                | Self::FieldImportance
        )
    }

    /// Spatial execution required by this operator's canonical semantics.
    #[must_use]
    pub const fn spatial_requirement(self) -> GraphSpatialRequirement {
        match self {
            Self::BlueNoisePoisson
            | Self::WeightedElimination
            | Self::VariableSpacing
            | Self::PriorityExclusion
            | Self::BoundsOverlap => GraphSpatialRequirement::Propagating,
            _ => GraphSpatialRequirement::FiniteSupport,
        }
    }

    /// Stable semantic names for every independent stochastic decision stream.
    #[must_use]
    pub const fn seed_namespace_names(self) -> &'static [&'static str] {
        match self {
            Self::StratifiedCoverage | Self::BlueNoisePoisson => &["sampling"],
            Self::Noise => &["noise"],
            Self::ClusterPatchColony => &["cluster"],
            Self::RecursiveCompanion => &["companions"],
            Self::Transform => &["variation"],
            Self::CommunityBlend => &["community"],
            Self::SuccessionInput => &["succession"],
            Self::MacroOutput => &["species-selection"],
            Self::MicroOutput => &["reconstruction"],
            _ => &[],
        }
    }

    /// Input schema for this operator. Module-call pins are supplied by its compiled module.
    #[must_use]
    pub fn input_pins(self) -> Vec<GraphPin> {
        use GraphDomain as D;
        match self {
            Self::InterfaceInput
            | Self::RegionInput
            | Self::SplineInput
            | Self::SpeciesInput
            | Self::CommunityInput
            | Self::ExplicitAnchors => Vec::new(),
            Self::StratifiedCoverage | Self::BlueNoisePoisson => vec![pin("regions", D::Regions)],
            Self::SurfaceProjection => vec![pin("candidates", D::Candidates)],
            Self::FieldSample
            | Self::PaintedTile
            | Self::Noise
            | Self::Gradient
            | Self::DistanceField => vec![pin("candidates", D::Candidates)],
            Self::Curve | Self::Remap | Self::Clamp => vec![pin("field", D::ScalarField)],
            Self::Combine => vec![pin("left", D::ScalarField), pin("right", D::ScalarField)],
            Self::WeightedElimination | Self::FieldImportance | Self::Suitability => vec![
                pin("candidates", D::Candidates),
                pin("weights", D::ScalarField),
            ],
            Self::PriorityExclusion => vec![
                pin("candidates", D::Candidates),
                pin("weights", D::ScalarField),
                pin("radius", D::ScalarField),
            ],
            Self::VariableSpacing => vec![
                pin("candidates", D::Candidates),
                pin("radius", D::ScalarField),
            ],
            Self::Competition => vec![
                pin("candidates", D::Candidates),
                pin("communities", D::CommunityTable),
            ],
            Self::ClusterPatchColony | Self::RecursiveCompanion | Self::SuccessionInput => {
                vec![pin("candidates", D::Candidates)]
            }
            Self::SplineFollow => vec![pin("splines", D::Splines)],
            Self::Transform => vec![
                pin("candidates", D::Candidates),
                optional_pin("surface", D::SurfaceField),
                optional_pin("scale", D::ScalarField),
                optional_pin("offset", D::VectorField),
            ],
            Self::BoundsOverlap => vec![pin("candidates", D::Candidates)],
            Self::CommunityBlend => vec![
                pin("candidates", D::Candidates),
                pin("communities", D::CommunityTable),
                optional_pin("shade", D::ScalarField),
            ],
            Self::MacroOutput => vec![
                pin("candidates", D::Candidates),
                pin("species", D::SpeciesTable),
            ],
            Self::MicroOutput => vec![
                pin("candidates", D::Candidates),
                optional_pin("density", D::ScalarField),
            ],
            Self::DiagnosticOutput => vec![
                optional_pin("candidates", D::Candidates),
                optional_pin("field", D::ScalarField),
            ],
            Self::ModuleCall => Vec::new(),
        }
    }

    /// Output schema for this operator. Module-call pins are supplied by its compiled module.
    #[must_use]
    pub fn output_pins(self) -> Vec<GraphPin> {
        use GraphDomain as D;
        if self == Self::SurfaceProjection {
            return vec![
                pin("candidates", D::Candidates),
                pin("surface", D::SurfaceField),
            ];
        }
        let (name, domain) = match self {
            Self::InterfaceInput | Self::ModuleCall => return Vec::new(),
            Self::RegionInput => ("regions", D::Regions),
            Self::SplineInput => ("splines", D::Splines),
            Self::SpeciesInput => ("species", D::SpeciesTable),
            Self::CommunityInput => ("communities", D::CommunityTable),
            Self::FieldSample
            | Self::PaintedTile
            | Self::Noise
            | Self::Curve
            | Self::Remap
            | Self::Combine
            | Self::Clamp
            | Self::DistanceField => ("field", D::ScalarField),
            Self::Gradient => ("field", D::ScalarField),
            Self::MacroOutput => ("points", D::MacroPoints),
            Self::MicroOutput => ("micro", D::MicroField),
            Self::DiagnosticOutput => ("diagnostics", D::Diagnostics),
            _ => ("candidates", D::Candidates),
        };
        vec![pin(name, domain)]
    }

    fn output_requires_input(self, output: &str, input: &str) -> bool {
        match self {
            Self::InterfaceInput
            | Self::RegionInput
            | Self::SplineInput
            | Self::SpeciesInput
            | Self::CommunityInput
            | Self::ExplicitAnchors
            | Self::ModuleCall => false,
            Self::StratifiedCoverage | Self::BlueNoisePoisson => {
                output == "candidates" && input == "regions"
            }
            Self::SurfaceProjection => {
                matches!(output, "candidates" | "surface") && input == "candidates"
            }
            Self::FieldSample
            | Self::PaintedTile
            | Self::Noise
            | Self::Gradient
            | Self::DistanceField => output == "field" && input == "candidates",
            Self::Curve | Self::Remap | Self::Clamp => output == "field" && input == "field",
            Self::Combine => output == "field" && matches!(input, "left" | "right"),
            Self::WeightedElimination | Self::FieldImportance | Self::Suitability => {
                output == "candidates" && matches!(input, "candidates" | "weights")
            }
            Self::PriorityExclusion => {
                output == "candidates" && matches!(input, "candidates" | "weights" | "radius")
            }
            Self::VariableSpacing => {
                output == "candidates" && matches!(input, "candidates" | "radius")
            }
            Self::Competition => {
                output == "candidates" && matches!(input, "candidates" | "communities")
            }
            Self::ClusterPatchColony | Self::RecursiveCompanion | Self::SuccessionInput => {
                output == "candidates" && input == "candidates"
            }
            Self::SplineFollow => output == "candidates" && input == "splines",
            Self::Transform => {
                output == "candidates"
                    && matches!(input, "candidates" | "surface" | "scale" | "offset")
            }
            Self::BoundsOverlap => output == "candidates" && input == "candidates",
            Self::CommunityBlend => {
                output == "candidates" && matches!(input, "candidates" | "communities" | "shade")
            }
            Self::MacroOutput => output == "points" && matches!(input, "candidates" | "species"),
            Self::MicroOutput => output == "micro",
            Self::DiagnosticOutput => {
                output == "diagnostics" && matches!(input, "candidates" | "field")
            }
        }
    }

    /// Typed parameter schema for this operator.
    #[must_use]
    pub fn parameter_schema(self) -> Vec<GraphParameterDescriptor> {
        use GraphParameterType as P;
        match self {
            Self::InterfaceInput => vec![parameter("name", P::String, true)],
            Self::RegionInput | Self::SplineInput | Self::SpeciesInput | Self::CommunityInput => {
                Vec::new()
            }
            Self::ExplicitAnchors => vec![parameter("layer", P::Guid, true)],
            Self::StratifiedCoverage => vec![
                parameter("count", P::U32, true),
                parameter("jitter", P::Unit, false),
            ],
            Self::BlueNoisePoisson => vec![
                parameter("count", P::U32, true),
                parameter("radius", P::Fixed, true),
                parameter("attempts", P::U32, false),
            ],
            Self::SurfaceProjection => vec![
                parameter("direction", P::FixedVec3, true),
                parameter("maxDistance", P::Fixed, true),
                parameter("provider", P::U64, false),
                parameter("tags", P::TagList, false),
                parameter("materialTags", P::TagList, false),
            ],
            Self::FieldSample => vec![
                parameter("channel", P::FieldChannel, true),
                parameter("derivative", P::FieldDerivative, false),
            ],
            Self::PaintedTile => vec![
                parameter("channel", P::FieldChannel, true),
                parameter("layer", P::Guid, true),
            ],
            Self::Noise => vec![
                parameter("frequency", P::Fixed, true),
                parameter("amplitude", P::Fixed, true),
                parameter("channel", P::U32, true),
            ],
            Self::Gradient => vec![
                parameter("direction", P::FixedVec3, true),
                parameter("exactOrigin", P::WorldPosition, true),
                parameter("scale", P::Fixed, true),
                parameter("bias", P::Fixed, true),
            ],
            Self::Curve => vec![parameter("curve", P::Curve, true)],
            Self::Remap => vec![
                parameter("inputMin", P::Fixed, true),
                parameter("inputMax", P::Fixed, true),
                parameter("outputMin", P::Fixed, true),
                parameter("outputMax", P::Fixed, true),
            ],
            Self::Combine => vec![parameter("operation", P::CombineOperation, true)],
            Self::Clamp => vec![
                parameter("minimum", P::Fixed, true),
                parameter("maximum", P::Fixed, true),
            ],
            Self::DistanceField => vec![
                parameter("source", P::DistanceSource, true),
                parameter("sourceGuid", P::Guid, false),
                parameter("maximumDistance", P::Fixed, true),
            ],
            Self::WeightedElimination => vec![
                parameter("targetCount", P::U32, true),
                parameter("eliminationRadius", P::Fixed, true),
                parameter("maximumNeighbours", P::U32, true),
            ],
            Self::VariableSpacing => vec![parameter("prototypeAware", P::Boolean, false)],
            Self::FieldImportance | Self::Suitability => {
                vec![parameter("threshold", P::Unit, true)]
            }
            Self::ClusterPatchColony => vec![
                parameter("children", P::U32, true),
                parameter("radius", P::Fixed, true),
                parameter("mode", P::ClusterMode, true),
            ],
            Self::SplineFollow => vec![
                parameter("spacing", P::Fixed, true),
                parameter("edgeOffset", P::Fixed, false),
            ],
            Self::RecursiveCompanion => vec![
                parameter("children", P::U32, true),
                parameter("radius", P::Fixed, true),
                parameter("maximumDepth", P::U32, true),
            ],
            Self::Transform => vec![
                parameter("orientToSurface", P::Boolean, false),
                parameter("yawMinimum", P::Unit, false),
                parameter("yawMaximum", P::Unit, false),
                parameter("scaleMinimum", P::Fixed, false),
                parameter("scaleMaximum", P::Fixed, false),
                parameter("variationCount", P::U32, false),
            ],
            Self::PriorityExclusion => vec![parameter("keepHighest", P::Boolean, false)],
            Self::BoundsOverlap => vec![parameter("padding", P::Fixed, false)],
            Self::Competition => vec![
                parameter("crownWeight", P::Unit, true),
                parameter("rootWeight", P::Unit, true),
            ],
            Self::CommunityBlend => vec![parameter("shadeTolerance", P::Unit, false)],
            Self::SuccessionInput => Vec::new(),
            Self::MacroOutput => vec![
                parameter("representationClass", P::U32, false),
                parameter("phenotype", P::U32, false),
            ],
            Self::MicroOutput => vec![
                parameter("dimensions", P::U32Vec3, true),
                parameter("attributeChannels", P::GuidList, false),
            ],
            Self::DiagnosticOutput => vec![parameter("label", P::String, false)],
            Self::ModuleCall => vec![parameter("callGuid", P::Guid, true)],
        }
    }
}

/// One typed node in an authored graph document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNodeDefinition {
    /// Stable node GUID.
    pub guid: u128,
    /// Operator schema version.
    pub version: u32,
    /// Revision changed only when this node's semantics/configuration change.
    pub semantic_revision: u32,
    /// Typed operation.
    pub operator: GraphOperator,
    /// Declared authority class.
    pub authority: GraphAuthority,
    /// Spatial stage and finite support policy.
    pub spatial: NodeSpatialPolicy,
    /// Immutable invalidation dependencies.
    pub dependencies: Vec<GraphDependencySource>,
    /// Named/domain-separated random namespaces.
    pub seed_namespaces: BTreeMap<String, u128>,
    /// Validated typed parameters.
    pub parameters: BTreeMap<String, GraphParameterValue>,
}

impl GraphNodeDefinition {
    /// Looks up one typed parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&GraphParameterValue> {
        self.parameters.get(name)
    }

    /// Canonical current-schema bytes used by execution plans and cache identities.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        saffron_json::dump_json_sorted(&node_to_json(self), -1).into_bytes()
    }
}

/// One directed typed edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    /// Source node.
    pub from_node: u128,
    /// Source output pin.
    pub from_pin: String,
    /// Destination node.
    pub to_node: u128,
    /// Destination input pin.
    pub to_pin: String,
}

/// One public module input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphInterfaceInput {
    /// Stable interface pin identity.
    pub id: u128,
    /// Stable interface pin name.
    pub name: String,
    /// Typed domain.
    pub domain: GraphDomain,
}

/// Authority-sensitive meaning of a root output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphSink {
    /// Persistent macro points and stable IDs.
    Macro,
    /// Authoritative density/attribute tile with cosmetic reconstruction.
    Micro,
    /// Collision/nav contribution input.
    CollisionNavigation,
    /// Persistent mutation input.
    PersistentMutation,
    /// Ecology/gameplay query input.
    EcologyGameplay,
    /// Read-only diagnostic output.
    Diagnostics,
}

impl GraphSink {
    fn requires_authority(self) -> bool {
        self != Self::Diagnostics
    }

    const fn expected_domain(self) -> GraphDomain {
        match self {
            Self::Macro
            | Self::CollisionNavigation
            | Self::PersistentMutation
            | Self::EcologyGameplay => GraphDomain::MacroPoints,
            Self::Micro => GraphDomain::MicroField,
            Self::Diagnostics => GraphDomain::Diagnostics,
        }
    }

    fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "macro" => Self::Macro,
            "micro" => Self::Micro,
            "collision-navigation" => Self::CollisionNavigation,
            "persistent-mutation" => Self::PersistentMutation,
            "ecology-gameplay" => Self::EcologyGameplay,
            "diagnostics" => Self::Diagnostics,
            _ => return None,
        })
    }

    /// Stable wire spelling.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Macro => "macro",
            Self::Micro => "micro",
            Self::CollisionNavigation => "collision-navigation",
            Self::PersistentMutation => "persistent-mutation",
            Self::EcologyGameplay => "ecology-gameplay",
            Self::Diagnostics => "diagnostics",
        }
    }
}

/// One public graph output and its source pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphInterfaceOutput {
    /// Stable interface pin identity.
    pub id: u128,
    /// Stable interface output name.
    pub name: String,
    /// Typed domain.
    pub domain: GraphDomain,
    /// Source node.
    pub node: u128,
    /// Source pin.
    pub pin: String,
    /// Root authority sink; module outputs leave this `None`.
    pub sink: Option<GraphSink>,
}

/// One typed biome graph document stored in `.sbiome.graph`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BiomeGraphDocument {
    /// Document version.
    pub version: u32,
    /// Strict public-interface schema version.
    pub interface_version: u32,
    /// Public module inputs.
    pub inputs: Vec<GraphInterfaceInput>,
    /// Root/module outputs.
    pub outputs: Vec<GraphInterfaceOutput>,
    /// Nodes in arbitrary authored order.
    pub nodes: Vec<GraphNodeDefinition>,
    /// Directed typed edges in arbitrary authored order.
    pub edges: Vec<GraphEdge>,
}

impl BiomeGraphDocument {
    /// Reads and validates the typed document shape from `.sbiome.graph` JSON.
    pub fn from_json(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| graph_document("graph", "expected object"))?;
        reject_unknown(
            object,
            &[
                "version",
                "interfaceVersion",
                "inputs",
                "outputs",
                "nodes",
                "edges",
            ],
            "graph",
        )?;
        let version = read_u32(object.get("version"), "graph.version")?;
        if version != BIOME_GRAPH_VERSION {
            return Err(Error::FormatVersion {
                format: ".sbiome graph",
                found: version,
                expected: BIOME_GRAPH_VERSION,
            });
        }
        let interface_version = read_u32(object.get("interfaceVersion"), "graph.interfaceVersion")?;
        if interface_version != BIOME_INTERFACE_VERSION {
            return Err(Error::FormatVersion {
                format: ".sbiome interface",
                found: interface_version,
                expected: BIOME_INTERFACE_VERSION,
            });
        }
        let inputs = read_array(object.get("inputs"), "graph.inputs")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_interface_input(value, index))
            .collect::<Result<Vec<_>>>()?;
        let outputs = read_array(object.get("outputs"), "graph.outputs")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_interface_output(value, index))
            .collect::<Result<Vec<_>>>()?;
        let nodes = read_array(object.get("nodes"), "graph.nodes")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_node(value, index))
            .collect::<Result<Vec<_>>>()?;
        let edges = read_array(object.get("edges"), "graph.edges")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_edge(value, index))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            version,
            interface_version,
            inputs,
            outputs,
            nodes,
            edges,
        })
    }

    /// Emits the one canonical typed JSON shape used by `.sbiome` authoring.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let inputs = self
            .inputs
            .iter()
            .map(|input| {
                object([
                    ("id", Value::String(guid_text(input.id))),
                    ("name", Value::String(input.name.clone())),
                    ("domain", Value::String(input.domain.as_wire().to_owned())),
                ])
            })
            .collect();
        let outputs = self
            .outputs
            .iter()
            .map(|output| {
                let mut fields = vec![
                    ("id", Value::String(guid_text(output.id))),
                    ("name", Value::String(output.name.clone())),
                    ("domain", Value::String(output.domain.as_wire().to_owned())),
                    ("node", Value::String(guid_text(output.node))),
                    ("pin", Value::String(output.pin.clone())),
                ];
                if let Some(sink) = output.sink {
                    fields.push(("sink", Value::String(sink.as_wire().to_owned())));
                }
                object(fields)
            })
            .collect();
        let nodes = self.nodes.iter().map(node_to_json).collect();
        let edges = self
            .edges
            .iter()
            .map(|edge| {
                object([
                    ("fromNode", Value::String(guid_text(edge.from_node))),
                    ("fromPin", Value::String(edge.from_pin.clone())),
                    ("toNode", Value::String(guid_text(edge.to_node))),
                    ("toPin", Value::String(edge.to_pin.clone())),
                ])
            })
            .collect();
        object([
            ("version", Value::from(self.version)),
            ("interfaceVersion", Value::from(self.interface_version)),
            ("inputs", Value::Array(inputs)),
            ("outputs", Value::Array(outputs)),
            ("nodes", Value::Array(nodes)),
            ("edges", Value::Array(edges)),
        ])
    }

    /// Canonical content identity independent of authored object key order.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let text = saffron_json::dump_json_sorted(&self.to_json(), -1);
        sha256(text.as_bytes())
    }
}

/// One immutable dependency and the exact content identity used for compilation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphDependencyFingerprint {
    /// Source identity.
    pub source: GraphDependencySource,
    /// Exact canonical content hash.
    pub content_hash: [u8; 32],
}

/// Resolves biome modules and immutable source hashes without coupling the domain crate to asset I/O.
pub trait BiomeGraphResolver {
    /// Loads one module asset by stable catalog UUID.
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset>;
    /// Resolves the canonical content hash for one declared source.
    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]>;
    /// Lists canonical non-asset sources available to operators that address a complete source set.
    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        Vec::new()
    }
}

/// Device/profile identity used for `EquivalentGpu` qualification.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GpuExecutionProfile {
    /// Stable profile name used in diagnostics and persisted evidence.
    pub name: String,
    /// Vulkan vendor ID.
    pub vendor_id: u32,
    /// Vulkan device ID.
    pub device_id: u32,
    /// Driver version.
    pub driver_version: u32,
    /// Vulkan API version exposed by the physical device.
    pub api_version: u32,
    /// Vulkan driver implementation identity.
    pub driver_id: u32,
    /// Stable Vulkan physical-device UUID.
    pub device_uuid: [u8; 16],
    /// Stable Vulkan driver UUID.
    pub driver_uuid: [u8; 16],
    /// Whether the profile runs through MoltenVK.
    pub molten_vk: bool,
}

/// Exact verified shader artifact bound into GPU qualification evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GpuShaderArtifactIdentity {
    /// Canonical manifest-record identity, including flags, defines, and source closure.
    pub record_hash: [u8; 32],
    /// Exact compile-input identity.
    pub compile_input_hash: [u8; 32],
    /// Exact loaded SPIR-V identity.
    pub spirv_hash: [u8; 32],
    /// Exact compiler identity.
    pub compiler_identity_hash: [u8; 32],
}

/// Matching Rust/Slang bytes for one operator/profile/corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuEquivalenceEvidence {
    /// Qualified profile.
    profile: GpuExecutionProfile,
    /// Exact shader artifact executed by the qualification corpus.
    artifact: GpuShaderArtifactIdentity,
    /// Operator.
    operator: GraphOperator,
    /// Operator semantic version.
    version: u32,
    /// Canonical qualification corpus identity.
    corpus_hash: [u8; 32],
    /// Reference output identity.
    rust_hash: [u8; 32],
    /// Slang output identity.
    slang_hash: [u8; 32],
}

impl GpuEquivalenceEvidence {
    /// Exact device/driver profile that executed this evidence.
    #[must_use]
    pub fn profile(&self) -> &GpuExecutionProfile {
        &self.profile
    }

    /// Exact verified shader artifact that executed this evidence.
    #[must_use]
    pub const fn artifact(&self) -> GpuShaderArtifactIdentity {
        self.artifact
    }

    /// Qualified operator and semantic version.
    #[must_use]
    pub const fn operator_version(&self) -> (GraphOperator, u32) {
        (self.operator, self.version)
    }

    /// Corpus, Rust reference, and Slang output identities.
    #[must_use]
    pub const fn result_hashes(&self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        (self.corpus_hash, self.rust_hash, self.slang_hash)
    }
}

/// Qualification evidence admitted by execution-plan selection.
#[derive(Clone, Debug, Default)]
pub struct GpuQualificationRegistry {
    evidence: Vec<GpuEquivalenceEvidence>,
}

impl GpuQualificationRegistry {
    /// Executes and verifies the canonical corpus before minting qualification evidence.
    pub fn qualify(
        profile: GpuExecutionProfile,
        artifact: GpuShaderArtifactIdentity,
        mut execute: impl FnMut(
            &crate::GraphGpuProgram,
            &crate::GraphGpuInvocationBatch,
        ) -> Result<Vec<crate::GraphGpuOutput>>,
    ) -> Result<Self> {
        if profile.name.is_empty()
            || profile.device_uuid == [0; 16]
            || profile.driver_uuid == [0; 16]
            || artifact.record_hash == [0; 32]
            || artifact.compile_input_hash == [0; 32]
            || artifact.spirv_hash == [0; 32]
            || artifact.compiler_identity_hash == [0; 32]
        {
            return Err(graph_document(
                "gpuQualification.identity",
                "device, driver, compiler, and shader artifact identities must be complete",
            ));
        }
        let corpus = crate::graph_gpu::qualification_corpus();
        let mut actual_bytes = Vec::new();
        for (batch_index, batch) in corpus.iter().enumerate() {
            let actual = execute(&batch.program, &batch.invocation_batch)?;
            if actual.len() != batch.invocation_batch.invocation_count() {
                return Err(graph_document(
                    "gpuQualification.outputs",
                    "qualification executor returned the wrong result count",
                ));
            }
            let expected = crate::graph_gpu::evaluate_gpu_program_reference(
                &batch.program,
                &batch.invocation_batch,
            )?;
            if let Some(invocation_index) = actual
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected)
            {
                let reason = format!(
                    "qualification mismatch at batch {batch_index}, invocation {invocation_index}"
                );
                return Err(graph_document("gpuQualification.outputs", &reason));
            }
            for output in actual {
                for word in output.words() {
                    actual_bytes.extend_from_slice(&word.to_be_bytes());
                }
            }
        }
        let corpus_hash = crate::graph_gpu::qualification_corpus_hash();
        let reference_hash = crate::graph_gpu::qualification_reference_hash();
        let slang_hash = sha256(&actual_bytes);
        if slang_hash != reference_hash {
            return Err(graph_document(
                "gpuQualification",
                "Rust and Slang result hashes must match the canonical reference",
            ));
        }
        let evidence = GraphOperator::ALL
            .iter()
            .copied()
            .filter(|operator| operator.has_slang_executor())
            .map(|operator| GpuEquivalenceEvidence {
                profile: profile.clone(),
                artifact,
                operator,
                version: BIOME_NODE_VERSION,
                corpus_hash,
                rust_hash: reference_hash,
                slang_hash,
            })
            .collect();
        Ok(Self { evidence })
    }

    /// Complete immutable evidence set in canonical operator order.
    #[must_use]
    pub fn evidence(&self) -> &[GpuEquivalenceEvidence] {
        &self.evidence
    }

    /// Whether one operator/version is qualified for this exact profile.
    #[must_use]
    pub fn contains(
        &self,
        operator: GraphOperator,
        version: u32,
        profile: &GpuExecutionProfile,
    ) -> bool {
        self.evidence.iter().any(|evidence| {
            evidence.operator == operator
                && evidence.version == version
                && &evidence.profile == profile
                && evidence.rust_hash == evidence.slang_hash
        })
    }
}

/// Hard planning and evaluation limits. Exceeding one aborts; quality is never reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphSafetyLimits {
    /// Maximum parallel cell workers admitted by one evaluator.
    pub max_workers: u16,
    /// Maximum output cells admitted by one bounded multi-cell evaluation.
    pub max_output_cells: u64,
    /// Maximum unique ancestor/global stage tiles admitted by one bounded evaluation.
    pub max_global_stage_tiles: u64,
    /// Maximum caller-supplied plus preparation-generated input tiles.
    pub max_input_tiles: u64,
    /// Maximum candidates admitted by a bounded evaluation.
    pub max_candidates: u64,
    /// Maximum accepted macro points.
    pub max_macro_points: u64,
    /// Maximum quantized micro samples.
    pub max_micro_samples: u64,
    /// Maximum evaluator-owned requested heap capacities and explicit worker stacks.
    ///
    /// The bound includes conservative allocator and ordered-map metadata. Caller-owned graph and
    /// provider pointees, compute-executor internals, and operating-system or standard-library
    /// thread bookkeeping are outside the evaluator's ownership boundary.
    pub max_memory_bytes: u64,
    /// Maximum estimated CPU/GPU transfer bytes.
    pub max_transfer_bytes: u64,
    /// Maximum nested module-call edges from the root, whose depth is zero.
    pub max_module_depth: u16,
    /// Maximum wall-clock evaluation time in milliseconds.
    pub max_time_ms: u64,
}

impl Default for GraphSafetyLimits {
    fn default() -> Self {
        Self {
            max_workers: 256,
            max_output_cells: 1_000_000,
            max_global_stage_tiles: 1_000_000,
            max_input_tiles: 1_000_000,
            max_candidates: 16_000_000,
            max_macro_points: 4_000_000,
            max_micro_samples: 256_000_000,
            max_memory_bytes: 4 * 1024 * 1024 * 1024,
            max_transfer_bytes: 1024 * 1024 * 1024,
            max_module_depth: 32,
            max_time_ms: 60_000,
        }
    }
}

/// Predicted work and memory for one node or complete graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphEstimate {
    /// Maximum candidates emitted by the stage.
    pub candidates: u64,
    /// Maximum accepted points emitted by the stage.
    pub accepted: u64,
    /// Maximum quantized micro samples emitted by the stage.
    pub micro_samples: u64,
    /// Maximum live bytes.
    pub memory_bytes: u64,
    /// Maximum transfer bytes at execution-domain boundaries.
    pub transfer_bytes: u64,
}

/// Stable path used by provenance and rejection diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphDebugSymbol {
    /// Module call path from the root.
    pub module_path: Vec<u128>,
    /// Node GUID local to the owning asset.
    pub node: u128,
    /// Human-readable stable label.
    pub label: String,
}

/// Stable fully qualified address of one compiled node, including nested module calls.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphNodeAddress {
    /// Module-call path from the root graph.
    pub module_path: Vec<u128>,
    /// Node GUID local to the owning graph.
    pub node: u128,
}

impl GraphNodeAddress {
    /// Canonical bytes used by stage, cache, and execution identities.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + self.module_path.len() * 16 + 16);
        bytes.extend_from_slice(&(self.module_path.len() as u64).to_be_bytes());
        for call in &self.module_path {
            bytes.extend_from_slice(&call.to_be_bytes());
        }
        bytes.extend_from_slice(&self.node.to_be_bytes());
        bytes
    }

    /// Root-biome-qualified compact execution identity used by candidate random streams.
    #[must_use]
    pub fn execution_identity(&self, biome: Uuid) -> u128 {
        let mut bytes = b"saffron-anima/vegetation-node-address/v1\0".to_vec();
        bytes.extend_from_slice(&biome.value().to_be_bytes());
        for call in &self.module_path {
            bytes.extend_from_slice(&call.to_be_bytes());
        }
        bytes.extend_from_slice(&self.node.to_be_bytes());
        u128::from_be_bytes(sha256(&bytes)[..16].try_into().unwrap())
    }
}

/// One pin on a fully qualified compiled node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedGraphPin {
    /// Fully qualified node owning the pin.
    pub node: GraphNodeAddress,
    /// Stable pin name.
    pub pin: String,
}

impl QualifiedGraphPin {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = self.node.canonical_bytes();
        bytes.extend_from_slice(&(self.pin.len() as u64).to_be_bytes());
        bytes.extend_from_slice(self.pin.as_bytes());
        bytes
    }
}

/// One immutable ancestor/global execution stage compiled from the canonical graph IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledGlobalStage {
    /// Stable identity derived from the complete graph and stage topology.
    pub id: [u8; 32],
    /// Canonical ancestor-cell level forming the closed solve domain.
    pub owner_level: u8,
    /// Maximal connected same-level global node slice in canonical order.
    pub nodes: Vec<GraphNodeAddress>,
    /// Complete upstream execution closure, stopping at earlier global stages.
    pub closure: Vec<GraphNodeAddress>,
    /// Immutable outputs from earlier stages required by this closure.
    pub input_pins: Vec<QualifiedGraphPin>,
    /// Global outputs materialized for downstream stages, cells, or graph outputs.
    pub output_pins: Vec<QualifiedGraphPin>,
    /// Exact immutable dependency fingerprints visible to the stage closure.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Finest hierarchy level required by any node in the closure.
    pub minimum_input_level: u8,
    /// Exact composed finite halo required around the closed owner-cell solve bounds.
    pub upstream_halo: DecisionScalar,
    /// Conservative aggregate work and retained-memory estimate.
    pub estimate: GraphEstimate,
}

/// Compiler-owned spatial schedule over the single canonical graph IR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledSpatialPlan {
    global_stages: Vec<CompiledGlobalStage>,
    global_stage_by_node: BTreeMap<GraphNodeAddress, [u8; 32]>,
}

impl CompiledSpatialPlan {
    /// Global stages in canonical dependency/topological order.
    #[must_use]
    pub fn global_stages(&self) -> &[CompiledGlobalStage] {
        &self.global_stages
    }

    /// Finds one global stage by its stable identity.
    #[must_use]
    pub fn global_stage(&self, id: [u8; 32]) -> Option<&CompiledGlobalStage> {
        self.global_stages.iter().find(|stage| stage.id == id)
    }

    /// Finds the global stage containing one fully qualified compiled node.
    #[must_use]
    pub fn global_stage_for_node(
        &self,
        address: &GraphNodeAddress,
    ) -> Option<&CompiledGlobalStage> {
        let id = self.global_stage_by_node.get(address)?;
        self.global_stage(*id)
    }

    /// Conservative tile count before canonical ancestor-owner deduplication.
    #[must_use]
    pub fn maximum_global_tiles_for_output_cells(&self, output_cells: u64) -> Option<u64> {
        (self.global_stages.len() as u64).checked_mul(output_cells)
    }
}

/// Symbolic candidate-stream lineage carried through the compiled typed IR.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphValueLineage {
    /// One public module input whose caller supplies the concrete lineage.
    InterfaceInput(String),
    /// One candidate-generation or expansion stage in the fully qualified module path.
    CandidateOrigin {
        /// Module call path from the root.
        module_path: Vec<u128>,
        /// Node GUID local to the owning graph.
        node: u128,
    },
}

/// One validated node ready for execution.
#[derive(Clone, Debug)]
pub struct CompiledGraphNode {
    /// Authored node definition after strict current-version validation.
    pub definition: GraphNodeDefinition,
    /// Canonical definition hash reused by execution plans and cache identities.
    pub definition_hash: [u8; 32],
    /// Complete typed inputs.
    pub inputs: Vec<GraphPin>,
    /// Complete typed outputs.
    pub outputs: Vec<GraphPin>,
    /// Declared parameter schema.
    pub parameter_schema: Vec<GraphParameterDescriptor>,
    /// Execution capabilities.
    pub capabilities: ExecutionCapabilities,
    /// Propagated output authority per pin.
    pub output_authority: BTreeMap<String, GraphAuthority>,
    /// Candidate-stream lineage carried by candidate-indexed output pins.
    pub output_lineage: BTreeMap<String, GraphValueLineage>,
    /// Conservative estimate for each output pin.
    pub output_estimates: BTreeMap<String, GraphEstimate>,
    /// Planning estimate after upstream propagation.
    pub estimate: GraphEstimate,
    /// Exact immutable dependency fingerprints read by this node.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Stable diagnostic symbol.
    pub debug_symbol: GraphDebugSymbol,
    /// Recursively compiled ordinary `.sbiome` module.
    pub module: Option<Box<CompiledGraphUnit>>,
}

impl CompiledGraphNode {
    /// Stable fully qualified address of this node.
    #[must_use]
    pub fn address(&self) -> GraphNodeAddress {
        GraphNodeAddress {
            module_path: self.debug_symbol.module_path.clone(),
            node: self.definition.guid,
        }
    }
}

/// One validated root or module graph in canonical topological order.
#[derive(Clone, Debug)]
pub struct CompiledGraphUnit {
    /// Owning biome asset.
    pub biome: Uuid,
    /// Root/module role.
    pub role: BiomeRole,
    /// Typed public inputs.
    pub inputs: Vec<GraphInterfaceInput>,
    /// Typed public outputs.
    pub outputs: Vec<GraphInterfaceOutput>,
    /// Propagated authority for each public output.
    pub output_authority: BTreeMap<String, GraphAuthority>,
    /// Conservative estimate for each public output.
    pub output_estimates: BTreeMap<String, GraphEstimate>,
    /// Candidate-stream lineage carried by candidate-indexed public outputs.
    pub output_lineage: BTreeMap<String, GraphValueLineage>,
    /// Nodes in canonical topological order.
    pub nodes: Vec<CompiledGraphNode>,
    /// Canonically sorted edges.
    pub edges: Vec<GraphEdge>,
    /// Graph document hash.
    pub document_hash: [u8; 32],
    /// Direct and transitive asset dependencies with canonical hashes.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Plant palette available to species-input nodes.
    pub palette: Vec<BiomePaletteEntry>,
    /// Field suitability rules available to field/suitability nodes.
    pub suitability: Vec<SuitabilityBinding>,
    /// Pairwise spacing/priority rules.
    pub competition: Vec<CompetitionRule>,
    /// Recursive child/companion rules.
    pub companions: Vec<CompanionRule>,
    /// Read-only succession inputs.
    pub succession: Vec<SuccessionRule>,
    /// Whether missing canonical fields are a compile/evaluation error.
    pub require_authoritative_fields: bool,
    /// Maximum composed finite influence for demanded outputs.
    pub maximum_influence_radius: DecisionScalar,
    /// Aggregate planning estimate.
    pub estimate: GraphEstimate,
    output_halo_by_level: BTreeMap<String, [DecisionScalar; 63]>,
}

/// The single compiled IR used by preview, offline cooking, and runtime evaluation.
#[derive(Clone, Debug)]
pub struct CompiledBiomeGraph {
    /// Root graph.
    pub root: CompiledGraphUnit,
    /// Exact root asset.
    pub biome: Uuid,
    /// Canonical IR identity.
    pub identity: [u8; 32],
    /// Safety limits used during validation.
    pub limits: GraphSafetyLimits,
    demand_plan: CompiledDemandPlan,
    spatial_plan: CompiledSpatialPlan,
    required_halo_by_level: [DecisionScalar; 63],
}

impl CompiledBiomeGraph {
    /// Maximum finite halo radius required by partitioned stages visible at `output_level`.
    #[must_use]
    pub fn required_halo(&self, output_level: u8) -> DecisionScalar {
        self.required_halo_by_level
            .get(usize::from(output_level))
            .copied()
            .unwrap_or(DecisionScalar::from_bits(0))
    }

    /// Canonically sorted direct and transitive immutable dependency fingerprints.
    #[must_use]
    pub fn dependencies(&self) -> &[GraphDependencyFingerprint] {
        &self.root.dependencies
    }

    /// Compiler-owned spatial schedule over the canonical graph IR.
    #[must_use]
    pub fn spatial_plan(&self) -> &CompiledSpatialPlan {
        &self.spatial_plan
    }

    /// Compiler-owned pin-level demand shared by planners and evaluators.
    #[must_use]
    pub(crate) const fn demand_plan(&self) -> &CompiledDemandPlan {
        &self.demand_plan
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandUnitSlice {
    pub(crate) nodes: Vec<u128>,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) inputs: BTreeSet<String>,
    pub(crate) outputs: BTreeSet<String>,
}

impl CompiledDemandUnitSlice {
    pub(crate) fn contains_node(&self, node: u128) -> bool {
        self.nodes.contains(&node)
    }

    pub(crate) fn contains_edge(&self, edge: &GraphEdge) -> bool {
        self.edges.iter().any(|candidate| candidate == edge)
    }
}

fn compile_demand_slice(
    root: &CompiledGraphUnit,
    seeds: impl IntoIterator<Item = QualifiedGraphPin>,
    stop_pins: &BTreeSet<QualifiedGraphPin>,
) -> Result<CompiledDemandSlice> {
    let mut slice = CompiledDemandSlice::default();
    let mut pending = seeds.into_iter().collect::<Vec<_>>();
    while let Some(output_pin) = pending.pop() {
        if !slice.output_pins.insert(output_pin.clone()) {
            continue;
        }
        let module_path = output_pin.node.module_path.as_slice();
        let unit = compiled_unit_at_path(root, module_path)?;
        let node = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == output_pin.node.node)
            .ok_or_else(|| {
                graph_document(
                    "graph.demand.output",
                    "demanded output node is missing from its compiled unit",
                )
            })?;
        slice.nodes.insert(node.address());
        {
            let unit_slice = slice.units.entry(module_path.to_vec()).or_default();
            if !unit_slice.nodes.contains(&node.definition.guid) {
                unit_slice.nodes.push(node.definition.guid);
            }
        }
        let exported_outputs = unit
            .outputs
            .iter()
            .filter(|output| output.node == output_pin.node.node && output.pin == output_pin.pin)
            .filter(|output| {
                module_path.is_empty()
                    || slice
                        .units
                        .get(module_path)
                        .is_some_and(|unit| unit.outputs.contains(&output.name))
            })
            .map(|output| output.name.clone())
            .collect::<Vec<_>>();
        for name in exported_outputs {
            slice
                .units
                .entry(module_path.to_vec())
                .or_default()
                .outputs
                .insert(name.clone());
            if !module_path.is_empty() {
                let parent_path = &module_path[..module_path.len() - 1];
                let parent = compiled_unit_at_path(root, parent_path)?;
                let call = module_call_by_guid(parent, module_path[module_path.len() - 1])?;
                pending.push(QualifiedGraphPin {
                    node: call.address(),
                    pin: name,
                });
            }
        }
        if stop_pins.contains(&output_pin) {
            continue;
        }
        slice.executed_nodes.insert(node.address());

        match node.definition.operator {
            GraphOperator::InterfaceInput => {
                let Some(GraphParameterValue::String(name)) = node.definition.parameter("name")
                else {
                    return Err(graph_document(
                        "graph.demand.interfaceInput",
                        "interface input name is missing",
                    ));
                };
                slice
                    .units
                    .entry(module_path.to_vec())
                    .or_default()
                    .inputs
                    .insert(name.clone());
                if module_path.is_empty() {
                    return Err(graph_document(
                        "graph.demand.interfaceInput",
                        "root graph cannot demand an interface input",
                    ));
                }
                let parent_path = &module_path[..module_path.len() - 1];
                let call_guid = module_path[module_path.len() - 1];
                let parent = compiled_unit_at_path(root, parent_path)?;
                let call = module_call_by_guid(parent, call_guid)?;
                let edge = parent
                    .edges
                    .iter()
                    .find(|edge| {
                        edge.to_node == call.definition.guid && edge.to_pin.as_str() == name
                    })
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.interfaceInput",
                            "demanded module input edge is missing",
                        )
                    })?;
                let call_input = QualifiedGraphPin {
                    node: call.address(),
                    pin: edge.to_pin.clone(),
                };
                slice.input_pins.insert(call_input);
                let parent_slice = slice.units.entry(parent_path.to_vec()).or_default();
                if !parent_slice.edges.contains(edge) {
                    parent_slice.edges.push(edge.clone());
                }
                let source = parent
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == edge.from_node)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.interfaceInput",
                            "module input source node is missing",
                        )
                    })?;
                pending.push(QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                });
            }
            GraphOperator::ModuleCall => {
                let module = node.module.as_deref().ok_or_else(|| {
                    graph_document("graph.demand.moduleCall", "compiled module is missing")
                })?;
                let public_output = module
                    .outputs
                    .iter()
                    .find(|output| output.name == output_pin.pin)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.moduleCall",
                            "demanded module output is missing",
                        )
                    })?;
                let call_guid = module_call_guid(node)?;
                let mut child_path = module_path.to_vec();
                child_path.push(call_guid);
                slice
                    .units
                    .entry(child_path.clone())
                    .or_default()
                    .outputs
                    .insert(public_output.name.clone());
                let source = module
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == public_output.node)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.moduleCall",
                            "module output source node is missing",
                        )
                    })?;
                pending.push(QualifiedGraphPin {
                    node: source.address(),
                    pin: public_output.pin.clone(),
                });
            }
            _ => {
                for edge in unit
                    .edges
                    .iter()
                    .filter(|edge| edge.to_node == node.definition.guid)
                    .filter(|edge| {
                        node.definition
                            .operator
                            .output_requires_input(&output_pin.pin, &edge.to_pin)
                    })
                {
                    slice.input_pins.insert(QualifiedGraphPin {
                        node: node.address(),
                        pin: edge.to_pin.clone(),
                    });
                    let unit_slice = slice.units.entry(module_path.to_vec()).or_default();
                    if !unit_slice.edges.contains(edge) {
                        unit_slice.edges.push(edge.clone());
                    }
                    let source = unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == edge.from_node)
                        .ok_or_else(|| {
                            graph_document(
                                "graph.demand.edge",
                                "demanded edge source node is missing",
                            )
                        })?;
                    pending.push(QualifiedGraphPin {
                        node: source.address(),
                        pin: edge.from_pin.clone(),
                    });
                }
            }
        }
    }
    let nested_paths = slice
        .nodes
        .iter()
        .map(|address| address.module_path.clone())
        .collect::<BTreeSet<_>>();
    for nested_path in nested_paths {
        for depth in 0..nested_path.len() {
            let parent_path = &nested_path[..depth];
            let parent = compiled_unit_at_path(root, parent_path)?;
            let call = module_call_by_guid(parent, nested_path[depth])?;
            slice.nodes.insert(call.address());
            slice.executed_nodes.insert(call.address());
            let parent_slice = slice.units.entry(parent_path.to_vec()).or_default();
            if !parent_slice.nodes.contains(&call.definition.guid) {
                parent_slice.nodes.push(call.definition.guid);
            }
        }
    }
    canonicalize_demand_slice(root, &[], &mut slice)?;
    let mut estimates = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    for pin in slice.output_pins.iter().cloned().collect::<Vec<_>>() {
        estimate_demand_pin(root, &slice, &pin, &mut estimates, &mut visiting)?;
    }
    slice.estimates = estimates;
    slice.dependencies = demanded_dependencies(root, &slice)?;
    Ok(slice)
}

fn demanded_dependencies(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<Vec<GraphDependencyFingerprint>> {
    let mut dependencies = BTreeSet::new();
    for address in &demand.nodes {
        let unit = compiled_unit_at_path(root, &address.module_path)?;
        let node = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == address.node)
            .ok_or_else(|| graph_document("graph.demand.dependencies", "live node is missing"))?;
        if node.definition.operator == GraphOperator::ModuleCall {
            dependencies.extend(
                node.dependencies
                    .iter()
                    .copied()
                    .filter(|dependency| node.definition.dependencies.contains(&dependency.source)),
            );
        } else {
            dependencies.extend(node.dependencies.iter().copied());
        }
    }
    Ok(dependencies.into_iter().collect())
}

fn demand_unit_semantic_hash(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    module_path: &[u128],
) -> Result<[u8; 32]> {
    let unit = compiled_unit_at_path(root, module_path)?;
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| graph_document("graph.demand.identity", "live unit slice is missing"))?;
    let mut bytes = b"saffron-anima/vegetation-live-unit/v1\0".to_vec();
    bytes.extend_from_slice(&(module_path.len() as u64).to_be_bytes());
    for call in module_path {
        bytes.extend_from_slice(&call.to_be_bytes());
    }
    let mut inputs = unit
        .inputs
        .iter()
        .filter(|input| unit_demand.inputs.contains(&input.name))
        .collect::<Vec<_>>();
    inputs.sort_by(|left, right| (left.id, &left.name).cmp(&(right.id, &right.name)));
    append_identity_collection(&mut bytes, "inputs", inputs.len());
    for input in inputs {
        bytes.extend_from_slice(&input.id.to_be_bytes());
        append_identity_text(&mut bytes, &input.name);
        append_identity_text(&mut bytes, input.domain.as_wire());
    }
    let mut outputs = unit
        .outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .collect::<Vec<_>>();
    outputs.sort_by(|left, right| (left.id, &left.name).cmp(&(right.id, &right.name)));
    append_identity_collection(&mut bytes, "outputs", outputs.len());
    for output in outputs {
        bytes.extend_from_slice(&output.id.to_be_bytes());
        append_identity_text(&mut bytes, &output.name);
        append_identity_text(&mut bytes, output.domain.as_wire());
        bytes.extend_from_slice(&output.node.to_be_bytes());
        append_identity_text(&mut bytes, &output.pin);
    }
    let nodes = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .collect::<Vec<_>>();
    append_identity_collection(&mut bytes, "nodes", nodes.len());
    for node in nodes {
        bytes.extend_from_slice(&live_node_semantic_hash(unit, node, demand, true)?);
        if node.definition.operator == GraphOperator::ModuleCall {
            let mut child_path = module_path.to_vec();
            child_path.push(module_call_guid(node)?);
            bytes.extend_from_slice(&demand_unit_semantic_hash(root, demand, &child_path)?);
        }
    }
    append_identity_collection(&mut bytes, "edges", unit_demand.edges.len());
    for edge in &unit_demand.edges {
        bytes.extend_from_slice(&edge.from_node.to_be_bytes());
        append_identity_text(&mut bytes, &edge.from_pin);
        bytes.extend_from_slice(&edge.to_node.to_be_bytes());
        append_identity_text(&mut bytes, &edge.to_pin);
    }
    Ok(sha256(&bytes))
}

fn live_node_semantic_hash(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    demand: &CompiledDemandSlice,
    include_ports: bool,
) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-live-node/v1\0".to_vec();
    let mut canonical_definition = node.definition.clone();
    canonical_definition.dependencies.sort();
    canonical_definition.dependencies.dedup();
    let definition = canonical_definition.canonical_bytes();
    bytes.extend_from_slice(&(definition.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&definition);
    if include_ports {
        let input_pins = demand
            .input_pins
            .iter()
            .filter(|pin| pin.node == node.address())
            .collect::<Vec<_>>();
        append_identity_collection(&mut bytes, "inputs", input_pins.len());
        for pin in input_pins {
            append_identity_text(&mut bytes, &pin.pin);
        }
        let output_pins = demand
            .output_pins
            .iter()
            .filter(|pin| pin.node == node.address())
            .collect::<Vec<_>>();
        append_identity_collection(&mut bytes, "outputs", output_pins.len());
        for pin in output_pins {
            append_identity_text(&mut bytes, &pin.pin);
        }
    } else {
        append_identity_collection(&mut bytes, "inputs", 0);
        append_identity_collection(&mut bytes, "outputs", 0);
    }
    let reads_palette = matches!(
        node.definition.operator,
        GraphOperator::SpeciesInput | GraphOperator::CommunityBlend
    );
    append_identity_collection(
        &mut bytes,
        "palette",
        if reads_palette { unit.palette.len() } else { 0 },
    );
    if reads_palette {
        let mut palette = unit.palette.clone();
        palette.sort_by_key(|entry| (entry.plant.value(), entry.weight, entry.seed_namespace));
        for entry in &palette {
            bytes.extend_from_slice(&entry.plant.value().to_be_bytes());
            bytes.extend_from_slice(&entry.weight.bits().to_be_bytes());
            bytes.extend_from_slice(&entry.seed_namespace.to_be_bytes());
        }
    }
    let suitability_count = if node.definition.operator == GraphOperator::Suitability {
        unit.suitability
            .iter()
            .filter(|binding| binding.node_guid == node.definition.guid)
            .count()
    } else {
        0
    };
    append_identity_collection(&mut bytes, "suitability", suitability_count);
    if node.definition.operator == GraphOperator::Suitability {
        let mut suitability = unit
            .suitability
            .iter()
            .filter(|binding| binding.node_guid == node.definition.guid)
            .copied()
            .collect::<Vec<_>>();
        suitability.sort_by_key(|binding| {
            (
                binding.node_guid,
                binding.channel,
                binding.minimum,
                binding.maximum,
                binding.falloff,
            )
        });
        for binding in suitability {
            append_identity_text(&mut bytes, &field_channel_wire(binding.channel));
            bytes.extend_from_slice(&binding.minimum.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.maximum.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.falloff.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.node_guid.to_be_bytes());
        }
    }
    if matches!(node.definition.operator, GraphOperator::CommunityInput) {
        append_identity_collection(&mut bytes, "competition", unit.competition.len());
        let mut rules = unit.competition.clone();
        rules.sort_by_key(|rule| {
            (
                rule.first.value(),
                rule.second.value(),
                rule.spacing,
                rule.priority,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.first.value().to_be_bytes());
            bytes.extend_from_slice(&rule.second.value().to_be_bytes());
            bytes.extend_from_slice(&rule.spacing.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.priority.to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "competition", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::RecursiveCompanion | GraphOperator::CommunityInput
    ) {
        append_identity_collection(&mut bytes, "companions", unit.companions.len());
        let mut rules = unit.companions.clone();
        rules.sort_by_key(|rule| {
            (
                rule.parent.value(),
                rule.child.value(),
                rule.minimum_distance,
                rule.maximum_distance,
                rule.probability,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.parent.value().to_be_bytes());
            bytes.extend_from_slice(&rule.child.value().to_be_bytes());
            bytes.extend_from_slice(&rule.minimum_distance.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.maximum_distance.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.probability.bits().to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "companions", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::SuccessionInput | GraphOperator::CommunityInput
    ) {
        append_identity_collection(&mut bytes, "succession", unit.succession.len());
        let mut rules = unit.succession.clone();
        rules.sort_by_key(|rule| {
            (
                rule.from.value(),
                rule.to.value(),
                rule.minimum_tick,
                rule.probability,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.from.value().to_be_bytes());
            bytes.extend_from_slice(&rule.to.value().to_be_bytes());
            bytes.extend_from_slice(&rule.minimum_tick.to_be_bytes());
            bytes.extend_from_slice(&rule.probability.bits().to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "succession", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::FieldSample | GraphOperator::PaintedTile
    ) {
        bytes.push(unit.require_authoritative_fields.into());
    } else {
        bytes.push(0);
    }
    let mut dependencies = node
        .dependencies
        .iter()
        .copied()
        .filter(|dependency| {
            node.definition.operator != GraphOperator::ModuleCall
                || node.definition.dependencies.contains(&dependency.source)
        })
        .collect::<Vec<_>>();
    dependencies.sort();
    dependencies.dedup();
    append_identity_collection(&mut bytes, "dependencies", dependencies.len());
    for dependency in dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    Ok(sha256(&bytes))
}

fn append_identity_text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn append_identity_collection(bytes: &mut Vec<u8>, name: &str, count: usize) {
    append_identity_text(bytes, name);
    bytes.extend_from_slice(&(count as u64).to_be_bytes());
}

fn live_node_dependencies(
    node: &CompiledGraphNode,
) -> impl Iterator<Item = GraphDependencyFingerprint> + '_ {
    node.dependencies.iter().copied().filter(|dependency| {
        if node.definition.operator != GraphOperator::ModuleCall {
            return true;
        }
        node.definition.dependencies.contains(&dependency.source)
    })
}

fn spatial_node_dependencies(node: &CompiledGraphNode) -> Vec<GraphDependencyFingerprint> {
    if node.definition.operator != GraphOperator::ModuleCall {
        return node.dependencies.clone();
    }
    node.dependencies
        .iter()
        .copied()
        .filter(|dependency| node.definition.dependencies.contains(&dependency.source))
        .collect()
}

fn estimate_demand_pin(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    pin: &QualifiedGraphPin,
    estimates: &mut BTreeMap<QualifiedGraphPin, GraphEstimate>,
    visiting: &mut BTreeSet<QualifiedGraphPin>,
) -> Result<GraphEstimate> {
    if let Some(estimate) = estimates.get(pin).copied() {
        return Ok(estimate);
    }
    if !visiting.insert(pin.clone()) {
        return Err(Error::GraphCycle {
            node: pin.node.node,
        });
    }
    let unit = compiled_unit_at_path(root, &pin.node.module_path)?;
    let node = unit
        .nodes
        .iter()
        .find(|node| node.definition.guid == pin.node.node)
        .ok_or_else(|| graph_document("graph.demand.estimate", "live output node is missing"))?;
    let estimate = match node.definition.operator {
        GraphOperator::InterfaceInput => {
            let Some(GraphParameterValue::String(name)) = node.definition.parameter("name") else {
                return Err(graph_document(
                    "graph.demand.estimate",
                    "interface input name is missing",
                ));
            };
            let module_path = pin.node.module_path.as_slice();
            let parent_path = &module_path[..module_path.len() - 1];
            let parent = compiled_unit_at_path(root, parent_path)?;
            let call = module_call_by_guid(parent, module_path[module_path.len() - 1])?;
            let edge = parent
                .edges
                .iter()
                .find(|edge| edge.to_node == call.definition.guid && edge.to_pin.as_str() == name)
                .ok_or_else(|| {
                    graph_document("graph.demand.estimate", "live module input edge is missing")
                })?;
            let source = parent
                .nodes
                .iter()
                .find(|source| source.definition.guid == edge.from_node)
                .ok_or_else(|| {
                    graph_document(
                        "graph.demand.estimate",
                        "live module input source is missing",
                    )
                })?;
            estimate_demand_pin(
                root,
                demand,
                &QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                estimates,
                visiting,
            )?
        }
        GraphOperator::ModuleCall => {
            let module = node.module.as_deref().ok_or_else(|| {
                graph_document("graph.demand.estimate", "compiled module is missing")
            })?;
            let output = module
                .outputs
                .iter()
                .find(|output| output.name == pin.pin)
                .ok_or_else(|| {
                    graph_document("graph.demand.estimate", "live module output is missing")
                })?;
            let source = module
                .nodes
                .iter()
                .find(|source| source.definition.guid == output.node)
                .ok_or_else(|| {
                    graph_document(
                        "graph.demand.estimate",
                        "live module output source is missing",
                    )
                })?;
            estimate_demand_pin(
                root,
                demand,
                &QualifiedGraphPin {
                    node: source.address(),
                    pin: output.pin.clone(),
                },
                estimates,
                visiting,
            )?
        }
        _ => {
            let unit_demand = demand.unit(&pin.node.module_path).ok_or_else(|| {
                graph_document("graph.demand.estimate", "live unit slice is missing")
            })?;
            let mut upstream = GraphEstimate::default();
            for edge in unit.edges.iter().filter(|edge| {
                edge.to_node == node.definition.guid
                    && unit_demand.contains_edge(edge)
                    && node
                        .definition
                        .operator
                        .output_requires_input(&pin.pin, &edge.to_pin)
            }) {
                let source = unit
                    .nodes
                    .iter()
                    .find(|source| source.definition.guid == edge.from_node)
                    .ok_or_else(|| {
                        graph_document("graph.demand.estimate", "live edge source node is missing")
                    })?;
                upstream = merge_input_estimate(
                    upstream,
                    estimate_demand_pin(
                        root,
                        demand,
                        &QualifiedGraphPin {
                            node: source.address(),
                            pin: edge.from_pin.clone(),
                        },
                        estimates,
                        visiting,
                    )?,
                )?;
            }
            estimate_node(&node.definition, upstream)?
        }
    };
    visiting.remove(pin);
    estimates.insert(pin.clone(), estimate);
    Ok(estimate)
}

fn compiled_unit_at_path<'a>(
    root: &'a CompiledGraphUnit,
    module_path: &[u128],
) -> Result<&'a CompiledGraphUnit> {
    let mut unit = root;
    for call_guid in module_path {
        unit = module_call_by_guid(unit, *call_guid)?
            .module
            .as_deref()
            .ok_or_else(|| {
                graph_document("graph.demand.modulePath", "compiled module is missing")
            })?;
    }
    Ok(unit)
}

fn module_call_by_guid(unit: &CompiledGraphUnit, call_guid: u128) -> Result<&CompiledGraphNode> {
    unit.nodes
        .iter()
        .find(|node| {
            node.definition.operator == GraphOperator::ModuleCall
                && matches!(
                    node.definition.parameter("callGuid"),
                    Some(GraphParameterValue::Guid(candidate)) if *candidate == call_guid
                )
        })
        .ok_or_else(|| graph_document("graph.demand.modulePath", "module call is missing"))
}

fn module_call_guid(node: &CompiledGraphNode) -> Result<u128> {
    match node.definition.parameter("callGuid") {
        Some(GraphParameterValue::Guid(call_guid)) => Ok(*call_guid),
        _ => Err(graph_document(
            "graph.demand.moduleCall",
            "module call GUID is missing",
        )),
    }
}

fn canonicalize_demand_slice(
    unit: &CompiledGraphUnit,
    module_path: &[u128],
    slice: &mut CompiledDemandSlice,
) -> Result<()> {
    if let Some(unit_slice) = slice.units.get_mut(module_path) {
        unit_slice.nodes = unit
            .nodes
            .iter()
            .filter(|node| slice.nodes.contains(&node.address()))
            .map(|node| node.definition.guid)
            .collect();
        unit_slice.edges = unit
            .edges
            .iter()
            .filter(|edge| unit_slice.edges.contains(edge))
            .cloned()
            .collect();
    }
    for node in &unit.nodes {
        if let Some(module) = node.module.as_deref() {
            let mut child_path = module_path.to_vec();
            child_path.push(module_call_guid(node)?);
            canonicalize_demand_slice(module, &child_path, slice)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandSlice {
    pub(crate) nodes: BTreeSet<GraphNodeAddress>,
    pub(crate) executed_nodes: BTreeSet<GraphNodeAddress>,
    pub(crate) input_pins: BTreeSet<QualifiedGraphPin>,
    pub(crate) output_pins: BTreeSet<QualifiedGraphPin>,
    pub(crate) estimates: BTreeMap<QualifiedGraphPin, GraphEstimate>,
    pub(crate) dependencies: Vec<GraphDependencyFingerprint>,
    pub(crate) units: BTreeMap<Vec<u128>, CompiledDemandUnitSlice>,
}

impl CompiledDemandSlice {
    pub(crate) fn unit(&self, module_path: &[u128]) -> Option<&CompiledDemandUnitSlice> {
        self.units.get(module_path)
    }

    pub(crate) fn contains_node(&self, address: &GraphNodeAddress) -> bool {
        self.nodes.contains(address)
    }

    pub(crate) fn executes_node(&self, address: &GraphNodeAddress) -> bool {
        self.executed_nodes.contains(address)
    }

    pub(crate) fn estimate(&self, pin: &QualifiedGraphPin) -> Option<GraphEstimate> {
        self.estimates.get(pin).copied()
    }

    pub(crate) fn node_estimate(&self, address: &GraphNodeAddress) -> GraphEstimate {
        self.estimates
            .iter()
            .filter(|(pin, _)| pin.node == *address)
            .map(|(_, estimate)| *estimate)
            .fold(GraphEstimate::default(), max_estimate)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandPlan {
    execution: CompiledDemandSlice,
    public: CompiledDemandSlice,
    stages: BTreeMap<[u8; 32], CompiledDemandSlice>,
}

impl CompiledDemandPlan {
    pub(crate) const fn execution_slice(&self) -> &CompiledDemandSlice {
        &self.execution
    }

    pub(crate) const fn public_slice(&self) -> &CompiledDemandSlice {
        &self.public
    }

    pub(crate) fn stage_slice(&self, stage: [u8; 32]) -> Option<&CompiledDemandSlice> {
        self.stages.get(&stage)
    }

    pub(crate) fn same_scope_membership(
        &self,
        left: &GraphNodeAddress,
        right: &GraphNodeAddress,
    ) -> bool {
        self.public.executes_node(left) == self.public.executes_node(right)
            && self
                .stages
                .values()
                .all(|stage| stage.executes_node(left) == stage.executes_node(right))
    }
}

/// Compiler inputs that do not alter graph semantics.
pub struct GraphCompileOptions {
    /// Hard estimates and evaluator caps.
    pub limits: GraphSafetyLimits,
}

impl GraphCompileOptions {
    /// Canonical compilation with the default hard limits.
    #[must_use]
    pub fn canonical() -> Self {
        Self {
            limits: GraphSafetyLimits::default(),
        }
    }
}

/// Compiles one root biome and its ordinary `.sbiome` modules into the canonical IR.
pub fn compile_biome_graph(
    root: &BiomeAsset,
    root_bindings: &[(u128, Value)],
    resolver: &dyn BiomeGraphResolver,
    options: GraphCompileOptions,
) -> Result<CompiledBiomeGraph> {
    validate_biome(root)?;
    if root.role != BiomeRole::Root {
        return Err(graph_document(
            "biome.role",
            "root compilation requires a root biome",
        ));
    }
    let mut stack = Vec::new();
    let mut root_unit = compile_unit(
        root,
        root_bindings,
        resolver,
        &mut stack,
        &[],
        usize::from(options.limits.max_module_depth),
    )?;
    let root_seeds = root_unit
        .outputs
        .iter()
        .map(|output| {
            let source = root_unit
                .nodes
                .iter()
                .find(|node| node.definition.guid == output.node)
                .ok_or_else(|| graph_document("graph.outputs", "output source node is missing"))?;
            Ok(QualifiedGraphPin {
                node: source.address(),
                pin: output.pin.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut execution_demand =
        compile_demand_slice(&root_unit, root_seeds.iter().cloned(), &BTreeSet::new())?;
    execution_demand
        .units
        .entry(Vec::new())
        .or_default()
        .outputs
        .extend(root_unit.outputs.iter().map(|output| output.name.clone()));
    apply_demanded_estimates(&mut root_unit, &[], &execution_demand)?;
    root_unit.dependencies = execution_demand.dependencies.clone();
    let live_estimate = demanded_estimate(&execution_demand);
    enforce_limits(live_estimate, options.limits)?;
    enforce_demanded_halo_policies(&root_unit, &execution_demand)?;
    let required_halo_by_level = demanded_output_halo(&root_unit, &execution_demand, &root_seeds)?;
    let mut identity_bytes = b"saffron-anima/compiled-biome-execution/v2\0".to_vec();
    identity_bytes.extend_from_slice(&BIOME_GRAPH_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&BIOME_INTERFACE_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&BIOME_NODE_VERSION.to_be_bytes());
    identity_bytes.extend_from_slice(&root.id.value().to_be_bytes());
    identity_bytes.extend_from_slice(&demand_unit_semantic_hash(
        &root_unit,
        &execution_demand,
        &[],
    )?);
    append_identity_collection(
        &mut identity_bytes,
        "dependencies",
        execution_demand.dependencies.len(),
    );
    for dependency in &execution_demand.dependencies {
        append_dependency_source(&mut identity_bytes, dependency.source);
        identity_bytes.extend_from_slice(&dependency.content_hash);
    }
    let identity = sha256(&identity_bytes);
    let spatial_plan = compile_spatial_plan(&root_unit, &execution_demand)?;
    let global_outputs = spatial_plan
        .global_stages()
        .iter()
        .flat_map(|stage| stage.output_pins.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut public_demand = compile_demand_slice(&root_unit, root_seeds, &global_outputs)?;
    public_demand
        .units
        .entry(Vec::new())
        .or_default()
        .outputs
        .extend(root_unit.outputs.iter().map(|output| output.name.clone()));
    let stages = spatial_plan
        .global_stages()
        .iter()
        .map(|stage| {
            Ok((
                stage.id,
                compile_demand_slice(
                    &root_unit,
                    stage.output_pins.iter().cloned(),
                    &stage.input_pins.iter().cloned().collect(),
                )?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(CompiledBiomeGraph {
        root: root_unit,
        biome: root.id,
        identity,
        limits: options.limits,
        demand_plan: CompiledDemandPlan {
            execution: execution_demand,
            public: public_demand,
            stages,
        },
        spatial_plan,
        required_halo_by_level,
    })
}

fn compile_unit(
    asset: &BiomeAsset,
    bindings: &[(u128, Value)],
    resolver: &dyn BiomeGraphResolver,
    stack: &mut Vec<Uuid>,
    module_path: &[u128],
    inherited_depth_limit: usize,
) -> Result<CompiledGraphUnit> {
    if stack.contains(&asset.id) {
        return Err(Error::GraphCycle {
            node: u128::from(asset.id.value()),
        });
    }
    let module_depth = module_path.len();
    if module_depth > inherited_depth_limit {
        return Err(Error::GraphLimit {
            resource: "module recursion",
            requested: u64::try_from(module_depth).map_err(|_| Error::NumericOverflow)?,
            limit: u64::try_from(inherited_depth_limit).map_err(|_| Error::NumericOverflow)?,
        });
    }
    let descendant_depth_limit = inherited_depth_limit.min(
        module_depth
            .checked_add(usize::from(asset.policy.maximum_recursion))
            .ok_or(Error::NumericOverflow)?,
    );
    stack.push(asset.id);
    validate_biome(asset)?;
    let mut document = BiomeGraphDocument::from_json(&asset.graph)?;
    resolve_parameter_bindings(&mut document, asset, bindings)?;
    validate_interface(&document, asset.role)?;
    let node_map: BTreeMap<_, _> = document
        .nodes
        .iter()
        .map(|node| (node.guid, node))
        .collect();
    if node_map.len() != document.nodes.len() || node_map.contains_key(&0) {
        return Err(graph_document(
            "graph.nodes.guid",
            "node GUIDs must be unique and non-zero",
        ));
    }
    for binding in &asset.suitability {
        if node_map
            .get(&binding.node_guid)
            .is_none_or(|node| node.operator != GraphOperator::Suitability)
        {
            return Err(graph_document(
                "biome.suitability",
                "every suitability binding must target one suitability node",
            ));
        }
    }
    if document.nodes.iter().any(|node| {
        node.operator == GraphOperator::Suitability
            && !asset
                .suitability
                .iter()
                .any(|binding| binding.node_guid == node.guid)
    }) {
        return Err(graph_document(
            "biome.suitability",
            "every suitability node requires exactly one suitability binding",
        ));
    }
    let mut module_by_call = BTreeMap::new();
    for module in &asset.modules {
        if module_by_call.insert(module.call_guid, module).is_some() || module.call_guid == 0 {
            return Err(graph_document(
                "biome.modules.callGuid",
                "module call GUIDs must be unique and non-zero",
            ));
        }
    }
    let mut module_units = BTreeMap::new();
    let mut used_module_calls = BTreeSet::new();
    let declared_seed_namespaces: BTreeSet<_> =
        asset.seed_namespaces.iter().map(|(_, id)| *id).collect();
    for node in &document.nodes {
        validate_node_definition(node)?;
        validate_spatial_contract(node, asset)?;
        if node
            .seed_namespaces
            .values()
            .any(|namespace| !declared_seed_namespaces.contains(namespace))
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
                "node uses a seed namespace not declared by its biome",
            ));
        }
        if node.operator == GraphOperator::ModuleCall {
            let call_guid = required_guid(node, "callGuid")?;
            if !used_module_calls.insert(call_guid) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.callGuid", node.guid),
                    "module call GUID is already used by another node",
                ));
            }
            let module_ref = module_by_call.get(&call_guid).ok_or_else(|| {
                graph_document(
                    &format!("graph.nodes.{:032x}.callGuid", node.guid),
                    "no matching biome module reference",
                )
            })?;
            let module_asset = resolver.resolve_biome(module_ref.biome)?;
            if module_asset.role != BiomeRole::Module {
                return Err(graph_document(
                    "biome.modules",
                    "module references must target module-role biome assets",
                ));
            }
            validate_parameter_bindings(&module_asset, &module_ref.bindings)?;
            let mut child_path = module_path.to_vec();
            child_path.push(call_guid);
            let unit = compile_unit(
                &module_asset,
                &module_ref.bindings,
                resolver,
                stack,
                &child_path,
                descendant_depth_limit,
            )?;
            module_units.insert(node.guid, Box::new(unit));
        }
    }
    if used_module_calls.len() != module_by_call.len() {
        return Err(graph_document(
            "biome.modules",
            "every module reference requires exactly one matching module-call node",
        ));
    }
    let signatures = node_signatures(&document.nodes, &document.inputs, &module_units)?;
    validate_edges(&document, &signatures)?;
    let order = topological_order(&document.nodes, &document.edges)?;
    let incoming = incoming_edges(&document.edges);
    let mut authority_by_pin: BTreeMap<(u128, String), GraphAuthority> = BTreeMap::new();
    let mut estimate_by_pin: BTreeMap<(u128, String), GraphEstimate> = BTreeMap::new();
    let mut lineage_by_pin: BTreeMap<(u128, String), GraphValueLineage> = BTreeMap::new();
    let mut compiled_nodes = Vec::with_capacity(order.len());
    for guid in order {
        let definition = node_map[&guid].clone();
        let (inputs, outputs) = signatures[&guid].clone();
        let mut authority = definition.authority;
        let mut upstream_estimate = GraphEstimate::default();
        let mut input_lineage = BTreeMap::new();
        for edge in incoming.get(&guid).into_iter().flatten() {
            let key = (edge.from_node, edge.from_pin.clone());
            authority = authority.join(authority_by_pin[&key]);
            upstream_estimate = merge_input_estimate(upstream_estimate, estimate_by_pin[&key])?;
            if let Some(lineage) = lineage_by_pin.get(&key) {
                input_lineage.insert(edge.to_pin.clone(), lineage.clone());
            }
        }
        if definition.authority == GraphAuthority::EquivalentGpu
            && !definition.operator.has_slang_executor()
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.authority", definition.guid),
                "equivalent-gpu authority requires a complete Slang implementation",
            ));
        }
        if definition.operator == GraphOperator::Transform
            && input_lineage.contains_key("offset")
            && definition.spatial.influence_radius().bits() <= 0
        {
            return Err(Error::GraphUnboundedInfluence {
                node: definition.guid,
            });
        }
        let base_estimate = estimate_node(&definition, upstream_estimate)?;
        let compiled_module = module_units.get(&guid).map(Box::as_ref);
        let dependencies =
            compile_node_dependencies(&definition, asset, compiled_module, resolver)?;
        let output_lineage = resolve_node_lineage(
            &definition,
            &outputs,
            &input_lineage,
            compiled_module,
            module_path,
        )?;
        let mut output_authority = BTreeMap::new();
        let mut output_estimates = BTreeMap::new();
        for output in &outputs {
            let output_authority_value = compiled_module
                .and_then(|module| module.output_authority.get(&output.name))
                .copied()
                .map_or(authority, |module_authority| {
                    authority.join(module_authority)
                });
            let output_estimate = compiled_module
                .and_then(|module| module.output_estimates.get(&output.name))
                .copied()
                .map_or(base_estimate, |module_estimate| {
                    max_estimate(base_estimate, module_estimate)
                });
            authority_by_pin.insert((guid, output.name.clone()), output_authority_value);
            estimate_by_pin.insert((guid, output.name.clone()), output_estimate);
            output_authority.insert(output.name.clone(), output_authority_value);
            output_estimates.insert(output.name.clone(), output_estimate);
            if let Some(lineage) = output_lineage.get(&output.name) {
                lineage_by_pin.insert((guid, output.name.clone()), lineage.clone());
            }
        }
        let estimate = output_estimates
            .values()
            .copied()
            .fold(base_estimate, max_estimate);
        let definition_hash = sha256(&definition.canonical_bytes());
        compiled_nodes.push(CompiledGraphNode {
            definition_hash,
            inputs,
            outputs,
            parameter_schema: definition.operator.parameter_schema(),
            capabilities: ExecutionCapabilities {
                reference_cpu: true,
                parallel_cpu: true,
                slang_compute: definition.operator.has_slang_executor(),
            },
            output_authority,
            output_lineage,
            output_estimates,
            estimate,
            dependencies,
            debug_symbol: GraphDebugSymbol {
                module_path: module_path.to_vec(),
                node: guid,
                label: format!("{}:{guid:032x}", definition.operator.as_wire()),
            },
            module: module_units.remove(&guid),
            definition,
        });
    }
    for output in &document.outputs {
        if let Some(sink) = output.sink
            && output.domain != sink.expected_domain()
        {
            return Err(graph_document(
                &format!("graph.outputs.{}.domain", output.name),
                &format!(
                    "{} sink requires {} domain",
                    sink.as_wire(),
                    sink.expected_domain().as_wire()
                ),
            ));
        }
        let source = authority_by_pin
            .get(&(output.node, output.pin.clone()))
            .copied()
            .ok_or_else(|| graph_document("graph.outputs", "output source pin does not exist"))?;
        if output.sink.is_some_and(GraphSink::requires_authority) && !source.can_feed_authority() {
            return Err(Error::GraphAuthority {
                node: output.node,
                reason: format!(
                    "cosmetic value reaches {} output '{}'",
                    output.sink.unwrap().as_wire(),
                    output.name
                ),
            });
        }
    }
    let output_authority = document
        .outputs
        .iter()
        .map(|output| {
            Ok((
                output.name.clone(),
                *authority_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .ok_or_else(|| {
                        graph_document("graph.outputs", "output authority is missing")
                    })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let output_estimates = document
        .outputs
        .iter()
        .map(|output| {
            Ok((
                output.name.clone(),
                *estimate_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .ok_or_else(|| graph_document("graph.outputs", "output estimate is missing"))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let output_lineage = document
        .outputs
        .iter()
        .filter(|output| domain_has_candidate_lineage(output.domain))
        .map(|output| {
            Ok((
                output.name.clone(),
                lineage_by_pin
                    .get(&(output.node, output.pin.clone()))
                    .cloned()
                    .ok_or_else(|| {
                        graph_document("graph.outputs", "candidate lineage is missing")
                    })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut edges = document.edges.clone();
    edges.sort_by(|left, right| {
        (left.to_node, &left.to_pin, left.from_node, &left.from_pin).cmp(&(
            right.to_node,
            &right.to_pin,
            right.from_node,
            &right.from_pin,
        ))
    });
    let mut dependencies = BTreeMap::new();
    for palette in &asset.palette {
        let source = GraphDependencySource::Asset(palette.plant);
        dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
    }
    for binding in &asset.suitability {
        let source = GraphDependencySource::Field(binding.channel);
        dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
    }
    for node in &compiled_nodes {
        for dependency in inferred_node_dependencies(&node.definition, resolver)? {
            dependencies.insert(dependency, resolver.resolve_dependency_hash(dependency)?);
        }
        for dependency in &node.definition.dependencies {
            dependencies.insert(*dependency, resolver.resolve_dependency_hash(*dependency)?);
        }
        if let Some(module) = &node.module {
            let source = GraphDependencySource::Asset(module.biome);
            dependencies.insert(source, resolver.resolve_dependency_hash(source)?);
            dependencies.extend(
                module
                    .dependencies
                    .iter()
                    .map(|dependency| (dependency.source, dependency.content_hash)),
            );
        }
    }
    let estimate = compiled_nodes
        .iter()
        .fold(GraphEstimate::default(), |total, node| {
            max_estimate(total, node.estimate)
        });
    let output_halo_by_level = compile_output_halo(&compiled_nodes, &edges, &document.outputs)?;
    let mut semantic_identity = b"saffron-anima/compiled-biome-unit/v1\0".to_vec();
    semantic_identity.extend_from_slice(&sha256(&crate::write_biome_asset(asset)?));
    semantic_identity.extend_from_slice(&document.identity());
    let document_hash = sha256(&semantic_identity);
    let result = CompiledGraphUnit {
        biome: asset.id,
        role: asset.role,
        inputs: document.inputs,
        outputs: document.outputs,
        output_authority,
        output_estimates,
        output_lineage,
        nodes: compiled_nodes,
        edges,
        document_hash,
        dependencies: dependencies
            .into_iter()
            .map(|(source, content_hash)| GraphDependencyFingerprint {
                source,
                content_hash,
            })
            .collect(),
        palette: asset.palette.clone(),
        suitability: asset.suitability.clone(),
        competition: asset.competition.clone(),
        companions: asset.companions.clone(),
        succession: asset.succession.clone(),
        require_authoritative_fields: asset.policy.require_authoritative_fields,
        maximum_influence_radius: asset.policy.maximum_influence_radius,
        estimate,
        output_halo_by_level,
    };
    stack.pop();
    Ok(result)
}

#[derive(Clone)]
struct SpatialPlanNode {
    address: GraphNodeAddress,
    semantic_hash: [u8; 32],
    spatial: NodeSpatialPolicy,
    estimate: GraphEstimate,
    dependencies: Vec<GraphDependencyFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SpatialPlanEdge {
    from: QualifiedGraphPin,
    to: QualifiedGraphPin,
}

#[derive(Default)]
struct FlattenedSpatialGraph {
    nodes: Vec<SpatialPlanNode>,
    edges: Vec<SpatialPlanEdge>,
    outputs: Vec<QualifiedGraphPin>,
}

fn compile_spatial_plan(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<CompiledSpatialPlan> {
    let mut flattened = FlattenedSpatialGraph::default();
    let outputs = flatten_spatial_unit(root, &BTreeMap::new(), &[], demand, &mut flattened)?;
    flattened.outputs = root
        .outputs
        .iter()
        .map(|output| {
            outputs.get(&output.name).cloned().ok_or_else(|| {
                graph_document("graph.outputs", "flattened output source is missing")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    flattened.edges.sort();
    flattened.edges.dedup();

    let node_indices = flattened
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.address.clone(), index))
        .collect::<BTreeMap<_, _>>();
    if node_indices.len() != flattened.nodes.len() {
        return Err(graph_document(
            "graph.spatialPlan",
            "flattened node addresses are not unique",
        ));
    }
    let global_nodes = flattened
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            matches!(node.spatial, NodeSpatialPolicy::Global { .. }).then_some(index)
        })
        .collect::<BTreeSet<_>>();
    let mut global_adjacency = BTreeMap::<usize, BTreeSet<usize>>::new();
    for edge in &flattened.edges {
        let Some(&from) = node_indices.get(&edge.from.node) else {
            continue;
        };
        let Some(&to) = node_indices.get(&edge.to.node) else {
            continue;
        };
        if global_nodes.contains(&from)
            && global_nodes.contains(&to)
            && flattened.nodes[from].spatial.level() == flattened.nodes[to].spatial.level()
        {
            global_adjacency.entry(from).or_default().insert(to);
            global_adjacency.entry(to).or_default().insert(from);
        }
    }

    let mut components = Vec::<Vec<usize>>::new();
    let mut assigned = BTreeSet::new();
    for &start in &global_nodes {
        if !assigned.insert(start) {
            continue;
        }
        let mut pending = vec![start];
        let mut component = Vec::new();
        while let Some(index) = pending.pop() {
            component.push(index);
            for &neighbour in global_adjacency.get(&index).into_iter().flatten() {
                if assigned.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }

    let mut component_by_node = BTreeMap::<GraphNodeAddress, usize>::new();
    for (component, nodes) in components.iter().enumerate() {
        for &index in nodes {
            component_by_node.insert(flattened.nodes[index].address.clone(), component);
        }
    }
    let mut incoming = BTreeMap::<GraphNodeAddress, Vec<&SpatialPlanEdge>>::new();
    for edge in &flattened.edges {
        incoming.entry(edge.to.node.clone()).or_default().push(edge);
    }

    let mut stages = Vec::with_capacity(components.len());
    let mut global_stage_by_node = BTreeMap::new();
    for (component_index, component) in components.iter().enumerate() {
        let owner_level = flattened.nodes[component[0]].spatial.level();
        let member_addresses = component
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<BTreeSet<_>>();
        let mut closure_indices = component.iter().copied().collect::<BTreeSet<_>>();
        let mut pending = component.clone();
        let mut input_pins = BTreeSet::new();
        while let Some(index) = pending.pop() {
            let address = &flattened.nodes[index].address;
            for edge in incoming.get(address).into_iter().flatten() {
                if component_by_node
                    .get(&edge.from.node)
                    .is_some_and(|source| *source != component_index)
                {
                    input_pins.insert(edge.from.clone());
                    continue;
                }
                let source = *node_indices.get(&edge.from.node).ok_or_else(|| {
                    graph_document("graph.spatialPlan", "upstream node is missing")
                })?;
                if closure_indices.insert(source) {
                    pending.push(source);
                }
            }
        }

        let mut output_pins = BTreeSet::new();
        for edge in &flattened.edges {
            if member_addresses.contains(&edge.from.node)
                && !member_addresses.contains(&edge.to.node)
            {
                output_pins.insert(edge.from.clone());
            }
        }
        for output in &flattened.outputs {
            if member_addresses.contains(&output.node) {
                output_pins.insert(output.clone());
            }
        }

        let closure = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<Vec<_>>();
        let dependencies = closure_indices
            .iter()
            .flat_map(|&index| flattened.nodes[index].dependencies.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let minimum_input_level = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].spatial.level())
            .min()
            .unwrap_or(owner_level);
        let upstream_halo = global_stage_upstream_halo(
            component,
            &closure_indices,
            &flattened.nodes,
            &flattened.edges,
            &node_indices,
        )?;
        let estimate = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].estimate)
            .fold(GraphEstimate::default(), max_estimate);
        let nodes = component
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<Vec<_>>();
        let input_pins = input_pins.into_iter().collect::<Vec<_>>();
        let output_pins = output_pins.into_iter().collect::<Vec<_>>();
        let prerequisite_stages = input_pins
            .iter()
            .filter_map(|pin| global_stage_by_node.get(&pin.node).copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let id = global_stage_identity(&GlobalStageIdentityInputs {
            root_biome: root.biome,
            owner_level,
            nodes: &nodes,
            closure: &closure,
            flattened_nodes: &flattened.nodes,
            edges: &flattened.edges,
            input_pins: &input_pins,
            output_pins: &output_pins,
            dependencies: &dependencies,
            prerequisite_stages: &prerequisite_stages,
        });
        for node in &nodes {
            global_stage_by_node.insert(node.clone(), id);
        }
        stages.push(CompiledGlobalStage {
            id,
            owner_level,
            nodes,
            closure,
            input_pins,
            output_pins,
            dependencies,
            minimum_input_level,
            upstream_halo,
            estimate,
        });
    }
    Ok(CompiledSpatialPlan {
        global_stages: stages,
        global_stage_by_node,
    })
}

fn global_stage_upstream_halo(
    stage_nodes: &[usize],
    closure: &BTreeSet<usize>,
    nodes: &[SpatialPlanNode],
    edges: &[SpatialPlanEdge],
    node_indices: &BTreeMap<GraphNodeAddress, usize>,
) -> Result<DecisionScalar> {
    let mut support_by_node = BTreeMap::<GraphNodeAddress, DecisionScalar>::new();
    for &index in closure {
        let node = &nodes[index];
        let upstream = edges
            .iter()
            .filter(|edge| edge.to.node == node.address)
            .filter_map(|edge| support_by_node.get(&edge.from.node).copied())
            .max()
            .unwrap_or(DecisionScalar::from_bits(0));
        let local = match node.spatial {
            NodeSpatialPolicy::Partitioned {
                influence_radius, ..
            } => influence_radius,
            NodeSpatialPolicy::Global { .. } => DecisionScalar::from_bits(0),
        };
        support_by_node.insert(node.address.clone(), upstream.checked_add(local)?);
    }
    stage_nodes
        .iter()
        .map(|&index| {
            let address = &nodes[index].address;
            if !node_indices.contains_key(address) {
                return Err(graph_document(
                    "graph.spatialPlan",
                    "global stage node is missing",
                ));
            }
            support_by_node.get(address).copied().ok_or_else(|| {
                graph_document("graph.spatialPlan", "global stage support is missing")
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(|support| {
            support
                .into_iter()
                .max()
                .unwrap_or(DecisionScalar::from_bits(0))
        })
}

fn flatten_spatial_unit(
    unit: &CompiledGraphUnit,
    interface_sources: &BTreeMap<String, QualifiedGraphPin>,
    inherited_dependencies: &[GraphDependencyFingerprint],
    demand: &CompiledDemandSlice,
    flattened: &mut FlattenedSpatialGraph,
) -> Result<BTreeMap<String, QualifiedGraphPin>> {
    let incoming = incoming_edges(&unit.edges);
    let module_path = unit
        .nodes
        .first()
        .map_or_else(Vec::new, |node| node.debug_symbol.module_path.clone());
    let unit_demand = demand.unit(&module_path).ok_or_else(|| {
        graph_document(
            "graph.spatialPlan",
            "demanded compiled unit slice is missing",
        )
    })?;
    let mut aliases = BTreeMap::<(u128, String), QualifiedGraphPin>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        if node.definition.operator == GraphOperator::InterfaceInput {
            let name = match node.definition.parameter("name") {
                Some(GraphParameterValue::String(name)) => name,
                _ => {
                    return Err(graph_document(
                        "graph.spatialPlan.interfaceInput",
                        "interface input name is missing",
                    ));
                }
            };
            let source = interface_sources.get(name).cloned().ok_or_else(|| {
                graph_document(
                    "graph.spatialPlan.interfaceInput",
                    "module input source is missing",
                )
            })?;
            for output in &node.outputs {
                let qualified = QualifiedGraphPin {
                    node: node.address(),
                    pin: output.name.clone(),
                };
                if !demand.output_pins.contains(&qualified) {
                    continue;
                }
                aliases.insert((node.definition.guid, output.name.clone()), source.clone());
            }
            continue;
        }

        let address = node.address();
        if node.definition.operator == GraphOperator::ModuleCall {
            let module = node.module.as_deref().ok_or_else(|| {
                graph_document("graph.spatialPlan.moduleCall", "compiled module is missing")
            })?;
            let direct_dependencies = spatial_node_dependencies(node);
            let module_dependencies =
                merged_dependencies(&direct_dependencies, inherited_dependencies);
            let mut module_inputs = BTreeMap::new();
            for input in node.inputs.iter().filter(|input| {
                demand.input_pins.contains(&QualifiedGraphPin {
                    node: address.clone(),
                    pin: input.name.clone(),
                })
            }) {
                let edge = incoming
                    .get(&node.definition.guid)
                    .into_iter()
                    .flatten()
                    .find(|edge| edge.to_pin == input.name)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.spatialPlan.moduleCall",
                            "module input edge is missing",
                        )
                    })?;
                let source = resolve_spatial_source(edge, &aliases)?;
                module_inputs.insert(input.name.clone(), source);
            }
            let module_outputs = flatten_spatial_unit(
                module,
                &module_inputs,
                &module_dependencies,
                demand,
                flattened,
            )?;
            flattened.nodes.push(SpatialPlanNode {
                address: address.clone(),
                semantic_hash: live_node_semantic_hash(unit, node, demand, false)?,
                spatial: node.definition.spatial,
                estimate: node.estimate,
                dependencies: merged_dependencies(&direct_dependencies, inherited_dependencies),
            });
            for (name, source) in &module_inputs {
                flattened.edges.push(SpatialPlanEdge {
                    from: source.clone(),
                    to: QualifiedGraphPin {
                        node: address.clone(),
                        pin: name.clone(),
                    },
                });
            }
            for output in node.outputs.iter().filter(|output| {
                demand.output_pins.contains(&QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                })
            }) {
                let source = module_outputs.get(&output.name).cloned().ok_or_else(|| {
                    graph_document(
                        "graph.spatialPlan.moduleCall",
                        "module output source is missing",
                    )
                })?;
                let pin = QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                };
                flattened.edges.push(SpatialPlanEdge {
                    from: source,
                    to: pin.clone(),
                });
                aliases.insert((node.definition.guid, output.name.clone()), pin);
            }
            continue;
        }

        let direct_dependencies = spatial_node_dependencies(node);
        flattened.nodes.push(SpatialPlanNode {
            address: address.clone(),
            semantic_hash: live_node_semantic_hash(unit, node, demand, false)?,
            spatial: node.definition.spatial,
            estimate: node.estimate,
            dependencies: merged_dependencies(&direct_dependencies, inherited_dependencies),
        });
        for edge in incoming
            .get(&node.definition.guid)
            .into_iter()
            .flatten()
            .filter(|edge| unit_demand.contains_edge(edge))
        {
            flattened.edges.push(SpatialPlanEdge {
                from: resolve_spatial_source(edge, &aliases)?,
                to: QualifiedGraphPin {
                    node: address.clone(),
                    pin: edge.to_pin.clone(),
                },
            });
        }
        for output in node.outputs.iter().filter(|output| {
            demand.output_pins.contains(&QualifiedGraphPin {
                node: address.clone(),
                pin: output.name.clone(),
            })
        }) {
            aliases.insert(
                (node.definition.guid, output.name.clone()),
                QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                },
            );
        }
    }
    unit.outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .map(|output| {
            let source = aliases
                .get(&(output.node, output.pin.clone()))
                .cloned()
                .ok_or_else(|| {
                    graph_document("graph.spatialPlan", "unit output source is missing")
                })?;
            Ok((output.name.clone(), source))
        })
        .collect()
}

fn merged_dependencies(
    direct: &[GraphDependencyFingerprint],
    inherited: &[GraphDependencyFingerprint],
) -> Vec<GraphDependencyFingerprint> {
    direct
        .iter()
        .chain(inherited)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_spatial_source(
    edge: &GraphEdge,
    aliases: &BTreeMap<(u128, String), QualifiedGraphPin>,
) -> Result<QualifiedGraphPin> {
    aliases
        .get(&(edge.from_node, edge.from_pin.clone()))
        .cloned()
        .ok_or_else(|| graph_document("graph.spatialPlan", "edge source alias is missing"))
}

struct GlobalStageIdentityInputs<'a> {
    root_biome: Uuid,
    owner_level: u8,
    nodes: &'a [GraphNodeAddress],
    closure: &'a [GraphNodeAddress],
    flattened_nodes: &'a [SpatialPlanNode],
    edges: &'a [SpatialPlanEdge],
    input_pins: &'a [QualifiedGraphPin],
    output_pins: &'a [QualifiedGraphPin],
    dependencies: &'a [GraphDependencyFingerprint],
    prerequisite_stages: &'a [[u8; 32]],
}

fn global_stage_identity(inputs: &GlobalStageIdentityInputs<'_>) -> [u8; 32] {
    let &GlobalStageIdentityInputs {
        root_biome,
        owner_level,
        nodes,
        closure,
        flattened_nodes,
        edges,
        input_pins,
        output_pins,
        dependencies,
        prerequisite_stages,
    } = inputs;
    let mut bytes = b"saffron-anima/vegetation-global-stage/v2\0".to_vec();
    bytes.extend_from_slice(&BIOME_GRAPH_VERSION.to_be_bytes());
    bytes.extend_from_slice(&BIOME_INTERFACE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&BIOME_NODE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&root_biome.value().to_be_bytes());
    bytes.push(owner_level);
    append_identity_collection(&mut bytes, "closure", closure.len());
    for node in closure {
        bytes.extend_from_slice(&node.canonical_bytes());
        if let Some(compiled) = flattened_nodes
            .iter()
            .find(|candidate| candidate.address == *node)
        {
            bytes.extend_from_slice(&compiled.semantic_hash);
        }
    }
    let closure_nodes = closure.iter().collect::<BTreeSet<_>>();
    let live_edges = edges
        .iter()
        .filter(|edge| {
            closure_nodes.contains(&edge.to.node)
                && (closure_nodes.contains(&edge.from.node) || input_pins.contains(&edge.from))
        })
        .collect::<Vec<_>>();
    append_identity_collection(&mut bytes, "edges", live_edges.len());
    for edge in live_edges {
        bytes.extend_from_slice(&edge.from.canonical_bytes());
        bytes.extend_from_slice(&edge.to.canonical_bytes());
    }
    for (name, pins) in [("inputs", input_pins), ("outputs", output_pins)] {
        append_identity_collection(&mut bytes, name, pins.len());
        for pin in pins {
            bytes.extend_from_slice(&pin.canonical_bytes());
        }
    }
    append_identity_collection(&mut bytes, "dependencies", dependencies.len());
    for dependency in dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    append_identity_collection(&mut bytes, "prerequisiteStages", prerequisite_stages.len());
    for prerequisite in prerequisite_stages {
        bytes.extend_from_slice(prerequisite);
    }
    append_identity_collection(&mut bytes, "members", nodes.len());
    for node in nodes {
        bytes.extend_from_slice(&node.canonical_bytes());
    }
    sha256(&bytes)
}

fn compile_node_dependencies(
    node: &GraphNodeDefinition,
    asset: &BiomeAsset,
    module: Option<&CompiledGraphUnit>,
    resolver: &dyn BiomeGraphResolver,
) -> Result<Vec<GraphDependencyFingerprint>> {
    let mut sources = inferred_node_dependencies(node, resolver)?;
    sources.extend(node.dependencies.iter().copied());
    if matches!(
        node.operator,
        GraphOperator::SpeciesInput
            | GraphOperator::Competition
            | GraphOperator::BoundsOverlap
            | GraphOperator::CommunityBlend
            | GraphOperator::MacroOutput
    ) || (node.operator == GraphOperator::VariableSpacing
        && matches!(
            node.parameter("prototypeAware"),
            Some(GraphParameterValue::Boolean(true))
        ))
    {
        sources.extend(
            asset
                .palette
                .iter()
                .map(|entry| GraphDependencySource::Asset(entry.plant)),
        );
    }
    if node.operator == GraphOperator::Suitability {
        sources.extend(
            asset
                .suitability
                .iter()
                .filter(|binding| binding.node_guid == node.guid)
                .map(|binding| GraphDependencySource::Field(binding.channel)),
        );
    }
    let mut dependencies = sources
        .into_iter()
        .map(|source| {
            Ok(GraphDependencyFingerprint {
                source,
                content_hash: resolver.resolve_dependency_hash(source)?,
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if let Some(module) = module {
        let source = GraphDependencySource::Asset(module.biome);
        dependencies.insert(GraphDependencyFingerprint {
            source,
            content_hash: resolver.resolve_dependency_hash(source)?,
        });
        dependencies.extend(module.dependencies.iter().copied());
    }
    Ok(dependencies.into_iter().collect())
}

fn inferred_node_dependencies(
    node: &GraphNodeDefinition,
    resolver: &dyn BiomeGraphResolver,
) -> Result<BTreeSet<GraphDependencySource>> {
    let mut dependencies = BTreeSet::new();
    match node.operator {
        GraphOperator::ExplicitAnchors | GraphOperator::PaintedTile => {
            dependencies.insert(GraphDependencySource::MapLayer(required_guid(
                node, "layer",
            )?));
        }
        GraphOperator::FieldSample => {
            dependencies.insert(GraphDependencySource::Field(
                match node.parameter("channel") {
                    Some(GraphParameterValue::FieldChannel(channel)) => *channel,
                    _ => {
                        return Err(graph_document(
                            &format!("graph.nodes.{:032x}.parameters.channel", node.guid),
                            "field sample requires a typed channel",
                        ));
                    }
                },
            ));
            dependencies.extend(
                resolver
                    .available_dependencies()
                    .into_iter()
                    .filter(|source| matches!(source, GraphDependencySource::SurfaceProvider(_))),
            );
        }
        GraphOperator::SurfaceProjection => {
            let provider = match node.parameter("provider") {
                Some(GraphParameterValue::U64(provider)) => *provider,
                None => 0,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.provider", node.guid),
                        "surface provider filter must be an unsigned identity",
                    ));
                }
            };
            if provider == 0 {
                dependencies.extend(
                    resolver
                        .available_dependencies()
                        .into_iter()
                        .filter(|source| {
                            matches!(source, GraphDependencySource::SurfaceProvider(_))
                        }),
                );
            } else {
                dependencies.insert(GraphDependencySource::SurfaceProvider(provider));
            }
        }
        GraphOperator::DistanceField => {
            let distance_source = match node.parameter("source") {
                Some(GraphParameterValue::DistanceSource(source)) => *source,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.source", node.guid),
                        "distance field requires a typed source",
                    ));
                }
            };
            match distance_source {
                GraphDistanceSource::Water => {
                    dependencies.insert(GraphDependencySource::Field(FieldChannel::WaterDistance));
                }
                GraphDistanceSource::Blocker => {
                    dependencies.insert(GraphDependencySource::Field(FieldChannel::SignedBlocker));
                }
                GraphDistanceSource::Spline | GraphDistanceSource::Shape => {}
            }
            let source = match node.parameter("sourceGuid") {
                Some(GraphParameterValue::Guid(source)) => *source,
                None => 0,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.sourceGuid", node.guid),
                        "distance source identity must be a GUID",
                    ));
                }
            };
            if source == 0 {
                dependencies.extend(
                    resolver
                        .available_dependencies()
                        .into_iter()
                        .filter(|source| matches!(source, GraphDependencySource::MapLayer(_))),
                );
            } else {
                dependencies.insert(GraphDependencySource::MapLayer(source));
            }
        }
        GraphOperator::SplineFollow => {
            dependencies.extend(
                resolver
                    .available_dependencies()
                    .into_iter()
                    .filter(|source| matches!(source, GraphDependencySource::MapLayer(_))),
            );
        }
        _ => {}
    }
    Ok(dependencies)
}

fn validate_node_definition(node: &GraphNodeDefinition) -> Result<()> {
    if node.version != BIOME_NODE_VERSION {
        return Err(Error::FormatVersion {
            format: ".sbiome node",
            found: node.version,
            expected: BIOME_NODE_VERSION,
        });
    }
    if node.semantic_revision == 0 {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.semanticRevision", node.guid),
            "semantic revision must be non-zero",
        ));
    }
    if let NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.spatial
        && influence_radius.bits() < 0
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    let schema = node.operator.parameter_schema();
    for descriptor in &schema {
        if descriptor.required && !node.parameters.contains_key(descriptor.name) {
            return Err(graph_document(
                &format!(
                    "graph.nodes.{:032x}.parameters.{}",
                    node.guid, descriptor.name
                ),
                "required parameter is missing",
            ));
        }
    }
    validate_operator_parameters(node)?;
    let known: BTreeSet<_> = schema.iter().map(|parameter| parameter.name).collect();
    if let Some(unknown) = node
        .parameters
        .keys()
        .find(|key| !known.contains(key.as_str()))
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{unknown}", node.guid),
            "unknown parameter",
        ));
    }
    for descriptor in &schema {
        if let Some(value) = node.parameters.get(descriptor.name)
            && !parameter_matches_type(value, descriptor.parameter_type)
        {
            return Err(graph_document(
                &format!(
                    "graph.nodes.{:032x}.parameters.{}",
                    node.guid, descriptor.name
                ),
                "parameter value does not match its declared type",
            ));
        }
    }
    let expected_seeds = node.operator.seed_namespace_names();
    if node.seed_namespaces.len() != expected_seeds.len()
        || expected_seeds
            .iter()
            .any(|name| !node.seed_namespaces.contains_key(*name))
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespace names do not match the operator's semantic streams",
        ));
    }
    if node
        .seed_namespaces
        .values()
        .any(|namespace| *namespace == 0)
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespaces must be non-zero",
        ));
    }
    let unique_seeds: BTreeSet<_> = node.seed_namespaces.values().copied().collect();
    if unique_seeds.len() != node.seed_namespaces.len() {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespaces must be unique",
        ));
    }
    let unique_dependencies: BTreeSet<_> = node.dependencies.iter().copied().collect();
    if unique_dependencies.len() != node.dependencies.len()
        || node.dependencies.iter().any(|dependency| match dependency {
            GraphDependencySource::Asset(id) => id.value() == 0,
            GraphDependencySource::SurfaceProvider(provider) => *provider == 0,
            GraphDependencySource::MapLayer(layer) => *layer == 0,
            GraphDependencySource::Field(_) => false,
        })
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.dependencies", node.guid),
            "dependencies must be unique and non-zero",
        ));
    }
    Ok(())
}

fn validate_operator_parameters(node: &GraphNodeDefinition) -> Result<()> {
    use GraphOperator as O;
    let positive_u32 = |name: &str| -> Result<()> {
        if required_u32(node, name)? == 0 {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                "value must be positive",
            ));
        }
        Ok(())
    };
    let positive_fixed = |name: &str| -> Result<()> {
        if required_fixed(node, name)?.bits() <= 0 {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                "value must be positive",
            ));
        }
        Ok(())
    };
    match node.operator {
        O::ExplicitAnchors => {
            if required_guid(node, "layer")? == 0 {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.layer", node.guid),
                    "layer identity must be non-zero",
                ));
            }
        }
        O::StratifiedCoverage => positive_u32("count")?,
        O::BlueNoisePoisson => {
            positive_u32("count")?;
            positive_fixed("radius")?;
            if let Some(GraphParameterValue::U32(attempts)) = node.parameter("attempts")
                && *attempts == 0
            {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.attempts", node.guid),
                    "value must be positive",
                ));
            }
        }
        O::SurfaceProjection => {
            positive_fixed("maxDistance")?;
            let direction = match node.parameter("direction") {
                Some(GraphParameterValue::FixedVec3(direction)) => direction,
                _ => {
                    return Err(graph_document(
                        "graph.nodes.surface-projection",
                        "direction is missing",
                    ));
                }
            };
            if direction.iter().all(|value| value.bits() == 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                    "direction must be non-zero",
                ));
            }
        }
        O::PaintedTile => {
            if required_guid(node, "layer")? == 0 {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.layer", node.guid),
                    "layer identity must be non-zero",
                ));
            }
        }
        O::Noise => positive_fixed("frequency")?,
        O::Gradient => {
            let direction = match node.parameter("direction") {
                Some(GraphParameterValue::FixedVec3(direction)) => direction,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                        "direction is missing",
                    ));
                }
            };
            if direction.iter().all(|value| value.bits() == 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                    "direction must be non-zero",
                ));
            }
        }
        O::Remap => {
            if required_fixed(node, "inputMin")? >= required_fixed(node, "inputMax")? {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.inputMax", node.guid),
                    "remap input range must be non-empty",
                ));
            }
        }
        O::Clamp => {
            if required_fixed(node, "minimum")? > required_fixed(node, "maximum")? {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.maximum", node.guid),
                    "clamp range is inverted",
                ));
            }
        }
        O::DistanceField => positive_fixed("maximumDistance")?,
        O::WeightedElimination => {
            positive_u32("targetCount")?;
            positive_fixed("eliminationRadius")?;
            positive_u32("maximumNeighbours")?;
        }
        O::ClusterPatchColony => {
            positive_u32("children")?;
            positive_fixed("radius")?;
        }
        O::SplineFollow => positive_fixed("spacing")?,
        O::RecursiveCompanion => {
            positive_u32("children")?;
            positive_fixed("radius")?;
            positive_u32("maximumDepth")?;
        }
        O::Transform => {
            let minimum =
                optional_fixed(node, "scaleMinimum")?.unwrap_or(DecisionScalar::from_bits(65_536));
            let maximum =
                optional_fixed(node, "scaleMaximum")?.unwrap_or(DecisionScalar::from_bits(65_536));
            if minimum.bits() <= 0 || minimum > maximum {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.scaleMaximum", node.guid),
                    "transform scale range is invalid",
                ));
            }
        }
        O::BoundsOverlap => {
            if optional_fixed(node, "padding")?.is_some_and(|padding| padding.bits() < 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.padding", node.guid),
                    "bounds padding cannot be negative",
                ));
            }
        }
        O::Competition => {
            let crown = match node.parameter("crownWeight") {
                Some(GraphParameterValue::Unit(value)) => *value,
                _ => UnitInterval::ZERO,
            };
            let root = match node.parameter("rootWeight") {
                Some(GraphParameterValue::Unit(value)) => *value,
                _ => UnitInterval::ZERO,
            };
            if crown == UnitInterval::ZERO && root == UnitInterval::ZERO {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.crownWeight", node.guid),
                    "competition crown and root weights cannot both be zero",
                ));
            }
        }
        O::MicroOutput => {
            let dimensions = required_u32_vec3(node, "dimensions")?;
            if dimensions.contains(&0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.dimensions", node.guid),
                    "micro dimensions must be positive",
                ));
            }
            let channels = match node.parameter("attributeChannels") {
                Some(GraphParameterValue::GuidList(channels)) => channels,
                None => return Ok(()),
                Some(_) => {
                    return Err(graph_document(
                        &format!(
                            "graph.nodes.{:032x}.parameters.attributeChannels",
                            node.guid
                        ),
                        "attribute channels must be a GUID list",
                    ));
                }
            };
            let unique = channels.iter().copied().collect::<BTreeSet<_>>();
            if unique.len() != channels.len() || unique.contains(&0) {
                return Err(graph_document(
                    &format!(
                        "graph.nodes.{:032x}.parameters.attributeChannels",
                        node.guid
                    ),
                    "attribute channels must be unique and non-zero",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn parameter_matches_type(value: &GraphParameterValue, expected: GraphParameterType) -> bool {
    matches!(
        (value, expected),
        (GraphParameterValue::Boolean(_), GraphParameterType::Boolean)
            | (GraphParameterValue::U32(_), GraphParameterType::U32)
            | (GraphParameterValue::U64(_), GraphParameterType::U64)
            | (GraphParameterValue::U32Vec3(_), GraphParameterType::U32Vec3)
            | (GraphParameterValue::Guid(_), GraphParameterType::Guid)
            | (GraphParameterValue::Asset(_), GraphParameterType::Asset)
            | (GraphParameterValue::Fixed(_), GraphParameterType::Fixed)
            | (GraphParameterValue::Unit(_), GraphParameterType::Unit)
            | (
                GraphParameterValue::FixedVec3(_),
                GraphParameterType::FixedVec3
            )
            | (
                GraphParameterValue::WorldPosition(_),
                GraphParameterType::WorldPosition
            )
            | (
                GraphParameterValue::FieldChannel(_),
                GraphParameterType::FieldChannel
            )
            | (
                GraphParameterValue::FieldDerivative(_),
                GraphParameterType::FieldDerivative
            )
            | (
                GraphParameterValue::CombineOperation(_),
                GraphParameterType::CombineOperation
            )
            | (
                GraphParameterValue::DistanceSource(_),
                GraphParameterType::DistanceSource
            )
            | (
                GraphParameterValue::ClusterMode(_),
                GraphParameterType::ClusterMode
            )
            | (GraphParameterValue::Curve(_), GraphParameterType::Curve)
            | (GraphParameterValue::String(_), GraphParameterType::String)
            | (
                GraphParameterValue::GuidList(_),
                GraphParameterType::GuidList
            )
            | (GraphParameterValue::TagList(_), GraphParameterType::TagList)
    )
}

fn validate_spatial_contract(node: &GraphNodeDefinition, asset: &BiomeAsset) -> Result<()> {
    if node.operator.spatial_requirement() == GraphSpatialRequirement::Propagating
        && matches!(node.spatial, NodeSpatialPolicy::Partitioned { .. })
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    let NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.spatial
    else {
        return Ok(());
    };
    if influence_radius > asset.policy.maximum_influence_radius {
        return Err(Error::GraphLimit {
            resource: "node influence radius",
            requested: u64::try_from(influence_radius.bits()).unwrap_or(u64::MAX),
            limit: u64::try_from(asset.policy.maximum_influence_radius.bits()).unwrap_or(0),
        });
    }
    let parameter_radius = match node.operator {
        GraphOperator::BlueNoisePoisson | GraphOperator::ClusterPatchColony => {
            required_fixed(node, "radius")?
        }
        GraphOperator::WeightedElimination => required_fixed(node, "eliminationRadius")?,
        GraphOperator::SurfaceProjection => required_fixed(node, "maxDistance")?,
        GraphOperator::DistanceField => required_fixed(node, "maximumDistance")?,
        GraphOperator::SplineFollow => {
            fixed_abs(optional_fixed(node, "edgeOffset")?.unwrap_or(DecisionScalar::from_bits(0)))?
        }
        GraphOperator::RecursiveCompanion => {
            let authored = required_fixed(node, "radius")?;
            let per_generation = asset
                .companions
                .iter()
                .map(|rule| rule.maximum_distance)
                .fold(authored, DecisionScalar::max);
            fixed_mul_u32(per_generation, required_u32(node, "maximumDepth")?)?
        }
        GraphOperator::BoundsOverlap => {
            fixed_abs(optional_fixed(node, "padding")?.unwrap_or(DecisionScalar::from_bits(0)))?
        }
        _ => DecisionScalar::from_bits(0),
    };
    if parameter_radius.bits() < 0 || parameter_radius > influence_radius {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    if matches!(
        node.operator,
        GraphOperator::VariableSpacing
            | GraphOperator::PriorityExclusion
            | GraphOperator::Competition
            | GraphOperator::BoundsOverlap
    ) && influence_radius.bits() <= 0
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    Ok(())
}

fn fixed_abs(value: DecisionScalar) -> Result<DecisionScalar> {
    Ok(DecisionScalar::from_bits(
        value.bits().checked_abs().ok_or(Error::NumericOverflow)?,
    ))
}

fn fixed_mul_u32(value: DecisionScalar, factor: u32) -> Result<DecisionScalar> {
    let bits = i64::from(value.bits())
        .checked_mul(i64::from(factor))
        .ok_or(Error::NumericOverflow)?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn domain_has_candidate_lineage(domain: GraphDomain) -> bool {
    matches!(
        domain,
        GraphDomain::Candidates
            | GraphDomain::ScalarField
            | GraphDomain::VectorField
            | GraphDomain::HessianField
            | GraphDomain::SurfaceField
    )
}

fn resolve_node_lineage(
    node: &GraphNodeDefinition,
    outputs: &[GraphPin],
    inputs: &BTreeMap<String, GraphValueLineage>,
    module: Option<&CompiledGraphUnit>,
    module_path: &[u128],
) -> Result<BTreeMap<String, GraphValueLineage>> {
    use GraphOperator as O;

    let require = |name: &str| {
        inputs.get(name).cloned().ok_or_else(|| {
            graph_document(
                &format!("graph.nodes.{:032x}.{name}", node.guid),
                "candidate lineage is missing",
            )
        })
    };
    let require_same = |left_name: &str, right_name: &str| {
        let left = require(left_name)?;
        let right = require(right_name)?;
        if left != right {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.{right_name}", node.guid),
                "input belongs to a different candidate stream",
            ));
        }
        Ok(left)
    };
    let validate_optional = |base: &GraphValueLineage, name: &str| {
        if let Some(value) = inputs.get(name)
            && value != base
        {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.{name}", node.guid),
                "input belongs to a different candidate stream",
            ));
        }
        Ok(())
    };

    let lineage = match node.operator {
        O::InterfaceInput => Some(GraphValueLineage::InterfaceInput(
            match node.parameter("name") {
                Some(GraphParameterValue::String(name)) => name.clone(),
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.name", node.guid),
                        "interface input requires a name",
                    ));
                }
            },
        )),
        O::ExplicitAnchors
        | O::StratifiedCoverage
        | O::BlueNoisePoisson
        | O::ClusterPatchColony
        | O::SplineFollow
        | O::RecursiveCompanion => Some(GraphValueLineage::CandidateOrigin {
            module_path: module_path.to_vec(),
            node: node.guid,
        }),
        O::SurfaceProjection => Some(require("candidates")?),
        O::FieldSample | O::PaintedTile | O::Noise | O::Gradient | O::DistanceField => {
            Some(require("candidates")?)
        }
        O::Curve | O::Remap | O::Clamp => Some(require("field")?),
        O::Combine => Some(require_same("left", "right")?),
        O::WeightedElimination | O::FieldImportance | O::Suitability => {
            Some(require_same("candidates", "weights")?)
        }
        O::PriorityExclusion => {
            let base = require_same("candidates", "weights")?;
            let radius = require("radius")?;
            if base != radius {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.radius", node.guid),
                    "input belongs to a different candidate stream",
                ));
            }
            Some(base)
        }
        O::VariableSpacing => Some(require_same("candidates", "radius")?),
        O::Competition => Some(require("candidates")?),
        O::Transform => {
            let base = require("candidates")?;
            validate_optional(&base, "surface")?;
            validate_optional(&base, "scale")?;
            validate_optional(&base, "offset")?;
            Some(base)
        }
        O::CommunityBlend => {
            let base = require("candidates")?;
            validate_optional(&base, "shade")?;
            Some(base)
        }
        O::BoundsOverlap | O::SuccessionInput => Some(require("candidates")?),
        O::MicroOutput => {
            let base = require("candidates")?;
            validate_optional(&base, "density")?;
            for (name, lineage) in inputs {
                if name.starts_with("attribute-") && lineage != &base {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.{name}", node.guid),
                        "input belongs to a different candidate stream",
                    ));
                }
            }
            None
        }
        O::DiagnosticOutput => {
            if let (Some(candidates), Some(field)) = (inputs.get("candidates"), inputs.get("field"))
                && candidates != field
            {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.field", node.guid),
                    "input belongs to a different candidate stream",
                ));
            }
            None
        }
        O::ModuleCall => {
            let module = module.ok_or_else(|| {
                graph_document("graph.nodes.module-call", "compiled module is missing")
            })?;
            let mut result = BTreeMap::new();
            for output in outputs {
                if !domain_has_candidate_lineage(output.domain) {
                    continue;
                }
                let lineage = module.output_lineage.get(&output.name).ok_or_else(|| {
                    graph_document(
                        &format!("graph.nodes.{:032x}.{}", node.guid, output.name),
                        "module output candidate lineage is missing",
                    )
                })?;
                let lineage = match lineage {
                    GraphValueLineage::InterfaceInput(name) => {
                        inputs.get(name).cloned().ok_or_else(|| {
                            graph_document(
                                &format!("graph.nodes.{:032x}.{name}", node.guid),
                                "module input candidate lineage is missing",
                            )
                        })?
                    }
                    GraphValueLineage::CandidateOrigin { .. } => lineage.clone(),
                };
                result.insert(output.name.clone(), lineage);
            }
            return Ok(result);
        }
        O::RegionInput | O::SplineInput | O::SpeciesInput | O::CommunityInput | O::MacroOutput => {
            None
        }
    };

    outputs
        .iter()
        .filter(|output| domain_has_candidate_lineage(output.domain))
        .map(|output| {
            Ok((
                output.name.clone(),
                lineage.clone().ok_or_else(|| {
                    graph_document(
                        &format!("graph.nodes.{:032x}.{}", node.guid, output.name),
                        "operator output candidate lineage is missing",
                    )
                })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()
}

type NodeSignature = (Vec<GraphPin>, Vec<GraphPin>);
type NodeSignatures = BTreeMap<u128, NodeSignature>;

fn node_signatures(
    nodes: &[GraphNodeDefinition],
    interface_inputs: &[GraphInterfaceInput],
    modules: &BTreeMap<u128, Box<CompiledGraphUnit>>,
) -> Result<NodeSignatures> {
    let mut result = BTreeMap::new();
    for node in nodes {
        let signature = if node.operator == GraphOperator::ModuleCall {
            let module = modules.get(&node.guid).ok_or_else(|| {
                graph_document("graph.nodes.module-call", "compiled module is missing")
            })?;
            (
                module
                    .inputs
                    .iter()
                    .map(|input| pin(&input.name, input.domain))
                    .collect(),
                module
                    .outputs
                    .iter()
                    .map(|output| pin(&output.name, output.domain))
                    .collect(),
            )
        } else if node.operator == GraphOperator::InterfaceInput {
            let name = match node.parameter("name") {
                Some(GraphParameterValue::String(value)) => value,
                _ => {
                    return Err(graph_document(
                        "graph.nodes.interface-input.name",
                        "interface input requires a name",
                    ));
                }
            };
            let input = interface_inputs
                .iter()
                .find(|input| &input.name == name)
                .ok_or_else(|| {
                    graph_document(
                        "graph.nodes.interface-input.name",
                        "unknown public interface input",
                    )
                })?;
            (Vec::new(), vec![pin("value", input.domain)])
        } else if node.operator == GraphOperator::FieldSample {
            let domain = match node.parameter("derivative") {
                None | Some(GraphParameterValue::FieldDerivative(FieldDerivative::Value)) => {
                    GraphDomain::ScalarField
                }
                Some(GraphParameterValue::FieldDerivative(FieldDerivative::Gradient)) => {
                    GraphDomain::VectorField
                }
                Some(GraphParameterValue::FieldDerivative(FieldDerivative::Hessian)) => {
                    GraphDomain::HessianField
                }
                Some(_) => {
                    return Err(graph_document(
                        "graph.nodes.field-sample.derivative",
                        "field derivative has the wrong type",
                    ));
                }
            };
            (node.operator.input_pins(), vec![pin("field", domain)])
        } else if node.operator == GraphOperator::MicroOutput {
            let mut inputs = node.operator.input_pins();
            let channels = match node.parameter("attributeChannels") {
                None => Vec::new(),
                Some(GraphParameterValue::GuidList(channels)) => channels.clone(),
                Some(_) => {
                    return Err(graph_document(
                        "graph.nodes.micro-output.attributeChannels",
                        "attribute channels have the wrong type",
                    ));
                }
            };
            inputs.extend(
                channels
                    .into_iter()
                    .map(|channel| pin(micro_attribute_pin(channel), GraphDomain::ScalarField)),
            );
            (inputs, node.operator.output_pins())
        } else {
            (node.operator.input_pins(), node.operator.output_pins())
        };
        result.insert(node.guid, signature);
    }
    Ok(result)
}

fn validate_edges(
    document: &BiomeGraphDocument,
    signatures: &BTreeMap<u128, (Vec<GraphPin>, Vec<GraphPin>)>,
) -> Result<()> {
    let mut destinations = BTreeSet::new();
    for edge in &document.edges {
        let source = signatures
            .get(&edge.from_node)
            .and_then(|(_, outputs)| outputs.iter().find(|pin| pin.name == edge.from_pin))
            .ok_or_else(|| graph_document("graph.edges.from", "source pin does not exist"))?;
        let destination = signatures
            .get(&edge.to_node)
            .and_then(|(inputs, _)| inputs.iter().find(|pin| pin.name == edge.to_pin))
            .ok_or_else(|| graph_document("graph.edges.to", "destination pin does not exist"))?;
        if source.domain != destination.domain {
            return Err(Error::GraphTypeMismatch {
                from_node: edge.from_node,
                from_pin: edge.from_pin.clone(),
                from_domain: source.domain.as_wire(),
                to_node: edge.to_node,
                to_pin: edge.to_pin.clone(),
                to_domain: destination.domain.as_wire(),
            });
        }
        if !destinations.insert((edge.to_node, edge.to_pin.clone())) {
            return Err(graph_document(
                "graph.edges.to",
                "an input pin has more than one producer",
            ));
        }
    }
    for (guid, (inputs, _)) in signatures {
        for input in inputs.iter().filter(|input| input.required) {
            if !destinations.contains(&(*guid, input.name.clone())) {
                return Err(graph_document(
                    &format!("graph.nodes.{guid:032x}.{}", input.name),
                    "required input is not connected",
                ));
            }
        }
    }
    for output in &document.outputs {
        let pin = signatures
            .get(&output.node)
            .and_then(|(_, outputs)| outputs.iter().find(|pin| pin.name == output.pin))
            .ok_or_else(|| graph_document("graph.outputs", "source pin does not exist"))?;
        if pin.domain != output.domain {
            return Err(graph_document(
                "graph.outputs.domain",
                "output domain does not match pin",
            ));
        }
    }
    Ok(())
}

fn topological_order(nodes: &[GraphNodeDefinition], edges: &[GraphEdge]) -> Result<Vec<u128>> {
    let mut incoming = nodes
        .iter()
        .map(|node| (node.guid, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing: BTreeMap<u128, BTreeSet<u128>> = BTreeMap::new();
    for edge in edges {
        if !incoming.contains_key(&edge.from_node) {
            return Err(graph_document("graph.edges", "unknown source node"));
        }
        let inserted = outgoing
            .entry(edge.from_node)
            .or_default()
            .insert(edge.to_node);
        if inserted {
            *incoming
                .get_mut(&edge.to_node)
                .ok_or_else(|| graph_document("graph.edges", "unknown destination node"))? += 1;
        }
    }
    let mut ready: BTreeSet<_> = incoming
        .iter()
        .filter_map(|(guid, count)| (*count == 0).then_some(*guid))
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(guid) = ready.pop_first() {
        order.push(guid);
        for destination in outgoing.get(&guid).into_iter().flatten() {
            let count = incoming.get_mut(destination).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.insert(*destination);
            }
        }
    }
    if order.len() != nodes.len() {
        let node = incoming
            .into_iter()
            .find_map(|(guid, count)| (count != 0).then_some(guid))
            .unwrap_or(0);
        return Err(Error::GraphCycle { node });
    }
    Ok(order)
}

fn incoming_edges(edges: &[GraphEdge]) -> BTreeMap<u128, Vec<&GraphEdge>> {
    let mut result: BTreeMap<u128, Vec<_>> = BTreeMap::new();
    for edge in edges {
        result.entry(edge.to_node).or_default().push(edge);
    }
    for values in result.values_mut() {
        values.sort_by(|left, right| left.to_pin.cmp(&right.to_pin));
    }
    result
}

fn compile_output_halo(
    nodes: &[CompiledGraphNode],
    edges: &[GraphEdge],
    outputs: &[GraphInterfaceOutput],
) -> Result<BTreeMap<String, [DecisionScalar; 63]>> {
    let incoming = incoming_edges(edges);
    let zero = DecisionScalar::from_bits(0);
    let mut result = outputs
        .iter()
        .map(|output| (output.name.clone(), [zero; 63]))
        .collect::<BTreeMap<_, _>>();
    for output_level in 0_u8..=62 {
        let mut support_by_pin = BTreeMap::<(u128, String), DecisionScalar>::new();
        for node in nodes {
            let upstream = incoming
                .get(&node.definition.guid)
                .into_iter()
                .flatten()
                .filter_map(|edge| {
                    support_by_pin
                        .get(&(edge.from_node, edge.from_pin.clone()))
                        .copied()
                })
                .max()
                .unwrap_or(zero);
            let local = match node.definition.spatial {
                NodeSpatialPolicy::Partitioned {
                    level,
                    influence_radius,
                } if level >= output_level => influence_radius,
                _ => zero,
            };
            for output in &node.outputs {
                let module = node
                    .module
                    .as_deref()
                    .and_then(|unit| unit.output_halo_by_level.get(&output.name))
                    .map_or(zero, |support| support[usize::from(output_level)]);
                let support = upstream.checked_add(local)?.checked_add(module)?;
                support_by_pin.insert((node.definition.guid, output.name.clone()), support);
            }
        }
        for output in outputs {
            let support = support_by_pin
                .get(&(output.node, output.pin.clone()))
                .copied()
                .ok_or_else(|| graph_document("graph.outputs", "output support is missing"))?;
            result.get_mut(&output.name).ok_or_else(|| {
                graph_document("graph.outputs", "output support table is missing")
            })?[usize::from(output_level)] = support;
        }
    }
    Ok(result)
}

fn demanded_output_halo(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    outputs: &[QualifiedGraphPin],
) -> Result<[DecisionScalar; 63]> {
    let mut result = [DecisionScalar::from_bits(0); 63];
    for output_level in 0_u8..=62 {
        let mut memo = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        for pin in outputs {
            result[usize::from(output_level)] = result[usize::from(output_level)].max(
                demanded_pin_halo(root, demand, pin, output_level, &mut memo, &mut visiting)?,
            );
        }
    }
    Ok(result)
}

fn enforce_demanded_halo_policies(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<()> {
    for (module_path, unit_demand) in &demand.units {
        let unit = compiled_unit_at_path(root, module_path)?;
        for output in unit
            .outputs
            .iter()
            .filter(|output| unit_demand.outputs.contains(&output.name))
        {
            let source = unit
                .nodes
                .iter()
                .find(|node| node.definition.guid == output.node)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "unit output source is missing")
                })?;
            let pin = QualifiedGraphPin {
                node: source.address(),
                pin: output.pin.clone(),
            };
            for output_level in 0_u8..=62 {
                let support = demanded_pin_halo(
                    root,
                    demand,
                    &pin,
                    output_level,
                    &mut BTreeMap::new(),
                    &mut BTreeSet::new(),
                )?;
                if support > unit.maximum_influence_radius {
                    return Err(Error::GraphLimit {
                        resource: "composed influence radius",
                        requested: u64::try_from(support.bits()).unwrap_or(u64::MAX),
                        limit: u64::try_from(unit.maximum_influence_radius.bits()).unwrap_or(0),
                    });
                }
            }
        }
    }
    Ok(())
}

fn demanded_pin_halo(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    pin: &QualifiedGraphPin,
    output_level: u8,
    memo: &mut BTreeMap<QualifiedGraphPin, DecisionScalar>,
    visiting: &mut BTreeSet<QualifiedGraphPin>,
) -> Result<DecisionScalar> {
    if let Some(support) = memo.get(pin).copied() {
        return Ok(support);
    }
    if !visiting.insert(pin.clone()) {
        return Err(Error::GraphCycle {
            node: pin.node.node,
        });
    }
    let unit = compiled_unit_at_path(root, &pin.node.module_path)?;
    let node = unit
        .nodes
        .iter()
        .find(|node| node.definition.guid == pin.node.node)
        .ok_or_else(|| graph_document("graph.demand.halo", "live output node is missing"))?;
    let local = match node.definition.spatial {
        NodeSpatialPolicy::Partitioned {
            level,
            influence_radius,
        } if level >= output_level => influence_radius,
        _ => DecisionScalar::from_bits(0),
    };
    let upstream = match node.definition.operator {
        GraphOperator::InterfaceInput => {
            let Some(GraphParameterValue::String(name)) = node.definition.parameter("name") else {
                return Err(graph_document(
                    "graph.demand.halo",
                    "interface input name is missing",
                ));
            };
            let module_path = pin.node.module_path.as_slice();
            let parent_path = &module_path[..module_path.len() - 1];
            let parent = compiled_unit_at_path(root, parent_path)?;
            let call = module_call_by_guid(parent, module_path[module_path.len() - 1])?;
            let edge = parent
                .edges
                .iter()
                .find(|edge| edge.to_node == call.definition.guid && edge.to_pin.as_str() == name)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "live module input edge is missing")
                })?;
            let source = parent
                .nodes
                .iter()
                .find(|source| source.definition.guid == edge.from_node)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "live module input source is missing")
                })?;
            demanded_pin_halo(
                root,
                demand,
                &QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                output_level,
                memo,
                visiting,
            )?
        }
        GraphOperator::ModuleCall => {
            let module = node
                .module
                .as_deref()
                .ok_or_else(|| graph_document("graph.demand.halo", "compiled module is missing"))?;
            let output = module
                .outputs
                .iter()
                .find(|output| output.name == pin.pin)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "live module output is missing")
                })?;
            let source = module
                .nodes
                .iter()
                .find(|source| source.definition.guid == output.node)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "live module output source is missing")
                })?;
            demanded_pin_halo(
                root,
                demand,
                &QualifiedGraphPin {
                    node: source.address(),
                    pin: output.pin.clone(),
                },
                output_level,
                memo,
                visiting,
            )?
        }
        _ => {
            let unit_demand = demand
                .unit(&pin.node.module_path)
                .ok_or_else(|| graph_document("graph.demand.halo", "live unit slice is missing"))?;
            let mut support = DecisionScalar::from_bits(0);
            for edge in unit.edges.iter().filter(|edge| {
                edge.to_node == node.definition.guid
                    && unit_demand.contains_edge(edge)
                    && node
                        .definition
                        .operator
                        .output_requires_input(&pin.pin, &edge.to_pin)
            }) {
                let source = unit
                    .nodes
                    .iter()
                    .find(|source| source.definition.guid == edge.from_node)
                    .ok_or_else(|| {
                        graph_document("graph.demand.halo", "live edge source node is missing")
                    })?;
                support = support.max(demanded_pin_halo(
                    root,
                    demand,
                    &QualifiedGraphPin {
                        node: source.address(),
                        pin: edge.from_pin.clone(),
                    },
                    output_level,
                    memo,
                    visiting,
                )?);
            }
            support
        }
    };
    let support = upstream.checked_add(local)?;
    visiting.remove(pin);
    memo.insert(pin.clone(), support);
    Ok(support)
}

fn estimate_node(node: &GraphNodeDefinition, upstream: GraphEstimate) -> Result<GraphEstimate> {
    let mut estimate = upstream;
    match node.operator {
        GraphOperator::StratifiedCoverage | GraphOperator::BlueNoisePoisson => {
            estimate.candidates = u64::from(required_u32(node, "count")?);
        }
        GraphOperator::WeightedElimination => {
            estimate.candidates = estimate
                .candidates
                .min(u64::from(required_u32(node, "targetCount")?));
            let adjacency = upstream
                .candidates
                .checked_mul(u64::from(required_u32(node, "maximumNeighbours")?))
                .and_then(|value| value.checked_mul(16))
                .ok_or(Error::NumericOverflow)?;
            estimate.memory_bytes = estimate
                .memory_bytes
                .checked_add(adjacency)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::ClusterPatchColony => {
            estimate.candidates = estimate
                .candidates
                .checked_mul(u64::from(required_u32(node, "children")?) + 1)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::RecursiveCompanion => {
            let children = u64::from(required_u32(node, "children")?);
            let depth = u64::from(required_u32(node, "maximumDepth")?);
            let mut generation = 1_u64;
            let mut factor = 1_u64;
            for _ in 0..depth {
                generation = generation
                    .checked_mul(children)
                    .ok_or(Error::NumericOverflow)?;
                factor = factor
                    .checked_add(generation)
                    .ok_or(Error::NumericOverflow)?;
            }
            estimate.candidates = estimate
                .candidates
                .checked_mul(factor)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::MacroOutput => estimate.accepted = estimate.candidates,
        GraphOperator::MicroOutput => {
            let dimensions = required_u32_vec3(node, "dimensions")?;
            let samples = dimensions.iter().try_fold(1_u64, |product, value| {
                product
                    .checked_mul(u64::from(*value))
                    .ok_or(Error::NumericOverflow)
            })?;
            estimate.micro_samples = samples;
            let attribute_channels = match node.parameter("attributeChannels") {
                Some(GraphParameterValue::GuidList(channels)) => channels.len() as u64,
                _ => 0,
            };
            let bytes_per_sample = attribute_channels
                .checked_mul(20)
                .and_then(|value| value.checked_add(10))
                .ok_or(Error::NumericOverflow)?;
            estimate.memory_bytes = estimate
                .memory_bytes
                .checked_add(
                    samples
                        .checked_mul(bytes_per_sample)
                        .ok_or(Error::NumericOverflow)?,
                )
                .ok_or(Error::NumericOverflow)?;
        }
        _ => {}
    }
    let candidate_bytes = estimate
        .candidates
        .checked_mul(128)
        .ok_or(Error::NumericOverflow)?;
    estimate.memory_bytes = estimate.memory_bytes.max(candidate_bytes);
    if node.authority != GraphAuthority::Authoritative && node.operator.has_slang_executor() {
        estimate.transfer_bytes = estimate.transfer_bytes.max(candidate_bytes);
    }
    Ok(estimate)
}

fn enforce_limits(estimate: GraphEstimate, limits: GraphSafetyLimits) -> Result<()> {
    for (resource, requested, limit) in [
        (
            "candidate count",
            estimate.candidates,
            limits.max_candidates,
        ),
        ("accepted count", estimate.accepted, limits.max_macro_points),
        (
            "micro samples",
            estimate.micro_samples,
            limits.max_micro_samples,
        ),
        (
            "memory bytes",
            estimate.memory_bytes,
            limits.max_memory_bytes,
        ),
        (
            "transfer bytes",
            estimate.transfer_bytes,
            limits.max_transfer_bytes,
        ),
    ] {
        if requested > limit {
            return Err(Error::GraphLimit {
                resource,
                requested,
                limit,
            });
        }
    }
    Ok(())
}

fn max_estimate(left: GraphEstimate, right: GraphEstimate) -> GraphEstimate {
    GraphEstimate {
        candidates: left.candidates.max(right.candidates),
        accepted: left.accepted.max(right.accepted),
        micro_samples: left.micro_samples.max(right.micro_samples),
        memory_bytes: left.memory_bytes.max(right.memory_bytes),
        transfer_bytes: left.transfer_bytes.max(right.transfer_bytes),
    }
}

fn demanded_estimate(demand: &CompiledDemandSlice) -> GraphEstimate {
    demand
        .estimates
        .values()
        .copied()
        .fold(GraphEstimate::default(), max_estimate)
}

fn apply_demanded_estimates(
    unit: &mut CompiledGraphUnit,
    module_path: &[u128],
    demand: &CompiledDemandSlice,
) -> Result<()> {
    for node in &mut unit.nodes {
        let address = node.address();
        if demand.contains_node(&address) {
            let mut estimate = GraphEstimate::default();
            for output in &node.outputs {
                let pin = QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                };
                if let Some(output_estimate) = demand.estimate(&pin) {
                    node.output_estimates
                        .insert(output.name.clone(), output_estimate);
                    estimate = max_estimate(estimate, output_estimate);
                }
            }
            node.estimate = estimate;
        }
        if node.module.is_some() {
            let call_guid = module_call_guid(node)?;
            let mut child_path = module_path.to_vec();
            child_path.push(call_guid);
            let module = node.module.as_deref_mut().ok_or_else(|| {
                graph_document("graph.demand.estimate", "compiled module is missing")
            })?;
            apply_demanded_estimates(module, &child_path, demand)?;
        }
    }
    for output in &unit.outputs {
        let source = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == output.node)
            .ok_or_else(|| {
                graph_document(
                    "graph.demand.estimate",
                    "unit output source node is missing",
                )
            })?;
        let pin = QualifiedGraphPin {
            node: source.address(),
            pin: output.pin.clone(),
        };
        if let Some(estimate) = demand.estimate(&pin) {
            unit.output_estimates.insert(output.name.clone(), estimate);
        }
    }
    unit.estimate = demand
        .estimates
        .iter()
        .filter(|(pin, _)| pin.node.module_path.starts_with(module_path))
        .map(|(_, estimate)| *estimate)
        .fold(GraphEstimate::default(), max_estimate);
    unit.dependencies = demand
        .nodes
        .iter()
        .filter(|address| address.module_path.starts_with(module_path))
        .flat_map(|address| {
            compiled_unit_at_path(unit, &address.module_path[module_path.len()..])
                .ok()
                .and_then(|owner| {
                    owner
                        .nodes
                        .iter()
                        .find(|node| node.definition.guid == address.node)
                })
                .into_iter()
                .flat_map(live_node_dependencies)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(())
}

fn merge_input_estimate(left: GraphEstimate, right: GraphEstimate) -> Result<GraphEstimate> {
    Ok(GraphEstimate {
        candidates: left.candidates.max(right.candidates),
        accepted: left.accepted.max(right.accepted),
        micro_samples: left.micro_samples.max(right.micro_samples),
        memory_bytes: left
            .memory_bytes
            .checked_add(right.memory_bytes)
            .ok_or(Error::NumericOverflow)?,
        transfer_bytes: left
            .transfer_bytes
            .checked_add(right.transfer_bytes)
            .ok_or(Error::NumericOverflow)?,
    })
}

fn validate_interface(document: &BiomeGraphDocument, role: BiomeRole) -> Result<()> {
    let input_names: BTreeSet<_> = document
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect();
    let output_names: BTreeSet<_> = document
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect();
    let pin_ids = document
        .inputs
        .iter()
        .map(|input| input.id)
        .chain(document.outputs.iter().map(|output| output.id))
        .collect::<BTreeSet<_>>();
    if input_names.len() != document.inputs.len()
        || output_names.len() != document.outputs.len()
        || pin_ids.len() != document.inputs.len() + document.outputs.len()
        || pin_ids.contains(&0)
        || input_names.contains("")
        || output_names.contains("")
        || document.outputs.iter().any(|output| output.pin.is_empty())
    {
        return Err(graph_document(
            "graph.interface",
            "interface pin IDs and names must be non-zero, non-empty, and unique",
        ));
    }
    let interface_nodes = document
        .nodes
        .iter()
        .filter(|node| node.operator == GraphOperator::InterfaceInput)
        .collect::<Vec<_>>();
    if role == BiomeRole::Root {
        if !document.inputs.is_empty() || !interface_nodes.is_empty() {
            return Err(graph_document(
                "graph.inputs",
                "root graphs cannot declare module interface inputs",
            ));
        }
        if document.outputs.iter().any(|output| output.sink.is_none()) {
            return Err(graph_document(
                "graph.outputs.sink",
                "root outputs require an authority sink",
            ));
        }
    }
    if role == BiomeRole::Module {
        if document.outputs.iter().any(|output| output.sink.is_some()) {
            return Err(graph_document(
                "graph.outputs.sink",
                "module outputs cannot declare root sinks",
            ));
        }
        let node_inputs = interface_nodes
            .iter()
            .map(|node| match node.parameter("name") {
                Some(GraphParameterValue::String(name)) if !name.is_empty() => Ok(name.as_str()),
                _ => Err(graph_document(
                    &format!("graph.nodes.{:032x}.name", node.guid),
                    "interface input requires a non-empty name",
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        let unique_node_inputs = node_inputs.iter().copied().collect::<BTreeSet<_>>();
        if node_inputs.len() != document.inputs.len() || unique_node_inputs != input_names {
            return Err(graph_document(
                "graph.inputs",
                "every module input requires exactly one matching interface-input node",
            ));
        }
    }
    Ok(())
}

fn validate_parameter_bindings(asset: &BiomeAsset, bindings: &[(u128, Value)]) -> Result<()> {
    let parameters: BTreeSet<_> = asset
        .parameters
        .iter()
        .map(|parameter| parameter.id)
        .collect();
    let mut seen = BTreeSet::new();
    for (parameter, _value) in bindings {
        if !parameters.contains(parameter) || !seen.insert(*parameter) {
            return Err(graph_document(
                "biome.modules.bindings",
                "binding target is unknown or duplicated",
            ));
        }
    }
    Ok(())
}

fn resolve_parameter_bindings(
    document: &mut BiomeGraphDocument,
    asset: &BiomeAsset,
    bindings: &[(u128, Value)],
) -> Result<()> {
    validate_parameter_bindings(asset, bindings)?;
    let parameters = asset
        .parameters
        .iter()
        .map(|parameter| (parameter.id, parameter))
        .collect::<BTreeMap<_, _>>();
    let supplied = bindings.iter().cloned().collect::<BTreeMap<_, _>>();
    for node in &mut document.nodes {
        let schema = node
            .operator
            .parameter_schema()
            .into_iter()
            .map(|parameter| (parameter.name, parameter.parameter_type))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in &mut node.parameters {
            let GraphParameterValue::Binding(parameter_id) = value else {
                continue;
            };
            let parameter = parameters.get(parameter_id).ok_or_else(|| {
                graph_document(
                    &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                    "binding references an unknown biome parameter",
                )
            })?;
            let expected = schema[name.as_str()];
            if !biome_parameter_matches_graph_type(parameter.parameter_type, expected) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                    "biome parameter type is incompatible with the node parameter",
                ));
            }
            let raw = supplied
                .get(parameter_id)
                .unwrap_or(&parameter.default_value);
            *value = parse_parameter_value(
                raw,
                expected,
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            )?;
        }
    }
    Ok(())
}

fn biome_parameter_matches_graph_type(
    parameter: BiomeParameterType,
    graph: GraphParameterType,
) -> bool {
    matches!(
        (parameter, graph),
        (BiomeParameterType::Scalar, GraphParameterType::Fixed)
            | (BiomeParameterType::Vector, GraphParameterType::FixedVec3)
            | (BiomeParameterType::Unit, GraphParameterType::Unit)
            | (BiomeParameterType::Plant, GraphParameterType::Asset)
            | (BiomeParameterType::Field, GraphParameterType::FieldChannel)
            | (BiomeParameterType::Boolean, GraphParameterType::Boolean)
    )
}

fn parse_interface_input(value: &Value, index: usize) -> Result<GraphInterfaceInput> {
    let path = format!("graph.inputs[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(object, &["id", "name", "domain"], &path)?;
    Ok(GraphInterfaceInput {
        id: read_guid(object.get("id"), &format!("{path}.id"))?,
        name: read_string(object.get("name"), &format!("{path}.name"))?,
        domain: read_domain(object.get("domain"), &format!("{path}.domain"))?,
    })
}

fn parse_interface_output(value: &Value, index: usize) -> Result<GraphInterfaceOutput> {
    let path = format!("graph.outputs[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(
        object,
        &["id", "name", "domain", "node", "pin", "sink"],
        &path,
    )?;
    let sink = object
        .get("sink")
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| graph_document(&format!("{path}.sink"), "expected string"))?;
            GraphSink::from_wire(text)
                .ok_or_else(|| graph_document(&format!("{path}.sink"), "unknown sink"))
        })
        .transpose()?;
    Ok(GraphInterfaceOutput {
        id: read_guid(object.get("id"), &format!("{path}.id"))?,
        name: read_string(object.get("name"), &format!("{path}.name"))?,
        domain: read_domain(object.get("domain"), &format!("{path}.domain"))?,
        node: read_guid(object.get("node"), &format!("{path}.node"))?,
        pin: read_string(object.get("pin"), &format!("{path}.pin"))?,
        sink,
    })
}

fn parse_node(value: &Value, index: usize) -> Result<GraphNodeDefinition> {
    let path = format!("graph.nodes[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(
        object,
        &[
            "guid",
            "version",
            "semanticRevision",
            "operator",
            "authority",
            "spatial",
            "dependencies",
            "seedNamespaces",
            "parameters",
        ],
        &path,
    )?;
    let operator_text = read_string(object.get("operator"), &format!("{path}.operator"))?;
    let operator = GraphOperator::from_wire(&operator_text)
        .ok_or_else(|| graph_document(&format!("{path}.operator"), "unknown operator"))?;
    let version = read_u32(object.get("version"), &format!("{path}.version"))?;
    if version != BIOME_NODE_VERSION {
        return Err(Error::FormatVersion {
            format: ".sbiome node",
            found: version,
            expected: BIOME_NODE_VERSION,
        });
    }
    let authority_text = read_string(object.get("authority"), &format!("{path}.authority"))?;
    let authority = GraphAuthority::from_wire(&authority_text)
        .ok_or_else(|| graph_document(&format!("{path}.authority"), "unknown authority"))?;
    let spatial = parse_spatial(object.get("spatial"), &format!("{path}.spatial"))?;
    let dependencies = object
        .get("dependencies")
        .map(|value| parse_dependencies(value, &format!("{path}.dependencies")))
        .transpose()?
        .unwrap_or_default();
    let seed_namespaces = object
        .get("seedNamespaces")
        .map(|value| parse_seed_namespaces(value, &format!("{path}.seedNamespaces")))
        .transpose()?
        .unwrap_or_default();
    let parameters = parse_parameters(
        object.get("parameters"),
        &format!("{path}.parameters"),
        operator,
    )?;
    Ok(GraphNodeDefinition {
        guid: read_guid(object.get("guid"), &format!("{path}.guid"))?,
        version,
        semantic_revision: read_u32(
            object.get("semanticRevision"),
            &format!("{path}.semanticRevision"),
        )?,
        operator,
        authority,
        spatial,
        dependencies,
        seed_namespaces,
        parameters,
    })
}

fn parse_edge(value: &Value, index: usize) -> Result<GraphEdge> {
    let path = format!("graph.edges[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(object, &["fromNode", "fromPin", "toNode", "toPin"], &path)?;
    Ok(GraphEdge {
        from_node: read_guid(object.get("fromNode"), &format!("{path}.fromNode"))?,
        from_pin: read_string(object.get("fromPin"), &format!("{path}.fromPin"))?,
        to_node: read_guid(object.get("toNode"), &format!("{path}.toNode"))?,
        to_pin: read_string(object.get("toPin"), &format!("{path}.toPin"))?,
    })
}

fn parse_spatial(value: Option<&Value>, path: &str) -> Result<NodeSpatialPolicy> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| graph_document(path, "expected object"))?;
    reject_unknown(object, &["scope", "level", "influenceRadiusBits"], path)?;
    let scope = read_string(object.get("scope"), &format!("{path}.scope"))?;
    let level = read_u8(object.get("level"), &format!("{path}.level"))?;
    match scope.as_str() {
        "partitioned" => {
            let radius = read_i32(
                object.get("influenceRadiusBits"),
                &format!("{path}.influenceRadiusBits"),
            )?;
            if radius < 0 {
                return Err(graph_document(path, "influence radius cannot be negative"));
            }
            Ok(NodeSpatialPolicy::Partitioned {
                level,
                influence_radius: DecisionScalar::from_bits(radius),
            })
        }
        "global" => Ok(NodeSpatialPolicy::Global { level }),
        _ => Err(graph_document(&format!("{path}.scope"), "unknown scope")),
    }
}

fn parse_dependencies(value: &Value, path: &str) -> Result<Vec<GraphDependencySource>> {
    read_array(Some(value), path)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let item_path = format!("{path}[{index}]");
            let object = value
                .as_object()
                .ok_or_else(|| graph_document(&item_path, "expected object"))?;
            reject_unknown(object, &["kind", "value"], &item_path)?;
            let kind = read_string(object.get("kind"), &format!("{item_path}.kind"))?;
            let value = object.get("value");
            match kind.as_str() {
                "asset" => Ok(GraphDependencySource::Asset(Uuid(read_u64(
                    value,
                    &format!("{item_path}.value"),
                )?))),
                "field" => Ok(GraphDependencySource::Field(read_field_channel(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                "surface-provider" => Ok(GraphDependencySource::SurfaceProvider(read_u64(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                "map-layer" => Ok(GraphDependencySource::MapLayer(read_guid(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                _ => Err(graph_document(
                    &format!("{item_path}.kind"),
                    "unknown dependency",
                )),
            }
        })
        .collect()
}

fn parse_parameters(
    value: Option<&Value>,
    path: &str,
    operator: GraphOperator,
) -> Result<BTreeMap<String, GraphParameterValue>> {
    let empty = Map::new();
    let object = match value {
        Some(value) => value
            .as_object()
            .ok_or_else(|| graph_document(path, "expected object"))?,
        None => &empty,
    };
    let schema = operator.parameter_schema();
    let by_name: BTreeMap<_, _> = schema
        .iter()
        .map(|item| (item.name, item.parameter_type))
        .collect();
    let mut parameters = BTreeMap::new();
    for (name, value) in object {
        let parameter_type = by_name
            .get(name.as_str())
            .copied()
            .ok_or_else(|| graph_document(&format!("{path}.{name}"), "unknown parameter"))?;
        parameters.insert(
            name.clone(),
            parse_parameter_value(value, parameter_type, &format!("{path}.{name}"))?,
        );
    }
    Ok(parameters)
}

fn parse_parameter_value(
    value: &Value,
    parameter_type: GraphParameterType,
    path: &str,
) -> Result<GraphParameterValue> {
    if let Some(binding) = value.as_object()
        && binding.len() == 1
        && binding.contains_key("$binding")
    {
        return Ok(GraphParameterValue::Binding(read_guid(
            binding.get("$binding"),
            &format!("{path}.$binding"),
        )?));
    }
    Ok(match parameter_type {
        GraphParameterType::Boolean => GraphParameterValue::Boolean(
            value
                .as_bool()
                .ok_or_else(|| graph_document(path, "expected boolean"))?,
        ),
        GraphParameterType::U32 => GraphParameterValue::U32(read_u32(Some(value), path)?),
        GraphParameterType::U64 => GraphParameterValue::U64(read_u64(Some(value), path)?),
        GraphParameterType::U32Vec3 => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(path, "expected three unsigned lanes"));
            }
            GraphParameterValue::U32Vec3([
                read_u32(Some(&values[0]), path)?,
                read_u32(Some(&values[1]), path)?,
                read_u32(Some(&values[2]), path)?,
            ])
        }
        GraphParameterType::Guid => GraphParameterValue::Guid(read_guid(Some(value), path)?),
        GraphParameterType::Asset => GraphParameterValue::Asset(Uuid(read_u64(Some(value), path)?)),
        GraphParameterType::Fixed => {
            GraphParameterValue::Fixed(DecisionScalar::from_bits(read_i32(Some(value), path)?))
        }
        GraphParameterType::Unit => {
            GraphParameterValue::Unit(UnitInterval::from_bits(read_u16(Some(value), path)?))
        }
        GraphParameterType::FixedVec3 => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(path, "expected three fixed-point lanes"));
            }
            GraphParameterValue::FixedVec3([
                DecisionScalar::from_bits(read_i32(Some(&values[0]), path)?),
                DecisionScalar::from_bits(read_i32(Some(&values[1]), path)?),
                DecisionScalar::from_bits(read_i32(Some(&values[2]), path)?),
            ])
        }
        GraphParameterType::WorldPosition => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(
                    path,
                    "expected three exact world-tick lanes",
                ));
            }
            let parse = |index: usize| {
                values[index]
                    .as_str()
                    .ok_or_else(|| graph_document(path, "expected world ticks as decimal strings"))?
                    .parse::<i128>()
                    .map_err(|_| graph_document(path, "world tick is outside signed 128-bit range"))
            };
            GraphParameterValue::WorldPosition([parse(0)?, parse(1)?, parse(2)?])
        }
        GraphParameterType::FieldChannel => {
            GraphParameterValue::FieldChannel(read_field_channel(Some(value), path)?)
        }
        GraphParameterType::FieldDerivative => {
            let value = value
                .as_str()
                .and_then(field_derivative_from_wire)
                .ok_or_else(|| graph_document(path, "unknown field derivative"))?;
            GraphParameterValue::FieldDerivative(value)
        }
        GraphParameterType::CombineOperation => {
            let value = value
                .as_str()
                .and_then(GraphCombineOperation::from_wire)
                .ok_or_else(|| graph_document(path, "unknown combine operation"))?;
            GraphParameterValue::CombineOperation(value)
        }
        GraphParameterType::DistanceSource => {
            let value = value
                .as_str()
                .and_then(GraphDistanceSource::from_wire)
                .ok_or_else(|| graph_document(path, "unknown distance source"))?;
            GraphParameterValue::DistanceSource(value)
        }
        GraphParameterType::ClusterMode => {
            let value = value
                .as_str()
                .and_then(GraphClusterMode::from_wire)
                .ok_or_else(|| graph_document(path, "unknown cluster mode"))?;
            GraphParameterValue::ClusterMode(value)
        }
        GraphParameterType::Curve => {
            let points = read_array(Some(value), path)?
                .iter()
                .enumerate()
                .map(|(index, point)| {
                    let values = point.as_array().ok_or_else(|| {
                        graph_document(&format!("{path}[{index}]"), "expected [unit, fixed]")
                    })?;
                    if values.len() != 2 {
                        return Err(graph_document(
                            &format!("{path}[{index}]"),
                            "expected [unit, fixed]",
                        ));
                    }
                    Ok((
                        UnitInterval::from_bits(read_u16(Some(&values[0]), path)?),
                        DecisionScalar::from_bits(read_i32(Some(&values[1]), path)?),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            saffron_spatial::DecisionCurve::new(points.clone())?;
            GraphParameterValue::Curve(points)
        }
        GraphParameterType::String => GraphParameterValue::String(
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| graph_document(path, "expected string"))?,
        ),
        GraphParameterType::GuidList => {
            GraphParameterValue::GuidList(parse_guid_array(value, path)?)
        }
        GraphParameterType::TagList => GraphParameterValue::TagList(
            read_array(Some(value), path)?
                .iter()
                .map(|value| read_u64(Some(value), path))
                .collect::<Result<Vec<_>>>()?,
        ),
    })
}

fn node_to_json(node: &GraphNodeDefinition) -> Value {
    let dependencies = node
        .dependencies
        .iter()
        .map(|dependency| match dependency {
            GraphDependencySource::Asset(id) => dependency_json("asset", id.value().to_string()),
            GraphDependencySource::Field(channel) => {
                dependency_json("field", field_channel_wire(*channel))
            }
            GraphDependencySource::SurfaceProvider(id) => {
                dependency_json("surface-provider", id.to_string())
            }
            GraphDependencySource::MapLayer(id) => dependency_json("map-layer", guid_text(*id)),
        })
        .collect();
    let spatial = match node.spatial {
        NodeSpatialPolicy::Partitioned {
            level,
            influence_radius,
        } => object([
            ("scope", Value::String("partitioned".to_owned())),
            ("level", Value::from(level)),
            ("influenceRadiusBits", Value::from(influence_radius.bits())),
        ]),
        NodeSpatialPolicy::Global { level } => object([
            ("scope", Value::String("global".to_owned())),
            ("level", Value::from(level)),
        ]),
    };
    let parameters = node
        .parameters
        .iter()
        .map(|(name, value)| (name.clone(), parameter_to_json(value)))
        .collect();
    object([
        ("guid", Value::String(guid_text(node.guid))),
        ("version", Value::from(node.version)),
        ("semanticRevision", Value::from(node.semantic_revision)),
        (
            "operator",
            Value::String(node.operator.as_wire().to_owned()),
        ),
        (
            "authority",
            Value::String(node.authority.as_wire().to_owned()),
        ),
        ("spatial", spatial),
        ("dependencies", Value::Array(dependencies)),
        (
            "seedNamespaces",
            Value::Object(
                node.seed_namespaces
                    .iter()
                    .map(|(name, value)| (name.clone(), Value::String(guid_text(*value))))
                    .collect(),
            ),
        ),
        ("parameters", Value::Object(parameters)),
    ])
}

fn parameter_to_json(value: &GraphParameterValue) -> Value {
    match value {
        GraphParameterValue::Binding(parameter) => {
            object([("$binding", Value::String(guid_text(*parameter)))])
        }
        GraphParameterValue::Boolean(value) => Value::Bool(*value),
        GraphParameterValue::U32(value) => Value::from(*value),
        GraphParameterValue::U64(value) => Value::String(value.to_string()),
        GraphParameterValue::U32Vec3(value) => {
            Value::Array(value.iter().map(|value| Value::from(*value)).collect())
        }
        GraphParameterValue::Guid(value) => Value::String(guid_text(*value)),
        GraphParameterValue::Asset(value) => Value::String(value.value().to_string()),
        GraphParameterValue::Fixed(value) => Value::from(value.bits()),
        GraphParameterValue::Unit(value) => Value::from(value.bits()),
        GraphParameterValue::FixedVec3(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::from(value.bits()))
                .collect(),
        ),
        GraphParameterValue::WorldPosition(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(value.to_string()))
                .collect(),
        ),
        GraphParameterValue::FieldChannel(value) => Value::String(field_channel_wire(*value)),
        GraphParameterValue::FieldDerivative(value) => {
            Value::String(field_derivative_wire(*value).to_owned())
        }
        GraphParameterValue::CombineOperation(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::DistanceSource(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::ClusterMode(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::Curve(value) => Value::Array(
            value
                .iter()
                .map(|(x, y)| Value::Array(vec![Value::from(x.bits()), Value::from(y.bits())]))
                .collect(),
        ),
        GraphParameterValue::String(value) => Value::String(value.clone()),
        GraphParameterValue::GuidList(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(guid_text(*value)))
                .collect(),
        ),
        GraphParameterValue::TagList(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(value.to_string()))
                .collect(),
        ),
    }
}

fn dependency_json(kind: &str, value: String) -> Value {
    object([
        ("kind", Value::String(kind.to_owned())),
        ("value", Value::String(value)),
    ])
}

fn required_u32(node: &GraphNodeDefinition, name: &str) -> Result<u32> {
    match node.parameter(name) {
        Some(GraphParameterValue::U32(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected u32",
        )),
    }
}

fn required_guid(node: &GraphNodeDefinition, name: &str) -> Result<u128> {
    match node.parameter(name) {
        Some(GraphParameterValue::Guid(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected GUID",
        )),
    }
}

fn required_fixed(node: &GraphNodeDefinition, name: &str) -> Result<DecisionScalar> {
    match node.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected fixed scalar",
        )),
    }
}

fn optional_fixed(node: &GraphNodeDefinition, name: &str) -> Result<Option<DecisionScalar>> {
    match node.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(Some(*value)),
        None => Ok(None),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected fixed scalar",
        )),
    }
}

fn required_u32_vec3(node: &GraphNodeDefinition, name: &str) -> Result<[u32; 3]> {
    match node.parameter(name) {
        Some(GraphParameterValue::U32Vec3(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected unsigned vector",
        )),
    }
}

fn pin(name: impl Into<String>, domain: GraphDomain) -> GraphPin {
    GraphPin {
        name: name.into(),
        domain,
        required: true,
    }
}

fn optional_pin(name: impl Into<String>, domain: GraphDomain) -> GraphPin {
    GraphPin {
        name: name.into(),
        domain,
        required: false,
    }
}

fn micro_attribute_pin(channel: u128) -> String {
    format!("attribute-{channel:032x}")
}

const fn parameter(
    name: &'static str,
    parameter_type: GraphParameterType,
    required: bool,
) -> GraphParameterDescriptor {
    GraphParameterDescriptor {
        name,
        parameter_type,
        required,
    }
}

fn object<K>(fields: impl IntoIterator<Item = (K, Value)>) -> Value
where
    K: Into<String>,
{
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn graph_document(path: &str, reason: &str) -> Error {
    Error::GraphDocument {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

fn reject_unknown(object: &Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(graph_document(&format!("{path}.{key}"), "unknown field"));
    }
    Ok(())
}

fn read_array<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a Vec<Value>> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| graph_document(path, "expected array"))
}

fn read_string(value: Option<&Value>, path: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| graph_document(path, "expected string"))
}

fn read_u8(value: Option<&Value>, path: &str) -> Result<u8> {
    read_u64(value, path)
        .and_then(|value| u8::try_from(value).map_err(|_| graph_document(path, "u8 out of range")))
}

fn read_u16(value: Option<&Value>, path: &str) -> Result<u16> {
    read_u64(value, path).and_then(|value| {
        u16::try_from(value).map_err(|_| graph_document(path, "u16 out of range"))
    })
}

fn read_u32(value: Option<&Value>, path: &str) -> Result<u32> {
    read_u64(value, path).and_then(|value| {
        u32::try_from(value).map_err(|_| graph_document(path, "u32 out of range"))
    })
}

fn read_u64(value: Option<&Value>, path: &str) -> Result<u64> {
    match value {
        Some(Value::String(value)) => value
            .parse()
            .map_err(|_| graph_document(path, "invalid unsigned decimal string")),
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or_else(|| graph_document(path, "expected unsigned integer")),
        _ => Err(graph_document(path, "expected unsigned integer")),
    }
}

fn read_i32(value: Option<&Value>, path: &str) -> Result<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| graph_document(path, "expected i32"))
}

fn read_guid(value: Option<&Value>, path: &str) -> Result<u128> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected lowercase 32-digit hexadecimal GUID"))?;
    if text.len() != 32
        || !text.bytes().all(|byte| byte.is_ascii_hexdigit())
        || text.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(graph_document(
            path,
            "expected lowercase 32-digit hexadecimal GUID",
        ));
    }
    u128::from_str_radix(text, 16).map_err(|_| graph_document(path, "invalid hexadecimal GUID"))
}

fn parse_guid_array(value: &Value, path: &str) -> Result<Vec<u128>> {
    read_array(Some(value), path)?
        .iter()
        .enumerate()
        .map(|(index, value)| read_guid(Some(value), &format!("{path}[{index}]")))
        .collect()
}

fn parse_seed_namespaces(value: &Value, path: &str) -> Result<BTreeMap<String, u128>> {
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(path, "expected object"))?;
    object
        .iter()
        .map(|(name, value)| {
            if name.is_empty() {
                return Err(graph_document(path, "seed namespace name cannot be empty"));
            }
            Ok((
                name.clone(),
                read_guid(Some(value), &format!("{path}.{name}"))?,
            ))
        })
        .collect()
}

fn read_domain(value: Option<&Value>, path: &str) -> Result<GraphDomain> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected domain string"))?;
    GraphDomain::from_wire(text).ok_or_else(|| graph_document(path, "unknown domain"))
}

fn read_field_channel(value: Option<&Value>, path: &str) -> Result<FieldChannel> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected field channel"))?;
    field_channel_from_wire(text).ok_or_else(|| graph_document(path, "unknown field channel"))
}

fn field_channel_from_wire(value: &str) -> Option<FieldChannel> {
    Some(match value {
        "altitude" => FieldChannel::Altitude,
        "slope" => FieldChannel::Slope,
        "curvature" => FieldChannel::Curvature,
        "concavity" => FieldChannel::Concavity,
        "drainage" => FieldChannel::Drainage,
        "moisture" => FieldChannel::Moisture,
        "temperature" => FieldChannel::Temperature,
        "precipitation" => FieldChannel::Precipitation,
        "sunlight" => FieldChannel::Sunlight,
        "exposure" => FieldChannel::Exposure,
        "water-distance" => FieldChannel::WaterDistance,
        "water-depth" => FieldChannel::WaterDepth,
        "signed-blocker" => FieldChannel::SignedBlocker,
        "spline-distance" => FieldChannel::SplineDistance,
        value if value.starts_with("user:") => FieldChannel::User(value[5..].parse().ok()?),
        _ => return None,
    })
}

fn field_channel_wire(value: FieldChannel) -> String {
    match value {
        FieldChannel::Altitude => "altitude".to_owned(),
        FieldChannel::Slope => "slope".to_owned(),
        FieldChannel::Curvature => "curvature".to_owned(),
        FieldChannel::Concavity => "concavity".to_owned(),
        FieldChannel::Drainage => "drainage".to_owned(),
        FieldChannel::Moisture => "moisture".to_owned(),
        FieldChannel::Temperature => "temperature".to_owned(),
        FieldChannel::Precipitation => "precipitation".to_owned(),
        FieldChannel::Sunlight => "sunlight".to_owned(),
        FieldChannel::Exposure => "exposure".to_owned(),
        FieldChannel::WaterDistance => "water-distance".to_owned(),
        FieldChannel::WaterDepth => "water-depth".to_owned(),
        FieldChannel::SignedBlocker => "signed-blocker".to_owned(),
        FieldChannel::SplineDistance => "spline-distance".to_owned(),
        FieldChannel::User(id) => format!("user:{id}"),
    }
}

fn field_derivative_from_wire(value: &str) -> Option<FieldDerivative> {
    Some(match value {
        "value" => FieldDerivative::Value,
        "gradient" => FieldDerivative::Gradient,
        "hessian" => FieldDerivative::Hessian,
        _ => return None,
    })
}

const fn field_derivative_wire(value: FieldDerivative) -> &'static str {
    match value {
        FieldDerivative::Value => "value",
        FieldDerivative::Gradient => "gradient",
        FieldDerivative::Hessian => "hessian",
    }
}

fn append_dependency_source(bytes: &mut Vec<u8>, source: GraphDependencySource) {
    bytes.extend_from_slice(&dependency_source_bytes(source));
}

fn dependency_source_bytes(source: GraphDependencySource) -> Vec<u8> {
    let mut bytes = Vec::new();
    match source {
        GraphDependencySource::Asset(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.value().to_be_bytes());
        }
        GraphDependencySource::Field(channel) => {
            bytes.push(1);
            let wire = field_channel_wire(channel);
            bytes.extend_from_slice(&(wire.len() as u64).to_be_bytes());
            bytes.extend_from_slice(wire.as_bytes());
        }
        GraphDependencySource::SurfaceProvider(provider) => {
            bytes.push(2);
            bytes.extend_from_slice(&provider.to_be_bytes());
        }
        GraphDependencySource::MapLayer(layer) => {
            bytes.push(3);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
    }
    bytes
}

fn guid_text(value: u128) -> String {
    format!("{value:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BIOME_ASSET_VERSION, BiomeGraphPolicy, BiomeModuleReference, BiomeParameter,
        BiomeParameterType,
    };

    struct NoModules;

    impl BiomeGraphResolver for NoModules {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            Err(graph_document(
                "resolver",
                &format!("unexpected module {}", id.value()),
            ))
        }

        fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
            Err(graph_document(
                "resolver",
                &format!("unexpected dependency {source:?}"),
            ))
        }
    }

    struct HashedDependency(u8);

    impl BiomeGraphResolver for HashedDependency {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            Err(graph_document(
                "resolver",
                &format!("unexpected module {}", id.value()),
            ))
        }

        fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
            Ok([self.0; 32])
        }
    }

    struct ModuleResolver {
        module: BiomeAsset,
    }

    impl BiomeGraphResolver for ModuleResolver {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            if id == self.module.id {
                Ok(self.module.clone())
            } else {
                Err(graph_document("resolver", "unknown module"))
            }
        }

        fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
            Ok([9; 32])
        }
    }

    struct SelectiveModuleResolver {
        module: BiomeAsset,
        source: GraphDependencySource,
        content_hash: [u8; 32],
    }

    impl BiomeGraphResolver for SelectiveModuleResolver {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            if id == self.module.id {
                Ok(self.module.clone())
            } else {
                Err(graph_document("resolver", "unknown module"))
            }
        }

        fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
            Ok(if source == self.source {
                self.content_hash
            } else {
                [9; 32]
            })
        }
    }

    struct ModuleSetResolver {
        modules: Vec<BiomeAsset>,
    }

    impl BiomeGraphResolver for ModuleSetResolver {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            self.modules
                .iter()
                .find(|module| module.id == id)
                .cloned()
                .ok_or_else(|| graph_document("resolver", "unknown module"))
        }

        fn resolve_dependency_hash(&self, _source: GraphDependencySource) -> Result<[u8; 32]> {
            Ok([9; 32])
        }
    }

    fn node(guid: u128, operator: GraphOperator) -> GraphNodeDefinition {
        GraphNodeDefinition {
            guid,
            version: BIOME_NODE_VERSION,
            semantic_revision: 1,
            operator,
            authority: GraphAuthority::Authoritative,
            spatial: NodeSpatialPolicy::Partitioned {
                level: 0,
                influence_radius: DecisionScalar::from_bits(0),
            },
            dependencies: Vec::new(),
            seed_namespaces: BTreeMap::new(),
            parameters: BTreeMap::new(),
        }
    }

    fn biome(document: BiomeGraphDocument) -> BiomeAsset {
        BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id: Uuid(7),
            name: "Test".to_owned(),
            role: BiomeRole::Root,
            parameters: Vec::new(),
            palette: Vec::new(),
            density: DecisionScalar::from_bits(0),
            clustering: UnitInterval::ZERO,
            suitability: Vec::new(),
            competition: Vec::new(),
            companions: Vec::new(),
            succession: Vec::new(),
            seed_namespaces: vec![("placement".to_owned(), 11)],
            modules: Vec::new(),
            policy: BiomeGraphPolicy {
                maximum_recursion: 8,
                maximum_influence_radius: DecisionScalar::from_bits(65_536),
                require_authoritative_fields: true,
            },
            graph: document.to_json(),
        }
    }

    fn simple_document() -> BiomeGraphDocument {
        let region = node(1, GraphOperator::RegionInput);
        let mut scatter = node(2, GraphOperator::StratifiedCoverage);
        scatter.seed_namespaces.insert("sampling".to_owned(), 11);
        scatter
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(16));
        let species = node(3, GraphOperator::SpeciesInput);
        let mut output = node(4, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 11);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, scatter, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        }
    }

    fn module_call_node(guid: u128, call_guid: u128) -> GraphNodeDefinition {
        let mut call = node(guid, GraphOperator::ModuleCall);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(call_guid));
        call
    }

    fn module_asset(id: u64, child: Option<(u64, u128)>, maximum_recursion: u16) -> BiomeAsset {
        let mut document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        let module_reference = child.map(|(child_id, call_guid)| {
            document.nodes.push(module_call_node(call_guid, call_guid));
            BiomeModuleReference {
                biome: Uuid(child_id),
                call_guid,
                bindings: Vec::new(),
            }
        });
        let mut asset = biome(document);
        asset.id = Uuid(id);
        asset.role = BiomeRole::Module;
        asset.name = format!("Module {id}");
        asset.policy.maximum_recursion = maximum_recursion;
        if let Some(module_reference) = module_reference {
            asset.modules.push(module_reference);
        }
        asset
    }

    fn root_with_module(module: u64, call_guid: u128) -> BiomeAsset {
        let mut document = simple_document();
        document.nodes.push(module_call_node(call_guid, call_guid));
        let mut root = biome(document);
        root.modules.push(BiomeModuleReference {
            biome: Uuid(module),
            call_guid,
            bindings: Vec::new(),
        });
        root
    }

    #[test]
    fn topological_order_counts_one_dependency_per_node_pair() {
        let nodes = [
            node(1, GraphOperator::RegionInput),
            node(2, GraphOperator::Transform),
        ];
        let edges = [
            GraphEdge {
                from_node: 1,
                from_pin: "first".to_owned(),
                to_node: 2,
                to_pin: "first".to_owned(),
            },
            GraphEdge {
                from_node: 1,
                from_pin: "second".to_owned(),
                to_node: 2,
                to_pin: "second".to_owned(),
            },
        ];

        assert_eq!(topological_order(&nodes, &edges).unwrap(), vec![1, 2]);
    }

    #[test]
    fn document_round_trip_is_canonical() {
        let document = simple_document();
        let decoded = BiomeGraphDocument::from_json(&document.to_json()).unwrap();
        assert_eq!(decoded, document);
        assert_eq!(decoded.identity(), document.identity());
    }

    #[test]
    fn noncurrent_node_versions_are_rejected() {
        let document = simple_document();
        let mut raw = document.to_json();
        let scatter = raw
            .get_mut("nodes")
            .and_then(Value::as_array_mut)
            .unwrap()
            .iter_mut()
            .find(|node| {
                node.get("guid").and_then(Value::as_str) == Some("00000000000000000000000000000002")
            })
            .unwrap()
            .as_object_mut()
            .unwrap();
        scatter.insert("version".to_owned(), Value::from(0));
        assert!(matches!(
            BiomeGraphDocument::from_json(&raw),
            Err(Error::FormatVersion {
                format: ".sbiome node",
                found: 0,
                expected: BIOME_NODE_VERSION,
            })
        ));
    }

    #[test]
    fn dependency_content_hashes_participate_in_compiled_identity() {
        let mut document = simple_document();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .dependencies = vec![GraphDependencySource::Asset(Uuid(99))];
        let asset = biome(document);
        let first = compile_biome_graph(
            &asset,
            &[],
            &HashedDependency(1),
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let second = compile_biome_graph(
            &asset,
            &[],
            &HashedDependency(2),
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_ne!(first.identity, second.identity);
        assert_eq!(first.dependencies()[0].content_hash, [1; 32]);
    }

    #[test]
    fn execution_and_stage_identities_ignore_dead_global_branches() {
        let mut document = simple_document();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .spatial = NodeSpatialPolicy::Global { level: 2 };
        let baseline = compile_biome_graph(
            &biome(document.clone()),
            &[],
            &NoModules,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_eq!(baseline.spatial_plan().global_stages().len(), 1);
        let mut other_root = biome(document.clone());
        other_root.id = Uuid(8);
        let other_root = compile_biome_graph(
            &other_root,
            &[],
            &NoModules,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_ne!(
            baseline.spatial_plan().global_stages()[0].id,
            other_root.spatial_plan().global_stages()[0].id
        );

        let dead_region = node(99, GraphOperator::RegionInput);
        let mut dead_coverage = node(100, GraphOperator::StratifiedCoverage);
        dead_coverage.spatial = NodeSpatialPolicy::Global { level: 4 };
        dead_coverage
            .seed_namespaces
            .insert("sampling".to_owned(), 11);
        dead_coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(u32::MAX));
        dead_coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        document.nodes.extend([dead_region, dead_coverage]);
        document.edges.push(GraphEdge {
            from_node: 99,
            from_pin: "regions".to_owned(),
            to_node: 100,
            to_pin: "regions".to_owned(),
        });
        let with_dead_branch = compile_biome_graph(
            &biome(document.clone()),
            &[],
            &NoModules,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        compile_biome_graph(
            &biome(document.clone()),
            &[],
            &NoModules,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_candidates: 16,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap();
        assert_eq!(baseline.identity, with_dead_branch.identity);
        assert_eq!(baseline.dependencies(), with_dead_branch.dependencies());
        assert_eq!(
            baseline
                .spatial_plan()
                .global_stages()
                .iter()
                .map(|stage| stage.id)
                .collect::<Vec<_>>(),
            with_dead_branch
                .spatial_plan()
                .global_stages()
                .iter()
                .map(|stage| stage.id)
                .collect::<Vec<_>>()
        );

        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(17));
        assert!(matches!(
            compile_biome_graph(
                &biome(document.clone()),
                &[],
                &NoModules,
                GraphCompileOptions {
                    limits: GraphSafetyLimits {
                        max_candidates: 16,
                        ..GraphSafetyLimits::default()
                    },
                },
            ),
            Err(Error::GraphLimit {
                resource: "candidate count",
                requested: 17,
                limit: 16,
            })
        ));
        let live_edit = compile_biome_graph(
            &biome(document),
            &[],
            &NoModules,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_ne!(baseline.identity, live_edit.identity);
        assert_ne!(
            baseline.spatial_plan().global_stages()[0].id,
            live_edit.spatial_plan().global_stages()[0].id
        );
    }

    #[test]
    fn execution_identity_is_invariant_to_dependency_order() {
        let mut document = simple_document();
        let live = document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap();
        live.spatial = NodeSpatialPolicy::Global { level: 2 };
        live.dependencies = vec![
            GraphDependencySource::Asset(Uuid(80)),
            GraphDependencySource::Asset(Uuid(81)),
        ];
        let first = compile_biome_graph(
            &biome(document.clone()),
            &[],
            &HashedDependency(7),
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .dependencies
            .reverse();
        let second = compile_biome_graph(
            &biome(document),
            &[],
            &HashedDependency(7),
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_eq!(first.identity, second.identity);
        assert_eq!(
            first.spatial_plan().global_stages()[0].id,
            second.spatial_plan().global_stages()[0].id
        );
    }

    #[test]
    fn propagating_operators_require_global_policy_but_competition_is_partitionable() {
        let asset = biome(simple_document());
        for (offset, operator) in [
            GraphOperator::BlueNoisePoisson,
            GraphOperator::WeightedElimination,
            GraphOperator::VariableSpacing,
            GraphOperator::PriorityExclusion,
            GraphOperator::BoundsOverlap,
        ]
        .into_iter()
        .enumerate()
        {
            let mut definition = node(100 + offset as u128, operator);
            definition.spatial = NodeSpatialPolicy::Partitioned {
                level: 0,
                influence_radius: DecisionScalar::from_bits(65_536),
            };
            assert!(matches!(
                validate_spatial_contract(&definition, &asset),
                Err(Error::GraphUnboundedInfluence { node }) if node == definition.guid
            ));
            definition.spatial = NodeSpatialPolicy::Global { level: 2 };
            validate_spatial_contract(&definition, &asset).unwrap();
        }

        let mut competition = node(200, GraphOperator::Competition);
        competition.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(65_536),
        };
        validate_spatial_contract(&competition, &asset).unwrap();
        assert_eq!(
            competition.operator.spatial_requirement(),
            GraphSpatialRequirement::FiniteSupport
        );
    }

    #[test]
    fn public_compiler_rejects_partitioned_propagating_nodes() {
        let mut document = simple_document();
        let scatter = document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap();
        scatter.operator = GraphOperator::BlueNoisePoisson;
        scatter.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        scatter
            .parameters
            .insert("attempts".to_owned(), GraphParameterValue::U32(8));

        assert!(matches!(
            compile_biome_graph(
                &biome(document),
                &[],
                &NoModules,
                GraphCompileOptions::canonical(),
            ),
            Err(Error::GraphUnboundedInfluence { node: 2 })
        ));
    }

    #[test]
    fn graph_numeric_parameters_reject_fractional_values_at_the_typed_path() {
        let mut value = simple_document().to_json();
        let nodes = value
            .get_mut("nodes")
            .and_then(Value::as_array_mut)
            .unwrap();
        let scatter_guid = guid_text(2);
        let scatter = nodes
            .iter_mut()
            .find(|node| node.get("guid").and_then(Value::as_str) == Some(scatter_guid.as_str()))
            .unwrap();
        scatter
            .get_mut("parameters")
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("count".to_owned(), Value::from(1.5));

        assert!(matches!(
            BiomeGraphDocument::from_json(&value),
            Err(Error::GraphDocument { path, reason })
                if path.ends_with(".parameters.count") && reason == "expected unsigned integer"
        ));
    }

    #[test]
    fn global_stage_closure_exposes_exact_composed_upstream_halo_and_boundary() {
        let mut document = simple_document();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(2 * 65_536),
        };
        let mut transform = node(5, GraphOperator::Transform);
        transform.spatial = NodeSpatialPolicy::Global { level: 1 };
        transform.seed_namespaces.insert("variation".to_owned(), 11);
        document.nodes.push(transform);
        document
            .edges
            .retain(|edge| !(edge.from_node == 2 && edge.to_node == 4));
        document.edges.extend([
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
        ]);
        let mut asset = biome(document);
        asset.policy.maximum_influence_radius = DecisionScalar::from_bits(2 * 65_536);
        let compiled =
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()).unwrap();
        let stage = &compiled.spatial_plan().global_stages()[0];
        assert_eq!(stage.owner_level, 1);
        assert_eq!(stage.minimum_input_level, 0);
        assert_eq!(stage.upstream_halo, DecisionScalar::from_bits(2 * 65_536));
        assert_eq!(
            stage.closure,
            vec![
                GraphNodeAddress {
                    module_path: Vec::new(),
                    node: 1,
                },
                GraphNodeAddress {
                    module_path: Vec::new(),
                    node: 2,
                },
                GraphNodeAddress {
                    module_path: Vec::new(),
                    node: 5,
                },
            ]
        );
        assert_eq!(
            stage.output_pins,
            vec![QualifiedGraphPin {
                node: GraphNodeAddress {
                    module_path: Vec::new(),
                    node: 5,
                },
                pin: "candidates".to_owned(),
            }]
        );
        assert_eq!(
            compiled
                .spatial_plan()
                .maximum_global_tiles_for_output_cells(3),
            Some(3)
        );
    }

    #[test]
    fn module_parameter_bindings_resolve_into_the_single_compiled_ir() {
        let mut interface = node(10, GraphOperator::InterfaceInput);
        interface.seed_namespaces.clear();
        interface.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("regions".to_owned()),
        );
        let mut poisson = node(11, GraphOperator::BlueNoisePoisson);
        poisson.seed_namespaces.insert("sampling".to_owned(), 11);
        poisson.spatial = NodeSpatialPolicy::Global { level: 0 };
        poisson
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        poisson
            .parameters
            .insert("radius".to_owned(), GraphParameterValue::Binding(55));
        poisson
            .parameters
            .insert("attempts".to_owned(), GraphParameterValue::U32(8));
        let mut transform = node(12, GraphOperator::Transform);
        transform.spatial = NodeSpatialPolicy::Global { level: 0 };
        transform.seed_namespaces.insert("variation".to_owned(), 11);
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: vec![GraphInterfaceInput {
                id: 2010,
                name: "regions".to_owned(),
                domain: GraphDomain::Regions,
            }],
            outputs: vec![GraphInterfaceOutput {
                id: 2011,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
                node: 12,
                pin: "candidates".to_owned(),
                sink: None,
            }],
            nodes: vec![transform, poisson, interface],
            edges: vec![
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 11,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 11,
                    from_pin: "candidates".to_owned(),
                    to_node: 12,
                    to_pin: "candidates".to_owned(),
                },
            ],
        };
        let mut module = biome(module_document);
        module.id = Uuid(88);
        module.role = BiomeRole::Module;
        module.policy.maximum_influence_radius = DecisionScalar::from_bits(4 * 65_536);
        module.parameters = vec![BiomeParameter {
            id: 55,
            name: "radius".to_owned(),
            parameter_type: BiomeParameterType::Scalar,
            default_value: Value::from(65_536),
        }];

        let mut region = node(1, GraphOperator::RegionInput);
        region.seed_namespaces.clear();
        let mut call = node(2, GraphOperator::ModuleCall);
        call.seed_namespaces.clear();
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let mut species = node(3, GraphOperator::SpeciesInput);
        species.seed_namespaces.clear();
        let mut output = node(4, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 11);
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, call, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut root = biome(root_document);
        root.policy.maximum_influence_radius = DecisionScalar::from_bits(4 * 65_536);
        root.modules = vec![BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: vec![(55, Value::from(2 * 65_536))],
        }];
        let resolver = ModuleResolver { module };
        let compiled =
            compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
        let radius = compiled.root.nodes[1]
            .module
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|node| node.definition.guid == 11)
            .unwrap()
            .definition
            .parameter("radius");
        assert_eq!(
            radius,
            Some(&GraphParameterValue::Fixed(DecisionScalar::from_bits(
                2 * 65_536
            )))
        );
        let stages = compiled.spatial_plan().global_stages();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            stages[0].nodes,
            vec![
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 11,
                },
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 12,
                },
            ]
        );
        assert!(stages[0].closure.contains(&GraphNodeAddress {
            module_path: Vec::new(),
            node: 1,
        }));
        assert_eq!(
            stages[0].output_pins,
            vec![QualifiedGraphPin {
                node: GraphNodeAddress {
                    module_path: vec![99],
                    node: 12,
                },
                pin: "candidates".to_owned(),
            }]
        );
    }

    #[test]
    fn unused_module_instance_does_not_create_a_global_stage() {
        let mut interface = node(10, GraphOperator::InterfaceInput);
        interface.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("regions".to_owned()),
        );
        let mut poisson = node(11, GraphOperator::BlueNoisePoisson);
        poisson.spatial = NodeSpatialPolicy::Global { level: 2 };
        poisson.seed_namespaces.insert("sampling".to_owned(), 11);
        poisson
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        poisson.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        poisson
            .parameters
            .insert("attempts".to_owned(), GraphParameterValue::U32(8));
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: vec![GraphInterfaceInput {
                id: 2010,
                name: "regions".to_owned(),
                domain: GraphDomain::Regions,
            }],
            outputs: vec![GraphInterfaceOutput {
                id: 2011,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
                node: 11,
                pin: "candidates".to_owned(),
                sink: None,
            }],
            nodes: vec![poisson, interface],
            edges: vec![GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "regions".to_owned(),
            }],
        };
        let mut module = biome(module_document);
        module.id = Uuid(88);
        module.role = BiomeRole::Module;

        let region = node(1, GraphOperator::RegionInput);
        let mut first_call = node(2, GraphOperator::ModuleCall);
        first_call
            .parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(91));
        let call_dependency = GraphDependencySource::Asset(Uuid(777));
        first_call.dependencies.push(call_dependency);
        let mut second_call = node(3, GraphOperator::ModuleCall);
        second_call
            .parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(92));
        let species = node(4, GraphOperator::SpeciesInput);
        let mut output = node(5, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 11);
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, second_call, first_call, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 3,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "species".to_owned(),
                    to_node: 5,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut root = biome(root_document);
        root.modules = vec![
            BiomeModuleReference {
                biome: module.id,
                call_guid: 91,
                bindings: Vec::new(),
            },
            BiomeModuleReference {
                biome: module.id,
                call_guid: 92,
                bindings: Vec::new(),
            },
        ];
        let compiled = compile_biome_graph(
            &root,
            &[],
            &SelectiveModuleResolver {
                module: module.clone(),
                source: call_dependency,
                content_hash: [7; 32],
            },
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let stages = compiled.spatial_plan().global_stages();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            stages
                .iter()
                .map(|stage| stage.nodes.clone())
                .collect::<Vec<_>>(),
            vec![vec![GraphNodeAddress {
                module_path: vec![91],
                node: 11,
            }]]
        );
        for stage in stages {
            assert!(stage.closure.contains(&GraphNodeAddress {
                module_path: Vec::new(),
                node: 1,
            }));
            assert_eq!(stage.owner_level, 2);
            assert_eq!(stage.minimum_input_level, 0);
            assert!(!stage.dependencies.is_empty());
            assert_eq!(
                compiled
                    .spatial_plan()
                    .global_stage_for_node(&stage.nodes[0])
                    .map(|candidate| candidate.id),
                Some(stage.id)
            );
            assert!(stage.dependencies.contains(&GraphDependencyFingerprint {
                source: call_dependency,
                content_hash: [7; 32],
            }));
        }
        let changed_dependency = compile_biome_graph(
            &root,
            &[],
            &SelectiveModuleResolver {
                module,
                source: call_dependency,
                content_hash: [8; 32],
            },
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        assert_ne!(
            compiled.spatial_plan().global_stages()[0].id,
            changed_dependency.spatial_plan().global_stages()[0].id
        );
    }

    #[test]
    fn module_demand_prunes_unused_output_branch_and_its_unique_input() {
        let mut input_a = node(10, GraphOperator::InterfaceInput);
        input_a.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("regions-a".to_owned()),
        );
        let mut input_b = node(20, GraphOperator::InterfaceInput);
        input_b.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("regions-b".to_owned()),
        );
        let mut live = node(11, GraphOperator::StratifiedCoverage);
        live.seed_namespaces.insert("sampling".to_owned(), 11);
        live.parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        live.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut dead = node(21, GraphOperator::StratifiedCoverage);
        dead.seed_namespaces.insert("sampling".to_owned(), 11);
        dead.parameters
            .insert("count".to_owned(), GraphParameterValue::U32(999));
        dead.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut dead_transform_a = node(22, GraphOperator::Transform);
        dead_transform_a.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(49_152),
        };
        dead_transform_a
            .seed_namespaces
            .insert("variation".to_owned(), 11);
        let mut dead_transform_b = node(23, GraphOperator::Transform);
        dead_transform_b.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(49_152),
        };
        dead_transform_b
            .seed_namespaces
            .insert("variation".to_owned(), 11);
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: vec![
                GraphInterfaceInput {
                    id: 2010,
                    name: "regions-a".to_owned(),
                    domain: GraphDomain::Regions,
                },
                GraphInterfaceInput {
                    id: 2020,
                    name: "regions-b".to_owned(),
                    domain: GraphDomain::Regions,
                },
            ],
            outputs: vec![
                GraphInterfaceOutput {
                    id: 2011,
                    name: "a".to_owned(),
                    domain: GraphDomain::Candidates,
                    node: 11,
                    pin: "candidates".to_owned(),
                    sink: None,
                },
                GraphInterfaceOutput {
                    id: 2021,
                    name: "b".to_owned(),
                    domain: GraphDomain::Candidates,
                    node: 23,
                    pin: "candidates".to_owned(),
                    sink: None,
                },
            ],
            nodes: vec![
                dead_transform_b,
                dead_transform_a,
                dead,
                live,
                input_b,
                input_a,
            ],
            edges: vec![
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 11,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 20,
                    from_pin: "value".to_owned(),
                    to_node: 21,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 21,
                    from_pin: "candidates".to_owned(),
                    to_node: 22,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 22,
                    from_pin: "candidates".to_owned(),
                    to_node: 23,
                    to_pin: "candidates".to_owned(),
                },
            ],
        };
        let mut module = biome(module_document);
        module.id = Uuid(88);
        module.role = BiomeRole::Module;

        let region = node(1, GraphOperator::RegionInput);
        let mut call = node(2, GraphOperator::ModuleCall);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let species = node(3, GraphOperator::SpeciesInput);
        let mut output = node(4, GraphOperator::MacroOutput);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 11);
        let mut root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, call, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions-a".to_owned(),
                },
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions-b".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "a".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let compile = |document: &BiomeGraphDocument| {
            let mut root = biome(document.clone());
            root.modules.push(BiomeModuleReference {
                biome: module.id,
                call_guid: 99,
                bindings: Vec::new(),
            });
            compile_biome_graph(
                &root,
                &[],
                &ModuleResolver {
                    module: module.clone(),
                },
                GraphCompileOptions {
                    limits: GraphSafetyLimits {
                        max_candidates: 10,
                        ..GraphSafetyLimits::default()
                    },
                },
            )
        };
        let compiled = compile(&root_document).unwrap();
        let nested = compiled
            .demand_plan()
            .execution_slice()
            .unit(&[99])
            .unwrap();
        assert_eq!(nested.nodes, vec![10, 11]);
        assert_eq!(nested.inputs, BTreeSet::from(["regions-a".to_owned()]));
        assert_eq!(nested.outputs, BTreeSet::from(["a".to_owned()]));
        assert!(
            !compiled
                .demand_plan()
                .execution_slice()
                .unit(&[])
                .unwrap()
                .edges
                .iter()
                .any(|edge| edge.to_node == 2 && edge.to_pin == "regions-b")
        );

        root_document
            .edges
            .iter_mut()
            .find(|edge| edge.to_node == 4 && edge.to_pin == "candidates")
            .unwrap()
            .from_pin = "b".to_owned();
        assert!(matches!(
            compile(&root_document),
            Err(Error::GraphLimit {
                resource: "candidate count",
                requested: 999,
                limit: 10,
            })
        ));
        let mut root = biome(root_document);
        root.modules.push(BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        });
        assert!(matches!(
            compile_biome_graph(
                &root,
                &[],
                &ModuleResolver { module },
                GraphCompileOptions {
                    limits: GraphSafetyLimits {
                        max_candidates: 1_000,
                        ..GraphSafetyLimits::default()
                    },
                },
            ),
            Err(Error::GraphLimit {
                resource: "composed influence radius",
                ..
            })
        ));
    }

    #[test]
    fn compiler_topologically_orders_and_estimates() {
        let asset = biome(simple_document());
        let compiled =
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()).unwrap();
        assert_eq!(
            compiled
                .root
                .nodes
                .iter()
                .map(|node| node.definition.guid)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(compiled.root.estimate.candidates, 16);
        assert_eq!(compiled.root.estimate.accepted, 16);
        for node in &compiled.root.nodes {
            assert_eq!(
                node.definition_hash,
                sha256(&node.definition.canonical_bytes())
            );
        }
    }

    #[test]
    fn compiler_rejects_fields_from_a_different_candidate_stream() {
        let mut document = simple_document();
        let mut second_scatter = node(5, GraphOperator::StratifiedCoverage);
        second_scatter
            .seed_namespaces
            .insert("sampling".to_owned(), 11);
        second_scatter
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(16));
        let mut noise = node(6, GraphOperator::Noise);
        noise.seed_namespaces.insert("noise".to_owned(), 11);
        noise.parameters.insert(
            "frequency".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise.parameters.insert(
            "amplitude".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise
            .parameters
            .insert("channel".to_owned(), GraphParameterValue::U32(0));
        let mut importance = node(7, GraphOperator::FieldImportance);
        importance.parameters.insert(
            "threshold".to_owned(),
            GraphParameterValue::Unit(UnitInterval::from_bits(32_768)),
        );
        document.nodes.extend([second_scatter, noise, importance]);
        document.edges.extend([
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 5,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 7,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "field".to_owned(),
                to_node: 7,
                to_pin: "weights".to_owned(),
            },
        ]);
        let error = compile_biome_graph(
            &biome(document),
            &[],
            &NoModules,
            GraphCompileOptions::canonical(),
        )
        .unwrap_err();
        assert!(matches!(error, Error::GraphDocument { path, .. } if path.ends_with(".weights")));
    }

    #[test]
    fn cosmetic_value_cannot_reach_macro_output() {
        let mut document = simple_document();
        document.nodes[2].authority = GraphAuthority::Cosmetic;
        let asset = biome(document);
        assert!(matches!(
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()),
            Err(Error::GraphAuthority { .. })
        ));
    }

    #[test]
    fn compiler_accumulates_spatial_support_along_dependency_paths() {
        let mut document = simple_document();
        let scatter = document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap();
        scatter.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(2 * 65_536),
        };

        let communities = node(5, GraphOperator::CommunityInput);
        let mut competition = node(6, GraphOperator::Competition);
        competition.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(4 * 65_536),
        };
        competition.parameters.insert(
            "crownWeight".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ONE),
        );
        competition.parameters.insert(
            "rootWeight".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ONE),
        );
        document.nodes.extend([communities, competition]);
        document
            .edges
            .retain(|edge| !(edge.from_node == 2 && edge.to_node == 4));
        document.edges.extend([
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "communities".to_owned(),
                to_node: 6,
                to_pin: "communities".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
        ]);

        let mut asset = biome(document);
        asset.policy.maximum_influence_radius = DecisionScalar::from_bits(6 * 65_536);
        let compiled =
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()).unwrap();
        assert_eq!(
            compiled.required_halo(0),
            DecisionScalar::from_bits(6 * 65_536)
        );

        asset.policy.maximum_influence_radius = DecisionScalar::from_bits(5 * 65_536);
        assert!(matches!(
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical(),),
            Err(Error::GraphLimit {
                resource: "composed influence radius",
                ..
            })
        ));
    }

    #[test]
    fn module_depth_counts_call_edges_and_enforces_exact_boundaries() {
        compile_biome_graph(
            &biome(simple_document()),
            &[],
            &NoModules,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 0,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap();

        let leaf = module_asset(90, None, 0);
        let single_resolver = ModuleSetResolver {
            modules: vec![leaf.clone()],
        };
        let single_root = root_with_module(90, 800);
        let error = compile_biome_graph(
            &single_root,
            &[],
            &single_resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 0,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "module recursion",
                requested: 1,
                limit: 0,
            }
        ));
        compile_biome_graph(
            &single_root,
            &[],
            &single_resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 1,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap();

        let branch = module_asset(89, Some((90, 900)), 8);
        let resolver = ModuleSetResolver {
            modules: vec![branch, leaf],
        };
        let mut root = root_with_module(89, 800);
        root.policy.maximum_recursion = 8;

        let error = compile_biome_graph(
            &root,
            &[],
            &resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 1,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "module recursion",
                requested: 2,
                limit: 1,
            }
        ));

        compile_biome_graph(
            &root,
            &[],
            &resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 2,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap();

        root.policy.maximum_recursion = 1;
        let error = compile_biome_graph(
            &root,
            &[],
            &resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 2,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "module recursion",
                requested: 2,
                limit: 1,
            }
        ));
    }

    #[test]
    fn module_policy_is_relative_to_its_entry_depth() {
        let leaf = module_asset(92, None, 0);
        let inner = module_asset(91, Some((92, 910)), 8);
        let outer = module_asset(90, Some((91, 900)), 1);
        let mut root = root_with_module(90, 800);
        root.policy.maximum_recursion = 8;
        let resolver = ModuleSetResolver {
            modules: vec![outer.clone(), inner.clone(), leaf.clone()],
        };
        let options = GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 3,
                ..GraphSafetyLimits::default()
            },
        };

        let error = compile_biome_graph(&root, &[], &resolver, options).unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "module recursion",
                requested: 3,
                limit: 2,
            }
        ));

        let mut allowed_outer = outer;
        allowed_outer.policy.maximum_recursion = 2;
        let resolver = ModuleSetResolver {
            modules: vec![allowed_outer, inner, leaf],
        };
        compile_biome_graph(
            &root,
            &[],
            &resolver,
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_module_depth: 3,
                    ..GraphSafetyLimits::default()
                },
            },
        )
        .unwrap();
    }

    #[test]
    fn indirect_module_cycle_returns_the_typed_cycle_error() {
        let first = module_asset(89, Some((90, 890)), 8);
        let second = module_asset(90, Some((89, 900)), 8);
        let resolver = ModuleSetResolver {
            modules: vec![first, second],
        };
        let mut root = root_with_module(89, 800);
        root.policy.maximum_recursion = 8;

        assert!(matches!(
            compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical(),),
            Err(Error::GraphCycle { node: 89 })
        ));
    }

    #[test]
    fn graph_cycle_and_hard_count_limit_are_typed_errors() {
        let mut document = simple_document();
        let mut first = node(5, GraphOperator::Transform);
        first.seed_namespaces.insert("variation".to_owned(), 11);
        let mut second = node(6, GraphOperator::Transform);
        second.seed_namespaces.insert("variation".to_owned(), 11);
        document.nodes.extend([first, second]);
        document.edges.extend([
            GraphEdge {
                from_node: 5,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
        ]);
        let asset = biome(document);
        assert!(matches!(
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()),
            Err(Error::GraphCycle { node: 5 })
        ));

        let asset = biome(simple_document());
        let options = GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_candidates: 4,
                ..GraphSafetyLimits::default()
            },
        };
        assert!(matches!(
            compile_biome_graph(&asset, &[], &NoModules, options),
            Err(Error::GraphLimit {
                resource: "candidate count",
                ..
            })
        ));
    }

    #[test]
    fn compiler_rejects_numeric_overflow_as_a_typed_error() {
        let mut document = simple_document();
        let mut recursive = node(5, GraphOperator::RecursiveCompanion);
        recursive
            .seed_namespaces
            .insert("companions".to_owned(), 11);
        recursive.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(i32::MAX),
        };
        recursive
            .parameters
            .insert("children".to_owned(), GraphParameterValue::U32(2));
        recursive.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(i32::MAX)),
        );
        recursive.parameters.insert(
            "maximumDepth".to_owned(),
            GraphParameterValue::U32(u32::MAX),
        );
        document.nodes.push(recursive);
        document
            .edges
            .retain(|edge| !(edge.from_node == 2 && edge.to_node == 4));
        document.edges.extend([
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
        ]);
        let mut asset = biome(document);
        asset.policy.maximum_influence_radius = DecisionScalar::from_bits(i32::MAX);

        assert!(matches!(
            compile_biome_graph(&asset, &[], &NoModules, GraphCompileOptions::canonical()),
            Err(Error::NumericOverflow)
        ));
    }
}
