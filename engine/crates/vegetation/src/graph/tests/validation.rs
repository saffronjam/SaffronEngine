//! Node, parameter, and spatial-contract validation.

use super::*;

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
