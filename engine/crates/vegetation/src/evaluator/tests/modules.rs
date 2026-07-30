use super::*;

struct OneModuleResolver {
    pub(super) module: BiomeAsset,
}

impl BiomeGraphResolver for OneModuleResolver {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        if id == self.module.id {
            Ok(self.module.clone())
        } else {
            Err(Error::GraphDocument {
                path: "test.resolver".to_owned(),
                reason: "unknown module".to_owned(),
            })
        }
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        match source {
            GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
            _ => Ok([9; 32]),
        }
    }
}

pub(super) fn compile_module_fixture() -> CompiledBiomeGraph {
    compile_module_fixture_with_dead_sibling(false)
}

pub(super) fn compile_module_fixture_with_dead_sibling(
    include_dead_sibling: bool,
) -> CompiledBiomeGraph {
    let mut interface = node(10, GraphOperator::InterfaceInput, 0);
    interface.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("candidates".to_owned()),
    );
    let mut cluster = node(11, GraphOperator::ClusterPatchColony, 0);
    cluster.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_bits(1),
    };
    cluster.seed_namespaces.insert("cluster".to_owned(), 17);
    cluster
        .parameters
        .insert("children".to_owned(), GraphParameterValue::U32(2));
    cluster.parameters.insert(
        "radius".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(1)),
    );
    cluster.parameters.insert(
        "mode".to_owned(),
        GraphParameterValue::ClusterMode(GraphClusterMode::Cluster),
    );
    let mut module_outputs = vec![GraphInterfaceOutput {
        id: 2011,
        name: "candidates".to_owned(),
        domain: GraphDomain::Candidates,
        node: 11,
        pin: "candidates".to_owned(),
        sink: None,
    }];
    if include_dead_sibling {
        module_outputs.push(GraphInterfaceOutput {
            id: 2012,
            name: "unused-sibling-output-with-an-intentionally-long-name".to_owned(),
            domain: GraphDomain::Candidates,
            node: 10,
            pin: "value".to_owned(),
            sink: None,
        });
    }
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 2010,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
        }],
        outputs: module_outputs,
        nodes: vec![cluster, interface],
        edges: vec![GraphEdge {
            from_node: 10,
            from_pin: "value".to_owned(),
            to_node: 11,
            to_pin: "candidates".to_owned(),
        }],
    };
    let mut module = fixture_asset(0);
    module.id = Uuid(880);
    module.role = BiomeRole::Module;
    module.graph = module_document.to_json();

    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(3));
    let mut call = node(3, GraphOperator::ModuleCall, 0);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let species = node(4, GraphOperator::SpeciesInput, 0);
    let mut output = node(5, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3005,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 5,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, call, coverage, regions],
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
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
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
    let mut root = fixture_asset(0);
    root.graph = root_document.to_json();
    root.modules = vec![BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: Vec::new(),
    }];
    compile_biome_graph(
        &root,
        &[],
        &OneModuleResolver { module },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_module_global_prerequisite_fixture(
    resident_source: bool,
) -> CompiledBiomeGraph {
    let mut interface = node(10, GraphOperator::InterfaceInput, 0);
    interface.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("candidates".to_owned()),
    );
    let (module_nodes, module_edges, module_output_node) = if resident_source {
        let mut noise = node(11, GraphOperator::Noise, 0);
        noise.authority = GraphAuthority::EquivalentGpu;
        noise.spatial = NodeSpatialPolicy::Global { level: 0 };
        noise.seed_namespaces.insert("noise".to_owned(), 17);
        noise.parameters.insert(
            "frequency".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
        );
        noise.parameters.insert(
            "amplitude".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise
            .parameters
            .insert("channel".to_owned(), GraphParameterValue::U32(3));
        let mut importance = node(12, GraphOperator::FieldImportance, 0);
        importance.authority = GraphAuthority::EquivalentGpu;
        importance.spatial = NodeSpatialPolicy::Global { level: 0 };
        importance.parameters.insert(
            "threshold".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        (
            vec![importance, noise, interface],
            vec![
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 11,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 12,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 11,
                    from_pin: "field".to_owned(),
                    to_node: 12,
                    to_pin: "weights".to_owned(),
                },
            ],
            12,
        )
    } else {
        let mut transform = node(11, GraphOperator::Transform, 0);
        transform.spatial = NodeSpatialPolicy::Global { level: 0 };
        transform.seed_namespaces.insert("variation".to_owned(), 17);
        (
            vec![transform, interface],
            vec![GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "candidates".to_owned(),
            }],
            11,
        )
    };
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 2010,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
        }],
        outputs: vec![GraphInterfaceOutput {
            id: 2011,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: module_output_node,
            pin: "candidates".to_owned(),
            sink: None,
        }],
        nodes: module_nodes,
        edges: module_edges,
    };
    let mut module = fixture_asset(0);
    module.id = Uuid(881);
    module.role = BiomeRole::Module;
    module.graph = module_document.to_json();

    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let mut call = node(3, GraphOperator::ModuleCall, 0);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let mut coarse = node(6, GraphOperator::Transform, 2);
    coarse.spatial = NodeSpatialPolicy::Global { level: 2 };
    coarse.seed_namespaces.insert("variation".to_owned(), 17);
    let species = node(4, GraphOperator::SpeciesInput, 0);
    let mut output = node(5, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3005,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 5,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, coarse, call, coverage, regions],
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
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
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
            GraphEdge {
                from_node: 4,
                from_pin: "species".to_owned(),
                to_node: 5,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let mut root = fixture_asset(0);
    root.graph = root_document.to_json();
    root.modules = vec![BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: Vec::new(),
    }];
    compile_biome_graph(
        &root,
        &[],
        &OneModuleResolver { module },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_module_global_surface_fixture() -> CompiledBiomeGraph {
    let mut interface = node(10, GraphOperator::InterfaceInput, 0);
    interface.parameters.insert(
        "name".to_owned(),
        GraphParameterValue::String("candidates".to_owned()),
    );
    let mut field = node(11, GraphOperator::FieldSample, 0);
    field.spatial = NodeSpatialPolicy::Global { level: 0 };
    field.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Moisture),
    );
    field.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Value),
    );
    let mut importance = node(12, GraphOperator::FieldImportance, 0);
    importance.spatial = NodeSpatialPolicy::Global { level: 0 };
    importance.parameters.insert(
        "threshold".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 2010,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
        }],
        outputs: vec![GraphInterfaceOutput {
            id: 2012,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: 12,
            pin: "candidates".to_owned(),
            sink: None,
        }],
        nodes: vec![importance, field, interface],
        edges: vec![
            GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 12,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 11,
                from_pin: "field".to_owned(),
                to_node: 12,
                to_pin: "weights".to_owned(),
            },
        ],
    };
    let mut module = fixture_asset(0);
    module.id = Uuid(883);
    module.role = BiomeRole::Module;
    module.graph = module_document.to_json();

    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let mut call = node(3, GraphOperator::ModuleCall, 0);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let species = node(4, GraphOperator::SpeciesInput, 0);
    let mut output = node(5, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3005,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 5,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, call, coverage, regions],
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
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
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
    let mut root = fixture_asset(0);
    root.graph = root_document.to_json();
    root.modules = vec![BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: Vec::new(),
    }];
    compile_biome_graph(
        &root,
        &[],
        &OneModuleResolver { module },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_stage_materialized_module_output_fixture() -> CompiledBiomeGraph {
    let regions = node(10, GraphOperator::RegionInput, 0);
    let mut coverage = node(11, GraphOperator::StratifiedCoverage, 0);
    coverage.spatial = NodeSpatialPolicy::Global { level: 2 };
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let species = node(12, GraphOperator::SpeciesInput, 0);
    let unrelated = node(14, GraphOperator::RegionInput, 0);
    let mut output = node(13, GraphOperator::MacroOutput, 0);
    output.spatial = NodeSpatialPolicy::Global { level: 2 };
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let module_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 2013,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 13,
            pin: "points".to_owned(),
            sink: None,
        }],
        nodes: vec![unrelated, output, species, coverage, regions],
        edges: vec![
            GraphEdge {
                from_node: 10,
                from_pin: "regions".to_owned(),
                to_node: 11,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 11,
                from_pin: "candidates".to_owned(),
                to_node: 13,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 12,
                from_pin: "species".to_owned(),
                to_node: 13,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let mut module = fixture_asset(0);
    module.id = Uuid(882);
    module.role = BiomeRole::Module;
    module.graph = module_document.to_json();

    let mut call = node(3, GraphOperator::ModuleCall, 0);
    call.parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
    let root_document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 3003,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 3,
            pin: "macro".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![call],
        edges: Vec::new(),
    };
    let mut root = fixture_asset(0);
    root.graph = root_document.to_json();
    root.modules = vec![BiomeModuleReference {
        biome: module.id,
        call_guid: 99,
        bindings: Vec::new(),
    }];
    compile_biome_graph(
        &root,
        &[],
        &OneModuleResolver { module },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}
