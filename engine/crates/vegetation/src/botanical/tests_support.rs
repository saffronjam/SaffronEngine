//! Graph fixtures shared by the botanical tests and the compile tests.

use saffron_spatial::{DecisionScalar, UnitInterval};

use super::document::{BotanicalEdge, BotanicalGraphDocument, BotanicalNode, BotanicalVariation};
use super::operator::{
    BOTANICAL_NODE_VERSION, BotanicalElement, BotanicalOperator, PhyllotaxisPattern,
};
use super::shape::linear_taper;

pub(crate) fn node(guid: u128, operator: BotanicalOperator) -> BotanicalNode {
    BotanicalNode {
        guid,
        version: BOTANICAL_NODE_VERSION,
        semantic_revision: 1,
        operator,
    }
}

pub(crate) fn edge(from: u128, from_pin: &str, to: u128, to_pin: &str) -> BotanicalEdge {
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
    let scalar = |metres: i32| DecisionScalar::from_integer(metres).expect("finite scalar");
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
