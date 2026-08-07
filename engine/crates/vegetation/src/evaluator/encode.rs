//! Canonical byte encoding of an evaluation result and its facets.

use super::*;

use saffron_core::Uuid;
use saffron_spatial::{FieldChannel, FieldDerivative, WorldCellKey, WorldPosition};

use crate::binary::BinaryReader;
use crate::canonical::CanonicalSink;
use crate::{
    Error, GraphNodeAddress, ProvenanceDecisionOutcome, ProvenanceHandle, ProvenanceTable, Result,
};

pub(super) fn validate_strict_order<T>(
    values: &[T],
    guard: Option<PreflightGuard<'_>>,
    before: impl Fn(&T, &T) -> bool,
) -> Result<bool> {
    for pair in values.windows(2) {
        guard.map_or(Ok(()), PreflightGuard::check)?;
        if !before(&pair[0], &pair[1]) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn projection_tile_order_key(
    tile: &QuantizedSurfaceProjectionTile,
) -> (u128, u32, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.provider_set_hash,
    )
}

pub(super) fn field_query_tile_order_key(
    tile: &QuantizedSurfaceFieldQueryTile,
) -> (u128, u32, FieldChannel, FieldDerivative, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.channel,
        tile.derivative,
        tile.provider_set_hash,
    )
}

pub(super) fn rejected_order_key(
    rejected: &RejectedCandidate,
) -> (CandidateIdentity, u8, ProvenanceHandle) {
    (
        rejected.candidate,
        rejection_reason_byte(rejected.reason),
        rejected.provenance,
    )
}

pub(super) fn diagnostic_stream_order_key(
    stream: &NamedDiagnosticStream,
) -> (&GraphNodeAddress, &str, DiagnosticStreamScope) {
    (&stream.node, stream.label.as_str(), stream.scope)
}

pub(super) fn encode_ordered<S, T, E>(sink: &mut S, values: &[T], mut encode: E) -> Result<()>
where
    S: CanonicalSink,
    E: FnMut(&mut S, &T) -> Result<()>,
{
    for value in values {
        encode(sink, value)?;
    }
    Ok(())
}

/// Encodes the micro-field rows shared by the section facet and the canonical result hash.
pub(crate) fn encode_micro_fields_body<S: CanonicalSink>(
    sink: &mut S,
    tiles: &[MicroFieldTile],
) -> Result<()> {
    push_len(sink, tiles.len())?;
    encode_ordered(sink, tiles, encode_micro_tile)
}

fn encode_micro_tile<S: CanonicalSink>(sink: &mut S, tile: &MicroFieldTile) -> Result<()> {
    sink.write(&tile.cell.canonical_bytes())?;
    sink.write(&tile.family.value().to_be_bytes())?;
    for dimension in tile.dimensions {
        sink.write(&dimension.to_be_bytes())?;
    }
    push_len(sink, tile.density.len())?;
    for value in &tile.density {
        sink.write(&value.to_be_bytes())?;
    }
    push_len(sink, tile.attributes.len())?;
    for (channel, values) in &tile.attributes {
        sink.write(&channel.to_be_bytes())?;
        push_len(sink, values.len())?;
        for value in values {
            sink.write(&value.to_be_bytes())?;
        }
    }
    sink.write(&tile.reconstruction_seed.to_be_bytes())
}

pub(super) fn encode_projection_tile<S: CanonicalSink>(
    sink: &mut S,
    tile: &QuantizedSurfaceProjectionTile,
) -> Result<()> {
    sink.write(&tile.node.to_be_bytes())?;
    sink.write(&tile.node_semantic_revision.to_be_bytes())?;
    sink.write(&tile.provider_set_hash)?;
    push_len(sink, tile.samples.len())?;
    for entry in &tile.samples {
        push_world_position(sink, entry.query)?;
        match &entry.sample {
            Some(sample) => {
                sink.write_byte(1)?;
                push_world_position(sink, sample.position)?;
                sink.write(&sample.attachment.provider.0.to_be_bytes())?;
                sink.write(&sample.attachment.primitive.0.to_be_bytes())?;
                for barycentric in sample.attachment.barycentric {
                    sink.write(&barycentric.bits().to_be_bytes())?;
                }
                sink.write(&sample.attachment.revision.0.to_be_bytes())?;
                for normal in sample.normal {
                    sink.write(&normal.bits().to_be_bytes())?;
                }
                for projection in sample.projection {
                    sink.write(&projection.bits().to_be_bytes())?;
                }
                push_len(sink, sample.tags.len())?;
                for tag in &sample.tags {
                    sink.write(&tag.tag.0.to_be_bytes())?;
                    sink.write(&tag.weight.bits().to_be_bytes())?;
                }
            }
            None => sink.write_byte(0)?,
        }
    }
    Ok(())
}

pub(super) fn encode_field_query_tile<S: CanonicalSink>(
    sink: &mut S,
    tile: &QuantizedSurfaceFieldQueryTile,
) -> Result<()> {
    sink.write(&tile.node.to_be_bytes())?;
    sink.write(&tile.node_semantic_revision.to_be_bytes())?;
    push_field_channel(sink, tile.channel)?;
    sink.write_byte(match tile.derivative {
        FieldDerivative::Value => 0,
        FieldDerivative::Gradient => 1,
        FieldDerivative::Hessian => 2,
    })?;
    sink.write(&tile.provider_set_hash)?;
    push_len(sink, tile.samples.len())?;
    for entry in &tile.samples {
        push_candidate_identity(sink, entry.candidate)?;
        push_world_position(sink, entry.query)?;
        match entry.value {
            QuantizedSurfaceFieldValue::Scalar(value) => {
                sink.write_byte(0)?;
                sink.write(&value.to_be_bytes())?;
            }
            QuantizedSurfaceFieldValue::Gradient(value) => {
                sink.write_byte(1)?;
                for lane in value {
                    sink.write(&lane.to_be_bytes())?;
                }
            }
            QuantizedSurfaceFieldValue::Hessian(value) => {
                sink.write_byte(2)?;
                for lane in value {
                    sink.write(&lane.to_be_bytes())?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn encode_unique_references<S: CanonicalSink>(
    sink: &mut S,
    references: &[WorldCellKey],
) -> Result<()> {
    push_len(sink, references.len())?;
    for reference in references {
        sink.write(&reference.canonical_bytes())?;
    }
    Ok(())
}

pub(super) fn encode_provenance<S: CanonicalSink>(
    sink: &mut S,
    table: &ProvenanceTable,
) -> Result<()> {
    push_len(sink, table.decisions().len())?;
    for decision in table.decisions() {
        push_len(sink, decision.parents.len())?;
        for parent in &decision.parents {
            sink.write(&parent.0.to_be_bytes())?;
        }
        push_len(sink, decision.subgraph_path.len())?;
        for call in &decision.subgraph_path {
            sink.write(&call.to_be_bytes())?;
        }
        sink.write(&decision.node.to_be_bytes())?;
        let operator = decision.operator.as_wire().as_bytes();
        push_len(sink, operator.len())?;
        sink.write(operator)?;
        sink.write(&decision.candidate.to_be_bytes())?;
        sink.write_byte(provenance_outcome_byte(decision.outcome))?;
    }
    push_len(sink, table.records().len())?;
    for record in table.records() {
        sink.write(&record.map.value().to_be_bytes())?;
        sink.write(&record.layer.to_be_bytes())?;
        sink.write(&record.biome.value().to_be_bytes())?;
        sink.write(&record.decision.0.to_be_bytes())?;
        sink.write(&record.candidate.to_be_bytes())?;
        push_optional_uuid(sink, record.family)?;
        match record.plant {
            Some(plant) => {
                sink.write_byte(1)?;
                sink.write(&plant.bytes())?;
            }
            None => sink.write_byte(0)?,
        }
        sink.write(&record.variation.to_be_bytes())?;
    }
    Ok(())
}

pub(super) fn encode_rejected_candidate<S: CanonicalSink>(
    sink: &mut S,
    candidate: &RejectedCandidate,
) -> Result<()> {
    push_candidate_identity(sink, candidate.candidate)?;
    for ticks in candidate.position.global_ticks() {
        sink.write(&ticks.to_be_bytes())?;
    }
    sink.write_byte(rejection_reason_byte(candidate.reason))?;
    sink.write(&candidate.provenance.0.to_be_bytes())
}

pub(super) fn encode_diagnostic_stream<S: CanonicalSink>(
    sink: &mut S,
    stream: &NamedDiagnosticStream,
) -> Result<()> {
    let node_length = 8_usize
        .checked_add(
            stream
                .node
                .module_path
                .len()
                .checked_mul(16)
                .ok_or(Error::NumericOverflow)?,
        )
        .and_then(|length| length.checked_add(16))
        .ok_or(Error::NumericOverflow)?;
    push_len(sink, node_length)?;
    push_len(sink, stream.node.module_path.len())?;
    for call in &stream.node.module_path {
        sink.write(&call.to_be_bytes())?;
    }
    sink.write(&stream.node.node.to_be_bytes())?;
    push_len(sink, stream.label.len())?;
    sink.write(stream.label.as_bytes())?;
    match stream.scope {
        DiagnosticStreamScope::GlobalSnapshot => sink.write_byte(0)?,
        DiagnosticStreamScope::CandidateLineage(lineage) => {
            sink.write_byte(1)?;
            sink.write(&lineage.0.to_be_bytes())?;
        }
    }
    match &stream.candidates {
        Some(candidates) => {
            sink.write_byte(1)?;
            push_len(sink, candidates.len())?;
            encode_ordered(sink, candidates, |sink, sample| {
                push_candidate_identity(sink, sample.identity)?;
                sink.write(&sample.owner.canonical_bytes())?;
                push_world_position(sink, sample.position)?;
                push_optional_uuid(sink, sample.family)?;
                sink.write(&sample.variation.to_be_bytes())?;
                sink.write(&sample.priority.bits().to_be_bytes())?;
                sink.write(&sample.ecology_tick.to_be_bytes())
            })?;
        }
        None => sink.write_byte(0)?,
    }
    match &stream.field {
        Some(field) => {
            sink.write_byte(1)?;
            push_len(sink, field.len())?;
            encode_ordered(sink, field, |sink, sample| {
                push_candidate_identity(sink, sample.candidate)?;
                sink.write(&sample.value.bits().to_be_bytes())
            })?;
        }
        None => sink.write_byte(0)?,
    }
    push_len(sink, stream.rejected.len())?;
    encode_ordered(sink, &stream.rejected, encode_rejected_candidate)
}

fn push_optional_uuid<S: CanonicalSink>(sink: &mut S, value: Option<Uuid>) -> Result<()> {
    match value {
        Some(value) => {
            sink.write_byte(1)?;
            sink.write(&value.value().to_be_bytes())?;
        }
        None => sink.write_byte(0)?,
    }
    Ok(())
}

fn push_world_position<S: CanonicalSink>(sink: &mut S, position: WorldPosition) -> Result<()> {
    for tick in position.global_ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

pub(super) fn skip_diagnostic_stream(reader: &mut BinaryReader<'_>) -> Result<()> {
    let node_length = reader.length()?;
    let mut node = BinaryReader::new(
        reader.take(node_length)?,
        "vegetation rejection diagnostics",
    );
    let module_count = node.count(16)?;
    for _ in 0..module_count {
        node.u128()?;
    }
    node.u128()?;
    node.complete()?;
    reader.string()?;
    match reader.u8()? {
        0 => {}
        1 => {
            reader.u128()?;
        }
        _ => {
            return Err(Error::ArtifactFormat {
                format: "vegetation rejection diagnostics",
                field: "streams.scope".to_owned(),
            });
        }
    }
    if reader.bool()? {
        let count = reader.count(142)?;
        for _ in 0..count {
            skip_candidate_identity(reader)?;
            reader.cell()?;
            for _ in 0..3 {
                reader.i128()?;
            }
            if reader.bool()? {
                reader.uuid()?;
            }
            reader.u32()?;
            reader.i32()?;
            reader.u64()?;
        }
    }
    if reader.bool()? {
        let count = reader.count(56)?;
        for _ in 0..count {
            skip_candidate_identity(reader)?;
            reader.i32()?;
        }
    }
    let rejected_count = reader.count(105)?;
    for _ in 0..rejected_count {
        skip_candidate_identity(reader)?;
        for _ in 0..3 {
            reader.u128()?;
        }
        rejection_reason_from_byte(reader.u8()?)?;
        reader.u32()?;
    }
    Ok(())
}

pub(super) fn skip_candidate_identity(reader: &mut BinaryReader<'_>) -> Result<()> {
    reader.u128()?;
    reader.u128()?;
    reader.u32()?;
    reader.u64()?;
    reader.u64()?;
    Ok(())
}

fn push_candidate_identity<S: CanonicalSink>(
    sink: &mut S,
    identity: CandidateIdentity,
) -> Result<()> {
    sink.write(&identity.node.to_be_bytes())?;
    sink.write(&identity.node_address.to_be_bytes())?;
    sink.write(&identity.node_semantic_revision.to_be_bytes())?;
    sink.write(&identity.ordinal.to_be_bytes())?;
    sink.write(&identity.ancestor.to_be_bytes())
}

fn push_field_channel<S: CanonicalSink>(sink: &mut S, channel: FieldChannel) -> Result<()> {
    let (tag, user) = match channel {
        FieldChannel::Altitude => (0, None),
        FieldChannel::Slope => (1, None),
        FieldChannel::Curvature => (2, None),
        FieldChannel::Concavity => (3, None),
        FieldChannel::Drainage => (4, None),
        FieldChannel::Moisture => (5, None),
        FieldChannel::Temperature => (6, None),
        FieldChannel::Precipitation => (7, None),
        FieldChannel::Sunlight => (8, None),
        FieldChannel::Exposure => (9, None),
        FieldChannel::WaterDistance => (10, None),
        FieldChannel::WaterDepth => (11, None),
        FieldChannel::SignedBlocker => (12, None),
        FieldChannel::SplineDistance => (13, None),
        FieldChannel::User(value) => (14, Some(value)),
    };
    sink.write_byte(tag)?;
    if let Some(value) = user {
        sink.write(&value.to_be_bytes())?;
    }
    Ok(())
}

const fn provenance_outcome_byte(outcome: ProvenanceDecisionOutcome) -> u8 {
    match outcome {
        ProvenanceDecisionOutcome::Produced => 0,
        ProvenanceDecisionOutcome::Retained => 1,
        ProvenanceDecisionOutcome::Accepted => 2,
        ProvenanceDecisionOutcome::Rejected => 3,
    }
}

pub(super) const fn rejection_reason_byte(reason: CandidateRejectionReason) -> u8 {
    match reason {
        CandidateRejectionReason::SurfaceMiss => 0,
        CandidateRejectionReason::Threshold => 1,
        CandidateRejectionReason::WeightedElimination => 2,
        CandidateRejectionReason::PriorityExclusion => 3,
        CandidateRejectionReason::Competition => 4,
        CandidateRejectionReason::ForeignOwner => 5,
        CandidateRejectionReason::NoSpecies => 6,
    }
}

pub(super) fn rejection_reason_from_byte(value: u8) -> Result<CandidateRejectionReason> {
    match value {
        0 => Ok(CandidateRejectionReason::SurfaceMiss),
        1 => Ok(CandidateRejectionReason::Threshold),
        2 => Ok(CandidateRejectionReason::WeightedElimination),
        3 => Ok(CandidateRejectionReason::PriorityExclusion),
        4 => Ok(CandidateRejectionReason::Competition),
        5 => Ok(CandidateRejectionReason::ForeignOwner),
        6 => Ok(CandidateRejectionReason::NoSpecies),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation rejection diagnostics",
            field: "rejectionReason".to_owned(),
        }),
    }
}
