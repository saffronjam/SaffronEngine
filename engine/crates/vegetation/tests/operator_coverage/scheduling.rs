use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_spatial::{DecisionScalar, WorldCellKey};
use saffron_vegetation::{
    BiomeGraphEvaluator, GlobalStageEvaluationInputs, GraphCancellationToken, GraphCompileOptions,
    GraphEvaluationInputs, GraphEvaluationJobInputs, GraphOperator, GraphParameterValue,
    compile_biome_graph, vegetation_content_hash,
};

use crate::fixtures::{
    FAMILY_A, FAMILY_B, FIXED_ONE, FixtureResolver, MAP, asset, diagnostic_document, edge, node,
    prototype,
};
use GraphOperator as O;

fn global_job(
    graph: &saffron_vegetation::CompiledBiomeGraph,
    cells: &[WorldCellKey],
) -> GraphEvaluationJobInputs {
    let cell_inputs = cells
        .iter()
        .map(|cell| {
            let mut input =
                GraphEvaluationInputs::for_cell(MAP, 41, *cell, graph.required_halo(cell.level()))
                    .unwrap();
            input.plant_prototypes = vec![prototype(FAMILY_A), prototype(FAMILY_B)];
            input
        })
        .collect();
    let mut owners = BTreeSet::new();
    let mut global_stages = Vec::new();
    for stage in graph.spatial_plan().global_stages() {
        for cell in cells {
            let owner = cell.ancestor(stage.owner_level).unwrap();
            if !owners.insert((stage.id, owner)) {
                continue;
            }
            let mut inputs =
                GraphEvaluationInputs::for_cell(MAP, 41, owner, stage.upstream_halo).unwrap();
            inputs.plant_prototypes = vec![prototype(FAMILY_A), prototype(FAMILY_B)];
            let mut identity = b"operator-coverage/global-stage/v1\0".to_vec();
            identity.extend_from_slice(&stage.id);
            identity.extend_from_slice(&owner.canonical_bytes());
            global_stages.push(GlobalStageEvaluationInputs {
                stage: stage.id,
                owner,
                solve_bounds: owner.bounds(),
                input_snapshot: vegetation_content_hash(&identity),
                inputs,
            });
        }
    }
    GraphEvaluationJobInputs {
        cells: cell_inputs,
        global_stages,
    }
}

#[test]
fn propagating_spatial_eliminators_are_schedule_stable_through_global_stage_tiles() {
    let region = node(501, O::RegionInput);
    let mut coverage = node(502, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(16));
    let communities = node(503, O::CommunityInput);
    let blend = node(504, O::CommunityBlend);
    let mut radius = node(505, O::Gradient);
    radius.parameters.insert(
        "direction".to_owned(),
        GraphParameterValue::FixedVec3([
            FIXED_ONE,
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(0),
        ]),
    );
    radius.parameters.insert(
        "exactOrigin".to_owned(),
        GraphParameterValue::WorldPosition([0; 3]),
    );
    radius.parameters.insert(
        "scale".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    radius.parameters.insert(
        "bias".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
    );
    let mut weighted = node(506, O::WeightedElimination);
    weighted
        .parameters
        .insert("targetCount".to_owned(), GraphParameterValue::U32(8));
    weighted.parameters.insert(
        "eliminationRadius".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(2 * 65_536)),
    );
    weighted
        .parameters
        .insert("maximumNeighbours".to_owned(), GraphParameterValue::U32(64));
    let mut variable = node(507, O::VariableSpacing);
    variable.parameters.insert(
        "prototypeAware".to_owned(),
        GraphParameterValue::Boolean(false),
    );
    let mut priority = node(508, O::PriorityExclusion);
    priority
        .parameters
        .insert("keepHighest".to_owned(), GraphParameterValue::Boolean(true));
    let mut bounds = node(509, O::BoundsOverlap);
    bounds.parameters.insert(
        "padding".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    let mut diagnostic = node(510, O::DiagnosticOutput);
    diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("eliminated".to_owned()),
    );
    let document = diagnostic_document(
        vec![
            region,
            coverage,
            communities,
            blend,
            radius,
            weighted,
            variable,
            priority,
            bounds,
            diagnostic,
        ],
        vec![
            edge(501, "regions", 502, "regions"),
            edge(502, "candidates", 504, "candidates"),
            edge(503, "communities", 504, "communities"),
            edge(504, "candidates", 505, "candidates"),
            edge(504, "candidates", 506, "candidates"),
            edge(505, "field", 506, "weights"),
            edge(506, "candidates", 507, "candidates"),
            edge(505, "field", 507, "radius"),
            edge(507, "candidates", 508, "candidates"),
            edge(505, "field", 508, "weights"),
            edge(505, "field", 508, "radius"),
            edge(508, "candidates", 509, "candidates"),
            edge(509, "candidates", 510, "candidates"),
        ],
        &[(510, "eliminator-output")],
    );
    let root = asset(document);
    let resolver = FixtureResolver::default();
    let graph = Arc::new(
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap(),
    );
    assert_eq!(graph.spatial_plan().global_stages().len(), 1);
    let stage = &graph.spatial_plan().global_stages()[0];
    assert_eq!(
        stage
            .nodes
            .iter()
            .map(|address| address.node)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([506, 507, 508, 509])
    );
    assert_eq!(stage.output_pins.len(), 1);
    assert_eq!(stage.output_pins[0].node.node, 509);
    assert_eq!(stage.output_pins[0].pin, "candidates");
    let cells = [WorldCellKey::base(0, 0, 0), WorldCellKey::base(1, 0, 0)];
    let serial = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
        .unwrap()
        .evaluate(
            global_job(&graph, &cells),
            &GraphCancellationToken::default(),
        )
        .unwrap();
    let reversed_cells = [cells[1], cells[0]];
    let reversed = BiomeGraphEvaluator::new(Arc::clone(&graph), 2)
        .unwrap()
        .evaluate(
            global_job(&graph, &reversed_cells),
            &GraphCancellationToken::default(),
        )
        .unwrap();
    let canonical_cells = |result: saffron_vegetation::GraphEvaluationJobResult| {
        result
            .cells
            .into_iter()
            .map(|cell| (cell.cell, cell.canonical_bytes().unwrap()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(canonical_cells(serial.clone()), canonical_cells(reversed));
    assert_eq!(serial.global_stages.len(), 2);
    for tile in &serial.global_stages {
        let executed = tile
            .result
            .diagnostics
            .nodes
            .iter()
            .map(|node| node.operator)
            .collect::<BTreeSet<_>>();
        assert!(executed.contains(&O::WeightedElimination));
        assert!(executed.contains(&O::VariableSpacing));
        assert!(executed.contains(&O::PriorityExclusion));
        assert!(executed.contains(&O::BoundsOverlap));
        assert!(tile.resident_bytes > 0);
    }
    for cell in &serial.cells {
        let stream = cell
            .diagnostics
            .streams
            .iter()
            .find(|stream| stream.label == "eliminated")
            .unwrap();
        let candidates = stream.candidates.as_ref().unwrap();
        assert!(!candidates.is_empty());
        assert!(candidates.len() <= 8);
        assert!(
            candidates
                .windows(2)
                .all(|pair| pair[0].identity < pair[1].identity)
        );
    }
}
