use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_geometry::glam::{DVec3, Vec3};
use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, FieldAvailability, FieldChannel,
    FieldDerivative, FieldSample, HessianFieldSample, SurfaceAttachment, SurfaceCapabilities,
    SurfaceCoordinates, SurfaceDirtyRegion, SurfaceField, SurfaceFrame, SurfaceHit,
    SurfaceNearestQuery, SurfacePrimitiveId, SurfaceProjection, SurfaceProviderDescriptor,
    SurfaceProviderId, SurfaceRay, SurfaceRevision, SurfaceTagId, SurfaceTileDescriptor,
    UnitInterval, VectorFieldSample, WeightedSurfaceTag, WorldBounds, WorldCellKey, WorldPosition,
};
use saffron_vegetation::{
    BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_INTERFACE_VERSION, BIOME_NODE_VERSION,
    BiomeAsset, BiomeGraphDocument, BiomeGraphEvaluator, BiomeGraphPolicy, BiomeGraphResolver,
    BiomeModuleReference, BiomePaletteEntry, BiomeRole, CompanionRule, CompetitionRule,
    EvaluationFieldSource, EvaluationFieldTile, EvaluationRegion, EvaluationRegionKind,
    EvaluationSpline, GlobalStageEvaluationInputs, GraphAuthority, GraphCancellationToken,
    GraphClusterMode, GraphCombineOperation, GraphCompileOptions, GraphDependencySource,
    GraphDistanceSource, GraphDomain, GraphEdge, GraphEvaluationInputs, GraphEvaluationJobInputs,
    GraphInterfaceInput, GraphInterfaceOutput, GraphNodeDefinition, GraphOperator,
    GraphParameterType, GraphParameterValue, GraphSink, GraphSpatialRequirement, NodeSpatialPolicy,
    PlantPrototype, QuantizedFieldTileValues, QuantizedSurfaceFieldValue, Result, SuccessionRule,
    SuitabilityBinding, canonical_surface_provider_set_hash, compile_biome_graph,
    precompute_surface_field_tile, vegetation_content_hash,
};

const FAMILY_A: Uuid = Uuid(7_001);
const FAMILY_B: Uuid = Uuid(7_002);
const ROOT_BIOME: Uuid = Uuid(8_001);
const MODULE_BIOME: Uuid = Uuid(8_002);
const MAP: Uuid = Uuid(9_001);
const FIXED_ONE: DecisionScalar = DecisionScalar::from_bits(65_536);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorityContract {
    CanonicalCpu,
    QualifiedGpuCapable,
    DynamicModuleBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticFixture {
    ModuleBoundary,
    CanonicalInput,
    CandidateGeneration,
    SurfaceAndField,
    FieldMath,
    Distance,
    SpatialEliminator,
    CommunityAndEcology,
    Transform,
    Output,
}

#[derive(Clone, Copy, Debug)]
struct OperatorContract {
    operator: GraphOperator,
    spatial: GraphSpatialRequirement,
    authority: AuthorityContract,
    seeds: &'static [&'static str],
    fixture: SemanticFixture,
}

use AuthorityContract::{CanonicalCpu, DynamicModuleBoundary, QualifiedGpuCapable};
use GraphOperator as O;
use GraphSpatialRequirement::{FiniteSupport, Propagating};
use SemanticFixture::{
    CandidateGeneration, CanonicalInput, CommunityAndEcology, Distance, FieldMath, ModuleBoundary,
    Output, SpatialEliminator, SurfaceAndField, Transform,
};

const OPERATOR_CONTRACTS: &[OperatorContract] = &[
    OperatorContract {
        operator: O::InterfaceInput,
        spatial: FiniteSupport,
        authority: DynamicModuleBoundary,
        seeds: &[],
        fixture: ModuleBoundary,
    },
    OperatorContract {
        operator: O::RegionInput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CanonicalInput,
    },
    OperatorContract {
        operator: O::SplineInput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CanonicalInput,
    },
    OperatorContract {
        operator: O::SpeciesInput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CanonicalInput,
    },
    OperatorContract {
        operator: O::CommunityInput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CanonicalInput,
    },
    OperatorContract {
        operator: O::ExplicitAnchors,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CandidateGeneration,
    },
    OperatorContract {
        operator: O::StratifiedCoverage,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["sampling"],
        fixture: CandidateGeneration,
    },
    OperatorContract {
        operator: O::BlueNoisePoisson,
        spatial: Propagating,
        authority: CanonicalCpu,
        seeds: &["sampling"],
        fixture: CandidateGeneration,
    },
    OperatorContract {
        operator: O::SurfaceProjection,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SurfaceAndField,
    },
    OperatorContract {
        operator: O::FieldSample,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SurfaceAndField,
    },
    OperatorContract {
        operator: O::PaintedTile,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SurfaceAndField,
    },
    OperatorContract {
        operator: O::Noise,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &["noise"],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::Gradient,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::Curve,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::Remap,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::Combine,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::Clamp,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: FieldMath,
    },
    OperatorContract {
        operator: O::DistanceField,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: Distance,
    },
    OperatorContract {
        operator: O::WeightedElimination,
        spatial: Propagating,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SpatialEliminator,
    },
    OperatorContract {
        operator: O::VariableSpacing,
        spatial: Propagating,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SpatialEliminator,
    },
    OperatorContract {
        operator: O::FieldImportance,
        spatial: FiniteSupport,
        authority: QualifiedGpuCapable,
        seeds: &[],
        fixture: SpatialEliminator,
    },
    OperatorContract {
        operator: O::ClusterPatchColony,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["cluster"],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::SplineFollow,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CandidateGeneration,
    },
    OperatorContract {
        operator: O::RecursiveCompanion,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["companions"],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::Transform,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["variation"],
        fixture: Transform,
    },
    OperatorContract {
        operator: O::PriorityExclusion,
        spatial: Propagating,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SpatialEliminator,
    },
    OperatorContract {
        operator: O::BoundsOverlap,
        spatial: Propagating,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: SpatialEliminator,
    },
    OperatorContract {
        operator: O::Competition,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::Suitability,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::CommunityBlend,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["community"],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::SuccessionInput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["succession"],
        fixture: CommunityAndEcology,
    },
    OperatorContract {
        operator: O::MacroOutput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["species-selection"],
        fixture: Output,
    },
    OperatorContract {
        operator: O::MicroOutput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &["reconstruction"],
        fixture: Output,
    },
    OperatorContract {
        operator: O::DiagnosticOutput,
        spatial: FiniteSupport,
        authority: CanonicalCpu,
        seeds: &[],
        fixture: Output,
    },
    OperatorContract {
        operator: O::ModuleCall,
        spatial: FiniteSupport,
        authority: DynamicModuleBoundary,
        seeds: &[],
        fixture: ModuleBoundary,
    },
];

#[derive(Default)]
struct FixtureResolver {
    modules: BTreeMap<u64, BiomeAsset>,
    dependency_hashes: BTreeMap<GraphDependencySource, [u8; 32]>,
    available: Vec<GraphDependencySource>,
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

fn dependency_hash(source: GraphDependencySource) -> [u8; 32] {
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

fn node(guid: u128, operator: GraphOperator) -> GraphNodeDefinition {
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

fn edge(from_node: u128, from_pin: &str, to_node: u128, to_pin: &str) -> GraphEdge {
    GraphEdge {
        from_node,
        from_pin: from_pin.to_owned(),
        to_node,
        to_pin: to_pin.to_owned(),
    }
}

fn output(
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

fn asset(document: BiomeGraphDocument) -> BiomeAsset {
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

fn comprehensive_module() -> BiomeAsset {
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

fn comprehensive_asset() -> (BiomeAsset, FixtureResolver) {
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

fn collect_compiled_operators(
    unit: &saffron_vegetation::CompiledGraphUnit,
    operators: &mut BTreeSet<GraphOperator>,
) {
    for node in &unit.nodes {
        operators.insert(node.definition.operator);
        if let Some(module) = &node.module {
            collect_compiled_operators(module, operators);
        }
    }
}

#[test]
fn operator_contract_table_and_production_compiler_cover_the_complete_inventory() {
    let declared = OPERATOR_CONTRACTS
        .iter()
        .map(|contract| contract.operator)
        .collect::<Vec<_>>();
    assert_eq!(declared, GraphOperator::ALL);
    assert_eq!(
        declared.iter().copied().collect::<BTreeSet<_>>().len(),
        declared.len()
    );

    for contract in OPERATOR_CONTRACTS {
        assert_eq!(
            contract.operator.spatial_requirement(),
            contract.spatial,
            "{} spatial contract drifted",
            contract.operator.as_wire()
        );
        assert_eq!(
            contract.operator.seed_namespace_names(),
            contract.seeds,
            "{} seed contract drifted",
            contract.operator.as_wire()
        );
        assert_eq!(
            contract.operator.has_slang_executor(),
            contract.authority == QualifiedGpuCapable,
            "{} authority/executor contract drifted",
            contract.operator.as_wire()
        );
        let input_names = contract
            .operator
            .input_pins()
            .into_iter()
            .map(|pin| pin.name)
            .collect::<Vec<_>>();
        let output_names = contract
            .operator
            .output_pins()
            .into_iter()
            .map(|pin| pin.name)
            .collect::<Vec<_>>();
        let parameter_names = contract
            .operator
            .parameter_schema()
            .into_iter()
            .map(|parameter| parameter.name)
            .collect::<Vec<_>>();
        assert_eq!(
            input_names.iter().collect::<BTreeSet<_>>().len(),
            input_names.len(),
            "{} has duplicate input pins",
            contract.operator.as_wire()
        );
        assert_eq!(
            output_names.iter().collect::<BTreeSet<_>>().len(),
            output_names.len(),
            "{} has duplicate output pins",
            contract.operator.as_wire()
        );
        assert_eq!(
            parameter_names
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            parameter_names.len(),
            "{} has duplicate parameters",
            contract.operator.as_wire()
        );
        assert!(matches!(
            contract.fixture,
            ModuleBoundary
                | CanonicalInput
                | CandidateGeneration
                | SurfaceAndField
                | FieldMath
                | Distance
                | SpatialEliminator
                | CommunityAndEcology
                | Transform
                | Output
        ));
    }

    let (asset, resolver) = comprehensive_asset();
    let compiled =
        compile_biome_graph(&asset, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut compiled_operators = BTreeSet::new();
    collect_compiled_operators(&compiled.root, &mut compiled_operators);
    assert_eq!(
        compiled_operators,
        GraphOperator::ALL.iter().copied().collect()
    );
    for node in &compiled.root.nodes {
        assert!(node.capabilities.reference_cpu && node.capabilities.parallel_cpu);
        assert_eq!(
            node.capabilities.slang_compute,
            node.definition.operator.has_slang_executor()
        );
        assert_eq!(
            node.parameter_schema,
            node.definition.operator.parameter_schema()
        );
    }
}

fn prototype(family: Uuid) -> PlantPrototype {
    PlantPrototype {
        family,
        crown_radius: [DecisionScalar::from_bits(16_384); 2],
        root_radius: [DecisionScalar::from_bits(16_384); 2],
        local_bounds_min: [DecisionScalar::from_bits(-8_192); 3],
        local_bounds_max: [DecisionScalar::from_bits(8_192); 3],
        shade_tolerance: UnitInterval::ONE,
    }
}

fn evaluation_input(graph: &saffron_vegetation::CompiledBiomeGraph) -> GraphEvaluationInputs {
    let cell = WorldCellKey::base(0, 0, 0);
    let mut input = GraphEvaluationInputs::for_cell(MAP, 41, cell, graph.required_halo(0)).unwrap();
    input.plant_prototypes = vec![prototype(FAMILY_A), prototype(FAMILY_B)];
    input
}

fn evaluate_partitioned(
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

fn diagnostic_document(
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

#[derive(Clone)]
struct CanonicalSurfaceProvider {
    descriptor: SurfaceProviderDescriptor,
}

impl CanonicalSurfaceProvider {
    fn new() -> Self {
        Self {
            descriptor: SurfaceProviderDescriptor {
                id: SurfaceProviderId(77),
                revision: SurfaceRevision(3),
                bounds: WorldBounds::new([-1_000_000_000_000; 3], [1_000_000_000_000; 3]).unwrap(),
                primitive_count: 1,
                max_tags_per_hit: 2,
                capabilities: SurfaceCapabilities {
                    ray: true,
                    project: true,
                    nearest: true,
                    uv: false,
                    authoritative_attachments: true,
                    authoritative_fields: true,
                },
            },
        }
    }

    fn hit(&self, position: WorldPosition, distance_m: f64) -> saffron_spatial::Result<SurfaceHit> {
        Ok(SurfaceHit {
            provider: self.descriptor.id,
            position,
            distance_m,
            frame: SurfaceFrame::from_normal(Vec3::Y)?,
            coordinates: SurfaceCoordinates {
                uv: None,
                projection: DVec3::new(0.25, 0.5, 0.75),
            },
            attachment: Some(SurfaceAttachment::new(
                self.descriptor.id,
                SurfacePrimitiveId(901),
                [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                self.descriptor.revision,
            )?),
            tags: vec![
                WeightedSurfaceTag {
                    tag: SurfaceTagId(5),
                    weight: UnitInterval::from_bits(40_000),
                },
                WeightedSurfaceTag {
                    tag: SurfaceTagId(9),
                    weight: UnitInterval::from_bits(25_535),
                },
            ],
            revision: self.descriptor.revision,
        })
    }

    fn require_altitude(&self, channel: FieldChannel) -> saffron_spatial::Result<()> {
        if channel == FieldChannel::Altitude {
            Ok(())
        } else {
            Err(saffron_spatial::Error::FieldUnavailable)
        }
    }
}

impl SurfaceField for CanonicalSurfaceProvider {
    fn descriptor(&self) -> SurfaceProviderDescriptor {
        self.descriptor.clone()
    }

    fn field_channels(&self) -> Vec<FieldChannel> {
        vec![FieldChannel::Altitude]
    }

    fn raycast(&self, query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let projection =
            SurfaceProjection::new(query.origin, query.direction, query.max_distance_m)?;
        self.project(&projection)
    }

    fn project(&self, query: &SurfaceProjection) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if query.direction.x <= 0.0
            || query.direction.y >= 0.0
            || (query.direction.x.abs() - query.direction.y.abs()).abs() > 1.0e-12
        {
            return Err(saffron_spatial::Error::DegenerateDirection);
        }
        let origin = query.origin.global_ticks();
        if origin[1] < 0 {
            return Ok(None);
        }
        let projected = WorldPosition::from_global_ticks([
            origin[0]
                .checked_add(origin[1])
                .ok_or(saffron_spatial::Error::NumericOverflow)?,
            0,
            origin[2],
        ])?;
        let vertical_metres = origin[1] as f64 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
        let distance_m = vertical_metres * 2.0_f64.sqrt();
        if distance_m > query.max_distance_m {
            return Ok(None);
        }
        self.hit(projected, distance_m).map(Some)
    }

    fn nearest(&self, query: &SurfaceNearestQuery) -> saffron_spatial::Result<Option<SurfaceHit>> {
        let ticks = query.position.global_ticks();
        let distance_m =
            ticks[1].unsigned_abs() as f64 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
        if distance_m > query.max_distance_m {
            return Ok(None);
        }
        self.hit(
            WorldPosition::from_global_ticks([ticks[0], 0, ticks[2]])?,
            distance_m,
        )
        .map(Some)
    }

    fn availability(
        &self,
        channel: FieldChannel,
        _derivative: FieldDerivative,
        _bounds: WorldBounds,
    ) -> FieldAvailability {
        if channel == FieldChannel::Altitude {
            FieldAvailability::Complete
        } else {
            FieldAvailability::Unavailable
        }
    }

    fn estimated_samples(&self, channel: FieldChannel, _bounds: WorldBounds) -> u64 {
        u64::from(channel == FieldChannel::Altitude)
    }

    fn sample_scalar(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<FieldSample> {
        self.require_altitude(channel)?;
        if derivative != FieldDerivative::Value {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        Ok(FieldSample {
            channel,
            derivative,
            value: DecisionScalar::from_bits(12_345),
            revision: self.descriptor.revision,
        })
    }

    fn sample_vector(
        &self,
        channel: FieldChannel,
        derivative: FieldDerivative,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<VectorFieldSample> {
        self.require_altitude(channel)?;
        if derivative != FieldDerivative::Gradient {
            return Err(saffron_spatial::Error::FieldUnavailable);
        }
        Ok(VectorFieldSample {
            channel,
            derivative,
            value: DecisionVec3 {
                x: DecisionScalar::from_bits(100),
                y: DecisionScalar::from_bits(200),
                z: DecisionScalar::from_bits(300),
            },
            revision: self.descriptor.revision,
        })
    }

    fn sample_hessian(
        &self,
        channel: FieldChannel,
        _position: WorldPosition,
    ) -> saffron_spatial::Result<HessianFieldSample> {
        self.require_altitude(channel)?;
        Ok(HessianFieldSample {
            channel,
            derivative: FieldDerivative::Hessian,
            value: DecisionHessian3 {
                xx: DecisionScalar::from_bits(400),
                xy: DecisionScalar::from_bits(500),
                xz: DecisionScalar::from_bits(600),
                yy: DecisionScalar::from_bits(700),
                yz: DecisionScalar::from_bits(800),
                zz: DecisionScalar::from_bits(900),
            },
            revision: self.descriptor.revision,
        })
    }

    fn authoritative_tiles(
        &self,
        channel: FieldChannel,
        bounds: WorldBounds,
    ) -> Vec<SurfaceTileDescriptor> {
        if channel != FieldChannel::Altitude {
            return Vec::new();
        }
        vec![SurfaceTileDescriptor {
            provider: self.descriptor.id,
            revision: self.descriptor.revision,
            bounds,
            dimensions: [2, 2, 2],
            value_quantum_bits: 1,
        }]
    }

    fn changes_since(&self, _revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion> {
        Vec::new()
    }

    fn reproject_attachment(
        &self,
        attachment: SurfaceAttachment,
    ) -> saffron_spatial::Result<Option<SurfaceHit>> {
        if attachment.provider != self.descriptor.id
            || attachment.primitive != SurfacePrimitiveId(901)
        {
            return Ok(None);
        }
        self.hit(WorldPosition::origin(), 0.0).map(Some)
    }
}

#[test]
fn canonical_provider_precomputes_all_derivatives_and_replays_live_queries() {
    let region = node(601, O::RegionInput);
    let mut coverage = node(602, O::StratifiedCoverage);
    coverage
        .parameters
        .insert("count".to_owned(), GraphParameterValue::U32(4));
    let mut projection = node(603, O::SurfaceProjection);
    projection.parameters.insert(
        "direction".to_owned(),
        GraphParameterValue::FixedVec3([
            FIXED_ONE,
            DecisionScalar::from_bits(-65_536),
            DecisionScalar::from_bits(0),
        ]),
    );
    projection.parameters.insert(
        "maxDistance".to_owned(),
        GraphParameterValue::Fixed(DecisionScalar::from_bits(64 * 65_536)),
    );
    projection
        .parameters
        .insert("provider".to_owned(), GraphParameterValue::U64(77));
    projection
        .parameters
        .insert("tags".to_owned(), GraphParameterValue::TagList(vec![5]));
    projection.parameters.insert(
        "materialTags".to_owned(),
        GraphParameterValue::TagList(vec![9]),
    );
    projection.spatial = NodeSpatialPolicy::Partitioned {
        level: 0,
        influence_radius: DecisionScalar::from_bits(64 * 65_536),
    };
    let mut value = node(604, O::FieldSample);
    value.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    value.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Value),
    );
    let mut gradient = node(605, O::FieldSample);
    gradient.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    gradient.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Gradient),
    );
    let mut hessian = node(606, O::FieldSample);
    hessian.parameters.insert(
        "channel".to_owned(),
        GraphParameterValue::FieldChannel(FieldChannel::Altitude),
    );
    hessian.parameters.insert(
        "derivative".to_owned(),
        GraphParameterValue::FieldDerivative(FieldDerivative::Hessian),
    );
    let mut field_diagnostic = node(607, O::DiagnosticOutput);
    field_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("canonical-field".to_owned()),
    );
    let mut projection_diagnostic = node(608, O::DiagnosticOutput);
    projection_diagnostic.parameters.insert(
        "label".to_owned(),
        GraphParameterValue::String("canonical-projection".to_owned()),
    );
    let document = diagnostic_document(
        vec![
            region,
            coverage,
            projection,
            value,
            gradient,
            hessian,
            field_diagnostic,
            projection_diagnostic,
        ],
        vec![
            edge(601, "regions", 602, "regions"),
            edge(602, "candidates", 603, "candidates"),
            edge(602, "candidates", 604, "candidates"),
            edge(602, "candidates", 605, "candidates"),
            edge(602, "candidates", 606, "candidates"),
            edge(602, "candidates", 607, "candidates"),
            edge(604, "field", 607, "field"),
            edge(603, "candidates", 608, "candidates"),
        ],
        &[(607, "field-output"), (608, "projection-output")],
    );
    let provider = Arc::new(CanonicalSurfaceProvider::new());
    let provider_dyn: Arc<dyn SurfaceField> = provider.clone();
    let provider_hash =
        canonical_surface_provider_set_hash(std::slice::from_ref(&provider_dyn), 1).unwrap();
    let mut root = asset(document);
    root.policy.maximum_influence_radius = DecisionScalar::from_bits(64 * 65_536);
    let resolver = FixtureResolver {
        dependency_hashes: BTreeMap::from([(
            GraphDependencySource::SurfaceProvider(77),
            provider_hash,
        )]),
        available: vec![GraphDependencySource::SurfaceProvider(77)],
        ..FixtureResolver::default()
    };
    let graph =
        compile_biome_graph(&root, &[], &resolver, GraphCompileOptions::canonical()).unwrap();
    let mut input = evaluation_input(&graph);
    let output_bounds = input.output_bounds;
    input.set_hierarchical_region(611, output_bounds).unwrap();
    input.surface_provider_set_hash = provider_hash;
    input.surface_providers = vec![provider_dyn];
    assert_eq!(
        provider.availability(
            FieldChannel::Altitude,
            FieldDerivative::Hessian,
            input.read_bounds,
        ),
        FieldAvailability::Complete
    );
    assert_eq!(
        provider
            .authoritative_tiles(FieldChannel::Altitude, input.read_bounds)
            .as_slice(),
        &[SurfaceTileDescriptor {
            provider: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: input.read_bounds,
            dimensions: [2, 2, 2],
            value_quantum_bits: 1,
        }]
    );
    let descriptor = provider
        .authoritative_tiles(FieldChannel::Altitude, input.read_bounds)
        .pop()
        .unwrap();
    let cancellation = GraphCancellationToken::default();
    for (derivative, expected) in [
        (
            FieldDerivative::Value,
            QuantizedFieldTileValues::Scalar(vec![12_345; 8]),
        ),
        (
            FieldDerivative::Gradient,
            QuantizedFieldTileValues::Gradient(vec![[100, 200, 300]; 8]),
        ),
        (
            FieldDerivative::Hessian,
            QuantizedFieldTileValues::Hessian(vec![[400, 500, 600, 700, 800, 900]; 8]),
        ),
    ] {
        let tile = precompute_surface_field_tile(
            provider.as_ref(),
            descriptor,
            FieldChannel::Altitude,
            derivative,
            provider_hash,
            &cancellation,
        )
        .unwrap();
        assert_eq!(tile.values, expected);
    }
    let evaluator = BiomeGraphEvaluator::new(Arc::new(graph), 1).unwrap();
    let live_job = GraphEvaluationJobInputs {
        cells: vec![input],
        global_stages: Vec::new(),
    };
    let live_bound = evaluator.preflight(&live_job, &cancellation).unwrap();
    let mut result = evaluator
        .evaluate(live_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert!(live_bound.input_tiles >= 3);
    let mut replay_input = evaluation_input(evaluator.graph());
    let replay_bounds = replay_input.output_bounds;
    replay_input
        .set_hierarchical_region(611, replay_bounds)
        .unwrap();
    replay_input.surface_provider_set_hash = provider_hash;
    replay_input.surface_projection_tiles = result.surface_projection_tiles.clone();
    replay_input.surface_field_query_tiles = result.surface_field_query_tiles.clone();
    let mut mismatched_projection = replay_input.clone();
    mismatched_projection.surface_projection_tiles[0].provider_set_hash = [0xA5; 32];
    let error = evaluator
        .preflight(
            &GraphEvaluationJobInputs {
                cells: vec![mismatched_projection],
                global_stages: Vec::new(),
            },
            &cancellation,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        saffron_vegetation::Error::GraphDocument { path, .. }
            if path == "evaluation.surfaceProjectionTiles"
    ));
    let mut mismatched_field_query = replay_input.clone();
    mismatched_field_query.surface_field_query_tiles[0].provider_set_hash = [0x5A; 32];
    let error = evaluator
        .preflight(
            &GraphEvaluationJobInputs {
                cells: vec![mismatched_field_query],
                global_stages: Vec::new(),
            },
            &cancellation,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        saffron_vegetation::Error::GraphDocument { path, .. }
            if path == "evaluation.surfaceFieldQueryTiles"
    ));
    let replay_job = GraphEvaluationJobInputs {
        cells: vec![replay_input],
        global_stages: Vec::new(),
    };
    let replay_bound = evaluator.preflight(&replay_job, &cancellation).unwrap();
    assert_eq!(replay_bound.input_tiles, 3);
    let replay = evaluator
        .evaluate(replay_job, &cancellation)
        .unwrap()
        .cells
        .pop()
        .unwrap();
    assert_eq!(
        replay.canonical_byte_len().unwrap(),
        replay.canonical_bytes().unwrap().len()
    );
    assert!(replay_bound.memory_bytes >= replay.canonical_bytes().unwrap().len() as u64);
    result = replay;
    assert_eq!(result.surface_projection_tiles.len(), 1);
    let projection_tile = &result.surface_projection_tiles[0];
    assert_eq!(projection_tile.node, 603);
    assert_eq!(projection_tile.provider_set_hash, provider_hash);
    assert_eq!(projection_tile.samples.len(), 4);
    for entry in &projection_tile.samples {
        let sample = entry.sample.as_ref().unwrap();
        let query = entry.query.global_ticks();
        assert_eq!(
            sample.position.global_ticks(),
            [query[0] + query[1], 0, query[2]]
        );
        let attachment = sample.attachment;
        assert_eq!(attachment.provider, SurfaceProviderId(77));
        assert_eq!(attachment.primitive, SurfacePrimitiveId(901));
        assert_eq!(attachment.revision, SurfaceRevision(3));
        assert_eq!(
            sample.tags,
            vec![
                WeightedSurfaceTag {
                    tag: SurfaceTagId(5),
                    weight: UnitInterval::from_bits(40_000)
                },
                WeightedSurfaceTag {
                    tag: SurfaceTagId(9),
                    weight: UnitInterval::from_bits(25_535)
                },
            ]
        );
    }
    assert_eq!(result.surface_field_query_tiles.len(), 1);
    for tile in &result.surface_field_query_tiles {
        assert_eq!(tile.channel, FieldChannel::Altitude);
        assert_eq!(tile.provider_set_hash, provider_hash);
        assert_eq!(tile.samples.len(), 4);
        for entry in &tile.samples {
            let expected = match tile.derivative {
                FieldDerivative::Value => QuantizedSurfaceFieldValue::Scalar(12_345),
                FieldDerivative::Gradient => QuantizedSurfaceFieldValue::Gradient([100, 200, 300]),
                FieldDerivative::Hessian => {
                    QuantizedSurfaceFieldValue::Hessian([400, 500, 600, 700, 800, 900])
                }
            };
            assert_eq!(entry.value, expected);
        }
    }
    let field_stream = result
        .diagnostics
        .streams
        .iter()
        .find(|stream| stream.label == "canonical-field")
        .unwrap();
    let projection_stream = result
        .diagnostics
        .streams
        .iter()
        .find(|stream| stream.label == "canonical-projection")
        .unwrap();
    let field_candidates = field_stream
        .candidates
        .as_ref()
        .unwrap()
        .iter()
        .map(|candidate| candidate.identity)
        .collect::<BTreeSet<_>>();
    let field_values = field_stream
        .field
        .as_ref()
        .unwrap()
        .iter()
        .map(|sample| sample.candidate)
        .collect::<BTreeSet<_>>();
    let projection_candidates = projection_stream
        .candidates
        .as_ref()
        .unwrap()
        .iter()
        .map(|candidate| candidate.identity)
        .collect::<BTreeSet<_>>();
    assert_eq!(field_candidates, field_values);
    assert_eq!(field_candidates, projection_candidates);
}
