//! Transform, priority exclusion, and bounds-overlap operators.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, UnitInterval, WorldBounds};

use crate::{CompiledGraphNode, Error, Result};

pub(super) fn transform_candidates(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let candidates = candidates_input(inputs, "candidates")?;
    let surface = optional_surface_input(inputs, "surface")?;
    let scales = optional_scalar_input(inputs, "scale")?;
    let offsets = optional_vector_input(inputs, "offset")?;
    let orient_to_surface = bool_parameter(node, "orientToSurface", false)?;
    let yaw_minimum = unit_parameter(node, "yawMinimum", UnitInterval::ZERO)?;
    let yaw_maximum = unit_parameter(node, "yawMaximum", UnitInterval::ONE)?;
    let scale_minimum = fixed_parameter(node, "scaleMinimum", DecisionScalar::from_bits(65_536))?;
    let scale_maximum = fixed_parameter(node, "scaleMaximum", DecisionScalar::from_bits(65_536))?;
    let variation_count = u32_parameter(node, "variationCount", 1)?.max(1);
    if yaw_minimum > yaw_maximum || scale_minimum.bits() <= 0 || scale_minimum > scale_maximum {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "transform yaw/scale range is invalid".to_owned(),
        });
    }
    for (name, lineage) in [
        ("surface", surface.map(|value| value.lineage)),
        ("scale", scales.map(|value| value.lineage)),
        ("offset", offsets.map(|value| value.lineage)),
    ] {
        if let Some(lineage) = lineage {
            ensure_lineage(node, name, candidates.lineage, lineage)?;
        }
    }
    let mut transformed = Vec::new();
    crate::memory::reserve_exact(
        &mut transformed,
        candidates.candidates.len(),
        "transformed candidates",
    )?;
    for candidate in &candidates.candidates {
        let mut candidate = candidate.clone();
        if let Some(projected) = surface.and_then(|surface| surface.values.get(&candidate.identity))
        {
            candidate.position = projected.position;
            candidate.owner = canonical_owner(projected.position, node.definition.spatial.level())?;
            candidate.attachment = projected.attachment;
            candidate.surface_normal = Some(projected.normal);
            candidate.surface_projection = projected.projection;
        }
        if let Some(offset) = offsets.and_then(|field| field.values.get(&candidate.identity)) {
            let [offset_x, offset_y, offset_z] =
                [offset.x, offset.y, offset.z].map(fixed_meters_to_ticks);
            let offset_ticks = [offset_x?, offset_y?, offset_z?];
            let offset_length = integer_sqrt(distance_squared(
                [0; 3],
                [offset_ticks[0], offset_ticks[1], offset_ticks[2]],
            )?);
            ensure_support_ticks(node, offset_length)?;
            candidate.position = offset_fixed(candidate.position, [offset.x, offset.y, offset.z])?;
            candidate.owner = canonical_owner(candidate.position, node.definition.spatial.level())?;
        }
        let stream = random_stream(
            node,
            state,
            "variation",
            RandomSampleAddress::new(candidate.owner, candidate.identity.ordinal)
                .with_ancestor(candidate.identity.ancestor)
                .with_species(
                    candidate
                        .family
                        .map_or(0, |family| u128::from(family.value())),
                )
                .with_channel(2),
        )?;
        let yaw = interpolate_unit(yaw_minimum, yaw_maximum, stream.unit(0, 0))?;
        let random_scale = scale_minimum.lerp(scale_maximum, stream.unit(0, 1))?;
        let sampled_scale = scales
            .and_then(|field| field.values.get(&candidate.identity))
            .copied()
            .unwrap_or(DecisionScalar::from_bits(65_536));
        let scale = random_scale.checked_mul(sampled_scale)?;
        candidate.scale = [scale; 3];
        candidate.variation = stream.lane(0, 2) % variation_count;
        candidate.orientation = if orient_to_surface {
            orientation_from_normal_and_yaw(candidate.surface_normal, yaw)?
        } else {
            yaw_orientation(yaw)?
        };
        transformed.push(candidate);
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: transformed,
    };
    result.canonicalize()?;
    Ok(result)
}

pub(super) fn priority_exclusion(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    radii: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    ensure_lineage(node, "radius", candidates.lineage, radii.lineage)?;
    state.check_transient_memory(xz_filter_scratch_bytes(candidates.candidates.len() as u64)?)?;
    let keep_highest = bool_parameter(node, "keepHighest", true)?;
    let mut transformed = Vec::new();
    crate::memory::reserve_exact(
        &mut transformed,
        candidates.candidates.len(),
        "priority-exclusion candidates",
    )?;
    transformed.extend(candidates.candidates.iter().cloned());
    for candidate in &mut transformed {
        if let Some(weight) = weights.values.get(&candidate.identity) {
            candidate.priority = if keep_highest {
                *weight
            } else {
                DecisionScalar::from_bits(
                    weight.bits().checked_neg().ok_or(Error::NumericOverflow)?,
                )
            };
        }
    }
    let mut radius_ticks = Vec::new();
    crate::memory::reserve_exact(
        &mut radius_ticks,
        candidates.candidates.len(),
        "priority-exclusion radii",
    )?;
    for candidate in &candidates.candidates {
        let radius = radii
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "priority-exclusion radius sample".to_owned(),
            })?;
        radius_ticks.push((
            candidate.identity,
            nonnegative_radius_ticks(node, "priority-exclusion radius sample", radius)?,
        ));
    }
    radius_ticks.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_support = radius_ticks
        .iter()
        .map(|(_, radius)| *radius)
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    transformed.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "priority-exclusion accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "priority-exclusion bucket links",
    )?;
    for candidate in transformed {
        let radius = lookup_candidate_radius(
            &radius_ticks,
            candidate.identity,
            node,
            "priority-exclusion radius sample",
        )?;
        let mut excluded = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let mut index = bucket_heads.get(&key).copied();
                while let Some(current) = index {
                    let other = accepted.get(current).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "priority-exclusion spatial index is invalid".to_owned(),
                    })?;
                    let other_radius = lookup_candidate_radius(
                        &radius_ticks,
                        other.identity,
                        node,
                        "priority-exclusion radius sample",
                    )?;
                    let required = radius
                        .checked_add(other_radius)
                        .ok_or(Error::NumericOverflow)?;
                    ensure_support_ticks(node, required)?;
                    if distance_squared_xz(candidate.position, other.position)?
                        < required
                            .checked_mul(required)
                            .ok_or(Error::NumericOverflow)?
                    {
                        excluded = true;
                        break 'neighbours;
                    }
                    index = *bucket_links
                        .get(current)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "priority-exclusion spatial index is invalid".to_owned(),
                        })?;
                }
            }
        }
        if excluded {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::PriorityExclusion,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push(candidate);
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

pub(super) fn nonnegative_radius_ticks(
    node: &CompiledGraphNode,
    input: &'static str,
    radius: DecisionScalar,
) -> Result<i128> {
    if radius.bits() < 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!("{input} must be nonnegative"),
        });
    }
    fixed_meters_to_ticks(radius)
}

pub(super) fn bounds_overlap(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    state.check_transient_memory(bounds_overlap_scratch_bytes(
        candidates.candidates.len() as u64
    )?)?;
    let padding = fixed_parameter(node, "padding", DecisionScalar::from_bits(0))?;
    let padding_ticks = fixed_meters_to_ticks(padding)?.unsigned_abs() as i128;
    let mut ordered = Vec::new();
    crate::memory::reserve_exact(
        &mut ordered,
        candidates.candidates.len(),
        "bounds-overlap candidates",
    )?;
    for candidate in candidates.candidates.iter().cloned() {
        let bounds = expand_bounds_checked(candidate_bounds(&candidate, state)?, padding_ticks)?;
        let radius = bounds_support_radius(candidate.position, bounds)?;
        ensure_support_ticks(node, radius)?;
        ordered.push((candidate, bounds, radius));
    }
    let maximum_support = ordered
        .iter()
        .map(|(_, _, radius)| *radius)
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    ordered.sort_unstable_by(|left, right| {
        right
            .0
            .priority
            .cmp(&left.0.priority)
            .then_with(|| left.0.identity.cmp(&right.0.identity))
    });
    let mut accepted: Vec<(GraphCandidate, WorldBounds, i128)> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "bounds-overlap accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "bounds-overlap bucket links",
    )?;
    for (candidate, bounds, radius) in ordered {
        let mut overlaps = false;
        let bucket = xyz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let key = offset_xyz_bucket(bucket, x, y, z)?;
                    let mut index = bucket_heads.get(&key).copied();
                    while let Some(current) = index {
                        let (_, other_bounds, other_radius) =
                            accepted.get(current).ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "bounds-overlap spatial index is invalid".to_owned(),
                            })?;
                        let total = radius
                            .checked_add(*other_radius)
                            .ok_or(Error::NumericOverflow)?;
                        ensure_support_ticks(node, total)?;
                        if bounds_intersect(bounds, *other_bounds) {
                            overlaps = true;
                            break 'neighbours;
                        }
                        index = *bucket_links
                            .get(current)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "bounds-overlap spatial index is invalid".to_owned(),
                            })?;
                    }
                }
            }
        }
        if overlaps {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push((candidate, bounds, radius));
        }
    }
    let mut retained = Vec::new();
    crate::memory::reserve_exact(
        &mut retained,
        accepted.len(),
        "bounds-overlap retained candidates",
    )?;
    retained.extend(accepted.into_iter().map(|(candidate, _, _)| candidate));
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: retained,
    };
    result.canonicalize()?;
    Ok(result)
}
