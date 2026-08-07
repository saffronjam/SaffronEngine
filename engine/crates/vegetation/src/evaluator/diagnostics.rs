//! Micro-field tiles, rejection records, and typed evaluation diagnostics.

use super::*;

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, WorldCellKey, WorldPosition};

use crate::binary::BinaryReader;
use crate::{
    Error, GraphExecutionDomain, GraphNodeAddress, GraphOperator, ProvenanceHandle, Result,
};

/// Quantized micro density/attribute tile. Individual reconstructed blades remain cosmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroFieldTile {
    /// Canonical owner cell.
    pub cell: WorldCellKey,
    /// Plant family whose cosmetic population is reconstructed from this tile.
    pub family: Uuid,
    pub dimensions: [u32; 3],
    /// Authoritative density samples.
    pub density: Vec<u16>,
    /// Optional typed attribute channels.
    pub attributes: BTreeMap<u128, Vec<i32>>,
    /// Stable cosmetic reconstruction seed.
    pub reconstruction_seed: u128,
}

/// Why one candidate did not reach a macro output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateRejectionReason {
    /// No eligible surface projection.
    SurfaceMiss,
    /// Scalar threshold rejected the candidate.
    Threshold,
    /// Stable weighted elimination removed the candidate.
    WeightedElimination,
    /// A higher-priority exclusion claim removed the candidate.
    PriorityExclusion,
    /// A prior immutable-stage claim won spacing or competition.
    Competition,
    /// Candidate lies in the halo but belongs to another cell.
    ForeignOwner,
    /// No plant family had positive weight.
    NoSpecies,
}

/// Expanded provenance for a rejected candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedCandidate {
    /// Stable pre-acceptance identity.
    pub candidate: CandidateIdentity,
    /// Exact quantized world position at rejection.
    pub position: WorldPosition,
    pub reason: CandidateRejectionReason,
    /// Complete lineage in the result's shared provenance table.
    pub provenance: ProvenanceHandle,
}

/// Scope captured by one named diagnostic output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticStreamScope {
    /// Complete rejection state at this point in canonical graph execution.
    GlobalSnapshot,
    /// One connected candidate lineage and its associated rejections.
    CandidateLineage(CandidateLineage),
}

/// Stable candidate data retained by a connected diagnostic output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticCandidateSample {
    /// Stable pre-acceptance identity.
    pub identity: CandidateIdentity,
    pub owner: WorldCellKey,
    /// Exact quantized position.
    pub position: WorldPosition,
    /// Selected plant family, when assigned.
    pub family: Option<Uuid>,
    pub variation: u32,
    /// Stable deterministic priority.
    pub priority: DecisionScalar,
    /// Ecology snapshot tick carried by the candidate.
    pub ecology_tick: u64,
}

/// Exact scalar datum retained by a connected diagnostic output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagnosticScalarSample {
    /// Candidate owning the datum.
    pub candidate: CandidateIdentity,
    pub value: DecisionScalar,
}

/// One retained, typed, user-named diagnostic stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedDiagnosticStream {
    /// Fully qualified node that captured the stream.
    pub node: GraphNodeAddress,
    /// User-facing stable stream name.
    pub label: String,
    /// Explicit global or connected-lineage scope.
    pub scope: DiagnosticStreamScope,
    /// Connected candidate samples, absent when the candidate pin is unconnected.
    pub candidates: Option<Vec<DiagnosticCandidateSample>>,
    /// Connected scalar samples, absent when the field pin is unconnected.
    pub field: Option<Vec<DiagnosticScalarSample>>,
    /// Rejections belonging to the selected scope at capture time.
    pub rejected: Vec<RejectedCandidate>,
}

/// Per-node evaluator planning and actual work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEvaluationDiagnostic {
    /// Module-call path from the root graph.
    pub module_path: Vec<u128>,
    /// Stable node GUID local to its owning graph.
    pub node: u128,
    /// Typed operator executed by the node.
    pub operator: GraphOperator,
    /// Stable debug symbol label.
    pub symbol: String,
    pub input_candidates: u64,
    pub output_candidates: u64,
    /// Output bytes retained by the evaluator.
    pub output_bytes: u64,
    /// CPU/GPU transfer bytes for this execution plan.
    pub transfer_bytes: u64,
    /// Conservative transfer bytes predicted by the compiled node estimate.
    pub predicted_transfer_bytes: u64,
    /// Measured wall time in microseconds.
    pub elapsed_micros: u64,
    /// Execution domain actually used.
    pub execution_domain: GraphExecutionDomain,
}

/// Actual work for one resident connected execution group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuGroupEvaluationDiagnostic {
    /// Canonical nodes executed by the single resident program.
    pub nodes: Vec<GraphNodeAddress>,
    /// Invocations submitted to the program.
    pub invocation_count: u64,
    /// Boundary bytes uploaded and downloaded once.
    pub transfer_bytes: u64,
    /// Boundary output bytes retained by the evaluator.
    pub output_bytes: u64,
    /// Measured wall time for the complete dispatch in microseconds.
    pub elapsed_micros: u64,
}

/// Complete typed diagnostics for one bounded evaluation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphEvaluationDiagnostics {
    /// Per-node diagnostics in canonical topological order.
    pub nodes: Vec<NodeEvaluationDiagnostic>,
    /// Resident group dispatches in canonical execution order.
    pub gpu_groups: Vec<GpuGroupEvaluationDiagnostic>,
    /// Expanded rejected-candidate records.
    pub rejected: Vec<RejectedCandidate>,
    /// Retained named outputs in canonical node order.
    pub streams: Vec<NamedDiagnosticStream>,
    /// Total candidates seen by output stages.
    pub candidate_count: u64,
    pub accepted_count: u64,
}

/// Validates a complete rejection-diagnostics facet and returns nonzero reason totals.
pub fn vegetation_rejection_totals(bytes: &[u8]) -> Result<Vec<(CandidateRejectionReason, u64)>> {
    let mut reader = BinaryReader::new(bytes, "vegetation rejection diagnostics");
    reader.expect(b"SVEGREJ2", "magic")?;
    let candidate_count = reader.u64()?;
    let accepted_count = reader.u64()?;
    if accepted_count > candidate_count {
        return Err(Error::ArtifactFormat {
            format: "vegetation rejection diagnostics",
            field: "acceptedCount".to_owned(),
        });
    }
    let rejected_count = reader.count(105)?;
    let mut totals = [0_u64; 7];
    for _ in 0..rejected_count {
        skip_candidate_identity(&mut reader)?;
        for _ in 0..3 {
            reader.u128()?;
        }
        let reason = rejection_reason_from_byte(reader.u8()?)?;
        reader.u32()?;
        totals[usize::from(rejection_reason_byte(reason))] = totals
            [usize::from(rejection_reason_byte(reason))]
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    }
    let stream_count = reader.count(27)?;
    for _ in 0..stream_count {
        skip_diagnostic_stream(&mut reader)?;
    }
    reader.complete()?;
    let reasons = [
        CandidateRejectionReason::SurfaceMiss,
        CandidateRejectionReason::Threshold,
        CandidateRejectionReason::WeightedElimination,
        CandidateRejectionReason::PriorityExclusion,
        CandidateRejectionReason::Competition,
        CandidateRejectionReason::ForeignOwner,
        CandidateRejectionReason::NoSpecies,
    ];
    Ok(reasons
        .into_iter()
        .zip(totals)
        .filter(|(_, count)| *count != 0)
        .collect())
}
