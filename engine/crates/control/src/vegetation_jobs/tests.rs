use std::sync::Arc;

use saffron_protocol::{
    ControlFailureDto, VegetationDiagnosticResultSourceDto, VegetationDiagnosticStreamScopeDto,
    VegetationEvaluationJobStateDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    CandidateIdentity, CandidateRejectionReason, DiagnosticCandidateSample, DiagnosticScalarSample,
    DiagnosticStreamScope, GraphCancellationToken, GraphEvaluationJobResult,
    GraphEvaluationPreflight, GraphEvaluationResult, GraphNodeAddress, NamedDiagnosticStream,
    RejectedCandidate,
};

use super::*;
use crate::owned_worker::OwnedWorker;
use saffron_spatial::{DecisionScalar, WorldPosition};
use saffron_vegetation::{
    CandidateLineage, GlobalStageEvaluationResult, GpuGroupEvaluationDiagnostic,
    GraphEvaluationDiagnostics, GraphSafetyLimits, PlantPointColumns, ProvenanceHandle,
    ProvenanceTable, vegetation_content_hash,
};

fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
    GraphEvaluationResult {
        cell,
        macro_points: PlantPointColumns::from_points(Vec::new()).unwrap(),
        micro_fields: Vec::new(),
        surface_projection_tiles: Vec::new(),
        surface_field_query_tiles: Vec::new(),
        ancestor_references: Vec::new(),
        provenance: ProvenanceTable::default(),
        diagnostics: GraphEvaluationDiagnostics::default(),
    }
}

fn running_job_with_outcome(
    cancellation: GraphCancellationToken,
    outcome: EvaluationOutcome,
) -> EvaluationJob {
    EvaluationJob {
        cancellation,
        preflight: sample_preflight(),
        state: EvaluationJobState::Running {
            worker: OwnedWorker::completed(outcome),
        },
    }
}

fn failed_job(error: impl Into<String>) -> EvaluationJob {
    EvaluationJob {
        cancellation: GraphCancellationToken::default(),
        preflight: sample_preflight(),
        state: EvaluationJobState::Failed(ControlFailureDto::Command {
            message: error.into(),
        }),
    }
}

fn sample_preflight() -> GraphEvaluationPreflight {
    GraphEvaluationPreflight {
        output_cells: 1,
        global_stage_tiles: 2,
        input_tiles: 3,
        retained_input_bytes: 4,
        generated_input_bytes: 5,
        candidate_count: 6,
        accepted_count: 7,
        micro_samples: 8,
        preflight_peak_bytes: 9,
        execution_peak_bytes: 10,
        memory_bytes: 10,
        transfer_bytes: 11,
        worker_count: 1,
        time_limit_ms: 12,
        limits: GraphSafetyLimits::default(),
    }
}

fn completed_job(canonical_bytes: u64) -> EvaluationJob {
    let results = GraphEvaluationJobResult::default();
    let (summary, measured_bytes) = summarize(&results).unwrap();
    assert!(measured_bytes > 0);
    EvaluationJob {
        cancellation: GraphCancellationToken::default(),
        preflight: sample_preflight(),
        state: EvaluationJobState::Completed {
            results: Arc::new(results),
            summary: Box::new(summary),
            canonical_bytes,
        },
    }
}

fn reference_job_canonical_bytes(results: &GraphEvaluationJobResult) -> Vec<u8> {
    let mut canonical = b"saffron-anima/vegetation-evaluation-job/v2\0".to_vec();
    canonical.extend_from_slice(&u64::try_from(results.cells.len()).unwrap().to_be_bytes());
    for result in &results.cells {
        let bytes = result.canonical_bytes().unwrap();
        canonical.push(0);
        canonical.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_be_bytes());
        canonical.extend_from_slice(&bytes);
    }
    canonical.extend_from_slice(
        &u64::try_from(results.global_stages.len())
            .unwrap()
            .to_be_bytes(),
    );
    for stage in &results.global_stages {
        let bytes = stage.result.canonical_bytes().unwrap();
        canonical.push(1);
        canonical.extend_from_slice(&stage.stage);
        canonical.extend_from_slice(&stage.owner.canonical_bytes());
        canonical.extend_from_slice(&stage.resident_bytes.to_be_bytes());
        canonical.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_be_bytes());
        canonical.extend_from_slice(&bytes);
    }
    canonical
}

#[test]
fn terminal_count_retention_evicts_the_oldest_unprotected_job() {
    let mut jobs = VegetationEvaluationJobs::default();
    let protected = u64::try_from(MAX_TERMINAL_JOBS).unwrap() + 1;
    for job_id in 1..=protected {
        jobs.jobs
            .insert(job_id, failed_job(format!("job {job_id}")));
    }

    jobs.maintain(Some(protected)).unwrap();

    assert_eq!(jobs.jobs.len(), MAX_TERMINAL_JOBS);
    assert!(!jobs.jobs.contains_key(&1));
    assert!(jobs.jobs.contains_key(&protected));
}

#[test]
fn retained_result_cap_evicts_an_older_completed_result() {
    let mut jobs = VegetationEvaluationJobs::default();
    jobs.jobs
        .insert(1, completed_job(MAX_RETAINED_CANONICAL_BYTES));
    jobs.jobs.insert(2, completed_job(1));

    jobs.maintain(Some(2)).unwrap();

    assert!(!jobs.jobs.contains_key(&1));
    assert!(jobs.jobs.contains_key(&2));
}

#[test]
fn cancellation_requested_before_refresh_wins_a_queued_success() {
    let cancellation = GraphCancellationToken::default();
    let mut job = running_job_with_outcome(
        cancellation.clone(),
        Ok(GraphEvaluationJobResult::default()),
    );
    cancellation.cancel();

    refresh(&mut job);

    assert!(matches!(&job.state, EvaluationJobState::Cancelled));
}

#[test]
fn completed_result_observed_before_cancel_remains_completed() {
    let mut jobs = VegetationEvaluationJobs::default();
    jobs.jobs.insert(
        1,
        running_job_with_outcome(
            GraphCancellationToken::default(),
            Ok(GraphEvaluationJobResult::default()),
        ),
    );

    let status = jobs.cancel(1).unwrap();

    assert_eq!(status.state, VegetationEvaluationJobStateDto::Completed);
    assert_eq!(status.preflight.output_cells, "1");
    assert_eq!(status.preflight.global_stage_tiles, "2");
    assert_eq!(status.preflight.retained_input_bytes, "4");
    assert_eq!(status.preflight.generated_input_bytes, "5");
    assert_eq!(status.preflight.preflight_peak_bytes, "9");
    assert_eq!(status.preflight.execution_peak_bytes, "10");
    assert_eq!(status.preflight.memory_bytes, "10");
    assert!(matches!(
        &jobs.jobs.get(&1).unwrap().state,
        EvaluationJobState::Completed { .. }
    ));
}

#[test]
fn summary_frames_global_stage_identity_and_resident_bytes() {
    let owner = WorldCellKey::base(0, 0, 0);
    let results = GraphEvaluationJobResult {
        cells: vec![empty_result(owner)],
        global_stages: vec![GlobalStageEvaluationResult {
            stage: [7; 32],
            owner,
            result: empty_result(owner),
            resident_bytes: 4096,
        }],
    };
    let reference = reference_job_canonical_bytes(&results);
    let (summary, retained) = summarize(&results).unwrap();
    assert_eq!(summary.cells, "1");
    assert_eq!(summary.global_stages, "1");
    assert_eq!(summary.global_resident_bytes, "4096");
    assert_eq!(
        summary.canonical_hash,
        hex_hash(vegetation_content_hash(&reference))
    );
    assert_eq!(retained, u64::try_from(reference.len()).unwrap());

    let mut changed = results;
    changed.global_stages[0].stage[0] ^= 1;
    let (changed_summary, _) = summarize(&changed).unwrap();
    assert_ne!(summary.canonical_hash, changed_summary.canonical_hash);
}

#[test]
fn summary_aggregates_actual_gpu_work_by_canonical_group_boundary() {
    let owner = WorldCellKey::base(0, 0, 0);
    let nodes = vec![
        GraphNodeAddress {
            module_path: vec![1],
            node: 2,
        },
        GraphNodeAddress {
            module_path: vec![1],
            node: 3,
        },
    ];
    let mut cell = empty_result(owner);
    cell.diagnostics
        .gpu_groups
        .push(GpuGroupEvaluationDiagnostic {
            nodes: nodes.clone(),
            invocation_count: 10,
            transfer_bytes: 100,
            output_bytes: 80,
            elapsed_micros: 7,
        });
    let mut global = empty_result(owner);
    global
        .diagnostics
        .gpu_groups
        .push(GpuGroupEvaluationDiagnostic {
            nodes,
            invocation_count: 20,
            transfer_bytes: 200,
            output_bytes: 160,
            elapsed_micros: 11,
        });
    let results = GraphEvaluationJobResult {
        cells: vec![cell],
        global_stages: vec![GlobalStageEvaluationResult {
            stage: [7; 32],
            owner,
            result: global,
            resident_bytes: 4096,
        }],
    };

    let (summary, _) = summarize(&results).unwrap();

    assert_eq!(summary.gpu_groups.len(), 1);
    let group = &summary.gpu_groups[0];
    assert_eq!(group.invocation_count, "30");
    assert_eq!(group.transfer_bytes, "300");
    assert_eq!(group.output_bytes, "240");
    assert_eq!(group.elapsed_micros, "18");
    assert_eq!(group.nodes.len(), 2);
    assert_eq!(group.nodes[0].module_path[0].0, format!("{:032x}", 1));
    assert_eq!(group.nodes[0].node.0, format!("{:032x}", 2));
    assert_eq!(group.nodes[1].node.0, format!("{:032x}", 3));
}

#[test]
fn summary_merges_named_streams_with_exact_result_scopes() {
    let owner = WorldCellKey::base(0, 0, 0);
    let node = GraphNodeAddress {
        module_path: vec![1],
        node: 2,
    };
    let stream = |identity: CandidateIdentity, value: i32, provenance: u32| NamedDiagnosticStream {
        node: node.clone(),
        label: "density audit".to_owned(),
        scope: DiagnosticStreamScope::CandidateLineage(CandidateLineage(9)),
        candidates: Some(vec![DiagnosticCandidateSample {
            identity,
            owner,
            position: WorldPosition::from_global_ticks([i128::from(value), 2, 3]).unwrap(),
            family: None,
            variation: 4,
            priority: DecisionScalar::from_bits(value),
            ecology_tick: 5,
        }]),
        field: Some(vec![DiagnosticScalarSample {
            candidate: identity,
            value: DecisionScalar::from_bits(value),
        }]),
        rejected: vec![RejectedCandidate {
            candidate: identity,
            position: saffron_spatial::WorldPosition::origin(),
            reason: CandidateRejectionReason::Threshold,
            provenance: ProvenanceHandle(provenance),
        }],
    };
    let first_identity = CandidateIdentity {
        node: 10,
        node_address: 11,
        node_semantic_revision: 1,
        ordinal: 12,
        ancestor: 13,
    };
    let second_identity = CandidateIdentity {
        ordinal: 14,
        ..first_identity
    };
    let mut cell = empty_result(owner);
    cell.diagnostics.streams.push(stream(first_identity, 16, 7));
    let mut global = empty_result(owner);
    global
        .diagnostics
        .streams
        .push(stream(second_identity, 32, 8));
    let results = GraphEvaluationJobResult {
        cells: vec![cell],
        global_stages: vec![GlobalStageEvaluationResult {
            stage: [7; 32],
            owner,
            result: global,
            resident_bytes: 4096,
        }],
    };

    let (summary, _) = summarize(&results).unwrap();

    assert_eq!(summary.streams.len(), 1);
    let stream = &summary.streams[0];
    assert_eq!(stream.label, "density audit");
    assert_eq!(stream.node.node.0, format!("{:032x}", 2));
    assert_eq!(
        stream.scope,
        VegetationDiagnosticStreamScopeDto::CandidateLineage {
            lineage: crate::commands_vegetation::vegetation_guid(9),
        }
    );
    assert_eq!(stream.candidate_samples.as_ref().unwrap().len(), 2);
    assert_eq!(stream.scalar_samples.as_ref().unwrap().len(), 2);
    assert_eq!(stream.rejected.len(), 2);
    assert!(matches!(
        stream.candidate_samples.as_ref().unwrap()[0].source,
        VegetationDiagnosticResultSourceDto::Cell { .. }
    ));
    assert!(matches!(
        stream.candidate_samples.as_ref().unwrap()[1].source,
        VegetationDiagnosticResultSourceDto::GlobalStage { .. }
    ));
    assert_eq!(stream.rejected[0].provenance.handle, 7);
    assert_eq!(stream.rejected[1].provenance.handle, 8);
}
