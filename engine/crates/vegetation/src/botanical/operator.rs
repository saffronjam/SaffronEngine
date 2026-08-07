use saffron_spatial::{DecisionCurve, DecisionScalar, UnitInterval};

use crate::{Error, PlantPartSemantic, Result};

/// Current botanical operator schema version.
pub const BOTANICAL_NODE_VERSION: u32 = 2;

/// Segment ceiling per axis.
pub const MAX_SEGMENTS: u32 = 64;
/// Attachment-node ceiling per axis.
pub const MAX_NODES: u32 = 64;
/// Ceiling on attachments per node, and on root axes.
pub const MAX_WHORL: u32 = 16;
/// Cross-section side ceiling.
pub const MAX_SIDES: u32 = 32;
/// Ceiling on axes one graph may grow, across every node.
pub const MAX_AXES: usize = 4_096;

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
    Frond,
    /// Broad leaf.
    Leaf,
    Needle,
    /// Grass or reed blade.
    Blade,
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
            _ => return Err(field("element")),
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
            _ => return Err(field("phyllotaxis.pattern")),
        })
    }
}

/// Which way a tropism bends an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TropismKind {
    /// Toward the light: the axis turns to follow the operator's light direction.
    Phototropism,
    /// With gravity: the droop of a loaded branch.
    Gravitropism,
    /// Away from an obstacle plane: the axis turns off the plane's outward normal, hardest where
    /// it is closest to the plane.
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
            _ => return Err(field("tropism.kind")),
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
            _ => return Err(field("prune.rule")),
        })
    }
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
    /// A spine an artist drew, point by point, entering the graph as an axis source.
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
    /// Bends axes toward the light, with gravity, or off an obstacle plane.
    Tropism {
        /// Which way it bends.
        kind: TropismKind,
        /// How strongly, accumulated along the axis.
        strength: UnitInterval,
        /// The light direction for `Phototropism` and the obstacle plane's outward normal for
        /// `Thigmotropism`, in family-local metres. Only its direction is read — the magnitude
        /// normalizes away. `Gravitropism` carries its own direction and ignores this one.
        stimulus: [DecisionScalar; 3],
        /// Signed distance from the family origin to the obstacle plane, along `stimulus`, in
        /// metres. Only `Thigmotropism` reads it.
        plane_offset: DecisionScalar,
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
    /// Grows another `.splant` module at each incoming frame.
    ModuleCall {
        /// Names the asset's [`crate::PlantModuleReference`] with the matching call GUID.
        call_guid: u128,
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
            Self::ModuleCall { .. } => "moduleCall",
            Self::Family => "family",
        }
    }

    /// Typed input pins, in canonical order.
    #[must_use]
    pub fn inputs(&self) -> &'static [(&'static str, BotanicalDomain)] {
        match self {
            Self::Drawn { .. } | Self::Trunk { .. } => &[],
            Self::Branch { .. } | Self::Instance { .. } | Self::ModuleCall { .. } => {
                &[("frames", BotanicalDomain::Frames)]
            }
            Self::Phyllotaxis { .. }
            | Self::Tropism { .. }
            | Self::Prune { .. }
            | Self::Roots { .. }
            | Self::Shell { .. } => &[("axes", BotanicalDomain::Spines)],
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
            Self::Drawn { .. }
            | Self::Trunk { .. }
            | Self::Branch { .. }
            | Self::Roots { .. }
            | Self::Tropism { .. }
            | Self::Prune { .. } => &[("axes", BotanicalDomain::Spines)],
            Self::Phyllotaxis { .. } => &[("frames", BotanicalDomain::Frames)],
            Self::Shell { .. } => &[("shells", BotanicalDomain::Shells)],
            Self::Instance { .. } => &[("elements", BotanicalDomain::Elements)],
            Self::ModuleCall { .. } => &[
                ("shells", BotanicalDomain::Shells),
                ("elements", BotanicalDomain::Elements),
            ],
            Self::Family => &[],
        }
    }
}

/// A botanical-graph format error naming the field that failed.
pub(super) fn field(name: &str) -> Error {
    Error::ArtifactFormat {
        format: "botanical graph",
        field: name.to_owned(),
    }
}

/// Checks one operator's parameters against the graph's declared ceilings.
pub(super) fn validate_operator(operator: &BotanicalOperator) -> Result<()> {
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
            // Repeated points give a zero-length segment, which sweeps into degenerate geometry.
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
        BotanicalOperator::ModuleCall { call_guid } => {
            // Zero is the absent GUID everywhere in this crate.
            if *call_guid == 0 {
                return Err(field("moduleCall.callGuid"));
            }
        }
        BotanicalOperator::Tropism { kind, stimulus, .. } => {
            // A zero stimulus names neither a light direction nor a plane, so the bend would have
            // no axis to follow; gravity supplies its own and does not read it.
            if !matches!(kind, TropismKind::Gravitropism)
                && stimulus.iter().all(|component| component.bits() == 0)
            {
                return Err(field("tropism.stimulus"));
            }
        }
        BotanicalOperator::Family => {}
    }
    Ok(())
}
