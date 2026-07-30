use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionCurve, DecisionScalar, UnitInterval};

use crate::{
    BotanicalEditAction, BotanicalManualEdit, ContentHash, PlantSourceSelector, Result,
    validate_manual_edits,
};

use super::operator::{
    BOTANICAL_NODE_VERSION, BotanicalElement, BotanicalOperator, PhyllotaxisPattern, field,
    validate_operator,
};
use super::shape::linear_taper;

/// Variations one document may carry, each grown as its own geometry in the compiled family.
pub const MAX_VARIATIONS: usize = 16;

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
    pub operator: BotanicalOperator,
}

/// One directed typed edge between botanical nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BotanicalEdge {
    pub from_node: u128,
    /// Source output pin.
    pub from_pin: String,
    pub to_node: u128,
    /// Destination input pin.
    pub to_pin: String,
}

/// One individual the graph grows: a seed, an intrinsic age, and a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotanicalVariation {
    /// Seed for every stochastic decision. Changing it regrows a different individual.
    pub seed: u128,
    /// Intrinsic age, where one is the fully grown plant the graph's parameters describe.
    pub age: UnitInterval,
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
    /// variation.
    pub edits: Vec<BotanicalManualEdit>,
}

impl BotanicalGraphDocument {
    /// The graph a new native family starts from: a tapering trunk swept into bark, leaves on
    /// spiral frames up its length, and roots below.
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

    #[must_use]
    pub fn node(&self, guid: u128) -> Option<&BotanicalNode> {
        self.nodes.iter().find(|node| node.guid == guid)
    }

    /// The single `Family` sink.
    ///
    /// # Errors
    ///
    /// [`crate::Error::ArtifactFormat`] unless exactly one exists: a family with two outputs has no
    /// defined compiled result, and one with none produces nothing.
    pub fn family_node(&self) -> Result<&BotanicalNode> {
        let mut found = self
            .nodes
            .iter()
            .filter(|node| matches!(node.operator, BotanicalOperator::Family));
        let first = found.next().ok_or_else(|| field("family"))?;
        if found.next().is_some() {
            return Err(field("family.duplicate"));
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
    /// [`crate::Error::ArtifactFormat`] naming the exact field that failed.
    pub fn validate(&self) -> Result<()> {
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
    /// [`crate::Error::ArtifactFormat`] when the graph has a cycle — a plant cannot grow from
    /// itself.
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
            return Err(field("cycle"));
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

fn push_selector(bytes: &mut Vec<u8>, selector: &PlantSourceSelector) {
    match selector {
        PlantSourceSelector::Whole => bytes.push(0),
        PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(&(path.len() as u64).to_be_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        PlantSourceSelector::Submesh { element, index } => {
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
        BotanicalOperator::ModuleCall { call_guid } => {
            bytes.extend_from_slice(&call_guid.to_be_bytes());
        }
        BotanicalOperator::Family => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::operator::MAX_SEGMENTS;
    use super::super::tests_support::{birch, edge, node};
    use super::*;

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
