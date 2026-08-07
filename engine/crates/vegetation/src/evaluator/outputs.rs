//! Diagnostic, macro-point, and micro-field output operators.

use super::*;

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{UnitInterval, div_round_ties_even};

use crate::hash::sha256;
use crate::identity::derive_procedural_plant_id;
use crate::{
    CompiledGraphNode, Error, GraphParameterValue, PlantFlags, PlantLifecycle, PlantPoint,
    ProceduralPlantIdentity, ProvenanceDecision, ProvenanceDecisionOutcome, ProvenanceRecord,
    Result,
};

pub(super) fn diagnostic_output(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    state: &EvaluationState<'_>,
) -> Result<NamedDiagnosticStream> {
    let candidates = optional_candidates_input(inputs, "candidates")?;
    let field = optional_scalar_input(inputs, "field")?;
    let lineage = match (candidates, field) {
        (Some(candidates), Some(field)) => {
            ensure_lineage(node, "field", candidates.lineage, field.lineage)?;
            Some(candidates.lineage)
        }
        (Some(candidates), None) => Some(candidates.lineage),
        (None, Some(field)) => Some(field.lineage),
        (None, None) => None,
    };
    let label = string_parameter(node, "label", "")?;
    let label = if label.is_empty() {
        node.debug_symbol.label.as_str()
    } else {
        label
    };
    let scope = lineage.map_or(
        DiagnosticStreamScope::GlobalSnapshot,
        DiagnosticStreamScope::CandidateLineage,
    );
    let rejected = lineage.map_or_else(
        || state.diagnostics.rejected.clone(),
        |lineage| {
            state
                .rejected_by_lineage
                .get(&lineage)
                .cloned()
                .unwrap_or_default()
        },
    );
    Ok(NamedDiagnosticStream {
        node: node.address(),
        label: label.to_owned(),
        scope,
        candidates: candidates.map(|stream| {
            stream
                .candidates
                .iter()
                .map(|candidate| DiagnosticCandidateSample {
                    identity: candidate.identity,
                    owner: candidate.owner,
                    position: candidate.position,
                    family: candidate.family,
                    variation: candidate.variation,
                    priority: candidate.priority,
                    ecology_tick: candidate.ecology_tick,
                })
                .collect()
        }),
        field: field.map(|field| {
            field
                .values
                .iter()
                .map(|(candidate, value)| DiagnosticScalarSample {
                    candidate: *candidate,
                    value: *value,
                })
                .collect()
        }),
        rejected,
    })
}

pub(super) fn macro_output(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<Vec<PlantPoint>> {
    if node.definition.spatial.level() > state.inputs.output_cell.level() {
        state.ancestor_references.insert(
            state
                .inputs
                .output_cell
                .ancestor(node.definition.spatial.level())?,
        );
    }
    let candidates = candidates_input(inputs, "candidates")?;
    state.check_transient_memory(macro_output_scratch_bytes(
        candidates.candidates.len() as u64
    )?)?;
    let species = species_input(inputs, "species")?;
    let representation_class = match node.definition.parameter("representationClass") {
        Some(GraphParameterValue::U32(value)) => Some(*value),
        Some(_) => return wrong_parameter(node, "representationClass"),
        None => None,
    };
    let phenotype = match node.definition.parameter("phenotype") {
        Some(GraphParameterValue::U32(value)) => Some(*value),
        Some(_) => return wrong_parameter(node, "phenotype"),
        None => None,
    };
    let mut resolved = Vec::new();
    crate::memory::reserve_exact(
        &mut resolved,
        candidates.candidates.len(),
        "macro resolved candidates",
    )?;
    for candidate in &candidates.candidates {
        if candidate.owner != state.inputs.output_cell {
            if candidate.owner.level() > state.inputs.output_cell.level()
                && state
                    .inputs
                    .output_cell
                    .ancestor(candidate.owner.level())
                    .is_ok_and(|ancestor| ancestor == candidate.owner)
            {
                state.ancestor_references.insert(candidate.owner);
                continue;
            }
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::ForeignOwner,
                candidate.family,
                candidate.variation,
                state,
            )?;
            continue;
        }
        let family = match candidate.family {
            Some(family) => Some(family),
            None => select_species(node, candidate.owner, candidate.identity, species, state)?,
        };
        let Some(family) = family else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::NoSpecies,
                None,
                candidate.variation,
                state,
            )?;
            continue;
        };
        let seed_namespace = species_seed_namespace(node, family, species)?;
        let id = candidate.authored_point.as_ref().map_or_else(
            || {
                derive_procedural_plant_id(ProceduralPlantIdentity {
                    map: state.inputs.map,
                    layer_guid: candidate.source_layer,
                    node_address: candidate.identity.node_address,
                    node_semantic_revision: candidate.identity.node_semantic_revision,
                    candidate: candidate.identity.ordinal,
                    ancestor: candidate.identity.ancestor,
                    seed_namespace,
                    owner: candidate.owner,
                    family,
                })
            },
            |point| point.id,
        );
        resolved.push((candidate, family, id));
    }

    let mut points = Vec::new();
    crate::memory::reserve_exact(&mut points, resolved.len(), "macro plant points")?;
    for &(candidate, family, id) in &resolved {
        let authored = candidate.authored_point.as_ref();
        let parent = match candidate.parent {
            Some(reference) => resolved
                .binary_search_by_key(&reference.identity, |candidate| candidate.0.identity)
                .ok()
                .and_then(|index| resolved.get(index).map(|(_, _, id)| *id))
                .map(Some)
                .unwrap_or(resolve_candidate_reference(
                    node, reference, species, state,
                )?),
            None => authored.and_then(|point| point.parent),
        };
        let colony = match candidate.colony {
            Some(reference) => resolved
                .binary_search_by_key(&reference.identity, |candidate| candidate.0.identity)
                .ok()
                .and_then(|index| resolved.get(index).map(|(_, _, id)| *id))
                .map(Some)
                .unwrap_or(resolve_candidate_reference(
                    node, reference, species, state,
                )?),
            None => authored.and_then(|point| point.colony),
        };
        let bounds = authored.map_or_else(
            || prototype_bounds(prototype_for_family(state, family)?, candidate),
            |point| Ok(point.bounds),
        )?;
        let parent_decision = state
            .candidate_decisions
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "accepted candidate has no provenance decision".to_owned(),
            })?;
        let decision = state.provenance.intern_decision(ProvenanceDecision {
            parents: vec![parent_decision],
            subgraph_path: node.debug_symbol.module_path.clone(),
            node: node.definition.guid,
            operator: node.definition.operator,
            candidate: candidate.identity.ordinal,
            outcome: ProvenanceDecisionOutcome::Accepted,
        });
        let provenance = state.provenance.intern(ProvenanceRecord {
            map: state.inputs.map,
            layer: candidate.source_layer,
            biome: state.graph.biome,
            decision,
            candidate: candidate.identity.ordinal,
            family: Some(family),
            plant: Some(id),
            variation: candidate.variation,
        });
        points.push(PlantPoint {
            id,
            owner: candidate.owner,
            position: candidate.position,
            orientation: candidate.orientation,
            scale: candidate.scale,
            bounds,
            family,
            variation: candidate.variation,
            lifecycle: authored.map_or(PlantLifecycle::Mature, |point| point.lifecycle),
            phenotype: phenotype
                .or_else(|| authored.map(|point| point.phenotype))
                .unwrap_or(0),
            representation_class: representation_class
                .or_else(|| authored.map(|point| point.representation_class))
                .unwrap_or(0),
            deterministic_key: authored.map_or_else(
                || candidate_key_u128(candidate.identity),
                |point| point.deterministic_key,
            ),
            candidate: authored.map_or(candidate.identity.ordinal, |point| point.candidate),
            parent,
            colony,
            ecology_tick: candidate.ecology_tick,
            health: authored.map_or(UnitInterval::ONE, |point| point.health),
            moisture: authored.map_or(UnitInterval::from_bits(32_768), |point| point.moisture),
            fuel: authored.map_or(UnitInterval::ONE, |point| point.fuel),
            phenology: authored.map_or(UnitInterval::ZERO, |point| point.phenology),
            flags: authored.map_or(PlantFlags::default(), |point| {
                point.flags.union(PlantFlags::AUTHORED)
            }),
            interaction_policy: match authored {
                Some(point) => point.interaction_policy,
                None => prototype_for_family(state, family)?.interaction_policy,
            },
            provenance: provenance.0,
            attachment: candidate.attachment,
            surface_projection: candidate.surface_projection,
        });
    }
    state.diagnostics.candidate_count = state
        .diagnostics
        .candidate_count
        .max(candidates.candidates.len() as u64);
    Ok(points)
}

pub(super) fn micro_output(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    state: &EvaluationState<'_>,
) -> Result<Vec<MicroFieldTile>> {
    let candidates = candidates_input(inputs, "candidates")?;
    let density = optional_scalar_input(inputs, "density")?;
    if let Some(density) = density {
        ensure_lineage(node, "density", candidates.lineage, density.lineage)?;
    }
    let dimensions = u32_vec3_parameter(node, "dimensions")?;
    let channels = guid_list_parameter(node, "attributeChannels")?;
    let samples_per_family = dimensions.iter().try_fold(1_u64, |product, value| {
        product
            .checked_mul(u64::from(*value))
            .ok_or(Error::NumericOverflow)
    })?;
    let mut families = Vec::new();
    crate::memory::reserve_exact(
        &mut families,
        candidates.candidates.len(),
        "micro candidate families",
    )?;
    for candidate in &candidates.candidates {
        if candidate.owner != state.inputs.output_cell {
            continue;
        }
        families.push(
            candidate
                .family
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "micro candidate family".to_owned(),
                })?,
        );
    }
    families.sort_unstable_by_key(|family| family.value());
    families.dedup_by_key(|family| family.value());
    let family_count = u64::try_from(families.len()).map_err(|_| Error::NumericOverflow)?;
    let total_samples = samples_per_family
        .checked_mul(family_count)
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "micro samples",
        total_samples,
        state.graph.limits.max_micro_samples,
    )?;
    state.check_transient_memory(micro_output_scratch_bytes(
        total_samples,
        channels.len() as u64,
    )?)?;
    let sample_count = usize::try_from(samples_per_family).map_err(|_| Error::NumericOverflow)?;
    let mut attribute_fields = Vec::new();
    crate::memory::reserve_exact(
        &mut attribute_fields,
        channels.len(),
        "micro attribute fields",
    )?;
    for channel in channels {
        let name = format!("attribute-{channel:032x}");
        let field = scalar_input(inputs, &name)?;
        ensure_lineage(node, &name, candidates.lineage, field.lineage)?;
        attribute_fields.push((*channel, field));
    }
    let bounds = state.inputs.output_bounds;
    let mut tiles = Vec::new();
    crate::memory::reserve_exact(&mut tiles, families.len(), "micro family tiles")?;
    for family in families {
        let mut samples = Vec::new();
        crate::memory::reserve_exact(&mut samples, sample_count, "micro density")?;
        samples.resize(sample_count, 0_u16);
        let mut attribute_weights = Vec::new();
        crate::memory::reserve_exact(
            &mut attribute_weights,
            sample_count,
            "micro attribute weights",
        )?;
        attribute_weights.resize(sample_count, 0_u64);
        let mut attribute_sums = BTreeMap::new();
        for (channel, _) in &attribute_fields {
            let mut sums = Vec::new();
            crate::memory::reserve_exact(&mut sums, sample_count, "micro attribute sums")?;
            sums.resize(sample_count, 0_i128);
            attribute_sums.insert(*channel, sums);
        }
        for candidate in &candidates.candidates {
            if candidate.owner != state.inputs.output_cell || candidate.family != Some(family) {
                continue;
            }
            let index = tile_index(candidate.position, bounds, dimensions)?;
            let value = density
                .and_then(|field| field.values.get(&candidate.identity))
                .map_or(u16::MAX, |value| {
                    value.bits().clamp(0, i32::from(u16::MAX)) as u16
                });
            samples[index] = samples[index].saturating_add(value);
            attribute_weights[index] = attribute_weights[index]
                .checked_add(u64::from(value))
                .ok_or(Error::NumericOverflow)?;
            for (channel, field) in &attribute_fields {
                let attribute = field.values.get(&candidate.identity).ok_or_else(|| {
                    Error::GraphAuthoritativeInput {
                        node: node.definition.guid,
                        input: format!("micro attribute {channel:032x}"),
                    }
                })?;
                let weighted = i128::from(attribute.bits())
                    .checked_mul(i128::from(value))
                    .ok_or(Error::NumericOverflow)?;
                let sums = attribute_sums
                    .get_mut(channel)
                    .ok_or_else(|| Error::GraphDocument {
                        path: format!("{}.parameters.attributeChannels", node.debug_symbol.label),
                        reason: format!("micro attribute channel {channel:032x} was not allocated"),
                    })?;
                sums[index] = sums[index]
                    .checked_add(weighted)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
        let attributes = attribute_sums
            .into_iter()
            .map(|(channel, sums)| {
                let mut values = Vec::new();
                crate::memory::reserve_exact(&mut values, sample_count, "micro attributes")?;
                for (sum, weight) in sums.into_iter().zip(&attribute_weights) {
                    let value = if *weight == 0 {
                        0
                    } else {
                        i32::try_from(div_round_ties_even(sum, i128::from(*weight))?)
                            .map_err(|_| Error::NumericOverflow)?
                    };
                    values.push(value);
                }
                Ok((channel, values))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        tiles.push(MicroFieldTile {
            cell: state.inputs.output_cell,
            family,
            dimensions,
            density: samples,
            attributes,
            reconstruction_seed: micro_reconstruction_seed(node, family)?,
        });
    }
    Ok(tiles)
}

fn micro_reconstruction_seed(node: &CompiledGraphNode, family: Uuid) -> Result<u128> {
    let hash = sha256(
        &[
            b"saffron-anima/micro-reconstruction-family/v1\0".as_slice(),
            seed_namespace(node, "reconstruction")?
                .to_be_bytes()
                .as_slice(),
            family.value().to_be_bytes().as_slice(),
        ]
        .concat(),
    );
    Ok(u128::from_be_bytes(hash[..16].try_into().unwrap()))
}
