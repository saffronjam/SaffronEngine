use std::collections::BTreeSet;

use saffron_vegetation::{
    GraphCompileOptions, GraphOperator, GraphSpatialRequirement, compile_biome_graph,
};

use crate::fixtures::comprehensive_asset;
use AuthorityContract::{CanonicalCpu, DynamicModuleBoundary, QualifiedGpuCapable};
use GraphOperator as O;
use GraphSpatialRequirement::{FiniteSupport, Propagating};
use SemanticFixture::{
    CandidateGeneration, CanonicalInput, CommunityAndEcology, Distance, FieldMath, ModuleBoundary,
    Output, SpatialEliminator, SurfaceAndField, Transform,
};

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
