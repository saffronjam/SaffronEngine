//! Global-stage value import, clipping, and canonical merging.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{WorldBounds, WorldCellKey};

use crate::memory::{
    checked_memory_sum, requested_btree_bytes, requested_btree_bytes_for_len, requested_vec_bytes,
    requested_vec_bytes_for_len,
};
use crate::{
    CompiledGraphNode, Error, GraphDomain, GraphNodeAddress, PlantId, PlantPoint,
    ProvenanceDecisionHandle, ProvenanceHandle, ProvenanceTable, Result,
};

pub(super) fn clip_global_value(
    mut value: GraphValue,
    solve_bounds: WorldBounds,
) -> Result<GraphValue> {
    match &mut value {
        GraphValue::Candidates(stream) => {
            stream
                .candidates
                .retain(|candidate| solve_bounds.contains(candidate.position));
        }
        GraphValue::Surface(field) => {
            field
                .values
                .retain(|_, sample| solve_bounds.contains(sample.position));
        }
        GraphValue::Macro(points) => {
            points.retain(|point| solve_bounds.contains(point.position));
        }
        GraphValue::Micro(tiles) => {
            tiles.retain(|tile| bounds_intersect(tile.cell.bounds(), solve_bounds));
        }
        GraphValue::Regions(regions) => {
            let mut clipped = Vec::new();
            crate::memory::reserve_exact(&mut clipped, regions.len(), "clipped global regions")?;
            for mut region in regions.drain(..) {
                if let Some(bounds) = intersect_bounds(region.bounds, solve_bounds)? {
                    region.bounds = bounds;
                    clipped.push(region);
                }
            }
            *regions = clipped;
        }
        GraphValue::Splines(splines) => {
            splines.retain(|spline| {
                spline
                    .points
                    .iter()
                    .any(|position| solve_bounds.contains(*position))
            });
        }
        GraphValue::Scalar(_)
        | GraphValue::Vector(_)
        | GraphValue::Hessian(_)
        | GraphValue::Species(_)
        | GraphValue::Communities(_)
        | GraphValue::Diagnostics(_) => {}
    }
    Ok(value)
}

pub(super) fn import_global_value(
    mut value: GraphValue,
    tile: &GlobalStageTile,
    state: &mut EvaluationState<'_>,
) -> Result<GraphValue> {
    let candidate_ids = graph_value_candidate_identities(&value);
    let mut decision_roots = Vec::new();
    crate::memory::reserve_exact(
        &mut decision_roots,
        candidate_ids.len(),
        "global provenance decision roots",
    )?;
    decision_roots.extend(
        candidate_ids
            .iter()
            .filter_map(|identity| tile.candidate_decisions.get(identity).copied()),
    );
    let decision_remap = state
        .provenance
        .import_decision_fragment(&tile.provenance, &decision_roots)?;
    for identity in candidate_ids {
        let Some(source) = tile.candidate_decisions.get(&identity) else {
            continue;
        };
        let destination =
            decision_remap
                .get(source)
                .copied()
                .ok_or_else(|| Error::GraphDocument {
                    path: "evaluation.globalStages.provenance".to_owned(),
                    reason: "candidate decision was not imported".to_owned(),
                })?;
        if let Some(existing) = state.candidate_decisions.get(&identity).copied()
            && existing != destination
        {
            if provenance_decision_descends_from(&state.provenance, destination, existing)? {
                state.candidate_decisions.insert(identity, destination);
            } else if !provenance_decision_descends_from(&state.provenance, existing, destination)?
            {
                return Err(Error::GraphDocument {
                    path: "evaluation.globalStages.provenance".to_owned(),
                    reason: "one candidate resolved to conflicting global-stage decisions"
                        .to_owned(),
                });
            }
        } else {
            state.candidate_decisions.insert(identity, destination);
        }
    }

    let record_count = match &value {
        GraphValue::Macro(points) => points.len(),
        GraphValue::Diagnostics(streams) => streams.iter().try_fold(0_usize, |total, stream| {
            total
                .checked_add(stream.rejected.len())
                .ok_or(Error::NumericOverflow)
        })?,
        _ => 0,
    };
    let mut record_handles = Vec::new();
    crate::memory::reserve_exact(
        &mut record_handles,
        record_count,
        "global provenance record handles",
    )?;
    match &value {
        GraphValue::Macro(points) => record_handles.extend(
            points
                .iter()
                .map(|point| ProvenanceHandle(point.provenance)),
        ),
        GraphValue::Diagnostics(streams) => record_handles.extend(
            streams
                .iter()
                .flat_map(|stream| stream.rejected.iter())
                .map(|rejected| rejected.provenance),
        ),
        _ => {}
    }
    let record_remap = state
        .provenance
        .import_fragment(&tile.provenance, &record_handles)?;
    match &mut value {
        GraphValue::Macro(points) => {
            if state.scope.is_cell() {
                state.ancestor_references.insert(tile.key.owner);
                points.clear();
            } else {
                for point in points {
                    point.provenance = record_remap
                        .records
                        .get(&ProvenanceHandle(point.provenance))
                        .ok_or_else(|| Error::GraphDocument {
                            path: "evaluation.globalStages.provenance".to_owned(),
                            reason: "macro point provenance was not imported".to_owned(),
                        })?
                        .0;
                }
            }
        }
        GraphValue::Diagnostics(streams) => {
            for stream in streams.iter_mut() {
                for value in &mut stream.rejected {
                    value.provenance = record_remap
                        .records
                        .get(&value.provenance)
                        .copied()
                        .ok_or_else(|| Error::GraphDocument {
                            path: "evaluation.globalStages.provenance".to_owned(),
                            reason: "rejection provenance was not imported".to_owned(),
                        })?;
                }
            }
            streams.clear();
        }
        _ => {}
    }
    Ok(value)
}

fn provenance_decision_descends_from(
    table: &ProvenanceTable,
    descendant: ProvenanceDecisionHandle,
    ancestor: ProvenanceDecisionHandle,
) -> Result<bool> {
    let mut pending = Vec::new();
    crate::memory::reserve_exact(
        &mut pending,
        table.decisions().len(),
        "global provenance ancestry traversal",
    )?;
    pending.push(descendant);
    let mut visited = BTreeSet::new();
    visited.insert(descendant);
    while let Some(handle) = pending.pop() {
        if handle == ancestor {
            return Ok(true);
        }
        let decision = table.decision(handle).ok_or_else(|| Error::GraphDocument {
            path: "evaluation.globalStages.provenance".to_owned(),
            reason: "candidate decision handle is missing".to_owned(),
        })?;
        pending.extend(
            decision
                .parents
                .iter()
                .copied()
                .filter(|parent| visited.insert(*parent)),
        );
    }
    Ok(false)
}

pub(super) fn graph_value_candidate_identity_upper_bound(value: &GraphValue) -> Result<u64> {
    let count = match value {
        GraphValue::Candidates(stream) => stream.candidates.len(),
        GraphValue::Scalar(field) => field.values.len(),
        GraphValue::Vector(field) => field.values.len(),
        GraphValue::Hessian(field) => field.values.len(),
        GraphValue::Surface(field) => field.values.len(),
        GraphValue::Diagnostics(streams) => streams.iter().try_fold(0_usize, |total, stream| {
            total
                .checked_add(stream.candidates.as_ref().map_or(0, Vec::len))
                .and_then(|count| count.checked_add(stream.field.as_ref().map_or(0, Vec::len)))
                .ok_or(Error::NumericOverflow)
        })?,
        GraphValue::Macro(_)
        | GraphValue::Micro(_)
        | GraphValue::Regions(_)
        | GraphValue::Splines(_)
        | GraphValue::Species(_)
        | GraphValue::Communities(_) => 0,
    };
    u64::try_from(count).map_err(|_| Error::NumericOverflow)
}

fn graph_value_candidate_identities(value: &GraphValue) -> BTreeSet<CandidateIdentity> {
    match value {
        GraphValue::Candidates(stream) => stream
            .candidates
            .iter()
            .map(|candidate| candidate.identity)
            .collect(),
        GraphValue::Scalar(field) => field.values.keys().copied().collect(),
        GraphValue::Vector(field) => field.values.keys().copied().collect(),
        GraphValue::Hessian(field) => field.values.keys().copied().collect(),
        GraphValue::Surface(field) => field.values.keys().copied().collect(),
        GraphValue::Diagnostics(streams) => streams
            .iter()
            .flat_map(|stream| {
                stream
                    .candidates
                    .iter()
                    .flatten()
                    .map(|sample| sample.identity)
                    .chain(stream.field.iter().flatten().map(|sample| sample.candidate))
            })
            .collect(),
        _ => BTreeSet::new(),
    }
}

type DiagnosticStreamMergeKey = (GraphNodeAddress, String, DiagnosticStreamScope);

#[derive(Clone, Copy, Default)]
struct DiagnosticMergeShape {
    pub(super) streams: u64,
    pub(super) candidates: u64,
    pub(super) fields: u64,
    pub(super) rejected: u64,
    module_path_items: u64,
    pub(super) label_bytes: u64,
}

impl DiagnosticMergeShape {
    pub(super) fn symbolic(value: SymbolicValueBound) -> Self {
        Self {
            streams: value.items,
            candidates: value.diagnostic_candidates,
            fields: value.diagnostic_fields,
            rejected: value.diagnostic_rejected,
            module_path_items: value.diagnostic_module_path_items,
            label_bytes: value.diagnostic_label_bytes,
        }
    }

    pub(super) fn actual(streams: &[NamedDiagnosticStream]) -> Result<Self> {
        streams.iter().try_fold(Self::default(), |shape, stream| {
            shape.checked_add(Self {
                streams: 1,
                candidates: stream.candidates.as_ref().map_or(0, Vec::len) as u64,
                fields: stream.field.as_ref().map_or(0, Vec::len) as u64,
                rejected: stream.rejected.len() as u64,
                module_path_items: stream.node.module_path.len() as u64,
                label_bytes: stream.label.len() as u64,
            })
        })
    }

    pub(super) fn checked_add(self, other: Self) -> Result<Self> {
        Ok(Self {
            streams: self
                .streams
                .checked_add(other.streams)
                .ok_or(Error::NumericOverflow)?,
            candidates: self
                .candidates
                .checked_add(other.candidates)
                .ok_or(Error::NumericOverflow)?,
            fields: self
                .fields
                .checked_add(other.fields)
                .ok_or(Error::NumericOverflow)?,
            rejected: self
                .rejected
                .checked_add(other.rejected)
                .ok_or(Error::NumericOverflow)?,
            module_path_items: self
                .module_path_items
                .checked_add(other.module_path_items)
                .ok_or(Error::NumericOverflow)?,
            label_bytes: self
                .label_bytes
                .checked_add(other.label_bytes)
                .ok_or(Error::NumericOverflow)?,
        })
    }

    fn scratch_bytes(self) -> Result<u64> {
        checked_memory_sum([
            requested_btree_bytes_for_len::<DiagnosticStreamMergeKey, NamedDiagnosticStream>(
                self.streams,
            )?,
            disjoint_vec_bytes::<u128>(self.module_path_items, self.streams)?,
            disjoint_vec_bytes::<u8>(self.label_bytes, self.streams)?,
            requested_vec_bytes_for_len::<NamedDiagnosticStream>(self.streams)?,
            requested_btree_bytes_for_len::<CandidateIdentity, DiagnosticCandidateSample>(
                self.candidates,
            )?,
            disjoint_vec_bytes::<DiagnosticCandidateSample>(self.candidates, self.streams)?,
            requested_btree_bytes_for_len::<CandidateIdentity, DiagnosticScalarSample>(
                self.fields,
            )?,
            disjoint_vec_bytes::<DiagnosticScalarSample>(self.fields, self.streams)?,
            disjoint_vec_bytes::<RejectedCandidate>(self.rejected, self.streams)?,
        ])
    }
}

pub(super) fn symbolic_global_merge_scratch_bytes(
    left: SymbolicValueBound,
    right: SymbolicValueBound,
) -> Result<u64> {
    let items = left
        .items
        .checked_add(right.items)
        .ok_or(Error::NumericOverflow)?;
    match left.domain {
        Some(GraphDomain::Candidates) => checked_memory_sum([
            requested_btree_bytes_for_len::<CandidateIdentity, GraphCandidate>(items)?,
            requested_vec_bytes_for_len::<GraphCandidate>(items)?,
        ]),
        Some(GraphDomain::MacroPoints) => checked_memory_sum([
            requested_btree_bytes_for_len::<PlantId, PlantPoint>(items)?,
            requested_vec_bytes_for_len::<PlantPoint>(items)?,
        ]),
        Some(GraphDomain::MicroField) => checked_memory_sum([
            requested_btree_bytes_for_len::<(WorldCellKey, u64), MicroFieldTile>(items)?,
            requested_vec_bytes_for_len::<MicroFieldTile>(items)?,
        ]),
        Some(GraphDomain::Regions) => checked_memory_sum([
            requested_btree_bytes_for_len::<(u128, WorldCellKey), EvaluationRegion>(items)?,
            requested_vec_bytes_for_len::<EvaluationRegion>(items)?,
        ]),
        Some(GraphDomain::Splines) => checked_memory_sum([
            requested_btree_bytes_for_len::<u128, EvaluationSpline>(items)?,
            requested_vec_bytes_for_len::<EvaluationSpline>(items)?,
        ]),
        Some(GraphDomain::Diagnostics) => DiagnosticMergeShape::symbolic(left)
            .checked_add(DiagnosticMergeShape::symbolic(right))?
            .scratch_bytes(),
        _ => Ok(0),
    }
}

pub(super) fn graph_value_merge_scratch_bytes(
    left: &GraphValue,
    right: &GraphValue,
) -> Result<u64> {
    match (left, right) {
        (GraphValue::Candidates(left), GraphValue::Candidates(right)) => {
            let items = left
                .candidates
                .len()
                .checked_add(right.candidates.len())
                .ok_or(Error::NumericOverflow)?;
            checked_memory_sum([
                requested_btree_bytes::<CandidateIdentity, GraphCandidate>(items)?,
                requested_vec_bytes::<GraphCandidate>(items)?,
            ])
        }
        (GraphValue::Macro(left), GraphValue::Macro(right)) => {
            let items = left
                .len()
                .checked_add(right.len())
                .ok_or(Error::NumericOverflow)?;
            checked_memory_sum([
                requested_btree_bytes::<PlantId, PlantPoint>(items)?,
                requested_vec_bytes::<PlantPoint>(items)?,
            ])
        }
        (GraphValue::Micro(left), GraphValue::Micro(right)) => {
            let items = left
                .len()
                .checked_add(right.len())
                .ok_or(Error::NumericOverflow)?;
            checked_memory_sum([
                requested_btree_bytes::<(WorldCellKey, u64), MicroFieldTile>(items)?,
                requested_vec_bytes::<MicroFieldTile>(items)?,
            ])
        }
        (GraphValue::Regions(left), GraphValue::Regions(right)) => {
            let items = left
                .len()
                .checked_add(right.len())
                .ok_or(Error::NumericOverflow)?;
            checked_memory_sum([
                requested_btree_bytes::<(u128, WorldCellKey), EvaluationRegion>(items)?,
                requested_vec_bytes::<EvaluationRegion>(items)?,
            ])
        }
        (GraphValue::Splines(left), GraphValue::Splines(right)) => {
            let items = left
                .len()
                .checked_add(right.len())
                .ok_or(Error::NumericOverflow)?;
            checked_memory_sum([
                requested_btree_bytes::<u128, EvaluationSpline>(items)?,
                requested_vec_bytes::<EvaluationSpline>(items)?,
            ])
        }
        (GraphValue::Diagnostics(left), GraphValue::Diagnostics(right)) => {
            DiagnosticMergeShape::actual(left)?
                .checked_add(DiagnosticMergeShape::actual(right)?)?
                .scratch_bytes()
        }
        _ => Ok(0),
    }
}

pub(super) fn merge_graph_value(
    destination: &mut Option<GraphValue>,
    source: GraphValue,
    node: &CompiledGraphNode,
) -> Result<()> {
    let Some(destination) = destination else {
        *destination = Some(source);
        return Ok(());
    };
    match (destination, source) {
        (GraphValue::Candidates(left), GraphValue::Candidates(right)) => {
            require_lineage(node, left.lineage, right.lineage)?;
            let mut candidates = left
                .candidates
                .drain(..)
                .map(|candidate| (candidate.identity, candidate))
                .collect::<BTreeMap<_, _>>();
            for candidate in right.candidates {
                insert_identical(&mut candidates, candidate.identity, candidate, node)?;
            }
            left.candidates = btree_values_vec(candidates, "merged global candidates")?;
        }
        (GraphValue::Scalar(left), GraphValue::Scalar(right)) => {
            require_lineage(node, left.lineage, right.lineage)?;
            merge_identical_maps(&mut left.values, right.values, node)?;
        }
        (GraphValue::Vector(left), GraphValue::Vector(right)) => {
            require_lineage(node, left.lineage, right.lineage)?;
            merge_identical_maps(&mut left.values, right.values, node)?;
        }
        (GraphValue::Hessian(left), GraphValue::Hessian(right)) => {
            require_lineage(node, left.lineage, right.lineage)?;
            merge_identical_maps(&mut left.values, right.values, node)?;
        }
        (GraphValue::Surface(left), GraphValue::Surface(right)) => {
            require_lineage(node, left.lineage, right.lineage)?;
            merge_identical_maps(&mut left.values, right.values, node)?;
        }
        (GraphValue::Macro(left), GraphValue::Macro(right)) => {
            let mut points = left
                .drain(..)
                .map(|point| (point.id, point))
                .collect::<BTreeMap<_, _>>();
            for point in right {
                insert_identical(&mut points, point.id, point, node)?;
            }
            *left = btree_values_vec(points, "merged global macro points")?;
        }
        (GraphValue::Micro(left), GraphValue::Micro(right)) => {
            let mut tiles = left
                .drain(..)
                .map(|tile| ((tile.cell, tile.family.value()), tile))
                .collect::<BTreeMap<_, _>>();
            for tile in right {
                insert_identical(&mut tiles, (tile.cell, tile.family.value()), tile, node)?;
            }
            *left = btree_values_vec(tiles, "merged global micro tiles")?;
        }
        (GraphValue::Regions(left), GraphValue::Regions(right)) => {
            let mut regions = left
                .drain(..)
                .map(|region| ((region.id, region.seed_cell), region))
                .collect::<BTreeMap<_, _>>();
            for region in right {
                insert_identical(&mut regions, (region.id, region.seed_cell), region, node)?;
            }
            *left = btree_values_vec(regions, "merged global regions")?;
        }
        (GraphValue::Splines(left), GraphValue::Splines(right)) => {
            let mut splines = left
                .drain(..)
                .map(|spline| (spline.id, spline))
                .collect::<BTreeMap<_, _>>();
            for spline in right {
                insert_identical(&mut splines, spline.id, spline, node)?;
            }
            *left = btree_values_vec(splines, "merged global splines")?;
        }
        (GraphValue::Species(left), GraphValue::Species(right)) => {
            if *left != right {
                return Err(global_merge_conflict(node));
            }
        }
        (GraphValue::Communities(left), GraphValue::Communities(right)) => {
            if *left != right {
                return Err(global_merge_conflict(node));
            }
        }
        (GraphValue::Diagnostics(left), GraphValue::Diagnostics(right)) => {
            merge_diagnostic_streams(left, right, node)?;
        }
        _ => return Err(global_merge_conflict(node)),
    }
    Ok(())
}

fn require_lineage(
    node: &CompiledGraphNode,
    left: CandidateLineage,
    right: CandidateLineage,
) -> Result<()> {
    if left != right {
        return Err(global_merge_conflict(node));
    }
    Ok(())
}

fn merge_diagnostic_streams(
    destination: &mut Vec<NamedDiagnosticStream>,
    source: Vec<NamedDiagnosticStream>,
    node: &CompiledGraphNode,
) -> Result<()> {
    let mut streams = destination
        .drain(..)
        .map(|stream| {
            (
                (stream.node.clone(), stream.label.clone(), stream.scope),
                stream,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for mut stream in source {
        let key = (stream.node.clone(), stream.label.clone(), stream.scope);
        let Some(existing) = streams.get_mut(&key) else {
            streams.insert(key, stream);
            continue;
        };
        match (&mut existing.candidates, stream.candidates.take()) {
            (Some(left), Some(right)) => {
                let mut candidates = left
                    .drain(..)
                    .map(|sample| (sample.identity, sample))
                    .collect::<BTreeMap<_, _>>();
                for sample in right {
                    insert_identical(&mut candidates, sample.identity, sample, node)?;
                }
                *left = btree_values_vec(candidates, "merged diagnostic candidates")?;
            }
            (None, None) => {}
            _ => return Err(global_merge_conflict(node)),
        }
        match (&mut existing.field, stream.field.take()) {
            (Some(left), Some(right)) => {
                let mut field = left
                    .drain(..)
                    .map(|sample| (sample.candidate, sample))
                    .collect::<BTreeMap<_, _>>();
                for sample in right {
                    insert_identical(&mut field, sample.candidate, sample, node)?;
                }
                *left = btree_values_vec(field, "merged diagnostic fields")?;
            }
            (None, None) => {}
            _ => return Err(global_merge_conflict(node)),
        }
        crate::memory::reserve_exact(
            &mut existing.rejected,
            stream.rejected.len(),
            "merged diagnostic rejections",
        )?;
        existing.rejected.append(&mut stream.rejected);
        existing.rejected.sort_unstable_by_key(|value| {
            (
                value.candidate,
                rejection_reason_byte(value.reason),
                value.provenance,
            )
        });
        existing.rejected.dedup();
    }
    *destination = btree_values_vec(streams, "merged diagnostic streams")?;
    Ok(())
}

fn btree_values_vec<K, V>(values: BTreeMap<K, V>, resource: &'static str) -> Result<Vec<V>> {
    let mut result = Vec::new();
    crate::memory::reserve_exact(&mut result, values.len(), resource)?;
    result.extend(values.into_values());
    Ok(result)
}

fn merge_identical_maps<K: Ord, V: PartialEq>(
    destination: &mut BTreeMap<K, V>,
    source: BTreeMap<K, V>,
    node: &CompiledGraphNode,
) -> Result<()> {
    for (key, value) in source {
        insert_identical(destination, key, value, node)?;
    }
    Ok(())
}

fn insert_identical<K: Ord, V: PartialEq>(
    destination: &mut BTreeMap<K, V>,
    key: K,
    value: V,
    node: &CompiledGraphNode,
) -> Result<()> {
    match destination.entry(key) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(value);
        }
        std::collections::btree_map::Entry::Occupied(entry) => {
            if entry.get() != &value {
                return Err(global_merge_conflict(node));
            }
        }
    }
    Ok(())
}

fn global_merge_conflict(node: &CompiledGraphNode) -> Error {
    Error::GraphDocument {
        path: node.debug_symbol.label.clone(),
        reason: "overlapping immutable global tiles produced conflicting values".to_owned(),
    }
}

pub(super) fn hex_hash(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}
