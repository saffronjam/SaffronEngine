//! Spline following and exact integer spline-segment math.

use super::*;

use saffron_spatial::{DecisionScalar, WorldPosition, div_round_ties_even};

use crate::memory::{checked_memory_sum, requested_vec_bytes_for_len};
use crate::{CompiledGraphNode, Error, Result};

pub(super) fn follow_splines(
    node: &CompiledGraphNode,
    splines: &[EvaluationSpline],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let spacing = fixed_parameter(node, "spacing", DecisionScalar::from_bits(0))?;
    let spacing_ticks = fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128;
    if spacing_ticks == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline spacing must be positive".to_owned(),
        });
    }
    let edge_offset = fixed_meters_to_ticks(fixed_parameter(
        node,
        "edgeOffset",
        DecisionScalar::from_bits(0),
    )?)?;
    let region_upper = if state.inputs.regions.is_empty() {
        cell_region_count(state.inputs.read_bounds, state.inputs.output_cell.level())? as u64
    } else {
        state.inputs.regions.len() as u64
    };
    state.check_transient_memory(stage_region_scratch_bytes(region_upper)?)?;
    let stage_regions = stage_regions(node, &state.inputs.regions, state)?;
    if splines.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline identities must be unique".to_owned(),
        });
    }
    let mut maximum_candidates = 0_usize;
    for spline in splines {
        let segments = spline_segments(&spline.points)?;
        let total_length = segments.iter().try_fold(0_i128, |total, segment| {
            total
                .checked_add(segment.length)
                .ok_or(Error::NumericOverflow)
        })?;
        maximum_candidates = maximum_candidates
            .checked_add(
                usize::try_from(total_length / spacing_ticks)
                    .map_err(|_| Error::NumericOverflow)?
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?,
            )
            .ok_or(Error::NumericOverflow)?;
    }
    state.check_count(
        "candidate count",
        maximum_candidates as u64,
        state.graph.limits.max_candidates,
    )?;
    let spline_points = splines.iter().try_fold(0_u64, |total, spline| {
        total
            .checked_add(spline.points.len() as u64)
            .ok_or(Error::NumericOverflow)
    })?;
    state.check_transient_memory(checked_memory_sum([
        requested_vec_bytes_for_len::<EvaluationRegion>(stage_regions.len() as u64)?,
        requested_vec_bytes_for_len::<[i128; 3]>(spline_points)?,
        requested_vec_bytes_for_len::<SplineSegment>(spline_points)?,
        requested_vec_bytes_for_len::<[i128; 3]>(maximum_candidates as u64)?,
        requested_vec_bytes_for_len::<GraphCandidate>(maximum_candidates as u64)?,
    ])?)?;
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        maximum_candidates,
        "spline-follow candidates",
    )?;
    for spline in splines {
        let segments = spline_segments(&spline.points)?;
        for (sample_index, ticks) in sample_spline_segments(&segments, spacing_ticks, edge_offset)?
            .into_iter()
            .enumerate()
        {
            let position = WorldPosition::from_global_ticks(ticks)?;
            if stage_regions
                .iter()
                .any(|region| region.bounds.contains(position))
            {
                let ordinal = candidate_ordinal(
                    node,
                    state,
                    spline.id,
                    u64::try_from(sample_index).map_err(|_| Error::NumericOverflow)?,
                    2,
                )?;
                let mut candidate = default_candidate(node, state, ordinal, 0, position)?;
                candidate.source_layer = spline.layer;
                candidates.push(candidate);
            }
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    result.canonicalize()?;
    Ok(result)
}

pub(super) fn sample_spline_segments(
    segments: &[SplineSegment],
    spacing_ticks: i128,
    edge_offset: i128,
) -> Result<Vec<[i128; 3]>> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    let total_length = segments.iter().try_fold(0_i128, |total, segment| {
        total
            .checked_add(segment.length)
            .ok_or(Error::NumericOverflow)
    })?;
    let mut segment_index = 0_usize;
    let mut segment_start_distance = 0_i128;
    let sample_count = total_length / spacing_ticks;
    let sample_capacity = usize::try_from(sample_count)
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or(Error::NumericOverflow)?;
    let mut samples = Vec::new();
    crate::memory::reserve_exact(&mut samples, sample_capacity, "spline samples")?;
    for sample_index in 0..=sample_count {
        let distance = sample_index
            .checked_mul(spacing_ticks)
            .ok_or(Error::NumericOverflow)?;
        while segment_index + 1 < segments.len()
            && distance
                >= segment_start_distance
                    .checked_add(segments[segment_index].length)
                    .ok_or(Error::NumericOverflow)?
        {
            segment_start_distance = segment_start_distance
                .checked_add(segments[segment_index].length)
                .ok_or(Error::NumericOverflow)?;
            segment_index += 1;
        }
        let segment = &segments[segment_index];
        let local_distance = distance
            .checked_sub(segment_start_distance)
            .ok_or(Error::NumericOverflow)?;
        let mut ticks = [0_i128; 3];
        for (axis, tick) in ticks.iter_mut().enumerate() {
            let along = div_round_ties_even(
                segment.direction[axis]
                    .checked_mul(local_distance)
                    .ok_or(Error::NumericOverflow)?,
                segment.length,
            )?;
            let lateral = div_round_ties_even(
                segment.lateral[axis]
                    .checked_mul(edge_offset)
                    .ok_or(Error::NumericOverflow)?,
                segment.lateral_length,
            )?;
            *tick = segment.start[axis]
                .checked_add(along)
                .and_then(|value| value.checked_add(lateral))
                .ok_or(Error::NumericOverflow)?;
        }
        samples.push(ticks);
    }
    Ok(samples)
}

#[derive(Clone, Copy)]
pub(super) struct SplineSegment {
    pub(super) start: [i128; 3],
    pub(super) direction: [i128; 3],
    pub(super) length: i128,
    lateral: [i128; 3],
    lateral_length: i128,
}

pub(super) fn spline_segments(points: &[WorldPosition]) -> Result<Vec<SplineSegment>> {
    let mut canonical = Vec::<[i128; 3]>::new();
    crate::memory::reserve_exact(&mut canonical, points.len(), "canonical spline points")?;
    for point in points {
        let point = point.global_ticks();
        if canonical.last() == Some(&point) {
            continue;
        }
        while canonical.len() >= 2 {
            let previous = canonical[canonical.len() - 2];
            let current = canonical[canonical.len() - 1];
            let incoming = subtract_position(current, previous)?;
            let outgoing = subtract_position(point, current)?;
            if primitive_direction(incoming)? != primitive_direction(outgoing)? {
                break;
            }
            canonical.pop();
        }
        canonical.push(point);
    }

    let mut segments = Vec::new();
    crate::memory::reserve_exact(
        &mut segments,
        canonical.len().saturating_sub(1),
        "spline segments",
    )?;
    let mut previous_lateral = None;
    for points in canonical.windows(2) {
        let start = points[0];
        let end = points[1];
        let direction = subtract_position(end, start)?;
        let length = integer_sqrt(distance_squared(start, end)?);
        if length == 0 {
            continue;
        }
        let mut lateral = if direction[0] == 0 && direction[2] == 0 {
            [
                direction[1].checked_neg().ok_or(Error::NumericOverflow)?,
                direction[0],
                0,
            ]
        } else {
            [
                direction[2],
                0,
                direction[0].checked_neg().ok_or(Error::NumericOverflow)?,
            ]
        };
        if let Some(previous) = previous_lateral
            && vector_dot(previous, lateral)? < 0
        {
            for value in &mut lateral {
                *value = value.checked_neg().ok_or(Error::NumericOverflow)?;
            }
        }
        let lateral_length = integer_sqrt(vector_length_squared(lateral)?);
        if lateral_length == 0 {
            return Err(Error::NumericOverflow);
        }
        previous_lateral = Some(lateral);
        segments.push(SplineSegment {
            start,
            direction,
            length,
            lateral,
            lateral_length,
        });
    }
    Ok(segments)
}

fn subtract_position(left: [i128; 3], right: [i128; 3]) -> Result<[i128; 3]> {
    let mut result = [0_i128; 3];
    for axis in 0..3 {
        result[axis] = left[axis]
            .checked_sub(right[axis])
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(result)
}

fn primitive_direction(direction: [i128; 3]) -> Result<[i128; 3]> {
    let divisor = direction
        .iter()
        .map(|value| value.unsigned_abs())
        .fold(0_u128, greatest_common_divisor);
    if divisor == 0 {
        return Ok([0; 3]);
    }
    let mut primitive = [0_i128; 3];
    for axis in 0..3 {
        let magnitude = direction[axis].unsigned_abs() / divisor;
        let magnitude = i128::try_from(magnitude).map_err(|_| Error::NumericOverflow)?;
        primitive[axis] = if direction[axis] < 0 {
            magnitude.checked_neg().ok_or(Error::NumericOverflow)?
        } else {
            magnitude
        };
    }
    Ok(primitive)
}

fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn vector_dot(left: [i128; 3], right: [i128; 3]) -> Result<i128> {
    (0..3).try_fold(0_i128, |sum, axis| {
        sum.checked_add(
            left[axis]
                .checked_mul(right[axis])
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)
    })
}

fn vector_length_squared(vector: [i128; 3]) -> Result<i128> {
    vector_dot(vector, vector)
}
