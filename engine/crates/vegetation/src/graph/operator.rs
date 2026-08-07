//! The closed biome operator vocabulary and its pin/parameter schemas.

use crate::graph_gpu::GraphGpuOperator;

use super::*;

/// Complete initial biome operator vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphOperator {
    /// Public module input value.
    InterfaceInput,
    /// Root/module region input.
    RegionInput,
    SplineInput,
    /// Plant-family palette input.
    SpeciesInput,
    /// Community/companion table input.
    CommunityInput,
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
        GraphGpuOperator::from_graph(self).is_some()
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

    pub(super) fn output_requires_input(self, output: &str, input: &str) -> bool {
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

pub(super) fn pin(name: impl Into<String>, domain: GraphDomain) -> GraphPin {
    GraphPin {
        name: name.into(),
        domain,
        required: true,
    }
}

pub(super) fn optional_pin(name: impl Into<String>, domain: GraphDomain) -> GraphPin {
    GraphPin {
        name: name.into(),
        domain,
        required: false,
    }
}

pub(super) fn micro_attribute_pin(channel: u128) -> String {
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
