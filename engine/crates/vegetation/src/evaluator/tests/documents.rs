use super::*;

pub(super) fn explicit_anchor_document() -> BiomeGraphDocument {
    let mut anchors = node(1, GraphOperator::ExplicitAnchors, 0);
    anchors
        .parameters
        .insert("layer".to_owned(), GraphParameterValue::Guid(42));
    let species = node(2, GraphOperator::SpeciesInput, 0);
    let mut output = node(3, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1003,
            name: "macro".to_owned(),
            domain: GraphDomain::MacroPoints,
            node: 3,
            pin: "points".to_owned(),
            sink: Some(GraphSink::Macro),
        }],
        nodes: vec![output, species, anchors],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "candidates".to_owned(),
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "species".to_owned(),
                to_node: 3,
                to_pin: "species".to_owned(),
            },
        ],
    }
}

pub(super) fn spline_document() -> BiomeGraphDocument {
    let splines = node(1, GraphOperator::SplineInput, 0);
    let mut follow = node(2, GraphOperator::SplineFollow, 0);
    follow.parameters.insert(
        "spacing".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_integer(1).unwrap()),
    );
    follow.parameters.insert(
        "edgeOffset".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
    );
    let species = node(3, GraphOperator::SpeciesInput, 0);
    let mut output = node(4, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    BiomeGraphDocument {
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
        nodes: vec![output, species, follow, splines],
        edges: vec![
            GraphEdge {
                from_node: 1,
                from_pin: "splines".to_owned(),
                to_node: 2,
                to_pin: "splines".to_owned(),
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
    }
}

pub(super) fn recursive_document() -> BiomeGraphDocument {
    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(2));
    let mut recursive = node(3, GraphOperator::RecursiveCompanion, 0);
    recursive.spatial = NodeSpatialPolicy::Global { level: 0 };
    recursive
        .seed_namespaces
        .insert("companions".to_owned(), 17);
    recursive
        .parameters
        .insert("children".to_owned(), GraphParameterValue::U32(2));
    recursive
        .parameters
        .insert("maximumDepth".to_owned(), GraphParameterValue::U32(2));
    recursive.parameters.insert(
        "radius".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(1)),
    );
    let species = node(4, GraphOperator::SpeciesInput, 0);
    let mut output = node(5, GraphOperator::MacroOutput, 0);
    output
        .seed_namespaces
        .insert("species-selection".to_owned(), 13);
    BiomeGraphDocument {
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
        nodes: vec![output, species, recursive, coverage, regions],
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
    }
}

pub(super) fn micro_document() -> BiomeGraphDocument {
    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(1));
    let mut output = node(3, GraphOperator::MicroOutput, 0);
    output
        .seed_namespaces
        .insert("reconstruction".to_owned(), 13);
    output.parameters.insert(
        "dimensions".to_owned(),
        GraphParameterValue::U32Vec3([4, 4, 4]),
    );
    output.parameters.insert(
        "attributeChannels".to_owned(),
        GraphParameterValue::GuidList(Vec::new()),
    );
    let communities = node(4, GraphOperator::CommunityInput, 0);
    let mut blend = node(5, GraphOperator::CommunityBlend, 0);
    blend.seed_namespaces.insert("community".to_owned(), 19);
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1003,
            name: "micro".to_owned(),
            domain: GraphDomain::MicroField,
            node: 3,
            pin: "micro".to_owned(),
            sink: Some(GraphSink::Micro),
        }],
        nodes: vec![blend, communities, output, coverage, regions],
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
                from_node: 4,
                from_pin: "communities".to_owned(),
                to_node: 5,
                to_pin: "communities".to_owned(),
            },
            GraphEdge {
                from_node: 5,
                from_pin: "candidates".to_owned(),
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
        ],
    }
}

pub(super) fn explicit_family_micro_document() -> BiomeGraphDocument {
    explicit_micro_document([2, 1, 1])
}

pub(super) fn explicit_micro_document(dimensions: [u32; 3]) -> BiomeGraphDocument {
    let mut anchors = node(1, GraphOperator::ExplicitAnchors, 0);
    anchors
        .parameters
        .insert("layer".to_owned(), GraphParameterValue::Guid(42));
    let mut output = node(2, GraphOperator::MicroOutput, 0);
    output
        .seed_namespaces
        .insert("reconstruction".to_owned(), 13);
    output.parameters.insert(
        "dimensions".to_owned(),
        GraphParameterValue::U32Vec3(dimensions),
    );
    output.parameters.insert(
        "attributeChannels".to_owned(),
        GraphParameterValue::GuidList(Vec::new()),
    );
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1002,
            name: "micro".to_owned(),
            domain: GraphDomain::MicroField,
            node: 2,
            pin: "micro".to_owned(),
            sink: Some(GraphSink::Micro),
        }],
        nodes: vec![output, anchors],
        edges: vec![GraphEdge {
            from_node: 1,
            from_pin: "candidates".to_owned(),
            to_node: 2,
            to_pin: "candidates".to_owned(),
        }],
    }
}

pub(super) fn micro_attribute_document(channel: u128) -> BiomeGraphDocument {
    let regions = node(1, GraphOperator::RegionInput, 0);
    let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
    coverage.seed_namespaces.insert("sampling".to_owned(), 11);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(1));
    coverage.parameters.insert(
        "jitter".to_owned(),
        GraphParameterValue::Unit(UnitInterval::ZERO),
    );
    let mut noise = node(3, GraphOperator::Noise, 0);
    noise.seed_namespaces.insert("noise".to_owned(), 13);
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
        .insert("channel".to_owned(), GraphParameterValue::U32(7));
    let mut output = node(4, GraphOperator::MicroOutput, 0);
    output
        .seed_namespaces
        .insert("reconstruction".to_owned(), 17);
    output.parameters.insert(
        "dimensions".to_owned(),
        GraphParameterValue::U32Vec3([2, 2, 2]),
    );
    output.parameters.insert(
        "attributeChannels".to_owned(),
        GraphParameterValue::GuidList(vec![channel]),
    );
    let communities = node(5, GraphOperator::CommunityInput, 0);
    let mut blend = node(6, GraphOperator::CommunityBlend, 0);
    blend.seed_namespaces.insert("community".to_owned(), 19);
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: crate::BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![GraphInterfaceOutput {
            id: 1004,
            name: "micro".to_owned(),
            domain: GraphDomain::MicroField,
            node: 4,
            pin: "micro".to_owned(),
            sink: Some(GraphSink::Micro),
        }],
        nodes: vec![blend, communities, output, noise, coverage, regions],
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
            GraphEdge {
                from_node: 3,
                from_pin: "field".to_owned(),
                to_node: 4,
                to_pin: format!("attribute-{channel:032x}"),
            },
        ],
    }
}
