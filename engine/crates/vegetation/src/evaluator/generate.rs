//! Candidate generation operators and their deterministic position sampling.

use super::*;

use saffron_spatial::{
    DecisionScalar, RandomStream, UnitInterval, WeightedSurfaceTag, WorldBounds, WorldPosition,
    div_round_ties_even,
};

use crate::{CompiledGraphNode, Error, Result};

pub(super) fn explicit_anchor_candidates(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let layer = guid_parameter(node, "layer", 0)?;
    state.check_count(
        "candidate count",
        state.inputs.anchors.len() as u64,
        state.graph.limits.max_candidates,
    )?;
    let candidate_count = state
        .inputs
        .anchors
        .iter()
        .filter(|anchor| {
            anchor.layer == layer && state.inputs.read_bounds.contains(anchor.point.position)
        })
        .count();
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        candidate_count,
        "explicit anchor candidates",
    )?;
    for anchor in state.inputs.anchors.iter().filter(|anchor| {
        anchor.layer == layer && state.inputs.read_bounds.contains(anchor.point.position)
    }) {
        let point = &anchor.point;
        point.validate()?;
        let radius = point_radius(point)?;
        candidates.push(GraphCandidate {
            identity: CandidateIdentity {
                node: node.definition.guid,
                node_address: node_execution_address(node, state),
                node_semantic_revision: node.definition.semantic_revision,
                ordinal: stable_ordinal(&[
                    &node_execution_address(node, state).to_be_bytes(),
                    &node.definition.semantic_revision.to_be_bytes(),
                    &point.id.bytes(),
                ])?,
                ancestor: 0,
            },
            owner: canonical_owner(point.position, node.definition.spatial.level())?,
            source_layer: anchor.layer,
            position: point.position,
            orientation: point.orientation,
            scale: point.scale,
            family: Some(point.family),
            variation: point.variation,
            parent: None,
            colony: None,
            priority: DecisionScalar::from_bits(i32::MAX),
            ecology_tick: point.ecology_tick,
            crown_radius: radius,
            root_radius: radius,
            attachment: point.attachment,
            surface_normal: None,
            surface_projection: point.surface_projection,
            authored_point: Some(point.clone()),
        });
    }
    let mut stream = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    stream.canonicalize()?;
    Ok(stream)
}

pub(super) fn stratified_candidates(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let count = u64::from(u32_parameter(node, "count", 0)?);
    state.check_transient_memory(stage_region_scratch_bytes(regions.len() as u64)?)?;
    let regions = stage_regions(node, regions, state)?;
    if count == 0 || regions.is_empty() {
        return Ok(CandidateStream {
            lineage: candidate_lineage(node, state),
            candidates: Vec::new(),
        });
    }
    let jitter = unit_parameter(node, "jitter", UnitInterval::ZERO)?;
    let requested = count
        .checked_mul(regions.len() as u64)
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "candidate count",
        requested,
        state.graph.limits.max_candidates,
    )?;
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        usize::try_from(requested).map_err(|_| Error::NumericOverflow)?,
        "stratified candidates",
    )?;
    for region in regions {
        let side = integer_sqrt_ceil(count);
        for local in 0..count {
            state.check_abort()?;
            let ordinal = candidate_ordinal(node, state, region.id, local, 0)?;
            let x = local % side;
            let z = local / side;
            let stream = random_stream(
                node,
                state,
                "sampling",
                RandomSampleAddress::new(region.seed_cell, ordinal),
            )?;
            let position = stratified_position(region.bounds, x, z, side, jitter, stream)?;
            let mut candidate = default_candidate(node, state, ordinal, 0, position)?;
            candidate.source_layer = region.layer;
            candidates.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    result.canonicalize()?;
    Ok(result)
}

pub(super) fn blue_noise_candidates(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let count = u64::from(u32_parameter(node, "count", 0)?);
    let radius = fixed_parameter(node, "radius", DecisionScalar::from_bits(0))?;
    let attempts = u64::from(u32_parameter(node, "attempts", 30)?).max(1);
    state.check_transient_memory(stage_region_scratch_bytes(regions.len() as u64)?)?;
    let regions = stage_regions(node, regions, state)?;
    if count == 0 || regions.is_empty() || radius.bits() <= 0 {
        return Ok(CandidateStream {
            lineage: candidate_lineage(node, state),
            candidates: Vec::new(),
        });
    }
    state.check_count(
        "candidate count",
        count
            .checked_mul(regions.len() as u64)
            .ok_or(Error::NumericOverflow)?,
        state.graph.limits.max_candidates,
    )?;
    let radius_ticks = fixed_meters_to_ticks(radius)?.unsigned_abs() as i128;
    let radius_squared = radius_ticks
        .checked_mul(radius_ticks)
        .ok_or(Error::NumericOverflow)?;
    let count = usize::try_from(count).map_err(|_| Error::NumericOverflow)?;
    let maximum_candidates = count
        .checked_mul(regions.len())
        .ok_or(Error::NumericOverflow)?;
    state.check_transient_memory(blue_noise_scratch_bytes(
        u64::try_from(maximum_candidates).map_err(|_| Error::NumericOverflow)?,
    )?)?;
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        maximum_candidates,
        "blue-noise accepted candidates",
    )?;
    for region in regions {
        let seed_ordinal = candidate_ordinal(node, state, region.id, 0, 1)?;
        let seed_stream = random_stream(
            node,
            state,
            "sampling",
            RandomSampleAddress::new(region.seed_cell, seed_ordinal).with_channel(1),
        )?;
        let mut seed = default_candidate(
            node,
            state,
            seed_ordinal,
            0,
            uniform_position(region.bounds, seed_stream, 0)?,
        )?;
        seed.source_layer = region.layer;
        let mut region_points = Vec::new();
        crate::memory::reserve_exact(&mut region_points, count, "blue-noise region candidates")?;
        region_points.push(seed.clone());
        let mut active = Vec::new();
        crate::memory::reserve_exact(&mut active, count, "blue-noise active candidates")?;
        active.push(seed);
        let mut proposal = 1_u64;
        while !active.is_empty() && region_points.len() < count {
            state.check_abort()?;
            let parent = active.remove(0);
            let mut produced = false;
            for attempt in 0..attempts {
                let ordinal = candidate_ordinal(node, state, region.id, proposal, 1)?;
                proposal = proposal.checked_add(1).ok_or(Error::NumericOverflow)?;
                let stream = random_stream(
                    node,
                    state,
                    "sampling",
                    RandomSampleAddress::new(region.seed_cell, ordinal)
                        .with_ancestor(parent.identity.ordinal)
                        .with_channel(u32::try_from(attempt).map_err(|_| Error::NumericOverflow)?),
                )?;
                let position = poisson_annulus_position(parent.position, radius_ticks, stream)?;
                let mut conflicts = false;
                for other in &region_points {
                    if distance_squared_xz(other.position, position)? < radius_squared {
                        conflicts = true;
                        break;
                    }
                }
                if !region.bounds.contains(position) || conflicts {
                    continue;
                }
                let mut candidate =
                    default_candidate(node, state, ordinal, parent.identity.ordinal, position)?;
                candidate.source_layer = region.layer;
                region_points.push(candidate.clone());
                active.push(candidate);
                produced = true;
                if region_points.len() >= count {
                    break;
                }
            }
            if produced {
                active.push(parent);
            }
        }
        accepted.extend(region_points);
    }
    accepted.sort_unstable_by_key(|candidate| candidate.identity);
    let mut globally_spaced: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut globally_spaced,
        accepted.len(),
        "blue-noise globally spaced candidates",
    )?;
    for candidate in accepted {
        let mut conflicts = false;
        for other in &globally_spaced {
            if distance_squared_xz(other.position, candidate.position)? < radius_squared {
                conflicts = true;
                break;
            }
        }
        if !conflicts {
            globally_spaced.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: globally_spaced,
    };
    result.canonicalize()?;
    Ok(result)
}

fn poisson_annulus_position(
    center: WorldPosition,
    minimum_radius: i128,
    stream: RandomStream,
) -> Result<WorldPosition> {
    annulus_position(
        center,
        minimum_radius,
        minimum_radius
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?,
        stream,
    )
}

pub(super) fn annulus_position(
    center: WorldPosition,
    minimum_radius: i128,
    maximum_radius: i128,
    stream: RandomStream,
) -> Result<WorldPosition> {
    if minimum_radius < 0 || maximum_radius < minimum_radius {
        return Err(Error::NumericOverflow);
    }
    let radius_span = maximum_radius
        .checked_sub(minimum_radius)
        .ok_or(Error::NumericOverflow)?;
    let radius_offset = div_round_ties_even(
        radius_span
            .checked_mul(i128::from(stream.lane(0, 1)))
            .ok_or(Error::NumericOverflow)?,
        i128::from(u32::MAX),
    )?;
    let radius = minimum_radius
        .checked_add(radius_offset)
        .ok_or(Error::NumericOverflow)?;
    let (cosine, sine) = cordic_sin_cos(stream.lane(0, 0));
    let dx = div_round_ties_even(
        radius
            .checked_mul(i128::from(cosine))
            .ok_or(Error::NumericOverflow)?,
        1_i128 << 30,
    )?;
    let dz = div_round_ties_even(
        radius
            .checked_mul(i128::from(sine))
            .ok_or(Error::NumericOverflow)?,
        1_i128 << 30,
    )?;
    offset_ticks(center, [dx, 0, dz])
}

fn stratified_position(
    bounds: WorldBounds,
    x: u64,
    z: u64,
    side: u64,
    jitter: UnitInterval,
    stream: RandomStream,
) -> Result<WorldPosition> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut ticks = minimum;
    for (axis, stratum, lane) in [(0, x, 0), (2, z, 1)] {
        let span = maximum[axis] - minimum[axis];
        let center_numerator = i128::from(stratum)
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .ok_or(Error::NumericOverflow)?;
        let center = minimum[axis]
            + div_round_ties_even(
                span.checked_mul(center_numerator)
                    .ok_or(Error::NumericOverflow)?,
                i128::from(side)
                    .checked_mul(2)
                    .ok_or(Error::NumericOverflow)?,
            )?;
        let cell_span = span / i128::from(side.max(1));
        let signed = i128::from(stream.lane(0, lane)) - i128::from(u32::MAX) / 2;
        let jitter_ticks = div_round_ties_even(
            signed
                .checked_mul(cell_span)
                .and_then(|value| value.checked_mul(i128::from(jitter.bits())))
                .ok_or(Error::NumericOverflow)?,
            i128::from(u32::MAX)
                .checked_mul(i128::from(u16::MAX))
                .ok_or(Error::NumericOverflow)?,
        )?;
        ticks[axis] = center
            .checked_add(jitter_ticks)
            .ok_or(Error::NumericOverflow)?
            .clamp(minimum[axis], maximum[axis] - 1);
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

fn uniform_position(
    bounds: WorldBounds,
    stream: RandomStream,
    sample: u64,
) -> Result<WorldPosition> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let random = stream.sample(sample);
    let mut ticks = [0_i128; 3];
    for axis in 0..3 {
        let span = maximum[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let offset = span
            .checked_mul(i128::from(random[axis]))
            .ok_or(Error::NumericOverflow)?
            / (i128::from(u32::MAX) + 1);
        ticks[axis] = minimum[axis]
            .checked_add(offset)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

pub(super) fn contains_required_tags(tags: &[WeightedSurfaceTag], required: &[u64]) -> bool {
    required.iter().all(|required| {
        tags.iter()
            .any(|tag| tag.tag.0 == *required && tag.weight.bits() != 0)
    })
}

pub(super) fn distance_key(value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::NumericOverflow);
    }
    let bits = value.to_bits();
    Ok(bits)
}
