//! Topological order, estimates, halo composition, and hard limits.

use super::*;

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
