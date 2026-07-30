//! Provenance and the single ordered vegetation-map layer algebra.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, DecisionVec3, FieldChannel, UnitInterval, WorldBounds, WorldPosition,
};

use crate::memory::{checked_memory_sum, requested_vec_bytes, requested_vec_with, reserve_exact};
use crate::{Error, GraphOperator, InteractionPolicy, PlantId, Result};

/// Compact handle into a cell/map provenance table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProvenanceHandle(pub u32);

/// Compact handle into the shared provenance decision DAG.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProvenanceDecisionHandle(pub u32);

/// Stable outcome of one candidate decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvenanceDecisionOutcome {
    /// A generator created the candidate.
    Produced,
    /// An operator retained or transformed the candidate.
    Retained,
    /// A macro output accepted the candidate as a plant.
    Accepted,
    /// An operator rejected the candidate.
    Rejected,
}

/// One shared node in the candidate decision DAG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceDecision {
    /// Prior decisions that contributed to this decision.
    pub parents: Vec<ProvenanceDecisionHandle>,
    /// Stable module/subgraph call path.
    pub subgraph_path: Vec<u128>,
    /// Stable evaluator-node identity.
    pub node: u128,
    /// Operator that made the decision.
    pub operator: GraphOperator,
    /// Sampler candidate identity.
    pub candidate: u64,
    pub outcome: ProvenanceDecisionOutcome,
}

/// Complete lineage for an accepted or rejected vegetation candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceRecord {
    pub map: Uuid,
    pub layer: u128,
    /// Root biome asset.
    pub biome: Uuid,
    /// Terminal candidate decision in the shared DAG.
    pub decision: ProvenanceDecisionHandle,
    /// Sampler candidate identity.
    pub candidate: u64,
    /// Selected plant family, absent when rejection occurred before selection.
    pub family: Option<Uuid>,
    /// Accepted stable plant identity, absent for rejected candidates.
    pub plant: Option<PlantId>,
    pub variation: u32,
}

/// Deduplicated compact provenance table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProvenanceTable {
    decisions: Vec<ProvenanceDecision>,
    records: Vec<ProvenanceRecord>,
}

/// Source-to-destination handles produced by one provenance-fragment import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProvenanceFragmentRemap {
    /// Every reachable source decision and its destination handle.
    pub decisions: BTreeMap<ProvenanceDecisionHandle, ProvenanceDecisionHandle>,
    /// Every selected source record and its destination handle.
    pub records: BTreeMap<ProvenanceHandle, ProvenanceHandle>,
}

impl ProvenanceTable {
    pub(crate) fn requested_memory_bytes(&self) -> Result<u64> {
        checked_memory_sum([
            requested_vec_with(&self.decisions, |decision| {
                checked_memory_sum([
                    requested_vec_bytes::<ProvenanceDecisionHandle>(decision.parents.capacity())?,
                    requested_vec_bytes::<u128>(decision.subgraph_path.capacity())?,
                ])
            })?,
            requested_vec_bytes::<ProvenanceRecord>(self.records.capacity())?,
        ])
    }

    /// Interns one decision and returns its stable table-local handle.
    pub fn intern_decision(
        &mut self,
        mut decision: ProvenanceDecision,
    ) -> ProvenanceDecisionHandle {
        decision.parents.sort_unstable();
        decision.parents.dedup();
        if let Some(index) = self
            .decisions
            .iter()
            .position(|candidate| candidate == &decision)
        {
            return ProvenanceDecisionHandle(index as u32);
        }
        let handle = ProvenanceDecisionHandle(self.decisions.len() as u32);
        self.decisions.push(decision);
        handle
    }

    /// Interns one lineage record and returns its stable table-local handle.
    pub fn intern(&mut self, record: ProvenanceRecord) -> ProvenanceHandle {
        if let Some(index) = self
            .records
            .iter()
            .position(|candidate| candidate == &record)
        {
            return ProvenanceHandle(index as u32);
        }
        let handle = ProvenanceHandle(self.records.len() as u32);
        self.records.push(record);
        handle
    }

    /// Imports selected records and their complete decision ancestry in canonical dependency order.
    pub fn import_fragment(
        &mut self,
        source: &Self,
        selected: &[ProvenanceHandle],
    ) -> Result<ProvenanceFragmentRemap> {
        let selected = selected.iter().copied().collect::<BTreeSet<_>>();
        let mut roots = Vec::new();
        reserve_exact(&mut roots, selected.len(), "provenance import roots")?;
        for handle in &selected {
            roots.push(
                source
                    .get(*handle)
                    .map(|record| record.decision)
                    .ok_or_else(|| {
                        invalid_provenance_reference(
                            format!("records.{}", handle.0),
                            "selected record is missing",
                        )
                    })?,
            );
        }
        let mut remap = ProvenanceFragmentRemap {
            decisions: self.import_decision_fragment(source, &roots)?,
            records: BTreeMap::new(),
        };
        reserve_exact(
            &mut self.records,
            selected.len(),
            "imported provenance records",
        )?;

        for source_handle in selected {
            let source_record = source.get(source_handle).ok_or_else(|| {
                invalid_provenance_reference(
                    format!("records.{}", source_handle.0),
                    "record is missing",
                )
            })?;
            let decision = remap
                .decisions
                .get(&source_record.decision)
                .copied()
                .ok_or_else(|| {
                    invalid_provenance_reference(
                        format!("records.{}.decision", source_handle.0),
                        "terminal decision was not imported",
                    )
                })?;
            let destination = self.intern(ProvenanceRecord {
                map: source_record.map,
                layer: source_record.layer,
                biome: source_record.biome,
                decision,
                candidate: source_record.candidate,
                family: source_record.family,
                plant: source_record.plant,
                variation: source_record.variation,
            });
            remap.records.insert(source_handle, destination);
        }

        Ok(remap)
    }

    /// Imports selected decisions and their complete ancestry in canonical dependency order.
    pub fn import_decision_fragment(
        &mut self,
        source: &Self,
        selected: &[ProvenanceDecisionHandle],
    ) -> Result<BTreeMap<ProvenanceDecisionHandle, ProvenanceDecisionHandle>> {
        let selected = selected.iter().copied().collect::<BTreeSet<_>>();
        let decision_order = provenance_fragment_decision_order(source, &selected)?;
        let mut remap = BTreeMap::new();
        reserve_exact(
            &mut self.decisions,
            decision_order.len(),
            "imported provenance decisions",
        )?;
        for source_handle in decision_order {
            let source_decision = source.decision(source_handle).ok_or_else(|| {
                invalid_provenance_reference(
                    format!("decisions.{}", source_handle.0),
                    "decision is missing",
                )
            })?;
            let mut parents = Vec::new();
            reserve_exact(
                &mut parents,
                source_decision.parents.len(),
                "imported provenance decision parents",
            )?;
            for parent in &source_decision.parents {
                parents.push(remap.get(parent).copied().ok_or_else(|| {
                    invalid_provenance_reference(
                        format!("decisions.{}.parents.{}", source_handle.0, parent.0),
                        "parent was not imported before its dependent decision",
                    )
                })?);
            }
            let destination = self.intern_decision(ProvenanceDecision {
                parents,
                subgraph_path: source_decision.subgraph_path.clone(),
                node: source_decision.node,
                operator: source_decision.operator,
                candidate: source_decision.candidate,
                outcome: source_decision.outcome,
            });
            remap.insert(source_handle, destination);
        }
        Ok(remap)
    }

    /// Resolves a compact handle.
    #[must_use]
    pub fn get(&self, handle: ProvenanceHandle) -> Option<&ProvenanceRecord> {
        self.records.get(handle.0 as usize)
    }

    #[must_use]
    pub fn decision(&self, handle: ProvenanceDecisionHandle) -> Option<&ProvenanceDecision> {
        self.decisions.get(handle.0 as usize)
    }

    /// Complete decision DAG in handle order.
    #[must_use]
    pub fn decisions(&self) -> &[ProvenanceDecision] {
        &self.decisions
    }

    /// Complete table in handle order.
    #[must_use]
    pub fn records(&self) -> &[ProvenanceRecord] {
        &self.records
    }
}

fn provenance_fragment_decision_order(
    source: &ProvenanceTable,
    selected: &BTreeSet<ProvenanceDecisionHandle>,
) -> Result<Vec<ProvenanceDecisionHandle>> {
    let mut reachable = BTreeSet::new();
    let mut pending = Vec::new();
    reserve_exact(
        &mut pending,
        source.decisions.len(),
        "provenance traversal pending decisions",
    )?;
    pending.extend(selected.iter().copied());
    reachable.extend(selected.iter().copied());
    while let Some(handle) = pending.pop() {
        let decision = source.decision(handle).ok_or_else(|| {
            invalid_provenance_reference(
                format!("decisions.{}", handle.0),
                "reachable decision is missing",
            )
        })?;
        pending.extend(
            decision
                .parents
                .iter()
                .copied()
                .filter(|parent| reachable.insert(*parent)),
        );
    }

    let mut unresolved_parents = BTreeMap::new();
    let mut child_edges = Vec::new();
    let parent_edges = reachable.iter().try_fold(0_usize, |total, handle| {
        source
            .decision(*handle)
            .ok_or_else(|| {
                invalid_provenance_reference(
                    format!("decisions.{}", handle.0),
                    "reachable decision is missing",
                )
            })?
            .parents
            .len()
            .checked_add(total)
            .ok_or(Error::NumericOverflow)
    })?;
    reserve_exact(
        &mut child_edges,
        parent_edges,
        "provenance traversal child edges",
    )?;
    for handle in &reachable {
        let decision = source.decision(*handle).ok_or_else(|| {
            invalid_provenance_reference(
                format!("decisions.{}", handle.0),
                "reachable decision is missing",
            )
        })?;
        let parents = decision.parents.iter().copied().collect::<BTreeSet<_>>();
        unresolved_parents.insert(*handle, parents.len());
        for parent in parents {
            child_edges.push((parent, *handle));
        }
    }
    child_edges.sort_unstable();
    child_edges.dedup();

    let mut ready = unresolved_parents
        .iter()
        .filter_map(|(handle, count)| (*count == 0).then_some(*handle))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::new();
    reserve_exact(
        &mut order,
        reachable.len(),
        "provenance traversal decision order",
    )?;
    while let Some(handle) = ready.pop_first() {
        order.push(handle);
        let start = child_edges.partition_point(|(parent, _)| *parent < handle);
        let end = child_edges.partition_point(|(parent, _)| *parent <= handle);
        for (_, dependent) in &child_edges[start..end] {
            let unresolved = unresolved_parents.get_mut(dependent).ok_or_else(|| {
                invalid_provenance_reference(
                    format!("decisions.{}", dependent.0),
                    "dependency accounting is incomplete",
                )
            })?;
            *unresolved = unresolved.checked_sub(1).ok_or_else(|| {
                invalid_provenance_reference(
                    format!("decisions.{}.parents", dependent.0),
                    "dependency accounting underflowed",
                )
            })?;
            if *unresolved == 0 {
                ready.insert(*dependent);
            }
        }
    }
    if order.len() != reachable.len() {
        return Err(invalid_provenance_reference(
            "decisions".to_owned(),
            "reachable decision ancestry contains a cycle",
        ));
    }
    Ok(order)
}

fn invalid_provenance_reference(path: String, reason: &'static str) -> Error {
    Error::GraphDocument {
        path: format!("provenance.{path}"),
        reason: reason.to_owned(),
    }
}

/// Coordinates in which a layer's authored inputs are expressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerCoordinateSpace {
    /// Exact world coordinates.
    #[default]
    World,
    /// Coordinates local to one stable surface provider.
    Surface,
    /// Coordinates local to a declared volume/spline owner.
    OwnerLocal,
}

/// Scalar/vector tile blending operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldBlendOperator {
    /// Replace the prior value inside the layer mask.
    #[default]
    Replace,
    /// Add values with checked fixed arithmetic.
    Add,
    /// Multiply values with checked fixed arithmetic.
    Multiply,
    /// Keep the smaller value.
    Minimum,
    /// Keep the larger value.
    Maximum,
}

/// Inclusion semantics for masks, volumes, splines, and blockers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InclusionOperator {
    /// Include candidates selected by the shape/field.
    #[default]
    Include,
    /// Exclude candidates selected by the shape/field.
    Exclude,
}

/// A typed field tile reference inside a sparse map chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldTileLayer {
    /// Shared surface/vegetation field channel.
    pub channel: FieldChannel,
    /// Stable tile-set identity in the map package.
    pub tile_set: u128,
    pub blend: FieldBlendOperator,
    /// Layer opacity/weight.
    pub weight: UnitInterval,
}

/// One family weight in a species-palette layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeciesWeight {
    pub family: Uuid,
    /// Canonical palette weight.
    pub weight: UnitInterval,
}

/// A world-space authored volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeLayer {
    /// Exact half-open volume bounds.
    pub bounds: WorldBounds,
    /// Include or exclude contents.
    pub operation: InclusionOperator,
    /// Soft edge distance in Q15.16 metres.
    pub falloff: DecisionScalar,
}

/// A world-space authored spline influence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplineLayer {
    pub spline: u128,
    /// Exact quantized world control points.
    pub points: Vec<WorldPosition>,
    /// Q15.16 influence radius.
    pub radius: DecisionScalar,
    /// Include or exclude contents.
    pub operation: InclusionOperator,
}

/// A persistent authored transform override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantTransformOverride {
    /// Target stable plant identity.
    pub plant: PlantId,
    /// Exact new position.
    pub position: WorldPosition,
    /// Quantized Euler-independent scale.
    pub scale: [DecisionScalar; 3],
}

/// A persistent authored state override.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantStateOverride {
    /// Target stable plant identity.
    pub plant: PlantId,
    /// Optional health replacement.
    pub health: Option<UnitInterval>,
    /// Optional moisture replacement.
    pub moisture: Option<UnitInterval>,
    /// Optional fuel replacement.
    pub fuel: Option<UnitInterval>,
    /// Optional interaction policy replacement.
    pub interaction_policy: Option<InteractionPolicy>,
}

/// Every authored vegetation-map operation in one algebra.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationLayerOperator {
    /// Quantized scalar field tiles.
    ScalarField(FieldTileLayer),
    /// Quantized vector field tiles.
    VectorField {
        channel: FieldChannel,
        tile_set: u128,
        /// Fixed vector added/replaced per tile sample.
        value: DecisionVec3,
        blend: FieldBlendOperator,
    },
    /// Family/community palette weights.
    SpeciesWeights(Vec<SpeciesWeight>),
    /// Density multiplier/addend field.
    Density(FieldTileLayer),
    /// Include/exclude mask tile.
    Mask {
        tile_set: u128,
        /// Inclusion semantics.
        operation: InclusionOperator,
    },
    /// Analytic authored volume.
    Volume(VolumeLayer),
    /// Authored spline influence.
    Spline(SplineLayer),
    /// Explicit authored plant anchors.
    Anchors(Vec<PlantId>),
    /// Procedural identities pinned across recooks.
    Pins(Vec<PlantId>),
    TransformOverrides(Vec<PlantTransformOverride>),
    /// Persistent authored biological/interaction overrides.
    StateOverrides(Vec<PlantStateOverride>),
    /// Signed blocker field with a typed category bitset.
    Blocker {
        tile_set: u128,
        /// Blocker categories affected by this layer.
        categories: u32,
    },
}

/// One stable ordered layer in a vegetation-map manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationLayer {
    /// Stable authored identity.
    pub id: u128,
    pub name: String,
    pub coordinate_space: LayerCoordinateSpace,
    /// Conservative exact bounds.
    pub bounds: WorldBounds,
    pub operator: VegetationLayerOperator,
    /// Stable IDs of layers/assets this operation reads.
    pub dependencies: Vec<u128>,
    /// Deterministic order, independent of container/UI order.
    pub order: i32,
    /// Locked against authoring changes.
    pub locked: bool,
    /// Muted from evaluation while retaining authored bytes.
    pub muted: bool,
    /// Monotonic authored revision.
    pub revision: u64,
}

impl VegetationLayer {
    /// Canonical evaluation key.
    #[must_use]
    pub fn order_key(&self) -> (i32, u128) {
        (self.order, self.id)
    }
}

/// An optional non-authoritative brush gesture record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrushGestureMetadata {
    /// Tool-local gesture identity.
    pub gesture: u128,
    /// Touched stable layer.
    pub layer: u128,
    /// Sampled world positions for UI replay only.
    pub samples: Vec<WorldPosition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(
        node: u128,
        parents: Vec<ProvenanceDecisionHandle>,
        outcome: ProvenanceDecisionOutcome,
    ) -> ProvenanceDecision {
        ProvenanceDecision {
            parents,
            subgraph_path: vec![node / 10],
            node,
            operator: GraphOperator::MacroOutput,
            candidate: node as u64,
            outcome,
        }
    }

    fn record(decision: ProvenanceDecisionHandle, candidate: u64) -> ProvenanceRecord {
        ProvenanceRecord {
            map: Uuid(1),
            layer: 2,
            biome: Uuid(3),
            decision,
            candidate,
            family: Some(Uuid(4)),
            plant: None,
            variation: 5,
        }
    }

    #[test]
    fn provenance_interns_exact_lineage_once() {
        let mut table = ProvenanceTable::default();
        let decision = ProvenanceDecision {
            parents: Vec::new(),
            subgraph_path: vec![4, 5],
            node: 6,
            operator: GraphOperator::MacroOutput,
            candidate: 7,
            outcome: ProvenanceDecisionOutcome::Accepted,
        };
        let decision_handle = table.intern_decision(decision.clone());
        assert_eq!(decision_handle, table.intern_decision(decision.clone()));
        let record = ProvenanceRecord {
            map: Uuid(1),
            layer: 2,
            biome: Uuid(3),
            decision: decision_handle,
            candidate: 7,
            family: Some(Uuid(8)),
            plant: Some(PlantId::explicit([9; 16]).unwrap()),
            variation: 9,
        };
        let a = table.intern(record.clone());
        let b = table.intern(record.clone());
        assert_eq!(a, b);
        assert_eq!(table.decisions(), &[decision]);
        assert_eq!(table.records(), &[record]);
    }

    #[test]
    fn provenance_fragment_import_is_order_independent_and_deduplicated() {
        let mut source = ProvenanceTable::default();
        let root = source.intern_decision(decision(
            10,
            Vec::new(),
            ProvenanceDecisionOutcome::Produced,
        ));
        let accepted_a = source.intern_decision(decision(
            20,
            vec![root],
            ProvenanceDecisionOutcome::Accepted,
        ));
        let accepted_b = source.intern_decision(decision(
            30,
            vec![root],
            ProvenanceDecisionOutcome::Accepted,
        ));
        let record_a = source.intern(record(accepted_a, 20));
        let record_b = source.intern(record(accepted_b, 30));

        let mut reverse_destination = ProvenanceTable::default();
        let reverse = reverse_destination
            .import_fragment(&source, &[record_b, record_a, record_b])
            .unwrap();
        let mut forward_destination = ProvenanceTable::default();
        let forward = forward_destination
            .import_fragment(&source, &[record_a, record_b])
            .unwrap();

        assert_eq!(reverse_destination, forward_destination);
        assert_eq!(reverse, forward);
        assert_eq!(reverse.decisions.len(), 3);
        assert_eq!(reverse.records.len(), 2);

        let unchanged = reverse_destination.clone();
        let repeated = reverse_destination
            .import_fragment(&source, &[record_a, record_b])
            .unwrap();
        assert_eq!(reverse_destination, unchanged);
        assert_eq!(repeated, reverse);
    }

    #[test]
    fn provenance_fragment_imports_nested_forward_parents_before_children() {
        let mut source = ProvenanceTable::default();
        let terminal = source.intern_decision(decision(
            40,
            vec![ProvenanceDecisionHandle(1), ProvenanceDecisionHandle(2)],
            ProvenanceDecisionOutcome::Accepted,
        ));
        let branch_a = source.intern_decision(decision(
            20,
            vec![ProvenanceDecisionHandle(3)],
            ProvenanceDecisionOutcome::Retained,
        ));
        let branch_b = source.intern_decision(decision(
            30,
            vec![ProvenanceDecisionHandle(3)],
            ProvenanceDecisionOutcome::Retained,
        ));
        let root = source.intern_decision(decision(
            10,
            Vec::new(),
            ProvenanceDecisionOutcome::Produced,
        ));
        let source_record = source.intern(record(terminal, 40));

        let mut destination = ProvenanceTable::default();
        destination.intern_decision(decision(
            99,
            Vec::new(),
            ProvenanceDecisionOutcome::Produced,
        ));
        let remap = destination
            .import_fragment(&source, &[source_record])
            .unwrap();

        let destination_root = remap.decisions[&root];
        let destination_branch_a = remap.decisions[&branch_a];
        let destination_branch_b = remap.decisions[&branch_b];
        let destination_terminal = remap.decisions[&terminal];
        assert_eq!(
            destination.decision(destination_branch_a).unwrap().parents,
            vec![destination_root]
        );
        assert_eq!(
            destination.decision(destination_branch_b).unwrap().parents,
            vec![destination_root]
        );
        assert_eq!(
            destination.decision(destination_terminal).unwrap().parents,
            vec![destination_branch_a, destination_branch_b]
        );
        assert_eq!(
            destination
                .get(remap.records[&source_record])
                .unwrap()
                .decision,
            destination_terminal
        );
        for decision in destination.decisions() {
            assert!(
                decision
                    .parents
                    .iter()
                    .all(|parent| destination.decision(*parent).is_some())
            );
        }
        assert!(
            destination
                .records()
                .iter()
                .all(|record| destination.decision(record.decision).is_some())
        );
    }

    #[test]
    fn provenance_decision_fragment_imports_candidate_lineage_without_records() {
        let mut source = ProvenanceTable::default();
        let produced = source.intern_decision(decision(
            10,
            Vec::new(),
            ProvenanceDecisionOutcome::Produced,
        ));
        let retained = source.intern_decision(decision(
            20,
            vec![produced],
            ProvenanceDecisionOutcome::Retained,
        ));
        let mut destination = ProvenanceTable::default();
        let remap = destination
            .import_decision_fragment(&source, &[retained])
            .unwrap();

        assert_eq!(remap.len(), 2);
        assert_eq!(
            destination.decision(remap[&retained]).unwrap().parents,
            vec![remap[&produced]]
        );
        assert!(destination.records().is_empty());
    }

    #[test]
    fn invalid_provenance_fragment_does_not_mutate_the_destination() {
        let mut source = ProvenanceTable::default();
        let terminal = source.intern_decision(decision(
            10,
            vec![ProvenanceDecisionHandle(99)],
            ProvenanceDecisionOutcome::Accepted,
        ));
        let source_record = source.intern(record(terminal, 10));
        let mut destination = ProvenanceTable::default();
        destination.intern_decision(decision(
            20,
            Vec::new(),
            ProvenanceDecisionOutcome::Produced,
        ));
        let unchanged = destination.clone();

        assert!(
            destination
                .import_fragment(&source, &[source_record])
                .is_err()
        );
        assert_eq!(destination, unchanged);
    }

    #[test]
    fn equal_order_uses_stable_layer_identity() {
        let bounds = WorldBounds::new([0; 3], [1; 3]).unwrap();
        let layer = |id| VegetationLayer {
            id,
            name: String::new(),
            coordinate_space: LayerCoordinateSpace::World,
            bounds,
            operator: VegetationLayerOperator::Anchors(Vec::new()),
            dependencies: Vec::new(),
            order: 3,
            locked: false,
            muted: false,
            revision: 0,
        };
        assert!(layer(1).order_key() < layer(2).order_key());
    }
}
