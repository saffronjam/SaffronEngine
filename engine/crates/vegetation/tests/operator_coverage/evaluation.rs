use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{
    DecisionScalar, FieldChannel, FieldDerivative, UnitInterval, WorldBounds, WorldPosition,
};
use saffron_vegetation::{
    BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BiomeGraphDocument, BiomeModuleReference,
    CompanionRule, CompetitionRule, EvaluationFieldSource, EvaluationFieldTile, EvaluationRegion,
    EvaluationRegionKind, EvaluationSpline, GraphCompileOptions, GraphDependencySource,
    GraphDistanceSource, GraphDomain, GraphOperator, GraphParameterValue, GraphSink,
    NodeSpatialPolicy, QuantizedFieldTileValues, SuccessionRule, compile_biome_graph,
};

use crate::fixtures::{
    FAMILY_A, FAMILY_B, FIXED_ONE, FixtureResolver, MODULE_BIOME, asset, comprehensive_module,
    dependency_hash, diagnostic_document, edge, evaluate_partitioned, evaluation_input, node,
    output,
};
use GraphOperator as O;

#[test]
fn canonical_field_and_all_distance_sources_execute_with_exact_authored_inputs() {
    let region = node(201, O::RegionInput);
    let mut coverage = node(202, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    let mut painted = node(203, O::PaintedTile);
    painted.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::User(44)),
    );
    painted
        .parameters
        .insert("layer".to_owned(), GraphParameterValue::Guid(404));
    let mut distances = Vec::new();
    let mut diagnostics = Vec::new();
    let mut nodes = vec![region, coverage, painted];
    let mut edges = vec![
        edge(201, "regions", 202, "regions"),
        edge(202, "candidates", 203, "candidates"),
    ];
    let sources = [
        GraphDistanceSource::Water,
        GraphDistanceSource::Spline,
        GraphDistanceSource::Shape,
        GraphDistanceSource::Blocker,
    ];
    for (index, source) in sources.into_iter().enumerate() {
        let distance_guid = 210 + index as u128;
        let diagnostic_guid = 220 + index as u128;
        let mut distance = node(distance_guid, O::DistanceField);
        distance.parameters.insert(
            "source".to_owned(),
            GraphParameterValue::DistanceSource(source),
        );
        distance.parameters.insert(
            "sourceGuid".to_owned(),
            GraphParameterValue::Guid(match source {
                GraphDistanceSource::Water | GraphDistanceSource::Blocker => 404,
                GraphDistanceSource::Spline => 405,
                GraphDistanceSource::Shape => 406,
            }),
        );
        distance.parameters.insert(
            "maximumDistance".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(8 * 65_536)),
        );
        distance.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(8 * 65_536),
        };
        let mut diagnostic = node(diagnostic_guid, O::DiagnosticOutput);
        diagnostic.parameters.insert(
            "label".to_owned(),
            GraphParameterValue::String(format!("distance-{source:?}")),
        );
        edges.extend([
            edge(202, "candidates", distance_guid, "candidates"),
            edge(202, "candidates", diagnostic_guid, "candidates"),
            edge(distance_guid, "field", diagnostic_guid, "field"),
        ]);
        nodes.extend([distance, diagnostic]);
        diagnostics.push((diagnostic_guid, format!("distance-output-{index}")));
        distances.push(distance_guid);
    }
    let painted_diagnostic_guid = 230;
    let mut painted_diagnostic = node(painted_diagnostic_guid, O::DiagnosticOutput);
    painted_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("painted".to_owned()),
    );
    nodes.push(painted_diagnostic);
    edges.extend([
        edge(202, "candidates", painted_diagnostic_guid, "candidates"),
        edge(203, "field", painted_diagnostic_guid, "field"),
    ]);
    let mut output_specs = diagnostics
        .iter()
        .map(|(guid, label)| (*guid, label.as_str()))
        .collect::<Vec<_>>();
    output_specs.push((painted_diagnostic_guid, "painted-output"));
    let document = diagnostic_document(nodes, edges, &output_specs);
    let root = asset(document);
    let resolver = FixtureResolver::default();
    let graph =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut input = evaluation_input(&graph);
    let bounds = input.read_bounds;
    input.regions.push(EvaluationRegion {
        id: 406,
        kind: EvaluationRegionKind::Shape,
        layer: 406,
        hierarchy_namespace: None,
        seed_cell: input.output_cell,
        bounds: WorldBounds::new([0, 0, 0], [2_048, 2_048, 2_048]).unwrap(),
    });
    input.splines.push(EvaluationSpline {
        id: 405,
        layer: 405,
        points: vec![
            WorldPosition::from_global_ticks([0, 0, 0]).unwrap(),
            WorldPosition::from_global_ticks([4_096, 0, 0]).unwrap(),
        ],
    });
    for (channel, value) in [
        (FieldChannel::User(44), 11_111),
        (FieldChannel::WaterDistance, 22_222),
        (FieldChannel::SignedBlocker, -3_333),
    ] {
        input.fields.push(EvaluationFieldTile {
            source: EvaluationFieldSource::MapLayer(404),
            channel,
            derivative: FieldDerivative::Value,
            blend: saffron_vegetation::FieldBlendOperator::Replace,
            weight: UnitInterval::ONE,
            layer_order: (0, 404),
            source_hash: dependency_hash(GraphDependencySource::MapLayer(404)),
            bounds,
            dimensions: [1, 1, 1],
            values: QuantizedFieldTileValues::Scalar(vec![value]),
        });
    }
    let result = evaluate_partitioned(&root, &resolver, input);
    assert_eq!(result.diagnostics.streams.len(), 5);
    let expected_candidates = usize::try_from(
        result
            .diagnostics
            .nodes
            .iter()
            .find(|node| node.node == 202)
            .unwrap()
            .output_candidates,
    )
    .unwrap();
    assert!(expected_candidates > 4);
    for stream in &result.diagnostics.streams {
        assert_eq!(
            stream.candidates.as_ref().map(Vec::len),
            Some(expected_candidates)
        );
        assert_eq!(
            stream.field.as_ref().map(Vec::len),
            Some(expected_candidates)
        );
    }
    let painted_values = result
        .diagnostics
        .streams
        .iter()
        .find(|stream| stream.label == "painted")
        .unwrap()
        .field
        .as_ref()
        .unwrap();
    assert!(
        painted_values
            .iter()
            .all(|sample| sample.value.bits() == 11_111)
    );
    let executed = result
        .diagnostics
        .nodes
        .iter()
        .map(|node| node.operator)
        .collect::<BTreeSet<_>>();
    assert!(executed.contains(&O::PaintedTile));
    assert_eq!(
        result
            .diagnostics
            .nodes
            .iter()
            .filter(|node| node.operator == O::DistanceField)
            .count(),
        4
    );
    assert_eq!(distances.len(), 4);
}

#[test]
fn spline_ecology_macro_micro_and_module_fixtures_use_the_production_evaluator() {
    let spline_input = node(301, O::SplineInput);
    let mut follow = node(302, O::SplineFollow);
    follow
        .parameters
        .insert("spacing".to_owned(), GraphParameterValue::Fixed(FIXED_ONE));
    follow.parameters.insert(
        "edgeOffset".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    let mut diagnostic = node(303, O::DiagnosticOutput);
    diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("spline".to_owned()),
    );
    let document = diagnostic_document(
        vec![spline_input, follow, diagnostic],
        vec![
            edge(301, "splines", 302, "splines"),
            edge(302, "candidates", 303, "candidates"),
        ],
        &[(303, "spline-output")],
    );
    let root = asset(document);
    let resolver = FixtureResolver::default();
    let graph =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut input = evaluation_input(&graph);
    let ticks = i128::from(saffron_spatial::LOCAL_TICKS_PER_METER);
    input.splines.push(EvaluationSpline {
        id: 77,
        layer: 88,
        points: vec![
            WorldPosition::from_global_ticks([ticks, 0, ticks]).unwrap(),
            WorldPosition::from_global_ticks([3 * ticks, 0, ticks]).unwrap(),
            WorldPosition::from_global_ticks([3 * ticks, 0, 3 * ticks]).unwrap(),
        ],
    });
    let result = evaluate_partitioned(&root, &resolver, input);
    let positions = result.diagnostics.streams[0]
        .candidates
        .as_ref()
        .unwrap()
        .iter()
        .map(|sample| sample.position.global_ticks())
        .collect::<BTreeSet<_>>();
    assert_eq!(positions.len(), 5);
    assert_eq!(
        positions,
        BTreeSet::from([
            [ticks, 0, ticks],
            [2 * ticks, 0, ticks],
            [3 * ticks, 0, ticks],
            [3 * ticks, 0, 2 * ticks],
            [3 * ticks, 0, 3 * ticks],
        ])
    );

    let module = comprehensive_module();
    let region = node(311, O::RegionInput);
    let mut coverage = node(312, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(3));
    let mut call = node(313, O::ModuleCall);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(9_902));
    let mut module_diagnostic = node(314, O::DiagnosticOutput);
    module_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("module".to_owned()),
    );
    let module_document = diagnostic_document(
        vec![region, coverage, call, module_diagnostic],
        vec![
            edge(311, "regions", 312, "regions"),
            edge(312, "candidates", 313, "candidates"),
            edge(313, "candidates", 314, "candidates"),
        ],
        &[(314, "module-output")],
    );
    let mut module_root = asset(module_document);
    module_root.modules.push(BiomeModuleReference {
        biome: MODULE_BIOME,
        call_guid: 9_902,
        bindings: Vec::new(),
    });
    let module_resolver = FixtureResolver {
        modules: BTreeMap::from([(MODULE_BIOME.value(), module)]),
        ..FixtureResolver::default()
    };
    let module_graph = compile_biome_graph(
        &module_root,
        &[],
        &module_resolver,
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    let module_result = evaluate_partitioned(
        &module_root,
        &module_resolver,
        evaluation_input(&module_graph),
    );
    assert_eq!(
        module_result.diagnostics.streams[0]
            .candidates
            .as_ref()
            .map(Vec::len),
        Some(3)
    );
    assert!(
        module_result
            .diagnostics
            .nodes
            .iter()
            .any(|node| node.operator == O::ModuleCall)
    );
    assert!(
        module_result
            .diagnostics
            .nodes
            .iter()
            .any(|node| node.module_path == vec![9_902] && node.operator == O::InterfaceInput)
    );
}

#[test]
fn ecology_and_output_fixture_retains_family_transition_macro_and_micro_products() {
    let region = node(401, O::RegionInput);
    let mut coverage = node(402, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    let communities = node(403, O::CommunityInput);
    let mut blend = node(404, O::CommunityBlend);
    blend.parameters.insert(
        "shadeTolerance".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let succession = node(405, O::SuccessionInput);
    let species = node(406, O::SpeciesInput);
    let macro_output = node(407, O::MacroOutput);
    let mut micro_output = node(408, O::MicroOutput);
    micro_output.parameters.insert(
        "dimensions".to_owned(),
        GraphParameterValue::U32Vec3([2, 1, 2]),
    );
    micro_output.parameters.insert(
        "attributeChannels".to_owned(),
        GraphParameterValue::GuidList(Vec::new()),
    );
    let mut diagnostic = node(409, O::DiagnosticOutput);
    diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("succession".to_owned()),
    );
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![
            output(
                701,
                "macro",
                GraphDomain::MacroPoints,
                407,
                "points",
                GraphSink::Macro,
            ),
            output(
                702,
                "micro",
                GraphDomain::MicroField,
                408,
                "micro",
                GraphSink::Micro,
            ),
            output(
                703,
                "diagnostics",
                GraphDomain::Diagnostics,
                409,
                "diagnostics",
                GraphSink::Diagnostics,
            ),
        ],
        nodes: vec![
            region,
            coverage,
            communities,
            blend,
            succession,
            species,
            macro_output,
            micro_output,
            diagnostic,
        ],
        edges: vec![
            edge(401, "regions", 402, "regions"),
            edge(402, "candidates", 404, "candidates"),
            edge(403, "communities", 404, "communities"),
            edge(404, "candidates", 405, "candidates"),
            edge(405, "candidates", 407, "candidates"),
            edge(406, "species", 407, "species"),
            edge(405, "candidates", 408, "candidates"),
            edge(405, "candidates", 409, "candidates"),
        ],
    };
    let mut root = asset(document);
    root.palette[0].weight = UnitInterval::ONE;
    root.palette[1].weight = UnitInterval::ZERO;
    root.succession.push(SuccessionRule {
        from: FAMILY_A,
        to: FAMILY_B,
        minimum_tick: 10,
        probability: UnitInterval::ONE,
    });
    root.companions.push(CompanionRule {
        parent: FAMILY_A,
        child: FAMILY_B,
        minimum_distance: DecisionScalar::from_bits(16_384),
        maximum_distance: DecisionScalar::from_bits(32_768),
        probability: UnitInterval::ONE,
    });
    root.competition.push(CompetitionRule {
        first: FAMILY_A,
        second: FAMILY_B,
        spacing: DecisionScalar::from_bits(16_384),
        priority: 1,
    });
    let resolver = FixtureResolver::default();
    let graph =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut input = evaluation_input(&graph);
    input.ecology_tick = 10;
    let result = evaluate_partitioned(&root, &resolver, input);
    let stream = &result.diagnostics.streams[0];
    assert!(
        stream
            .candidates
            .as_ref()
            .unwrap()
            .iter()
            .all(|candidate| candidate.family == Some(FAMILY_B) && candidate.ecology_tick == 10)
    );
    assert_eq!(result.macro_points.row_count().unwrap(), 4);
    assert_eq!(result.macro_points.families, vec![FAMILY_B; 4]);
    assert_eq!(result.micro_fields.len(), 1);
    assert_eq!(result.micro_fields[0].dimensions, [2, 1, 2]);
    assert_eq!(result.micro_fields[0].density.len(), 4);
}
