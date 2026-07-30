use super::*;

pub(super) fn fixture_asset(level: u8) -> BiomeAsset {
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: Uuid(701),
        name: "Determinism fixture".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![BiomePaletteEntry {
            plant: Uuid(702),
            weight: UnitInterval::ONE,
            seed_namespace: 13,
        }],
        density: DecisionScalar::from_bits(65_536),
        clustering: UnitInterval::ZERO,
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces: vec![
            ("poisson".to_owned(), 11),
            ("species".to_owned(), 13),
            ("noise".to_owned(), 17),
            ("community".to_owned(), 19),
        ],
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: DecisionScalar::from_bits(6 * 65_536),
            require_authoritative_fields: true,
        },
        graph: fixture_document(level).to_json(),
    }
}

pub(super) fn compile_fixture(level: u8) -> CompiledBiomeGraph {
    compile_biome_graph(
        &fixture_asset(level),
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_resident_branch_fixture() -> CompiledBiomeGraph {
    let level = 0;
    let region = node(1, GraphOperator::RegionInput, level);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(24));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::from_bits(32_768)),
    );
    let species = node(3, GraphOperator::SpeciesInput, level);
    let mut output = node(4, GraphOperator::MacroOutput, level);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let mut noise = node(6, GraphOperator::Noise, level);
    noise.authority = GraphAuthority::EquivalentGpu;
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
    let mut curve = node(10, GraphOperator::Curve, level);
    curve.authority = GraphAuthority::EquivalentGpu;
    curve.parameters.insert(
        "curve".to_owned(),
        GraphParameterValue::Curve(vec![
            (UnitInterval::ZERO, DecisionScalar::from_bits(0)),
            (UnitInterval::ONE, DecisionScalar::from_bits(65_535)),
        ]),
    );
    let mut remap = node(11, GraphOperator::Remap, level);
    remap.authority = GraphAuthority::EquivalentGpu;
    for (name, value) in [
        ("inputMin", -65_536),
        ("inputMax", 65_536),
        ("outputMin", 0),
        ("outputMax", 32_768),
    ] {
        remap.parameters.insert(
            name.to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(value)),
        );
    }
    let mut combine = node(12, GraphOperator::Combine, level);
    combine.authority = GraphAuthority::EquivalentGpu;
    combine.parameters.insert(
        "operation".to_owned(),
        GraphParameterValue::CombineOperation(GraphCombineOperation::Add),
    );
    let mut clamp = node(13, GraphOperator::Clamp, level);
    clamp.authority = GraphAuthority::EquivalentGpu;
    clamp.parameters.insert(
        "minimum".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    clamp.parameters.insert(
        "maximum".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(65_535)),
    );
    let mut importance = node(14, GraphOperator::FieldImportance, level);
    importance.authority = GraphAuthority::EquivalentGpu;
    importance.parameters.insert(
        "threshold".to_owned(),
        GraphParameterValue::Unit(UnitInterval::from_bits(16_384)),
    );
    let mut dead_clamp = node(15, GraphOperator::Clamp, level);
    dead_clamp.authority = GraphAuthority::EquivalentGpu;
    dead_clamp.parameters.insert(
        "minimum".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    dead_clamp.parameters.insert(
        "maximum".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(65_535)),
    );
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![
            dead_clamp, output, importance, clamp, combine, remap, curve, noise, species, coverage,
            region,
        ],
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
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "field".to_owned(),
                to_node: 10,
                to_pin: "field".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "field".to_owned(),
                to_node: 11,
                to_pin: "field".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "field".to_owned(),
                to_node: 15,
                to_pin: "field".to_owned(),
            },
            GraphEdge {
                from_node: 10,
                from_pin: "field".to_owned(),
                to_node: 12,
                to_pin: "left".to_owned(),
            },
            GraphEdge {
                from_node: 11,
                from_pin: "field".to_owned(),
                to_node: 12,
                to_pin: "right".to_owned(),
            },
            GraphEdge {
                from_node: 12,
                from_pin: "field".to_owned(),
                to_node: 13,
                to_pin: "field".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 14,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 13,
                from_pin: "field".to_owned(),
                to_node: 14,
                to_pin: "weights".to_owned(),
            },
            GraphEdge {
                from_node: 14,
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
    let mut asset = fixture_asset(level);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_global_fixture() -> CompiledBiomeGraph {
    let mut asset = fixture_asset(0);
    let mut document = fixture_document(0);
    let sampler = document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 2)
        .unwrap();
    sampler.operator = GraphOperator::BlueNoisePoisson;
    sampler.spatial = NodeSpatialPolicy::Global { level: 1 };
    sampler.parameters.remove("jitter");
    sampler.parameters.insert(
        "radius".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(2 * 65_536)),
    );
    sampler
        .parameters
        .insert("attempts".to_owned(), GraphParameterValue::U32(24));
    for guid in [4_u128, 8, 9] {
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == guid)
            .unwrap()
            .spatial = NodeSpatialPolicy::Global { level: 1 };
    }
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_same_level_macro_stages_fixture() -> CompiledBiomeGraph {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut outputs = Vec::new();
    for branch in 0_u128..2 {
        let base = branch * 10;
        let region = node(base + 1, GraphOperator::RegionInput, 0);
        let mut coverage = node(base + 2, GraphOperator::StratifiedCoverage, 1);
        coverage.spatial = NodeSpatialPolicy::Global { level: 1 };
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(1));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let species = node(base + 3, GraphOperator::SpeciesInput, 0);
        let mut output = node(base + 4, GraphOperator::MacroOutput, 1);
        output.spatial = NodeSpatialPolicy::Global { level: 1 };
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        nodes.extend([output, species, coverage, region]);
        edges.extend([
            GraphEdge {
                from_node: base + 1,
                from_pin: "regions".to_owned(),
                to_node: base + 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: base + 2,
                from_pin: "candidates".to_owned(),
                to_node: base + 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: base + 3,
                from_pin: "species".to_owned(),
                to_node: base + 4,
                to_pin: "species".to_owned(),
            },
        ]);
        outputs.push(GraphInterfaceOutput {
            id: 1_004 + base,
            name: format!("macro-{branch}"),
            domain: GraphDomain::MacroPoints,
            node: base + 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        });
    }
    compile_document(BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs,
        nodes,
        edges,
    })
}

pub(super) fn compile_candidate_only_global_fixture() -> CompiledBiomeGraph {
    let mut document = recursive_document();
    document
        .nodes
        .iter_mut()
        .find(|node| node.guid == 3)
        .unwrap()
        .spatial = NodeSpatialPolicy::Global { level: 1 };
    compile_document(document)
}

pub(super) fn compile_split_surface_projection_fixture(
    provider_hash: [u8; 32],
) -> CompiledBiomeGraph {
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
    let mut projection = node(3, GraphOperator::SurfaceProjection, 0);
    projection.spatial = NodeSpatialPolicy::Global { level: 0 };
    projection.parameters.insert(
        "direction".to_owned(),
        GraphParameterValue::FixedVec3([
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(-65_536),
            DecisionScalar::from_bits(0),
        ]),
    );
    projection.parameters.insert(
        "maxDistance".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_integer(10).unwrap()),
    );
    projection
        .parameters
        .insert("provider".to_owned(), GraphParameterValue::U64(77));
    let mut transform = node(4, GraphOperator::Transform, 2);
    transform.spatial = NodeSpatialPolicy::Global { level: 2 };
    transform.seed_namespaces.insert("variation".to_owned(), 13);
    transform.parameters.insert(
        "orientToSurface".to_owned(),
        GraphParameterValue::Boolean(true),
    );
    let species = node(5, GraphOperator::SpeciesInput, 0);
    let mut direct_output = node(6, GraphOperator::MacroOutput, 0);
    direct_output
        .seed_namespaces
        .insert("species-selection".to_owned(), 17);
    let mut transformed_output = node(7, GraphOperator::MacroOutput, 0);
    transformed_output
        .seed_namespaces
        .insert("species-selection".to_owned(), 19);
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![
            GraphInterfaceOutput {
                id: 1006,
                name: "direct".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 6,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            },
            GraphInterfaceOutput {
                id: 1007,
                name: "transformed".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 7,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            },
        ],
        nodes: vec![
            transformed_output,
            direct_output,
            species,
            transform,
            projection,
            coverage,
            regions,
        ],
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
                to_node: 4,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "surface".to_owned(),
                to_node: 4,
                to_pin: "surface".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 4,
                from_pin: "candidates".to_owned(),
                to_node: 7,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "species".to_owned(),
                to_node: 6,
                to_pin: "species".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "species".to_owned(),
                to_node: 7,
                to_pin: "species".to_owned(),
            },
        ],
    };
    let mut asset = fixture_asset(0);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &SurfaceDependencies { provider_hash },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_projection_output_demand_fixture(
    provider_hash: [u8; 32],
    surface_only: bool,
) -> CompiledBiomeGraph {
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
    let mut projection = node(3, GraphOperator::SurfaceProjection, 0);
    projection.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_integer(1).unwrap(),
    };
    projection.parameters.insert(
        "direction".to_owned(),
        GraphParameterValue::FixedVec3([
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(-65_536),
            DecisionScalar::from_bits(0),
        ]),
    );
    projection.parameters.insert(
        "maxDistance".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_integer(1).unwrap()),
    );
    projection
        .parameters
        .insert("provider".to_owned(), GraphParameterValue::U64(77));
    let species = node(4, GraphOperator::SpeciesInput, 0);
    let mut output = node(5, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let mut nodes = vec![output, species, projection, coverage, regions];
    let mut edges = vec![
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
            from_node: 4,
            from_pin: "species".to_owned(),
            to_node: 5,
            to_pin: "species".to_owned(),
        },
    ];
    if surface_only {
        let mut transform = node(6, GraphOperator::Transform, 0);
        transform.seed_namespaces.insert("variation".to_owned(), 17);
        transform.parameters.insert(
            "orientToSurface".to_owned(),
            GraphParameterValue::Boolean(true),
        );
        nodes.push(transform);
        edges.extend([
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 3,
                from_pin: "surface".to_owned(),
                to_node: 6,
                to_pin: "surface".to_owned(),
            },
            GraphEdge {
                from_node: 6,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
        ]);
    } else {
        edges.push(GraphEdge {
            from_node: 3,
            from_pin: "candidates".to_owned(),
            to_node: 5,
            to_pin: "candidates".to_owned(),
        });
    }
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1005,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 5,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes,
        edges,
    };
    let mut asset = fixture_asset(0);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &SurfaceDependencies { provider_hash },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_document(document: BiomeGraphDocument) -> CompiledBiomeGraph {
    let mut asset = fixture_asset(0);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &NoDependencies,
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_surface_field_fixture(provider_hash: [u8; 32]) -> CompiledBiomeGraph {
    let level = 0;
    let region = node(1, GraphOperator::RegionInput, level);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let species = node(3, GraphOperator::SpeciesInput, level);
    let mut output = node(4, GraphOperator::MacroOutput, level);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    let mut field = node(5, GraphOperator::FieldSample, level);
    field.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Moisture),
    );
    field.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Value),
    );
    let mut importance = node(6, GraphOperator::FieldImportance, level);
    importance.parameters.insert(
        "threshold".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1004,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 4,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, importance, field, species, coverage, region],
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
                to_node: 5,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 6,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "field".to_owned(),
                to_node: 6,
                to_pin: "weights".to_owned(),
            },
            GraphEdge {
                from_node: 6,
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
    let mut asset = fixture_asset(level);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &SurfaceDependencies { provider_hash },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}

pub(super) fn compile_dead_surface_branch_fixture(
    provider_hash: [u8; 32],
    include_dead_branch: bool,
) -> CompiledBiomeGraph {
    let mut document = fixture_document(0);
    if include_dead_branch {
        let mut field = node(20, GraphOperator::FieldSample, 0);
        field.parameters.insert(
            "channel".to_owned(),
            GraphParameterValue::FieldChannel(FieldChannel::Moisture),
        );
        field.parameters.insert(
            "derivative".to_owned(),
            GraphParameterValue::FieldDerivative(FieldDerivative::Value),
        );
        document.nodes.push(field);
        document.edges.push(GraphEdge {
            from_node: 2,
            from_pin: "candidates".to_owned(),
            to_node: 20,
            to_pin: "candidates".to_owned(),
        });
    }
    let mut asset = fixture_asset(0);
    asset.graph = document.to_json();
    compile_biome_graph(
        &asset,
        &[],
        &SurfaceDependencies { provider_hash },
        GraphCompileOptions::canonical(),
    )
    .unwrap()
}
