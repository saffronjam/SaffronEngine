//! Evaluation values, scopes, caches, and per-run state.

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, FieldChannel, FieldDerivative, WorldCellKey,
    WorldPosition, world_cells_covering_bounds,
};

use crate::graph::CompiledDemandSlice;
use crate::memory::{
    checked_memory_sum, requested_btree_bytes, requested_btree_with, requested_vec_bytes,
    requested_vec_with,
};
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphNode, Error, GraphComputeExecutor,
    GraphDomain, GraphExecutionPlan, GraphNodeAddress, PlantPoint, ProvenanceDecisionHandle,
    ProvenanceTable, QualifiedGraphPin, Result,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GraphValue {
    Scalar(ScalarFieldSamples),
    Vector(VectorFieldSamples),
    Hessian(HessianFieldSamples),
    Surface(ProjectedSurfaceSamples),
    Candidates(CandidateStream),
    Macro(Vec<PlantPoint>),
    Micro(Vec<MicroFieldTile>),
    Regions(Vec<EvaluationRegion>),
    Splines(Vec<EvaluationSpline>),
    Species(Vec<crate::BiomePaletteEntry>),
    Communities(CommunityTables),
    Diagnostics(Vec<NamedDiagnosticStream>),
}

impl GraphValue {
    pub(super) fn domain(&self) -> GraphDomain {
        match self {
            Self::Scalar(_) => GraphDomain::ScalarField,
            Self::Vector(_) => GraphDomain::VectorField,
            Self::Hessian(_) => GraphDomain::HessianField,
            Self::Surface(_) => GraphDomain::SurfaceField,
            Self::Candidates(_) => GraphDomain::Candidates,
            Self::Macro(_) => GraphDomain::MacroPoints,
            Self::Micro(_) => GraphDomain::MicroField,
            Self::Regions(_) => GraphDomain::Regions,
            Self::Splines(_) => GraphDomain::Splines,
            Self::Species(_) => GraphDomain::SpeciesTable,
            Self::Communities(_) => GraphDomain::CommunityTable,
            Self::Diagnostics(_) => GraphDomain::Diagnostics,
        }
    }

    pub(super) fn candidate_count(&self) -> usize {
        match self {
            Self::Candidates(stream) => stream.candidates.len(),
            Self::Surface(surface) => surface.values.len(),
            Self::Scalar(field) => field.values.len(),
            Self::Vector(field) => field.values.len(),
            Self::Hessian(field) => field.values.len(),
            Self::Macro(points) => points.len(),
            _ => 0,
        }
    }

    pub(super) fn requested_memory_bytes(&self) -> Result<u64> {
        match self {
            Self::Candidates(stream) => {
                requested_vec_bytes::<GraphCandidate>(stream.candidates.capacity())
            }
            Self::Surface(surface) => {
                requested_btree_with(&surface.values, |_| Ok(0), projected_surface_sample_memory)
            }
            Self::Scalar(field) => {
                requested_btree_bytes::<CandidateIdentity, DecisionScalar>(field.values.len())
            }
            Self::Vector(field) => {
                requested_btree_bytes::<CandidateIdentity, DecisionVec3>(field.values.len())
            }
            Self::Hessian(field) => {
                requested_btree_bytes::<CandidateIdentity, DecisionHessian3>(field.values.len())
            }
            Self::Macro(points) => requested_vec_bytes::<PlantPoint>(points.capacity()),
            Self::Micro(tiles) => requested_vec_with(tiles, micro_field_tile_memory),
            Self::Regions(regions) => requested_vec_bytes::<EvaluationRegion>(regions.capacity()),
            Self::Splines(splines) => requested_vec_with(splines, |spline| {
                requested_vec_bytes::<WorldPosition>(spline.points.capacity())
            }),
            Self::Species(species) => {
                requested_vec_bytes::<crate::BiomePaletteEntry>(species.capacity())
            }
            Self::Communities(communities) => communities.requested_memory_bytes(),
            Self::Diagnostics(streams) => requested_vec_with(streams, diagnostic_stream_memory),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct GlobalStageCacheKey {
    pub(super) stage: [u8; 32],
    pub(super) map: u64,
    pub(super) biome_instance: u128,
    pub(super) owner: WorldCellKey,
    pub(super) input_snapshot: [u8; 32],
}

#[derive(Clone)]
pub(super) struct GlobalStageTile {
    pub(super) key: GlobalStageCacheKey,
    pub(super) outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    pub(super) provenance: ProvenanceTable,
    pub(super) candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
    pub(super) resident_bytes: u64,
}

pub(super) struct PlannedEvaluation {
    pub(super) result: GraphEvaluationResult,
    pub(super) materialized_outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    pub(super) candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
}

#[derive(Default)]
pub(super) struct GlobalStageStore {
    pub(super) tiles: BTreeMap<GlobalStageCacheKey, GlobalStageTile>,
    by_owner: BTreeMap<([u8; 32], WorldCellKey), GlobalStageCacheKey>,
}

impl GlobalStageStore {
    pub(super) fn insert(&mut self, tile: GlobalStageTile) -> Result<()> {
        if tile.key.input_snapshot == [0; 32] {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "a global-stage tile has no immutable input identity".to_owned(),
            });
        }
        let owner_key = (tile.key.stage, tile.key.owner);
        if self.by_owner.contains_key(&owner_key) || self.tiles.contains_key(&tile.key) {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "a global-stage owner tile was produced more than once".to_owned(),
            });
        }
        self.by_owner.insert(owner_key, tile.key.clone());
        self.tiles.insert(tile.key.clone(), tile);
        Ok(())
    }

    pub(super) fn tile(&self, stage: [u8; 32], owner: WorldCellKey) -> Result<&GlobalStageTile> {
        self.by_owner
            .get(&(stage, owner))
            .and_then(|key| self.tiles.get(key))
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: 0,
                input: format!(
                    "materialized global stage {} owner {owner}",
                    hex_hash(stage)
                ),
            })
    }

    pub(super) fn resident_bytes(&self) -> Result<u64> {
        self.tiles.values().try_fold(
            checked_memory_sum([
                requested_btree_bytes::<GlobalStageCacheKey, GlobalStageTile>(self.tiles.len())?,
                requested_btree_bytes::<([u8; 32], WorldCellKey), GlobalStageCacheKey>(
                    self.by_owner.len(),
                )?,
            ])?,
            |total, tile| {
                total
                    .checked_add(tile.resident_bytes)
                    .ok_or(Error::NumericOverflow)
            },
        )
    }
}

#[derive(Clone, Copy)]
pub(super) enum EvaluationScope<'a> {
    Cell {
        global_store: &'a GlobalStageStore,
    },
    Global {
        stage: &'a CompiledGlobalStage,
        global_store: &'a GlobalStageStore,
    },
}

impl<'a> EvaluationScope<'a> {
    pub(super) fn global_store(self) -> &'a GlobalStageStore {
        match self {
            Self::Cell { global_store } | Self::Global { global_store, .. } => global_store,
        }
    }

    pub(super) fn current_global_stage(self) -> Option<&'a CompiledGlobalStage> {
        match self {
            Self::Cell { .. } => None,
            Self::Global { stage, .. } => Some(stage),
        }
    }

    pub(super) fn is_cell(self) -> bool {
        matches!(self, Self::Cell { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CommunityTables {
    pub(super) competition: Vec<crate::CompetitionRule>,
    pub(super) companions: Vec<crate::CompanionRule>,
    pub(super) succession: Vec<crate::SuccessionRule>,
}

impl CommunityTables {
    pub(super) fn canonicalize(&mut self) {
        self.competition.sort_unstable_by_key(|rule| {
            (
                rule.first.value(),
                rule.second.value(),
                rule.spacing,
                rule.priority,
            )
        });
        self.companions.sort_unstable_by_key(|rule| {
            (
                rule.parent.value(),
                rule.child.value(),
                rule.minimum_distance,
                rule.maximum_distance,
                rule.probability,
            )
        });
        self.succession.sort_unstable_by_key(|rule| {
            (
                rule.minimum_tick,
                rule.from.value(),
                rule.to.value(),
                rule.probability,
            )
        });
    }

    pub(super) fn requested_memory_bytes(&self) -> Result<u64> {
        checked_memory_sum([
            requested_vec_bytes::<crate::CompetitionRule>(self.competition.capacity())?,
            requested_vec_bytes::<crate::CompanionRule>(self.companions.capacity())?,
            requested_vec_bytes::<crate::SuccessionRule>(self.succession.capacity())?,
        ])
    }
}

pub(super) type SurfaceProjectionCacheKey = (u128, u32, [u8; 32]);
type SurfaceProjectionCache = BTreeMap<
    SurfaceProjectionCacheKey,
    BTreeMap<WorldPosition, Option<QuantizedSurfaceProjectionSample>>,
>;
pub(super) type SurfaceFieldQueryCacheKey = (u128, u32, FieldChannel, FieldDerivative, [u8; 32]);
type SurfaceFieldQueryCache = BTreeMap<
    SurfaceFieldQueryCacheKey,
    BTreeMap<(CandidateIdentity, WorldPosition), QuantizedSurfaceFieldValue>,
>;

#[derive(Clone, Copy)]
pub(super) struct RandomSampleAddress {
    pub(super) cell: WorldCellKey,
    pub(super) candidate: u64,
    pub(super) ancestor: u64,
    pub(super) species: u128,
    pub(super) channel: u32,
}

impl RandomSampleAddress {
    pub(super) fn new(cell: WorldCellKey, candidate: u64) -> Self {
        Self {
            cell,
            candidate,
            ancestor: 0,
            species: 0,
            channel: 0,
        }
    }

    pub(super) fn with_ancestor(mut self, ancestor: u64) -> Self {
        self.ancestor = ancestor;
        self
    }

    pub(super) fn with_species(mut self, species: u128) -> Self {
        self.species = species;
        self
    }

    pub(super) fn with_channel(mut self, channel: u32) -> Self {
        self.channel = channel;
        self
    }
}

pub(super) struct EvaluationState<'a> {
    pub(super) graph: &'a CompiledBiomeGraph,
    pub(super) inputs: &'a GraphEvaluationInputs,
    pub(super) cancellation: &'a GraphCancellationToken,
    pub(super) compute: Option<&'a dyn GraphComputeExecutor>,
    pub(super) execution_plan: &'a GraphExecutionPlan,
    pub(super) scope: EvaluationScope<'a>,
    pub(super) pass: EvaluationPass,
    pub(super) deadline: Instant,
    pub(super) provenance: ProvenanceTable,
    pub(super) candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
    pub(super) prepared_surface_projections: SurfaceProjectionCache,
    pub(super) prepared_surface_fields: SurfaceFieldQueryCache,
    pub(super) ancestor_references: BTreeSet<WorldCellKey>,
    pub(super) transferred_bytes: u64,
    pub(super) diagnostics: GraphEvaluationDiagnostics,
    pub(super) rejected_by_lineage: BTreeMap<CandidateLineage, Vec<RejectedCandidate>>,
    pub(super) materialized_outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    pub(super) current_node_live_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EvaluationPass {
    PrepareCanonicalInputs,
    AuthoritativeReplay,
}

#[derive(Clone, Copy)]
pub(super) struct EvaluationContext<'a> {
    pub(super) graph: &'a CompiledBiomeGraph,
    pub(super) cancellation: &'a GraphCancellationToken,
    pub(super) compute: Option<&'a dyn GraphComputeExecutor>,
    pub(super) execution_plan: &'a GraphExecutionPlan,
    pub(super) scope: EvaluationScope<'a>,
    pub(super) deadline: Instant,
}

impl EvaluationState<'_> {
    pub(super) fn check_transient_memory(&self, additional_bytes: u64) -> Result<()> {
        let requested = self
            .current_node_live_bytes
            .checked_add(additional_bytes)
            .ok_or(Error::NumericOverflow)?;
        self.check_count(
            "memory bytes",
            requested,
            self.graph.limits.max_memory_bytes,
        )
    }

    pub(super) fn should_load_global_node(&self, address: &GraphNodeAddress) -> bool {
        let Some(owner) = self.graph.spatial_plan().global_stage_for_node(address) else {
            return false;
        };
        self.scope
            .current_global_stage()
            .is_none_or(|current| current.id != owner.id)
    }

    pub(super) fn load_global_node_outputs(
        &mut self,
        node: &CompiledGraphNode,
        demand: &CompiledDemandSlice,
    ) -> Result<BTreeMap<String, GraphValue>> {
        let address = node.address();
        let stage = self
            .graph
            .spatial_plan()
            .global_stage_for_node(&address)
            .ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "global node is absent from the compiled spatial plan".to_owned(),
            })?;
        let owners = world_cells_covering_bounds(
            self.inputs.read_bounds,
            stage.owner_level,
            self.graph.limits.max_global_stage_tiles,
        )?;
        let global_store = self.scope.global_store();
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
            let mut merged: Option<GraphValue> = None;
            for owner in &owners {
                let tile = global_store.tile(stage.id, *owner)?;
                let value = tile.outputs.get(&pin).ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!(
                        "materialized global stage omitted boundary pin '{}'",
                        output.name
                    ),
                })?;
                let candidate_identities = graph_value_candidate_identity_upper_bound(value)?;
                let import_scratch = global_import_scratch_bytes(
                    candidate_identities,
                    tile.provenance.decisions().len() as u64,
                    tile.provenance.records().len() as u64,
                    tile.provenance.requested_memory_bytes()?,
                )?;
                self.check_transient_memory(checked_memory_sum([
                    value.requested_memory_bytes()?,
                    import_scratch,
                ])?)?;
                let value = import_global_value(value.clone(), tile, self)?;
                if let Some(current) = merged.as_ref() {
                    self.check_transient_memory(checked_memory_sum([
                        current.requested_memory_bytes()?,
                        value.requested_memory_bytes()?,
                        graph_value_merge_scratch_bytes(current, &value)?,
                    ])?)?;
                }
                merge_graph_value(&mut merged, value, node)?;
            }
            let value = merged.ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!(
                    "global stage {} output '{}'",
                    hex_hash(stage.id),
                    output.name
                ),
            })?;
            outputs.insert(output.name.clone(), value);
        }
        Ok(outputs)
    }

    pub(super) fn capture_global_outputs(
        &mut self,
        node: &CompiledGraphNode,
        outputs: &BTreeMap<String, GraphValue>,
    ) -> Result<()> {
        let Some(stage) = self.scope.current_global_stage() else {
            return Ok(());
        };
        let address = node.address();
        for output in &stage.output_pins {
            if output.node != address {
                continue;
            }
            let value = outputs
                .get(&output.pin)
                .cloned()
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!("global boundary output '{}' is missing", output.pin),
                })?;
            let value = clip_global_value(value, self.inputs.output_bounds)?;
            self.materialized_outputs.insert(output.clone(), value);
        }
        Ok(())
    }

    pub(super) fn capture_resident_global_outputs(
        &mut self,
        node: &CompiledGraphNode,
        outputs: &BTreeMap<(u128, String), GraphValue>,
    ) -> Result<()> {
        let Some(stage) = self.scope.current_global_stage() else {
            return Ok(());
        };
        let address = node.address();
        for output in &stage.output_pins {
            if output.node != address {
                continue;
            }
            let value = outputs
                .iter()
                .find(|((source, pin), _)| *source == node.definition.guid && pin == &output.pin)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!("global boundary output '{}' is missing", output.pin),
                })?;
            let value = clip_global_value(value, self.inputs.output_bounds)?;
            self.materialized_outputs.insert(output.clone(), value);
        }
        Ok(())
    }

    pub(super) fn check_abort(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: self.graph.limits.max_time_ms.saturating_add(1),
                limit: self.graph.limits.max_time_ms,
            });
        }
        Ok(())
    }

    pub(super) fn check_count(
        &self,
        resource: &'static str,
        requested: u64,
        limit: u64,
    ) -> Result<()> {
        if requested > limit {
            return Err(Error::GraphLimit {
                resource,
                requested,
                limit,
            });
        }
        Ok(())
    }
}
