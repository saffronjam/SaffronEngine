use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, FieldChannel, FieldDerivative, UnitInterval, WorldCellKey};
use saffron_vegetation::{
    BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION,
    BiomeAsset, BiomeGraphDocument, BiomeGraphEvaluator, BiomeGraphPolicy, BiomeGraphResolver,
    BiomeModuleReference, BiomePaletteEntry, BiomeRole, GraphAuthority, GraphCancellationToken,
    GraphClusterMode, GraphCombineOperation, GraphCompileOptions, GraphDependencySource,
    GraphDistanceSource, GraphDomain, GraphEdge, GraphEvaluationInputs, GraphEvaluationJobInputs,
    GraphInterfaceInput, GraphInterfaceOutput, GraphNodeDefinition, GraphOperator,
    GraphParameterType, GraphParameterValue, GraphSink, GraphSpatialRequirement, NodeSpatialPolicy,
    PlantPrototype, Result, SuitabilityBinding, compile_biome_graph, vegetation_content_hash,
};

use GraphOperator as O;

pub const FAMILY_A: Uuid = Uuid(7_001);
pub const FAMILY_B: Uuid = Uuid(7_002);
const ROOT_BIOME: Uuid = Uuid(8_001);
pub const MODULE_BIOME: Uuid = Uuid(8_002);
pub const MAP: Uuid = Uuid(9_001);
pub const FIXED_ONE: DecisionScalar = DecisionScalar::from_bits(65_536);

#[derive(Default)]
pub struct FixtureResolver {
    pub modules: BTreeMap<u64, BiomeAsset>,
    pub dependency_hashes: BTreeMap<GraphDependencySource, [u8; 32]>,
    pub available: Vec<GraphDependencySource>,
}

impl BiomeGraphResolver for FixtureResolver {
    fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
        self.modules.get(&id.value()).cloned().ok_or_else(|| {
            saffron_vegetation::Error::GraphDocument {
                path: "operator-coverage.resolver".to_owned(),
                reason: format!("unknown module {}", id.value()),
            }
        })
    }

    fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
        Ok(self
            .dependency_hashes
            .get(&source)
            .copied()
            .unwrap_or_else(|| dependency_hash(source)))
    }

    fn available_dependencies(&self) -> Vec<GraphDependencySource> {
        self.available.clone()
    }
}

pub fn dependency_hash(source: GraphDependencySource) -> [u8; 32] {
    vegetation_content_hash(format!("operator-coverage/{source:?}").as_bytes())
}

fn parameter_value(parameter_type: GraphParameterType) -> GraphParameterValue {
    match parameter_type {
        GraphParameterType::Boolean => GraphParameterValue::Boolean(false),
        GraphParameterType::U32 => GraphParameterValue::U32(4),
        GraphParameterType::U64 => GraphParameterValue::U64(77),
        GraphParameterType::U32Vec3 => GraphParameterValue::U32Vec3([2, 1, 2]),
        GraphParameterType::Guid => GraphParameterValue::Guid(901),
        GraphParameterType::Asset => GraphParameterValue::Asset(FAMILY_A),
        GraphParameterType::Fixed => GraphParameterValue::Fixed(FIXED_ONE),
        GraphParameterType::Unit => GraphParameterValue::Unit(UnitInterval::ONE),
        GraphParameterType::FixedVec3 => GraphParameterValue::FixedVec3([
            FIXED_ONE,
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(0),
        ]),
        GraphParameterType::WorldPosition => GraphParameterValue::WorldPosition([0; 3]),
        GraphParameterType::FieldChannel => {
            GraphParameterValue::FieldChannel(FieldChannel::Altitude)
        }
        GraphParameterType::FieldDerivative => {
            GraphParameterValue::FieldDerivative(FieldDerivative::Value)
        }
        GraphParameterType::CombineOperation => {
            GraphParameterValue::CombineOperation(GraphCombineOperation::Add)
        }
        GraphParameterType::DistanceSource => {
            GraphParameterValue::DistanceSource(GraphDistanceSource::Spline)
        }
        GraphParameterType::ClusterMode => {
            GraphParameterValue::ClusterMode(GraphClusterMode::Cluster)
        }
        GraphParameterType::Curve => GraphParameterValue::Curve(vec![
            (UnitInterval::ZERO, DecisionScalar::from_bits(0)),
            (UnitInterval::ONE, FIXED_ONE),
        ]),
        GraphParameterType::String => GraphParameterValue::String("candidates".to_owned()),
        GraphParameterType::GuidList => GraphParameterValue::GuidList(vec![902]),
        GraphParameterType::TagList => GraphParameterValue::TagList(vec![903]),
    }
}

pub fn node(guid: u128, operator: GraphOperator) -> GraphNodeDefinition {
    let mut parameters = operator
        .parameter_schema()
        .into_iter()
        .filter(|descriptor| descriptor.required)
        .map(|descriptor| {
            (
                descriptor.name.to_owned(),
                parameter_value(descriptor.parameter_type),
            )
        })
        .collect::<BTreeMap<_, _>>();
    match operator {
        O::Remap => {
            parameters.insert(
                "inputMin".to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
            );
            parameters.insert("inputMax".to_owned(), GraphParameterValue::Fixed(FIXED_ONE));
            parameters.insert(
                "outputMin".to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
            );
            parameters.insert(
                "outputMax".to_owned(),
                GraphParameterValue::Fixed(FIXED_ONE),
            );
        }
        O::Clamp => {
            parameters.insert(
                "minimum".to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
            );
            parameters.insert("maximum".to_owned(), GraphParameterValue::Fixed(FIXED_ONE));
        }
        O::Competition => {
            parameters.insert(
                "crownWeight".to_owned(),
                GraphParameterValue::Unit(UnitInterval::ONE),
            );
            parameters.insert(
                "rootWeight".to_owned(),
                GraphParameterValue::Unit(UnitInterval::ONE),
            );
        }
        _ => {}
    }
    let seed_namespaces = operator
        .seed_namespace_names()
        .iter()
        .enumerate()
        .map(|(index, name)| ((*name).to_owned(), guid * 100 + index as u128 + 1))
        .collect();
    let spatial = match operator.spatial_requirement() {
        GraphSpatialRequirement::Propagating => NodeSpatialPolicy::Global { level: 0 },
        GraphSpatialRequirement::FiniteSupport => NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(match operator {
                O::RecursiveCompanion => 4 * 65_536,
                O::SurfaceProjection
                | O::DistanceField
                | O::ClusterPatchColony
                | O::Competition => 65_536,
                _ => 0,
            }),
        },
    };
    GraphNodeDefinition {
        guid,
        version: BIOME_NODE_VERSION,
        semantic_revision: 1,
        operator,
        authority: GraphAuthority::Authoritative,
        spatial,
        dependencies: Vec::new(),
        seed_namespaces,
        parameters,
    }
}

pub fn edge(from_node: u128, from_pin: &str, to_node: u128, to_pin: &str) -> GraphEdge {
    GraphEdge {
        from_node,
        from_pin: from_pin.to_owned(),
        to_node,
        to_pin: to_pin.to_owned(),
    }
}

pub fn output(
    id: u128,
    name: &str,
    domain: GraphDomain,
    node: u128,
    pin: &str,
    sink: GraphSink,
) -> GraphInterfaceOutput {
    GraphInterfaceOutput {
        id,
        name: name.to_owned(),
        domain,
        node,
        pin: pin.to_owned(),
        sink: Some(sink),
    }
}

pub fn asset(document: BiomeGraphDocument) -> BiomeAsset {
    let mut seed_namespaces = document
        .nodes
        .iter()
        .flat_map(|node| node.seed_namespaces.iter())
        .map(|(name, value)| (format!("{name}-{value}"), *value))
        .collect::<Vec<_>>();
    seed_namespaces.extend([("palette-a".to_owned(), 31), ("palette-b".to_owned(), 32)]);
    BiomeAsset {
        version: BIOME_ASSET_VERSION,
        id: ROOT_BIOME,
        name: "Operator coverage".to_owned(),
        role: BiomeRole::Root,
        parameters: Vec::new(),
        palette: vec![
            BiomePaletteEntry {
                plant: FAMILY_A,
                weight: UnitInterval::ONE,
                seed_namespace: 31,
            },
            BiomePaletteEntry {
                plant: FAMILY_B,
                weight: UnitInterval::ONE,
                seed_namespace: 32,
            },
        ],
        density: FIXED_ONE,
        clustering: UnitInterval::ZERO,
        suitability: Vec::new(),
        competition: Vec::new(),
        companions: Vec::new(),
        succession: Vec::new(),
        seed_namespaces,
        modules: Vec::new(),
        policy: BiomeGraphPolicy {
            maximum_recursion: 8,
            maximum_influence_radius: DecisionScalar::from_bits(32 * 65_536),
            require_authoritative_fields: true,
        },
        graph: document.to_json(),
    }
}

fn operator_guid(operator: GraphOperator) -> u128 {
    GraphOperator::ALL
        .iter()
        .position(|candidate| *candidate == operator)
        .map_or(0, |index| index as u128 + 1)
}

pub fn comprehensive_module() -> BiomeAsset {
    let interface = node(operator_guid(O::InterfaceInput), O::InterfaceInput);
    let transform = node(101, O::Transform);
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: vec![GraphInterfaceInput {
            id: 201,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
        }],
        outputs: vec![GraphInterfaceOutput {
            id: 202,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: 101,
            pin: "candidates".to_owned(),
            sink: None,
        }],
        nodes: vec![interface, transform],
        edges: vec![edge(
            operator_guid(O::InterfaceInput),
            "value",
            101,
            "candidates",
        )],
    };
    let mut module = asset(document);
    module.id = MODULE_BIOME;
    module.name = "Operator coverage module".to_owned();
    module.role = BiomeRole::Module;
    module
}

pub fn comprehensive_asset() -> (BiomeAsset, FixtureResolver) {
    let guid = operator_guid;
    let mut nodes = GraphOperator::ALL
        .iter()
        .copied()
        .filter(|operator| *operator != O::InterfaceInput)
        .map(|operator| node(guid(operator), operator))
        .collect::<Vec<_>>();
    nodes
        .iter_mut()
        .find(|node| node.operator == O::ModuleCall)
        .unwrap()
        .parameters
        .insert("callGuid".to_owned(), GraphParameterValue::Guid(9_901));
    let mut edges = vec![
        edge(
            guid(O::RegionInput),
            "regions",
            guid(O::StratifiedCoverage),
            "regions",
        ),
        edge(
            guid(O::RegionInput),
            "regions",
            guid(O::BlueNoisePoisson),
            "regions",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::SurfaceProjection),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::FieldSample),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::PaintedTile),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::Noise),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::Gradient),
            "candidates",
        ),
        edge(guid(O::Noise), "field", guid(O::Curve), "field"),
        edge(guid(O::Noise), "field", guid(O::Remap), "field"),
        edge(guid(O::Noise), "field", guid(O::Combine), "left"),
        edge(guid(O::Gradient), "field", guid(O::Combine), "right"),
        edge(guid(O::Combine), "field", guid(O::Clamp), "field"),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::DistanceField),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::WeightedElimination),
            "candidates",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::WeightedElimination),
            "weights",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::VariableSpacing),
            "candidates",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::VariableSpacing),
            "radius",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::FieldImportance),
            "candidates",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::FieldImportance),
            "weights",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::ClusterPatchColony),
            "candidates",
        ),
        edge(
            guid(O::SplineInput),
            "splines",
            guid(O::SplineFollow),
            "splines",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::RecursiveCompanion),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::Transform),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::PriorityExclusion),
            "candidates",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::PriorityExclusion),
            "weights",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::PriorityExclusion),
            "radius",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::BoundsOverlap),
            "candidates",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::Competition),
            "candidates",
        ),
        edge(
            guid(O::CommunityInput),
            "communities",
            guid(O::Competition),
            "communities",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::Suitability),
            "candidates",
        ),
        edge(guid(O::Gradient), "field", guid(O::Suitability), "weights"),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::CommunityBlend),
            "candidates",
        ),
        edge(
            guid(O::CommunityInput),
            "communities",
            guid(O::CommunityBlend),
            "communities",
        ),
        edge(
            guid(O::CommunityBlend),
            "candidates",
            guid(O::SuccessionInput),
            "candidates",
        ),
        edge(
            guid(O::SuccessionInput),
            "candidates",
            guid(O::MacroOutput),
            "candidates",
        ),
        edge(
            guid(O::SpeciesInput),
            "species",
            guid(O::MacroOutput),
            "species",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::MicroOutput),
            "candidates",
        ),
        edge(guid(O::Gradient), "field", guid(O::MicroOutput), "density"),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::DiagnosticOutput),
            "candidates",
        ),
        edge(
            guid(O::Gradient),
            "field",
            guid(O::DiagnosticOutput),
            "field",
        ),
        edge(
            guid(O::StratifiedCoverage),
            "candidates",
            guid(O::ModuleCall),
            "candidates",
        ),
    ];
    edges.sort_by(|left, right| {
        (left.to_node, &left.to_pin, left.from_node, &left.from_pin).cmp(&(
            right.to_node,
            &right.to_pin,
            right.from_node,
            &right.from_pin,
        ))
    });
    let document = BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: vec![
            output(
                301,
                "macro",
                GraphDomain::MacroPoints,
                guid(O::MacroOutput),
                "points",
                GraphSink::Macro,
            ),
            output(
                302,
                "micro",
                GraphDomain::MicroField,
                guid(O::MicroOutput),
                "micro",
                GraphSink::Micro,
            ),
            output(
                303,
                "diagnostics",
                GraphDomain::Diagnostics,
                guid(O::DiagnosticOutput),
                "diagnostics",
                GraphSink::Diagnostics,
            ),
        ],
        nodes,
        edges,
    };
    let mut root = asset(document);
    root.suitability.push(SuitabilityBinding {
        channel: FieldChannel::Altitude,
        minimum: DecisionScalar::from_bits(0),
        maximum: FIXED_ONE,
        falloff: FIXED_ONE,
        node_guid: guid(O::Suitability),
    });
    root.modules.push(BiomeModuleReference {
        biome: MODULE_BIOME,
        call_guid: 9_901,
        bindings: Vec::new(),
    });
    let module = comprehensive_module();
    let resolver = FixtureResolver {
        modules: BTreeMap::from([(MODULE_BIOME.value(), module)]),
        ..FixtureResolver::default()
    };
    (root, resolver)
}

pub fn prototype(family: Uuid) -> PlantPrototype {
    PlantPrototype {
        family,
        crown_radius: [DecisionScalar::from_bits(16_384); 2],
        root_radius: [DecisionScalar::from_bits(16_384); 2],
        local_bounds_min: [DecisionScalar::from_bits(-8_192); 3],
        local_bounds_max: [DecisionScalar::from_bits(8_192); 3],
        shade_tolerance: UnitInterval::ONE,
        interaction_policy: saffron_vegetation::InteractionPolicy::Structural,
    }
}

pub fn evaluation_input(graph: &saffron_vegetation::CompiledBiomeGraph) -> GraphEvaluationInputs {
    let cell = WorldCellKey::base(0, 0, 0);
    let mut input = GraphEvaluationInputs::for_cell(MAP, 41, cell, graph.required_halo(0)).unwrap();
    input.plant_prototypes = vec![prototype(FAMILY_A), prototype(FAMILY_B)];
    input
}

pub fn evaluate_partitioned(
    asset: &BiomeAsset,
    resolver: &FixtureResolver,
    mut input: GraphEvaluationInputs,
) -> saffron_vegetation::GraphEvaluationResult {
    let graph = Arc::new(
        compile_biome_graph(asset, &[], resolver, GraphCompileOptions::canonical()).unwrap(),
    );
    input.read_bounds =
        GraphEvaluationInputs::for_cell(MAP, 41, input.output_cell, graph.required_halo(0))
            .unwrap()
            .read_bounds;
    BiomeGraphEvaluator::new(graph, 1)
        .unwrap()
        .evaluate(
            GraphEvaluationJobInputs {
                cells: vec![input],
                global_stages: Vec::new(),
            },
            &GraphCancellationToken::default(),
        )
        .unwrap()
        .cells
        .pop()
        .unwrap()
}

pub fn diagnostic_document(
    nodes: Vec<GraphNodeDefinition>,
    edges: Vec<GraphEdge>,
    diagnostic_nodes: &[(u128, &str)],
) -> BiomeGraphDocument {
    BiomeGraphDocument {
        version: BIOME_GRAPH_VERSION,
        interface_version: BIOME_INTERFACE_VERSION,
        inputs: Vec::new(),
        outputs: diagnostic_nodes
            .iter()
            .enumerate()
            .map(|(index, (node, label))| {
                output(
                    500 + index as u128,
                    label,
                    GraphDomain::Diagnostics,
                    *node,
                    "diagnostics",
                    GraphSink::Diagnostics,
                )
            })
            .collect(),
        nodes,
        edges,
    }
}
