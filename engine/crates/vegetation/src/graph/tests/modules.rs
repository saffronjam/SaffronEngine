//! Module calls: parameter bindings, demand pruning, depth, and cycles.

use super::*;

#[test]
fn module_parameter_bindings_resolve_into_the_single_compiled_ir() {
    let mut interface = node(10, GraphOperator::InterfaceInput);
    interface.seed_namespaces.clear();
    interface.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("regions".to_owned()),
    );
    let mut poisson = node(11, GraphOperator::BlueNoisePoisson);
    poisson.seed_namespaces.insert("sampling".to_owned(), 11);
    poisson.spatial = NodeSpatialPolicy::Global { level: 0 };
    poisson
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    poisson
        .parameters
        .insert("radius".to_owned(), GraphParameterValue::Binding(55));
    poisson
        .parameters
        .insert("attempts".to_owned(), GraphParameterValue::U32(8));
    let mut transform = node(12, GraphOperator::Transform);
    transform.spatial = NodeSpatialPolicy::Global { level: 0 };
    transform.seed_namespaces.insert("variation".to_owned(), 11);
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 2010,
            name: "regions".to_owned(),
            domain: GraphDomain::Regions,
        }],
        outputs: vec![GraphInterfaceOutput {
            id: 2011,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: 12,
            pin: "candidates".to_owned(),
            sink: None,
        }],
        nodes: vec![transform, poisson, interface],
        edges: vec![
            GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 11,
                from_pin: "candidates".to_owned(),
                to_node: 12,
                to_pin: "candidates".to_owned(),
            },
        ],
    };
    let mut module = biome(module_document);
    module.id = Uuid(88);
    module.role = BiomeRole::Module;
    module.policy.maximum_influence_radius = DecisionScalar::from_bits(4 * 65_536);
    module.parameters = vec![BiomeParameter {
        id: 55,
        name: "radius".to_owned(),
        parameter_type: BiomeParameterType::Scalar,
        default_value: Value::from(65_536),
    }];

    let mut region = node(1, GraphOperator::RegionInput);
    region.seed_namespaces.clear();
    let mut call = node(2, GraphOperator::ModuleCall);
    call.seed_namespaces.clear();
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let mut species = node(3, GraphOperator::SpeciesInput);
    species.seed_namespaces.clear();
    let mut output = node(4, GraphOperator::MacroOutput);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 11);
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, call, region],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "species".to_owned(),
                to_node: 4,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let mut root = biome(root_document);
    root.policy.maximum_influence_radius = DecisionScalar::from_bits(4 * 65_536);
    root.modules = vec![BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: vec![(55, Value::from(2 * 65_536))],
    }];
    let resolver = ModuleResolver { module };
    let compiled =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let radius = compiled.root.nodes[1]
        .module
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|node| node.definition.guid == 11)
        .unwrap()
        .definition
        .parameter("radius");
    assert_eq!(
        radius,
        Some(&GraphParameterValue::Fixed(DecisionScalar::from_bits(
            2 * 65_536
        )))
    );
    let stages = compiled.spatial_plan().global_stages();
    assert_eq!(stages.len(), 1);
    assert_eq!(
        stages[0].nodes,
        vec![
            GraphNodeAddress {
                module_path: vec![99],
                node: 11,
            },
            GraphNodeAddress {
                module_path: vec![99],
                node: 12,
            },
        ]
    );
    assert!(stages[0].closure.contains(&GraphNodeAddress {
        module_path: Vec::new(),
        node: 1,
    }));
    assert_eq!(
        stages[0].output_pins,
        vec![QualifiedGraphPin {
            node: GraphNodeAddress {
                module_path: vec![99],
                node: 12,
            },
            pin: "candidates".to_owned(),
        }]
    );
}

#[test]
fn unused_module_instance_does_not_create_a_global_stage() {
    let mut interface = node(10, GraphOperator::InterfaceInput);
    interface.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("regions".to_owned()),
    );
    let mut poisson = node(11, GraphOperator::BlueNoisePoisson);
    poisson.spatial = NodeSpatialPolicy::Global { level: 2 };
    poisson.seed_namespaces.insert("sampling".to_owned(), 11);
    poisson
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    poisson.parameters.insert(
        "radius".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
    );
    poisson
        .parameters
        .insert("attempts".to_owned(), GraphParameterValue::U32(8));
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 2010,
            name: "regions".to_owned(),
            domain: GraphDomain::Regions,
        }],
        outputs: vec![GraphInterfaceOutput {
            id: 2011,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: 11,
            pin: "candidates".to_owned(),
            sink: None,
        }],
        nodes: vec![poisson, interface],
        edges: vec![GraphEdge {
            from_node: 10,
            from_pin: "value".to_owned(),
            to_node: 11,
            to_pin: "regions".to_owned(),
        }],
    };
    let mut module = biome(module_document);
    module.id = Uuid(88);
    module.role = BiomeRole::Module;

    let region = node(1, GraphOperator::RegionInput);
    let mut first_call = node(2, GraphOperator::ModuleCall);
    first_call
        .parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(91));
    let call_dependency = GraphDependencySource::Asset(Uuid(777));
    first_call.dependencies.push(call_dependency);
    let mut second_call = node(3, GraphOperator::ModuleCall);
    second_call
        .parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(92));
    let species = node(4, GraphOperator::SpeciesInput);
    let mut output = node(5, GraphOperator::MacroOutput);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 11);
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3005,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 5,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, second_call, first_call, region],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 3,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 4,
                from_pin: "species".to_owned(),
                to_node: 5,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let mut root = biome(root_document);
    root.modules = vec![
        BiomeModuleReference {
            biome: module.id,
            call_guid: 91,
            bindings: Vec::new(),
        },
        BiomeModuleReference {
            biome: module.id,
            call_guid: 92,
            bindings: Vec::new(),
        },
    ];
    let compiled = compile_biome_graph(
        &root,
        &[],
        &SelectiveModuleResolver {
            module: module.clone(),
            source: call_dependency,
            content_hash: [7; 32],
        },
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    let stages = compiled.spatial_plan().global_stages();
    assert_eq!(stages.len(), 1);
    assert_eq!(
        stages
            .iter()
            .map(|stage| stage.nodes.clone())
            .collect::<Vec<_>>(),
        vec![vec![GraphNodeAddress {
            module_path: vec![91],
            node: 11,
        }]]
    );
    for stage in stages {
        assert!(stage.closure.contains(&GraphNodeAddress {
            module_path: Vec::new(),
            node: 1,
        }));
        assert_eq!(stage.owner_level, 2);
        assert_eq!(stage.minimum_input_level, 0);
        assert!(!stage.dependencies.is_empty());
        assert_eq!(
            compiled
                .spatial_plan()
                .global_stage_for_node(&stage.nodes[0])
                .map(|candidate| candidate.id),
            Some(stage.id)
        );
        assert!(stage.dependencies.contains(&GraphDependencyFingerprint {
            source: call_dependency,
            content_hash: [7; 32],
        }));
    }
    let changed_dependency = compile_biome_graph(
        &root,
        &[],
        &SelectiveModuleResolver {
            module,
            source: call_dependency,
            content_hash: [8; 32],
        },
        GraphCompileOptions::canonical(),
    )
    .unwrap();
    assert_ne!(
        compiled.spatial_plan().global_stages()[0].id,
        changed_dependency.spatial_plan().global_stages()[0].id
    );
}

#[test]
fn module_demand_prunes_unused_output_branch_and_its_unique_input() {
    let mut input_a = node(10, GraphOperator::InterfaceInput);
    input_a.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("regions-a".to_owned()),
    );
    let mut input_b = node(20, GraphOperator::InterfaceInput);
    input_b.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("regions-b".to_owned()),
    );
    let mut live = node(11, GraphOperator::StratifiedCoverage);
    live.seed_namespaces.insert("sampling".to_owned(), 11);
    live.parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    live.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let mut dead = node(21, GraphOperator::StratifiedCoverage);
    dead.seed_namespaces.insert("sampling".to_owned(), 11);
    dead.parameters
        .insert("count".to_owned(), GraphParameterValue::U32(999));
    dead.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let mut dead_transform_a = node(22, GraphOperator::Transform);
    dead_transform_a.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_bits(49_152),
    };
    dead_transform_a
        .seed_namespaces
        .insert("variation".to_owned(), 11);
    let mut dead_transform_b = node(23, GraphOperator::Transform);
    dead_transform_b.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_bits(49_152),
    };
    dead_transform_b
        .seed_namespaces
        .insert("variation".to_owned(), 11);
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: vec![
            GraphInterfaceInput {
                id: 2010,
                name: "regions-a".to_owned(),
                domain: GraphDomain::Regions,
            },
            GraphInterfaceInput {
                id: 2020,
                name: "regions-b".to_owned(),
                domain: GraphDomain::Regions,
            },
        ],
        outputs: vec![
            GraphInterfaceOutput {
                id: 2011,
                name: "a".to_owned(),
                domain: GraphDomain::Candidates,
                node: 11,
                pin: "candidates".to_owned(),
                sink: None,
            },
            GraphInterfaceOutput {
                id: 2021,
                name: "b".to_owned(),
                domain: GraphDomain::Candidates,
                node: 23,
                pin: "candidates".to_owned(),
                sink: None,
            },
        ],
        nodes: vec![
            dead_transform_b,
            dead_transform_a,
            dead,
            live,
            input_b,
            input_a,
        ],
        edges: vec![
            GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 20,
                from_pin: "value".to_owned(),
                to_node: 21,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 21,
                from_pin: "candidates".to_owned(),
                to_node: 22,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 22,
                from_pin: "candidates".to_owned(),
                to_node: 23,
                to_pin: "candidates".to_owned(),
            },
        ],
    };
    let mut module = biome(module_document);
    module.id = Uuid(88);
    module.role = BiomeRole::Module;

    let region = node(1, GraphOperator::RegionInput);
    let mut call = node(2, GraphOperator::ModuleCall);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let species = node(3, GraphOperator::SpeciesInput);
    let mut output = node(4, GraphOperator::MacroOutput);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 11);
    let mut root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, call, region],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions-a".to_owned(),
            },
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions-b".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "a".to_owned(),
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "species".to_owned(),
                to_node: 4,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let compile = |document: &BiomeGraphDocument| {
        let mut root = biome(document.clone());
        root.modules.push(BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        });
        compile_biome_graph(
            &root,
            &[],
            &ModuleResolver {
                module: module.clone(),
            },
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_candidates: 10,
                    ..GraphSafetyLimits::default()
                },
            },
        )
    };
    let compiled = compile(&root_document).unwrap();
    let nested = compiled
        .demand_plan()
        .execution_slice()
        .unit(&[99])
        .unwrap();
    assert_eq!(nested.nodes, vec![10, 11]);
    assert_eq!(nested.inputs, BTreeSet::from(["regions-a".to_owned()]));
    assert_eq!(nested.outputs, BTreeSet::from(["a".to_owned()]));
    assert!(
        !compiled
            .demand_plan()
            .execution_slice()
            .unit(&[])
            .unwrap()
            .edges
            .iter()
            .any(|edge| edge.to_node == 2 && edge.to_pin == "regions-b")
    );

    root_document
        .edges
        .iter_mut()
        .find(|edge| edge.to_node == 4 && edge.to_pin == "candidates")
        .unwrap()
        .from_pin = "b".to_owned();
    assert!(matches!(
        compile(&root_document),
        Err(Error::GraphLimit {
            resource: "candidate count",
            requested: 999,
            limit: 10,
        })
    ));
    let mut root = biome(root_document);
    root.modules.push(BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: Vec::new(),
    });
    assert!(matches!(
        compile_biome_graph(
            &root,
            &[],
            &ModuleResolver { module },
            GraphCompileOptions {
                limits: GraphSafetyLimits {
                    max_candidates: 1_000,
                    ..GraphSafetyLimits::default()
                },
            },
        ),
        Err(Error::GraphLimit {
            resource: "composed influence radius",
            ..
        })
    ));
}

#[test]
fn module_depth_counts_call_edges_and_enforces_exact_boundaries() {
    compile_biome_graph(
        &biome(simple_document()),
        &[],
        &NoModules,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 0,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap();

    let leaf = module_asset(90, None, 0);
    let single_resolver = ModuleSetResolver {
        modules: vec![leaf.clone()],
    };
    let single_root = root_with_module(90, 800);
    let error = compile_biome_graph(
        &single_root,
        &[],
        &single_resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 0,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "module recursion",
            requested: 1,
            limit: 0,
        }
    ));
    compile_biome_graph(
        &single_root,
        &[],
        &single_resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 1,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap();

    let branch = module_asset(89, Some((90, 900)), 8);
    let resolver = ModuleSetResolver {
        modules: vec![branch, leaf],
    };
    let mut root = root_with_module(89, 800);
    root.policy.maximum_recursion = 8;

    let error = compile_biome_graph(
        &root,
        &[],
        &resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 1,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "module recursion",
            requested: 2,
            limit: 1,
        }
    ));

    compile_biome_graph(
        &root,
        &[],
        &resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 2,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap();

    root.policy.maximum_recursion = 1;
    let error = compile_biome_graph(
        &root,
        &[],
        &resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 2,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "module recursion",
            requested: 2,
            limit: 1,
        }
    ));
}

#[test]
fn module_policy_is_relative_to_its_entry_depth() {
    let leaf = module_asset(92, None, 0);
    let inner = module_asset(91, Some((92, 910)), 8);
    let outer = module_asset(90, Some((91, 900)), 1);
    let mut root = root_with_module(90, 800);
    root.policy.maximum_recursion = 8;
    let resolver = ModuleSetResolver {
        modules: vec![outer.clone(), inner.clone(), leaf.clone()],
    };
    let options = GraphCompileOptions {
        limits: GraphSafetyLimits {
            max_module_depth: 3,
            ..GraphSafetyLimits::default()
        },
    };

    let error = compile_biome_graph(&root, &[], &resolver, options).unwrap_err();
    assert!(matches!(
        error,
        Error::GraphLimit {
            resource: "module recursion",
            requested: 3,
            limit: 2,
        }
    ));

    let mut allowed_outer = outer;
    allowed_outer.policy.maximum_recursion = 2;
    let resolver = ModuleSetResolver {
        modules: vec![allowed_outer, inner, leaf],
    };
    compile_biome_graph(
        &root,
        &[],
        &resolver,
        GraphCompileOptions {
            limits: GraphSafetyLimits {
                max_module_depth: 3,
                ..GraphSafetyLimits::default()
            },
        },
    )
    .unwrap();
}

#[test]
fn indirect_module_cycle_returns_the_typed_cycle_error() {
    let first = module_asset(89, Some((90, 890)), 8);
    let second = module_asset(90, Some((89, 900)), 8);
    let resolver = ModuleSetResolver {
        modules: vec![first, second],
    };
    let mut root = root_with_module(89, 800);
    root.policy.maximum_recursion = 8;

    assert!(matches!(
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical(),),
        Err(Error::GraphCycle { node: 89 })
    ));
}
