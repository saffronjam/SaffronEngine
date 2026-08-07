//! Threshold, suitability, cluster, and companion operators.

use super::*;

use saffron_spatial::{DecisionScalar, UnitInterval, div_round_ties_even};

use crate::{CompiledGraphNode, CompiledGraphUnit, Error, GraphClusterMode, Result};

pub(super) fn threshold_candidates(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let threshold = i32::from(unit_parameter(node, "threshold", UnitInterval::ZERO)?.bits());
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "threshold retained candidates",
    )?;
    for candidate in &candidates.candidates {
        if weights
            .values
            .get(&candidate.identity)
            .is_some_and(|value| value.bits() >= threshold)
        {
            accepted.push(candidate.clone());
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Threshold,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

pub(super) fn suitability_candidates(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    unit: &CompiledGraphUnit,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let binding = unit
        .suitability
        .iter()
        .find(|binding| binding.node_guid == node.definition.guid)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "suitability node requires one matching biome suitability binding".to_owned(),
        })?;
    let threshold = unit_parameter(node, "threshold", UnitInterval::ZERO)?;
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "suitability retained candidates",
    )?;
    for candidate in &candidates.candidates {
        let value = weights
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!("{:?} suitability sample", binding.channel),
            })?;
        if suitability_score(value, *binding)? >= threshold {
            accepted.push(candidate.clone());
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Threshold,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

fn suitability_score(
    value: DecisionScalar,
    binding: crate::SuitabilityBinding,
) -> Result<UnitInterval> {
    if value >= binding.minimum && value <= binding.maximum {
        return Ok(UnitInterval::ONE);
    }
    if binding.falloff.bits() <= 0 {
        return Ok(UnitInterval::ZERO);
    }
    let distance = if value < binding.minimum {
        binding.minimum.checked_sub(value)?
    } else {
        value.checked_sub(binding.maximum)?
    };
    if distance >= binding.falloff {
        return Ok(UnitInterval::ZERO);
    }
    let remaining = binding.falloff.checked_sub(distance)?;
    let bits = div_round_ties_even(
        i128::from(remaining.bits()) * i128::from(u16::MAX),
        i128::from(binding.falloff.bits()),
    )?;
    Ok(UnitInterval::from_bits(
        u16::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

pub(super) fn expand_cluster(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let children = u64::from(u32_parameter(node, "children", 0)?);
    let radius = fixed_parameter(node, "radius", DecisionScalar::from_bits(0))?;
    let radius_ticks = fixed_meters_to_ticks(radius)?.unsigned_abs() as i128;
    let mode = cluster_mode_parameter(node, "mode")?;
    let maximum_output = candidates
        .candidates
        .len()
        .checked_mul(
            usize::try_from(children)
                .map_err(|_| Error::NumericOverflow)?
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "candidate count",
        u64::try_from(maximum_output).map_err(|_| Error::NumericOverflow)?,
        state.graph.limits.max_candidates,
    )?;
    let mut expanded = Vec::new();
    crate::memory::reserve_exact(&mut expanded, maximum_output, "expanded cluster candidates")?;
    expanded.extend(candidates.candidates.iter().cloned());
    for parent in &candidates.candidates {
        for child in 0..children {
            let ordinal =
                candidate_ordinal(node, state, candidate_key_u128(parent.identity), child, 3)?;
            let stream = random_stream(
                node,
                state,
                "cluster",
                RandomSampleAddress::new(parent.owner, ordinal)
                    .with_ancestor(parent.identity.ordinal)
                    .with_species(parent.family.map_or(0, |family| u128::from(family.value()))),
            )?;
            let position = annulus_position(parent.position, 0, radius_ticks, stream)?;
            if !state.inputs.read_bounds.contains(position) {
                continue;
            }
            let mut candidate = parent.clone();
            candidate.identity = CandidateIdentity {
                node: node.definition.guid,
                node_address: node_execution_address(node, state),
                node_semantic_revision: node.definition.semantic_revision,
                ordinal,
                ancestor: parent.identity.ordinal,
            };
            candidate.position = position;
            candidate.owner = canonical_owner(position, node.definition.spatial.level())?;
            match mode {
                GraphClusterMode::Cluster => {
                    candidate.parent = Some(CandidateReference::from_candidate(parent));
                    candidate.colony = parent.colony;
                }
                GraphClusterMode::Patch => {
                    candidate.parent = None;
                    candidate.colony = None;
                }
                GraphClusterMode::Colony => {
                    candidate.parent = Some(CandidateReference::from_candidate(parent));
                    candidate.colony = parent
                        .colony
                        .or(Some(CandidateReference::from_candidate(parent)));
                }
            }
            candidate.authored_point = None;
            expanded.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: expanded,
    };
    result.canonicalize()?;
    Ok(result)
}

pub(super) fn expand_companions(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    unit: &CompiledGraphUnit,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let children = u64::from(u32_parameter(node, "children", 0)?);
    let maximum_depth = u64::from(u32_parameter(node, "maximumDepth", 0)?);
    let fallback_radius = fixed_meters_to_ticks(fixed_parameter(
        node,
        "radius",
        DecisionScalar::from_bits(0),
    )?)?
    .unsigned_abs() as i128;
    let mut rules: Vec<&crate::CompanionRule> = Vec::new();
    crate::memory::reserve_exact(&mut rules, unit.companions.len(), "companion rules")?;
    rules.extend(&unit.companions);
    rules.sort_unstable_by_key(|rule| {
        (
            rule.parent.value(),
            rule.child.value(),
            rule.minimum_distance,
            rule.maximum_distance,
            rule.probability,
        )
    });
    let mut maximum_output = candidates.candidates.len() as u64;
    let mut generation = candidates.candidates.len() as u64;
    for _ in 0..maximum_depth {
        generation = generation
            .checked_mul(children)
            .ok_or(Error::NumericOverflow)?;
        maximum_output = maximum_output
            .checked_add(generation)
            .ok_or(Error::NumericOverflow)?;
    }
    state.check_count(
        "candidate count",
        maximum_output,
        state.graph.limits.max_candidates,
    )?;
    state.check_transient_memory(companion_scratch_bytes(
        candidates.candidates.len() as u64,
        maximum_output,
        unit.companions.len() as u64,
    )?)?;
    let maximum_output = usize::try_from(maximum_output).map_err(|_| Error::NumericOverflow)?;
    let mut expanded = Vec::new();
    crate::memory::reserve_exact(&mut expanded, maximum_output, "expanded companions")?;
    expanded.extend(candidates.candidates.iter().cloned());
    let mut frontier = Vec::new();
    crate::memory::reserve_exact(
        &mut frontier,
        candidates.candidates.len(),
        "companion frontier",
    )?;
    frontier.extend(candidates.candidates.iter().cloned());
    for depth in 1..=maximum_depth {
        let mut next = Vec::new();
        let next_capacity = frontier
            .len()
            .checked_mul(usize::try_from(children).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)?;
        crate::memory::reserve_exact(&mut next, next_capacity, "companion frontier")?;
        for parent in &frontier {
            let eligible_count = rules
                .iter()
                .filter(|rule| parent.family.is_some_and(|family| rule.parent == family))
                .count();
            for child in 0..children {
                let ordinal = candidate_ordinal(
                    node,
                    state,
                    candidate_key_u128(parent.identity),
                    child,
                    u32::try_from(depth).map_err(|_| Error::NumericOverflow)?,
                )?;
                let stream = random_stream(
                    node,
                    state,
                    "companions",
                    RandomSampleAddress::new(parent.owner, ordinal)
                        .with_ancestor(parent.identity.ordinal)
                        .with_species(parent.family.map_or(0, |family| u128::from(family.value())))
                        .with_channel(u32::try_from(depth).map_err(|_| Error::NumericOverflow)?),
                )?;
                let rule = if eligible_count == 0 {
                    None
                } else {
                    let selected =
                        usize::try_from(u64::from(stream.lane(0, 0)) % eligible_count as u64)
                            .map_err(|_| Error::NumericOverflow)?;
                    rules
                        .iter()
                        .filter(|rule| parent.family.is_some_and(|family| rule.parent == family))
                        .nth(selected)
                        .copied()
                };
                if rule.is_some_and(|rule| !stream.chance(0, 1, rule.probability)) {
                    continue;
                }
                let (minimum, maximum) = if let Some(rule) = rule {
                    (
                        fixed_meters_to_ticks(rule.minimum_distance)?.unsigned_abs() as i128,
                        fixed_meters_to_ticks(rule.maximum_distance)?.unsigned_abs() as i128,
                    )
                } else {
                    (0, fallback_radius)
                };
                if minimum > maximum {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "companion distance range is inverted".to_owned(),
                    });
                }
                let position = annulus_position(parent.position, minimum, maximum, stream)?;
                if !state.inputs.read_bounds.contains(position) {
                    continue;
                }
                let mut candidate = parent.clone();
                candidate.identity = CandidateIdentity {
                    node: node.definition.guid,
                    node_address: node_execution_address(node, state),
                    node_semantic_revision: node.definition.semantic_revision,
                    ordinal,
                    ancestor: parent.identity.ordinal,
                };
                candidate.position = position;
                candidate.owner = canonical_owner(position, node.definition.spatial.level())?;
                candidate.family = rule.map(|rule| rule.child).or(parent.family);
                candidate.parent = Some(CandidateReference::from_candidate(parent));
                candidate.colony = parent
                    .colony
                    .or(Some(CandidateReference::from_candidate(parent)));
                candidate.authored_point = None;
                next.push(candidate);
            }
        }
        next.sort_unstable_by_key(|candidate| candidate.identity);
        expanded.extend(next.iter().cloned());
        frontier = next;
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: expanded,
    };
    result.canonicalize()?;
    Ok(result)
}
