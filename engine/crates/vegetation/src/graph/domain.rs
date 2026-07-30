//! Typed value domains, authority classes, spatial policy, and parameter vocabulary.

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, FieldChannel, FieldDerivative, UnitInterval};

use super::*;

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

    pub(super) fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "authoritative" => Self::Authoritative,
            "equivalent-gpu" => Self::EquivalentGpu,
            "cosmetic" => Self::Cosmetic,
            _ => return None,
        })
    }

    pub(super) fn join(self, other: Self) -> Self {
        self.max(other)
    }

    pub(super) fn can_feed_authority(self) -> bool {
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
    pub name: String,
    pub domain: GraphDomain,
    /// Whether an input edge is mandatory.
    pub required: bool,
}

/// Typed parameter shapes accepted by operator schemas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphParameterType {
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
    GuidList(Vec<u128>),
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
    pub(super) const fn as_wire(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Multiply => "multiply",
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
        }
    }

    pub(super) fn from_wire(value: &str) -> Option<Self> {
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
    pub(super) const fn as_wire(self) -> &'static str {
        match self {
            Self::Water => "water",
            Self::Spline => "spline",
            Self::Shape => "shape",
            Self::Blocker => "blocker",
        }
    }

    pub(super) fn from_wire(value: &str) -> Option<Self> {
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
    pub(super) const fn as_wire(self) -> &'static str {
        match self {
            Self::Cluster => "cluster",
            Self::Patch => "patch",
            Self::Colony => "colony",
        }
    }

    pub(super) fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "cluster" => Self::Cluster,
            "patch" => Self::Patch,
            "colony" => Self::Colony,
            _ => return None,
        })
    }
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
    PersistentMutation,
    /// Ecology/gameplay query input.
    EcologyGameplay,
    /// Read-only diagnostic output.
    Diagnostics,
}

impl GraphSink {
    pub(super) fn requires_authority(self) -> bool {
        self != Self::Diagnostics
    }

    pub(super) const fn expected_domain(self) -> GraphDomain {
        match self {
            Self::Macro
            | Self::CollisionNavigation
            | Self::PersistentMutation
            | Self::EcologyGameplay => GraphDomain::MacroPoints,
            Self::Micro => GraphDomain::MicroField,
            Self::Diagnostics => GraphDomain::Diagnostics,
        }
    }

    pub(super) fn from_wire(value: &str) -> Option<Self> {
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
