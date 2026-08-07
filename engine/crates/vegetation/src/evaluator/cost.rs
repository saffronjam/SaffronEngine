//! Requested-byte accounting for evaluator scratch, caches, and results.

use super::*;

use std::collections::BTreeMap;
use std::time::Instant;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, WeightedSurfaceTag, WorldBounds, WorldCellKey, WorldPosition,
};

use crate::graph::CompiledDemandSlice;
use crate::memory::{
    ALLOCATION_OVERHEAD_BYTES, checked_memory_sum, requested_btree_bytes,
    requested_btree_bytes_for_len, requested_btree_with, requested_string_bytes,
    requested_vec_bytes, requested_vec_bytes_for_len, requested_vec_with,
};
use crate::{
    CompiledBiomeGraph, CompiledGraphNode, CompiledGraphUnit, Error, GraphExecutionBoundary,
    GraphExecutionGroup, GraphExecutionNode, GraphGpuInstruction, GraphGpuRegister,
    GraphGpuRegisterType, GraphGpuValue, GraphNodeAddress, PlantId, PlantPoint, QualifiedGraphPin,
    Result,
};

fn graph_node_address_memory(address: &GraphNodeAddress) -> Result<u64> {
    requested_vec_bytes::<u128>(address.module_path.capacity())
}

pub(super) fn qualified_graph_pin_memory(pin: &QualifiedGraphPin) -> Result<u64> {
    checked_memory_sum([
        graph_node_address_memory(&pin.node)?,
        requested_string_bytes(&pin.pin)?,
    ])
}

pub(super) fn micro_field_tile_memory(tile: &MicroFieldTile) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes::<u16>(tile.density.capacity())?,
        requested_btree_with(
            &tile.attributes,
            |_| Ok(0),
            |values| requested_vec_bytes::<i32>(values.capacity()),
        )?,
    ])
}

pub(super) fn projected_surface_sample_memory(sample: &ProjectedSurfaceSample) -> Result<u64> {
    requested_vec_bytes::<WeightedSurfaceTag>(sample.tags.capacity())
}

fn projection_tile_memory(tile: &QuantizedSurfaceProjectionTile) -> Result<u64> {
    requested_vec_with(&tile.samples, |entry| {
        entry.sample.as_ref().map_or(Ok(0), |sample| {
            requested_vec_bytes::<WeightedSurfaceTag>(sample.tags.capacity())
        })
    })
}

fn field_query_tile_memory(tile: &QuantizedSurfaceFieldQueryTile) -> Result<u64> {
    requested_vec_bytes::<QuantizedSurfaceFieldQueryEntry>(tile.samples.capacity())
}

pub(super) fn diagnostic_stream_memory(stream: &NamedDiagnosticStream) -> Result<u64> {
    checked_memory_sum([
        graph_node_address_memory(&stream.node)?,
        requested_string_bytes(&stream.label)?,
        stream.candidates.as_ref().map_or(Ok(0), |values| {
            requested_vec_bytes::<DiagnosticCandidateSample>(values.capacity())
        })?,
        stream.field.as_ref().map_or(Ok(0), |values| {
            requested_vec_bytes::<DiagnosticScalarSample>(values.capacity())
        })?,
        requested_vec_bytes::<RejectedCandidate>(stream.rejected.capacity())?,
    ])
}

fn graph_diagnostics_memory(diagnostics: &GraphEvaluationDiagnostics) -> Result<u64> {
    checked_memory_sum([
        requested_vec_with(&diagnostics.nodes, |node| {
            checked_memory_sum([
                requested_vec_bytes::<u128>(node.module_path.capacity())?,
                requested_string_bytes(&node.symbol)?,
            ])
        })?,
        requested_vec_with(&diagnostics.gpu_groups, |group| {
            requested_vec_with(&group.nodes, graph_node_address_memory)
        })?,
        requested_vec_bytes::<RejectedCandidate>(diagnostics.rejected.capacity())?,
        requested_vec_with(&diagnostics.streams, diagnostic_stream_memory)?,
    ])
}

pub(super) fn graph_result_memory(result: &GraphEvaluationResult) -> Result<u64> {
    checked_memory_sum([
        result.macro_points.requested_memory_bytes()?,
        requested_vec_with(&result.micro_fields, micro_field_tile_memory)?,
        requested_vec_with(&result.surface_projection_tiles, projection_tile_memory)?,
        requested_vec_with(&result.surface_field_query_tiles, field_query_tile_memory)?,
        requested_vec_bytes::<WorldCellKey>(result.ancestor_references.capacity())?,
        result.provenance.requested_memory_bytes()?,
        graph_diagnostics_memory(&result.diagnostics)?,
    ])
}

#[derive(Clone, Copy)]
pub(super) struct PreflightGuard<'a> {
    pub(super) cancellation: &'a GraphCancellationToken,
    pub(super) deadline: Instant,
    pub(super) time_limit_ms: u64,
}

impl PreflightGuard<'_> {
    pub(super) fn check(self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: self.time_limit_ms.saturating_add(1),
                limit: self.time_limit_ms,
            });
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct SymbolicInputPreflight {
    pub(super) input_tiles: u64,
    pub(super) retained_input_bytes: u64,
    pub(super) generated_input_bytes: u64,
    pub(super) candidate_count: u64,
    pub(super) accepted_count: u64,
    pub(super) micro_samples: u64,
    pub(super) transfer_bytes: u64,
    pub(super) active_worker_memory: u64,
    pub(super) retained_result_bytes: u64,
}

/// Bytes for `items` values of `T` spread over `allocations` separate allocations.
pub(super) fn disjoint_vec_bytes<T>(items: u64, allocations: u64) -> Result<u64> {
    items
        .checked_mul(size_of::<T>() as u64)
        .and_then(|bytes| {
            allocations
                .checked_mul(ALLOCATION_OVERHEAD_BYTES)
                .and_then(|overhead| bytes.checked_add(overhead))
        })
        .ok_or(Error::NumericOverflow)
}

pub(super) fn weighted_elimination_scratch_bytes(
    candidates: u64,
    target: u64,
    maximum_neighbours: u64,
) -> Result<u64> {
    let adjacency_row = requested_vec_bytes_for_len::<(usize, u64)>(maximum_neighbours)?;
    checked_memory_sum([
        requested_vec_bytes_for_len::<u32>(candidates)?,
        requested_vec_bytes_for_len::<((i128, i128), usize)>(candidates)?,
        requested_vec_bytes_for_len::<Vec<(usize, u64)>>(candidates)?,
        candidates
            .checked_mul(adjacency_row)
            .ok_or(Error::NumericOverflow)?,
        requested_vec_bytes_for_len::<u64>(candidates)?,
        requested_vec_bytes_for_len::<bool>(candidates)?,
        requested_vec_bytes_for_len::<EliminationScore>(candidates)?,
        requested_vec_bytes_for_len::<usize>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(target.min(candidates))?,
    ])
}

pub(super) fn micro_output_scratch_bytes(samples: u64, channels: u64) -> Result<u64> {
    let sum_values = requested_vec_bytes_for_len::<i128>(samples)?;
    let output_values = requested_vec_bytes_for_len::<i32>(samples)?;
    checked_memory_sum([
        requested_vec_bytes_for_len::<u64>(samples)?,
        requested_vec_bytes_for_len::<(u128, &ScalarFieldSamples)>(channels)?,
        requested_btree_bytes_for_len::<u128, Vec<i128>>(channels)?,
        channels
            .checked_mul(sum_values)
            .ok_or(Error::NumericOverflow)?,
        requested_btree_bytes_for_len::<u128, Vec<i32>>(channels)?,
        channels
            .checked_mul(output_values)
            .ok_or(Error::NumericOverflow)?,
    ])
}

pub(super) fn blue_noise_scratch_bytes(candidates: u64) -> Result<u64> {
    requested_vec_bytes_for_len::<GraphCandidate>(
        candidates.checked_mul(4).ok_or(Error::NumericOverflow)?,
    )
}

pub(super) fn stage_region_scratch_bytes(regions: u64) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bytes_for_len::<(u8, u128, WorldCellKey), EvaluationRegion>(regions)?,
        requested_vec_bytes_for_len::<EvaluationRegion>(regions)?,
    ])
}

pub(super) fn projection_preparation_cache_bytes(items: u64, tags_per_item: u64) -> Result<u64> {
    let tag_bytes = requested_vec_bytes_for_len::<WeightedSurfaceTag>(tags_per_item)?;
    checked_memory_sum([
        requested_btree_bytes_for_len::<
            SurfaceProjectionCacheKey,
            BTreeMap<WorldPosition, Option<QuantizedSurfaceProjectionSample>>,
        >(1)?,
        requested_btree_bytes_for_len::<WorldPosition, Option<QuantizedSurfaceProjectionSample>>(
            items,
        )?,
        items.checked_mul(tag_bytes).ok_or(Error::NumericOverflow)?,
    ])
}

pub(super) fn field_preparation_cache_bytes(items: u64) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bytes_for_len::<
            SurfaceFieldQueryCacheKey,
            BTreeMap<(CandidateIdentity, WorldPosition), QuantizedSurfaceFieldValue>,
        >(1)?,
        requested_btree_bytes_for_len::<
            (CandidateIdentity, WorldPosition),
            QuantizedSurfaceFieldValue,
        >(items)?,
    ])
}

pub(super) fn xz_filter_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(CandidateIdentity, i128)>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(
            candidates.checked_mul(2).ok_or(Error::NumericOverflow)?,
        )?,
        requested_btree_bytes_for_len::<(i128, i128), usize>(candidates)?,
        requested_vec_bytes_for_len::<Option<usize>>(candidates)?,
    ])
}

pub(super) fn competition_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(CandidateIdentity, i128)>(candidates)?,
        requested_vec_bytes_for_len::<((i128, i128), usize)>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

pub(super) fn bounds_overlap_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(GraphCandidate, WorldBounds, i128)>(
            candidates.checked_mul(2).ok_or(Error::NumericOverflow)?,
        )?,
        requested_btree_bytes_for_len::<(i128, i128, i128), usize>(candidates)?,
        requested_vec_bytes_for_len::<Option<usize>>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

pub(super) fn community_blend_scratch_bytes(candidates: u64, palette_entries: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<crate::BiomePaletteEntry>(palette_entries)?,
        requested_vec_bytes_for_len::<(u64, u64)>(palette_entries)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

pub(super) fn companion_scratch_bytes(
    input_candidates: u64,
    output_candidates: u64,
    rules: u64,
) -> Result<u64> {
    let maximum_generation = output_candidates
        .checked_sub(input_candidates)
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        requested_vec_bytes_for_len::<&crate::CompanionRule>(rules)?,
        requested_vec_bytes_for_len::<GraphCandidate>(
            maximum_generation
                .checked_mul(2)
                .ok_or(Error::NumericOverflow)?,
        )?,
        requested_vec_bytes_for_len::<GraphCandidate>(output_candidates)?,
    ])
}

pub(super) fn macro_output_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(&GraphCandidate, Uuid, PlantId)>(candidates)?,
        requested_vec_bytes_for_len::<PlantPoint>(candidates)?,
    ])
}

#[derive(Clone, Copy, Default)]
pub(super) struct ResidentGroupAllocationShape {
    pub(super) invocations: u64,
    pub(super) inputs: u64,
    pub(super) instructions: u64,
    pub(super) curve_instructions: u64,
    pub(super) curve_points: u64,
    pub(super) external_inputs: u64,
    pub(super) external_pin_bytes: u64,
    pub(super) output_entries: u64,
    pub(super) output_pin_bytes: u64,
    pub(super) noise_nodes: u64,
    pub(super) gradient_nodes: u64,
    pub(super) candidate_output: bool,
    pub(super) scalar_output: bool,
}

pub(super) fn resident_group_scratch_bytes(shape: ResidentGroupAllocationShape) -> Result<u64> {
    let registers = shape
        .inputs
        .checked_add(shape.instructions)
        .ok_or(Error::NumericOverflow)?;
    let invocation_values = shape
        .invocations
        .checked_mul(shape.inputs)
        .ok_or(Error::NumericOverflow)?;
    let curve_allocations = shape
        .curve_instructions
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let external_key_allocations = shape
        .external_inputs
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let output_key_allocations = shape
        .output_entries
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        requested_vec_bytes_for_len::<GraphGpuRegisterType>(shape.inputs)?,
        requested_vec_bytes_for_len::<GraphGpuInstruction>(shape.instructions)?,
        requested_vec_bytes_for_len::<GraphGpuRegisterType>(registers)?,
        requested_vec_bytes_for_len::<(u16, i32)>(shape.curve_points)?,
        curve_allocations,
        requested_vec_bytes_for_len::<GraphGpuValue>(invocation_values)?,
        requested_vec_bytes_for_len::<CandidateIdentity>(shape.invocations)?,
        requested_vec_bytes_for_len::<bool>(shape.invocations)?,
        requested_vec_bytes_for_len::<crate::GraphGpuOutput>(shape.invocations)?,
        requested_vec_bytes_for_len::<&CompiledGraphNode>(shape.instructions)?,
        requested_vec_bytes_for_len::<ResidentInputBinding>(shape.inputs)?,
        requested_btree_bytes_for_len::<(u128, String), ()>(shape.external_inputs)?,
        requested_vec_bytes_for_len::<u8>(shape.external_pin_bytes)?,
        external_key_allocations,
        requested_btree_bytes_for_len::<(u128, String), GraphValue>(shape.output_entries)?,
        requested_vec_bytes_for_len::<u8>(shape.output_pin_bytes)?,
        output_key_allocations,
        requested_btree_bytes_for_len::<(u128, String), GraphGpuRegister>(shape.external_inputs)?,
        requested_vec_bytes_for_len::<u8>(shape.external_pin_bytes)?,
        external_key_allocations,
        requested_btree_bytes_for_len::<u128, ([GraphGpuRegister; 8], [GraphGpuRegister; 3])>(
            shape.noise_nodes,
        )?,
        requested_btree_bytes_for_len::<u128, ([GraphGpuRegister; 3], [GraphGpuRegister; 3])>(
            shape.gradient_nodes,
        )?,
        requested_btree_bytes_for_len::<(u128, String), GraphGpuRegister>(shape.instructions)?,
        requested_vec_bytes_for_len::<u8>(
            shape
                .instructions
                .checked_mul(10)
                .ok_or(Error::NumericOverflow)?,
        )?,
        shape
            .instructions
            .checked_mul(ALLOCATION_OVERHEAD_BYTES)
            .ok_or(Error::NumericOverflow)?,
        if shape.candidate_output {
            requested_vec_bytes_for_len::<GraphCandidate>(shape.invocations)?
        } else {
            0
        },
        if shape.scalar_output {
            requested_btree_bytes_for_len::<CandidateIdentity, DecisionScalar>(shape.invocations)?
        } else {
            0
        },
    ])
}

#[derive(Default)]
struct ExecutionPlanAllocationShape {
    pub(super) nodes: usize,
    pub(super) edges: usize,
    pub(super) pins: usize,
    maximum_module_path_words: usize,
    maximum_pin_name_bytes: usize,
}

pub(super) fn execution_plan_allocation_bound(graph: &CompiledBiomeGraph) -> Result<u64> {
    fn visit(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
        shape: &mut ExecutionPlanAllocationShape,
    ) -> Result<()> {
        let module_path = unit
            .nodes
            .first()
            .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
        let unit_demand = demand
            .unit(module_path)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.demandPlan".to_owned(),
                reason: "live unit has no execution demand slice".to_owned(),
            })?;
        shape.nodes = shape
            .nodes
            .checked_add(unit_demand.nodes.len())
            .ok_or(Error::NumericOverflow)?;
        shape.edges = shape
            .edges
            .checked_add(unit_demand.edges.len())
            .ok_or(Error::NumericOverflow)?;
        shape.pins = shape
            .pins
            .checked_add(unit_demand.inputs.len())
            .and_then(|pins| pins.checked_add(unit_demand.outputs.len()))
            .ok_or(Error::NumericOverflow)?;
        for edge in &unit_demand.edges {
            shape.maximum_pin_name_bytes = shape
                .maximum_pin_name_bytes
                .max(edge.from_pin.len())
                .max(edge.to_pin.len());
        }
        for name in unit_demand.inputs.iter().chain(&unit_demand.outputs) {
            shape.maximum_pin_name_bytes = shape.maximum_pin_name_bytes.max(name.len());
        }
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            shape.maximum_module_path_words = shape
                .maximum_module_path_words
                .max(node.debug_symbol.module_path.len());
            if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
            {
                visit(module, demand, shape)?;
            }
        }
        Ok(())
    }

    let mut shape = ExecutionPlanAllocationShape::default();
    visit(
        &graph.root,
        graph.demand_plan().execution_slice(),
        &mut shape,
    )?;
    let boundaries = shape
        .edges
        .checked_mul(2)
        .and_then(|value| value.checked_add(shape.pins))
        .ok_or(Error::NumericOverflow)?;
    let address_count = shape
        .nodes
        .checked_add(boundaries)
        .ok_or(Error::NumericOverflow)?;
    let address_words = address_count
        .checked_mul(shape.maximum_module_path_words)
        .ok_or(Error::NumericOverflow)?;
    let boundary_name_bytes = boundaries
        .checked_mul(shape.maximum_pin_name_bytes)
        .ok_or(Error::NumericOverflow)?;
    let group_inner_allocations = u64::try_from(shape.nodes)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(3)
        .and_then(|count| count.checked_mul(ALLOCATION_OVERHEAD_BYTES))
        .ok_or(Error::NumericOverflow)?;
    let address_allocations = u64::try_from(address_count)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let string_allocations = u64::try_from(boundaries)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let component_inner_allocations = u64::try_from(shape.nodes)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        requested_vec_bytes::<GraphExecutionGroup>(shape.nodes)?,
        requested_vec_bytes::<GraphExecutionNode>(shape.nodes)?,
        requested_vec_bytes::<GraphExecutionNode>(shape.nodes)?,
        requested_vec_bytes::<GraphExecutionBoundary>(boundaries)?,
        requested_vec_bytes::<Vec<u128>>(shape.nodes)?,
        requested_vec_bytes::<u128>(shape.nodes)?,
        requested_vec_bytes::<u128>(shape.nodes)?,
        requested_vec_bytes::<u128>(address_words)?,
        requested_vec_bytes::<u8>(boundary_name_bytes)?,
        requested_btree_bytes::<u128, &CompiledGraphNode>(shape.nodes)?,
        requested_btree_bytes::<u128, ()>(shape.nodes)?,
        requested_btree_bytes::<u128, usize>(shape.nodes)?,
        requested_btree_bytes::<usize, ()>(shape.nodes)?,
        requested_btree_bytes::<&crate::GraphValueLineage, ()>(shape.edges)?,
        requested_btree_bytes::<(u128, &str), ()>(boundaries)?,
        requested_btree_bytes::<(u128, &str), ()>(boundaries)?,
        requested_btree_bytes::<(u128, &str), ()>(boundaries)?,
        requested_btree_bytes::<(u128, &str), ()>(boundaries)?,
        requested_btree_bytes::<GraphExecutionBoundary, ()>(boundaries)?,
        requested_btree_bytes::<GraphExecutionBoundary, ()>(boundaries)?,
        group_inner_allocations,
        address_allocations,
        string_allocations,
        component_inner_allocations,
    ])
}
