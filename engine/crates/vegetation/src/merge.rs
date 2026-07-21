//! Canonical map-level composition of local biome-instance evaluation results.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_spatial::{FieldChannel, FieldDerivative, WorldCellKey};

use crate::hash::sha256;
use crate::memory::reserve_exact;
use crate::{
    CandidateIdentity, Error, ExtensionColumn, GpuGroupEvaluationDiagnostic,
    GraphEvaluationDiagnostics, GraphEvaluationResult, GraphNodeAddress, MicroFieldTile,
    NodeEvaluationDiagnostic, PlantPointColumns, ProvenanceDecision, ProvenanceDecisionHandle,
    ProvenanceHandle, ProvenanceRecord, ProvenanceTable, QuantizedSurfaceFieldQueryTile,
    QuantizedSurfaceProjectionTile, RejectedCandidate, Result,
};

/// Merges overlapping local-biome outputs for one map cell through the canonical evaluator result.
///
/// Inputs are ordered by biome-instance GUID before composition, so worker completion order cannot
/// affect bytes. Every instance-local node, candidate, provenance, query-tile, diagnostic, and
/// reconstruction-seed identity is domain-separated by that GUID. Micro tiles sharing a
/// `(cell, family)` key must have identical dimensions; density and like-named attribute lanes
/// combine with element-wise saturating addition (`u16::MAX` and `i32::{MIN,MAX}` respectively).
/// The output seed hashes the family and ordered `(instance, source seed)` set rather than selecting
/// one contributor.
pub fn merge_graph_evaluation_results(
    mut inputs: Vec<(u128, GraphEvaluationResult)>,
) -> Result<GraphEvaluationResult> {
    if inputs.is_empty() {
        return Err(merge_error(
            "inputs",
            "at least one biome instance is required",
        ));
    }
    inputs.sort_unstable_by_key(|(instance, _)| *instance);
    for pair in inputs.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(merge_error(
                "inputs.biomeInstance",
                "biome instance identity is duplicated",
            ));
        }
    }
    let cell = inputs[0].1.cell;
    for (_, result) in &inputs {
        if result.cell != cell {
            return Err(merge_error(
                "inputs.cell",
                "all biome-instance results must own the same output cell",
            ));
        }
        result.canonical_bytes()?;
    }

    let mut namespaced = Vec::new();
    reserve_exact(
        &mut namespaced,
        inputs.len(),
        "namespaced graph evaluation results",
    )?;
    for (instance, result) in inputs {
        namespaced.push((instance, namespace_result(instance, result)?));
    }

    let mut provenance = ProvenanceTable::default();
    for (_, result) in &mut namespaced {
        import_result_provenance(&mut provenance, result)?;
    }

    let macro_points = merge_macro_points(&namespaced)?;
    let micro_fields = merge_micro_fields(&namespaced)?;
    let surface_projection_tiles = merge_projection_tiles(&namespaced)?;
    let surface_field_query_tiles = merge_field_query_tiles(&namespaced)?;
    let ancestor_references = namespaced
        .iter()
        .flat_map(|(_, result)| result.ancestor_references.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let diagnostics = merge_diagnostics(&namespaced)?;
    if diagnostics.accepted_count
        != u64::try_from(macro_points.ids.len()).map_err(|_| Error::NumericOverflow)?
    {
        return Err(merge_error(
            "diagnostics.acceptedCount",
            "summed accepted work does not match the merged macro row count",
        ));
    }

    let merged = GraphEvaluationResult {
        cell,
        macro_points,
        micro_fields,
        surface_projection_tiles,
        surface_field_query_tiles,
        ancestor_references,
        provenance,
        diagnostics,
    };
    merged.canonical_bytes()?;
    merged.cell_artifact_sections()?;
    Ok(merged)
}

fn namespace_result(
    instance: u128,
    mut result: GraphEvaluationResult,
) -> Result<GraphEvaluationResult> {
    let (provenance, remap) = namespace_provenance(instance, &result.provenance)?;
    remap_result_provenance(&mut result, &remap)?;
    result.provenance = provenance;

    for value in &mut result.macro_points.deterministic_keys {
        *value = namespace_u128(b"macro-deterministic-key", instance, *value);
    }
    for value in &mut result.macro_points.candidates {
        *value = namespace_u64(b"macro-candidate", instance, *value);
    }
    for tile in &mut result.surface_projection_tiles {
        tile.node = namespace_u128(b"node", instance, tile.node);
    }
    result
        .surface_projection_tiles
        .sort_unstable_by_key(projection_key);
    reject_adjacent(
        &result.surface_projection_tiles,
        projection_key,
        "surfaceProjectionTiles",
    )?;

    for tile in &mut result.surface_field_query_tiles {
        tile.node = namespace_u128(b"node", instance, tile.node);
        for sample in &mut tile.samples {
            sample.candidate = namespace_candidate(instance, sample.candidate);
        }
        tile.samples
            .sort_unstable_by_key(|sample| (sample.candidate, sample.query));
        reject_adjacent(
            &tile.samples,
            |sample| (sample.candidate, sample.query),
            "surfaceFieldQueryTiles.samples",
        )?;
    }
    result
        .surface_field_query_tiles
        .sort_unstable_by_key(field_query_key);
    reject_adjacent(
        &result.surface_field_query_tiles,
        field_query_key,
        "surfaceFieldQueryTiles",
    )?;

    namespace_diagnostics(instance, &mut result.diagnostics)?;
    result.canonical_bytes()?;
    Ok(result)
}

fn namespace_provenance(
    instance: u128,
    source: &ProvenanceTable,
) -> Result<(
    ProvenanceTable,
    BTreeMap<ProvenanceHandle, ProvenanceHandle>,
)> {
    checked_handle_capacity(source.decisions().len())?;
    checked_handle_capacity(source.records().len())?;
    let mut destination = ProvenanceTable::default();
    let mut decisions = BTreeMap::new();
    for (index, decision) in source.decisions().iter().enumerate() {
        let source_handle =
            ProvenanceDecisionHandle(u32::try_from(index).map_err(|_| Error::NumericOverflow)?);
        let mut parents = Vec::new();
        reserve_exact(
            &mut parents,
            decision.parents.len(),
            "namespaced provenance parents",
        )?;
        for parent in &decision.parents {
            parents.push(decisions.get(parent).copied().ok_or_else(|| {
                merge_error(
                    "provenance.decisions.parents",
                    "a provenance parent is missing or not parent-before-child",
                )
            })?);
        }
        let mut subgraph_path = Vec::new();
        reserve_exact(
            &mut subgraph_path,
            decision.subgraph_path.len().saturating_add(1),
            "namespaced provenance subgraph path",
        )?;
        subgraph_path.push(instance);
        subgraph_path.extend_from_slice(&decision.subgraph_path);
        let destination_handle = destination.intern_decision(ProvenanceDecision {
            parents,
            subgraph_path,
            node: namespace_u128(b"node", instance, decision.node),
            operator: decision.operator,
            candidate: namespace_u64(
                b"provenance-decision-candidate",
                instance,
                decision.candidate,
            ),
            outcome: decision.outcome,
        });
        decisions.insert(source_handle, destination_handle);
    }

    let mut records = BTreeMap::new();
    for (index, record) in source.records().iter().enumerate() {
        let source_handle =
            ProvenanceHandle(u32::try_from(index).map_err(|_| Error::NumericOverflow)?);
        let decision = decisions.get(&record.decision).copied().ok_or_else(|| {
            merge_error(
                "provenance.records.decision",
                "a provenance terminal decision is missing",
            )
        })?;
        let destination_handle = destination.intern(ProvenanceRecord {
            map: record.map,
            layer: record.layer,
            biome: record.biome,
            decision,
            candidate: namespace_u64(b"provenance-record-candidate", instance, record.candidate),
            family: record.family,
            plant: record.plant,
            variation: record.variation,
        });
        records.insert(source_handle, destination_handle);
    }
    Ok((destination, records))
}

fn import_result_provenance(
    destination: &mut ProvenanceTable,
    result: &mut GraphEvaluationResult,
) -> Result<()> {
    checked_handle_capacity(
        destination
            .decisions()
            .len()
            .checked_add(result.provenance.decisions().len())
            .ok_or(Error::NumericOverflow)?,
    )?;
    checked_handle_capacity(
        destination
            .records()
            .len()
            .checked_add(result.provenance.records().len())
            .ok_or(Error::NumericOverflow)?,
    )?;
    let selected = (0..result.provenance.records().len())
        .map(|index| {
            u32::try_from(index)
                .map(ProvenanceHandle)
                .map_err(|_| Error::NumericOverflow)
        })
        .collect::<Result<Vec<_>>>()?;
    let remap = destination.import_fragment(&result.provenance, &selected)?;
    remap_result_provenance(result, &remap.records)
}

fn remap_result_provenance(
    result: &mut GraphEvaluationResult,
    remap: &BTreeMap<ProvenanceHandle, ProvenanceHandle>,
) -> Result<()> {
    for handle in &mut result.macro_points.provenance {
        *handle = remapped_handle(remap, ProvenanceHandle(*handle))?.0;
    }
    for rejected in &mut result.diagnostics.rejected {
        rejected.provenance = remapped_handle(remap, rejected.provenance)?;
    }
    for stream in &mut result.diagnostics.streams {
        for rejected in &mut stream.rejected {
            rejected.provenance = remapped_handle(remap, rejected.provenance)?;
        }
    }
    Ok(())
}

fn remapped_handle(
    remap: &BTreeMap<ProvenanceHandle, ProvenanceHandle>,
    handle: ProvenanceHandle,
) -> Result<ProvenanceHandle> {
    remap.get(&handle).copied().ok_or_else(|| {
        merge_error(
            "provenance.handle",
            "an accepted or rejected result references a missing provenance record",
        )
    })
}

fn merge_macro_points(inputs: &[(u128, GraphEvaluationResult)]) -> Result<PlantPointColumns> {
    let total = inputs.iter().try_fold(0_usize, |total, (_, result)| {
        total
            .checked_add(result.macro_points.row_count()?)
            .ok_or(Error::NumericOverflow)
    })?;
    let schema = extension_schema(&inputs[0].1.macro_points);
    for (_, result) in &inputs[1..] {
        if extension_schema(&result.macro_points) != schema {
            return Err(Error::PointSchema(
                "biome-instance results use different registered extension columns".to_owned(),
            ));
        }
    }

    let mut rows = Vec::new();
    reserve_exact(&mut rows, total, "merged macro row order")?;
    for (input, (_, result)) in inputs.iter().enumerate() {
        rows.extend(
            result
                .macro_points
                .ids
                .iter()
                .copied()
                .enumerate()
                .map(|(row, id)| (id, input, row)),
        );
    }
    rows.sort_unstable_by_key(|(id, _, _)| *id);
    for pair in rows.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(Error::DuplicatePlantId(pair[0].0.to_string()));
        }
    }

    let mut merged = PlantPointColumns::default();
    merged.reserve_rows(total)?;
    for (id, element_type, stride) in schema {
        let byte_count = total
            .checked_mul(stride as usize)
            .ok_or(Error::NumericOverflow)?;
        let mut bytes = Vec::new();
        reserve_exact(&mut bytes, byte_count, "merged point extension bytes")?;
        merged.extensions.push(ExtensionColumn {
            id,
            element_type,
            stride,
            bytes,
        });
    }

    for (_, input, row) in rows {
        let source = &inputs[input].1.macro_points;
        macro_rules! push_column {
            ($field:ident) => {
                merged.$field.push(source.$field[row].clone())
            };
        }
        push_column!(ids);
        push_column!(owner_cells);
        push_column!(positions);
        push_column!(orientations);
        push_column!(scales);
        push_column!(bounds);
        push_column!(families);
        push_column!(variations);
        push_column!(lifecycles);
        push_column!(phenotypes);
        push_column!(representation_classes);
        push_column!(deterministic_keys);
        push_column!(candidates);
        push_column!(parents);
        push_column!(colonies);
        push_column!(ecology_ticks);
        push_column!(health);
        push_column!(moisture);
        push_column!(fuel);
        push_column!(phenology);
        push_column!(flags);
        push_column!(interaction_policies);
        push_column!(provenance);
        push_column!(attachments);
        push_column!(surface_projections);
        for (destination, source_column) in merged.extensions.iter_mut().zip(&source.extensions) {
            let stride = source_column.stride as usize;
            let start = row.checked_mul(stride).ok_or(Error::NumericOverflow)?;
            let end = start.checked_add(stride).ok_or(Error::NumericOverflow)?;
            destination
                .bytes
                .extend_from_slice(source_column.bytes.get(start..end).ok_or_else(|| {
                    Error::PointSchema("extension column row is incomplete".to_owned())
                })?);
        }
    }
    merged.validate()?;
    Ok(merged)
}

fn extension_schema(
    columns: &PlantPointColumns,
) -> Vec<(crate::PointColumnId, crate::PointColumnType, u32)> {
    columns
        .extensions
        .iter()
        .map(|column| (column.id, column.element_type, column.stride))
        .collect()
}

fn merge_micro_fields(inputs: &[(u128, GraphEvaluationResult)]) -> Result<Vec<MicroFieldTile>> {
    let mut by_cell_family =
        BTreeMap::<(WorldCellKey, u64), (Uuid, Vec<(u128, &MicroFieldTile)>)>::new();
    for (instance, result) in inputs {
        for tile in &result.micro_fields {
            validate_micro_tile(tile)?;
            by_cell_family
                .entry((tile.cell, tile.family.value()))
                .or_insert_with(|| (tile.family, Vec::new()))
                .1
                .push((*instance, tile));
        }
    }
    let mut merged = Vec::new();
    reserve_exact(&mut merged, by_cell_family.len(), "merged micro tiles")?;
    for ((cell, _), (family, contributors)) in by_cell_family {
        let dimensions = contributors[0].1.dimensions;
        let sample_count = contributors[0].1.density.len();
        let mut density = vec![0_u16; sample_count];
        let mut attributes = BTreeMap::<u128, Vec<i32>>::new();
        for (_, tile) in &contributors {
            if tile.dimensions != dimensions || tile.density.len() != sample_count {
                return Err(merge_error(
                    "microFields.dimensions",
                    "overlapping micro tiles have incompatible dimensions",
                ));
            }
            for (destination, source) in density.iter_mut().zip(&tile.density) {
                *destination = destination.saturating_add(*source);
            }
            for (channel, source) in &tile.attributes {
                let destination = attributes
                    .entry(*channel)
                    .or_insert_with(|| vec![0_i32; sample_count]);
                for (destination, source) in destination.iter_mut().zip(source) {
                    *destination = destination.saturating_add(*source);
                }
            }
        }
        merged.push(MicroFieldTile {
            cell,
            family,
            dimensions,
            density,
            attributes,
            reconstruction_seed: merged_micro_seed(cell, family, dimensions, &contributors),
        });
    }
    Ok(merged)
}

fn validate_micro_tile(tile: &MicroFieldTile) -> Result<()> {
    let sample_count = tile
        .dimensions
        .into_iter()
        .try_fold(1_usize, |total, value| {
            total
                .checked_mul(value as usize)
                .ok_or(Error::NumericOverflow)
        })?;
    if tile.dimensions.contains(&0)
        || tile.density.len() != sample_count
        || tile
            .attributes
            .values()
            .any(|values| values.len() != sample_count)
    {
        return Err(merge_error(
            "microFields",
            "micro tile dimensions and density/attribute row counts disagree",
        ));
    }
    Ok(())
}

fn merged_micro_seed(
    cell: WorldCellKey,
    family: Uuid,
    dimensions: [u32; 3],
    contributors: &[(u128, &MicroFieldTile)],
) -> u128 {
    let mut bytes = b"saffron-anima/map-biome-micro-merge-seed/v1\0".to_vec();
    bytes.extend_from_slice(&cell.canonical_bytes());
    bytes.extend_from_slice(&family.value().to_be_bytes());
    for dimension in dimensions {
        bytes.extend_from_slice(&dimension.to_be_bytes());
    }
    for (instance, tile) in contributors {
        bytes.extend_from_slice(&instance.to_be_bytes());
        bytes.extend_from_slice(&tile.reconstruction_seed.to_be_bytes());
    }
    u128::from_be_bytes(sha256(&bytes)[..16].try_into().unwrap())
}

fn merge_projection_tiles(
    inputs: &[(u128, GraphEvaluationResult)],
) -> Result<Vec<QuantizedSurfaceProjectionTile>> {
    let mut tiles = BTreeMap::new();
    for (_, result) in inputs {
        for tile in &result.surface_projection_tiles {
            if tiles.insert(projection_key(tile), tile.clone()).is_some() {
                return Err(merge_error(
                    "surfaceProjectionTiles",
                    "namespaced projection tile identity collided",
                ));
            }
        }
    }
    Ok(tiles.into_values().collect())
}

fn merge_field_query_tiles(
    inputs: &[(u128, GraphEvaluationResult)],
) -> Result<Vec<QuantizedSurfaceFieldQueryTile>> {
    let mut tiles = BTreeMap::new();
    for (_, result) in inputs {
        for tile in &result.surface_field_query_tiles {
            if tiles.insert(field_query_key(tile), tile.clone()).is_some() {
                return Err(merge_error(
                    "surfaceFieldQueryTiles",
                    "namespaced field-query tile identity collided",
                ));
            }
        }
    }
    Ok(tiles.into_values().collect())
}

fn merge_diagnostics(
    inputs: &[(u128, GraphEvaluationResult)],
) -> Result<GraphEvaluationDiagnostics> {
    let mut merged = GraphEvaluationDiagnostics::default();
    let mut nodes = BTreeMap::<GraphNodeAddress, NodeEvaluationDiagnostic>::new();
    let mut gpu_groups = BTreeMap::<Vec<GraphNodeAddress>, GpuGroupEvaluationDiagnostic>::new();
    let mut rejected = BTreeMap::new();
    let mut streams = BTreeMap::new();
    for (_, result) in inputs {
        checked_add(
            &mut merged.candidate_count,
            result.diagnostics.candidate_count,
        )?;
        checked_add(
            &mut merged.accepted_count,
            result.diagnostics.accepted_count,
        )?;
        for node in &result.diagnostics.nodes {
            let key = GraphNodeAddress {
                module_path: node.module_path.clone(),
                node: node.node,
            };
            match nodes.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(node.clone());
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge_node_diagnostic(entry.get_mut(), node)?;
                }
            }
        }
        for group in &result.diagnostics.gpu_groups {
            match gpu_groups.entry(group.nodes.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(group.clone());
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge_gpu_diagnostic(entry.get_mut(), group)?;
                }
            }
        }
        for value in &result.diagnostics.rejected {
            let key = rejected_key(value);
            if rejected.insert(key, value.clone()).is_some() {
                return Err(merge_error(
                    "diagnostics.rejected",
                    "namespaced rejected-candidate identity collided",
                ));
            }
        }
        for stream in &result.diagnostics.streams {
            let key = (stream.node.clone(), stream.label.clone(), stream.scope);
            if streams.insert(key, stream.clone()).is_some() {
                return Err(merge_error(
                    "diagnostics.streams",
                    "namespaced diagnostic stream identity collided",
                ));
            }
        }
    }
    merged.nodes = nodes.into_values().collect();
    merged.gpu_groups = gpu_groups.into_values().collect();
    merged.rejected = rejected.into_values().collect();
    merged.streams = streams.into_values().collect();
    Ok(merged)
}

fn namespace_diagnostics(
    instance: u128,
    diagnostics: &mut GraphEvaluationDiagnostics,
) -> Result<()> {
    for node in &mut diagnostics.nodes {
        node.module_path.insert(0, instance);
        node.node = namespace_u128(b"node", instance, node.node);
    }
    diagnostics.nodes.sort_unstable_by(|left, right| {
        (&left.module_path, left.node).cmp(&(&right.module_path, right.node))
    });
    for group in &mut diagnostics.gpu_groups {
        for node in &mut group.nodes {
            namespace_graph_node(instance, node);
        }
        group.nodes.sort_unstable();
        reject_adjacent(&group.nodes, Clone::clone, "diagnostics.gpuGroups.nodes")?;
    }
    diagnostics
        .gpu_groups
        .sort_unstable_by(|left, right| left.nodes.cmp(&right.nodes));

    for rejected in &mut diagnostics.rejected {
        namespace_rejected(instance, rejected);
    }
    diagnostics.rejected.sort_unstable_by_key(rejected_key);
    reject_adjacent(&diagnostics.rejected, rejected_key, "diagnostics.rejected")?;

    for stream in &mut diagnostics.streams {
        namespace_graph_node(instance, &mut stream.node);
        if let crate::DiagnosticStreamScope::CandidateLineage(lineage) = &mut stream.scope {
            lineage.0 = namespace_u128(b"diagnostic-lineage", instance, lineage.0);
        }
        if let Some(candidates) = &mut stream.candidates {
            for sample in candidates.iter_mut() {
                sample.identity = namespace_candidate(instance, sample.identity);
            }
            candidates.sort_unstable_by_key(|sample| sample.identity);
            reject_adjacent(
                candidates,
                |sample| sample.identity,
                "diagnostics.streams.candidates",
            )?;
        }
        if let Some(field) = &mut stream.field {
            for sample in field.iter_mut() {
                sample.candidate = namespace_candidate(instance, sample.candidate);
            }
            field.sort_unstable_by_key(|sample| sample.candidate);
            reject_adjacent(
                field,
                |sample| sample.candidate,
                "diagnostics.streams.field",
            )?;
        }
        for rejected in &mut stream.rejected {
            namespace_rejected(instance, rejected);
        }
        stream.rejected.sort_unstable_by_key(rejected_key);
        reject_adjacent(
            &stream.rejected,
            rejected_key,
            "diagnostics.streams.rejected",
        )?;
    }
    diagnostics.streams.sort_unstable_by(|left, right| {
        (&left.node, &left.label, left.scope).cmp(&(&right.node, &right.label, right.scope))
    });
    Ok(())
}

fn namespace_rejected(instance: u128, rejected: &mut RejectedCandidate) {
    rejected.candidate = namespace_candidate(instance, rejected.candidate);
}

fn namespace_candidate(instance: u128, value: CandidateIdentity) -> CandidateIdentity {
    CandidateIdentity {
        node: namespace_u128(b"node", instance, value.node),
        node_address: namespace_u128(b"candidate-node-address", instance, value.node_address),
        node_semantic_revision: value.node_semantic_revision,
        ordinal: value.ordinal,
        ancestor: value.ancestor,
    }
}

fn namespace_graph_node(instance: u128, value: &mut GraphNodeAddress) {
    value.module_path.insert(0, instance);
    value.node = namespace_u128(b"node", instance, value.node);
}

fn merge_node_diagnostic(
    destination: &mut NodeEvaluationDiagnostic,
    source: &NodeEvaluationDiagnostic,
) -> Result<()> {
    if destination.operator != source.operator
        || destination.symbol != source.symbol
        || destination.execution_domain != source.execution_domain
    {
        return Err(merge_error(
            "diagnostics.nodes",
            "one namespaced node identity has incompatible diagnostic metadata",
        ));
    }
    checked_add(&mut destination.input_candidates, source.input_candidates)?;
    checked_add(&mut destination.output_candidates, source.output_candidates)?;
    checked_add(&mut destination.output_bytes, source.output_bytes)?;
    checked_add(&mut destination.transfer_bytes, source.transfer_bytes)?;
    checked_add(
        &mut destination.predicted_transfer_bytes,
        source.predicted_transfer_bytes,
    )?;
    checked_add(&mut destination.elapsed_micros, source.elapsed_micros)
}

fn merge_gpu_diagnostic(
    destination: &mut GpuGroupEvaluationDiagnostic,
    source: &GpuGroupEvaluationDiagnostic,
) -> Result<()> {
    checked_add(&mut destination.invocation_count, source.invocation_count)?;
    checked_add(&mut destination.transfer_bytes, source.transfer_bytes)?;
    checked_add(&mut destination.output_bytes, source.output_bytes)?;
    checked_add(&mut destination.elapsed_micros, source.elapsed_micros)
}

fn checked_add(destination: &mut u64, source: u64) -> Result<()> {
    *destination = destination
        .checked_add(source)
        .ok_or(Error::NumericOverflow)?;
    Ok(())
}

fn projection_key(tile: &QuantizedSurfaceProjectionTile) -> (u128, u32, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.provider_set_hash,
    )
}

fn field_query_key(
    tile: &QuantizedSurfaceFieldQueryTile,
) -> (u128, u32, FieldChannel, FieldDerivative, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.channel,
        tile.derivative,
        tile.provider_set_hash,
    )
}

fn rejected_key(value: &RejectedCandidate) -> (CandidateIdentity, u8, ProvenanceHandle) {
    (value.candidate, rejection_reason(value), value.provenance)
}

fn rejection_reason(value: &RejectedCandidate) -> u8 {
    match value.reason {
        crate::CandidateRejectionReason::SurfaceMiss => 0,
        crate::CandidateRejectionReason::Threshold => 1,
        crate::CandidateRejectionReason::WeightedElimination => 2,
        crate::CandidateRejectionReason::PriorityExclusion => 3,
        crate::CandidateRejectionReason::Competition => 4,
        crate::CandidateRejectionReason::ForeignOwner => 5,
        crate::CandidateRejectionReason::NoSpecies => 6,
    }
}

fn reject_adjacent<T, K: Eq>(
    values: &[T],
    key: impl Fn(&T) -> K,
    path: &'static str,
) -> Result<()> {
    if values.windows(2).any(|pair| key(&pair[0]) == key(&pair[1])) {
        return Err(merge_error(path, "canonical identity is duplicated"));
    }
    Ok(())
}

fn checked_handle_capacity(length: usize) -> Result<()> {
    if length > u32::MAX as usize {
        return Err(Error::NumericOverflow);
    }
    Ok(())
}

fn namespace_u128(domain: &[u8], instance: u128, value: u128) -> u128 {
    let mut bytes = b"saffron-anima/map-biome-instance-namespace/v1\0".to_vec();
    bytes.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&instance.to_be_bytes());
    bytes.extend_from_slice(&value.to_be_bytes());
    u128::from_be_bytes(sha256(&bytes)[..16].try_into().unwrap())
}

fn namespace_u64(domain: &[u8], instance: u128, value: u64) -> u64 {
    let mut bytes = b"saffron-anima/map-biome-instance-namespace/v1\0".to_vec();
    bytes.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&instance.to_be_bytes());
    bytes.extend_from_slice(&value.to_be_bytes());
    u64::from_be_bytes(sha256(&bytes)[..8].try_into().unwrap())
}

fn merge_error(path: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::GraphDocument {
        path: format!("evaluation.mapMerge.{}", path.into()),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use saffron_spatial::{DecisionScalar, UnitInterval, WorldBounds, WorldPosition};

    use super::*;
    use crate::{
        CandidateRejectionReason, DiagnosticStreamScope, GraphOperator, InteractionPolicy,
        NamedDiagnosticStream, PlantFlags, PlantId, PlantLifecycle, PlantPoint, PointColumnId,
        PointColumnType, ProvenanceDecisionOutcome, QuantizedOrientation,
    };

    fn empty_result(cell: WorldCellKey) -> GraphEvaluationResult {
        GraphEvaluationResult {
            cell,
            macro_points: PlantPointColumns::default(),
            micro_fields: Vec::new(),
            surface_projection_tiles: Vec::new(),
            surface_field_query_tiles: Vec::new(),
            ancestor_references: Vec::new(),
            provenance: ProvenanceTable::default(),
            diagnostics: GraphEvaluationDiagnostics::default(),
        }
    }

    fn micro_result(cell: WorldCellKey, tiles: Vec<MicroFieldTile>) -> GraphEvaluationResult {
        GraphEvaluationResult {
            micro_fields: tiles,
            ..empty_result(cell)
        }
    }

    fn micro_tile(
        cell: WorldCellKey,
        family: u64,
        density: [u16; 2],
        attribute: Option<(u128, [i32; 2])>,
        seed: u128,
    ) -> MicroFieldTile {
        MicroFieldTile {
            cell,
            family: Uuid(family),
            dimensions: [2, 1, 1],
            density: density.to_vec(),
            attributes: attribute
                .map(|(channel, values)| (channel, values.to_vec()))
                .into_iter()
                .collect(),
            reconstruction_seed: seed,
        }
    }

    fn provenance(
        candidate: u64,
        plant: Option<PlantId>,
        outcome: ProvenanceDecisionOutcome,
    ) -> ProvenanceTable {
        let mut table = ProvenanceTable::default();
        let decision = table.intern_decision(ProvenanceDecision {
            parents: Vec::new(),
            subgraph_path: vec![91],
            node: 17,
            operator: GraphOperator::MicroOutput,
            candidate,
            outcome,
        });
        table.intern(ProvenanceRecord {
            map: Uuid(1),
            layer: 2,
            biome: Uuid(3),
            decision,
            candidate,
            family: Some(Uuid(7)),
            plant,
            variation: 0,
        });
        table
    }

    fn candidate(ordinal: u64) -> CandidateIdentity {
        CandidateIdentity {
            node: 17,
            node_address: 19,
            node_semantic_revision: 1,
            ordinal,
            ancestor: 0,
        }
    }

    fn rejected_result(cell: WorldCellKey, ordinal: u64) -> GraphEvaluationResult {
        let rejected = RejectedCandidate {
            candidate: candidate(ordinal),
            reason: CandidateRejectionReason::Threshold,
            provenance: ProvenanceHandle(0),
        };
        GraphEvaluationResult {
            provenance: provenance(ordinal, None, ProvenanceDecisionOutcome::Rejected),
            diagnostics: GraphEvaluationDiagnostics {
                rejected: vec![rejected.clone()],
                streams: vec![NamedDiagnosticStream {
                    node: GraphNodeAddress {
                        module_path: vec![91],
                        node: 17,
                    },
                    label: "rejections".to_owned(),
                    scope: DiagnosticStreamScope::GlobalSnapshot,
                    candidates: None,
                    field: None,
                    rejected: vec![rejected],
                }],
                candidate_count: 1,
                ..GraphEvaluationDiagnostics::default()
            },
            ..empty_result(cell)
        }
    }

    fn point_result(cell: WorldCellKey, id_byte: u8, extension: u32) -> GraphEvaluationResult {
        let id = PlantId::explicit([id_byte; 16]).unwrap();
        let position = WorldPosition::from_global_ticks([i128::from(id_byte), 0, 0]).unwrap();
        let mut macro_points = PlantPointColumns::from_points(vec![PlantPoint {
            id,
            owner: cell,
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_integer(1).unwrap(); 3],
            bounds: WorldBounds::new(
                [i128::from(id_byte) - 1, -1, -1],
                [i128::from(id_byte) + 2, 2, 2],
            )
            .unwrap(),
            family: Uuid(7),
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: u128::from(id_byte),
            candidate: u64::from(id_byte),
            parent: None,
            colony: None,
            ecology_tick: 0,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ONE,
            fuel: UnitInterval::ONE,
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::default(),
            interaction_policy: InteractionPolicy::Decorative,
            provenance: 0,
            attachment: None,
            surface_projection: [DecisionScalar::from_bits(0); 3],
        }])
        .unwrap();
        macro_points
            .add_extension(ExtensionColumn {
                id: PointColumnId(0x8000_0001),
                element_type: PointColumnType::U32,
                stride: 4,
                bytes: extension.to_be_bytes().to_vec(),
            })
            .unwrap();
        GraphEvaluationResult {
            macro_points,
            provenance: provenance(
                u64::from(id_byte),
                Some(id),
                ProvenanceDecisionOutcome::Accepted,
            ),
            diagnostics: GraphEvaluationDiagnostics {
                candidate_count: 1,
                accepted_count: 1,
                ..GraphEvaluationDiagnostics::default()
            },
            ..empty_result(cell)
        }
    }

    #[test]
    fn merge_is_permutation_invariant_and_saturates_per_family() {
        let cell = WorldCellKey::base(0, 0, 0);
        let first = micro_result(
            cell,
            vec![micro_tile(
                cell,
                7,
                [u16::MAX, 2],
                Some((41, [i32::MAX, -2])),
                11,
            )],
        );
        let second = micro_result(
            cell,
            vec![
                micro_tile(cell, 7, [1, u16::MAX], Some((41, [1, i32::MIN])), 13),
                micro_tile(cell, 9, [3, 5], None, 17),
            ],
        );
        let forward =
            merge_graph_evaluation_results(vec![(101, first.clone()), (202, second.clone())])
                .unwrap();
        let reverse = merge_graph_evaluation_results(vec![(202, second), (101, first)]).unwrap();

        assert_eq!(forward, reverse);
        assert_eq!(
            forward.canonical_bytes().unwrap(),
            reverse.canonical_bytes().unwrap()
        );
        assert_eq!(forward.cell_artifact_sections().unwrap().len(), 12);
        assert_eq!(forward.micro_fields.len(), 2);
        assert_eq!(forward.micro_fields[0].family, Uuid(7));
        assert_eq!(forward.micro_fields[0].density, vec![u16::MAX; 2]);
        assert_eq!(
            forward.micro_fields[0].attributes[&41],
            vec![i32::MAX, i32::MIN]
        );
        assert_ne!(forward.micro_fields[0].reconstruction_seed, 11);
        assert_eq!(forward.micro_fields[1].family, Uuid(9));
    }

    #[test]
    fn merge_namespaces_rejection_provenance_and_stream_identities() {
        let cell = WorldCellKey::base(0, 0, 0);
        let merged = merge_graph_evaluation_results(vec![
            (101, rejected_result(cell, 1)),
            (202, rejected_result(cell, 1)),
        ])
        .unwrap();

        assert_eq!(merged.diagnostics.rejected.len(), 2);
        assert_eq!(merged.diagnostics.streams.len(), 2);
        assert_ne!(
            merged.diagnostics.rejected[0].candidate,
            merged.diagnostics.rejected[1].candidate
        );
        for rejected in &merged.diagnostics.rejected {
            let explanation = merged.explain_rejection(rejected.candidate).unwrap();
            assert_eq!(
                explanation.rejection_reason,
                Some(CandidateRejectionReason::Threshold)
            );
        }
    }

    #[test]
    fn merge_sorts_macro_rows_and_keeps_extension_rows_attached() {
        let cell = WorldCellKey::base(0, 0, 0);
        let later = point_result(cell, 9, 900);
        let earlier = point_result(cell, 3, 300);
        let merged = merge_graph_evaluation_results(vec![(101, later), (202, earlier)]).unwrap();

        assert!(merged.macro_points.ids[0] < merged.macro_points.ids[1]);
        assert_eq!(
            merged.macro_points.extensions[0].bytes,
            [300_u32.to_be_bytes(), 900_u32.to_be_bytes()].concat()
        );
        for id in &merged.macro_points.ids {
            assert_eq!(merged.explain_plant(*id).unwrap().record.plant, Some(*id));
        }
    }

    #[test]
    fn merge_rejects_input_and_identity_collisions() {
        let cell = WorldCellKey::base(0, 0, 0);
        assert!(matches!(
            merge_graph_evaluation_results(vec![
                (101, empty_result(cell)),
                (101, empty_result(cell)),
            ]),
            Err(Error::GraphDocument { path, .. }) if path.ends_with("inputs.biomeInstance")
        ));
        assert!(matches!(
            merge_graph_evaluation_results(vec![
                (101, empty_result(cell)),
                (202, empty_result(WorldCellKey::base(1, 0, 0))),
            ]),
            Err(Error::GraphDocument { path, .. }) if path.ends_with("inputs.cell")
        ));
        assert!(matches!(
            merge_graph_evaluation_results(vec![
                (101, point_result(cell, 3, 300)),
                (202, point_result(cell, 3, 300)),
            ]),
            Err(Error::DuplicatePlantId(_))
        ));
    }

    #[test]
    fn merge_checks_work_diagnostic_overflow() {
        let cell = WorldCellKey::base(0, 0, 0);
        let mut first = empty_result(cell);
        first.diagnostics.candidate_count = u64::MAX;
        let mut second = empty_result(cell);
        second.diagnostics.candidate_count = 1;
        assert!(matches!(
            merge_graph_evaluation_results(vec![(101, first), (202, second)]),
            Err(Error::NumericOverflow)
        ));
    }
}
