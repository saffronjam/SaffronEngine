use std::fmt::Write as _;

use saffron_protocol::{
    Uuid as WireUuid, VegetationCandidateIdentityDto, VegetationDiagnosticCandidateSampleDto,
    VegetationDiagnosticProvenanceIdDto, VegetationDiagnosticRejectionDto,
    VegetationDiagnosticResultSourceDto, VegetationDiagnosticScalarSampleDto,
    VegetationDiagnosticStreamScopeDto, VegetationEvaluationJobDto,
    VegetationEvaluationJobStateDto, VegetationEvaluationPreflightDto,
    VegetationEvaluationStatusDto, VegetationExecutionDomainDto, VegetationGraphNodeAddressDto,
    WorldCellDto,
};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    CandidateIdentity, DiagnosticCandidateSample, DiagnosticScalarSample, DiagnosticStreamScope,
    GraphEvaluationPreflight, GraphExecutionDomain, GraphNodeAddress, RejectedCandidate,
};

use super::*;

pub(crate) fn graph_node_address_dto(address: GraphNodeAddress) -> VegetationGraphNodeAddressDto {
    VegetationGraphNodeAddressDto {
        module_path: address
            .module_path
            .into_iter()
            .map(crate::commands_vegetation::vegetation_guid)
            .collect(),
        node: crate::commands_vegetation::vegetation_guid(address.node),
    }
}

pub(crate) fn diagnostic_stream_scope_dto(
    scope: DiagnosticStreamScope,
) -> VegetationDiagnosticStreamScopeDto {
    match scope {
        DiagnosticStreamScope::GlobalSnapshot => VegetationDiagnosticStreamScopeDto::GlobalSnapshot,
        DiagnosticStreamScope::CandidateLineage(lineage) => {
            VegetationDiagnosticStreamScopeDto::CandidateLineage {
                lineage: crate::commands_vegetation::vegetation_guid(lineage.0),
            }
        }
    }
}

pub(crate) fn diagnostic_source_dto(
    source: DiagnosticResultSource,
) -> VegetationDiagnosticResultSourceDto {
    match source {
        DiagnosticResultSource::Cell(cell) => VegetationDiagnosticResultSourceDto::Cell {
            cell: world_cell_dto(cell),
        },
        DiagnosticResultSource::GlobalStage { stage, owner } => {
            VegetationDiagnosticResultSourceDto::GlobalStage {
                stage: hex_hash(stage),
                owner: world_cell_dto(owner),
            }
        }
    }
}

pub(crate) fn diagnostic_candidate_sample_dto(
    source: DiagnosticResultSource,
    sample: DiagnosticCandidateSample,
) -> VegetationDiagnosticCandidateSampleDto {
    VegetationDiagnosticCandidateSampleDto {
        source: diagnostic_source_dto(source),
        identity: candidate_identity_dto(sample.identity),
        owner: world_cell_dto(sample.owner),
        position_ticks: sample.position.global_ticks().map(|tick| tick.to_string()),
        family: sample.family.map(WireUuid::from),
        variation: sample.variation,
        priority_bits: sample.priority.bits(),
        ecology_tick: sample.ecology_tick.to_string(),
    }
}

pub(crate) fn diagnostic_scalar_sample_dto(
    source: DiagnosticResultSource,
    sample: DiagnosticScalarSample,
) -> VegetationDiagnosticScalarSampleDto {
    VegetationDiagnosticScalarSampleDto {
        source: diagnostic_source_dto(source),
        candidate: candidate_identity_dto(sample.candidate),
        value_bits: sample.value.bits(),
    }
}

pub(crate) fn diagnostic_rejection_dto(
    source: DiagnosticResultSource,
    rejected: RejectedCandidate,
) -> VegetationDiagnosticRejectionDto {
    VegetationDiagnosticRejectionDto {
        candidate: candidate_identity_dto(rejected.candidate),
        reason: crate::commands_vegetation::candidate_rejection_reason_dto(rejected.reason),
        provenance: VegetationDiagnosticProvenanceIdDto {
            source: diagnostic_source_dto(source),
            handle: rejected.provenance.0,
        },
    }
}

pub(crate) fn candidate_identity_dto(
    identity: CandidateIdentity,
) -> VegetationCandidateIdentityDto {
    VegetationCandidateIdentityDto {
        node: crate::commands_vegetation::vegetation_guid(identity.node),
        node_address: crate::commands_vegetation::vegetation_guid(identity.node_address),
        node_semantic_revision: identity.node_semantic_revision,
        ordinal: identity.ordinal.to_string(),
        ancestor: identity.ancestor.to_string(),
    }
}

pub(crate) fn world_cell_dto(cell: WorldCellKey) -> WorldCellDto {
    WorldCellDto {
        coordinates: cell.coordinates().map(|coordinate| coordinate.to_string()),
        level: cell.level(),
    }
}

pub(crate) fn execution_domain_dto(domain: GraphExecutionDomain) -> VegetationExecutionDomainDto {
    match domain {
        GraphExecutionDomain::ReferenceCpu => VegetationExecutionDomainDto::ReferenceCpu,
        GraphExecutionDomain::ParallelCpu => VegetationExecutionDomainDto::ParallelCpu,
        GraphExecutionDomain::SlangCompute => VegetationExecutionDomainDto::SlangCompute,
    }
}

pub(crate) fn job_dto(job_id: u64, job: &EvaluationJob) -> VegetationEvaluationJobDto {
    VegetationEvaluationJobDto {
        job: job_id.to_string(),
        state: job_state_dto(&job.state),
        preflight: preflight_dto(job.preflight),
    }
}

pub(crate) fn job_state_dto(state: &EvaluationJobState) -> VegetationEvaluationJobStateDto {
    match state {
        EvaluationJobState::Prepared { .. } => VegetationEvaluationJobStateDto::Prepared,
        EvaluationJobState::Running { .. } => VegetationEvaluationJobStateDto::Running,
        EvaluationJobState::Completed { .. } => VegetationEvaluationJobStateDto::Completed,
        EvaluationJobState::Cancelled => VegetationEvaluationJobStateDto::Cancelled,
        EvaluationJobState::Failed(_) => VegetationEvaluationJobStateDto::Failed,
    }
}

pub(crate) fn preflight_dto(
    preflight: GraphEvaluationPreflight,
) -> VegetationEvaluationPreflightDto {
    VegetationEvaluationPreflightDto {
        output_cells: preflight.output_cells.to_string(),
        global_stage_tiles: preflight.global_stage_tiles.to_string(),
        input_tiles: preflight.input_tiles.to_string(),
        retained_input_bytes: preflight.retained_input_bytes.to_string(),
        generated_input_bytes: preflight.generated_input_bytes.to_string(),
        candidate_count: preflight.candidate_count.to_string(),
        accepted_count: preflight.accepted_count.to_string(),
        micro_samples: preflight.micro_samples.to_string(),
        preflight_peak_bytes: preflight.preflight_peak_bytes.to_string(),
        execution_peak_bytes: preflight.execution_peak_bytes.to_string(),
        memory_bytes: preflight.memory_bytes.to_string(),
        transfer_bytes: preflight.transfer_bytes.to_string(),
        worker_count: preflight.worker_count,
        time_limit_ms: preflight.time_limit_ms.to_string(),
        limits: crate::commands_vegetation::graph_limits_dto(preflight.limits),
    }
}

pub(crate) fn status_dto(job_id: u64, job: &EvaluationJob) -> VegetationEvaluationStatusDto {
    let (summary, error) = match &job.state {
        EvaluationJobState::Prepared { .. } | EvaluationJobState::Running { .. } => (None, None),
        EvaluationJobState::Completed { summary, .. } => (Some((**summary).clone()), None),
        EvaluationJobState::Cancelled => (None, None),
        EvaluationJobState::Failed(error) => (None, Some(error.clone())),
    };
    VegetationEvaluationStatusDto {
        job: job_id.to_string(),
        state: job_state_dto(&job.state),
        preflight: preflight_dto(job.preflight),
        summary,
        error,
    }
}

pub(crate) fn hex_hash(hash: [u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}
