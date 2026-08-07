//! Surface projection and the authoritative surface-hit contract.

use super::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use saffron_geometry::glam::DVec3;
use saffron_spatial::{
    DecisionScalar, SignedUnit, SurfaceField, SurfaceHit, SurfaceProjection,
    SurfaceProviderDescriptor, WeightedSurfaceTag, WorldPosition,
};

use crate::{CompiledGraphNode, Error, GraphAuthority, Result};

pub(super) fn project_candidates(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    output_demand: NodeOutputDemand<'_>,
    state: &mut EvaluationState<'_>,
) -> Result<(BTreeMap<String, GraphValue>, u64)> {
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    if authoritative
        && (state.inputs.surface_provider_set_hash == [0; 32]
            || state
                .inputs
                .surface_projection_tiles
                .iter()
                .filter(|tile| {
                    tile.node == node.definition.guid
                        && tile.node_semantic_revision == node.definition.semantic_revision
                })
                .any(|tile| tile.provider_set_hash != state.inputs.surface_provider_set_hash))
    {
        return Err(Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: "canonical surface projection tiles".to_owned(),
        });
    }
    let mut values = output_demand.contains("surface").then(BTreeMap::new);
    let mut retained = output_demand.contains("candidates").then(Vec::new);
    if let Some(retained) = retained.as_mut() {
        crate::memory::reserve_exact(
            retained,
            candidates.candidates.len(),
            "surface-projected retained candidates",
        )?;
    }
    let mut retained_count = 0_u64;
    for candidate in &candidates.candidates {
        state.check_abort()?;
        let projected = if authoritative {
            let sample = if let Some(sample) = state
                .inputs
                .surface_projection_tiles
                .iter()
                .filter(|tile| {
                    tile.node == node.definition.guid
                        && tile.node_semantic_revision == node.definition.semantic_revision
                })
                .find_map(|tile| tile.sample(candidate.position))
            {
                sample.clone()
            } else if state.pass == EvaluationPass::PrepareCanonicalInputs
                && !state.inputs.surface_providers.is_empty()
            {
                select_surface_hit(
                    node,
                    candidate.position,
                    &state.inputs.surface_providers,
                    true,
                )?
                .map(|hit| quantize_authoritative_surface_hit(node, hit))
                .transpose()?
            } else {
                return Err(Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: format!(
                        "exact surface projection query at {:?}",
                        candidate.position.global_ticks()
                    ),
                });
            };
            state
                .prepared_surface_projections
                .entry((
                    node.definition.guid,
                    node.definition.semantic_revision,
                    state.inputs.surface_provider_set_hash,
                ))
                .or_default()
                .insert(candidate.position, sample.clone());
            sample.as_ref().map(|sample| ProjectedSurfaceSample {
                position: sample.position,
                attachment: Some(sample.attachment),
                normal: sample.normal,
                projection: sample.projection,
                tags: sample.tags.clone(),
            })
        } else {
            select_surface_hit(
                node,
                candidate.position,
                &state.inputs.surface_providers,
                false,
            )?
            .map(|hit| {
                let normal = quantize_surface_normal(hit.frame.normal.to_array())?;
                let projection =
                    quantize_surface_projection(hit.coordinates.projection.to_array())?;
                validate_canonical_tags(&hit.tags)?;
                Ok::<_, Error>(ProjectedSurfaceSample {
                    position: hit.position,
                    attachment: hit.attachment,
                    normal,
                    projection,
                    tags: hit.tags,
                })
            })
            .transpose()?
        };
        if let Some(projected) = projected {
            let displacement = integer_sqrt(distance_squared(
                candidate.position.global_ticks(),
                projected.position.global_ticks(),
            )?);
            ensure_support_ticks(node, displacement)?;
            retained_count = retained_count
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?;
            if let Some(values) = values.as_mut() {
                values.insert(candidate.identity, projected);
            }
            if let Some(retained) = retained.as_mut() {
                retained.push(candidate.clone());
            }
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::SurfaceMiss,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    let mut outputs = BTreeMap::new();
    if let Some(retained) = retained {
        outputs.insert(
            "candidates".to_owned(),
            GraphValue::Candidates(CandidateStream {
                lineage: candidates.lineage,
                candidates: retained,
            }),
        );
    }
    if let Some(values) = values {
        outputs.insert(
            "surface".to_owned(),
            GraphValue::Surface(ProjectedSurfaceSamples {
                lineage: candidates.lineage,
                values,
            }),
        );
    }
    Ok((outputs, retained_count))
}

pub(super) fn select_surface_hit(
    node: &CompiledGraphNode,
    position: WorldPosition,
    providers: &[Arc<dyn SurfaceField>],
    authoritative_only: bool,
) -> Result<Option<SurfaceHit>> {
    let direction = fixed_vec3_parameter(node, "direction", [DecisionScalar::from_bits(0); 3])?;
    let direction = DVec3::new(
        direction[0].to_f64(),
        direction[1].to_f64(),
        direction[2].to_f64(),
    );
    let max_distance = fixed_parameter(node, "maxDistance", DecisionScalar::from_bits(0))?;
    let provider_filter = u64_parameter(node, "provider", 0)?;
    let required_tags = tag_list_parameter(node, "tags")?;
    let required_material_tags = tag_list_parameter(node, "materialTags")?;
    let query = SurfaceProjection::new(position, direction, max_distance.to_f64())?;
    let mut best: Option<((u64, u64), SurfaceHit)> = None;
    for provider in providers {
        let descriptor = provider.descriptor();
        if !descriptor.capabilities.project
            || (authoritative_only && !descriptor.capabilities.authoritative_attachments)
            || (provider_filter != 0 && descriptor.id.0 != provider_filter)
        {
            continue;
        }
        let Some(hit) = provider.project(&query)? else {
            continue;
        };
        validate_surface_hit_contract(node, &descriptor, &hit)?;
        if !contains_required_tags(&hit.tags, required_tags)
            || !contains_required_tags(&hit.tags, required_material_tags)
        {
            continue;
        }
        let key = (distance_key(hit.distance_m)?, hit.provider.0);
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, hit));
        }
    }
    Ok(best.map(|(_, hit)| hit))
}

pub(super) fn validate_surface_hit_contract(
    node: &CompiledGraphNode,
    descriptor: &SurfaceProviderDescriptor,
    hit: &SurfaceHit,
) -> Result<()> {
    if hit.provider != descriptor.id || hit.revision != descriptor.revision {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "surface hit identity does not match its provider descriptor".to_owned(),
        });
    }
    if let Some(attachment) = hit.attachment
        && (attachment.provider != descriptor.id || attachment.revision != descriptor.revision)
    {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "surface attachment identity does not match its provider descriptor".to_owned(),
        });
    }
    check_limit(
        "surface tags per hit",
        hit.tags.len() as u64,
        u64::from(descriptor.max_tags_per_hit),
    )?;
    validate_canonical_tags(&hit.tags)
}

pub(super) fn quantize_surface_normal(normal: [f32; 3]) -> Result<[SignedUnit; 3]> {
    let [x, y, z] = normal.map(|value| SignedUnit::from_f64(f64::from(value)));
    Ok([x?, y?, z?])
}

pub(super) fn quantize_surface_projection(projection: [f64; 3]) -> Result<[DecisionScalar; 3]> {
    let [x, y, z] = projection.map(DecisionScalar::from_f64);
    Ok([x?, y?, z?])
}

pub(super) fn validate_canonical_tags(tags: &[WeightedSurfaceTag]) -> Result<()> {
    if tags.windows(2).any(|pair| pair[0].tag >= pair[1].tag) {
        return Err(Error::GraphDocument {
            path: "surface.tags".to_owned(),
            reason: "surface tags must be sorted and unique".to_owned(),
        });
    }
    Ok(())
}
