//! The native botanical graph: a typed authoring IR that grows a plant family.
//!
//! A `.splant` carries exactly one source. An imported family is a recipe of references to external
//! geometry; a native family is this graph. Both normalize to the same compiled family, so nothing
//! downstream — cooker, renderer, wind, collision, navigation, lifecycle — can tell which one it
//! came from.
//!
//! The type system here is botanical and deliberately shares no pin domain, operator name, or JSON
//! shape with the biome graph. A biome graph decides *where plants go*; this one decides *what one
//! plant is*. Confusing the two would let a biome operator land inside a trunk.
//!
//! Everything is deterministic. Positions and radii are Q15.16 metres, angles are signed normalized
//! half-turns, and every stochastic choice draws from a counter-based stream keyed by (graph, node,
//! element ordinal, channel) — so the same graph grows the same plant on any machine, in any order,
//! and adding a node cannot perturb an unrelated one's variation.

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionCurve, DecisionScalar, RandomDomain, RandomStream, UnitInterval};

use crate::{
    BotanicalEditAction, BotanicalEditDiagnostics, BotanicalManualEdit, ContentHash, Error,
    PlantPartSemantic, Result, apply_manual_edits, validate_manual_edits,
};

/// Current botanical operator schema version.
pub const BOTANICAL_NODE_VERSION: u32 = 1;

/// What a botanical pin carries between nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BotanicalDomain {
    /// Directed skeleton curves with stable element identity — trunks, branches, roots, vines.
    Spines,
    /// Oriented attachment frames sitting on spines, where instanced elements go.
    Frames,
    /// Swept surface shells with a material slot.
    Shells,
    /// Placed instanced elements — leaves, needles, blades, flowers, fruit, buds, scars.
    Elements,
}

/// A semantic element class the graph produces, and the part semantic it compiles to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BotanicalElement {
    /// Primary stem.
    Trunk,
    /// Secondary stem.
    Branch,
    /// Below-ground structure.
    Root,
    /// Climbing stem.
    Vine,
    /// Frond blade.
    Frond,
    /// Broad leaf.
    Leaf,
    /// Needle.
    Needle,
    /// Grass or reed blade.
    Blade,
    /// Flower.
    Flower,
    /// Fruit or seed body.
    Fruit,
    /// Dormant bud.
    Bud,
    /// Bark scar left by a shed or broken part.
    Scar,
    /// Dead or broken material still attached.
    DeadPart,
}

impl BotanicalElement {
    /// The compiled part semantic this class normalizes to.
    ///
    /// Several botanical classes share one part semantic: a needle and a broad leaf differ in how
    /// they are authored and instanced, not in what the runtime does with them.
    #[must_use]
    pub const fn part_semantic(self) -> PlantPartSemantic {
        match self {
            Self::Trunk => PlantPartSemantic::Trunk,
            Self::Branch | Self::Vine | Self::DeadPart => PlantPartSemantic::Branch,
            Self::Root => PlantPartSemantic::Root,
            Self::Frond => PlantPartSemantic::Frond,
            Self::Leaf | Self::Needle | Self::Bud | Self::Scar => PlantPartSemantic::Leaf,
            Self::Blade => PlantPartSemantic::Blade,
            Self::Flower => PlantPartSemantic::Flower,
            Self::Fruit => PlantPartSemantic::Fruit,
        }
    }

    /// Whether the class is a woody or fleshy axis that carries a spine.
    #[must_use]
    pub const fn is_axis(self) -> bool {
        matches!(self, Self::Trunk | Self::Branch | Self::Root | Self::Vine)
    }

    /// Canonical wire tag.
    #[must_use]
    pub const fn tag(self) -> u32 {
        match self {
            Self::Trunk => 0,
            Self::Branch => 1,
            Self::Root => 2,
            Self::Vine => 3,
            Self::Frond => 4,
            Self::Leaf => 5,
            Self::Needle => 6,
            Self::Blade => 7,
            Self::Flower => 8,
            Self::Fruit => 9,
            Self::Bud => 10,
            Self::Scar => 11,
            Self::DeadPart => 12,
        }
    }
}

impl TryFrom<u32> for BotanicalElement {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            0 => Self::Trunk,
            1 => Self::Branch,
            2 => Self::Root,
            3 => Self::Vine,
            4 => Self::Frond,
            5 => Self::Leaf,
            6 => Self::Needle,
            7 => Self::Blade,
            8 => Self::Flower,
            9 => Self::Fruit,
            10 => Self::Bud,
            11 => Self::Scar,
            12 => Self::DeadPart,
            _ => {
                return Err(Error::ArtifactFormat {
                    format: "botanical graph",
                    field: "element".to_owned(),
                });
            }
        })
    }
}

/// How child attachments are arranged around a parent axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhyllotaxisPattern {
    /// One attachment per node, alternating sides.
    Alternate,
    /// Two attachments per node, opposite each other.
    Opposite,
    /// `count` attachments evenly spaced at each node.
    Whorled,
    /// A continuous spiral at the declared divergence angle.
    Spiral,
}

impl PhyllotaxisPattern {
    /// Canonical wire tag.
    #[must_use]
    pub const fn tag(self) -> u32 {
        match self {
            Self::Alternate => 0,
            Self::Opposite => 1,
            Self::Whorled => 2,
            Self::Spiral => 3,
        }
    }
}

impl TryFrom<u32> for PhyllotaxisPattern {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            0 => Self::Alternate,
            1 => Self::Opposite,
            2 => Self::Whorled,
            3 => Self::Spiral,
            _ => {
                return Err(Error::ArtifactFormat {
                    format: "botanical graph",
                    field: "phyllotaxis.pattern".to_owned(),
                });
            }
        })
    }
}

/// Which way a tropism bends an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TropismKind {
    /// Toward light: upward, away from the shading crown.
    Phototropism,
    /// With gravity: the droop of a loaded branch.
    Gravitropism,
    /// Away from an obstacle plane.
    Thigmotropism,
}

impl TropismKind {
    /// Canonical wire tag.
    #[must_use]
    pub const fn tag(self) -> u32 {
        match self {
            Self::Phototropism => 0,
            Self::Gravitropism => 1,
            Self::Thigmotropism => 2,
        }
    }
}

impl TryFrom<u32> for TropismKind {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            0 => Self::Phototropism,
            1 => Self::Gravitropism,
            2 => Self::Thigmotropism,
            _ => {
                return Err(Error::ArtifactFormat {
                    format: "botanical graph",
                    field: "tropism.kind".to_owned(),
                });
            }
        })
    }
}

/// Which axes a prune rule removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneRule {
    /// Remove axes whose base sits below a height, the way a forest tree sheds low branches.
    BelowHeight,
    /// Remove axes shorter than a length, clearing runts.
    ShorterThan,
    /// Keep at most `count` axes per parent, the strongest first.
    KeepStrongest,
}

impl PruneRule {
    /// Canonical wire tag.
    #[must_use]
    pub const fn tag(self) -> u32 {
        match self {
            Self::BelowHeight => 0,
            Self::ShorterThan => 1,
            Self::KeepStrongest => 2,
        }
    }
}

impl TryFrom<u32> for PruneRule {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            0 => Self::BelowHeight,
            1 => Self::ShorterThan,
            2 => Self::KeepStrongest,
            _ => {
                return Err(Error::ArtifactFormat {
                    format: "botanical graph",
                    field: "prune.rule".to_owned(),
                });
            }
        })
    }
}

/// Random channels, one per stochastic decision, so adding a rule cannot perturb another's stream.
mod channel {
    /// Jitter on a child axis's angle around its parent.
    pub const AZIMUTH: u32 = 1;
    /// Jitter on a child axis's declination from its parent.
    pub const DECLINATION: u32 = 2;
    /// Jitter on a child axis's length.
    pub const LENGTH: u32 = 3;
    /// Jitter on an instanced element's roll.
    pub const ROLL: u32 = 4;
}

/// One point of a hand-drawn spine: where the curve passes, and how thick it is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotanicalDrawnPoint {
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Radius at that position.
    pub radius: DecisionScalar,
}

/// One typed botanical operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BotanicalOperator {
    /// A spine an artist drew, point by point.
    ///
    /// Generators produce plants that follow rules; a hero silhouette often does not. A drawn spine
    /// enters the graph as an axis source exactly like a grown one, so everything downstream —
    /// phyllotaxis, shells, tropism, pruning — treats it as ordinary botanical structure.
    Drawn {
        /// Element class the spine carries.
        element: BotanicalElement,
        /// The drawn curve, base first.
        points: Vec<BotanicalDrawnPoint>,
    },
    /// The plant's primary axis, grown from the ground up.
    Trunk {
        /// Element class the axis carries; a `Trunk` for a tree, a `Blade` for grass.
        element: BotanicalElement,
        /// Total length in metres.
        length: DecisionScalar,
        /// Radius at the base.
        base_radius: DecisionScalar,
        /// Radius as a fraction of the base along the axis, sampled at each segment.
        taper: DecisionCurve,
        /// Segments along the axis; more segments bend more smoothly and cost more.
        segments: u32,
    },
    /// Child axes grown from the frames of a parent axis.
    Branch {
        /// Element class the child axes carry.
        element: BotanicalElement,
        /// Child length as a fraction of the parent's.
        length_ratio: UnitInterval,
        /// Child base radius as a fraction of the parent's radius at the attachment.
        radius_ratio: UnitInterval,
        /// Declination from the parent axis, as a signed normalized half-turn.
        declination: UnitInterval,
        /// How much the declination and length may vary per child.
        jitter: UnitInterval,
        /// Segments per child axis.
        segments: u32,
    },
    /// Attachment frames placed along an axis.
    Phyllotaxis {
        /// Arrangement around the axis.
        pattern: PhyllotaxisPattern,
        /// Attachments per node, for `Whorled`.
        count: u32,
        /// Nodes along the axis.
        nodes: u32,
        /// Where along the axis the first node sits.
        start: UnitInterval,
        /// Where along the axis the last node sits.
        end: UnitInterval,
        /// Turn between successive attachments, as a signed normalized half-turn.
        divergence: UnitInterval,
    },
    /// Bends axes toward or away from a direction.
    Tropism {
        /// Which way it bends.
        kind: TropismKind,
        /// How strongly, accumulated along the axis.
        strength: UnitInterval,
    },
    /// Removes axes by rule.
    Prune {
        /// Which axes go.
        rule: PruneRule,
        /// Height for `BelowHeight`, length for `ShorterThan`, unused otherwise.
        threshold: DecisionScalar,
        /// Retained count for `KeepStrongest`.
        count: u32,
    },
    /// Mirrors an axis system below ground as roots.
    Roots {
        /// Depth as a fraction of the source axis's length.
        depth_ratio: UnitInterval,
        /// Spread as a fraction of that depth.
        spread_ratio: UnitInterval,
        /// Root axes.
        count: u32,
    },
    /// Sweeps axes into surface shells.
    Shell {
        /// Material slot the shells bind.
        material_slot: u32,
        /// Cross-section sides; three is a ribbon, more is a tube.
        sides: u32,
    },
    /// Places instanced elements at frames.
    Instance {
        /// Element class placed.
        element: BotanicalElement,
        /// Material slot the elements bind.
        material_slot: u32,
        /// Element size in metres.
        size: DecisionScalar,
        /// How much the roll may vary per element.
        jitter: UnitInterval,
    },
    /// The single family output. Every shell and element reaching it is in the compiled family.
    Family,
}

impl BotanicalOperator {
    /// Stable operator name, used in diagnostics and the canonical document.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Drawn { .. } => "drawn",
            Self::Trunk { .. } => "trunk",
            Self::Branch { .. } => "branch",
            Self::Phyllotaxis { .. } => "phyllotaxis",
            Self::Tropism { .. } => "tropism",
            Self::Prune { .. } => "prune",
            Self::Roots { .. } => "roots",
            Self::Shell { .. } => "shell",
            Self::Instance { .. } => "instance",
            Self::Family => "family",
        }
    }

    /// Typed input pins, in canonical order.
    #[must_use]
    pub fn inputs(&self) -> &'static [(&'static str, BotanicalDomain)] {
        match self {
            Self::Drawn { .. } | Self::Trunk { .. } => &[],
            Self::Branch { .. } => &[("frames", BotanicalDomain::Frames)],
            Self::Phyllotaxis { .. } | Self::Tropism { .. } | Self::Prune { .. } => {
                &[("axes", BotanicalDomain::Spines)]
            }
            Self::Roots { .. } | Self::Shell { .. } => &[("axes", BotanicalDomain::Spines)],
            Self::Instance { .. } => &[("frames", BotanicalDomain::Frames)],
            Self::Family => &[
                ("shells", BotanicalDomain::Shells),
                ("elements", BotanicalDomain::Elements),
            ],
        }
    }

    /// Typed output pins, in canonical order.
    #[must_use]
    pub fn outputs(&self) -> &'static [(&'static str, BotanicalDomain)] {
        match self {
            Self::Drawn { .. } | Self::Trunk { .. } | Self::Branch { .. } | Self::Roots { .. } => {
                &[("axes", BotanicalDomain::Spines)]
            }
            Self::Tropism { .. } | Self::Prune { .. } => &[("axes", BotanicalDomain::Spines)],
            Self::Phyllotaxis { .. } => &[("frames", BotanicalDomain::Frames)],
            Self::Shell { .. } => &[("shells", BotanicalDomain::Shells)],
            Self::Instance { .. } => &[("elements", BotanicalDomain::Elements)],
            Self::Family => &[],
        }
    }
}

/// One node of a botanical graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalNode {
    /// Stable node GUID. Manual edits and diagnostics address elements through it, so it outlives
    /// parameter changes.
    pub guid: u128,
    /// Operator schema version the node was authored against.
    pub version: u32,
    /// Bumped only when this node's own semantics change, which is what invalidates its subtree.
    pub semantic_revision: u32,
    /// The typed operation.
    pub operator: BotanicalOperator,
}

/// One directed typed edge between botanical nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BotanicalEdge {
    /// Source node.
    pub from_node: u128,
    /// Source output pin.
    pub from_pin: String,
    /// Destination node.
    pub to_node: u128,
    /// Destination input pin.
    pub to_pin: String,
}

/// Variations one document may carry. A bound, not a budget: every variation is its own grown
/// geometry in the compiled family, so the ceiling is what keeps a family's size predictable.
pub const MAX_VARIATIONS: usize = 16;

/// One individual the graph grows: a seed, an intrinsic age, and a name.
///
/// A family's variations are the same species at different draws and different ages. The seed picks
/// which individual; the age scales it continuously, which is how one family carries a seedling, a
/// sapling, and a mature tree without three graphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalVariation {
    /// Seed for every stochastic decision. Changing it regrows a different individual.
    pub seed: u128,
    /// Intrinsic age, where one is the fully grown plant the graph's parameters describe.
    pub age: UnitInterval,
    /// Artist-facing name.
    pub name: String,
}

/// A native botanical graph document: the authored source of a native plant family.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalGraphDocument {
    /// Nodes in canonical GUID order.
    pub nodes: Vec<BotanicalNode>,
    /// Edges in canonical order.
    pub edges: Vec<BotanicalEdge>,
    /// The individuals the graph grows, in authored order. The first is the representative one, and
    /// each becomes a family variation the runtime can select.
    pub variations: Vec<BotanicalVariation>,
    /// Manual edits laid over what the nodes grow, in canonical target order.
    ///
    /// Element identities do not depend on the seed or the age, so one layer applies to every
    /// variation: a leaf nudged on the mature tree is nudged on the sapling too.
    pub edits: Vec<BotanicalManualEdit>,
}

impl BotanicalGraphDocument {
    /// The graph a new native family starts from: a tapering trunk swept into bark, leaves on
    /// spiral frames up its length, and roots below.
    ///
    /// This is what the authoring surface hands an artist when they create a plant — a whole small
    /// tree they can immediately grow, preview, and edit, rather than an empty canvas.
    #[must_use]
    pub fn sapling(seed: u128) -> Self {
        let node = |guid: u128, operator: BotanicalOperator| BotanicalNode {
            guid,
            version: BOTANICAL_NODE_VERSION,
            semantic_revision: 1,
            operator,
        };
        let edge = |from: u128, from_pin: &str, to: u128, to_pin: &str| BotanicalEdge {
            from_node: from,
            from_pin: from_pin.to_owned(),
            to_node: to,
            to_pin: to_pin.to_owned(),
        };
        let mut document = Self {
            variations: vec![BotanicalVariation {
                seed,
                age: UnitInterval::ONE,
                name: "Mature".to_owned(),
            }],
            edits: Vec::new(),
            nodes: vec![
                node(
                    1,
                    BotanicalOperator::Trunk {
                        element: BotanicalElement::Trunk,
                        length: DecisionScalar::from_bits(4 << 16),
                        base_radius: DecisionScalar::from_bits(9_830),
                        taper: linear_taper(),
                        segments: 6,
                    },
                ),
                node(
                    2,
                    BotanicalOperator::Phyllotaxis {
                        pattern: PhyllotaxisPattern::Spiral,
                        count: 1,
                        nodes: 5,
                        start: UnitInterval::from_bits(26_000),
                        end: UnitInterval::from_bits(62_000),
                        divergence: UnitInterval::from_bits(22_800),
                    },
                ),
                node(
                    3,
                    BotanicalOperator::Instance {
                        element: BotanicalElement::Leaf,
                        material_slot: 1,
                        size: DecisionScalar::from_bits(6_553),
                        jitter: UnitInterval::from_bits(24_000),
                    },
                ),
                node(
                    4,
                    BotanicalOperator::Roots {
                        depth_ratio: UnitInterval::from_bits(18_000),
                        spread_ratio: UnitInterval::from_bits(40_000),
                        count: 3,
                    },
                ),
                node(
                    5,
                    BotanicalOperator::Shell {
                        material_slot: 0,
                        sides: 6,
                    },
                ),
                node(6, BotanicalOperator::Family),
            ],
            edges: vec![
                edge(1, "axes", 2, "axes"),
                edge(1, "axes", 4, "axes"),
                edge(1, "axes", 5, "axes"),
                edge(2, "frames", 3, "frames"),
                edge(3, "elements", 6, "elements"),
                edge(4, "axes", 5, "axes"),
                edge(5, "shells", 6, "shells"),
            ],
        };
        document.edges.sort();
        document
    }

    /// The node with `guid`.
    #[must_use]
    pub fn node(&self, guid: u128) -> Option<&BotanicalNode> {
        self.nodes.iter().find(|node| node.guid == guid)
    }

    /// The single `Family` sink.
    ///
    /// # Errors
    ///
    /// [`Error::ArtifactFormat`] unless exactly one exists: a family with two outputs has no
    /// defined compiled result, and one with none produces nothing.
    pub fn family_node(&self) -> Result<&BotanicalNode> {
        let mut found = self
            .nodes
            .iter()
            .filter(|node| matches!(node.operator, BotanicalOperator::Family));
        let first = found.next().ok_or_else(|| Error::ArtifactFormat {
            format: "botanical graph",
            field: "family".to_owned(),
        })?;
        if found.next().is_some() {
            return Err(Error::ArtifactFormat {
                format: "botanical graph",
                field: "family.duplicate".to_owned(),
            });
        }
        Ok(first)
    }

    /// Canonical bytes: the graph's exact identity, and the cache key its compiled family hangs on.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = b"saffron-anima/botanical-graph/v1".to_vec();
        bytes.extend_from_slice(&(self.variations.len() as u64).to_be_bytes());
        for variation in &self.variations {
            bytes.extend_from_slice(&variation.seed.to_be_bytes());
            push_unit(&mut bytes, variation.age);
            bytes.extend_from_slice(&(variation.name.len() as u64).to_be_bytes());
            bytes.extend_from_slice(variation.name.as_bytes());
        }
        bytes.extend_from_slice(&(self.nodes.len() as u64).to_be_bytes());
        for node in &self.nodes {
            bytes.extend_from_slice(&node.guid.to_be_bytes());
            bytes.extend_from_slice(&node.version.to_be_bytes());
            bytes.extend_from_slice(&node.semantic_revision.to_be_bytes());
            bytes.extend_from_slice(node.operator.name().as_bytes());
            push_operator(&mut bytes, &node.operator);
        }
        bytes.extend_from_slice(&(self.edges.len() as u64).to_be_bytes());
        for edge in &self.edges {
            bytes.extend_from_slice(&edge.from_node.to_be_bytes());
            bytes.extend_from_slice(edge.from_pin.as_bytes());
            bytes.extend_from_slice(&edge.to_node.to_be_bytes());
            bytes.extend_from_slice(edge.to_pin.as_bytes());
        }
        // Edits change the compiled family, so they are part of the graph's identity: a document
        // that differs only by a hand offset is a different plant.
        bytes.extend_from_slice(&(self.edits.len() as u64).to_be_bytes());
        for edit in &self.edits {
            bytes.extend_from_slice(&edit.target.value().to_be_bytes());
            bytes.extend_from_slice(&edit.action.tag().to_be_bytes());
            match &edit.action {
                BotanicalEditAction::Transform {
                    offset,
                    roll,
                    scale,
                } => {
                    for component in offset {
                        push_scalar(&mut bytes, *component);
                    }
                    push_unit(&mut bytes, *roll);
                    push_scalar(&mut bytes, *scale);
                }
                BotanicalEditAction::Trim { at } => push_unit(&mut bytes, *at),
                BotanicalEditAction::Remove => {}
                BotanicalEditAction::Graft { source, selector } => {
                    bytes.extend_from_slice(&source.to_be_bytes());
                    push_selector(&mut bytes, selector);
                }
            }
        }
        bytes
    }

    /// Exact content identity of the graph.
    #[must_use]
    pub fn identity(&self) -> ContentHash {
        ContentHash::of(&self.canonical_bytes())
    }

    /// Validates structure: canonical ordering, unique GUIDs, typed pins, one family sink, and no
    /// cycles.
    ///
    /// # Errors
    ///
    /// [`Error::ArtifactFormat`] naming the exact field that failed.
    pub fn validate(&self) -> Result<()> {
        let field = |name: &str| Error::ArtifactFormat {
            format: "botanical graph",
            field: name.to_owned(),
        };
        if self.nodes.is_empty() {
            return Err(field("nodes"));
        }
        let mut guids = BTreeSet::new();
        for pair in self.nodes.windows(2) {
            if pair[0].guid >= pair[1].guid {
                return Err(field("nodes.order"));
            }
        }
        for node in &self.nodes {
            if node.guid == 0 || !guids.insert(node.guid) {
                return Err(field("nodes.guid"));
            }
            if node.version != BOTANICAL_NODE_VERSION {
                return Err(field("nodes.version"));
            }
            validate_operator(&node.operator)?;
        }
        for pair in self.edges.windows(2) {
            if pair[0] >= pair[1] {
                return Err(field("edges.order"));
            }
        }
        for edge in &self.edges {
            let from = self
                .node(edge.from_node)
                .ok_or_else(|| field("edges.from"))?;
            let to = self.node(edge.to_node).ok_or_else(|| field("edges.to"))?;
            let source = from
                .operator
                .outputs()
                .iter()
                .find(|(name, _)| *name == edge.from_pin)
                .ok_or_else(|| field("edges.fromPin"))?;
            let sink = to
                .operator
                .inputs()
                .iter()
                .find(|(name, _)| *name == edge.to_pin)
                .ok_or_else(|| field("edges.toPin"))?;
            if source.1 != sink.1 {
                return Err(field("edges.domain"));
            }
        }
        self.family_node()?;
        if self.variations.is_empty() || self.variations.len() > MAX_VARIATIONS {
            return Err(field("variations"));
        }
        let mut seeds = BTreeSet::new();
        for variation in &self.variations {
            // Two variations drawing the same seed at the same age are the same individual twice,
            // which doubles the family's geometry for nothing.
            if !seeds.insert((variation.seed, variation.age.bits())) {
                return Err(field("variations.duplicate"));
            }
            if variation.age.bits() == 0 {
                return Err(field("variations.age"));
            }
            if variation.name.trim().is_empty() {
                return Err(field("variations.name"));
            }
        }
        validate_manual_edits(&self.edits)?;
        self.topological_order().map(|_| ())
    }

    /// Node GUIDs in evaluation order.
    ///
    /// # Errors
    ///
    /// [`Error::ArtifactFormat`] when the graph has a cycle — a plant cannot grow from itself.
    pub fn topological_order(&self) -> Result<Vec<u128>> {
        let mut incoming: BTreeMap<u128, usize> =
            self.nodes.iter().map(|node| (node.guid, 0)).collect();
        for edge in &self.edges {
            *incoming.entry(edge.to_node).or_default() += 1;
        }
        // Canonical GUID order among ready nodes, so the evaluation order is reproducible.
        let mut ready: BTreeSet<u128> = incoming
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(guid, _)| *guid)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(&guid) = ready.iter().next() {
            ready.remove(&guid);
            order.push(guid);
            for edge in self.edges.iter().filter(|edge| edge.from_node == guid) {
                let count = incoming.entry(edge.to_node).or_default();
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(edge.to_node);
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err(Error::ArtifactFormat {
                format: "botanical graph",
                field: "cycle".to_owned(),
            });
        }
        Ok(order)
    }
}

fn push_scalar(bytes: &mut Vec<u8>, value: DecisionScalar) {
    bytes.extend_from_slice(&value.canonical_bytes());
}

fn push_unit(bytes: &mut Vec<u8>, value: UnitInterval) {
    bytes.extend_from_slice(&value.canonical_bytes());
}

fn push_selector(bytes: &mut Vec<u8>, selector: &crate::PlantSourceSelector) {
    match selector {
        crate::PlantSourceSelector::Whole => bytes.push(0),
        crate::PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(&(path.len() as u64).to_be_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        crate::PlantSourceSelector::Submesh { element, index } => {
            bytes.push(2);
            bytes.extend_from_slice(&element.to_be_bytes());
            bytes.extend_from_slice(&index.to_be_bytes());
        }
    }
}

fn push_curve(bytes: &mut Vec<u8>, curve: &DecisionCurve) {
    bytes.extend_from_slice(&(curve.points().len() as u64).to_be_bytes());
    for (at, value) in curve.points() {
        push_unit(bytes, *at);
        push_scalar(bytes, *value);
    }
}

fn push_operator(bytes: &mut Vec<u8>, operator: &BotanicalOperator) {
    match operator {
        BotanicalOperator::Drawn { element, points } => {
            bytes.extend_from_slice(&element.tag().to_be_bytes());
            bytes.extend_from_slice(&(points.len() as u64).to_be_bytes());
            for point in points {
                for component in point.position {
                    push_scalar(bytes, component);
                }
                push_scalar(bytes, point.radius);
            }
        }
        BotanicalOperator::Trunk {
            element,
            length,
            base_radius,
            taper,
            segments,
        } => {
            bytes.extend_from_slice(&element.tag().to_be_bytes());
            push_scalar(bytes, *length);
            push_scalar(bytes, *base_radius);
            push_curve(bytes, taper);
            bytes.extend_from_slice(&segments.to_be_bytes());
        }
        BotanicalOperator::Branch {
            element,
            length_ratio,
            radius_ratio,
            declination,
            jitter,
            segments,
        } => {
            bytes.extend_from_slice(&element.tag().to_be_bytes());
            for value in [length_ratio, radius_ratio, declination, jitter] {
                push_unit(bytes, *value);
            }
            bytes.extend_from_slice(&segments.to_be_bytes());
        }
        BotanicalOperator::Phyllotaxis {
            pattern,
            count,
            nodes,
            start,
            end,
            divergence,
        } => {
            bytes.extend_from_slice(&pattern.tag().to_be_bytes());
            bytes.extend_from_slice(&count.to_be_bytes());
            bytes.extend_from_slice(&nodes.to_be_bytes());
            for value in [start, end, divergence] {
                push_unit(bytes, *value);
            }
        }
        BotanicalOperator::Tropism { kind, strength } => {
            bytes.extend_from_slice(&kind.tag().to_be_bytes());
            push_unit(bytes, *strength);
        }
        BotanicalOperator::Prune {
            rule,
            threshold,
            count,
        } => {
            bytes.extend_from_slice(&rule.tag().to_be_bytes());
            push_scalar(bytes, *threshold);
            bytes.extend_from_slice(&count.to_be_bytes());
        }
        BotanicalOperator::Roots {
            depth_ratio,
            spread_ratio,
            count,
        } => {
            push_unit(bytes, *depth_ratio);
            push_unit(bytes, *spread_ratio);
            bytes.extend_from_slice(&count.to_be_bytes());
        }
        BotanicalOperator::Shell {
            material_slot,
            sides,
        } => {
            bytes.extend_from_slice(&material_slot.to_be_bytes());
            bytes.extend_from_slice(&sides.to_be_bytes());
        }
        BotanicalOperator::Instance {
            element,
            material_slot,
            size,
            jitter,
        } => {
            bytes.extend_from_slice(&element.tag().to_be_bytes());
            bytes.extend_from_slice(&material_slot.to_be_bytes());
            push_scalar(bytes, *size);
            push_unit(bytes, *jitter);
        }
        BotanicalOperator::Family => {}
    }
}

fn validate_operator(operator: &BotanicalOperator) -> Result<()> {
    let field = |name: &str| Error::ArtifactFormat {
        format: "botanical graph",
        field: name.to_owned(),
    };
    match operator {
        BotanicalOperator::Drawn { element, points } => {
            if !element.is_axis() {
                return Err(field("drawn.element"));
            }
            if points.len() < 2 || points.len() > MAX_SEGMENTS as usize + 1 {
                return Err(field("drawn.points"));
            }
            if points.iter().any(|point| point.radius.bits() <= 0) {
                return Err(field("drawn.radius"));
            }
            // A drawn spine whose points repeat has a zero-length segment, which sweeps into
            // degenerate geometry rather than a surface.
            if points
                .windows(2)
                .any(|pair| pair[0].position == pair[1].position)
            {
                return Err(field("drawn.degenerate"));
            }
        }
        BotanicalOperator::Trunk {
            element,
            length,
            base_radius,
            taper,
            segments,
        } => {
            if !element.is_axis() {
                return Err(field("trunk.element"));
            }
            if length.bits() <= 0 || base_radius.bits() <= 0 {
                return Err(field("trunk.length"));
            }
            if *segments == 0 || *segments > MAX_SEGMENTS {
                return Err(field("trunk.segments"));
            }
            DecisionCurve::validate_points(taper.points())?;
        }
        BotanicalOperator::Branch {
            element, segments, ..
        } => {
            if !element.is_axis() {
                return Err(field("branch.element"));
            }
            if *segments == 0 || *segments > MAX_SEGMENTS {
                return Err(field("branch.segments"));
            }
        }
        BotanicalOperator::Phyllotaxis {
            pattern,
            count,
            nodes,
            start,
            end,
            ..
        } => {
            if *nodes == 0 || *nodes > MAX_NODES {
                return Err(field("phyllotaxis.nodes"));
            }
            if matches!(pattern, PhyllotaxisPattern::Whorled) && (*count < 2 || *count > MAX_WHORL)
            {
                return Err(field("phyllotaxis.count"));
            }
            if start.bits() > end.bits() {
                return Err(field("phyllotaxis.range"));
            }
        }
        BotanicalOperator::Prune { rule, count, .. } => {
            if matches!(rule, PruneRule::KeepStrongest) && *count == 0 {
                return Err(field("prune.count"));
            }
        }
        BotanicalOperator::Roots { count, .. } => {
            if *count == 0 || *count > MAX_WHORL {
                return Err(field("roots.count"));
            }
        }
        BotanicalOperator::Shell { sides, .. } => {
            if *sides < 3 || *sides > MAX_SIDES {
                return Err(field("shell.sides"));
            }
        }
        BotanicalOperator::Instance { size, .. } => {
            if size.bits() <= 0 {
                return Err(field("instance.size"));
            }
        }
        BotanicalOperator::Family | BotanicalOperator::Tropism { .. } => {}
    }
    Ok(())
}

/// Segment ceiling per axis. A bound, not a budget: an authored graph that asks for more is a
/// mistake caught at validation rather than a multi-gigabyte compile.
pub const MAX_SEGMENTS: u32 = 64;
/// Attachment-node ceiling per axis.
pub const MAX_NODES: u32 = 64;
/// Ceiling on attachments per node, and on root axes.
pub const MAX_WHORL: u32 = 16;
/// Cross-section side ceiling.
pub const MAX_SIDES: u32 = 32;
/// Ceiling on axes one graph may grow, across every node.
pub const MAX_AXES: usize = 4_096;

/// Stable identity of one grown element.
///
/// Derived from the producing node, the parent element, and the ordinal within that parent — never
/// from a global counter. A manual edit keyed to this identity survives any parameter change that
/// leaves its ancestry and ordinal intact, and is reported orphaned when one does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BotanicalElementId(u128);

impl BotanicalElementId {
    /// The root identity of a node's own output.
    #[must_use]
    pub fn root(node: u128, ordinal: u32) -> Self {
        Self(mix(mix(node, 0), u128::from(ordinal)))
    }

    /// The identity of `ordinal`'s child of `self`, produced by `node`.
    #[must_use]
    pub fn child(self, node: u128, ordinal: u32) -> Self {
        Self(mix(mix(self.0, node), u128::from(ordinal)))
    }

    /// The raw identity.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }

    /// The identity a raw value names, as an authored edit or a wire payload spells it.
    #[must_use]
    pub const fn from_value(value: u128) -> Self {
        Self(value)
    }
}

/// A stable, order-independent mix of two identities.
fn mix(left: u128, right: u128) -> u128 {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(&left.to_be_bytes());
    bytes[16..].copy_from_slice(&right.to_be_bytes());
    let hash = ContentHash::of(&bytes).bytes();
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&hash[..16]);
    u128::from_be_bytes(leading)
}

/// One grown axis: a directed curve of rest points with a radius at each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalAxis {
    /// Stable identity.
    pub id: BotanicalElementId,
    /// Parent axis, absent for a trunk.
    pub parent: Option<BotanicalElementId>,
    /// Attachment frame it grew from, absent for a trunk, a drawn spine, or a root.
    ///
    /// A cut through the parent takes everything attached above it, and the frame is what says
    /// where along the parent this axis sits.
    pub frame: Option<BotanicalElementId>,
    /// Element class.
    pub element: BotanicalElement,
    /// Rest points in family-local metres, base first.
    pub points: Vec<[DecisionScalar; 3]>,
    /// Radius at each rest point.
    pub radii: Vec<DecisionScalar>,
}

impl BotanicalAxis {
    /// Straight-line length from base to tip.
    #[must_use]
    pub fn length(&self) -> DecisionScalar {
        let (Some(base), Some(tip)) = (self.points.first(), self.points.last()) else {
            return DecisionScalar::from_bits(0);
        };
        let squared: i64 = (0..3)
            .map(|axis| {
                let delta = i64::from(tip[axis].bits() - base[axis].bits());
                delta * delta
            })
            .sum();
        DecisionScalar::from_bits(i32::try_from(isqrt(squared)).unwrap_or(i32::MAX))
    }

    /// Height of the axis base above the family origin.
    #[must_use]
    pub fn base_height(&self) -> DecisionScalar {
        self.points
            .first()
            .map_or(DecisionScalar::from_bits(0), |point| point[1])
    }
}

/// One oriented attachment frame on an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotanicalFrame {
    /// Stable identity.
    pub id: BotanicalElementId,
    /// The axis it sits on.
    pub axis: BotanicalElementId,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Outward direction, unit-scaled in Q15.16.
    pub direction: [DecisionScalar; 3],
    /// Radius of the parent axis at this point, which sizes what attaches here.
    pub radius: DecisionScalar,
    /// Where along the axis it sits.
    pub along: UnitInterval,
}

/// One swept surface shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalShell {
    /// Stable identity.
    pub id: BotanicalElementId,
    /// The axis it sweeps.
    pub axis: BotanicalElementId,
    /// Element class of that axis.
    pub element: BotanicalElement,
    /// Material slot.
    pub material_slot: u32,
    /// Cross-section sides.
    pub sides: u32,
}

/// One placed instanced element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotanicalPlacement {
    /// Stable identity.
    pub id: BotanicalElementId,
    /// The frame it sits on.
    pub frame: BotanicalElementId,
    /// Element class.
    pub element: BotanicalElement,
    /// Material slot.
    pub material_slot: u32,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Size in metres.
    pub size: DecisionScalar,
    /// Roll about the frame direction, as a signed normalized half-turn.
    pub roll: UnitInterval,
}

/// One hand-modelled mesh standing in for a generated element.
///
/// The graft keeps the element's identity and its frame, so the plant's structure is unchanged and
/// only the surface differs. Its geometry comes from an external source resolved through the same
/// importer an imported family uses — there is no native-only mesh path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalGraft {
    /// Identity of the element it replaced.
    pub id: BotanicalElementId,
    /// The frame it stands on.
    pub frame: BotanicalElementId,
    /// The family graft source supplying the geometry.
    pub source: u128,
    /// Which of that source's elements to take.
    pub selector: crate::PlantSourceSelector,
    /// Position in family-local metres.
    pub position: [DecisionScalar; 3],
    /// Roll about the frame direction, as a signed normalized half-turn.
    pub roll: UnitInterval,
}

/// Everything one graph grew, in canonical identity order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalAssembly {
    /// Axes, trunks first by construction order then canonical identity.
    pub axes: Vec<BotanicalAxis>,
    /// Attachment frames.
    pub frames: Vec<BotanicalFrame>,
    /// Swept shells reaching the family output.
    pub shells: Vec<BotanicalShell>,
    /// Instanced elements reaching the family output.
    pub elements: Vec<BotanicalPlacement>,
    /// Hand-modelled meshes standing in for elements the edit layer grafted over.
    pub grafts: Vec<BotanicalGraft>,
}

impl BotanicalAssembly {
    /// Conservative local bounds over every axis point and placed element.
    #[must_use]
    pub fn local_bounds(&self) -> ([DecisionScalar; 3], [DecisionScalar; 3]) {
        let mut minimum = [i32::MAX; 3];
        let mut maximum = [i32::MIN; 3];
        let mut extend = |point: [DecisionScalar; 3], pad: i32| {
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(point[axis].bits().saturating_sub(pad));
                maximum[axis] = maximum[axis].max(point[axis].bits().saturating_add(pad));
            }
        };
        for axis in &self.axes {
            for (point, radius) in axis.points.iter().zip(&axis.radii) {
                extend(*point, radius.bits());
            }
        }
        for element in &self.elements {
            extend(element.position, element.size.bits());
        }
        if minimum[0] == i32::MAX {
            let zero = DecisionScalar::from_bits(0);
            return ([zero; 3], [zero; 3]);
        }
        (
            std::array::from_fn(|axis| DecisionScalar::from_bits(minimum[axis])),
            std::array::from_fn(|axis| DecisionScalar::from_bits(maximum[axis])),
        )
    }
}

/// Integer square root, so a length never depends on floating-point rounding.
fn isqrt(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut low = 0_i64;
    let mut high = 3_037_000_499_i64.min(value);
    while low < high {
        let mid = low + (high - low + 1) / 2;
        if mid.saturating_mul(mid) <= value {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// A grown plant and what its manual edit layer did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BotanicalGrowth {
    /// The plant, with every edit that found its target already applied.
    pub assembly: BotanicalAssembly,
    /// Edits that landed, and the ones that had nothing to land on.
    pub diagnostics: BotanicalEditDiagnostics,
}

/// Grows one of the plants a graph describes and lays its manual edits over the result.
///
/// `variation` indexes [`BotanicalGraphDocument::variations`]; zero is the representative
/// individual. The walk is a topological pass in canonical GUID order, and each node reads its
/// inputs from what its predecessors produced, so the assembly is a pure function of the document
/// and the index — the same graph grows the same plant on any machine.
///
/// # Errors
///
/// Propagates validation, fails when `variation` names no declared individual, and fails when the
/// graph would grow past [`MAX_AXES`].
pub fn grow(document: &BotanicalGraphDocument, variation: usize) -> Result<BotanicalGrowth> {
    let mut assembly = grow_nodes(document, variation)?;
    let diagnostics = apply_manual_edits(&mut assembly, &document.edits);
    Ok(BotanicalGrowth {
        assembly,
        diagnostics,
    })
}

/// Grows what the nodes alone describe, before any manual edit.
fn grow_nodes(document: &BotanicalGraphDocument, variation: usize) -> Result<BotanicalAssembly> {
    document.validate()?;
    let individual = document
        .variations
        .get(variation)
        .ok_or_else(|| Error::ArtifactFormat {
            format: "botanical graph",
            field: "variations.index".to_owned(),
        })?;
    let order = document.topological_order()?;
    let map = leading_u128(document.identity().bytes());
    // Age scales the plant continuously. Lengths and radii carry it; the structure the graph
    // describes does not change, so a young individual is the same plant seen earlier.
    let scaled_by_age = |value: DecisionScalar| -> i32 {
        i32::try_from(
            (i64::from(value.bits()) * i64::from(individual.age.bits()))
                / i64::from(UnitInterval::ONE.bits()),
        )
        .unwrap_or(i32::MAX)
    };
    // A length or a radius never reaches zero — a plant with no extent is not a young plant.
    let aged = |value: DecisionScalar| DecisionScalar::from_bits(scaled_by_age(value).max(1));
    // A position scales about the origin and keeps its sign, so a drawn curve stays the shape it
    // was drawn as.
    let aged_position = |value: DecisionScalar| DecisionScalar::from_bits(scaled_by_age(value));

    let mut axes_out: BTreeMap<u128, Vec<BotanicalAxis>> = BTreeMap::new();
    let mut frames_out: BTreeMap<u128, Vec<BotanicalFrame>> = BTreeMap::new();
    let mut shells_out: BTreeMap<u128, Vec<BotanicalShell>> = BTreeMap::new();
    let mut elements_out: BTreeMap<u128, Vec<BotanicalPlacement>> = BTreeMap::new();
    // A node that transforms axes hands back the same identities carrying different geometry, so an
    // axis is only ever collected from the node that fed it into the family — never rescanned from
    // every node's output, where the pre-transform copy would be indistinguishable.
    let mut family_axes: BTreeMap<BotanicalElementId, BotanicalAxis> = BTreeMap::new();
    let mut frame_axes: BTreeMap<BotanicalElementId, BotanicalAxis> = BTreeMap::new();
    let mut assembly = BotanicalAssembly::default();

    for guid in order {
        let node = document.node(guid).ok_or_else(|| Error::ArtifactFormat {
            format: "botanical graph",
            field: "nodes.guid".to_owned(),
        })?;
        let incoming = |pin: &str| -> Vec<u128> {
            document
                .edges
                .iter()
                .filter(|edge| edge.to_node == guid && edge.to_pin == pin)
                .map(|edge| edge.from_node)
                .collect()
        };
        let input_axes = |pin: &str| -> Vec<BotanicalAxis> {
            incoming(pin)
                .iter()
                .filter_map(|from| axes_out.get(from))
                .flatten()
                .cloned()
                .collect()
        };
        let input_frames = |pin: &str| -> Vec<BotanicalFrame> {
            incoming(pin)
                .iter()
                .filter_map(|from| frames_out.get(from))
                .flatten()
                .copied()
                .collect()
        };

        match &node.operator {
            BotanicalOperator::Drawn { element, points } => {
                axes_out.insert(
                    guid,
                    vec![BotanicalAxis {
                        id: BotanicalElementId::root(guid, 0),
                        parent: None,
                        frame: None,
                        element: *element,
                        points: points
                            .iter()
                            .map(|point| point.position.map(aged_position))
                            .collect(),
                        radii: points.iter().map(|point| aged(point.radius)).collect(),
                    }],
                );
            }
            BotanicalOperator::Trunk {
                element,
                length,
                base_radius,
                taper,
                segments,
            } => {
                axes_out.insert(
                    guid,
                    vec![straight_axis(
                        BotanicalElementId::root(guid, 0),
                        None,
                        *element,
                        [DecisionScalar::from_bits(0); 3],
                        [0, 1, 0],
                        aged(*length),
                        aged(*base_radius),
                        taper,
                        *segments,
                    )?],
                );
            }
            BotanicalOperator::Branch {
                element,
                length_ratio,
                radius_ratio,
                declination,
                jitter,
                segments,
            } => {
                let frames = input_frames("frames");
                let mut grown = Vec::new();
                for (ordinal, frame) in frames.iter().enumerate() {
                    let ordinal = u32::try_from(ordinal).map_err(|_| Error::NumericOverflow)?;
                    let stream = |ch: u32| {
                        RandomStream::new(RandomDomain {
                            map,
                            node_guid: guid,
                            node_semantic_revision: node.semantic_revision,
                            seed_namespace: individual.seed,
                            cell: saffron_spatial::WorldCellKey::base(0, 0, 0),
                            candidate: frame.id.value() as u64,
                            ancestor: 0,
                            species: 0,
                            channel: ch,
                        })
                    };
                    // Length and direction vary per child, keyed by the frame it grows from — so
                    // regrowing the same graph regrows the same branch, and adding a sibling
                    // node cannot shift it.
                    let vary = |channel: u32, base: i64| -> i64 {
                        if jitter.bits() == 0 {
                            return base;
                        }
                        let sample = i64::from(stream(channel).lane(0, 0) % 2_048) - 1_024;
                        base + base * sample * i64::from(jitter.bits())
                            / (1_024 * i64::from(UnitInterval::ONE.bits()))
                    };
                    let parent_length = i64::from(frame.radius.bits()).max(1);
                    let length = DecisionScalar::from_bits(
                        i32::try_from(
                            vary(
                                channel::LENGTH,
                                i64::from(frame.radius.bits())
                                    * i64::from(length_ratio.bits())
                                    * 24
                                    / i64::from(UnitInterval::ONE.bits()),
                            )
                            .max(parent_length),
                        )
                        .unwrap_or(i32::MAX),
                    );
                    let declination_bits =
                        vary(channel::DECLINATION, i64::from(declination.bits()));
                    let azimuth = stream(channel::AZIMUTH).lane(0, 1) % 4_096;
                    let direction =
                        cone_direction(frame.direction, declination_bits, i64::from(azimuth));
                    let radius = DecisionScalar::from_bits(
                        (i64::from(frame.radius.bits()) * i64::from(radius_ratio.bits())
                            / i64::from(UnitInterval::ONE.bits())) as i32,
                    );
                    let mut axis = straight_axis(
                        frame.id.child(guid, ordinal),
                        Some(frame.axis),
                        *element,
                        frame.position,
                        direction,
                        length,
                        radius.max(DecisionScalar::from_bits(1)),
                        &linear_taper(),
                        *segments,
                    )?;
                    axis.frame = Some(frame.id);
                    grown.push(axis);
                    if grown.len() > MAX_AXES {
                        return Err(Error::ArtifactFormat {
                            format: "botanical graph",
                            field: "branch.axes".to_owned(),
                        });
                    }
                }
                axes_out.insert(guid, grown);
            }
            BotanicalOperator::Phyllotaxis {
                pattern,
                count,
                nodes,
                start,
                end,
                divergence,
            } => {
                let axes = input_axes("axes");
                for axis in &axes {
                    frame_axes.insert(axis.id, axis.clone());
                }
                let mut frames = Vec::new();
                for axis in &axes {
                    let per_node = match pattern {
                        PhyllotaxisPattern::Alternate | PhyllotaxisPattern::Spiral => 1,
                        PhyllotaxisPattern::Opposite => 2,
                        PhyllotaxisPattern::Whorled => *count,
                    };
                    let mut ordinal = 0_u32;
                    for step in 0..*nodes {
                        let along = interpolate(*start, *end, step, *nodes);
                        for lane in 0..per_node {
                            let turn = i64::from(divergence.bits()) * i64::from(ordinal)
                                + i64::from(UnitInterval::ONE.bits()) * i64::from(lane)
                                    / i64::from(per_node);
                            let (position, direction, radius) = sample_axis(axis, along, turn)?;
                            frames.push(BotanicalFrame {
                                id: axis.id.child(guid, ordinal),
                                axis: axis.id,
                                position,
                                direction,
                                radius,
                                along,
                            });
                            ordinal += 1;
                        }
                    }
                }
                frames_out.insert(guid, frames);
            }
            BotanicalOperator::Tropism { kind, strength } => {
                let mut axes = input_axes("axes");
                for axis in &mut axes {
                    bend(axis, *kind, *strength)?;
                }
                axes_out.insert(guid, axes);
            }
            BotanicalOperator::Prune {
                rule,
                threshold,
                count,
            } => {
                let axes = input_axes("axes");
                axes_out.insert(guid, prune(axes, *rule, *threshold, *count));
            }
            BotanicalOperator::Roots {
                depth_ratio,
                spread_ratio,
                count,
            } => {
                let axes = input_axes("axes");
                let mut roots = Vec::new();
                for source in &axes {
                    let length = source.length();
                    let depth = DecisionScalar::from_bits(
                        (i64::from(length.bits()) * i64::from(depth_ratio.bits())
                            / i64::from(UnitInterval::ONE.bits())) as i32,
                    );
                    let spread = i64::from(depth.bits()) * i64::from(spread_ratio.bits())
                        / i64::from(UnitInterval::ONE.bits());
                    for ordinal in 0..*count {
                        let turn = i64::from(UnitInterval::ONE.bits()) * i64::from(ordinal)
                            / i64::from(*count);
                        let (sin, cos) = turn_sin_cos(turn);
                        let direction = [
                            (spread * cos / i64::from(UnitInterval::ONE.bits())) as i32,
                            -i32::try_from(i64::from(depth.bits()).min(i64::from(i32::MAX)))
                                .unwrap_or(i32::MAX),
                            (spread * sin / i64::from(UnitInterval::ONE.bits())) as i32,
                        ];
                        roots.push(straight_axis(
                            source.id.child(guid, ordinal),
                            Some(source.id),
                            BotanicalElement::Root,
                            source.points[0],
                            direction,
                            depth,
                            source.radii[0],
                            &linear_taper(),
                            4,
                        )?);
                    }
                }
                axes_out.insert(guid, roots);
            }
            BotanicalOperator::Shell {
                material_slot,
                sides,
            } => {
                let axes = input_axes("axes");
                let shells: Vec<BotanicalShell> = axes
                    .iter()
                    .map(|axis| BotanicalShell {
                        id: axis.id.child(guid, 0),
                        axis: axis.id,
                        element: axis.element,
                        material_slot: *material_slot,
                        sides: *sides,
                    })
                    .collect();
                // Shells describe axes, so the axes they sweep travel with them.
                for axis in axes {
                    family_axes.insert(axis.id, axis);
                }
                shells_out.insert(guid, shells);
            }
            BotanicalOperator::Instance {
                element,
                material_slot,
                size,
                jitter,
            } => {
                let frames = input_frames("frames");
                let placed: Vec<BotanicalPlacement> = frames
                    .iter()
                    .map(|frame| {
                        let roll = if jitter.bits() == 0 {
                            UnitInterval::ZERO
                        } else {
                            let sample = RandomStream::new(RandomDomain {
                                map,
                                node_guid: guid,
                                node_semantic_revision: node.semantic_revision,
                                seed_namespace: individual.seed,
                                cell: saffron_spatial::WorldCellKey::base(0, 0, 0),
                                candidate: frame.id.value() as u64,
                                ancestor: 0,
                                species: 0,
                                channel: channel::ROLL,
                            })
                            .lane(0, 0);
                            UnitInterval::from_bits(
                                (u32::from(jitter.bits()) * (sample % 65_536) / 65_535) as u16,
                            )
                        };
                        BotanicalPlacement {
                            id: frame.id.child(guid, 0),
                            frame: frame.id,
                            element: *element,
                            material_slot: *material_slot,
                            position: frame.position,
                            size: aged(*size),
                            roll,
                        }
                    })
                    .collect();
                elements_out.insert(guid, placed);
            }
            BotanicalOperator::Family => {
                for from in incoming("shells") {
                    if let Some(shells) = shells_out.get(&from) {
                        assembly.shells.extend(shells.iter().cloned());
                    }
                }
                for from in incoming("elements") {
                    if let Some(elements) = elements_out.get(&from) {
                        assembly.elements.extend(elements.iter().copied());
                    }
                }
            }
        }
        if axes_out.values().map(Vec::len).sum::<usize>() > MAX_AXES {
            return Err(Error::ArtifactFormat {
                format: "botanical graph",
                field: "axes".to_owned(),
            });
        }
    }

    // The frames a placed element sits on come along too, so a compiled part can find its parent.
    let placed_frames: BTreeSet<BotanicalElementId> = assembly
        .elements
        .iter()
        .map(|element| element.frame)
        .collect();
    assembly.frames = frames_out
        .values()
        .flatten()
        .filter(|frame| placed_frames.contains(&frame.id))
        .copied()
        .collect();
    for frame in &assembly.frames {
        if let Some(axis) = frame_axes.get(&frame.axis)
            && !family_axes.contains_key(&frame.axis)
        {
            family_axes.insert(frame.axis, axis.clone());
        }
    }
    assembly.axes = family_axes.into_values().collect();
    assembly.axes.sort_by_key(|axis| axis.id);
    assembly.frames.sort_by_key(|frame| frame.id);
    assembly.shells.sort_by_key(|shell| shell.id);
    assembly.elements.sort_by_key(|element| element.id);
    Ok(assembly)
}

/// A straight axis of `segments` segments from `base` along `direction`, tapering by `taper`.
#[allow(clippy::too_many_arguments)]
fn straight_axis(
    id: BotanicalElementId,
    parent: Option<BotanicalElementId>,
    element: BotanicalElement,
    base: [DecisionScalar; 3],
    direction: [i32; 3],
    length: DecisionScalar,
    base_radius: DecisionScalar,
    taper: &DecisionCurve,
    segments: u32,
) -> Result<BotanicalAxis> {
    let magnitude = isqrt(
        (0..3)
            .map(|axis| i64::from(direction[axis]) * i64::from(direction[axis]))
            .sum(),
    )
    .max(1);
    let mut points = Vec::with_capacity(segments as usize + 1);
    let mut radii = Vec::with_capacity(segments as usize + 1);
    for step in 0..=segments {
        let along = interpolate(UnitInterval::ZERO, UnitInterval::ONE, step, segments);
        let travelled = i64::from(length.bits()) * i64::from(along.bits())
            / i64::from(UnitInterval::ONE.bits());
        points.push(std::array::from_fn(|axis| {
            DecisionScalar::from_bits(
                base[axis].bits().saturating_add(
                    i32::try_from(travelled * i64::from(direction[axis]) / magnitude)
                        .unwrap_or(i32::MAX),
                ),
            )
        }));
        let factor = taper.sample(along)?;
        radii.push(DecisionScalar::from_bits(
            ((i64::from(base_radius.bits()) * i64::from(factor.bits())) >> 16) as i32,
        ));
    }
    Ok(BotanicalAxis {
        id,
        parent,
        frame: None,
        element,
        points,
        radii,
    })
}

/// The default taper: full radius at the base falling to a tenth at the tip.
fn linear_taper() -> DecisionCurve {
    DecisionCurve::new(vec![
        (UnitInterval::ZERO, DecisionScalar::from_bits(65_536)),
        (UnitInterval::ONE, DecisionScalar::from_bits(6_553)),
    ])
    .expect("the default taper is two ordered points")
}

/// Where `step` of `steps` falls between `start` and `end`.
fn interpolate(start: UnitInterval, end: UnitInterval, step: u32, steps: u32) -> UnitInterval {
    if steps == 0 {
        return start;
    }
    let span = i64::from(end.bits()) - i64::from(start.bits());
    UnitInterval::from_bits(
        (i64::from(start.bits()) + span * i64::from(step) / i64::from(steps)).clamp(0, 65_535)
            as u16,
    )
}

/// Sine and cosine of a signed normalized turn, in `UnitInterval` bits, from an integer table.
///
/// A table rather than `f64`: an authored plant must grow the same on every target, and a libm
/// difference of one bit would move a branch.
pub(crate) fn turn_sin_cos(turn: i64) -> (i64, i64) {
    const QUARTER: i64 = 16_384;
    const TABLE: [i64; 17] = [
        0, 6_393, 12_539, 18_204, 23_170, 27_245, 30_273, 32_137, 32_767, 32_137, 30_273, 27_245,
        23_170, 18_204, 12_539, 6_393, 0,
    ];
    let sample = |phase: i64| -> i64 {
        let phase = phase.rem_euclid(4 * QUARTER);
        let (index, sign) = if phase < 2 * QUARTER {
            (phase, 1)
        } else {
            (phase - 2 * QUARTER, -1)
        };
        let slot = (index * 16 / (2 * QUARTER)).clamp(0, 16) as usize;
        sign * TABLE[slot] * 2
    };
    (sample(turn), sample(turn + QUARTER))
}

/// A direction `declination` off `axis`, rotated `azimuth` about it.
fn cone_direction(axis: [DecisionScalar; 3], declination: i64, azimuth: i64) -> [i32; 3] {
    let (sin_dec, cos_dec) = turn_sin_cos(declination / 2);
    let (sin_az, cos_az) = turn_sin_cos(azimuth * 4);
    let one = i64::from(UnitInterval::ONE.bits());
    // The axis contributes the cosine share; the perpendicular plane contributes the sine share.
    let lateral = sin_dec.abs().min(one);
    [
        ((i64::from(axis[0].bits()) * cos_dec + lateral * cos_az * 4) / one) as i32,
        ((i64::from(axis[1].bits()) * cos_dec) / one) as i32,
        ((i64::from(axis[2].bits()) * cos_dec + lateral * sin_az * 4) / one) as i32,
    ]
}

/// Position, outward direction, and radius at `along` on `axis`, rotated `turn` about it.
fn sample_axis(
    axis: &BotanicalAxis,
    along: UnitInterval,
    turn: i64,
) -> Result<([DecisionScalar; 3], [DecisionScalar; 3], DecisionScalar)> {
    if axis.points.is_empty() {
        return Err(Error::ArtifactFormat {
            format: "botanical graph",
            field: "axis.points".to_owned(),
        });
    }
    let last = axis.points.len() - 1;
    let scaled = usize::from(along.bits()) * last;
    let index = (scaled / usize::from(UnitInterval::ONE.bits())).min(last);
    let position = axis.points[index];
    let radius = axis.radii[index.min(axis.radii.len() - 1)];
    let (sin, cos) = turn_sin_cos(turn);
    let one = i64::from(UnitInterval::ONE.bits());
    let direction = [
        DecisionScalar::from_bits((cos * i64::from(UnitInterval::ONE.bits()) / one) as i32),
        DecisionScalar::from_bits(i32::from(UnitInterval::ONE.bits()) / 4),
        DecisionScalar::from_bits((sin * i64::from(UnitInterval::ONE.bits()) / one) as i32),
    ];
    Ok((position, direction, radius))
}

/// Bends an axis, accumulating along its length so the tip moves most and the base not at all.
///
/// The displacement is measured from how far along the axis a point sits, never from its current
/// height — otherwise an already-drooping branch would be *lifted* by gravity.
fn bend(axis: &mut BotanicalAxis, kind: TropismKind, strength: UnitInterval) -> Result<()> {
    if strength.bits() == 0 || axis.points.len() < 2 {
        return Ok(());
    }
    let sign: i64 = match kind {
        TropismKind::Phototropism => 1,
        TropismKind::Gravitropism | TropismKind::Thigmotropism => -1,
    };
    let one = i64::from(UnitInterval::ONE.bits());
    let base = axis.points[0];
    let count = (axis.points.len() - 1) as i64;
    for (step, point) in axis.points.iter_mut().enumerate().skip(1) {
        let travelled = isqrt(
            (0..3)
                .map(|lane| {
                    let delta = i64::from(point[lane].bits() - base[lane].bits());
                    delta * delta
                })
                .sum(),
        );
        // Quadratic in distance travelled: a smooth arc rather than a kink at the base.
        let step = step as i64;
        let share = step * step * one / (count * count).max(1);
        let displacement = i64::from(strength.bits()) * travelled / one * share / one;
        point[1] = DecisionScalar::from_bits(
            point[1]
                .bits()
                .saturating_add(i32::try_from(sign * displacement).unwrap_or(0)),
        );
    }
    Ok(())
}

/// Applies a prune rule, keeping canonical order.
fn prune(
    axes: Vec<BotanicalAxis>,
    rule: PruneRule,
    threshold: DecisionScalar,
    count: u32,
) -> Vec<BotanicalAxis> {
    match rule {
        PruneRule::BelowHeight => axes
            .into_iter()
            .filter(|axis| axis.base_height().bits() >= threshold.bits())
            .collect(),
        PruneRule::ShorterThan => axes
            .into_iter()
            .filter(|axis| axis.length().bits() >= threshold.bits())
            .collect(),
        PruneRule::KeepStrongest => {
            // Strength is base radius; ties break on identity so the survivor never depends on
            // input order.
            let mut per_parent: BTreeMap<Option<BotanicalElementId>, Vec<BotanicalAxis>> =
                BTreeMap::new();
            for axis in axes {
                per_parent.entry(axis.parent).or_default().push(axis);
            }
            let mut kept = Vec::new();
            for (_, mut siblings) in per_parent {
                siblings.sort_by_key(|axis| {
                    (
                        std::cmp::Reverse(axis.radii.first().map_or(0, |radius| radius.bits())),
                        axis.id,
                    )
                });
                siblings.truncate(count as usize);
                kept.extend(siblings);
            }
            kept.sort_by_key(|axis| axis.id);
            kept
        }
    }
}

/// The leading 16 bytes of a content hash, as the random domain's map identity.
fn leading_u128(bytes: [u8; 32]) -> u128 {
    let mut leading = [0_u8; 16];
    leading.copy_from_slice(&bytes[..16]);
    u128::from_be_bytes(leading)
}

/// Graph fixtures shared by the botanical tests and the compile tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    fn scalar(metres: i32) -> DecisionScalar {
        DecisionScalar::from_integer(metres).expect("finite scalar")
    }

    fn node(guid: u128, operator: BotanicalOperator) -> BotanicalNode {
        BotanicalNode {
            guid,
            version: BOTANICAL_NODE_VERSION,
            semantic_revision: 1,
            operator,
        }
    }

    fn edge(from: u128, from_pin: &str, to: u128, to_pin: &str) -> BotanicalEdge {
        BotanicalEdge {
            from_node: from,
            from_pin: from_pin.to_owned(),
            to_node: to,
            to_pin: to_pin.to_owned(),
        }
    }

    /// A birch: a trunk, frames up its length, branches from those frames, leaves on the branches'
    /// own frames, roots below, and one family output.
    pub(crate) fn birch() -> BotanicalGraphDocument {
        let mut document = BotanicalGraphDocument {
            variations: vec![BotanicalVariation {
                seed: 0xb17c4,
                age: UnitInterval::ONE,
                name: "Mature".to_owned(),
            }],
            edits: Vec::new(),
            nodes: vec![
                node(
                    1,
                    BotanicalOperator::Trunk {
                        element: BotanicalElement::Trunk,
                        length: scalar(9),
                        base_radius: DecisionScalar::from_bits(19_660),
                        taper: linear_taper(),
                        segments: 8,
                    },
                ),
                node(
                    2,
                    BotanicalOperator::Phyllotaxis {
                        pattern: PhyllotaxisPattern::Spiral,
                        count: 1,
                        nodes: 6,
                        start: UnitInterval::from_bits(20_000),
                        end: UnitInterval::from_bits(60_000),
                        divergence: UnitInterval::from_bits(22_800),
                    },
                ),
                node(
                    3,
                    BotanicalOperator::Branch {
                        element: BotanicalElement::Branch,
                        length_ratio: UnitInterval::from_bits(40_000),
                        radius_ratio: UnitInterval::from_bits(26_000),
                        declination: UnitInterval::from_bits(14_000),
                        jitter: UnitInterval::from_bits(8_000),
                        segments: 4,
                    },
                ),
                node(
                    4,
                    BotanicalOperator::Phyllotaxis {
                        pattern: PhyllotaxisPattern::Alternate,
                        count: 1,
                        nodes: 4,
                        start: UnitInterval::from_bits(10_000),
                        end: UnitInterval::ONE,
                        divergence: UnitInterval::from_bits(32_767),
                    },
                ),
                node(
                    5,
                    BotanicalOperator::Instance {
                        element: BotanicalElement::Leaf,
                        material_slot: 1,
                        size: DecisionScalar::from_bits(4_915),
                        jitter: UnitInterval::from_bits(30_000),
                    },
                ),
                node(
                    6,
                    BotanicalOperator::Shell {
                        material_slot: 0,
                        sides: 8,
                    },
                ),
                node(
                    7,
                    BotanicalOperator::Roots {
                        depth_ratio: UnitInterval::from_bits(20_000),
                        spread_ratio: UnitInterval::from_bits(45_000),
                        count: 4,
                    },
                ),
                node(8, BotanicalOperator::Family),
            ],
            edges: vec![
                edge(1, "axes", 2, "axes"),
                edge(1, "axes", 6, "axes"),
                edge(1, "axes", 7, "axes"),
                edge(2, "frames", 3, "frames"),
                edge(3, "axes", 4, "axes"),
                edge(3, "axes", 6, "axes"),
                edge(4, "frames", 5, "frames"),
                edge(5, "elements", 8, "elements"),
                edge(6, "shells", 8, "shells"),
                edge(7, "axes", 6, "axes"),
            ],
        };
        document.edges.sort();
        document
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::birch;
    use super::*;

    fn node(guid: u128, operator: BotanicalOperator) -> BotanicalNode {
        BotanicalNode {
            guid,
            version: BOTANICAL_NODE_VERSION,
            semantic_revision: 1,
            operator,
        }
    }

    fn edge(from: u128, from_pin: &str, to: u128, to_pin: &str) -> BotanicalEdge {
        BotanicalEdge {
            from_node: from,
            from_pin: from_pin.to_owned(),
            to_node: to,
            to_pin: to_pin.to_owned(),
        }
    }

    /// The graph grows a plant with the structure it describes, and the same graph grows it again
    /// identically — the property every downstream cache and every save file depends on.
    #[test]
    fn a_graph_grows_the_same_plant_every_time() {
        let document = birch();
        let first = grow(&document, 0).expect("the birch grows");
        let again = grow(&document, 0).expect("the birch grows again");
        assert_eq!(first, again, "growing is a pure function of the document");
        let first = first.assembly;

        // A trunk, six branch frames each carrying a branch, four roots.
        let trunks = first
            .axes
            .iter()
            .filter(|axis| axis.element == BotanicalElement::Trunk)
            .count();
        let branches = first
            .axes
            .iter()
            .filter(|axis| axis.element == BotanicalElement::Branch)
            .count();
        let roots = first
            .axes
            .iter()
            .filter(|axis| axis.element == BotanicalElement::Root)
            .count();
        assert_eq!((trunks, branches, roots), (1, 6, 4));
        // Every axis reaching the family is swept, and every branch carries four leaves.
        assert_eq!(first.shells.len(), 11);
        assert_eq!(first.elements.len(), 24);
        assert!(
            first
                .elements
                .iter()
                .all(|element| element.element == BotanicalElement::Leaf)
        );

        // Identity is structural, so no two grown elements collide.
        let ids: BTreeSet<u128> = first
            .axes
            .iter()
            .map(|axis| axis.id.value())
            .chain(first.elements.iter().map(|element| element.id.value()))
            .collect();
        assert_eq!(ids.len(), first.axes.len() + first.elements.len());
    }

    /// A different seed regrows a different individual, and the same seed the same one — which is
    /// what makes family variations coherent rather than random.
    #[test]
    fn the_seed_selects_the_individual() {
        let document = birch();
        let mut other = document.clone();
        other.variations[0].seed = 0xb17c5;
        let first = grow(&document, 0).unwrap().assembly;
        let second = grow(&other, 0).unwrap().assembly;
        assert_ne!(
            first.axes, second.axes,
            "a different seed grows a different individual"
        );
        assert_eq!(
            first.axes.len(),
            second.axes.len(),
            "but the same species: the structure the graph describes is unchanged"
        );
    }

    /// An element's identity is derived from its ancestry and ordinal, not from a counter, so a
    /// parameter change elsewhere leaves it addressable.
    #[test]
    fn element_identity_survives_an_unrelated_parameter_change() {
        let document = birch();
        let before = grow(&document, 0).unwrap().assembly;
        let mut edited = document.clone();
        // Change how big the leaves are: the branches it hangs them on are untouched.
        for node in &mut edited.nodes {
            if let BotanicalOperator::Instance { size, .. } = &mut node.operator {
                *size = DecisionScalar::from_bits(8_000);
            }
        }
        let after = grow(&edited, 0).unwrap().assembly;
        let axis_ids = |assembly: &BotanicalAssembly| -> Vec<u128> {
            assembly.axes.iter().map(|axis| axis.id.value()).collect()
        };
        assert_eq!(
            axis_ids(&before),
            axis_ids(&after),
            "every axis kept its identity"
        );
        assert_ne!(before.elements, after.elements, "the leaves did change");
    }

    /// Structural validation refuses what has no defined result rather than growing something
    /// arbitrary.
    #[test]
    fn structural_mistakes_are_refused() {
        let document = birch();
        document.validate().expect("the birch is well formed");

        // Two family sinks: no defined compiled result.
        let mut two_sinks = document.clone();
        two_sinks.nodes.push(node(9, BotanicalOperator::Family));
        assert!(two_sinks.validate().is_err());

        // A shell output plugged into a frames input.
        let mut mistyped = document.clone();
        mistyped.edges.push(edge(6, "shells", 3, "frames"));
        mistyped.edges.sort();
        assert!(mistyped.validate().is_err());

        // A cycle: a branch growing from frames on itself.
        let mut cyclic = document.clone();
        cyclic.edges.push(edge(3, "axes", 2, "axes"));
        cyclic.edges.push(edge(2, "frames", 3, "frames"));
        cyclic.edges.sort();
        cyclic.edges.dedup();
        assert!(cyclic.validate().is_err() || cyclic.topological_order().is_err());

        // Out-of-range parameters.
        let mut oversized = document.clone();
        for node in &mut oversized.nodes {
            if let BotanicalOperator::Trunk { segments, .. } = &mut node.operator {
                *segments = MAX_SEGMENTS + 1;
            }
        }
        assert!(oversized.validate().is_err());
    }

    /// A tropism bends the axis it is given and leaves its base where it was.
    #[test]
    fn a_tropism_bends_the_tip_and_not_the_base() {
        let mut document = birch();
        document.nodes.push(node(
            9,
            BotanicalOperator::Tropism {
                kind: TropismKind::Gravitropism,
                strength: UnitInterval::from_bits(40_000),
            },
        ));
        // Branches droop, then feed the same frames and shells.
        document
            .edges
            .retain(|edge| !(edge.from_node == 3 && (edge.to_node == 4 || edge.to_node == 6)));
        document.edges.push(edge(3, "axes", 9, "axes"));
        document.edges.push(edge(9, "axes", 4, "axes"));
        document.edges.push(edge(9, "axes", 6, "axes"));
        document.edges.sort();

        let straight = grow(&birch(), 0).unwrap().assembly;
        let drooped = grow(&document, 0).unwrap().assembly;
        let tips = |assembly: &BotanicalAssembly| -> Vec<i32> {
            assembly
                .axes
                .iter()
                .filter(|axis| axis.element == BotanicalElement::Branch)
                .map(|axis| axis.points.last().expect("tip")[1].bits())
                .collect()
        };
        let bases = |assembly: &BotanicalAssembly| -> Vec<i32> {
            assembly
                .axes
                .iter()
                .filter(|axis| axis.element == BotanicalElement::Branch)
                .map(|axis| axis.points[0][1].bits())
                .collect()
        };
        assert_eq!(bases(&straight), bases(&drooped), "the bases do not move");
        assert!(
            tips(&drooped)
                .iter()
                .zip(tips(&straight))
                .all(|(drooped, straight)| *drooped <= straight),
            "gravity never lifts a tip"
        );
    }

    /// Pruning removes axes by rule and keeps canonical order, so what survives cannot depend on
    /// the order the axes arrived in.
    #[test]
    fn pruning_keeps_the_strongest_in_canonical_order() {
        let document = birch();
        let grown = grow(&document, 0).unwrap().assembly;
        let branches: Vec<BotanicalAxis> = grown
            .axes
            .iter()
            .filter(|axis| axis.element == BotanicalElement::Branch)
            .cloned()
            .collect();
        assert!(branches.len() > 2);

        let kept = prune(
            branches.clone(),
            PruneRule::KeepStrongest,
            DecisionScalar::from_bits(0),
            2,
        );
        let mut reversed = branches.clone();
        reversed.reverse();
        let kept_reversed = prune(
            reversed,
            PruneRule::KeepStrongest,
            DecisionScalar::from_bits(0),
            2,
        );
        assert_eq!(
            kept, kept_reversed,
            "input order cannot change the survivors"
        );
        assert_eq!(kept.len(), 2);

        // A height rule drops everything below the cut.
        let high = prune(
            branches,
            PruneRule::BelowHeight,
            DecisionScalar::from_integer(4).unwrap(),
            0,
        );
        assert!(high.iter().all(
            |axis| axis.base_height().bits() >= DecisionScalar::from_integer(4).unwrap().bits()
        ));
    }

    /// The graph identity changes with any parameter, and two identical documents agree.
    #[test]
    fn the_graph_identity_is_its_exact_content() {
        let document = birch();
        assert_eq!(document.identity(), birch().identity());
        let mut edited = document.clone();
        edited.variations[0].seed += 1;
        assert_ne!(document.identity(), edited.identity());
    }
}
