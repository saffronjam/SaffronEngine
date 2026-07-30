//! Symbolic input bounds feeding preflight traversal.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, WorldCellKey, world_cells_covering_bounds,
};

use crate::graph::{CompiledDemandSlice, CompiledDemandUnitSlice};
use crate::memory::{
    checked_memory_sum, requested_btree_bytes_for_len, requested_btree_with,
    requested_vec_bytes_for_len,
};
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphNode, Error, GraphAuthority, GraphDomain,
    GraphNodeAddress, GraphParameterValue, ProvenanceDecisionHandle, ProvenanceHandle,
    QualifiedGraphPin, Result,
};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SymbolicValueBound {
    pub(super) domain: Option<GraphDomain>,
    pub(super) items: u64,
    pub(super) bytes: u64,
    pub(super) diagnostic_candidates: u64,
    pub(super) diagnostic_fields: u64,
    pub(super) diagnostic_rejected: u64,
    pub(super) diagnostic_module_path_items: u64,
    pub(super) diagnostic_label_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SymbolicEvaluationBound {
    pub(super) candidate_peak: u64,
    pub(super) candidate_events: u64,
    pub(super) accepted: u64,
    pub(super) micro_samples: u64,
    pub(super) memory_bytes: u64,
    pub(super) transfer_bytes: u64,
    pub(super) rejected: u64,
    pub(super) provenance_bytes: u64,
    pub(super) imported_provenance_records: u64,
    pub(super) diagnostic_metadata_bytes: u64,
    pub(super) generated_input_tiles: u64,
    pub(super) generated_input_bytes: u64,
    pub(super) published_input_tiles: u64,
    pub(super) published_input_bytes: u64,
    pub(super) candidate_decision_scratch_bytes: u64,
}

#[derive(Default)]
pub(super) struct SymbolicGlobalTile {
    pub(super) outputs: BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    pub(super) provenance_decisions: u64,
    pub(super) provenance_records: u64,
    pub(super) provenance_bytes: u64,
}

impl SymbolicGlobalTile {
    pub(super) fn requested_memory_bytes(&self) -> Result<u64> {
        requested_btree_with(&self.outputs, qualified_graph_pin_memory, |_| Ok(0))
    }
}

#[derive(Default)]
pub(super) struct SymbolicGlobalStore {
    pub(super) tiles: BTreeMap<([u8; 32], WorldCellKey), SymbolicGlobalTile>,
}

impl SymbolicGlobalStore {
    pub(super) fn requested_memory_bytes(&self) -> Result<u64> {
        requested_btree_with(
            &self.tiles,
            |_| Ok(0),
            SymbolicGlobalTile::requested_memory_bytes,
        )
    }
}

#[derive(Clone, Copy)]
pub(super) enum SymbolicEvaluationScope<'a> {
    Cell {
        global_store: &'a SymbolicGlobalStore,
    },
    Global {
        stage: &'a CompiledGlobalStage,
        global_store: &'a SymbolicGlobalStore,
    },
}

impl<'a> SymbolicEvaluationScope<'a> {
    pub(super) fn current_global_stage(self) -> Option<&'a CompiledGlobalStage> {
        match self {
            Self::Cell { .. } => None,
            Self::Global { stage, .. } => Some(stage),
        }
    }

    pub(super) fn global_store(self) -> &'a SymbolicGlobalStore {
        match self {
            Self::Cell { global_store } | Self::Global { global_store, .. } => global_store,
        }
    }

    pub(super) const fn is_cell(self) -> bool {
        matches!(self, Self::Cell { .. })
    }
}

pub(super) fn compiled_demand_slice<'a>(
    graph: &'a CompiledBiomeGraph,
    stage: Option<&CompiledGlobalStage>,
) -> Result<&'a CompiledDemandSlice> {
    match stage {
        Some(stage) => {
            graph
                .demand_plan()
                .stage_slice(stage.id)
                .ok_or_else(|| Error::GraphDocument {
                    path: "graph.demandPlan".to_owned(),
                    reason: format!("global stage {} has no demand slice", hex_hash(stage.id)),
                })
        }
        None => Ok(graph.demand_plan().public_slice()),
    }
}

pub(super) fn root_demand_unit(demand: &CompiledDemandSlice) -> Result<&CompiledDemandUnitSlice> {
    demand.unit(&[]).ok_or_else(|| Error::GraphDocument {
        path: "graph.demandPlan".to_owned(),
        reason: "root unit has no demand slice".to_owned(),
    })
}

pub(super) fn find_child_demand_unit<'a>(
    demand: &'a CompiledDemandSlice,
    node: &CompiledGraphNode,
) -> Option<&'a CompiledDemandUnitSlice> {
    let GraphParameterValue::Guid(call_guid) = node.definition.parameter("callGuid")? else {
        return None;
    };
    let parent = node.debug_symbol.module_path.as_slice();
    demand
        .units
        .iter()
        .find(|(path, _)| {
            path.len() == parent.len() + 1
                && path[..parent.len()] == *parent
                && path[parent.len()] == *call_guid
        })
        .map(|(_, unit)| unit)
}

pub(super) fn child_demand_unit<'a>(
    demand: &'a CompiledDemandSlice,
    node: &CompiledGraphNode,
) -> Result<&'a CompiledDemandUnitSlice> {
    find_child_demand_unit(demand, node).ok_or_else(|| Error::GraphDocument {
        path: "graph.demandPlan".to_owned(),
        reason: format!(
            "module call '{}' has no demand slice",
            node.debug_symbol.label
        ),
    })
}

pub(super) struct SymbolicPlannedEvaluation {
    pub(super) bound: SymbolicEvaluationBound,
    pub(super) materialized_outputs: BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    pub(super) public_result_bytes: u64,
    pub(super) global_tile_bytes: u64,
    pub(super) ancestor_references: u64,
}

#[derive(Clone, Copy)]
pub(super) struct SymbolicTraversalContext<'a> {
    pub(super) graph: &'a CompiledBiomeGraph,
    pub(super) demand: &'a CompiledDemandSlice,
    pub(super) scope: SymbolicEvaluationScope<'a>,
    pub(super) inputs: &'a GraphEvaluationInputs,
    pub(super) guard: PreflightGuard<'a>,
}

pub(super) fn symbolic_should_load_global_node(
    graph: &CompiledBiomeGraph,
    scope: SymbolicEvaluationScope<'_>,
    address: &GraphNodeAddress,
) -> bool {
    let Some(owner) = graph.spatial_plan().global_stage_for_node(address) else {
        return false;
    };
    scope
        .current_global_stage()
        .is_none_or(|current| current.id != owner.id)
}

fn symbolic_merge_value(
    current: Option<SymbolicValueBound>,
    incoming: SymbolicValueBound,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let Some(current) = current else {
        return Ok(incoming);
    };
    if current.domain != incoming.domain {
        return Err(Error::GraphDocument {
            path: "graph.symbolicBound.global".to_owned(),
            reason: "global boundary values have different domains".to_owned(),
        });
    }
    let item_limit = match current.domain {
        Some(GraphDomain::Candidates) => limits.max_candidates,
        Some(GraphDomain::MacroPoints) => limits.max_macro_points,
        Some(GraphDomain::MicroField) => limits.max_micro_samples,
        _ => u64::MAX,
    };
    Ok(SymbolicValueBound {
        domain: current.domain,
        items: bound_add(
            match current.domain {
                Some(GraphDomain::Candidates) => "candidate count",
                Some(GraphDomain::MacroPoints) => "accepted count",
                Some(GraphDomain::MicroField) => "micro samples",
                _ => "symbolic items",
            },
            current.items,
            incoming.items,
            item_limit,
        )?,
        bytes: bound_add(
            "memory bytes",
            current.bytes,
            incoming.bytes,
            limits.max_memory_bytes,
        )?,
        diagnostic_candidates: current
            .diagnostic_candidates
            .checked_add(incoming.diagnostic_candidates)
            .ok_or(Error::NumericOverflow)?,
        diagnostic_fields: current
            .diagnostic_fields
            .checked_add(incoming.diagnostic_fields)
            .ok_or(Error::NumericOverflow)?,
        diagnostic_rejected: current
            .diagnostic_rejected
            .checked_add(incoming.diagnostic_rejected)
            .ok_or(Error::NumericOverflow)?,
        diagnostic_module_path_items: current
            .diagnostic_module_path_items
            .checked_add(incoming.diagnostic_module_path_items)
            .ok_or(Error::NumericOverflow)?,
        diagnostic_label_bytes: current
            .diagnostic_label_bytes
            .checked_add(incoming.diagnostic_label_bytes)
            .ok_or(Error::NumericOverflow)?,
    })
}

fn symbolic_import_global_value(
    mut value: SymbolicValueBound,
    scope: SymbolicEvaluationScope<'_>,
) -> SymbolicValueBound {
    if (scope.is_cell() && value.domain == Some(GraphDomain::MacroPoints))
        || value.domain == Some(GraphDomain::Diagnostics)
    {
        value.items = 0;
        value.bytes = 0;
        value.diagnostic_candidates = 0;
        value.diagnostic_fields = 0;
        value.diagnostic_rejected = 0;
        value.diagnostic_module_path_items = 0;
        value.diagnostic_label_bytes = 0;
    }
    value
}

pub(super) fn global_import_scratch_bytes(
    candidate_identities: u64,
    provenance_decisions: u64,
    provenance_records: u64,
    provenance_bytes: u64,
) -> Result<u64> {
    let handle_bytes = u64::try_from(std::mem::size_of::<ProvenanceDecisionHandle>())
        .map_err(|_| Error::NumericOverflow)?;
    let traversal_handles = provenance_decisions
        .checked_add(provenance_bytes / handle_bytes.max(1))
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        requested_btree_bytes_for_len::<CandidateIdentity, ()>(candidate_identities)?,
        requested_vec_bytes_for_len::<ProvenanceDecisionHandle>(candidate_identities)?,
        requested_btree_bytes_for_len::<ProvenanceDecisionHandle, ()>(candidate_identities)?,
        requested_btree_bytes_for_len::<ProvenanceDecisionHandle, ()>(provenance_decisions)?,
        requested_vec_bytes_for_len::<ProvenanceDecisionHandle>(traversal_handles)?,
        requested_btree_bytes_for_len::<ProvenanceDecisionHandle, usize>(provenance_decisions)?,
        requested_vec_bytes_for_len::<(ProvenanceDecisionHandle, ProvenanceDecisionHandle)>(
            traversal_handles,
        )?,
        requested_btree_bytes_for_len::<ProvenanceDecisionHandle, ()>(provenance_decisions)?,
        requested_vec_bytes_for_len::<ProvenanceDecisionHandle>(provenance_decisions)?,
        requested_btree_bytes_for_len::<ProvenanceDecisionHandle, ProvenanceDecisionHandle>(
            provenance_decisions,
        )?,
        requested_vec_bytes_for_len::<ProvenanceDecisionHandle>(provenance_records)?,
        requested_btree_bytes_for_len::<ProvenanceHandle, ()>(provenance_records)?,
        requested_btree_bytes_for_len::<ProvenanceHandle, ProvenanceHandle>(provenance_records)?,
        requested_vec_bytes_for_len::<ProvenanceHandle>(provenance_records)?,
        provenance_bytes,
    ])
}

fn symbolic_import_global_provenance(
    bound: &mut SymbolicEvaluationBound,
    tile: &SymbolicGlobalTile,
    candidate_identities: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<()> {
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        global_import_scratch_bytes(
            candidate_identities,
            tile.provenance_decisions,
            tile.provenance_records,
            tile.provenance_bytes,
        )?,
        limits.max_memory_bytes,
    )?;
    bound.candidate_events = bound_add(
        "diagnostic samples",
        bound.candidate_events,
        tile.provenance_decisions,
        u64::MAX,
    )?;
    bound.imported_provenance_records = bound_add(
        "diagnostic samples",
        bound.imported_provenance_records,
        tile.provenance_records,
        u64::MAX,
    )?;
    bound.provenance_bytes = bound_add(
        "memory bytes",
        bound.provenance_bytes,
        tile.provenance_bytes,
        limits.max_memory_bytes,
    )?;
    Ok(())
}

pub(super) fn symbolic_load_global_node_outputs(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
    scope: SymbolicEvaluationScope<'_>,
    inputs: &GraphEvaluationInputs,
    node: &CompiledGraphNode,
    bound: &mut SymbolicEvaluationBound,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    let address = node.address();
    let stage = graph
        .spatial_plan()
        .global_stage_for_node(&address)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "global node is absent from the symbolic spatial plan".to_owned(),
        })?;
    let owners = world_cells_covering_bounds(
        inputs.read_bounds,
        stage.owner_level,
        graph.limits.max_global_stage_tiles,
    )?;
    let mut outputs = BTreeMap::new();
    for output in node.outputs.iter().filter(|output| {
        NodeOutputDemand::new(demand, node).contains(&output.name)
            && stage.output_pins.contains(&QualifiedGraphPin {
                node: address.clone(),
                pin: output.name.clone(),
            })
    }) {
        let pin = QualifiedGraphPin {
            node: address.clone(),
            pin: output.name.clone(),
        };
        let mut merged = None;
        for owner in &owners {
            let tile = scope
                .global_store()
                .tiles
                .get(&(stage.id, *owner))
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: format!(
                        "symbolic global stage {} owner {}",
                        hex_hash(stage.id),
                        owner
                    ),
                })?;
            let value = tile
                .outputs
                .get(&pin)
                .copied()
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!(
                        "symbolic global stage omitted boundary pin '{}'",
                        output.name
                    ),
                })?;
            symbolic_import_global_provenance(bound, tile, value.items, graph.limits)?;
            let imported = symbolic_import_global_value(value, scope);
            if let Some(current) = merged {
                bound.memory_bytes = bound_add(
                    "memory bytes",
                    bound.memory_bytes,
                    symbolic_global_merge_scratch_bytes(current, imported)?,
                    graph.limits.max_memory_bytes,
                )?;
            }
            merged = Some(symbolic_merge_value(merged, imported, graph.limits)?);
        }
        outputs.insert(
            output.name.clone(),
            merged.ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!(
                    "symbolic global stage {} output '{}'",
                    hex_hash(stage.id),
                    output.name
                ),
            })?,
        );
    }
    Ok(outputs)
}

pub(super) fn symbolic_vec_value<T>(
    domain: GraphDomain,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let bytes = requested_vec_bytes_for_len::<T>(items)?;
    check_limit("memory bytes", bytes, limits.max_memory_bytes)?;
    Ok(SymbolicValueBound {
        domain: Some(domain),
        items,
        bytes,
        ..SymbolicValueBound::default()
    })
}

pub(super) fn symbolic_candidate_value(
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    check_limit("candidate count", items, limits.max_candidates)?;
    symbolic_vec_value::<GraphCandidate>(GraphDomain::Candidates, items, limits)
}

pub(super) fn symbolic_field_value(
    domain: GraphDomain,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let bytes = match domain {
        GraphDomain::ScalarField => {
            requested_btree_bytes_for_len::<CandidateIdentity, DecisionScalar>(items)?
        }
        GraphDomain::VectorField => {
            requested_btree_bytes_for_len::<CandidateIdentity, DecisionVec3>(items)?
        }
        GraphDomain::HessianField => {
            requested_btree_bytes_for_len::<CandidateIdentity, DecisionHessian3>(items)?
        }
        GraphDomain::SurfaceField => {
            requested_btree_bytes_for_len::<CandidateIdentity, ProjectedSurfaceSample>(items)?
        }
        _ => {
            return Err(Error::GraphDocument {
                path: "graph.symbolicBound".to_owned(),
                reason: "field bound has a non-field domain".to_owned(),
            });
        }
    };
    check_limit("memory bytes", bytes, limits.max_memory_bytes)?;
    Ok(SymbolicValueBound {
        domain: Some(domain),
        items,
        bytes,
        ..SymbolicValueBound::default()
    })
}

pub(super) fn symbolic_surface_tags_per_hit(
    node: &CompiledGraphNode,
    inputs: &GraphEvaluationInputs,
) -> Result<u64> {
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let provider_filter = u64_parameter(node, "provider", 0)?;
    let provider_tags = inputs
        .surface_providers
        .iter()
        .map(|provider| provider.descriptor())
        .filter(|descriptor| {
            descriptor.capabilities.project
                && (!authoritative || descriptor.capabilities.authoritative_attachments)
                && (provider_filter == 0 || descriptor.id.0 == provider_filter)
        })
        .map(|descriptor| u64::from(descriptor.max_tags_per_hit))
        .max()
        .unwrap_or(0);
    let tile_tags = inputs
        .surface_projection_tiles
        .iter()
        .filter(|tile| {
            tile.node == node.definition.guid
                && tile.node_semantic_revision == node.definition.semantic_revision
        })
        .flat_map(|tile| tile.samples.iter())
        .filter_map(|entry| entry.sample.as_ref())
        .map(|sample| sample.tags.len() as u64)
        .max()
        .unwrap_or(0);
    Ok(provider_tags.max(tile_tags))
}

pub(super) fn symbolic_add_generated_input(
    bound: &mut SymbolicEvaluationBound,
    bytes: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<()> {
    bound.generated_input_tiles = bound_add(
        "input tiles",
        bound.generated_input_tiles,
        1,
        limits.max_input_tiles,
    )?;
    bound.generated_input_bytes = bound_add(
        "memory bytes",
        bound.generated_input_bytes,
        bytes,
        limits.max_memory_bytes,
    )?;
    Ok(())
}

pub(super) fn symbolic_add_published_input(
    bound: &mut SymbolicEvaluationBound,
    bytes: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<()> {
    bound.published_input_tiles = bound_add(
        "input tiles",
        bound.published_input_tiles,
        1,
        limits.max_input_tiles,
    )?;
    bound.published_input_bytes = bound_add(
        "memory bytes",
        bound.published_input_bytes,
        bytes,
        limits.max_memory_bytes,
    )?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        bytes,
        limits.max_memory_bytes,
    )?;
    Ok(())
}

pub(super) fn symbolic_input<'a>(
    node: &CompiledGraphNode,
    incoming: &BTreeMap<u128, Vec<&crate::GraphEdge>>,
    values: &'a BTreeMap<(u128, String), SymbolicValueBound>,
    pin: &str,
) -> Result<Option<&'a SymbolicValueBound>> {
    let Some(edge) = incoming
        .get(&node.definition.guid)
        .into_iter()
        .flatten()
        .find(|edge| edge.to_pin == pin)
    else {
        return Ok(None);
    };
    values
        .get(&(edge.from_node, edge.from_pin.clone()))
        .map(Some)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!("symbolic input '{pin}' is missing"),
        })
}

pub(super) fn required_symbolic_items(
    node: &CompiledGraphNode,
    incoming: &BTreeMap<u128, Vec<&crate::GraphEdge>>,
    values: &BTreeMap<(u128, String), SymbolicValueBound>,
    pin: &str,
) -> Result<u64> {
    symbolic_input(node, incoming, values, pin)?.map_or_else(
        || {
            Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: format!("required symbolic input '{pin}' is missing"),
            })
        },
        |value| Ok(value.items),
    )
}

pub(super) fn symbolic_stage_region_count(
    node: &CompiledGraphNode,
    inputs: &GraphEvaluationInputs,
    scope: SymbolicEvaluationScope<'_>,
    bound: &mut SymbolicEvaluationBound,
    limits: crate::GraphSafetyLimits,
) -> Result<u64> {
    let level = node.definition.spatial.level();
    let minimum_input_level = scope
        .current_global_stage()
        .map_or(inputs.output_cell.level(), |stage| {
            stage.minimum_input_level
        });
    if level < minimum_input_level {
        return Ok(0);
    }
    if inputs.regions.is_empty() {
        return u64::try_from(cell_region_count(inputs.read_bounds, level)?)
            .map_err(|_| Error::NumericOverflow);
    }
    let region_upper = inputs
        .regions
        .iter()
        .filter(|region| region.kind == EvaluationRegionKind::Biome)
        .count() as u64;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        requested_btree_bytes_for_len::<(u8, u128, WorldCellKey), ()>(region_upper)?,
        limits.max_memory_bytes,
    )?;
    let mut canonical = BTreeSet::new();
    for region in inputs
        .regions
        .iter()
        .filter(|region| region.kind == EvaluationRegionKind::Biome)
    {
        if region.seed_cell.level() > level {
            return Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "region seed cell is coarser than the node stage".to_owned(),
            });
        }
        let stage_cell = region.seed_cell.ancestor(level)?;
        if intersect_bounds(region.bounds, stage_cell.bounds())?.is_none() {
            continue;
        }
        canonical.insert(if let Some(namespace) = region.hierarchy_namespace {
            (0_u8, namespace, stage_cell)
        } else {
            (1_u8, region.id, stage_cell)
        });
    }
    u64::try_from(canonical.len()).map_err(|_| Error::NumericOverflow)
}

pub(super) fn symbolic_spline_candidate_count(
    node: &CompiledGraphNode,
    inputs: &GraphEvaluationInputs,
    limits: crate::GraphSafetyLimits,
    bound: &mut SymbolicEvaluationBound,
) -> Result<u64> {
    let spacing = fixed_parameter(node, "spacing", DecisionScalar::from_bits(0))?;
    let spacing_ticks = fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128;
    if spacing_ticks == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline spacing must be positive".to_owned(),
        });
    }
    let maximum_points = inputs
        .splines
        .iter()
        .map(|spline| spline.points.len() as u64)
        .max()
        .unwrap_or(0);
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        checked_memory_sum([
            requested_vec_bytes_for_len::<[i128; 3]>(maximum_points)?,
            requested_vec_bytes_for_len::<SplineSegment>(maximum_points)?,
        ])?,
        limits.max_memory_bytes,
    )?;
    inputs.splines.iter().try_fold(0_u64, |total, spline| {
        let segments = spline_segments(&spline.points)?;
        if segments.is_empty() {
            return Ok(total);
        }
        let length = segments.iter().try_fold(0_i128, |sum, segment| {
            sum.checked_add(segment.length)
                .ok_or(Error::NumericOverflow)
        })?;
        let samples = u64::try_from(length / spacing_ticks)
            .map_err(|_| Error::NumericOverflow)?
            .checked_add(1)
            .ok_or(Error::NumericOverflow)?;
        bound_add("candidate count", total, samples, limits.max_candidates)
    })
}

pub(super) fn symbolic_provenance_bytes_per_decision(node: &CompiledGraphNode) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<ProvenanceDecisionHandle>(3)?,
        requested_vec_bytes_for_len::<u128>(node.debug_symbol.module_path.len() as u64)?,
    ])
}

pub(super) fn symbolic_add_rejections(
    bound: &mut SymbolicEvaluationBound,
    node: &CompiledGraphNode,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<()> {
    bound.rejected = bound_add("diagnostic samples", bound.rejected, items, u64::MAX)?;
    let provenance = bound_mul(
        "memory bytes",
        items,
        symbolic_provenance_bytes_per_decision(node)?,
        limits.max_memory_bytes,
    )?;
    bound.provenance_bytes = bound_add(
        "memory bytes",
        bound.provenance_bytes,
        provenance,
        limits.max_memory_bytes,
    )?;
    Ok(())
}
