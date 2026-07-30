use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use saffron_core::Uuid;
use saffron_spatial::{
    BASE_CELL_TICKS, DecisionHessian3, DecisionScalar, DecisionVec3, FieldAvailability,
    FieldChannel, FieldDerivative, FieldSample, HessianFieldSample, LOCAL_TICKS_PER_METER,
    SignedUnit, SurfaceAttachment, SurfaceCapabilities, SurfaceDirtyRegion, SurfaceField,
    SurfaceHit, SurfaceNearestQuery, SurfaceProjection, SurfaceProviderDescriptor,
    SurfaceProviderId, SurfaceRay, SurfaceRevision, SurfaceTileDescriptor, UnitInterval,
    VectorFieldSample, WeightedSurfaceTag, WorldBounds, WorldCellKey, WorldPosition,
    world_cell_count_covering_bounds,
};

use crate::hash::{VegetationContentHasher, sha256};
use crate::memory::{requested_btree_bytes, requested_vec_bytes};
use crate::{
    BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_NODE_VERSION, BiomeAsset, BiomeGraphDocument,
    BiomeGraphPolicy, BiomeGraphResolver, BiomeModuleReference, BiomePaletteEntry, BiomeRole,
    CompiledBiomeGraph, Error, GpuExecutionProfile, GpuQualificationRegistry,
    GpuShaderArtifactIdentity, GraphAuthority, GraphClusterMode, GraphCombineOperation,
    GraphCompileOptions, GraphComputeExecutor, GraphDependencySource, GraphDomain, GraphEdge,
    GraphExecutionDomain, GraphGpuInvocationBatch, GraphGpuProgram, GraphGpuScheduling,
    GraphInterfaceInput, GraphInterfaceOutput, GraphNodeAddress, GraphNodeDefinition,
    GraphOperator, GraphParameterValue, GraphSink, InteractionPolicy, NodeSpatialPolicy,
    PlantFlags, PlantId, PlantLifecycle, PlantPoint, PlantPointColumns, ProvenanceDecisionOutcome,
    ProvenanceHandle, ProvenanceTable, QualifiedGraphPin, QuantizedOrientation, Result,
    VegetationCellFacet, VegetationCellSectionKind, build_execution_plan, compile_biome_graph,
    decode_vegetation_cell_facet, evaluate_gpu_program_reference,
};

mod budget;
mod cancel;
mod canonical;
mod contracts;
mod demand;
mod documents;
mod fixtures;
mod modules;
mod parallel;
mod splines;
mod support;

use documents::*;
use fixtures::*;
use modules::*;
use support::*;

fn explicit_point(candidate: u64, layer: u128, position: WorldPosition) -> EvaluationAnchor {
    let ticks = position.global_ticks();
    EvaluationAnchor {
        layer,
        point: PlantPoint {
            id: PlantId::explicit([candidate as u8 + 1; 16]).unwrap(),
            owner: position.cell(),
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_integer(1).unwrap(); 3],
            bounds: WorldBounds::new(
                [ticks[0] - 1, ticks[1] - 1, ticks[2] - 1],
                [ticks[0] + 2, ticks[1] + 2, ticks[2] + 2],
            )
            .unwrap(),
            family: Uuid(702),
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: u128::from(candidate),
            candidate,
            parent: None,
            colony: None,
            ecology_tick: 0,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ONE,
            fuel: UnitInterval::ONE,
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::default(),
            interaction_policy: InteractionPolicy::Decorative,
            provenance: 0,
            attachment: None,
            surface_projection: [DecisionScalar::from_bits(0); 3],
        },
    }
}

fn input(cell: WorldCellKey, halo: DecisionScalar) -> GraphEvaluationInputs {
    let mut input = GraphEvaluationInputs::for_cell(Uuid(801), 91, cell, halo).unwrap();
    input.plant_prototypes.push(PlantPrototype {
        family: Uuid(702),
        crown_radius: [DecisionScalar::from_bits(65_536); 2],
        root_radius: [DecisionScalar::from_bits(65_536); 2],
        local_bounds_min: [
            DecisionScalar::from_bits(-65_536),
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(-65_536),
        ],
        local_bounds_max: [
            DecisionScalar::from_bits(65_536),
            DecisionScalar::from_bits(4 * 65_536),
            DecisionScalar::from_bits(65_536),
        ],
        shade_tolerance: UnitInterval::from_bits(32_768),
        interaction_policy: InteractionPolicy::Structural,
    });
    input
}

fn projection_provider() -> (Arc<dyn SurfaceField>, [u8; 32]) {
    let descriptor = SurfaceProviderDescriptor {
        id: SurfaceProviderId(77),
        revision: SurfaceRevision(3),
        bounds: WorldCellKey::new(0, 0, 0, 2).unwrap().bounds(),
        primitive_count: 1,
        max_tags_per_hit: 0,
        capabilities: SurfaceCapabilities {
            project: true,
            authoritative_attachments: true,
            ..SurfaceCapabilities::default()
        },
    };
    let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
        descriptor,
        failing_cell_x: None,
        project_hits: true,
        successful_samples: Arc::new(AtomicUsize::new(0)),
    });
    let provider_hash = canonical_surface_provider_set_hash(
        &[Arc::clone(&provider)],
        crate::GraphSafetyLimits::default().max_input_tiles,
    )
    .unwrap();
    (provider, provider_hash)
}

fn projection_job(
    graph: &CompiledBiomeGraph,
    provider: Arc<dyn SurfaceField>,
    provider_hash: [u8; 32],
) -> GraphEvaluationJobInputs {
    let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
    inputs.surface_provider_set_hash = provider_hash;
    inputs.surface_providers.push(provider);
    job(vec![inputs])
}

fn job(cells: Vec<GraphEvaluationInputs>) -> GraphEvaluationJobInputs {
    GraphEvaluationJobInputs {
        cells,
        global_stages: Vec::new(),
    }
}

fn assert_graph_limit(error: Error, resource: &'static str) {
    assert!(
        matches!(error, Error::GraphLimit { resource: actual, .. } if actual == resource),
        "expected {resource} limit, got {error:?}"
    );
}

fn global_job(
    graph: &CompiledBiomeGraph,
    cells: Vec<GraphEvaluationInputs>,
) -> GraphEvaluationJobInputs {
    let global_stages = expected_global_stage_tiles(graph, &cells)
        .unwrap()
        .into_iter()
        .map(|(stage_id, owner)| {
            let stage = graph.spatial_plan().global_stage(stage_id).unwrap();
            let mut inputs = input(owner, stage.upstream_halo);
            inputs.output_bounds = owner.bounds();
            inputs.read_bounds = expand_bounds_checked(
                owner.bounds(),
                fixed_meters_to_ticks(stage.upstream_halo)
                    .unwrap()
                    .unsigned_abs() as i128,
            )
            .unwrap();
            inputs.regions =
                canonical_cell_regions(inputs.read_bounds, stage.minimum_input_level, 0).unwrap();
            let mut snapshot = b"global-stage-test-input/v1\0".to_vec();
            snapshot.extend_from_slice(&stage_id);
            snapshot.extend_from_slice(&owner.canonical_bytes());
            GlobalStageEvaluationInputs {
                stage: stage_id,
                owner,
                solve_bounds: owner.bounds(),
                input_snapshot: sha256(&snapshot),
                inputs,
            }
        })
        .collect();
    GraphEvaluationJobInputs {
        cells,
        global_stages,
    }
}

fn position(columns: &PlantPointColumns, row: usize) -> WorldPosition {
    columns.positions[row]
}
