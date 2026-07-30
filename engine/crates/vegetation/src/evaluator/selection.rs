//! Per-candidate species selection, prototype lookup, random streams, and ordinals.

use super::*;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, RandomDomain, RandomStream, WorldCellKey, WorldPosition};

use crate::hash::VegetationContentHasher;
use crate::identity::derive_procedural_plant_id;
use crate::{
    CompiledGraphNode, Error, PlantId, PlantPoint, ProceduralPlantIdentity, ProvenanceDecision,
    ProvenanceDecisionOutcome, ProvenanceRecord, QuantizedOrientation, Result,
};

pub(super) fn reject_candidate(
    node: &CompiledGraphNode,
    candidate: &GraphCandidate,
    lineage: CandidateLineage,
    reason: CandidateRejectionReason,
    family: Option<Uuid>,
    variation: u32,
    state: &mut EvaluationState<'_>,
) -> Result<()> {
    let parents = state
        .candidate_decisions
        .get(&candidate.identity)
        .copied()
        .into_iter()
        .collect();
    let decision = state.provenance.intern_decision(ProvenanceDecision {
        parents,
        subgraph_path: node.debug_symbol.module_path.clone(),
        node: node.definition.guid,
        operator: node.definition.operator,
        candidate: candidate.identity.ordinal,
        outcome: ProvenanceDecisionOutcome::Rejected,
    });
    let provenance = state.provenance.intern(ProvenanceRecord {
        map: state.inputs.map,
        layer: candidate.source_layer,
        biome: state.graph.biome,
        decision,
        candidate: candidate.identity.ordinal,
        family,
        plant: None,
        variation,
    });
    let rejected = RejectedCandidate {
        candidate: candidate.identity,
        position: candidate.position,
        reason,
        provenance,
    };
    crate::memory::reserve_exact(
        &mut state.diagnostics.rejected,
        1,
        "rejected candidate diagnostics",
    )?;
    state.diagnostics.rejected.push(rejected.clone());
    let lineage_rejections = state.rejected_by_lineage.entry(lineage).or_default();
    crate::memory::reserve_exact(lineage_rejections, 1, "lineage rejection history")?;
    lineage_rejections.push(rejected);
    Ok(())
}

pub(super) fn select_species(
    node: &CompiledGraphNode,
    owner: WorldCellKey,
    identity: CandidateIdentity,
    species: &[crate::BiomePaletteEntry],
    state: &EvaluationState<'_>,
) -> Result<Option<Uuid>> {
    let total: u64 = species
        .iter()
        .map(|entry| u64::from(entry.weight.bits()))
        .sum();
    if total == 0 {
        return Ok(None);
    }
    let stream = random_stream(
        node,
        state,
        "species-selection",
        RandomSampleAddress::new(owner, identity.ordinal)
            .with_ancestor(identity.ancestor)
            .with_channel(4),
    )?;
    let mut selection = u64::from(stream.lane(0, 0)) % total;
    let mut previous = None;
    for _ in 0..species.len() {
        let entry = species
            .iter()
            .filter(|entry| previous.is_none_or(|plant| entry.plant.value() > plant))
            .min_by_key(|entry| entry.plant.value())
            .ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "biome palette ordering is incomplete".to_owned(),
            })?;
        let weight = u64::from(entry.weight.bits());
        if selection < weight {
            return Ok(Some(entry.plant));
        }
        selection -= weight;
        previous = Some(entry.plant.value());
    }
    Ok(None)
}

pub(super) fn species_seed_namespace(
    node: &CompiledGraphNode,
    family: Uuid,
    species: &[crate::BiomePaletteEntry],
) -> Result<u128> {
    species
        .iter()
        .find(|entry| entry.plant == family)
        .map(|entry| entry.seed_namespace)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!(
                "selected plant family {} is absent from the biome palette",
                family.value()
            ),
        })
}

pub(super) fn resolve_candidate_reference(
    node: &CompiledGraphNode,
    reference: CandidateReference,
    species: &[crate::BiomePaletteEntry],
    state: &EvaluationState<'_>,
) -> Result<Option<PlantId>> {
    if let Some(id) = reference.authored_id {
        return Ok(Some(id));
    }
    let family = match reference.family {
        Some(family) => Some(family),
        None => select_species(node, reference.owner, reference.identity, species, state)?,
    };
    let Some(family) = family else {
        return Ok(None);
    };
    let seed_namespace = species_seed_namespace(node, family, species)?;
    Ok(Some(derive_procedural_plant_id(ProceduralPlantIdentity {
        map: state.inputs.map,
        layer_guid: reference.source_layer,
        node_address: reference.identity.node_address,
        node_semantic_revision: reference.identity.node_semantic_revision,
        candidate: reference.identity.ordinal,
        ancestor: reference.identity.ancestor,
        seed_namespace,
        owner: reference.owner,
        family,
    })))
}

pub(super) fn prototype_for_family<'a>(
    state: &'a EvaluationState<'_>,
    family: Uuid,
) -> Result<&'a PlantPrototype> {
    state
        .inputs
        .plant_prototypes
        .binary_search_by_key(&family.value(), |prototype| prototype.family.value())
        .ok()
        .map(|index| &state.inputs.plant_prototypes[index])
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: 0,
            input: format!("plant prototype {}", family.value()),
        })
}

pub(super) fn random_stream(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    namespace: &str,
    address: RandomSampleAddress,
) -> Result<RandomStream> {
    Ok(RandomStream::new(RandomDomain {
        map: u128::from(state.inputs.map.value()),
        node_guid: node_execution_address(node, state),
        node_semantic_revision: node.definition.semantic_revision,
        seed_namespace: seed_namespace(node, namespace)?,
        cell: address.cell,
        candidate: address.candidate,
        ancestor: address.ancestor,
        species: address.species,
        channel: address.channel,
    }))
}

pub(super) fn seed_namespace(node: &CompiledGraphNode, name: &str) -> Result<u128> {
    node.definition
        .seed_namespaces
        .get(name)
        .copied()
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!("stochastic operation has no '{name}' seed namespace"),
        })
}

pub(super) fn default_candidate(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    ordinal: u64,
    ancestor: u64,
    position: WorldPosition,
) -> Result<GraphCandidate> {
    Ok(GraphCandidate {
        identity: CandidateIdentity {
            node: node.definition.guid,
            node_address: node_execution_address(node, state),
            node_semantic_revision: node.definition.semantic_revision,
            ordinal,
            ancestor,
        },
        owner: canonical_owner(position, node.definition.spatial.level())?,
        source_layer: state.inputs.biome_instance,
        position,
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_bits(65_536); 3],
        family: None,
        variation: 0,
        parent: None,
        colony: None,
        priority: DecisionScalar::from_bits(0),
        ecology_tick: state.inputs.ecology_tick,
        crown_radius: DecisionScalar::from_bits(65_536),
        root_radius: DecisionScalar::from_bits(65_536),
        attachment: None,
        surface_normal: None,
        surface_projection: [DecisionScalar::from_bits(0); 3],
        authored_point: None,
    })
}

pub(super) fn point_radius(point: &PlantPoint) -> Result<DecisionScalar> {
    let position = point.position.global_ticks();
    let minimum = point.bounds.min_ticks();
    let maximum = point.bounds.max_ticks_exclusive();
    let mut radius = 1_i128;
    for axis in 0..3 {
        radius = radius.max(
            position[axis]
                .checked_sub(minimum[axis])
                .ok_or(Error::NumericOverflow)?,
        );
        radius = radius.max(
            maximum[axis]
                .checked_sub(1)
                .and_then(|value| value.checked_sub(position[axis]))
                .ok_or(Error::NumericOverflow)?,
        );
    }
    Ok(DecisionScalar::from_bits(ticks_to_fixed_meters(radius)?))
}

pub(super) fn stable_ordinal(parts: &[&[u8]]) -> Result<u64> {
    let mut hasher = VegetationContentHasher::new();
    hasher.update(b"saffron-anima/vegetation-candidate/v1\0")?;
    for part in parts {
        hasher.update(
            &u64::try_from(part.len())
                .map_err(|_| Error::NumericOverflow)?
                .to_be_bytes(),
        )?;
        hasher.update(part)?;
    }
    let digest = hasher.finalize()?;
    Ok(u64::from_be_bytes(digest[..8].try_into().unwrap()))
}

pub(super) fn candidate_ordinal(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    source: u128,
    local: u64,
    channel: u32,
) -> Result<u64> {
    stable_ordinal(&[
        &node_execution_address(node, state).to_be_bytes(),
        &node.definition.semantic_revision.to_be_bytes(),
        &source.to_be_bytes(),
        &local.to_be_bytes(),
        &channel.to_be_bytes(),
    ])
}

pub(super) fn candidate_lineage(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
) -> CandidateLineage {
    CandidateLineage(node_execution_address(node, state))
}

pub(super) fn ensure_lineage(
    node: &CompiledGraphNode,
    input: &str,
    expected: CandidateLineage,
    actual: CandidateLineage,
) -> Result<()> {
    if expected == actual {
        return Ok(());
    }
    Err(Error::GraphDocument {
        path: format!("{}.inputs.{input}", node.debug_symbol.label),
        reason: format!(
            "candidate lineage mismatch: expected {:032x}, found {:032x}",
            expected.0, actual.0
        ),
    })
}

pub(super) fn node_execution_address(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
) -> u128 {
    node.address().execution_identity(state.graph.biome)
}
