//! Community blending and succession inputs.

use super::*;

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::UnitInterval;

use crate::{CompiledGraphNode, CompiledGraphUnit, Error, Result};

pub(super) fn community_blend(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    unit: &CompiledGraphUnit,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    let candidates = candidates_input(inputs, "candidates")?;
    let communities = communities_input(inputs, "communities")?;
    let shade = optional_scalar_input(inputs, "shade")?;
    let shade_bias = unit_parameter(node, "shadeTolerance", UnitInterval::ZERO)?;
    if let Some(shade) = shade {
        ensure_lineage(node, "shade", candidates.lineage, shade.lineage)?;
    }
    state.check_transient_memory(community_blend_scratch_bytes(
        candidates.candidates.len() as u64,
        unit.palette.len() as u64,
    )?)?;
    let mut palette = Vec::new();
    crate::memory::reserve_exact(&mut palette, unit.palette.len(), "community palette")?;
    palette.extend(unit.palette.iter().cloned());
    palette.sort_unstable_by_key(|entry| entry.plant.value());
    let mut weights = Vec::new();
    crate::memory::reserve_exact(&mut weights, palette.len(), "community weights")?;
    let mut result = Vec::new();
    crate::memory::reserve_exact(
        &mut result,
        candidates.candidates.len(),
        "community candidates",
    )?;
    for source_candidate in &candidates.candidates {
        let mut candidate = source_candidate.clone();
        let shade_value = shade
            .and_then(|field| field.values.get(&candidate.identity))
            .map_or(0, |value| value.bits().clamp(0, i32::from(u16::MAX)) as u16);
        weights.clear();
        for entry in &palette {
            let prototype = prototype_for_family(state, entry.plant)?;
            let tolerance = prototype
                .shade_tolerance
                .bits()
                .saturating_add(shade_bias.bits());
            let shade_scale =
                u64::from(u16::MAX.saturating_sub(shade_value.saturating_sub(tolerance)));
            weights.push((
                entry.plant.value(),
                u64::from(entry.weight.bits())
                    .checked_mul(shade_scale)
                    .ok_or(Error::NumericOverflow)?,
            ));
        }
        for rule in communities
            .succession
            .iter()
            .filter(|rule| state.inputs.ecology_tick >= rule.minimum_tick)
        {
            let from_index = weights
                .binary_search_by_key(&rule.from.value(), |(family, _)| *family)
                .map_err(|_| Error::NumericOverflow)?;
            let to_index = weights
                .binary_search_by_key(&rule.to.value(), |(family, _)| *family)
                .map_err(|_| Error::NumericOverflow)?;
            let from = weights[from_index].1;
            let transfer = from
                .checked_mul(u64::from(rule.probability.bits()))
                .ok_or(Error::NumericOverflow)?
                / u64::from(u16::MAX);
            if transfer == 0 {
                continue;
            }
            weights[from_index].1 = from - transfer;
            weights[to_index].1 = weights[to_index]
                .1
                .checked_add(transfer)
                .ok_or(Error::NumericOverflow)?;
        }
        if let Some(parent) = candidate.parent.and_then(|parent| parent.family) {
            for rule in communities
                .companions
                .iter()
                .filter(|rule| rule.parent == parent)
            {
                let child_index = weights
                    .binary_search_by_key(&rule.child.value(), |(family, _)| *family)
                    .map_err(|_| Error::NumericOverflow)?;
                let child = weights[child_index].1;
                let boost = child
                    .checked_mul(u64::from(rule.probability.bits()))
                    .ok_or(Error::NumericOverflow)?
                    / u64::from(u16::MAX);
                weights[child_index].1 = child.checked_add(boost).ok_or(Error::NumericOverflow)?;
            }
        }
        let total = weights.iter().try_fold(0_u64, |total, (_, weight)| {
            total.checked_add(*weight).ok_or(Error::NumericOverflow)
        })?;
        if total == 0 {
            reject_candidate(
                node,
                source_candidate,
                candidates.lineage,
                CandidateRejectionReason::NoSpecies,
                source_candidate.family,
                source_candidate.variation,
                state,
            )?;
            continue;
        }
        let stream = random_stream(
            node,
            state,
            "community",
            RandomSampleAddress::new(candidate.owner, candidate.identity.ordinal)
                .with_ancestor(candidate.identity.ancestor)
                .with_channel(3),
        )?;
        let mut selection = u64::from(stream.lane(0, 0)) % total;
        for &(family, weight) in &weights {
            if selection < weight {
                let family = Uuid(family);
                let prototype = prototype_for_family(state, family)?;
                candidate.family = Some(family);
                candidate.crown_radius = prototype.crown_radius[0].max(prototype.crown_radius[1]);
                candidate.root_radius = prototype.root_radius[0].max(prototype.root_radius[1]);
                break;
            }
            selection -= weight;
        }
        result.push(candidate);
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: result,
    })
}

pub(super) fn succession_input(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    unit: &CompiledGraphUnit,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let mut result = candidates.clone();
    for candidate in &mut result.candidates {
        let Some(family) = candidate.family else {
            candidate.ecology_tick = state.inputs.ecology_tick;
            continue;
        };
        if let Some(rule) = unit
            .succession
            .iter()
            .filter(|rule| rule.from == family && rule.minimum_tick <= state.inputs.ecology_tick)
            .max_by_key(|rule| (rule.minimum_tick, rule.to.value()))
        {
            let decision = stable_ordinal(&[
                &candidate_key_u128(candidate.identity).to_be_bytes(),
                &rule.from.value().to_be_bytes(),
                &rule.to.value().to_be_bytes(),
                &rule.minimum_tick.to_be_bytes(),
                &state.inputs.ecology_tick.to_be_bytes(),
            ])?;
            let stream = random_stream(
                node,
                state,
                "succession",
                RandomSampleAddress::new(candidate.owner, decision)
                    .with_ancestor(candidate.identity.ordinal)
                    .with_species(u128::from(family.value())),
            )?;
            if stream.chance(0, 0, rule.probability) {
                let prototype = prototype_for_family(state, rule.to)?;
                candidate.family = Some(rule.to);
                candidate.crown_radius = prototype.crown_radius[0].max(prototype.crown_radius[1]);
                candidate.root_radius = prototype.root_radius[0].max(prototype.root_radius[1]);
            }
        }
        candidate.ecology_tick = state.inputs.ecology_tick;
    }
    Ok(result)
}
