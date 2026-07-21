//! Canonical reference and parallel biome-graph evaluation.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use saffron_core::Uuid;
use saffron_geometry::glam::DVec3;
use saffron_spatial::{
    BASE_CELL_TICKS, DecisionCurve, DecisionHessian3, DecisionScalar, DecisionVec3,
    FieldAvailability, FieldChannel, FieldDerivative, LOCAL_TICKS_PER_METER, MAX_HIERARCHY_LEVEL,
    RandomDomain, RandomStream, SignedUnit, SurfaceAttachment, SurfaceField, SurfaceHit,
    SurfaceProjection, SurfaceProviderDescriptor, SurfaceProviderId, SurfaceRevision,
    SurfaceTileDescriptor, UnitInterval, WeightedSurfaceTag, WorldBounds, WorldCellKey,
    WorldPosition, div_round_ties_even, world_cell_count_covering_bounds,
    world_cells_covering_bounds,
};

use crate::binary::BinaryReader;
use crate::canonical::{ByteSink, CanonicalSink, CountSink};
use crate::graph::{CompiledDemandSlice, CompiledDemandUnitSlice};
use crate::hash::{VegetationContentHasher, sha256};
use crate::memory::{
    ALLOCATION_OVERHEAD_BYTES, checked_memory_sum as sum_memory_bytes,
    requested_btree_bytes as memory_requested_btree_bytes,
    requested_btree_bytes_for_len as memory_requested_btree_bound, requested_btree_with,
    requested_string_bytes, requested_vec_bytes as memory_requested_vec_bytes,
    requested_vec_bytes_for_len, requested_vec_bytes_for_len as memory_requested_vec_bytes_for_len,
    requested_vec_with,
};
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphNode, CompiledGraphUnit, Error,
    FieldBlendOperator, GRAPH_GPU_INSTRUCTION_WORDS, GRAPH_GPU_INVOCATION_WORDS,
    GRAPH_GPU_MAX_CURVE_POINTS, GRAPH_GPU_MAX_INSTRUCTIONS, GRAPH_GPU_OUTPUT_WORDS,
    GRAPH_GPU_PROGRAM_HEADER_WORDS, GraphAuthority, GraphClusterMode, GraphCombineOperation,
    GraphComputeExecutor, GraphDependencySource, GraphDistanceSource, GraphDomain,
    GraphExecutionBoundary, GraphExecutionDomain, GraphExecutionGroup, GraphExecutionNode,
    GraphExecutionPlan, GraphGpuInstruction, GraphGpuInvocationBatch, GraphGpuProgram,
    GraphGpuRegister, GraphGpuRegisterType, GraphGpuScheduling, GraphGpuValue, GraphNodeAddress,
    GraphOperator, GraphParameterValue, IdentityConflictReport, InteractionPolicy, PlantFlags,
    PlantId, PlantLifecycle, PlantPoint, PlantPointColumns, ProceduralPlantIdentity,
    ProvenanceDecision, ProvenanceDecisionHandle, ProvenanceDecisionOutcome, ProvenanceHandle,
    ProvenanceRecord, ProvenanceTable, QualifiedGraphPin, QuantizedOrientation, Result,
    VegetationCellSection, VegetationCellSectionKind, build_execution_plan, identity_conflicts,
};

const EVALUATOR_WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;
const HIERARCHY_LEVEL_COUNT: usize = MAX_HIERARCHY_LEVEL as usize + 1;

/// Stable identity assigned before acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidateIdentity {
    /// Node that created the candidate.
    pub node: u128,
    /// Fully qualified root-biome/module-path/node execution address.
    pub node_address: u128,
    /// Semantic revision of the node that created the candidate.
    pub node_semantic_revision: u32,
    /// Stable ordinal in that node's namespace.
    pub ordinal: u64,
    /// Parent/ancestor candidate ordinal for hierarchical placement.
    pub ancestor: u64,
}

/// Identity material needed to derive a referenced candidate's stable plant ID in any cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateReference {
    /// Pre-acceptance identity.
    pub identity: CandidateIdentity,
    /// Canonical owner cell.
    pub owner: WorldCellKey,
    /// Stable authored layer that owns the candidate lineage.
    pub source_layer: u128,
    /// Family already assigned by an upstream community decision.
    pub family: Option<Uuid>,
    /// Selected family variation.
    pub variation: u32,
    /// Authored identity when the source is an explicit plant.
    pub authored_id: Option<PlantId>,
}

impl CandidateReference {
    fn from_candidate(candidate: &GraphCandidate) -> Self {
        Self {
            identity: candidate.identity,
            owner: candidate.owner,
            source_layer: candidate.source_layer,
            family: candidate.family,
            variation: candidate.variation,
            authored_id: candidate.authored_point.as_ref().map(|point| point.id),
        }
    }
}

/// One candidate carried between graph stages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphCandidate {
    /// Pre-acceptance identity.
    pub identity: CandidateIdentity,
    /// Canonical owner cell.
    pub owner: WorldCellKey,
    /// Stable authored layer that owns this candidate lineage.
    pub source_layer: u128,
    /// Exact quantized position.
    pub position: WorldPosition,
    /// Quantized orientation.
    pub orientation: QuantizedOrientation,
    /// Q15.16 scale.
    pub scale: [DecisionScalar; 3],
    /// Selected plant family, when assigned.
    pub family: Option<Uuid>,
    /// Selected family variation.
    pub variation: u32,
    /// Parent candidate identity.
    pub parent: Option<CandidateReference>,
    /// Colony root candidate identity.
    pub colony: Option<CandidateReference>,
    /// Stable deterministic priority.
    pub priority: DecisionScalar,
    /// Read-only ecology snapshot tick carried into succession decisions.
    pub ecology_tick: u64,
    /// Crown spacing radius in Q15.16 metres.
    pub crown_radius: DecisionScalar,
    /// Root spacing radius in Q15.16 metres.
    pub root_radius: DecisionScalar,
    /// Canonical surface attachment after projection.
    pub attachment: Option<SurfaceAttachment>,
    /// Quantized surface normal, when projected.
    pub surface_normal: Option<[SignedUnit; 3]>,
    /// Canonical provider-local projection coordinate.
    pub surface_projection: [DecisionScalar; 3],
    /// Complete authored source row when this candidate is an explicit anchor.
    pub authored_point: Option<PlantPoint>,
}

/// Candidate stream in canonical identity order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidateLineage(pub u128);

/// Candidate stream in canonical identity order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateStream {
    /// Identity of the candidate-generation or expansion stage that owns this stream.
    pub lineage: CandidateLineage,
    /// Sorted unique candidates.
    pub candidates: Vec<GraphCandidate>,
}

impl CandidateStream {
    fn canonicalize(&mut self) -> Result<()> {
        self.candidates
            .sort_unstable_by_key(|candidate| candidate.identity);
        if self
            .candidates
            .windows(2)
            .any(|pair| pair[0].identity == pair[1].identity)
        {
            return Err(Error::Mutation(
                "graph candidate identity was duplicated".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Canonical scalar samples keyed by candidate identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarFieldSamples {
    /// Candidate stream for which these samples were evaluated.
    pub lineage: CandidateLineage,
    /// Exact value per candidate.
    pub values: BTreeMap<CandidateIdentity, DecisionScalar>,
}

/// Canonical vector samples keyed by candidate identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorFieldSamples {
    /// Candidate stream for which these samples were evaluated.
    pub lineage: CandidateLineage,
    /// Exact value per candidate.
    pub values: BTreeMap<CandidateIdentity, DecisionVec3>,
}

/// Canonical Hessian samples keyed by candidate identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HessianFieldSamples {
    /// Candidate stream for which these samples were evaluated.
    pub lineage: CandidateLineage,
    /// Exact symmetric Hessian per candidate.
    pub values: BTreeMap<CandidateIdentity, DecisionHessian3>,
}

/// Canonical projected surface data keyed by candidate identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedSurfaceSamples {
    /// Candidate stream for which these samples were evaluated.
    pub lineage: CandidateLineage,
    /// Exact projected samples.
    pub values: BTreeMap<CandidateIdentity, ProjectedSurfaceSample>,
}

/// One quantized projected surface sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedSurfaceSample {
    /// Exact projected position.
    pub position: WorldPosition,
    /// Stable attachment.
    pub attachment: Option<SurfaceAttachment>,
    /// Quantized normal.
    pub normal: [SignedUnit; 3],
    /// Canonical provider-local projection coordinate.
    pub projection: [DecisionScalar; 3],
    /// Canonical weighted tags.
    pub tags: Vec<WeightedSurfaceTag>,
}

/// One exact authored or analytic region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvaluationRegion {
    /// Stable region identity.
    pub id: u128,
    /// Semantic role of this region.
    pub kind: EvaluationRegionKind,
    /// Stable authored layer owning the region.
    pub layer: u128,
    /// Namespace shared by cell fragments of one hierarchical region.
    pub hierarchy_namespace: Option<u128>,
    /// Stable cell key used for every random stream originating in this region.
    pub seed_cell: WorldCellKey,
    /// Exact half-open world bounds.
    pub bounds: WorldBounds,
}

/// Closed semantic role for exact evaluation regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluationRegionKind {
    /// Root biome coverage used by candidate generators.
    Biome,
    /// Authored analytic shape used by shape-distance nodes.
    Shape,
}

/// One exact quantized spline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationSpline {
    /// Stable spline identity.
    pub id: u128,
    /// Stable authored layer owning the spline.
    pub layer: u128,
    /// Ordered exact world points.
    pub points: Vec<WorldPosition>,
}

/// Immutable plant-family geometry and ecological metadata resolved before evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantPrototype {
    /// Plant-family asset identity.
    pub family: Uuid,
    /// Crown radius along local X/Z.
    pub crown_radius: [DecisionScalar; 2],
    /// Root radius along local X/Z.
    pub root_radius: [DecisionScalar; 2],
    /// Conservative family-local bounds.
    pub local_bounds_min: [DecisionScalar; 3],
    /// Conservative family-local bounds maximum.
    pub local_bounds_max: [DecisionScalar; 3],
    /// Closed shade amount tolerated without reducing community weight.
    pub shade_tolerance: UnitInterval,
}

impl PlantPrototype {
    /// Resolves the graph-facing prototype from the authoritative plant-family asset.
    pub fn from_family(asset: &crate::PlantFamilyAsset) -> Result<Self> {
        crate::validate_plant_family(asset)?;
        let prototype = Self {
            family: asset.id,
            crown_radius: asset.dimensions.crown_radius,
            root_radius: asset.dimensions.root_radius,
            local_bounds_min: asset.dimensions.local_bounds_min,
            local_bounds_max: asset.dimensions.local_bounds_max,
            shade_tolerance: asset
                .habitat
                .as_ref()
                .map_or(UnitInterval::ZERO, |habitat| habitat.shade_tolerance),
        };
        prototype.validate()?;
        Ok(prototype)
    }

    /// Validates positive footprints and a non-empty local bound.
    pub fn validate(self) -> Result<()> {
        if self.family.value() == 0
            || self
                .crown_radius
                .iter()
                .chain(&self.root_radius)
                .any(|value| value.bits() <= 0)
            || (0..3).any(|axis| self.local_bounds_min[axis] >= self.local_bounds_max[axis])
        {
            return Err(Error::GraphDocument {
                path: "evaluation.plantPrototypes".to_owned(),
                reason: "prototype identity, footprint, or local bounds are invalid".to_owned(),
            });
        }
        Ok(())
    }
}

/// One sparse authored scalar tile available to graph evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationFieldTile {
    /// Immutable source of the quantized tile.
    pub source: EvaluationFieldSource,
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Value, gradient, or Hessian sample semantics.
    pub derivative: FieldDerivative,
    /// Canonical ordered-layer blend operation.
    pub blend: FieldBlendOperator,
    /// Canonical ordered-layer weight.
    pub weight: UnitInterval,
    /// Stable ordered-layer key.
    pub layer_order: (i32, u128),
    /// Exact source content identity.
    pub source_hash: [u8; 32],
    /// Exact covered bounds.
    pub bounds: WorldBounds,
    /// Packed sample dimensions.
    pub dimensions: [u32; 3],
    /// Typed Q15.16 values in X-major, then Y, then Z order.
    pub values: QuantizedFieldTileValues,
}

/// Packed canonical field values whose lane shape matches the derivative domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuantizedFieldTileValues {
    /// Scalar values.
    Scalar(Vec<i32>),
    /// Three-lane gradients.
    Gradient(Vec<[i32; 3]>),
    /// Symmetric Hessians in `xx, xy, xz, yy, yz, zz` order.
    Hessian(Vec<[i32; 6]>),
}

/// One exact quantized surface-field value prepared for an authoritative candidate query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantizedSurfaceFieldValue {
    /// Scalar value sample.
    Scalar(i32),
    /// Three-lane gradient sample.
    Gradient([i32; 3]),
    /// Symmetric Hessian sample in `xx, xy, xz, yy, yz, zz` order.
    Hessian([i32; 6]),
}

/// One exact candidate-position lookup in a canonical surface-field query tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceFieldQueryEntry {
    /// Stable candidate identity whose field value was sampled.
    pub candidate: CandidateIdentity,
    /// Exact query position used during canonical preparation.
    pub query: WorldPosition,
    /// Quantized typed field value.
    pub value: QuantizedSurfaceFieldValue,
}

/// Canonical exact-query surface-field data consumed by authoritative replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceFieldQueryTile {
    /// Field-sample node GUID.
    pub node: u128,
    /// Exact node semantic revision.
    pub node_semantic_revision: u32,
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Requested derivative domain.
    pub derivative: FieldDerivative,
    /// Sorted exact candidate queries.
    pub samples: Vec<QuantizedSurfaceFieldQueryEntry>,
    /// Content identity of the complete provider set used for preparation.
    pub provider_set_hash: [u8; 32],
}

impl QuantizedSurfaceFieldQueryTile {
    fn sample(
        &self,
        candidate: CandidateIdentity,
        query: WorldPosition,
    ) -> Option<QuantizedSurfaceFieldValue> {
        let index = self
            .samples
            .binary_search_by_key(&(candidate, query), |entry| (entry.candidate, entry.query))
            .ok()?;
        self.samples.get(index).map(|entry| entry.value)
    }
}

/// Immutable source identity for a canonical quantized field tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvaluationFieldSource {
    /// Sparse authored map layer.
    MapLayer(u128),
    /// Canonical surface-provider tile at an exact revision.
    SurfaceProvider {
        /// Stable provider identity.
        provider: SurfaceProviderId,
        /// Exact provider revision used to precompute the tile.
        revision: SurfaceRevision,
    },
}

/// One canonical precomputed projection sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceProjectionSample {
    /// Exact projected point.
    pub position: WorldPosition,
    /// Stable provider attachment.
    pub attachment: SurfaceAttachment,
    /// Quantized geometric normal.
    pub normal: [SignedUnit; 3],
    /// Canonical provider-local projection coordinate.
    pub projection: [DecisionScalar; 3],
    /// Canonical weighted tags.
    pub tags: Vec<WeightedSurfaceTag>,
}

/// Runtime-authoritative, node-specific quantized surface projection tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceProjectionTile {
    /// Surface-projection node GUID.
    pub node: u128,
    /// Exact node semantic revision.
    pub node_semantic_revision: u32,
    /// Sorted exact query positions and their optional projection results.
    pub samples: Vec<QuantizedSurfaceProjectionEntry>,
    /// Content identity of the complete provider set used for precomputation.
    pub provider_set_hash: [u8; 32],
}

/// One exact authoritative projection query and its quantized result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceProjectionEntry {
    /// Exact candidate position submitted to the surface query.
    pub query: WorldPosition,
    /// Quantized hit, or `None` when the exact query missed every eligible provider.
    pub sample: Option<QuantizedSurfaceProjectionSample>,
}

/// Computes the canonical immutable identity of a complete surface-provider set.
pub fn canonical_surface_provider_set_hash(
    providers: &[Arc<dyn SurfaceField>],
    max_providers: u64,
) -> Result<[u8; 32]> {
    canonical_surface_provider_set_hash_guarded(providers, max_providers, None)
}

fn canonical_surface_provider_set_hash_guarded(
    providers: &[Arc<dyn SurfaceField>],
    max_providers: u64,
    guard: Option<PreflightGuard<'_>>,
) -> Result<[u8; 32]> {
    if providers.is_empty() {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviders".to_owned(),
            reason: "surface provider set cannot be empty".to_owned(),
        });
    }
    check_limit("input tiles", providers.len() as u64, max_providers)?;
    let mut descriptors = Vec::new();
    crate::memory::reserve_exact(
        &mut descriptors,
        providers.len(),
        "surface provider descriptors",
    )?;
    for provider in providers {
        if let Some(guard) = guard {
            guard.check()?;
        }
        descriptors.push(provider.descriptor());
    }
    descriptors.sort_unstable_by_key(|descriptor| descriptor.id);
    for pair in descriptors.windows(2) {
        if let Some(guard) = guard {
            guard.check()?;
        }
        if pair[0].id == pair[1].id {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProviders".to_owned(),
                reason: "surface provider identities must be unique".to_owned(),
            });
        }
    }
    let mut hasher = VegetationContentHasher::new();
    hasher.update(b"saffron-anima/surface-provider-set/v1\0")?;
    for descriptor in descriptors {
        if let Some(guard) = guard {
            guard.check()?;
        }
        hasher.update(&descriptor.id.0.to_be_bytes())?;
        hasher.update(&descriptor.revision.0.to_be_bytes())?;
        for value in descriptor.bounds.min_ticks() {
            hasher.update(&value.to_be_bytes())?;
        }
        for value in descriptor.bounds.max_ticks_exclusive() {
            hasher.update(&value.to_be_bytes())?;
        }
        hasher.update(&descriptor.primitive_count.to_be_bytes())?;
        hasher.update(&descriptor.max_tags_per_hit.to_be_bytes())?;
        let capabilities = descriptor.capabilities;
        hasher.update(&[
            u8::from(capabilities.ray),
            u8::from(capabilities.project),
            u8::from(capabilities.nearest),
            u8::from(capabilities.uv),
            u8::from(capabilities.authoritative_attachments),
            u8::from(capabilities.authoritative_fields),
        ])?;
    }
    hasher.finalize()
}

impl QuantizedSurfaceProjectionTile {
    /// Validates exact query ordering, tags, and node identity.
    pub fn validate(&self) -> Result<()> {
        if self.node == 0
            || self.node_semantic_revision == 0
            || self.provider_set_hash == [0; 32]
            || self
                .samples
                .windows(2)
                .any(|pair| pair[0].query >= pair[1].query)
        {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile identity or exact query ordering is invalid".to_owned(),
            });
        }
        for sample in self
            .samples
            .iter()
            .filter_map(|entry| entry.sample.as_ref())
        {
            if sample
                .tags
                .windows(2)
                .any(|pair| pair[0].tag >= pair[1].tag)
            {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceProjectionTiles.tags".to_owned(),
                    reason: "projection sample tags must be sorted and unique".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn sample(&self, position: WorldPosition) -> Option<&Option<QuantizedSurfaceProjectionSample>> {
        let index = self
            .samples
            .binary_search_by_key(&position, |entry| entry.query)
            .ok()?;
        self.samples.get(index).map(|entry| &entry.sample)
    }
}

/// Precomputes exact authoritative projection queries for one surface-projection node.
pub fn precompute_surface_projection_tile(
    node: &CompiledGraphNode,
    queries: &[WorldPosition],
    provider_set_hash: [u8; 32],
    providers: &[Arc<dyn SurfaceField>],
    cancellation: &GraphCancellationToken,
) -> Result<QuantizedSurfaceProjectionTile> {
    if node.definition.operator != GraphOperator::SurfaceProjection || provider_set_hash == [0; 32]
    {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "surface projection precompute requires a projection node and provider hash"
                .to_owned(),
        });
    }
    let mut canonical_queries = Vec::new();
    crate::memory::reserve_exact(
        &mut canonical_queries,
        queries.len(),
        "surface projection queries",
    )?;
    canonical_queries.extend_from_slice(queries);
    canonical_queries.sort_unstable();
    canonical_queries.dedup();
    let mut samples = Vec::new();
    crate::memory::reserve_exact(
        &mut samples,
        canonical_queries.len(),
        "surface projection samples",
    )?;
    for position in canonical_queries {
        if cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        let hit = select_surface_hit(node, position, providers, true)?;
        let sample = hit
            .map(|hit| quantize_authoritative_surface_hit(node, hit))
            .transpose()?;
        samples.push(QuantizedSurfaceProjectionEntry {
            query: position,
            sample,
        });
    }
    let tile = QuantizedSurfaceProjectionTile {
        node: node.definition.guid,
        node_semantic_revision: node.definition.semantic_revision,
        samples,
        provider_set_hash,
    };
    tile.validate()?;
    Ok(tile)
}

fn quantize_authoritative_surface_hit(
    node: &CompiledGraphNode,
    hit: SurfaceHit,
) -> Result<QuantizedSurfaceProjectionSample> {
    let attachment = hit
        .attachment
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: format!("surface attachment for provider {}", hit.provider.0),
        })?;
    let normal = quantize_surface_normal(hit.frame.normal.to_array())?;
    let projection = quantize_surface_projection(hit.coordinates.projection.to_array())?;
    validate_canonical_tags(&hit.tags)?;
    Ok(QuantizedSurfaceProjectionSample {
        position: hit.position,
        attachment,
        normal,
        projection,
        tags: hit.tags,
    })
}

/// Precomputes one canonical typed field tile from a surface provider descriptor.
pub fn precompute_surface_field_tile(
    provider: &dyn SurfaceField,
    descriptor: SurfaceTileDescriptor,
    channel: FieldChannel,
    derivative: FieldDerivative,
    source_hash: [u8; 32],
    cancellation: &GraphCancellationToken,
) -> Result<EvaluationFieldTile> {
    let provider_descriptor = provider.descriptor();
    if !provider_descriptor.capabilities.authoritative_fields
        || provider_descriptor.id != descriptor.provider
        || provider_descriptor.revision != descriptor.revision
        || source_hash == [0; 32]
    {
        return Err(Error::GraphDocument {
            path: "surfaceFieldPrecompute".to_owned(),
            reason: "provider descriptor or canonical content identity does not match".to_owned(),
        });
    }
    let count = packed_sample_count(descriptor.dimensions)?;
    let capacity = usize::try_from(count).map_err(|_| Error::NumericOverflow)?;
    let values = match derivative {
        FieldDerivative::Value => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface scalar field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_scalar(channel, derivative, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push(sample.value.bits());
            }
            QuantizedFieldTileValues::Scalar(values)
        }
        FieldDerivative::Gradient => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface gradient field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_vector(channel, derivative, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push([
                    sample.value.x.bits(),
                    sample.value.y.bits(),
                    sample.value.z.bits(),
                ]);
            }
            QuantizedFieldTileValues::Gradient(values)
        }
        FieldDerivative::Hessian => {
            let mut values = Vec::new();
            crate::memory::reserve_exact(&mut values, capacity, "surface Hessian field samples")?;
            for index in 0..count {
                if cancellation.is_cancelled() {
                    return Err(Error::GraphCancelled);
                }
                let position =
                    tile_sample_position(descriptor.bounds, descriptor.dimensions, index)?;
                let sample = provider.sample_hessian(channel, position)?;
                validate_field_sample_identity(
                    sample.channel,
                    sample.derivative,
                    sample.revision,
                    channel,
                    derivative,
                    descriptor.revision,
                )?;
                values.push([
                    sample.value.xx.bits(),
                    sample.value.xy.bits(),
                    sample.value.xz.bits(),
                    sample.value.yy.bits(),
                    sample.value.yz.bits(),
                    sample.value.zz.bits(),
                ]);
            }
            QuantizedFieldTileValues::Hessian(values)
        }
    };
    let tile = EvaluationFieldTile {
        source: EvaluationFieldSource::SurfaceProvider {
            provider: descriptor.provider,
            revision: descriptor.revision,
        },
        channel,
        derivative,
        blend: FieldBlendOperator::Replace,
        weight: UnitInterval::ONE,
        layer_order: (i32::MIN, u128::from(descriptor.provider.0)),
        source_hash,
        bounds: descriptor.bounds,
        dimensions: descriptor.dimensions,
        values,
    };
    tile.validate()?;
    Ok(tile)
}

fn validate_field_sample_identity(
    sample_channel: FieldChannel,
    sample_derivative: FieldDerivative,
    sample_revision: SurfaceRevision,
    expected_channel: FieldChannel,
    expected_derivative: FieldDerivative,
    expected_revision: SurfaceRevision,
) -> Result<()> {
    if sample_channel == expected_channel
        && sample_derivative == expected_derivative
        && sample_revision == expected_revision
    {
        return Ok(());
    }
    Err(Error::GraphDocument {
        path: "surfaceFieldPrecompute".to_owned(),
        reason: "provider returned a mismatched canonical field sample".to_owned(),
    })
}

impl EvaluationFieldTile {
    /// Validates dimensions and packed row count.
    pub fn validate(&self) -> Result<()> {
        let count = self.dimensions.iter().try_fold(1_u64, |product, value| {
            product
                .checked_mul(u64::from(*value))
                .ok_or(Error::NumericOverflow)
        })?;
        let value_count = match &self.values {
            QuantizedFieldTileValues::Scalar(values) => values.len(),
            QuantizedFieldTileValues::Gradient(values) => values.len(),
            QuantizedFieldTileValues::Hessian(values) => values.len(),
        };
        let shape_matches = matches!(
            (&self.values, self.derivative),
            (QuantizedFieldTileValues::Scalar(_), FieldDerivative::Value)
                | (
                    QuantizedFieldTileValues::Gradient(_),
                    FieldDerivative::Gradient
                )
                | (
                    QuantizedFieldTileValues::Hessian(_),
                    FieldDerivative::Hessian
                )
        );
        if count == 0
            || usize::try_from(count).ok() != Some(value_count)
            || !shape_matches
            || self.source_hash == [0; 32]
        {
            return Err(Error::GraphDocument {
                path: "evaluation.fields".to_owned(),
                reason: "tile dimensions do not match packed values".to_owned(),
            });
        }
        Ok(())
    }

    fn sample_index(&self, position: WorldPosition) -> Option<usize> {
        if !self.bounds.contains(position) {
            return None;
        }
        let minimum = self.bounds.min_ticks();
        let maximum = self.bounds.max_ticks_exclusive();
        let point = position.global_ticks();
        let mut coordinate = [0_u32; 3];
        for axis in 0..3 {
            let span = maximum[axis] - minimum[axis];
            let offset = point[axis] - minimum[axis];
            let scaled = offset.checked_mul(i128::from(self.dimensions[axis]))?;
            let index = scaled.div_euclid(span);
            coordinate[axis] = u32::try_from(index)
                .ok()?
                .min(self.dimensions[axis].saturating_sub(1));
        }
        let index = u64::from(coordinate[0])
            .checked_mul(u64::from(self.dimensions[1]))?
            .checked_add(u64::from(coordinate[1]))?
            .checked_mul(u64::from(self.dimensions[2]))?
            .checked_add(u64::from(coordinate[2]))?;
        usize::try_from(index).ok()
    }

    fn sample_scalar(&self, position: WorldPosition) -> Option<DecisionScalar> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Scalar(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(DecisionScalar::from_bits)
    }

    fn sample_vector(&self, position: WorldPosition) -> Option<DecisionVec3> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Gradient(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(|value| DecisionVec3 {
            x: DecisionScalar::from_bits(value[0]),
            y: DecisionScalar::from_bits(value[1]),
            z: DecisionScalar::from_bits(value[2]),
        })
    }

    fn sample_hessian(&self, position: WorldPosition) -> Option<DecisionHessian3> {
        let index = self.sample_index(position)?;
        let QuantizedFieldTileValues::Hessian(values) = &self.values else {
            return None;
        };
        values.get(index).copied().map(|value| DecisionHessian3 {
            xx: DecisionScalar::from_bits(value[0]),
            xy: DecisionScalar::from_bits(value[1]),
            xz: DecisionScalar::from_bits(value[2]),
            yy: DecisionScalar::from_bits(value[3]),
            yz: DecisionScalar::from_bits(value[4]),
            zz: DecisionScalar::from_bits(value[5]),
        })
    }
}

/// Quantized micro density/attribute tile. Individual reconstructed blades remain cosmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroFieldTile {
    /// Canonical owner cell.
    pub cell: WorldCellKey,
    /// Plant family whose cosmetic population is reconstructed from this tile.
    pub family: Uuid,
    /// Packed dimensions.
    pub dimensions: [u32; 3],
    /// Authoritative density samples.
    pub density: Vec<u16>,
    /// Optional typed attribute channels.
    pub attributes: BTreeMap<u128, Vec<i32>>,
    /// Stable cosmetic reconstruction seed.
    pub reconstruction_seed: u128,
}

/// Why one candidate did not reach a macro output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateRejectionReason {
    /// No eligible surface projection.
    SurfaceMiss,
    /// Scalar threshold rejected the candidate.
    Threshold,
    /// Stable weighted elimination removed the candidate.
    WeightedElimination,
    /// A higher-priority exclusion claim removed the candidate.
    PriorityExclusion,
    /// A prior immutable-stage claim won spacing or competition.
    Competition,
    /// Candidate lies in the halo but belongs to another cell.
    ForeignOwner,
    /// No plant family had positive weight.
    NoSpecies,
}

/// Expanded provenance for a rejected candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedCandidate {
    /// Stable pre-acceptance identity.
    pub candidate: CandidateIdentity,
    /// Rejection reason.
    pub reason: CandidateRejectionReason,
    /// Complete lineage in the result's shared provenance table.
    pub provenance: ProvenanceHandle,
}

/// Scope captured by one named diagnostic output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticStreamScope {
    /// Complete rejection state at this point in canonical graph execution.
    GlobalSnapshot,
    /// One connected candidate lineage and its associated rejections.
    CandidateLineage(CandidateLineage),
}

/// Stable candidate data retained by a connected diagnostic output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticCandidateSample {
    /// Stable pre-acceptance identity.
    pub identity: CandidateIdentity,
    /// Canonical owner cell.
    pub owner: WorldCellKey,
    /// Exact quantized position.
    pub position: WorldPosition,
    /// Selected plant family, when assigned.
    pub family: Option<Uuid>,
    /// Selected family variation.
    pub variation: u32,
    /// Stable deterministic priority.
    pub priority: DecisionScalar,
    /// Ecology snapshot tick carried by the candidate.
    pub ecology_tick: u64,
}

/// Exact scalar datum retained by a connected diagnostic output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagnosticScalarSample {
    /// Candidate owning the datum.
    pub candidate: CandidateIdentity,
    /// Exact scalar value.
    pub value: DecisionScalar,
}

/// One retained, typed, user-named diagnostic stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedDiagnosticStream {
    /// Fully qualified node that captured the stream.
    pub node: GraphNodeAddress,
    /// User-facing stable stream name.
    pub label: String,
    /// Explicit global or connected-lineage scope.
    pub scope: DiagnosticStreamScope,
    /// Connected candidate samples, absent when the candidate pin is unconnected.
    pub candidates: Option<Vec<DiagnosticCandidateSample>>,
    /// Connected scalar samples, absent when the field pin is unconnected.
    pub field: Option<Vec<DiagnosticScalarSample>>,
    /// Rejections belonging to the selected scope at capture time.
    pub rejected: Vec<RejectedCandidate>,
}

/// Per-node evaluator planning and actual work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEvaluationDiagnostic {
    /// Module-call path from the root graph.
    pub module_path: Vec<u128>,
    /// Stable node GUID local to its owning graph.
    pub node: u128,
    /// Typed operator executed by the node.
    pub operator: GraphOperator,
    /// Stable debug symbol label.
    pub symbol: String,
    /// Input candidates.
    pub input_candidates: u64,
    /// Output candidates.
    pub output_candidates: u64,
    /// Output bytes retained by the evaluator.
    pub output_bytes: u64,
    /// CPU/GPU transfer bytes for this execution plan.
    pub transfer_bytes: u64,
    /// Conservative transfer bytes predicted by the compiled node estimate.
    pub predicted_transfer_bytes: u64,
    /// Measured wall time in microseconds.
    pub elapsed_micros: u64,
    /// Execution domain actually used.
    pub execution_domain: GraphExecutionDomain,
}

/// Actual work for one resident connected execution group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuGroupEvaluationDiagnostic {
    /// Canonical nodes executed by the single resident program.
    pub nodes: Vec<GraphNodeAddress>,
    /// Invocations submitted to the program.
    pub invocation_count: u64,
    /// Boundary bytes uploaded and downloaded once.
    pub transfer_bytes: u64,
    /// Boundary output bytes retained by the evaluator.
    pub output_bytes: u64,
    /// Measured wall time for the complete dispatch in microseconds.
    pub elapsed_micros: u64,
}

/// Complete typed diagnostics for one bounded evaluation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphEvaluationDiagnostics {
    /// Per-node diagnostics in canonical topological order.
    pub nodes: Vec<NodeEvaluationDiagnostic>,
    /// Resident group dispatches in canonical execution order.
    pub gpu_groups: Vec<GpuGroupEvaluationDiagnostic>,
    /// Expanded rejected-candidate records.
    pub rejected: Vec<RejectedCandidate>,
    /// Retained named outputs in canonical node order.
    pub streams: Vec<NamedDiagnosticStream>,
    /// Total candidates seen by output stages.
    pub candidate_count: u64,
    /// Accepted macro point count.
    pub accepted_count: u64,
}

/// Validates a complete rejection-diagnostics facet and returns nonzero reason totals.
pub fn vegetation_rejection_totals(bytes: &[u8]) -> Result<Vec<(CandidateRejectionReason, u64)>> {
    let mut reader = BinaryReader::new(bytes, "vegetation rejection diagnostics");
    reader.expect(b"SVEGREJ1", "magic")?;
    let candidate_count = reader.u64()?;
    let accepted_count = reader.u64()?;
    if accepted_count > candidate_count {
        return Err(Error::ArtifactFormat {
            format: "vegetation rejection diagnostics",
            field: "acceptedCount".to_owned(),
        });
    }
    let rejected_count = reader.count(57)?;
    let mut totals = [0_u64; 7];
    for _ in 0..rejected_count {
        skip_candidate_identity(&mut reader)?;
        let reason = rejection_reason_from_byte(reader.u8()?)?;
        reader.u32()?;
        totals[usize::from(rejection_reason_byte(reason))] = totals
            [usize::from(rejection_reason_byte(reason))]
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    }
    let stream_count = reader.count(27)?;
    for _ in 0..stream_count {
        skip_diagnostic_stream(&mut reader)?;
    }
    reader.complete()?;
    let reasons = [
        CandidateRejectionReason::SurfaceMiss,
        CandidateRejectionReason::Threshold,
        CandidateRejectionReason::WeightedElimination,
        CandidateRejectionReason::PriorityExclusion,
        CandidateRejectionReason::Competition,
        CandidateRejectionReason::ForeignOwner,
        CandidateRejectionReason::NoSpecies,
    ];
    Ok(reasons
        .into_iter()
        .zip(totals)
        .filter(|(_, count)| *count != 0)
        .collect())
}

/// Complete result of one reference evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEvaluationResult {
    /// Canonical output cell.
    pub cell: WorldCellKey,
    /// Schema-hashed canonical macro columns.
    pub macro_points: PlantPointColumns,
    /// Quantized micro fields.
    pub micro_fields: Vec<MicroFieldTile>,
    /// Exact canonical surface projection queries produced during this evaluation.
    pub surface_projection_tiles: Vec<QuantizedSurfaceProjectionTile>,
    /// Exact canonical surface-field queries produced during preparation or replay.
    pub surface_field_query_tiles: Vec<QuantizedSurfaceFieldQueryTile>,
    /// Coarse cells whose macro products are referenced by this finer cell.
    pub ancestor_references: Vec<WorldCellKey>,
    /// Expanded provenance table used by accepted points.
    pub provenance: ProvenanceTable,
    /// Typed counts, timings, and rejection explanations.
    pub diagnostics: GraphEvaluationDiagnostics,
}

/// One complete, parent-before-child explanation of an accepted or rejected candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenanceExplanation {
    /// Record handle referenced by the point or rejection.
    pub provenance: ProvenanceHandle,
    /// Map, layer, biome, candidate, family, and optional accepted plant identity.
    pub record: ProvenanceRecord,
    /// Reachable decision DAG nodes in canonical handle order.
    pub decisions: Vec<(ProvenanceDecisionHandle, ProvenanceDecision)>,
    /// Terminal rejection reason, absent for an accepted plant.
    pub rejection_reason: Option<CandidateRejectionReason>,
}

impl GraphEvaluationResult {
    /// Explains one accepted plant through the shared provenance decision DAG.
    pub fn explain_plant(&self, plant: PlantId) -> Result<ProvenanceExplanation> {
        let index =
            self.macro_points
                .ids
                .binary_search(&plant)
                .map_err(|_| Error::GraphDocument {
                    path: format!("evaluation.plants.{plant}"),
                    reason: "accepted plant is not present in this result".to_owned(),
                })?;
        let handle = self
            .macro_points
            .provenance
            .get(index)
            .copied()
            .map(ProvenanceHandle)
            .ok_or_else(|| {
                Error::PointSchema("plant provenance column is incomplete".to_owned())
            })?;
        self.explain_provenance(handle, None)
    }

    /// Explains one rejected candidate through the shared provenance decision DAG.
    pub fn explain_rejection(&self, candidate: CandidateIdentity) -> Result<ProvenanceExplanation> {
        let rejected = self
            .diagnostics
            .rejected
            .iter()
            .find(|rejected| rejected.candidate == candidate)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.rejected".to_owned(),
                reason: "candidate is not present in the rejection diagnostics".to_owned(),
            })?;
        self.explain_provenance(rejected.provenance, Some(rejected.reason))
    }

    fn explain_provenance(
        &self,
        handle: ProvenanceHandle,
        rejection_reason: Option<CandidateRejectionReason>,
    ) -> Result<ProvenanceExplanation> {
        let record = self
            .provenance
            .get(handle)
            .cloned()
            .ok_or_else(|| Error::GraphDocument {
                path: format!("evaluation.provenance.{}", handle.0),
                reason: "provenance record is missing".to_owned(),
            })?;
        let mut reachable = BTreeSet::new();
        let mut pending = vec![record.decision];
        while let Some(decision) = pending.pop() {
            if !reachable.insert(decision) {
                continue;
            }
            let value = self
                .provenance
                .decision(decision)
                .ok_or_else(|| Error::GraphDocument {
                    path: format!("evaluation.provenance.decisions.{}", decision.0),
                    reason: "provenance decision is missing".to_owned(),
                })?;
            pending.extend(value.parents.iter().copied());
        }
        let decisions = reachable
            .into_iter()
            .map(|decision| {
                self.provenance
                    .decision(decision)
                    .cloned()
                    .map(|value| (decision, value))
                    .ok_or_else(|| Error::GraphDocument {
                        path: format!("evaluation.provenance.decisions.{}", decision.0),
                        reason: "provenance decision is missing".to_owned(),
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ProvenanceExplanation {
            provenance: handle,
            record,
            decisions,
            rejection_reason,
        })
    }

    /// Exact canonical encoding length without allocating the encoded result.
    pub fn canonical_byte_len(&self) -> Result<usize> {
        let mut sink = CountSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    /// Stable result bytes used by determinism and scheduling tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut sink = ByteSink::new();
        self.encode_canonical(&mut sink)?;
        Ok(sink.finish())
    }

    /// Streams the exact canonical encoding into a vegetation content digest.
    pub fn update_content_hasher(&self, hasher: &mut VegetationContentHasher) -> Result<()> {
        self.encode_canonical(hasher)
    }

    /// Produces independently resident `.svegcell` facets from the canonical evaluator result.
    pub fn cell_artifact_sections(&self) -> Result<Vec<VegetationCellSection>> {
        self.validate_canonical_encoding()?;
        let macro_points = self.macro_points.canonical_bytes()?;

        let mut micro_fields = ByteSink::new();
        micro_fields.write(b"SVEGMIC2")?;
        self.encode_micro_fields(&mut micro_fields)?;

        let mut provenance = ByteSink::new();
        provenance.write(b"SVEGPRV1")?;
        self.encode_provenance(&mut provenance)?;

        let mut diagnostics = ByteSink::new();
        diagnostics.write(b"SVEGREJ1")?;
        self.encode_rejection_diagnostics(&mut diagnostics)?;

        let mut attachments = ByteSink::new();
        attachments.write(b"SVEGSAT1")?;
        self.encode_surface_attachments(&mut attachments)?;

        let mut surface_dependencies = ByteSink::new();
        surface_dependencies.write(b"SVEGSDE1")?;
        self.encode_surface_dependencies(&mut surface_dependencies)?;

        let mut render_references = ByteSink::new();
        render_references.write(b"SVEGRRF1")?;
        self.encode_render_references(&mut render_references)?;

        let mut render_bounds = ByteSink::new();
        render_bounds.write(b"SVEGRBD1")?;
        self.encode_render_bounds(&mut render_bounds)?;

        let mut collision_inputs = ByteSink::new();
        collision_inputs.write(b"SVEGCOL1")?;
        self.encode_collision_inputs(&mut collision_inputs)?;

        let mut navigation = ByteSink::new();
        navigation.write(b"SVEGNAV1")?;
        self.encode_navigation_contributions(&mut navigation)?;

        let mut ecology_boundary = ByteSink::new();
        ecology_boundary.write(b"SVEGEBD1")?;
        self.encode_ecology_boundary(&mut ecology_boundary)?;

        let mut ecology_checkpoint = ByteSink::new();
        ecology_checkpoint.write(b"SVEGECP1")?;
        self.encode_ecology_checkpoint(&mut ecology_checkpoint)?;

        Ok(vec![
            VegetationCellSection::raw(VegetationCellSectionKind::MacroPoints, macro_points),
            VegetationCellSection::raw(
                VegetationCellSectionKind::MicroFields,
                micro_fields.finish(),
            ),
            VegetationCellSection::raw(VegetationCellSectionKind::Provenance, provenance.finish()),
            VegetationCellSection::raw(
                VegetationCellSectionKind::RejectionDiagnostics,
                diagnostics.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::SurfaceAttachments,
                attachments.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::SurfaceDependencies,
                surface_dependencies.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::RenderReferences,
                render_references.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::RenderBounds,
                render_bounds.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::CollisionInputs,
                collision_inputs.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::NavigationContributions,
                navigation.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::EcologyBoundary,
                ecology_boundary.finish(),
            ),
            VegetationCellSection::raw(
                VegetationCellSectionKind::EcologyCheckpoint,
                ecology_checkpoint.finish(),
            ),
        ])
    }

    fn encode_canonical<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        self.validate_canonical_encoding()?;
        sink.write(b"SVEGEVAL05")?;
        sink.write(&self.cell.canonical_bytes())?;
        let macro_length = self.macro_points.canonical_byte_len()?;
        push_len(sink, macro_length)?;
        self.macro_points.encode_canonical(sink)?;
        self.encode_micro_fields(sink)?;
        self.encode_surface_attachments(sink)?;
        self.encode_surface_dependencies(sink)?;
        encode_unique_references(sink, &self.ancestor_references)?;
        self.encode_provenance(sink)?;
        self.encode_rejection_diagnostics(sink)
    }

    fn encode_micro_fields<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.micro_fields.len())?;
        encode_ordered(sink, &self.micro_fields, encode_micro_tile)
    }

    fn encode_surface_attachments<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.surface_projection_tiles.len())?;
        encode_ordered(sink, &self.surface_projection_tiles, encode_projection_tile)
    }

    fn encode_surface_dependencies<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.surface_field_query_tiles.len())?;
        encode_ordered(
            sink,
            &self.surface_field_query_tiles,
            encode_field_query_tile,
        )
    }

    fn encode_render_references<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            sink.write(&self.macro_points.variations[row].to_be_bytes())?;
            sink.write(&self.macro_points.phenotypes[row].to_be_bytes())?;
            sink.write(&self.macro_points.representation_classes[row].to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_render_bounds<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
        }
        Ok(())
    }

    fn encode_collision_inputs<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_position(sink, self.macro_points.positions[row])?;
            for lane in self.macro_points.orientations[row].bits() {
                sink.write(&lane.to_be_bytes())?;
            }
            for scale in self.macro_points.scales[row] {
                sink.write(&scale.canonical_bytes())?;
            }
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&(self.macro_points.interaction_policies[row] as u32).to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_navigation_contributions<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&(self.macro_points.interaction_policies[row] as u32).to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_ecology_boundary<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        let cell_bounds = self.cell.bounds();
        let boundary_rows = self
            .macro_points
            .bounds
            .iter()
            .enumerate()
            .filter(|(_, bounds)| touches_boundary(**bounds, cell_bounds))
            .map(|(row, _)| row)
            .collect::<Vec<_>>();
        push_len(sink, boundary_rows.len())?;
        for row in boundary_rows {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            encode_world_bounds(sink, self.macro_points.bounds[row])?;
            sink.write(&self.macro_points.ecology_ticks[row].to_be_bytes())?;
            sink.write(&self.macro_points.health[row].canonical_bytes())?;
            sink.write(&self.macro_points.moisture[row].canonical_bytes())?;
            sink.write(&self.macro_points.fuel[row].canonical_bytes())?;
            sink.write(&self.macro_points.phenology[row].canonical_bytes())?;
        }
        Ok(())
    }

    fn encode_ecology_checkpoint<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        push_len(sink, self.macro_points.ids.len())?;
        for row in 0..self.macro_points.ids.len() {
            sink.write(&self.macro_points.ids[row].bytes())?;
            sink.write(&self.macro_points.families[row].value().to_be_bytes())?;
            sink.write(&(self.macro_points.lifecycles[row] as u32).to_be_bytes())?;
            sink.write(&self.macro_points.phenotypes[row].to_be_bytes())?;
            sink.write(&self.macro_points.ecology_ticks[row].to_be_bytes())?;
            sink.write(&self.macro_points.health[row].canonical_bytes())?;
            sink.write(&self.macro_points.moisture[row].canonical_bytes())?;
            sink.write(&self.macro_points.fuel[row].canonical_bytes())?;
            sink.write(&self.macro_points.phenology[row].canonical_bytes())?;
            sink.write(&self.macro_points.flags[row].bits().to_be_bytes())?;
        }
        Ok(())
    }

    fn encode_provenance<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        encode_provenance(sink, &self.provenance)
    }

    fn encode_rejection_diagnostics<S: CanonicalSink>(&self, sink: &mut S) -> Result<()> {
        sink.write(&self.diagnostics.candidate_count.to_be_bytes())?;
        sink.write(&self.diagnostics.accepted_count.to_be_bytes())?;
        push_len(sink, self.diagnostics.rejected.len())?;
        encode_ordered(sink, &self.diagnostics.rejected, encode_rejected_candidate)?;
        push_len(sink, self.diagnostics.streams.len())?;
        encode_ordered(sink, &self.diagnostics.streams, encode_diagnostic_stream)
    }

    fn validate_canonical_encoding(&self) -> Result<()> {
        self.validate_canonical_encoding_with_guard(None)
    }

    fn validate_canonical_encoding_guarded(&self, guard: PreflightGuard<'_>) -> Result<()> {
        self.validate_canonical_encoding_with_guard(Some(guard))
    }

    fn validate_canonical_encoding_with_guard(
        &self,
        guard: Option<PreflightGuard<'_>>,
    ) -> Result<()> {
        self.macro_points
            .validate_guarded(|| guard.map_or(Ok(()), PreflightGuard::check))?;
        for tile in &self.surface_projection_tiles {
            guard.map_or(Ok(()), PreflightGuard::check)?;
            if let Some(guard) = guard {
                validate_projection_tile_guarded(tile, guard)?;
            } else {
                tile.validate()?;
            }
        }
        for tile in &self.surface_field_query_tiles {
            validate_field_query_tile_with_guard(tile, guard)?;
        }
        let ordered =
            validate_strict_order(&self.micro_fields, guard, |left, right| {
                (left.cell, left.family.value()) < (right.cell, right.family.value())
            })? && validate_strict_order(&self.surface_projection_tiles, guard, |left, right| {
                projection_tile_order_key(left) < projection_tile_order_key(right)
            })? && validate_strict_order(&self.surface_field_query_tiles, guard, |left, right| {
                field_query_tile_order_key(left) < field_query_tile_order_key(right)
            })? && validate_strict_order(&self.ancestor_references, guard, |left, right| {
                left < right
            })? && validate_strict_order(&self.diagnostics.rejected, guard, |left, right| {
                rejected_order_key(left) < rejected_order_key(right)
            })? && validate_strict_order(&self.diagnostics.streams, guard, |left, right| {
                diagnostic_stream_order_key(left) < diagnostic_stream_order_key(right)
            })?;
        if !ordered {
            return Err(Error::GraphDocument {
                path: "evaluation.canonicalEncoding".to_owned(),
                reason: "result collections are not in canonical order".to_owned(),
            });
        }
        for stream in &self.diagnostics.streams {
            guard.map_or(Ok(()), PreflightGuard::check)?;
            let candidates_ordered = match &stream.candidates {
                Some(values) => validate_strict_order(values, guard, |left, right| {
                    left.identity < right.identity
                })?,
                None => true,
            };
            let field_ordered = match &stream.field {
                Some(values) => validate_strict_order(values, guard, |left, right| {
                    left.candidate < right.candidate
                })?,
                None => true,
            };
            let rejected_ordered =
                validate_strict_order(&stream.rejected, guard, |left, right| {
                    rejected_order_key(left) < rejected_order_key(right)
                })?;
            let stream_ordered = candidates_ordered && field_ordered && rejected_ordered;
            if !stream_ordered {
                return Err(Error::GraphDocument {
                    path: "evaluation.canonicalEncoding.streams".to_owned(),
                    reason: "diagnostic stream samples are not in canonical order".to_owned(),
                });
            }
        }
        guard.map_or(Ok(()), PreflightGuard::check)
    }
}

fn validate_strict_order<T>(
    values: &[T],
    guard: Option<PreflightGuard<'_>>,
    before: impl Fn(&T, &T) -> bool,
) -> Result<bool> {
    for pair in values.windows(2) {
        guard.map_or(Ok(()), PreflightGuard::check)?;
        if !before(&pair[0], &pair[1]) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn projection_tile_order_key(tile: &QuantizedSurfaceProjectionTile) -> (u128, u32, [u8; 32]) {
    (
        tile.node,
        tile.node_semantic_revision,
        tile.provider_set_hash,
    )
}

fn field_query_tile_order_key(
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

fn rejected_order_key(rejected: &RejectedCandidate) -> (CandidateIdentity, u8, ProvenanceHandle) {
    (
        rejected.candidate,
        rejection_reason_byte(rejected.reason),
        rejected.provenance,
    )
}

fn diagnostic_stream_order_key(
    stream: &NamedDiagnosticStream,
) -> (&GraphNodeAddress, &str, DiagnosticStreamScope) {
    (&stream.node, stream.label.as_str(), stream.scope)
}

fn encode_ordered<S, T, E>(sink: &mut S, values: &[T], mut encode: E) -> Result<()>
where
    S: CanonicalSink,
    E: FnMut(&mut S, &T) -> Result<()>,
{
    for value in values {
        encode(sink, value)?;
    }
    Ok(())
}

fn encode_micro_tile<S: CanonicalSink>(sink: &mut S, tile: &MicroFieldTile) -> Result<()> {
    sink.write(&tile.cell.canonical_bytes())?;
    sink.write(&tile.family.value().to_be_bytes())?;
    for dimension in tile.dimensions {
        sink.write(&dimension.to_be_bytes())?;
    }
    push_len(sink, tile.density.len())?;
    for value in &tile.density {
        sink.write(&value.to_be_bytes())?;
    }
    push_len(sink, tile.attributes.len())?;
    for (channel, values) in &tile.attributes {
        sink.write(&channel.to_be_bytes())?;
        push_len(sink, values.len())?;
        for value in values {
            sink.write(&value.to_be_bytes())?;
        }
    }
    sink.write(&tile.reconstruction_seed.to_be_bytes())
}

fn encode_projection_tile<S: CanonicalSink>(
    sink: &mut S,
    tile: &QuantizedSurfaceProjectionTile,
) -> Result<()> {
    sink.write(&tile.node.to_be_bytes())?;
    sink.write(&tile.node_semantic_revision.to_be_bytes())?;
    sink.write(&tile.provider_set_hash)?;
    push_len(sink, tile.samples.len())?;
    for entry in &tile.samples {
        push_world_position(sink, entry.query)?;
        match &entry.sample {
            Some(sample) => {
                sink.write_byte(1)?;
                push_world_position(sink, sample.position)?;
                sink.write(&sample.attachment.provider.0.to_be_bytes())?;
                sink.write(&sample.attachment.primitive.0.to_be_bytes())?;
                for barycentric in sample.attachment.barycentric {
                    sink.write(&barycentric.bits().to_be_bytes())?;
                }
                sink.write(&sample.attachment.revision.0.to_be_bytes())?;
                for normal in sample.normal {
                    sink.write(&normal.bits().to_be_bytes())?;
                }
                for projection in sample.projection {
                    sink.write(&projection.bits().to_be_bytes())?;
                }
                push_len(sink, sample.tags.len())?;
                for tag in &sample.tags {
                    sink.write(&tag.tag.0.to_be_bytes())?;
                    sink.write(&tag.weight.bits().to_be_bytes())?;
                }
            }
            None => sink.write_byte(0)?,
        }
    }
    Ok(())
}

fn encode_field_query_tile<S: CanonicalSink>(
    sink: &mut S,
    tile: &QuantizedSurfaceFieldQueryTile,
) -> Result<()> {
    sink.write(&tile.node.to_be_bytes())?;
    sink.write(&tile.node_semantic_revision.to_be_bytes())?;
    push_field_channel(sink, tile.channel)?;
    sink.write_byte(match tile.derivative {
        FieldDerivative::Value => 0,
        FieldDerivative::Gradient => 1,
        FieldDerivative::Hessian => 2,
    })?;
    sink.write(&tile.provider_set_hash)?;
    push_len(sink, tile.samples.len())?;
    for entry in &tile.samples {
        push_candidate_identity(sink, entry.candidate)?;
        push_world_position(sink, entry.query)?;
        match entry.value {
            QuantizedSurfaceFieldValue::Scalar(value) => {
                sink.write_byte(0)?;
                sink.write(&value.to_be_bytes())?;
            }
            QuantizedSurfaceFieldValue::Gradient(value) => {
                sink.write_byte(1)?;
                for lane in value {
                    sink.write(&lane.to_be_bytes())?;
                }
            }
            QuantizedSurfaceFieldValue::Hessian(value) => {
                sink.write_byte(2)?;
                for lane in value {
                    sink.write(&lane.to_be_bytes())?;
                }
            }
        }
    }
    Ok(())
}

fn encode_unique_references<S: CanonicalSink>(
    sink: &mut S,
    references: &[WorldCellKey],
) -> Result<()> {
    push_len(sink, references.len())?;
    for reference in references {
        sink.write(&reference.canonical_bytes())?;
    }
    Ok(())
}

fn encode_provenance<S: CanonicalSink>(sink: &mut S, table: &ProvenanceTable) -> Result<()> {
    push_len(sink, table.decisions().len())?;
    for decision in table.decisions() {
        push_len(sink, decision.parents.len())?;
        for parent in &decision.parents {
            sink.write(&parent.0.to_be_bytes())?;
        }
        push_len(sink, decision.subgraph_path.len())?;
        for call in &decision.subgraph_path {
            sink.write(&call.to_be_bytes())?;
        }
        sink.write(&decision.node.to_be_bytes())?;
        let operator = decision.operator.as_wire().as_bytes();
        push_len(sink, operator.len())?;
        sink.write(operator)?;
        sink.write(&decision.candidate.to_be_bytes())?;
        sink.write_byte(provenance_outcome_byte(decision.outcome))?;
    }
    push_len(sink, table.records().len())?;
    for record in table.records() {
        sink.write(&record.map.value().to_be_bytes())?;
        sink.write(&record.layer.to_be_bytes())?;
        sink.write(&record.biome.value().to_be_bytes())?;
        sink.write(&record.decision.0.to_be_bytes())?;
        sink.write(&record.candidate.to_be_bytes())?;
        push_optional_uuid(sink, record.family)?;
        match record.plant {
            Some(plant) => {
                sink.write_byte(1)?;
                sink.write(&plant.bytes())?;
            }
            None => sink.write_byte(0)?,
        }
        sink.write(&record.variation.to_be_bytes())?;
    }
    Ok(())
}

fn encode_rejected_candidate<S: CanonicalSink>(
    sink: &mut S,
    candidate: &RejectedCandidate,
) -> Result<()> {
    push_candidate_identity(sink, candidate.candidate)?;
    sink.write_byte(rejection_reason_byte(candidate.reason))?;
    sink.write(&candidate.provenance.0.to_be_bytes())
}

fn encode_diagnostic_stream<S: CanonicalSink>(
    sink: &mut S,
    stream: &NamedDiagnosticStream,
) -> Result<()> {
    let node_length = 8_usize
        .checked_add(
            stream
                .node
                .module_path
                .len()
                .checked_mul(16)
                .ok_or(Error::NumericOverflow)?,
        )
        .and_then(|length| length.checked_add(16))
        .ok_or(Error::NumericOverflow)?;
    push_len(sink, node_length)?;
    push_len(sink, stream.node.module_path.len())?;
    for call in &stream.node.module_path {
        sink.write(&call.to_be_bytes())?;
    }
    sink.write(&stream.node.node.to_be_bytes())?;
    push_len(sink, stream.label.len())?;
    sink.write(stream.label.as_bytes())?;
    match stream.scope {
        DiagnosticStreamScope::GlobalSnapshot => sink.write_byte(0)?,
        DiagnosticStreamScope::CandidateLineage(lineage) => {
            sink.write_byte(1)?;
            sink.write(&lineage.0.to_be_bytes())?;
        }
    }
    match &stream.candidates {
        Some(candidates) => {
            sink.write_byte(1)?;
            push_len(sink, candidates.len())?;
            encode_ordered(sink, candidates, |sink, sample| {
                push_candidate_identity(sink, sample.identity)?;
                sink.write(&sample.owner.canonical_bytes())?;
                push_world_position(sink, sample.position)?;
                push_optional_uuid(sink, sample.family)?;
                sink.write(&sample.variation.to_be_bytes())?;
                sink.write(&sample.priority.bits().to_be_bytes())?;
                sink.write(&sample.ecology_tick.to_be_bytes())
            })?;
        }
        None => sink.write_byte(0)?,
    }
    match &stream.field {
        Some(field) => {
            sink.write_byte(1)?;
            push_len(sink, field.len())?;
            encode_ordered(sink, field, |sink, sample| {
                push_candidate_identity(sink, sample.candidate)?;
                sink.write(&sample.value.bits().to_be_bytes())
            })?;
        }
        None => sink.write_byte(0)?,
    }
    push_len(sink, stream.rejected.len())?;
    encode_ordered(sink, &stream.rejected, encode_rejected_candidate)
}

fn push_optional_uuid<S: CanonicalSink>(sink: &mut S, value: Option<Uuid>) -> Result<()> {
    match value {
        Some(value) => {
            sink.write_byte(1)?;
            sink.write(&value.value().to_be_bytes())?;
        }
        None => sink.write_byte(0)?,
    }
    Ok(())
}

fn push_world_position<S: CanonicalSink>(sink: &mut S, position: WorldPosition) -> Result<()> {
    for tick in position.global_ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

fn skip_diagnostic_stream(reader: &mut BinaryReader<'_>) -> Result<()> {
    let node_length = reader.length()?;
    let mut node = BinaryReader::new(
        reader.take(node_length)?,
        "vegetation rejection diagnostics",
    );
    let module_count = node.count(16)?;
    for _ in 0..module_count {
        node.u128()?;
    }
    node.u128()?;
    node.complete()?;
    reader.string()?;
    match reader.u8()? {
        0 => {}
        1 => {
            reader.u128()?;
        }
        _ => {
            return Err(Error::ArtifactFormat {
                format: "vegetation rejection diagnostics",
                field: "streams.scope".to_owned(),
            });
        }
    }
    if reader.bool()? {
        let count = reader.count(142)?;
        for _ in 0..count {
            skip_candidate_identity(reader)?;
            reader.cell()?;
            for _ in 0..3 {
                reader.i128()?;
            }
            if reader.bool()? {
                reader.uuid()?;
            }
            reader.u32()?;
            reader.i32()?;
            reader.u64()?;
        }
    }
    if reader.bool()? {
        let count = reader.count(56)?;
        for _ in 0..count {
            skip_candidate_identity(reader)?;
            reader.i32()?;
        }
    }
    let rejected_count = reader.count(57)?;
    for _ in 0..rejected_count {
        skip_candidate_identity(reader)?;
        rejection_reason_from_byte(reader.u8()?)?;
        reader.u32()?;
    }
    Ok(())
}

fn skip_candidate_identity(reader: &mut BinaryReader<'_>) -> Result<()> {
    reader.u128()?;
    reader.u128()?;
    reader.u32()?;
    reader.u64()?;
    reader.u64()?;
    Ok(())
}

fn push_candidate_identity<S: CanonicalSink>(
    sink: &mut S,
    identity: CandidateIdentity,
) -> Result<()> {
    sink.write(&identity.node.to_be_bytes())?;
    sink.write(&identity.node_address.to_be_bytes())?;
    sink.write(&identity.node_semantic_revision.to_be_bytes())?;
    sink.write(&identity.ordinal.to_be_bytes())?;
    sink.write(&identity.ancestor.to_be_bytes())
}

fn push_field_channel<S: CanonicalSink>(sink: &mut S, channel: FieldChannel) -> Result<()> {
    let (tag, user) = match channel {
        FieldChannel::Altitude => (0, None),
        FieldChannel::Slope => (1, None),
        FieldChannel::Curvature => (2, None),
        FieldChannel::Concavity => (3, None),
        FieldChannel::Drainage => (4, None),
        FieldChannel::Moisture => (5, None),
        FieldChannel::Temperature => (6, None),
        FieldChannel::Precipitation => (7, None),
        FieldChannel::Sunlight => (8, None),
        FieldChannel::Exposure => (9, None),
        FieldChannel::WaterDistance => (10, None),
        FieldChannel::WaterDepth => (11, None),
        FieldChannel::SignedBlocker => (12, None),
        FieldChannel::SplineDistance => (13, None),
        FieldChannel::User(value) => (14, Some(value)),
    };
    sink.write_byte(tag)?;
    if let Some(value) = user {
        sink.write(&value.to_be_bytes())?;
    }
    Ok(())
}

const fn provenance_outcome_byte(outcome: ProvenanceDecisionOutcome) -> u8 {
    match outcome {
        ProvenanceDecisionOutcome::Produced => 0,
        ProvenanceDecisionOutcome::Retained => 1,
        ProvenanceDecisionOutcome::Accepted => 2,
        ProvenanceDecisionOutcome::Rejected => 3,
    }
}

const fn rejection_reason_byte(reason: CandidateRejectionReason) -> u8 {
    match reason {
        CandidateRejectionReason::SurfaceMiss => 0,
        CandidateRejectionReason::Threshold => 1,
        CandidateRejectionReason::WeightedElimination => 2,
        CandidateRejectionReason::PriorityExclusion => 3,
        CandidateRejectionReason::Competition => 4,
        CandidateRejectionReason::ForeignOwner => 5,
        CandidateRejectionReason::NoSpecies => 6,
    }
}

fn rejection_reason_from_byte(value: u8) -> Result<CandidateRejectionReason> {
    match value {
        0 => Ok(CandidateRejectionReason::SurfaceMiss),
        1 => Ok(CandidateRejectionReason::Threshold),
        2 => Ok(CandidateRejectionReason::WeightedElimination),
        3 => Ok(CandidateRejectionReason::PriorityExclusion),
        4 => Ok(CandidateRejectionReason::Competition),
        5 => Ok(CandidateRejectionReason::ForeignOwner),
        6 => Ok(CandidateRejectionReason::NoSpecies),
        _ => Err(Error::ArtifactFormat {
            format: "vegetation rejection diagnostics",
            field: "rejectionReason".to_owned(),
        }),
    }
}

/// Accepted/removed/moved identities and override conflicts for a destructive graph edit preview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphIdentityEditPreview {
    /// Newly accepted identities.
    pub accepted: Vec<PlantId>,
    /// Identities removed by the edit.
    pub removed: Vec<PlantId>,
    /// Retained identities whose exact positions moved.
    pub moved: Vec<PlantId>,
    /// Pins and authored overrides invalidated by removed identities.
    pub conflicts: IdentityConflictReport,
}

/// Compares two complete evaluator results before committing an identity-affecting edit.
pub fn preview_graph_identity_edit(
    previous: &GraphEvaluationResult,
    proposed: &GraphEvaluationResult,
    pins: &[PlantId],
    overrides: &[PlantId],
) -> Result<GraphIdentityEditPreview> {
    previous.macro_points.validate()?;
    proposed.macro_points.validate()?;
    let old: BTreeMap<_, _> = previous
        .macro_points
        .ids
        .iter()
        .copied()
        .zip(previous.macro_points.positions.iter().copied())
        .collect();
    let new: BTreeMap<_, _> = proposed
        .macro_points
        .ids
        .iter()
        .copied()
        .zip(proposed.macro_points.positions.iter().copied())
        .collect();
    let old_ids = old.keys().copied().collect::<BTreeSet<_>>();
    let new_ids = new.keys().copied().collect::<BTreeSet<_>>();
    let accepted = new_ids.difference(&old_ids).copied().collect();
    let removed = old_ids.difference(&new_ids).copied().collect();
    let moved = old_ids
        .intersection(&new_ids)
        .copied()
        .filter(|id| old[id] != new[id])
        .collect();
    Ok(GraphIdentityEditPreview {
        accepted,
        removed,
        moved,
        conflicts: identity_conflicts(
            &old_ids.into_iter().collect::<Vec<_>>(),
            &new_ids.into_iter().collect::<Vec<_>>(),
            pins,
            overrides,
        ),
    })
}

/// Cooperative cancellation token. A cancelled job never publishes a partial result.
#[derive(Clone, Debug)]
pub struct GraphCancellationToken {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    remaining_checks: Arc<AtomicU64>,
    #[cfg(test)]
    observed_checks: Arc<AtomicU64>,
    #[cfg(test)]
    abort_checkpoint: Arc<AtomicU64>,
    #[cfg(test)]
    abort_kind: Arc<AtomicU64>,
}

impl Default for GraphCancellationToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            remaining_checks: Arc::new(AtomicU64::new(u64::MAX)),
            #[cfg(test)]
            observed_checks: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            abort_checkpoint: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            abort_kind: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl GraphCancellationToken {
    /// Cancels every evaluator sharing this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(test)]
        {
            self.observed_checks.fetch_add(1, Ordering::Relaxed);
            match self.remaining_checks.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |remaining| match remaining {
                    u64::MAX | 0 => None,
                    _ => Some(remaining - 1),
                },
            ) {
                Ok(_) | Err(u64::MAX) => false,
                Err(0) => true,
                Err(_) => false,
            }
        }
        #[cfg(not(test))]
        false
    }

    #[cfg(test)]
    fn cancel_after_checks(&self, checks: u64) {
        self.remaining_checks.store(checks, Ordering::Release);
    }

    #[cfg(test)]
    fn observed_checks(&self) -> u64 {
        self.observed_checks.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn abort_at_checkpoint(&self, checkpoint: TestEvaluationCheckpoint, kind: TestAbortKind) {
        self.abort_kind.store(kind as u64, Ordering::Release);
        self.abort_checkpoint
            .store(checkpoint as u64, Ordering::Release);
    }

    #[cfg(test)]
    fn check_test_checkpoint(
        &self,
        checkpoint: TestEvaluationCheckpoint,
        time_limit_ms: u64,
    ) -> Result<()> {
        if self.abort_checkpoint.load(Ordering::Acquire) != checkpoint as u64 {
            return Ok(());
        }
        match self.abort_kind.load(Ordering::Acquire) {
            value if value == TestAbortKind::Cancelled as u64 => Err(Error::GraphCancelled),
            value if value == TestAbortKind::Deadline as u64 => Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: time_limit_ms.saturating_add(1),
                limit: time_limit_ms,
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestEvaluationCheckpoint {
    AfterPreflight = 1,
    AfterPreparation = 2,
    AfterTraversal = 3,
    BeforeFinalValidation = 4,
    BeforePublication = 5,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestAbortKind {
    Cancelled = 1,
    Deadline = 2,
}

/// One explicit authored anchor with its stable vegetation-layer ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationAnchor {
    /// Stable authored vegetation layer owning the anchor.
    pub layer: u128,
    /// Complete canonical explicit point.
    pub point: PlantPoint,
}

/// Immutable inputs available to one cell job, including its halo.
#[derive(Clone)]
pub struct GraphEvaluationInputs {
    /// Vegetation map identity.
    pub map: Uuid,
    /// Stable local biome-instance identity used by procedural plant IDs and provenance.
    pub biome_instance: u128,
    /// Canonical output cell.
    pub output_cell: WorldCellKey,
    /// Exact output bounds.
    pub output_bounds: WorldBounds,
    /// Immutable halo bounds read by partitioned nodes.
    pub read_bounds: WorldBounds,
    /// Regions available to region-input nodes.
    pub regions: Vec<EvaluationRegion>,
    /// Splines available to spline-input nodes.
    pub splines: Vec<EvaluationSpline>,
    /// Explicit authored anchors.
    pub anchors: Vec<EvaluationAnchor>,
    /// Complete plant prototypes for every family the graph can emit.
    pub plant_prototypes: Vec<PlantPrototype>,
    /// Immutable ecology snapshot tick for read-only succession inputs.
    pub ecology_tick: u64,
    /// Quantized authored field tiles.
    pub fields: Vec<EvaluationFieldTile>,
    /// Precomputed canonical surface projection tiles for authoritative nodes.
    pub surface_projection_tiles: Vec<QuantizedSurfaceProjectionTile>,
    /// Exact canonical surface-field queries for authoritative nodes.
    pub surface_field_query_tiles: Vec<QuantizedSurfaceFieldQueryTile>,
    /// Exact content identity shared by every surface-derived tile in this job.
    pub surface_provider_set_hash: [u8; 32],
    /// Immutable surface providers sorted by descriptor ID during evaluation.
    pub surface_providers: Vec<Arc<dyn SurfaceField>>,
    /// Temporary render origin. It never enters candidate or plant identity.
    pub render_origin: WorldPosition,
}

impl GraphEvaluationInputs {
    /// Creates a cell job with an exact symmetric halo in Q15.16 metres.
    pub fn for_cell(
        map: Uuid,
        biome_instance: u128,
        cell: WorldCellKey,
        halo: DecisionScalar,
    ) -> Result<Self> {
        let output_bounds = cell.bounds();
        let halo_ticks = fixed_meters_to_ticks(halo)?.unsigned_abs() as i128;
        let minimum = output_bounds.min_ticks();
        let maximum = output_bounds.max_ticks_exclusive();
        let mut read_minimum = [0_i128; 3];
        let mut read_maximum = [0_i128; 3];
        for axis in 0..3 {
            read_minimum[axis] = minimum[axis]
                .checked_sub(halo_ticks)
                .ok_or(Error::NumericOverflow)?;
            read_maximum[axis] = maximum[axis]
                .checked_add(halo_ticks)
                .ok_or(Error::NumericOverflow)?;
        }
        let read_bounds = WorldBounds::new(read_minimum, read_maximum)?;
        Ok(Self {
            map,
            biome_instance,
            output_cell: cell,
            output_bounds,
            read_bounds,
            regions: canonical_cell_regions(read_bounds, cell.level(), 0)?,
            splines: Vec::new(),
            anchors: Vec::new(),
            plant_prototypes: Vec::new(),
            ecology_tick: 0,
            fields: Vec::new(),
            surface_projection_tiles: Vec::new(),
            surface_field_query_tiles: Vec::new(),
            surface_provider_set_hash: [0; 32],
            surface_providers: Vec::new(),
            render_origin: WorldPosition::origin(),
        })
    }

    /// Replaces the default halo regions with one clipped hierarchical authored region.
    pub fn set_hierarchical_region(&mut self, namespace: u128, bounds: WorldBounds) -> Result<()> {
        let bounds =
            intersect_bounds(bounds, self.read_bounds)?.ok_or_else(|| Error::GraphDocument {
                path: "evaluation.regions".to_owned(),
                reason: "hierarchical region does not intersect the immutable read bounds"
                    .to_owned(),
            })?;
        self.regions = canonical_cell_regions(bounds, self.output_cell.level(), namespace)?;
        Ok(())
    }
}

/// Immutable inputs for one compiler-owned ancestor/global stage tile.
#[derive(Clone)]
pub struct GlobalStageEvaluationInputs {
    /// Stable compiled stage identity.
    pub stage: [u8; 32],
    /// Canonical ancestor cell that owns the solved tile.
    pub owner: WorldCellKey,
    /// Exact finite solve domain. It must equal the owner cell bounds.
    pub solve_bounds: WorldBounds,
    /// Canonical identity of every immutable input visible to this stage tile.
    pub input_snapshot: [u8; 32],
    /// Complete immutable inputs over the stage's expanded read domain.
    pub inputs: GraphEvaluationInputs,
}

/// One atomic graph job containing output cells and every unique global-stage input tile.
#[derive(Clone, Default)]
pub struct GraphEvaluationJobInputs {
    /// Partitioned output-cell inputs.
    pub cells: Vec<GraphEvaluationInputs>,
    /// Deduplicated global-stage tiles in arbitrary request order.
    pub global_stages: Vec<GlobalStageEvaluationInputs>,
}

/// Checked work and retained-memory prediction for one complete atomic graph job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEvaluationPreflight {
    /// Partitioned output cells admitted by the job.
    pub output_cells: u64,
    /// Unique compiler-owned global-stage tiles admitted by the job.
    pub global_stage_tiles: u64,
    /// Caller-supplied and preparation-generated input tiles admitted by the job.
    pub input_tiles: u64,
    /// Caller-supplied immutable input allocations retained while the job runs.
    pub retained_input_bytes: u64,
    /// Canonical input allocations created by preparation and retained for replay and publication.
    pub generated_input_bytes: u64,
    /// Conservative total candidate-stream peak across every evaluated scope.
    pub candidate_count: u64,
    /// Conservative total accepted macro points.
    pub accepted_count: u64,
    /// Exact total quantized micro samples.
    pub micro_samples: u64,
    /// Peak requested evaluator-owned bytes during symbolic admission.
    pub preflight_peak_bytes: u64,
    /// Peak requested evaluator-owned bytes during execution and atomic result assembly.
    pub execution_peak_bytes: u64,
    /// Greater of the preflight and execution peaks.
    pub memory_bytes: u64,
    /// Exact resident-program transfer bytes for the selected execution plan.
    pub transfer_bytes: u64,
    /// Bounded cell workers participating in the job.
    pub worker_count: u16,
    /// Maximum wall-clock duration admitted for execution.
    pub time_limit_ms: u64,
    /// Hard limits enforced by this preflight and the matching evaluator run.
    pub limits: crate::GraphSafetyLimits,
}

/// Published result and retained-memory accounting for one global-stage tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalStageEvaluationResult {
    /// Stable compiled stage identity.
    pub stage: [u8; 32],
    /// Canonical ancestor cell that owns the solved tile.
    pub owner: WorldCellKey,
    /// Public macro, micro, provenance, and diagnostic products of the stage.
    pub result: GraphEvaluationResult,
    /// Conservative evaluator-owned requested bytes retained for downstream replay in this job.
    pub resident_bytes: u64,
}

/// Complete atomic result of one graph job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphEvaluationJobResult {
    /// Canonically sorted partitioned output-cell results.
    pub cells: Vec<GraphEvaluationResult>,
    /// Canonically sorted global-stage tile results, published exactly once.
    pub global_stages: Vec<GlobalStageEvaluationResult>,
}

fn graph_node_address_memory(address: &GraphNodeAddress) -> Result<u64> {
    requested_vec_bytes::<u128>(address.module_path.capacity())
}

fn qualified_graph_pin_memory(pin: &QualifiedGraphPin) -> Result<u64> {
    checked_memory_sum([
        graph_node_address_memory(&pin.node)?,
        requested_string_bytes(&pin.pin)?,
    ])
}

fn micro_field_tile_memory(tile: &MicroFieldTile) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes::<u16>(tile.density.capacity())?,
        requested_btree_with(
            &tile.attributes,
            |_| Ok(0),
            |values| requested_vec_bytes::<i32>(values.capacity()),
        )?,
    ])
}

fn projected_surface_sample_memory(sample: &ProjectedSurfaceSample) -> Result<u64> {
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

fn diagnostic_stream_memory(stream: &NamedDiagnosticStream) -> Result<u64> {
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

fn graph_result_memory(result: &GraphEvaluationResult) -> Result<u64> {
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum GraphValue {
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
    fn domain(&self) -> GraphDomain {
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

    fn candidate_count(&self) -> usize {
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

    fn requested_memory_bytes(&self) -> Result<u64> {
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
struct GlobalStageCacheKey {
    stage: [u8; 32],
    map: u64,
    biome_instance: u128,
    owner: WorldCellKey,
    input_snapshot: [u8; 32],
}

#[derive(Clone)]
struct GlobalStageTile {
    key: GlobalStageCacheKey,
    outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    provenance: ProvenanceTable,
    candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
    resident_bytes: u64,
}

struct PlannedEvaluation {
    result: GraphEvaluationResult,
    materialized_outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
}

#[derive(Default)]
struct GlobalStageStore {
    tiles: BTreeMap<GlobalStageCacheKey, GlobalStageTile>,
    by_owner: BTreeMap<([u8; 32], WorldCellKey), GlobalStageCacheKey>,
}

impl GlobalStageStore {
    fn insert(&mut self, tile: GlobalStageTile) -> Result<()> {
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

    fn tile(&self, stage: [u8; 32], owner: WorldCellKey) -> Result<&GlobalStageTile> {
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

    fn resident_bytes(&self) -> Result<u64> {
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
enum EvaluationScope<'a> {
    Cell {
        global_store: &'a GlobalStageStore,
    },
    Global {
        stage: &'a CompiledGlobalStage,
        global_store: &'a GlobalStageStore,
    },
}

impl<'a> EvaluationScope<'a> {
    fn global_store(self) -> &'a GlobalStageStore {
        match self {
            Self::Cell { global_store } | Self::Global { global_store, .. } => global_store,
        }
    }

    fn current_global_stage(self) -> Option<&'a CompiledGlobalStage> {
        match self {
            Self::Cell { .. } => None,
            Self::Global { stage, .. } => Some(stage),
        }
    }

    fn is_cell(self) -> bool {
        matches!(self, Self::Cell { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CommunityTables {
    competition: Vec<crate::CompetitionRule>,
    companions: Vec<crate::CompanionRule>,
    succession: Vec<crate::SuccessionRule>,
}

impl CommunityTables {
    fn canonicalize(&mut self) {
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

    fn requested_memory_bytes(&self) -> Result<u64> {
        checked_memory_sum([
            requested_vec_bytes::<crate::CompetitionRule>(self.competition.capacity())?,
            requested_vec_bytes::<crate::CompanionRule>(self.companions.capacity())?,
            requested_vec_bytes::<crate::SuccessionRule>(self.succession.capacity())?,
        ])
    }
}

type SurfaceProjectionCacheKey = (u128, u32, [u8; 32]);
type SurfaceProjectionCache = BTreeMap<
    SurfaceProjectionCacheKey,
    BTreeMap<WorldPosition, Option<QuantizedSurfaceProjectionSample>>,
>;
type SurfaceFieldQueryCacheKey = (u128, u32, FieldChannel, FieldDerivative, [u8; 32]);
type SurfaceFieldQueryCache = BTreeMap<
    SurfaceFieldQueryCacheKey,
    BTreeMap<(CandidateIdentity, WorldPosition), QuantizedSurfaceFieldValue>,
>;

#[derive(Clone, Copy)]
struct RandomSampleAddress {
    cell: WorldCellKey,
    candidate: u64,
    ancestor: u64,
    species: u128,
    channel: u32,
}

impl RandomSampleAddress {
    fn new(cell: WorldCellKey, candidate: u64) -> Self {
        Self {
            cell,
            candidate,
            ancestor: 0,
            species: 0,
            channel: 0,
        }
    }

    fn with_ancestor(mut self, ancestor: u64) -> Self {
        self.ancestor = ancestor;
        self
    }

    fn with_species(mut self, species: u128) -> Self {
        self.species = species;
        self
    }

    fn with_channel(mut self, channel: u32) -> Self {
        self.channel = channel;
        self
    }
}

struct EvaluationState<'a> {
    graph: &'a CompiledBiomeGraph,
    inputs: &'a GraphEvaluationInputs,
    cancellation: &'a GraphCancellationToken,
    compute: Option<&'a dyn GraphComputeExecutor>,
    execution_plan: &'a GraphExecutionPlan,
    scope: EvaluationScope<'a>,
    pass: EvaluationPass,
    deadline: Instant,
    provenance: ProvenanceTable,
    candidate_decisions: BTreeMap<CandidateIdentity, ProvenanceDecisionHandle>,
    prepared_surface_projections: SurfaceProjectionCache,
    prepared_surface_fields: SurfaceFieldQueryCache,
    ancestor_references: BTreeSet<WorldCellKey>,
    transferred_bytes: u64,
    diagnostics: GraphEvaluationDiagnostics,
    rejected_by_lineage: BTreeMap<CandidateLineage, Vec<RejectedCandidate>>,
    materialized_outputs: BTreeMap<QualifiedGraphPin, GraphValue>,
    current_node_live_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvaluationPass {
    PrepareCanonicalInputs,
    AuthoritativeReplay,
}

#[derive(Clone, Copy)]
struct EvaluationContext<'a> {
    graph: &'a CompiledBiomeGraph,
    cancellation: &'a GraphCancellationToken,
    compute: Option<&'a dyn GraphComputeExecutor>,
    execution_plan: &'a GraphExecutionPlan,
    scope: EvaluationScope<'a>,
    deadline: Instant,
}

/// The single evaluator surface shared by preview, cooking, and runtime generation.
#[derive(Clone)]
pub struct BiomeGraphEvaluator {
    graph: Arc<CompiledBiomeGraph>,
    worker_count: usize,
    compute: Option<Arc<dyn GraphComputeExecutor>>,
}

struct EvaluationPlans {
    execution: GraphExecutionPlan,
    preparation: Option<GraphExecutionPlan>,
    allocation_bytes: u64,
}

fn build_evaluation_plans(
    graph: &CompiledBiomeGraph,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
    guard: PreflightGuard<'_>,
) -> Result<EvaluationPlans> {
    guard.check()?;
    let one_plan_bytes = execution_plan_allocation_bound(graph)?;
    let needs_preparation =
        demand_requires_canonical_preparation(&graph.root, graph.demand_plan().execution_slice())?;
    let plan_count = 1 + u64::from(needs_preparation);
    let allocation_bytes = bound_mul(
        "memory bytes",
        one_plan_bytes,
        plan_count,
        graph.limits.max_memory_bytes,
    )?;
    guard.check()?;
    let execution = build_execution_plan(graph, parallel_cpu, gpu)?;
    guard.check()?;
    let preparation = needs_preparation
        .then(|| build_execution_plan(graph, false, None))
        .transpose()?;
    guard.check()?;
    Ok(EvaluationPlans {
        execution,
        preparation,
        allocation_bytes,
    })
}

impl BiomeGraphEvaluator {
    /// Creates an evaluator with an explicit bounded cell-worker count.
    pub fn new(graph: Arc<CompiledBiomeGraph>, worker_count: usize) -> Result<Self> {
        if worker_count == 0 {
            return Err(Error::GraphLimit {
                resource: "worker count",
                requested: 0,
                limit: 1,
            });
        }
        if worker_count > usize::from(graph.limits.max_workers) {
            return Err(Error::GraphLimit {
                resource: "worker count",
                requested: worker_count as u64,
                limit: u64::from(graph.limits.max_workers),
            });
        }
        Ok(Self {
            graph,
            worker_count,
            compute: None,
        })
    }

    /// Installs the qualified Slang executor used by eligible nodes in the same compiled IR.
    #[must_use]
    pub fn with_compute_executor(mut self, compute: Arc<dyn GraphComputeExecutor>) -> Self {
        self.compute = Some(compute);
        self
    }

    /// Compiled graph used by every evaluation surface.
    #[must_use]
    pub fn graph(&self) -> &CompiledBiomeGraph {
        &self.graph
    }

    /// Predicts and checks the complete job before any worker or GPU dispatch starts.
    pub fn preflight(
        &self,
        inputs: &GraphEvaluationJobInputs,
        cancellation: &GraphCancellationToken,
    ) -> Result<GraphEvaluationPreflight> {
        let deadline = evaluation_deadline(&self.graph)?;
        let guard = PreflightGuard {
            cancellation,
            deadline,
            time_limit_ms: self.graph.limits.max_time_ms,
        };
        guard.check()?;
        check_job_collection_limits(&self.graph, inputs)?;
        guard.check()?;
        let workers = self.worker_count.min(inputs.cells.len().max(1));
        let gpu = self.compute.as_deref().map(|compute| GraphGpuScheduling {
            profile: compute.profile(),
            qualifications: compute.qualifications(),
        });
        let plans = build_evaluation_plans(&self.graph, workers > 1, gpu, guard)?;
        preflight_evaluation_job(
            &self.graph,
            inputs,
            workers,
            &plans.execution,
            plans.allocation_bytes,
            guard,
        )
    }

    /// Evaluates one complete batch and publishes no cells or global tiles on failure.
    pub fn evaluate(
        &self,
        inputs: GraphEvaluationJobInputs,
        cancellation: &GraphCancellationToken,
    ) -> Result<GraphEvaluationJobResult> {
        evaluate_job(
            &self.graph,
            inputs,
            self.worker_count,
            cancellation,
            self.compute.as_deref(),
        )
    }
}

#[cfg(test)]
fn evaluate_cell_reference(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    cancellation: &GraphCancellationToken,
) -> Result<GraphEvaluationResult> {
    let mut result = evaluate_job(
        graph,
        GraphEvaluationJobInputs {
            cells: vec![inputs.clone()],
            global_stages: Vec::new(),
        },
        1,
        cancellation,
        None,
    )?;
    result.cells.pop().ok_or_else(|| Error::GraphDocument {
        path: "evaluation.results".to_owned(),
        reason: "single-cell reference evaluation produced no result".to_owned(),
    })
}

fn evaluate_cell_planned(
    context: &EvaluationContext<'_>,
    inputs: &GraphEvaluationInputs,
    pass: EvaluationPass,
) -> Result<PlannedEvaluation> {
    let graph = context.graph;
    let scope = context.scope;
    let guard = PreflightGuard {
        cancellation: context.cancellation,
        deadline: context.deadline,
        time_limit_ms: graph.limits.max_time_ms,
    };
    validate_static_inputs(graph, inputs, scope.current_global_stage(), guard)?;
    let demand = compiled_demand_slice(graph, scope.current_global_stage())?;
    let root_demand = root_demand_unit(demand)?;
    let mut state = EvaluationState {
        graph,
        inputs,
        cancellation: context.cancellation,
        compute: context.compute,
        execution_plan: context.execution_plan,
        scope,
        pass,
        deadline: context.deadline,
        provenance: ProvenanceTable::default(),
        candidate_decisions: BTreeMap::new(),
        prepared_surface_projections: BTreeMap::new(),
        prepared_surface_fields: BTreeMap::new(),
        ancestor_references: BTreeSet::new(),
        transferred_bytes: 0,
        diagnostics: GraphEvaluationDiagnostics::default(),
        rejected_by_lineage: BTreeMap::new(),
        materialized_outputs: BTreeMap::new(),
        current_node_live_bytes: 0,
    };
    state.check_abort()?;
    let mut outputs = evaluate_unit(
        &graph.root,
        root_demand,
        demand,
        &BTreeMap::new(),
        &mut state,
    )?;
    #[cfg(test)]
    context.cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::AfterTraversal,
        graph.limits.max_time_ms,
    )?;
    state.check_abort()?;
    let mut macro_points = Vec::new();
    let mut micro_fields = Vec::new();
    let mut diagnostic_streams = Vec::new();
    let (macro_count, micro_count, stream_count) = graph.root.outputs.iter().try_fold(
        (0_usize, 0_usize, 0_usize),
        |(macro_count, micro_count, stream_count), output| -> Result<_> {
            match outputs.get(&output.name) {
                Some(GraphValue::Macro(points)) => Ok((
                    macro_count
                        .checked_add(points.len())
                        .ok_or(Error::NumericOverflow)?,
                    micro_count,
                    stream_count,
                )),
                Some(GraphValue::Micro(tiles)) => Ok((
                    macro_count,
                    micro_count
                        .checked_add(tiles.len())
                        .ok_or(Error::NumericOverflow)?,
                    stream_count,
                )),
                Some(GraphValue::Diagnostics(streams)) => Ok((
                    macro_count,
                    micro_count,
                    stream_count
                        .checked_add(streams.len())
                        .ok_or(Error::NumericOverflow)?,
                )),
                _ => Ok((macro_count, micro_count, stream_count)),
            }
        },
    )?;
    crate::memory::reserve_exact(&mut macro_points, macro_count, "terminal macro points")?;
    crate::memory::reserve_exact(&mut micro_fields, micro_count, "terminal micro fields")?;
    crate::memory::reserve_exact(
        &mut diagnostic_streams,
        stream_count,
        "terminal diagnostic streams",
    )?;
    for output in &graph.root.outputs {
        state.check_abort()?;
        let Some(value) = outputs.remove(&output.name) else {
            if state.scope.current_global_stage().is_some() {
                continue;
            }
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "evaluator did not produce output".to_owned(),
            });
        };
        match value {
            GraphValue::Macro(mut points) => macro_points.append(&mut points),
            GraphValue::Micro(mut tiles) => micro_fields.append(&mut tiles),
            GraphValue::Diagnostics(mut streams) => diagnostic_streams.append(&mut streams),
            _ => {}
        }
    }
    drop(outputs);
    state.check_abort()?;
    macro_points.sort_unstable_by_key(|point| point.id);
    micro_fields.sort_unstable_by_key(|tile| (tile.cell, tile.family.value()));
    state.check_abort()?;
    state.diagnostics.accepted_count = macro_points.len() as u64;
    state
        .diagnostics
        .rejected
        .sort_unstable_by_key(rejected_order_key);
    for stream in &mut diagnostic_streams {
        state.check_abort()?;
        if let Some(candidates) = &mut stream.candidates {
            candidates.sort_unstable_by_key(|sample| sample.identity);
        }
        if let Some(field) = &mut stream.field {
            field.sort_unstable_by_key(|sample| sample.candidate);
        }
        stream.rejected.sort_unstable_by_key(rejected_order_key);
    }
    diagnostic_streams.sort_unstable_by(|left, right| {
        diagnostic_stream_order_key(left).cmp(&diagnostic_stream_order_key(right))
    });
    state.check_abort()?;
    state.diagnostics.streams = diagnostic_streams;
    state.check_count(
        "accepted count",
        macro_points.len() as u64,
        graph.limits.max_macro_points,
    )?;
    let columns = PlantPointColumns::from_points(macro_points)?;
    state.check_abort()?;
    let mut surface_projection_tiles = Vec::new();
    crate::memory::reserve_exact(
        &mut surface_projection_tiles,
        state.prepared_surface_projections.len(),
        "prepared surface projection tiles",
    )?;
    for ((node, node_semantic_revision, provider_set_hash), samples) in
        state.prepared_surface_projections
    {
        let mut entries = Vec::new();
        crate::memory::reserve_exact(
            &mut entries,
            samples.len(),
            "prepared surface projection entries",
        )?;
        entries.extend(
            samples
                .into_iter()
                .map(|(query, sample)| QuantizedSurfaceProjectionEntry { query, sample }),
        );
        surface_projection_tiles.push(QuantizedSurfaceProjectionTile {
            node,
            node_semantic_revision,
            samples: entries,
            provider_set_hash,
        });
    }
    let mut surface_field_query_tiles = Vec::new();
    crate::memory::reserve_exact(
        &mut surface_field_query_tiles,
        state.prepared_surface_fields.len(),
        "prepared surface field query tiles",
    )?;
    for ((node, node_semantic_revision, channel, derivative, provider_set_hash), samples) in
        state.prepared_surface_fields
    {
        let mut entries = Vec::new();
        crate::memory::reserve_exact(
            &mut entries,
            samples.len(),
            "prepared surface field query entries",
        )?;
        entries.extend(samples.into_iter().map(|((candidate, query), value)| {
            QuantizedSurfaceFieldQueryEntry {
                candidate,
                query,
                value,
            }
        }));
        surface_field_query_tiles.push(QuantizedSurfaceFieldQueryTile {
            node,
            node_semantic_revision,
            channel,
            derivative,
            samples: entries,
            provider_set_hash,
        });
    }
    let mut ancestor_references = Vec::new();
    crate::memory::reserve_exact(
        &mut ancestor_references,
        state.ancestor_references.len(),
        "ancestor references",
    )?;
    ancestor_references.extend(state.ancestor_references);
    guard.check()?;
    let result = GraphEvaluationResult {
        cell: inputs.output_cell,
        macro_points: columns,
        micro_fields,
        surface_projection_tiles,
        surface_field_query_tiles,
        ancestor_references,
        provenance: state.provenance,
        diagnostics: state.diagnostics,
    };
    #[cfg(test)]
    context.cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::BeforeFinalValidation,
        graph.limits.max_time_ms,
    )?;
    result.validate_canonical_encoding_guarded(guard)?;
    let evaluated = PlannedEvaluation {
        result,
        materialized_outputs: state.materialized_outputs,
        candidate_decisions: state.candidate_decisions,
    };
    guard.check()?;
    Ok(evaluated)
}

fn validate_projection_tile_guarded(
    tile: &QuantizedSurfaceProjectionTile,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    if tile.node == 0 || tile.node_semantic_revision == 0 || tile.provider_set_hash == [0; 32] {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProjectionTiles".to_owned(),
            reason: "projection tile identity or exact query ordering is invalid".to_owned(),
        });
    }
    let mut previous_query = None;
    for entry in &tile.samples {
        guard.check()?;
        if previous_query.is_some_and(|previous| previous >= entry.query) {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile identity or exact query ordering is invalid".to_owned(),
            });
        }
        previous_query = Some(entry.query);
        if let Some(sample) = &entry.sample {
            for pair in sample.tags.windows(2) {
                guard.check()?;
                if pair[0].tag >= pair[1].tag {
                    return Err(Error::GraphDocument {
                        path: "evaluation.surfaceProjectionTiles.tags".to_owned(),
                        reason: "projection sample tags must be sorted and unique".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn validate_field_query_tile_guarded(
    tile: &QuantizedSurfaceFieldQueryTile,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    validate_field_query_tile_with_guard(tile, Some(guard))
}

fn validate_field_query_tile_with_guard(
    tile: &QuantizedSurfaceFieldQueryTile,
    guard: Option<PreflightGuard<'_>>,
) -> Result<()> {
    guard.map_or(Ok(()), PreflightGuard::check)?;
    if tile.node == 0 || tile.node_semantic_revision == 0 || tile.provider_set_hash == [0; 32] {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceFieldQueryTiles".to_owned(),
            reason: "field query tile identity, type, or exact query ordering is invalid"
                .to_owned(),
        });
    }
    let mut previous_query = None;
    for entry in &tile.samples {
        guard.map_or(Ok(()), PreflightGuard::check)?;
        let type_matches = matches!(
            (tile.derivative, entry.value),
            (
                FieldDerivative::Value,
                QuantizedSurfaceFieldValue::Scalar(_)
            ) | (
                FieldDerivative::Gradient,
                QuantizedSurfaceFieldValue::Gradient(_)
            ) | (
                FieldDerivative::Hessian,
                QuantizedSurfaceFieldValue::Hessian(_)
            )
        );
        let query = (entry.candidate, entry.query);
        if !type_matches || previous_query.is_some_and(|previous| previous >= query) {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile identity, type, or exact query ordering is invalid"
                    .to_owned(),
            });
        }
        previous_query = Some(query);
    }
    guard.map_or(Ok(()), PreflightGuard::check)
}

fn validate_static_inputs(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    global_stage: Option<&CompiledGlobalStage>,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    guard.check()?;
    let required_halo = fixed_meters_to_ticks(global_stage.map_or_else(
        || graph.required_halo(inputs.output_cell.level()),
        |stage| stage.upstream_halo,
    ))?
    .unsigned_abs() as i128;
    let required_read_bounds = expand_bounds_checked(inputs.output_bounds, required_halo)?;
    if !bounds_contains_bounds(inputs.read_bounds, required_read_bounds) {
        return Err(Error::GraphAuthoritativeInput {
            node: 0,
            input: format!(
                "immutable halo of {} fixed ticks around {}",
                required_halo, inputs.output_cell
            ),
        });
    }
    for field in &inputs.fields {
        guard.check()?;
        field.validate()?;
        validate_field_dependency(graph, inputs, field)?;
    }
    let mut projection_queries = BTreeSet::new();
    for tile in &inputs.surface_projection_tiles {
        guard.check()?;
        validate_projection_tile_guarded(tile, guard)?;
        if tile.provider_set_hash != inputs.surface_provider_set_hash {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceProjectionTiles".to_owned(),
                reason: "projection tile does not match the declared provider set".to_owned(),
            });
        }
        for entry in &tile.samples {
            guard.check()?;
            if !projection_queries.insert((tile.node, tile.node_semantic_revision, entry.query)) {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceProjectionTiles".to_owned(),
                    reason: "an exact projection query is duplicated".to_owned(),
                });
            }
        }
    }
    let mut field_queries = BTreeSet::new();
    for tile in &inputs.surface_field_query_tiles {
        guard.check()?;
        validate_field_query_tile_guarded(tile, guard)?;
        if tile.provider_set_hash != inputs.surface_provider_set_hash {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile does not match the declared provider set".to_owned(),
            });
        }
        for entry in &tile.samples {
            guard.check()?;
            if !field_queries.insert((
                tile.node,
                tile.node_semantic_revision,
                tile.channel,
                tile.derivative,
                entry.candidate,
                entry.query,
            )) {
                return Err(Error::GraphDocument {
                    path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                    reason: "an exact field query is duplicated".to_owned(),
                });
            }
        }
    }
    if !inputs.surface_providers.is_empty()
        && canonical_surface_provider_set_hash_guarded(
            &inputs.surface_providers,
            graph.limits.max_input_tiles,
            Some(guard),
        )? != inputs.surface_provider_set_hash
    {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviderSetHash".to_owned(),
            reason: "surface provider descriptors do not match the declared set identity"
                .to_owned(),
        });
    }
    for provider in &inputs.surface_providers {
        guard.check()?;
        let descriptor = provider.descriptor();
        let source = GraphDependencySource::SurfaceProvider(descriptor.id.0);
        let Some(dependency) = graph
            .dependencies()
            .iter()
            .find(|dependency| dependency.source == source)
        else {
            continue;
        };
        let actual = canonical_surface_provider_set_hash_guarded(
            &[Arc::clone(provider)],
            graph.limits.max_input_tiles,
            Some(guard),
        )?;
        if actual != dependency.content_hash {
            return Err(Error::GraphAuthoritativeInput {
                node: 0,
                input: format!("content hash for {source:?}"),
            });
        }
    }
    for prototype in &inputs.plant_prototypes {
        guard.check()?;
        prototype.validate()?;
    }
    if inputs
        .plant_prototypes
        .windows(2)
        .any(|pair| pair[0].family.value() >= pair[1].family.value())
    {
        return Err(Error::GraphDocument {
            path: "evaluation.plantPrototypes".to_owned(),
            reason: "plant prototypes must be sorted by unique family identity".to_owned(),
        });
    }
    guard.check()
}

fn validate_field_dependency(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    field: &EvaluationFieldTile,
) -> Result<()> {
    let source = match field.source {
        EvaluationFieldSource::MapLayer(layer) => GraphDependencySource::MapLayer(layer),
        EvaluationFieldSource::SurfaceProvider { provider, revision } => {
            if let Some(descriptor) = inputs
                .surface_providers
                .iter()
                .map(|provider| provider.descriptor())
                .find(|descriptor| descriptor.id == provider)
                && descriptor.revision != revision
            {
                return Err(Error::GraphAuthoritativeInput {
                    node: 0,
                    input: format!("surface provider {} revision", provider.0),
                });
            }
            GraphDependencySource::SurfaceProvider(provider.0)
        }
    };
    let dependency = graph
        .dependencies()
        .iter()
        .find(|dependency| dependency.source == source)
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: 0,
            input: format!("compiled dependency {source:?}"),
        })?;
    if dependency.content_hash != field.source_hash {
        return Err(Error::GraphAuthoritativeInput {
            node: 0,
            input: format!("content hash for {source:?}"),
        });
    }
    Ok(())
}

fn evaluate_cell_atomically(
    mut inputs: GraphEvaluationInputs,
    context: EvaluationContext<'_>,
    preparation_plan: Option<&GraphExecutionPlan>,
) -> Result<PlannedEvaluation> {
    let demand = compiled_demand_slice(context.graph, context.scope.current_global_stage())?;
    if !inputs.surface_providers.is_empty()
        && demand_requires_canonical_preparation(&context.graph.root, demand)?
    {
        let preparation_plan = preparation_plan.ok_or_else(|| Error::GraphDocument {
            path: "evaluation.preparationPlan".to_owned(),
            reason: "canonical surface preparation plan is missing".to_owned(),
        })?;
        let preparation_context = EvaluationContext {
            graph: context.graph,
            cancellation: context.cancellation,
            compute: None,
            execution_plan: preparation_plan,
            scope: context.scope,
            deadline: context.deadline,
        };
        let (surface_projection_tiles, surface_field_query_tiles) = {
            let prepared = evaluate_cell_planned(
                &preparation_context,
                &inputs,
                EvaluationPass::PrepareCanonicalInputs,
            )?;
            let GraphEvaluationResult {
                surface_projection_tiles,
                surface_field_query_tiles,
                ..
            } = prepared.result;
            (surface_projection_tiles, surface_field_query_tiles)
        };
        #[cfg(test)]
        context.cancellation.check_test_checkpoint(
            TestEvaluationCheckpoint::AfterPreparation,
            context.graph.limits.max_time_ms,
        )?;
        inputs.surface_projection_tiles = surface_projection_tiles;
        inputs.surface_field_query_tiles = surface_field_query_tiles;
        check_limit(
            "input tiles",
            evaluation_input_tile_count(&inputs)?,
            context.graph.limits.max_input_tiles,
        )?;
        return evaluate_cell_planned(&context, &inputs, EvaluationPass::AuthoritativeReplay);
    }
    evaluate_cell_planned(&context, &inputs, EvaluationPass::AuthoritativeReplay)
}

fn demand_requires_canonical_preparation(
    unit: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<bool> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| Error::GraphDocument {
            path: "graph.demandPlan".to_owned(),
            reason: "live unit has no preparation demand slice".to_owned(),
        })?;
    for node in unit.nodes.iter().filter(|node| {
        unit_demand.contains_node(node.definition.guid) && demand.executes_node(&node.address())
    }) {
        if node.definition.authority != GraphAuthority::Cosmetic
            && matches!(
                node.definition.operator,
                GraphOperator::SurfaceProjection | GraphOperator::FieldSample
            )
        {
            return Ok(true);
        }
        if let Some(module) = node.module.as_deref()
            && find_child_demand_unit(demand, node).is_some()
            && demand_requires_canonical_preparation(module, demand)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn evaluation_deadline(graph: &CompiledBiomeGraph) -> Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_millis(graph.limits.max_time_ms))
        .ok_or(Error::NumericOverflow)
}

fn check_job_collection_limits(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
) -> Result<()> {
    check_limit(
        "output cells",
        inputs.cells.len() as u64,
        graph.limits.max_output_cells,
    )?;
    check_limit(
        "global stage tiles",
        inputs.global_stages.len() as u64,
        graph.limits.max_global_stage_tiles,
    )
}

fn canonicalize_evaluation_input(input: &mut GraphEvaluationInputs) {
    input.regions.sort_unstable_by_key(|region| {
        (
            region.id,
            region.layer,
            region.kind as u8,
            region.hierarchy_namespace,
            region.seed_cell,
            region.bounds.min_ticks(),
            region.bounds.max_ticks_exclusive(),
        )
    });
    input.splines.sort_unstable_by_key(|spline| spline.id);
    input
        .anchors
        .sort_unstable_by_key(|anchor| (anchor.layer, anchor.point.id));
    input
        .plant_prototypes
        .sort_unstable_by_key(|prototype| prototype.family.value());
    input.fields.sort_unstable_by_key(|tile| {
        (
            tile.channel,
            tile.derivative,
            tile.layer_order,
            tile.source,
            tile.bounds.min_ticks(),
            tile.bounds.max_ticks_exclusive(),
            tile.source_hash,
        )
    });
    input.surface_projection_tiles.sort_unstable_by_key(|tile| {
        (
            tile.node,
            tile.node_semantic_revision,
            tile.samples.first().map(|entry| entry.query),
            tile.samples.last().map(|entry| entry.query),
            tile.provider_set_hash,
        )
    });
    input
        .surface_field_query_tiles
        .sort_unstable_by_key(|tile| {
            (
                tile.node,
                tile.node_semantic_revision,
                tile.channel,
                tile.derivative,
                tile.samples
                    .first()
                    .map(|entry| (entry.candidate, entry.query)),
                tile.samples
                    .last()
                    .map(|entry| (entry.candidate, entry.query)),
                tile.provider_set_hash,
            )
        });
    input
        .surface_providers
        .sort_unstable_by_key(|provider| provider.descriptor().id);
}

#[derive(Clone, Copy, Debug, Default)]
struct SymbolicValueBound {
    domain: Option<GraphDomain>,
    items: u64,
    bytes: u64,
    diagnostic_candidates: u64,
    diagnostic_fields: u64,
    diagnostic_rejected: u64,
    diagnostic_module_path_items: u64,
    diagnostic_label_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct SymbolicEvaluationBound {
    candidate_peak: u64,
    candidate_events: u64,
    accepted: u64,
    micro_samples: u64,
    memory_bytes: u64,
    transfer_bytes: u64,
    rejected: u64,
    provenance_bytes: u64,
    imported_provenance_records: u64,
    diagnostic_metadata_bytes: u64,
    generated_input_tiles: u64,
    generated_input_bytes: u64,
    published_input_tiles: u64,
    published_input_bytes: u64,
    candidate_decision_scratch_bytes: u64,
}

#[derive(Default)]
struct SymbolicGlobalTile {
    outputs: BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    provenance_decisions: u64,
    provenance_records: u64,
    provenance_bytes: u64,
}

impl SymbolicGlobalTile {
    fn requested_memory_bytes(&self) -> Result<u64> {
        requested_btree_with(&self.outputs, qualified_graph_pin_memory, |_| Ok(0))
    }
}

#[derive(Default)]
struct SymbolicGlobalStore {
    tiles: BTreeMap<([u8; 32], WorldCellKey), SymbolicGlobalTile>,
}

impl SymbolicGlobalStore {
    fn requested_memory_bytes(&self) -> Result<u64> {
        requested_btree_with(
            &self.tiles,
            |_| Ok(0),
            SymbolicGlobalTile::requested_memory_bytes,
        )
    }
}

#[derive(Clone, Copy)]
enum SymbolicEvaluationScope<'a> {
    Cell {
        global_store: &'a SymbolicGlobalStore,
    },
    Global {
        stage: &'a CompiledGlobalStage,
        global_store: &'a SymbolicGlobalStore,
    },
}

impl<'a> SymbolicEvaluationScope<'a> {
    fn current_global_stage(self) -> Option<&'a CompiledGlobalStage> {
        match self {
            Self::Cell { .. } => None,
            Self::Global { stage, .. } => Some(stage),
        }
    }

    fn global_store(self) -> &'a SymbolicGlobalStore {
        match self {
            Self::Cell { global_store } | Self::Global { global_store, .. } => global_store,
        }
    }

    const fn is_cell(self) -> bool {
        matches!(self, Self::Cell { .. })
    }
}

fn compiled_demand_slice<'a>(
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

fn root_demand_unit(demand: &CompiledDemandSlice) -> Result<&CompiledDemandUnitSlice> {
    demand.unit(&[]).ok_or_else(|| Error::GraphDocument {
        path: "graph.demandPlan".to_owned(),
        reason: "root unit has no demand slice".to_owned(),
    })
}

fn find_child_demand_unit<'a>(
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

fn child_demand_unit<'a>(
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

struct SymbolicPlannedEvaluation {
    bound: SymbolicEvaluationBound,
    materialized_outputs: BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    public_result_bytes: u64,
    global_tile_bytes: u64,
    ancestor_references: u64,
}

#[derive(Clone, Copy)]
struct SymbolicTraversalContext<'a> {
    graph: &'a CompiledBiomeGraph,
    demand: &'a CompiledDemandSlice,
    scope: SymbolicEvaluationScope<'a>,
    inputs: &'a GraphEvaluationInputs,
    guard: PreflightGuard<'a>,
}

#[derive(Clone, Copy)]
struct PreflightGuard<'a> {
    cancellation: &'a GraphCancellationToken,
    deadline: Instant,
    time_limit_ms: u64,
}

impl PreflightGuard<'_> {
    fn check(self) -> Result<()> {
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
struct SymbolicInputPreflight {
    input_tiles: u64,
    retained_input_bytes: u64,
    generated_input_bytes: u64,
    candidate_count: u64,
    accepted_count: u64,
    micro_samples: u64,
    transfer_bytes: u64,
    active_worker_memory: u64,
    retained_result_bytes: u64,
}

fn requested_vec_bytes<T>(capacity: usize) -> Result<u64> {
    memory_requested_vec_bytes::<T>(capacity)
}

fn requested_slice_bytes<T>(items: usize) -> Result<u64> {
    memory_requested_vec_bytes_for_len::<T>(
        u64::try_from(items).map_err(|_| Error::NumericOverflow)?,
    )
}

fn requested_btree_bytes<K, V>(entries: usize) -> Result<u64> {
    memory_requested_btree_bytes::<K, V>(entries)
}

fn requested_vec_bound<T>(items: u64) -> Result<u64> {
    memory_requested_vec_bytes_for_len::<T>(items)
}

fn requested_btree_bound<K, V>(entries: u64) -> Result<u64> {
    memory_requested_btree_bound::<K, V>(entries)
}

fn weighted_elimination_scratch_bytes(
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

fn micro_output_scratch_bytes(samples: u64, channels: u64) -> Result<u64> {
    let sum_values = requested_vec_bytes_for_len::<i128>(samples)?;
    let output_values = requested_vec_bytes_for_len::<i32>(samples)?;
    checked_memory_sum([
        requested_vec_bytes_for_len::<u64>(samples)?,
        requested_vec_bytes_for_len::<(u128, &ScalarFieldSamples)>(channels)?,
        requested_btree_bound::<u128, Vec<i128>>(channels)?,
        channels
            .checked_mul(sum_values)
            .ok_or(Error::NumericOverflow)?,
        requested_btree_bound::<u128, Vec<i32>>(channels)?,
        channels
            .checked_mul(output_values)
            .ok_or(Error::NumericOverflow)?,
    ])
}

fn blue_noise_scratch_bytes(candidates: u64) -> Result<u64> {
    requested_vec_bytes_for_len::<GraphCandidate>(
        candidates.checked_mul(4).ok_or(Error::NumericOverflow)?,
    )
}

fn stage_region_scratch_bytes(regions: u64) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bound::<(u8, u128, WorldCellKey), EvaluationRegion>(regions)?,
        requested_vec_bound::<EvaluationRegion>(regions)?,
    ])
}

fn projection_preparation_cache_bytes(items: u64, tags_per_item: u64) -> Result<u64> {
    let tag_bytes = requested_vec_bound::<WeightedSurfaceTag>(tags_per_item)?;
    checked_memory_sum([
        requested_btree_bound::<
            SurfaceProjectionCacheKey,
            BTreeMap<WorldPosition, Option<QuantizedSurfaceProjectionSample>>,
        >(1)?,
        requested_btree_bound::<WorldPosition, Option<QuantizedSurfaceProjectionSample>>(items)?,
        items.checked_mul(tag_bytes).ok_or(Error::NumericOverflow)?,
    ])
}

fn field_preparation_cache_bytes(items: u64) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bound::<
            SurfaceFieldQueryCacheKey,
            BTreeMap<(CandidateIdentity, WorldPosition), QuantizedSurfaceFieldValue>,
        >(1)?,
        requested_btree_bound::<(CandidateIdentity, WorldPosition), QuantizedSurfaceFieldValue>(
            items,
        )?,
    ])
}

fn xz_filter_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(CandidateIdentity, i128)>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(
            candidates.checked_mul(2).ok_or(Error::NumericOverflow)?,
        )?,
        requested_btree_bound::<(i128, i128), usize>(candidates)?,
        requested_vec_bytes_for_len::<Option<usize>>(candidates)?,
    ])
}

fn competition_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(CandidateIdentity, i128)>(candidates)?,
        requested_vec_bytes_for_len::<((i128, i128), usize)>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

fn bounds_overlap_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(GraphCandidate, WorldBounds, i128)>(
            candidates.checked_mul(2).ok_or(Error::NumericOverflow)?,
        )?,
        requested_btree_bound::<(i128, i128, i128), usize>(candidates)?,
        requested_vec_bytes_for_len::<Option<usize>>(candidates)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

fn community_blend_scratch_bytes(candidates: u64, palette_entries: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<crate::BiomePaletteEntry>(palette_entries)?,
        requested_vec_bytes_for_len::<(u64, u64)>(palette_entries)?,
        requested_vec_bytes_for_len::<GraphCandidate>(candidates)?,
    ])
}

fn companion_scratch_bytes(
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

fn macro_output_scratch_bytes(candidates: u64) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes_for_len::<(&GraphCandidate, Uuid, PlantId)>(candidates)?,
        requested_vec_bytes_for_len::<PlantPoint>(candidates)?,
    ])
}

#[derive(Clone, Copy, Default)]
struct ResidentGroupAllocationShape {
    invocations: u64,
    inputs: u64,
    instructions: u64,
    curve_instructions: u64,
    curve_points: u64,
    external_inputs: u64,
    external_pin_bytes: u64,
    output_entries: u64,
    output_pin_bytes: u64,
    noise_nodes: u64,
    gradient_nodes: u64,
    candidate_output: bool,
    scalar_output: bool,
}

fn resident_group_scratch_bytes(shape: ResidentGroupAllocationShape) -> Result<u64> {
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
        requested_btree_bound::<(u128, String), ()>(shape.external_inputs)?,
        requested_vec_bytes_for_len::<u8>(shape.external_pin_bytes)?,
        external_key_allocations,
        requested_btree_bound::<(u128, String), GraphValue>(shape.output_entries)?,
        requested_vec_bytes_for_len::<u8>(shape.output_pin_bytes)?,
        output_key_allocations,
        requested_btree_bound::<(u128, String), GraphGpuRegister>(shape.external_inputs)?,
        requested_vec_bytes_for_len::<u8>(shape.external_pin_bytes)?,
        external_key_allocations,
        requested_btree_bound::<u128, ([GraphGpuRegister; 8], [GraphGpuRegister; 3])>(
            shape.noise_nodes,
        )?,
        requested_btree_bound::<u128, ([GraphGpuRegister; 3], [GraphGpuRegister; 3])>(
            shape.gradient_nodes,
        )?,
        requested_btree_bound::<(u128, String), GraphGpuRegister>(shape.instructions)?,
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
            requested_btree_bound::<CandidateIdentity, DecisionScalar>(shape.invocations)?
        } else {
            0
        },
    ])
}

fn checked_memory_sum(values: impl IntoIterator<Item = u64>) -> Result<u64> {
    sum_memory_bytes(values)
}

#[derive(Default)]
struct ExecutionPlanAllocationShape {
    nodes: usize,
    edges: usize,
    pins: usize,
    maximum_module_path_words: usize,
    maximum_pin_name_bytes: usize,
}

fn execution_plan_allocation_bound(graph: &CompiledBiomeGraph) -> Result<u64> {
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

struct TraversalAllocationSummary {
    emitted_pins: usize,
    emitted_path_words: usize,
    emitted_path_allocations: usize,
    emitted_name_bytes: usize,
    executed_nodes: usize,
    executed_path_words: usize,
    executed_path_allocations: usize,
    ancestor_candidate_levels: [bool; HIERARCHY_LEVEL_COUNT],
}

impl Default for TraversalAllocationSummary {
    fn default() -> Self {
        Self {
            emitted_pins: 0,
            emitted_path_words: 0,
            emitted_path_allocations: 0,
            emitted_name_bytes: 0,
            executed_nodes: 0,
            executed_path_words: 0,
            executed_path_allocations: 0,
            ancestor_candidate_levels: [false; HIERARCHY_LEVEL_COUNT],
        }
    }
}

#[derive(Clone, Copy)]
struct NodeOutputDemand<'a> {
    demand: &'a CompiledDemandSlice,
    node: &'a CompiledGraphNode,
}

impl<'a> NodeOutputDemand<'a> {
    const fn new(demand: &'a CompiledDemandSlice, node: &'a CompiledGraphNode) -> Self {
        Self { demand, node }
    }

    fn contains(self, output: &str) -> bool {
        self.demand.output_pins.iter().any(|pin| {
            pin.node.node == self.node.definition.guid
                && pin.node.module_path == self.node.debug_symbol.module_path
                && pin.pin == output
        })
    }
}

struct NodeOutputBuilder<'a, T> {
    demand: NodeOutputDemand<'a>,
    outputs: BTreeMap<String, T>,
}

impl<'a, T> NodeOutputBuilder<'a, T> {
    fn new(demand: NodeOutputDemand<'a>) -> Self {
        Self {
            demand,
            outputs: BTreeMap::new(),
        }
    }

    fn insert(&mut self, output: String, value: T) -> Result<()> {
        if !self
            .demand
            .node
            .outputs
            .iter()
            .any(|schema| schema.name == output)
        {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted unknown pin '{output}'"),
            });
        }
        if !self.demand.contains(&output) {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted undemanded pin '{output}'"),
            });
        }
        if self.outputs.insert(output.clone(), value).is_some() {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted duplicate pin '{output}'"),
            });
        }
        Ok(())
    }

    fn extend(&mut self, outputs: BTreeMap<String, T>) -> Result<()> {
        for (output, value) in outputs {
            self.insert(output, value)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<BTreeMap<String, T>> {
        for output in self
            .demand
            .node
            .outputs
            .iter()
            .filter(|output| self.demand.contains(&output.name))
        {
            if !self.outputs.contains_key(&output.name) {
                return Err(Error::GraphDocument {
                    path: self.demand.node.debug_symbol.label.clone(),
                    reason: format!("operator omitted demanded pin '{}'", output.name),
                });
            }
        }
        Ok(self.outputs)
    }
}

fn traversal_allocation_summary(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
) -> Result<TraversalAllocationSummary> {
    fn visit(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
        summary: &mut TraversalAllocationSummary,
    ) -> Result<()> {
        let module_path = unit
            .nodes
            .first()
            .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
        let unit_demand = demand
            .unit(module_path)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.demandPlan".to_owned(),
                reason: "live unit has no demand slice".to_owned(),
            })?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            if demand.estimates.keys().any(|pin| {
                pin.node.node == node.definition.guid
                    && pin.node.module_path == node.debug_symbol.module_path
                    && node.outputs.iter().any(|output| {
                        output.name == pin.pin
                            && (output.domain == GraphDomain::Candidates
                                || node.definition.operator == GraphOperator::MacroOutput)
                    })
            }) {
                summary.ancestor_candidate_levels[usize::from(node.definition.spatial.level())] =
                    true;
            }
            if demand.executes_node(&node.address()) {
                summary.executed_nodes = summary
                    .executed_nodes
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
                summary.executed_path_words = summary
                    .executed_path_words
                    .checked_add(node.debug_symbol.module_path.len())
                    .ok_or(Error::NumericOverflow)?;
                summary.executed_path_allocations = summary
                    .executed_path_allocations
                    .checked_add(usize::from(!node.debug_symbol.module_path.is_empty()))
                    .ok_or(Error::NumericOverflow)?;
            }
            for output in node
                .outputs
                .iter()
                .filter(|output| NodeOutputDemand::new(demand, node).contains(&output.name))
            {
                summary.emitted_pins = summary
                    .emitted_pins
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_path_words = summary
                    .emitted_path_words
                    .checked_add(node.debug_symbol.module_path.len())
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_path_allocations = summary
                    .emitted_path_allocations
                    .checked_add(usize::from(!node.debug_symbol.module_path.is_empty()))
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_name_bytes = summary
                    .emitted_name_bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?;
            }
            if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
            {
                visit(module, demand, summary)?;
            }
        }
        Ok(())
    }

    let mut summary = TraversalAllocationSummary::default();
    visit(&graph.root, demand, &mut summary)?;
    Ok(summary)
}

fn qualified_output_domain(
    unit: &CompiledGraphUnit,
    pin: &QualifiedGraphPin,
) -> Option<GraphDomain> {
    for node in &unit.nodes {
        if node.definition.guid == pin.node.node
            && node.debug_symbol.module_path == pin.node.module_path
        {
            return node
                .outputs
                .iter()
                .find(|output| output.name == pin.pin)
                .map(|output| output.domain);
        }
        if let Some(domain) = node
            .module
            .as_deref()
            .and_then(|module| qualified_output_domain(module, pin))
        {
            return Some(domain);
        }
    }
    None
}

fn global_stage_has_macro_output(
    graph: &CompiledBiomeGraph,
    stage: &CompiledGlobalStage,
) -> Result<bool> {
    let mut has_macro_output = false;
    for pin in &stage.output_pins {
        let domain =
            qualified_output_domain(&graph.root, pin).ok_or_else(|| Error::GraphDocument {
                path: "graph.spatialPlan".to_owned(),
                reason: "global-stage output pin is absent from the compiled graph".to_owned(),
            })?;
        has_macro_output |= domain == GraphDomain::MacroPoints;
    }
    Ok(has_macro_output)
}

fn ancestor_reference_upper_bound(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    include_global_stage_owners: bool,
) -> Result<u64> {
    let summary = traversal_allocation_summary(graph, graph.demand_plan().execution_slice())?;
    let mut global_macro_levels = [false; HIERARCHY_LEVEL_COUNT];
    if include_global_stage_owners {
        for stage in graph.spatial_plan().global_stages() {
            if global_stage_has_macro_output(graph, stage)? {
                global_macro_levels[usize::from(stage.owner_level)] = true;
            }
        }
    }

    let mut references = 0_u64;
    for level in 0..=MAX_HIERARCHY_LEVEL {
        if global_macro_levels[usize::from(level)] {
            references = references
                .checked_add(world_cell_count_covering_bounds(
                    inputs.read_bounds,
                    level,
                    graph.limits.max_global_stage_tiles,
                )?)
                .ok_or(Error::NumericOverflow)?;
        } else if level > inputs.output_cell.level()
            && summary.ancestor_candidate_levels[usize::from(level)]
        {
            references = references.checked_add(1).ok_or(Error::NumericOverflow)?;
        }
    }
    Ok(references)
}

fn separately_allocated_storage<T>(items: usize, allocations: usize) -> Result<u64> {
    let item_bytes = u64::try_from(items)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(std::mem::size_of::<T>() as u64)
        .ok_or(Error::NumericOverflow)?;
    let allocation_bytes = u64::try_from(allocations)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    item_bytes
        .checked_add(allocation_bytes)
        .ok_or(Error::NumericOverflow)
}

fn named_btree_allocation<K, V>(entries: usize, name_bytes: usize) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bytes::<K, V>(entries)?,
        separately_allocated_storage::<u8>(name_bytes, entries)?,
    ])
}

fn unit_demand_slice<'a>(
    unit: &CompiledGraphUnit,
    demand: &'a CompiledDemandSlice,
) -> Result<&'a CompiledDemandUnitSlice> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    demand
        .unit(module_path)
        .ok_or_else(|| Error::GraphDocument {
            path: "graph.demandPlan".to_owned(),
            reason: "live unit has no traversal demand slice".to_owned(),
        })
}

fn emitted_output_stats(
    node: &CompiledGraphNode,
    demand: &CompiledDemandSlice,
) -> Result<(usize, usize)> {
    node.outputs
        .iter()
        .filter(|output| NodeOutputDemand::new(demand, node).contains(&output.name))
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn retained_output_stats(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    demand: &CompiledDemandSlice,
) -> Result<(usize, usize)> {
    unit.nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .flat_map(|node| {
            node.outputs.iter().filter(move |output| {
                NodeOutputDemand::new(demand, node).contains(&output.name)
                    && (unit_demand.edges.iter().any(|edge| {
                        edge.from_node == node.definition.guid && edge.from_pin == output.name
                    }) || unit.outputs.iter().any(|unit_output| {
                        unit_demand.outputs.contains(&unit_output.name)
                            && unit_output.node == node.definition.guid
                            && unit_output.pin == output.name
                    }))
            })
        })
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn node_input_stats(
    node: &CompiledGraphNode,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<(usize, usize)> {
    unit_demand
        .edges
        .iter()
        .filter(|edge| edge.to_node == node.definition.guid)
        .try_fold((0_usize, 0_usize), |(entries, bytes), edge| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(edge.to_pin.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn unit_output_stats(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<(usize, usize)> {
    unit.outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn incoming_allocation_bytes(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<u64> {
    let nested = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .try_fold(0_u64, |total, node| {
            let edges = unit_demand
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.definition.guid)
                .count();
            total
                .checked_add(requested_vec_bytes::<&crate::GraphEdge>(edges)?)
                .ok_or(Error::NumericOverflow)
        })?;
    checked_memory_sum([
        requested_btree_bytes::<u128, Vec<&crate::GraphEdge>>(unit_demand.nodes.len())?,
        nested,
    ])
}

fn runtime_traversal_allocation_bound(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
    plan: &GraphExecutionPlan,
    ancestor_references: u64,
) -> Result<u64> {
    fn visit(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
        plan: &GraphExecutionPlan,
    ) -> Result<u64> {
        let unit_demand = unit_demand_slice(unit, demand)?;
        let (retained_entries, retained_name_bytes) =
            retained_output_stats(unit, unit_demand, demand)?;
        let resident_nodes = unit
            .nodes
            .iter()
            .filter(|node| {
                unit_demand.contains_node(node.definition.guid)
                    && demand.executes_node(&node.address())
                    && plan.domain_for(&node.debug_symbol.module_path, node.definition.guid)
                        == Some(GraphExecutionDomain::SlangCompute)
            })
            .count();
        let base = checked_memory_sum([
            incoming_allocation_bytes(unit, unit_demand)?,
            named_btree_allocation::<(u128, String), u64>(retained_entries, retained_name_bytes)?,
            named_btree_allocation::<(u128, String), GraphValue>(
                retained_entries,
                retained_name_bytes,
            )?,
            requested_btree_bytes::<u128, ()>(resident_nodes)?,
        ])?;
        let (unit_output_entries, unit_output_name_bytes) = unit_output_stats(unit, unit_demand)?;
        let mut peak = named_btree_allocation::<String, GraphValue>(
            unit_output_entries,
            unit_output_name_bytes,
        )?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            let (input_entries, input_name_bytes) = node_input_stats(node, unit_demand)?;
            let inputs =
                named_btree_allocation::<String, GraphValue>(input_entries, input_name_bytes)?;
            let node_peak = if demand.executes_node(&node.address())
                && plan.domain_for(&node.debug_symbol.module_path, node.definition.guid)
                    == Some(GraphExecutionDomain::SlangCompute)
            {
                let group =
                    plan.group_for(&node.address())
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident node has no execution group".to_owned(),
                        })?;
                let group_contains = |guid| {
                    group.nodes.iter().any(|member| {
                        member.address.module_path == node.debug_symbol.module_path
                            && member.address.node == guid
                    })
                };
                let (boundary_entries, boundary_name_bytes) = unit_demand
                    .edges
                    .iter()
                    .filter(|edge| !group_contains(edge.from_node) && group_contains(edge.to_node))
                    .try_fold((0_usize, 0_usize), |(entries, bytes), edge| {
                        Ok::<_, Error>((
                            entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                            bytes
                                .checked_add(edge.from_pin.len())
                                .ok_or(Error::NumericOverflow)?,
                        ))
                    })?;
                let (output_entries, output_name_bytes) = group
                    .outputs
                    .iter()
                    .filter(|output| output.pin.node.module_path == node.debug_symbol.module_path)
                    .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
                        Ok::<_, Error>((
                            entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                            bytes
                                .checked_add(output.pin.pin.len())
                                .ok_or(Error::NumericOverflow)?,
                        ))
                    })?;
                checked_memory_sum([
                    named_btree_allocation::<(u128, String), GraphValue>(
                        boundary_entries,
                        boundary_name_bytes,
                    )?,
                    named_btree_allocation::<(u128, String), GraphValue>(
                        output_entries,
                        output_name_bytes,
                    )?,
                ])?
            } else if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
                && demand.executes_node(&node.address())
            {
                inputs
                    .checked_add(visit(module, demand, plan)?)
                    .ok_or(Error::NumericOverflow)?
            } else {
                let (output_entries, output_name_bytes) = emitted_output_stats(node, demand)?;
                checked_memory_sum([
                    inputs,
                    named_btree_allocation::<String, GraphValue>(
                        output_entries,
                        output_name_bytes,
                    )?,
                ])?
            };
            peak = peak.max(node_peak);
        }
        base.checked_add(peak).ok_or(Error::NumericOverflow)
    }

    checked_memory_sum([
        visit(&graph.root, demand, plan)?,
        requested_vec_bytes_for_len::<WorldCellKey>(ancestor_references)?,
    ])
}

fn symbolic_traversal_allocation_bound(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
) -> Result<u64> {
    fn visit(unit: &CompiledGraphUnit, demand: &CompiledDemandSlice) -> Result<u64> {
        let unit_demand = unit_demand_slice(unit, demand)?;
        let (emitted_entries, emitted_name_bytes) = unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
            .try_fold((0_usize, 0_usize), |(entries, bytes), node| {
                let (node_entries, node_bytes) = emitted_output_stats(node, demand)?;
                Ok::<_, Error>((
                    entries
                        .checked_add(node_entries)
                        .ok_or(Error::NumericOverflow)?,
                    bytes
                        .checked_add(node_bytes)
                        .ok_or(Error::NumericOverflow)?,
                ))
            })?;
        let base = checked_memory_sum([
            incoming_allocation_bytes(unit, unit_demand)?,
            named_btree_allocation::<(u128, String), SymbolicValueBound>(
                emitted_entries,
                emitted_name_bytes,
            )?,
        ])?;
        let (unit_output_entries, unit_output_name_bytes) = unit_output_stats(unit, unit_demand)?;
        let mut peak = named_btree_allocation::<String, SymbolicValueBound>(
            unit_output_entries,
            unit_output_name_bytes,
        )?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            let (output_entries, output_name_bytes) = emitted_output_stats(node, demand)?;
            let outputs = named_btree_allocation::<String, SymbolicValueBound>(
                output_entries,
                output_name_bytes,
            )?;
            let node_peak = if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
                && demand.executes_node(&node.address())
            {
                let (input_entries, input_name_bytes) = node_input_stats(node, unit_demand)?;
                checked_memory_sum([
                    named_btree_allocation::<String, SymbolicValueBound>(
                        input_entries,
                        input_name_bytes,
                    )?,
                    visit(module, demand)?,
                ])?
            } else {
                outputs
            };
            peak = peak.max(node_peak);
        }
        base.checked_add(peak).ok_or(Error::NumericOverflow)
    }

    let summary = traversal_allocation_summary(graph, demand)?;
    checked_memory_sum([
        visit(&graph.root, demand)?,
        requested_btree_bytes::<QualifiedGraphPin, SymbolicValueBound>(summary.emitted_pins)?,
        separately_allocated_storage::<u128>(
            summary.emitted_path_words,
            summary.emitted_path_allocations,
        )?,
        separately_allocated_storage::<u8>(summary.emitted_name_bytes, summary.emitted_pins)?,
        requested_btree_bytes::<GraphNodeAddress, ()>(summary.executed_nodes)?,
        separately_allocated_storage::<u128>(
            summary.executed_path_words,
            summary.executed_path_allocations,
        )?,
    ])
}

fn bound_add(resource: &'static str, left: u64, right: u64, limit: u64) -> Result<u64> {
    let requested = u128::from(left) + u128::from(right);
    if requested > u128::from(limit) {
        return Err(Error::GraphLimit {
            resource,
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(requested as u64)
}

fn bound_mul(resource: &'static str, left: u64, right: u64, limit: u64) -> Result<u64> {
    let requested = u128::from(left) * u128::from(right);
    if requested > u128::from(limit) {
        return Err(Error::GraphLimit {
            resource,
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(requested as u64)
}

fn symbolic_should_load_global_node(
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

fn global_import_scratch_bytes(
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
        requested_btree_bound::<CandidateIdentity, ()>(candidate_identities)?,
        requested_vec_bound::<ProvenanceDecisionHandle>(candidate_identities)?,
        requested_btree_bound::<ProvenanceDecisionHandle, ()>(candidate_identities)?,
        requested_btree_bound::<ProvenanceDecisionHandle, ()>(provenance_decisions)?,
        requested_vec_bound::<ProvenanceDecisionHandle>(traversal_handles)?,
        requested_btree_bound::<ProvenanceDecisionHandle, usize>(provenance_decisions)?,
        requested_vec_bound::<(ProvenanceDecisionHandle, ProvenanceDecisionHandle)>(
            traversal_handles,
        )?,
        requested_btree_bound::<ProvenanceDecisionHandle, ()>(provenance_decisions)?,
        requested_vec_bound::<ProvenanceDecisionHandle>(provenance_decisions)?,
        requested_btree_bound::<ProvenanceDecisionHandle, ProvenanceDecisionHandle>(
            provenance_decisions,
        )?,
        requested_vec_bound::<ProvenanceDecisionHandle>(provenance_records)?,
        requested_btree_bound::<ProvenanceHandle, ()>(provenance_records)?,
        requested_btree_bound::<ProvenanceHandle, ProvenanceHandle>(provenance_records)?,
        requested_vec_bound::<ProvenanceHandle>(provenance_records)?,
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

fn symbolic_load_global_node_outputs(
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

fn symbolic_vec_value<T>(
    domain: GraphDomain,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let bytes = requested_vec_bound::<T>(items)?;
    check_limit("memory bytes", bytes, limits.max_memory_bytes)?;
    Ok(SymbolicValueBound {
        domain: Some(domain),
        items,
        bytes,
        ..SymbolicValueBound::default()
    })
}

fn symbolic_candidate_value(
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    check_limit("candidate count", items, limits.max_candidates)?;
    symbolic_vec_value::<GraphCandidate>(GraphDomain::Candidates, items, limits)
}

fn symbolic_field_value(
    domain: GraphDomain,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let bytes = match domain {
        GraphDomain::ScalarField => {
            requested_btree_bound::<CandidateIdentity, DecisionScalar>(items)?
        }
        GraphDomain::VectorField => {
            requested_btree_bound::<CandidateIdentity, DecisionVec3>(items)?
        }
        GraphDomain::HessianField => {
            requested_btree_bound::<CandidateIdentity, DecisionHessian3>(items)?
        }
        GraphDomain::SurfaceField => {
            requested_btree_bound::<CandidateIdentity, ProjectedSurfaceSample>(items)?
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

fn symbolic_surface_tags_per_hit(
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

fn symbolic_add_generated_input(
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

fn symbolic_add_published_input(
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

fn symbolic_input<'a>(
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

fn required_symbolic_items(
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

fn symbolic_stage_region_count(
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
        requested_btree_bound::<(u8, u128, WorldCellKey), ()>(region_upper)?,
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

fn symbolic_spline_candidate_count(
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
            requested_vec_bound::<[i128; 3]>(maximum_points)?,
            requested_vec_bound::<SplineSegment>(maximum_points)?,
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

fn symbolic_provenance_bytes_per_decision(node: &CompiledGraphNode) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bound::<ProvenanceDecisionHandle>(3)?,
        requested_vec_bound::<u128>(node.debug_symbol.module_path.len() as u64)?,
    ])
}

fn symbolic_add_rejections(
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

fn symbolic_node_outputs(
    context: SymbolicTraversalContext<'_>,
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    incoming: &BTreeMap<u128, Vec<&crate::GraphEdge>>,
    values: &BTreeMap<(u128, String), SymbolicValueBound>,
    bound: &mut SymbolicEvaluationBound,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    use GraphOperator as O;
    let inputs = context.inputs;
    let limits = context.graph.limits;
    let candidate_input = || required_symbolic_items(node, incoming, values, "candidates");
    let field_input = |pin| required_symbolic_items(node, incoming, values, pin);
    let candidate = |items| symbolic_candidate_value(items, limits);
    let scalar = |items| symbolic_field_value(GraphDomain::ScalarField, items, limits);
    let singleton = |name: &str, value| BTreeMap::from([(name.to_owned(), value)]);
    let outputs = match node.definition.operator {
        O::InterfaceInput | O::ModuleCall => unreachable!("handled by symbolic unit traversal"),
        O::RegionInput => {
            let regions = if inputs.regions.is_empty() {
                cell_region_count(inputs.read_bounds, inputs.output_cell.level())?
            } else {
                inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .count()
            };
            singleton(
                "regions",
                symbolic_vec_value::<EvaluationRegion>(
                    GraphDomain::Regions,
                    regions as u64,
                    limits,
                )?,
            )
        }
        O::SplineInput => {
            let points = inputs.splines.iter().try_fold(0_u64, |total, spline| {
                bound_add(
                    "memory bytes",
                    total,
                    spline.points.len() as u64,
                    limits.max_memory_bytes / 64,
                )
            })?;
            singleton(
                "splines",
                SymbolicValueBound {
                    domain: Some(GraphDomain::Splines),
                    items: inputs.splines.len() as u64,
                    bytes: checked_memory_sum([
                        requested_vec_bound::<EvaluationSpline>(inputs.splines.len() as u64)?,
                        requested_vec_bound::<WorldPosition>(points)?,
                        (inputs.splines.len() as u64)
                            .checked_mul(ALLOCATION_OVERHEAD_BYTES)
                            .ok_or(Error::NumericOverflow)?,
                    ])?,
                    ..SymbolicValueBound::default()
                },
            )
        }
        O::SpeciesInput => singleton(
            "species",
            symbolic_vec_value::<crate::BiomePaletteEntry>(
                GraphDomain::SpeciesTable,
                unit.palette.len() as u64,
                limits,
            )?,
        ),
        O::CommunityInput => singleton(
            "communities",
            SymbolicValueBound {
                domain: Some(GraphDomain::CommunityTable),
                items: 1,
                bytes: checked_memory_sum([
                    requested_vec_bytes_for_len::<crate::CompetitionRule>(
                        unit.competition.len() as u64
                    )?,
                    requested_vec_bytes_for_len::<crate::CompanionRule>(
                        unit.companions.len() as u64
                    )?,
                    requested_vec_bytes_for_len::<crate::SuccessionRule>(
                        unit.succession.len() as u64
                    )?,
                ])?,
                ..SymbolicValueBound::default()
            },
        ),
        O::ExplicitAnchors => {
            let layer = guid_parameter(node, "layer", 0)?;
            let count = inputs
                .anchors
                .iter()
                .filter(|anchor| {
                    anchor.layer == layer && inputs.read_bounds.contains(anchor.point.position)
                })
                .count() as u64;
            singleton("candidates", candidate(count)?)
        }
        O::StratifiedCoverage => {
            let region_inputs = required_symbolic_items(node, incoming, values, "regions")?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                stage_region_scratch_bytes(region_inputs)?,
                limits.max_memory_bytes,
            )?;
            let regions = symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let count = u64::from(u32_parameter(node, "count", 0)?);
            let items = bound_mul("candidate count", regions, count, limits.max_candidates)?;
            singleton("candidates", candidate(items)?)
        }
        O::BlueNoisePoisson => {
            let region_inputs = required_symbolic_items(node, incoming, values, "regions")?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                stage_region_scratch_bytes(region_inputs)?,
                limits.max_memory_bytes,
            )?;
            let regions = symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let count = u64::from(u32_parameter(node, "count", 0)?);
            let items = bound_mul("candidate count", regions, count, limits.max_candidates)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                blue_noise_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::SurfaceProjection => {
            let items = candidate_input()?;
            let output_demand = NodeOutputDemand::new(context.demand, node);
            symbolic_add_rejections(bound, node, items, limits)?;
            let tags_per_hit = symbolic_surface_tags_per_hit(node, inputs)?;
            let tag_bytes = requested_vec_bound::<WeightedSurfaceTag>(tags_per_hit)?;
            let has_matching_tile = inputs.surface_projection_tiles.iter().any(|tile| {
                tile.node == node.definition.guid
                    && tile.node_semantic_revision == node.definition.semantic_revision
            });
            if items > 0 && node.definition.authority != GraphAuthority::Cosmetic {
                let tile_bytes_per_item =
                    requested_vec_bound::<QuantizedSurfaceProjectionEntry>(1)?;
                let tile_bytes_per_item = bound_add(
                    "memory bytes",
                    tile_bytes_per_item,
                    tag_bytes,
                    limits.max_memory_bytes,
                )?;
                let tile_bytes = bound_mul(
                    "memory bytes",
                    items,
                    tile_bytes_per_item,
                    limits.max_memory_bytes,
                )?;
                if !inputs.surface_providers.is_empty() || has_matching_tile {
                    bound.memory_bytes = bound_add(
                        "memory bytes",
                        bound.memory_bytes,
                        projection_preparation_cache_bytes(items, tags_per_hit)?,
                        limits.max_memory_bytes,
                    )?;
                    symbolic_add_published_input(bound, tile_bytes, limits)?;
                }
                if !inputs.surface_providers.is_empty() {
                    symbolic_add_generated_input(bound, tile_bytes, limits)?;
                }
            }
            let mut outputs = BTreeMap::new();
            if output_demand.contains("candidates") {
                outputs.insert("candidates".to_owned(), candidate(items)?);
            }
            if output_demand.contains("surface") {
                let mut surface = symbolic_field_value(GraphDomain::SurfaceField, items, limits)?;
                surface.bytes = bound_add(
                    "memory bytes",
                    surface.bytes,
                    bound_mul("memory bytes", items, tag_bytes, limits.max_memory_bytes)?,
                    limits.max_memory_bytes,
                )?;
                outputs.insert("surface".to_owned(), surface);
            }
            outputs
        }
        O::FieldSample => {
            let items = candidate_input()?;
            let domain = node
                .outputs
                .iter()
                .find(|output| output.name == "field")
                .map(|output| output.domain)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "field-sample output schema is missing".to_owned(),
                })?;
            let has_matching_tile = inputs.surface_field_query_tiles.iter().any(|tile| {
                tile.node == node.definition.guid
                    && tile.node_semantic_revision == node.definition.semantic_revision
            });
            if items > 0 && node.definition.authority != GraphAuthority::Cosmetic {
                let tile_bytes = requested_vec_bound::<QuantizedSurfaceFieldQueryEntry>(items)?;
                if !inputs.surface_providers.is_empty() || has_matching_tile {
                    bound.memory_bytes = bound_add(
                        "memory bytes",
                        bound.memory_bytes,
                        field_preparation_cache_bytes(items)?,
                        limits.max_memory_bytes,
                    )?;
                    symbolic_add_published_input(bound, tile_bytes, limits)?;
                }
                if !inputs.surface_providers.is_empty() {
                    symbolic_add_generated_input(bound, tile_bytes, limits)?;
                }
            }
            singleton("field", symbolic_field_value(domain, items, limits)?)
        }
        O::PaintedTile | O::Noise | O::Gradient | O::DistanceField => {
            singleton("field", scalar(candidate_input()?)?)
        }
        O::Curve | O::Remap | O::Clamp => singleton("field", scalar(field_input("field")?)?),
        O::Combine => singleton(
            "field",
            scalar(field_input("left")?.max(field_input("right")?))?,
        ),
        O::WeightedElimination => {
            let input = candidate_input()?;
            let items = input.min(u64::from(u32_parameter(node, "targetCount", 0)?));
            let scratch = weighted_elimination_scratch_bytes(
                input,
                items,
                u64::from(u32_parameter(node, "maximumNeighbours", 0)?),
            )?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                scratch,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, input, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::VariableSpacing | O::PriorityExclusion => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                xz_filter_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::Competition => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                competition_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::BoundsOverlap => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                bounds_overlap_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::CommunityBlend => {
            let items = candidate_input()?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                community_blend_scratch_bytes(items, unit.palette.len() as u64)?,
                limits.max_memory_bytes,
            )?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::FieldImportance | O::Suitability => {
            let items = candidate_input()?;
            symbolic_add_rejections(bound, node, items, limits)?;
            singleton("candidates", candidate(items)?)
        }
        O::ClusterPatchColony => {
            let factor = u64::from(u32_parameter(node, "children", 0)?) + 1;
            let items = bound_mul(
                "candidate count",
                candidate_input()?,
                factor,
                limits.max_candidates,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::RecursiveCompanion => {
            let children = u64::from(u32_parameter(node, "children", 0)?);
            let depth = u32_parameter(node, "maximumDepth", 0)?;
            let mut generation = 1_u64;
            let mut factor = 1_u64;
            for _ in 0..depth {
                generation = bound_mul(
                    "candidate count",
                    generation,
                    children,
                    limits.max_candidates,
                )?;
                factor = bound_add("candidate count", factor, generation, limits.max_candidates)?;
            }
            let items = bound_mul(
                "candidate count",
                candidate_input()?,
                factor,
                limits.max_candidates,
            )?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                companion_scratch_bytes(candidate_input()?, items, unit.companions.len() as u64)?,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::SplineFollow => {
            let items = symbolic_spline_candidate_count(node, inputs, limits, bound)?;
            let region_inputs = if inputs.regions.is_empty() {
                u64::try_from(cell_region_count(
                    inputs.read_bounds,
                    inputs.output_cell.level(),
                )?)
                .map_err(|_| Error::NumericOverflow)?
            } else {
                inputs.regions.len() as u64
            };
            let stage_regions =
                symbolic_stage_region_count(node, inputs, context.scope, bound, limits)?;
            let points = inputs.splines.iter().try_fold(0_u64, |total, spline| {
                total
                    .checked_add(spline.points.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?;
            let scratch = checked_memory_sum([
                stage_region_scratch_bytes(region_inputs)?,
                requested_vec_bound::<EvaluationRegion>(stage_regions)?,
                requested_vec_bound::<[i128; 3]>(points)?,
                requested_vec_bound::<SplineSegment>(points)?,
                requested_vec_bound::<[i128; 3]>(items)?,
                requested_vec_bound::<GraphCandidate>(items)?,
            ])?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                scratch,
                limits.max_memory_bytes,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::Transform | O::SuccessionInput => {
            singleton("candidates", candidate(candidate_input()?)?)
        }
        O::MacroOutput => {
            let items = candidate_input()?;
            check_limit("accepted count", items, limits.max_macro_points)?;
            symbolic_add_rejections(bound, node, items, limits)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                macro_output_scratch_bytes(items)?,
                limits.max_memory_bytes,
            )?;
            singleton(
                "points",
                symbolic_vec_value::<PlantPoint>(GraphDomain::MacroPoints, items, limits)?,
            )
        }
        O::MicroOutput => {
            let dimensions = u32_vec3_parameter(node, "dimensions")?;
            let mut samples_per_family = 1_u64;
            for dimension in dimensions {
                samples_per_family = bound_mul(
                    "micro samples",
                    samples_per_family,
                    u64::from(dimension),
                    limits.max_micro_samples,
                )?;
            }
            let families = candidate_input()?;
            let samples = bound_mul(
                "micro samples",
                samples_per_family,
                families,
                limits.max_micro_samples,
            )?;
            let channels = guid_list_parameter(node, "attributeChannels")?.len() as u64;
            let attribute_maps = channels
                .checked_mul(families)
                .ok_or(Error::NumericOverflow)?;
            let attribute_values = bound_mul(
                "memory bytes",
                channels,
                requested_vec_bound::<i32>(samples)?,
                limits.max_memory_bytes,
            )?;
            let bytes = checked_memory_sum([
                requested_vec_bound::<MicroFieldTile>(families)?,
                requested_vec_bound::<u16>(samples)?,
                requested_btree_bound::<u128, Vec<i32>>(attribute_maps)?,
                attribute_values,
            ])?;
            check_limit("memory bytes", bytes, limits.max_memory_bytes)?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                micro_output_scratch_bytes(samples, channels)?,
                limits.max_memory_bytes,
            )?;
            singleton(
                "micro",
                SymbolicValueBound {
                    domain: Some(GraphDomain::MicroField),
                    items: samples,
                    bytes,
                    ..SymbolicValueBound::default()
                },
            )
        }
        O::DiagnosticOutput => {
            let candidates = symbolic_input(node, incoming, values, "candidates")?
                .map_or(0, |value| value.items);
            let field =
                symbolic_input(node, incoming, values, "field")?.map_or(0, |value| value.items);
            let candidate_bytes = requested_vec_bound::<DiagnosticCandidateSample>(candidates)?;
            let field_bytes = requested_vec_bound::<DiagnosticScalarSample>(field)?;
            let rejected_bytes = requested_vec_bound::<RejectedCandidate>(bound.rejected)?;
            let label_bytes = string_parameter(node, "label", "")?.len() as u64;
            let container_bytes = checked_memory_sum([
                requested_vec_bound::<NamedDiagnosticStream>(1)?,
                requested_vec_bound::<u128>(node.debug_symbol.module_path.len() as u64)?,
                requested_vec_bound::<u8>(label_bytes)?,
            ])?;
            singleton(
                "diagnostics",
                SymbolicValueBound {
                    domain: Some(GraphDomain::Diagnostics),
                    items: 1,
                    bytes: checked_memory_sum([
                        candidate_bytes,
                        field_bytes,
                        rejected_bytes,
                        container_bytes,
                    ])?,
                    diagnostic_candidates: candidates,
                    diagnostic_fields: field,
                    diagnostic_rejected: bound.rejected,
                    diagnostic_module_path_items: node.debug_symbol.module_path.len() as u64,
                    diagnostic_label_bytes: label_bytes,
                },
            )
        }
    };
    Ok(outputs)
}

fn symbolic_evaluate_unit(
    context: SymbolicTraversalContext<'_>,
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    interface_values: &BTreeMap<String, SymbolicValueBound>,
    bound: &mut SymbolicEvaluationBound,
    values_by_pin: &mut BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    executed_nodes: &mut BTreeSet<GraphNodeAddress>,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    let graph = context.graph;
    let scope = context.scope;
    let inputs = context.inputs;
    let limits = graph.limits;
    let mut incoming = BTreeMap::<u128, Vec<_>>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        let edge_count = unit_demand
            .edges
            .iter()
            .filter(|edge| edge.to_node == node.definition.guid)
            .count();
        let mut edges = Vec::new();
        crate::memory::reserve_exact(&mut edges, edge_count, "symbolic incoming graph edges")?;
        incoming.insert(node.definition.guid, edges);
    }
    for edge in &unit_demand.edges {
        incoming
            .get_mut(&edge.to_node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.symbolicBound".to_owned(),
                reason: "compiled graph edge targets an unknown node".to_owned(),
            })?
            .push(edge);
    }
    let mut values = BTreeMap::<(u128, String), SymbolicValueBound>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        context.guard.check()?;
        let loads_global = symbolic_should_load_global_node(graph, scope, &node.address());
        if !loads_global {
            let input_clone_bytes = incoming
                .get(&node.definition.guid)
                .into_iter()
                .flatten()
                .try_fold(0_u64, |total, edge| {
                    let value = values
                        .get(&(edge.from_node, edge.from_pin.clone()))
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: format!("symbolic input '{}' is missing", edge.to_pin),
                        })?;
                    bound_add("memory bytes", total, value.bytes, limits.max_memory_bytes)
                })?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                input_clone_bytes,
                limits.max_memory_bytes,
            )?;
        }
        let outputs = if loads_global {
            symbolic_load_global_node_outputs(graph, context.demand, scope, inputs, node, bound)?
        } else {
            executed_nodes.insert(node.address());
            let produced = match node.definition.operator {
                GraphOperator::InterfaceInput => {
                    let name = string_parameter(node, "name", "")?;
                    BTreeMap::from([(
                        "value".to_owned(),
                        *interface_values
                            .get(name)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: format!("symbolic module input '{name}' is missing"),
                            })?,
                    )])
                }
                GraphOperator::ModuleCall => {
                    let module = node.module.as_deref().ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "compiled module is missing".to_owned(),
                    })?;
                    let module_inputs = incoming
                        .get(&node.definition.guid)
                        .into_iter()
                        .flatten()
                        .map(|edge| {
                            Ok((
                                edge.to_pin.clone(),
                                *values
                                    .get(&(edge.from_node, edge.from_pin.clone()))
                                    .ok_or_else(|| Error::GraphDocument {
                                        path: node.debug_symbol.label.clone(),
                                        reason: format!(
                                            "symbolic module input '{}' is missing",
                                            edge.to_pin
                                        ),
                                    })?,
                            ))
                        })
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    let child_demand = child_demand_unit(context.demand, node)?;
                    symbolic_evaluate_unit(
                        context,
                        module,
                        child_demand,
                        &module_inputs,
                        bound,
                        values_by_pin,
                        executed_nodes,
                    )?
                }
                _ => symbolic_node_outputs(context, unit, node, &incoming, &values, bound)?,
            };
            let mut outputs = NodeOutputBuilder::new(NodeOutputDemand::new(context.demand, node));
            outputs.extend(produced)?;
            outputs.finish()?
        };
        if !loads_global {
            let metadata_bytes = checked_memory_sum([
                requested_vec_bound::<NodeEvaluationDiagnostic>(1)?,
                requested_vec_bound::<u128>(node.debug_symbol.module_path.len() as u64)?,
                requested_vec_bound::<u8>(node.debug_symbol.label.len() as u64)?,
            ])?;
            bound.diagnostic_metadata_bytes = bound_add(
                "memory bytes",
                bound.diagnostic_metadata_bytes,
                metadata_bytes,
                limits.max_memory_bytes,
            )?;
        }
        let projection_decision_items =
            if !loads_global && node.definition.operator == GraphOperator::SurfaceProjection {
                outputs.values().map(|value| value.items).max().unwrap_or(0)
            } else {
                0
            };
        if projection_decision_items != 0 {
            bound.candidate_decision_scratch_bytes =
                bound
                    .candidate_decision_scratch_bytes
                    .max(requested_btree_bound::<CandidateIdentity, ()>(
                        projection_decision_items,
                    )?);
            bound.candidate_events = bound_add(
                "diagnostic samples",
                bound.candidate_events,
                projection_decision_items,
                u64::MAX,
            )?;
            let provenance = bound_mul(
                "memory bytes",
                projection_decision_items,
                symbolic_provenance_bytes_per_decision(node)?,
                limits.max_memory_bytes,
            )?;
            bound.provenance_bytes = bound_add(
                "memory bytes",
                bound.provenance_bytes,
                provenance,
                limits.max_memory_bytes,
            )?;
        }
        for (pin, value) in outputs {
            let candidate_items = if value.domain == Some(GraphDomain::Candidates) {
                value.items
            } else {
                0
            };
            if candidate_items != 0 {
                bound.candidate_decision_scratch_bytes = bound
                    .candidate_decision_scratch_bytes
                    .max(requested_btree_bound::<CandidateIdentity, ()>(
                        candidate_items,
                    )?);
            }
            bound.candidate_peak = bound.candidate_peak.max(candidate_items);
            if !loads_global && node.definition.operator != GraphOperator::SurfaceProjection {
                bound.candidate_events = bound_add(
                    "diagnostic samples",
                    bound.candidate_events,
                    candidate_items,
                    u64::MAX,
                )?;
                let provenance = bound_mul(
                    "memory bytes",
                    candidate_items,
                    symbolic_provenance_bytes_per_decision(node)?,
                    limits.max_memory_bytes,
                )?;
                bound.provenance_bytes = bound_add(
                    "memory bytes",
                    bound.provenance_bytes,
                    provenance,
                    limits.max_memory_bytes,
                )?;
            }
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                value.bytes,
                limits.max_memory_bytes,
            )?;
            values_by_pin.insert(
                QualifiedGraphPin {
                    node: node.address(),
                    pin: pin.clone(),
                },
                value,
            );
            values.insert((node.definition.guid, pin), value);
        }
    }
    let mut outputs = BTreeMap::new();
    for output in &unit.outputs {
        if !unit_demand.outputs.contains(&output.name) {
            continue;
        }
        if let Some(value) = values.get(&(output.node, output.pin.clone())) {
            outputs.insert(output.name.clone(), *value);
        } else {
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "symbolic output is missing".to_owned(),
            });
        }
    }
    let unit_output_clone_bytes = outputs.values().try_fold(0_u64, |total, value| {
        bound_add("memory bytes", total, value.bytes, limits.max_memory_bytes)
    })?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        unit_output_clone_bytes,
        limits.max_memory_bytes,
    )?;
    if unit.role == crate::BiomeRole::Root {
        for value in outputs.values() {
            match value.domain {
                Some(GraphDomain::MacroPoints) => {
                    bound.accepted = bound_add(
                        "accepted count",
                        bound.accepted,
                        value.items,
                        limits.max_macro_points,
                    )?;
                }
                Some(GraphDomain::MicroField) => {
                    bound.micro_samples = bound_add(
                        "micro samples",
                        bound.micro_samples,
                        value.items,
                        limits.max_micro_samples,
                    )?;
                }
                _ => {}
            }
        }
    }
    Ok(outputs)
}

fn symbolic_evaluation_bound(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    plan: &GraphExecutionPlan,
    scope: SymbolicEvaluationScope<'_>,
    guard: PreflightGuard<'_>,
) -> Result<SymbolicPlannedEvaluation> {
    guard.check()?;
    let ancestor_references = ancestor_reference_upper_bound(graph, inputs, scope.is_cell())?;
    let mut bound = SymbolicEvaluationBound::default();
    let mut values_by_pin = BTreeMap::new();
    let mut executed_nodes = BTreeSet::new();
    let demand = compiled_demand_slice(graph, scope.current_global_stage())?;
    let context = SymbolicTraversalContext {
        graph,
        demand,
        scope,
        inputs,
        guard,
    };
    let public_outputs = symbolic_evaluate_unit(
        context,
        &graph.root,
        root_demand_unit(demand)?,
        &BTreeMap::new(),
        &mut bound,
        &mut values_by_pin,
        &mut executed_nodes,
    )?;
    let terminal_micro_tiles = public_outputs
        .values()
        .filter(|value| value.domain == Some(GraphDomain::MicroField))
        .try_fold(0_u64, |total, value| {
            total.checked_add(value.items).ok_or(Error::NumericOverflow)
        })?;
    let terminal_diagnostic_streams = public_outputs
        .values()
        .filter(|value| value.domain == Some(GraphDomain::Diagnostics))
        .try_fold(0_u64, |total, value| {
            total.checked_add(value.items).ok_or(Error::NumericOverflow)
        })?;
    let result_assembly_bytes = checked_memory_sum([
        requested_vec_bound::<PlantPoint>(bound.accepted)?,
        requested_vec_bound::<MicroFieldTile>(terminal_micro_tiles)?,
        requested_vec_bound::<NamedDiagnosticStream>(terminal_diagnostic_streams)?,
        requested_vec_bound::<QuantizedSurfaceProjectionTile>(bound.published_input_tiles)?,
        requested_vec_bound::<QuantizedSurfaceFieldQueryTile>(bound.published_input_tiles)?,
        requested_vec_bound::<WorldCellKey>(ancestor_references)?,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        result_assembly_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let rejection_bytes = requested_vec_bound::<RejectedCandidate>(bound.rejected)?;
    let rejection_lineage_bytes = checked_memory_sum([
        requested_btree_bound::<CandidateLineage, Vec<RejectedCandidate>>(bound.rejected)?,
        requested_vec_bound::<RejectedCandidate>(bound.rejected)?,
        bound
            .rejected
            .checked_mul(ALLOCATION_OVERHEAD_BYTES)
            .ok_or(Error::NumericOverflow)?,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        bound.candidate_decision_scratch_bytes,
        graph.limits.max_memory_bytes,
    )?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        checked_memory_sum([rejection_bytes, rejection_lineage_bytes])?,
        graph.limits.max_memory_bytes,
    )?;
    let provenance_records = bound
        .accepted
        .checked_add(bound.rejected)
        .and_then(|records| records.checked_add(bound.imported_provenance_records))
        .ok_or(Error::NumericOverflow)?;
    let retained_state_bytes = checked_memory_sum([
        requested_vec_bound::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bound::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
        requested_btree_bound::<CandidateIdentity, ProvenanceDecisionHandle>(
            bound.candidate_events,
        )?,
        requested_btree_bound::<WorldCellKey, ()>(ancestor_references)?,
        bound.diagnostic_metadata_bytes,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        retained_state_bytes,
        graph.limits.max_memory_bytes,
    )?;
    for group in plan.groups.iter().filter(|group| {
        group.domain == GraphExecutionDomain::SlangCompute
            && group
                .nodes
                .iter()
                .any(|node| executed_nodes.contains(&node.address))
    }) {
        if group
            .nodes
            .iter()
            .any(|node| !executed_nodes.contains(&node.address))
        {
            return Err(Error::GraphDocument {
                path: "graph.symbolicBound.gpuGroup".to_owned(),
                reason: "resident group crosses the active symbolic spatial boundary".to_owned(),
            });
        }
        let group_metadata = group.nodes.iter().try_fold(
            checked_memory_sum([
                requested_vec_bound::<GpuGroupEvaluationDiagnostic>(1)?,
                requested_vec_bound::<GraphNodeAddress>(group.nodes.len() as u64)?,
            ])?,
            |total, node| {
                total
                    .checked_add(requested_vec_bound::<u128>(
                        node.address.module_path.len() as u64
                    )?)
                    .ok_or(Error::NumericOverflow)
            },
        )?;
        bound.diagnostic_metadata_bytes = bound_add(
            "memory bytes",
            bound.diagnostic_metadata_bytes,
            group_metadata,
            graph.limits.max_memory_bytes,
        )?;
        let invocations = group
            .inputs
            .iter()
            .filter_map(|input| values_by_pin.get(&input.pin))
            .map(|value| value.items)
            .max()
            .unwrap_or(0);
        if invocations == 0 {
            continue;
        }
        let scalar_inputs = group
            .inputs
            .iter()
            .filter(|input| input.domain == GraphDomain::ScalarField)
            .count() as u64;
        let synthetic_inputs = group.nodes.iter().try_fold(1_u64, |total, node| {
            let extra = match node.operator {
                GraphOperator::Noise => 11,
                GraphOperator::Gradient => 6,
                _ => 0,
            };
            total.checked_add(extra).ok_or(Error::NumericOverflow)
        })?;
        let input_count = scalar_inputs
            .checked_add(synthetic_inputs)
            .ok_or(Error::NumericOverflow)?;
        let curve_instructions = group
            .nodes
            .iter()
            .filter(|node| node.operator == GraphOperator::Curve)
            .count() as u64;
        let allocation_shape = ResidentGroupAllocationShape {
            invocations,
            inputs: input_count,
            instructions: group.nodes.len() as u64,
            curve_instructions,
            curve_points: curve_instructions
                .checked_mul(GRAPH_GPU_MAX_CURVE_POINTS as u64)
                .ok_or(Error::NumericOverflow)?,
            external_inputs: group.inputs.len() as u64,
            external_pin_bytes: group.inputs.iter().try_fold(0_u64, |total, input| {
                total
                    .checked_add(input.pin.pin.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?,
            output_entries: group.outputs.len() as u64,
            output_pin_bytes: group.outputs.iter().try_fold(0_u64, |total, output| {
                total
                    .checked_add(output.pin.pin.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?,
            noise_nodes: group
                .nodes
                .iter()
                .filter(|node| node.operator == GraphOperator::Noise)
                .count() as u64,
            gradient_nodes: group
                .nodes
                .iter()
                .filter(|node| node.operator == GraphOperator::Gradient)
                .count() as u64,
            candidate_output: group
                .outputs
                .iter()
                .any(|output| output.domain == GraphDomain::Candidates),
            scalar_output: group
                .outputs
                .iter()
                .any(|output| output.domain == GraphDomain::ScalarField),
        };
        bound.memory_bytes = bound_add(
            "memory bytes",
            bound.memory_bytes,
            resident_group_scratch_bytes(allocation_shape)?,
            graph.limits.max_memory_bytes,
        )?;
        let program_words = (GRAPH_GPU_PROGRAM_HEADER_WORDS as u64)
            .checked_add(input_count)
            .and_then(|words| {
                words.checked_add((group.nodes.len() as u64) * (GRAPH_GPU_INSTRUCTION_WORDS as u64))
            })
            .ok_or(Error::NumericOverflow)?;
        let invocation_words = bound_mul(
            "transfer bytes",
            invocations,
            input_count
                .checked_mul(GRAPH_GPU_INVOCATION_WORDS as u64)
                .ok_or(Error::NumericOverflow)?,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let output_words = bound_mul(
            "transfer bytes",
            invocations,
            GRAPH_GPU_OUTPUT_WORDS as u64,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let words = bound_add(
            "transfer bytes",
            bound_add(
                "transfer bytes",
                program_words,
                invocation_words,
                graph.limits.max_transfer_bytes / 4,
            )?,
            output_words,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let bytes = bound_mul("transfer bytes", words, 4, graph.limits.max_transfer_bytes)?;
        bound.transfer_bytes = bound_add(
            "transfer bytes",
            bound.transfer_bytes,
            bytes,
            graph.limits.max_transfer_bytes,
        )?;
    }
    let materialized_outputs = scope
        .current_global_stage()
        .map_or_else(BTreeMap::new, |stage| {
            stage
                .output_pins
                .iter()
                .filter_map(|pin| {
                    values_by_pin
                        .get(pin)
                        .copied()
                        .map(|value| (pin.clone(), value))
                })
                .collect()
        });
    let rejection_history_bytes = requested_vec_bound::<RejectedCandidate>(bound.rejected)?;
    let terminal_value_bytes = public_outputs
        .values()
        .filter(|value| {
            matches!(
                value.domain,
                Some(GraphDomain::MicroField | GraphDomain::Diagnostics)
            )
        })
        .try_fold(0_u64, |total, value| {
            bound_add(
                "memory bytes",
                total,
                value.bytes,
                graph.limits.max_memory_bytes,
            )
        })?;
    let public_result_bytes = checked_memory_sum([
        PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?,
        terminal_value_bytes,
        bound.published_input_bytes,
        requested_vec_bound::<QuantizedSurfaceProjectionTile>(bound.published_input_tiles)?,
        requested_vec_bound::<QuantizedSurfaceFieldQueryTile>(bound.published_input_tiles)?,
        requested_vec_bound::<WorldCellKey>(ancestor_references)?,
        requested_vec_bound::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bound::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
        rejection_history_bytes,
        bound.diagnostic_metadata_bytes,
    ])?;
    check_limit(
        "memory bytes",
        public_result_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let materialized_bytes = materialized_outputs
        .values()
        .try_fold(0_u64, |total, value| {
            bound_add(
                "memory bytes",
                total,
                value.bytes,
                graph.limits.max_memory_bytes,
            )
        })?;
    let candidate_decision_bytes = requested_btree_bound::<
        CandidateIdentity,
        ProvenanceDecisionHandle,
    >(bound.candidate_events)?;
    let materialized_key_bytes = materialized_outputs.keys().try_fold(0_u64, |total, pin| {
        total
            .checked_add(qualified_graph_pin_memory(pin)?)
            .ok_or(Error::NumericOverflow)
    })?;
    let materialized_container_bytes = checked_memory_sum([
        requested_btree_bound::<QualifiedGraphPin, GraphValue>(materialized_outputs.len() as u64)?,
        materialized_key_bytes,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        checked_memory_sum([materialized_bytes, materialized_container_bytes])?,
        graph.limits.max_memory_bytes,
    )?;
    let cloned_provenance_bytes = checked_memory_sum([
        requested_vec_bound::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bound::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
    ])?;
    let global_tile_bytes = [
        public_result_bytes,
        candidate_decision_bytes,
        materialized_container_bytes,
        cloned_provenance_bytes,
    ]
    .into_iter()
    .try_fold(materialized_bytes, |total, bytes| {
        bound_add("memory bytes", total, bytes, graph.limits.max_memory_bytes)
    })?;
    Ok(SymbolicPlannedEvaluation {
        bound,
        materialized_outputs,
        public_result_bytes,
        global_tile_bytes,
        ancestor_references,
    })
}

fn preflight_evaluation_inputs(
    graph: &CompiledBiomeGraph,
    inputs: &[GraphEvaluationInputs],
    worker_count: usize,
    plan: &GraphExecutionPlan,
    global_store: &SymbolicGlobalStore,
    guard: PreflightGuard<'_>,
) -> Result<SymbolicInputPreflight> {
    guard.check()?;
    check_limit(
        "output cells",
        inputs.len() as u64,
        graph.limits.max_output_cells,
    )?;
    let mut cells = BTreeSet::new();
    let mut input_tiles = 0_u64;
    let mut retained_input_bytes = 0_u64;
    let mut generated_input_bytes = 0_u64;
    let mut candidate_count = 0_u64;
    let mut accepted_count = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    let mut retained_result_bytes = 0_u64;
    let mut worker_memory = BinaryHeap::with_capacity(worker_count);
    for input in inputs {
        guard.check()?;
        if !cells.insert(input.output_cell) {
            return Err(Error::GraphDocument {
                path: "evaluation.outputCells".to_owned(),
                reason: format!("output cell {} is duplicated", input.output_cell),
            });
        }
        input_tiles = bound_add(
            "input tiles",
            input_tiles,
            evaluation_input_tile_count(input)?,
            graph.limits.max_input_tiles,
        )?;
        retained_input_bytes = bound_add(
            "memory bytes",
            retained_input_bytes,
            estimated_input_bytes(input, guard)?,
            graph.limits.max_memory_bytes,
        )?;
        validate_static_inputs(graph, input, None, guard)?;
        guard.check()?;
        let symbolic = symbolic_evaluation_bound(
            graph,
            input,
            plan,
            SymbolicEvaluationScope::Cell { global_store },
            guard,
        )?;
        input_tiles = bound_add(
            "input tiles",
            input_tiles,
            symbolic.bound.generated_input_tiles,
            graph.limits.max_input_tiles,
        )?;
        generated_input_bytes = bound_add(
            "memory bytes",
            generated_input_bytes,
            symbolic.bound.generated_input_bytes,
            graph.limits.max_memory_bytes,
        )?;
        retained_result_bytes = bound_add(
            "memory bytes",
            retained_result_bytes,
            symbolic.public_result_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let ancestor_references = symbolic.ancestor_references;
        let bound = symbolic.bound;
        let estimated_candidates = bound.candidate_peak;
        candidate_count = bound_add(
            "candidate count",
            candidate_count,
            estimated_candidates,
            graph.limits.max_candidates,
        )?;
        accepted_count = bound_add(
            "accepted count",
            accepted_count,
            bound.accepted,
            graph.limits.max_macro_points,
        )?;
        micro_samples = bound_add(
            "micro samples",
            micro_samples,
            bound.micro_samples,
            graph.limits.max_micro_samples,
        )?;
        transfer_bytes = bound_add(
            "transfer bytes",
            transfer_bytes,
            bound.transfer_bytes,
            graph.limits.max_transfer_bytes,
        )?;
        let point_column_peak = PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?;
        let worker_bytes = checked_memory_sum([
            bound.memory_bytes,
            input_validation_scratch_bytes(input)?,
            point_column_peak,
            runtime_traversal_allocation_bound(
                graph,
                graph.demand_plan().public_slice(),
                plan,
                ancestor_references,
            )?,
        ])?;
        check_limit("memory bytes", worker_bytes, graph.limits.max_memory_bytes)?;
        if worker_memory.len() < worker_count {
            worker_memory.push(Reverse(worker_bytes));
        } else if worker_memory
            .peek()
            .is_some_and(|minimum| worker_bytes > minimum.0)
        {
            worker_memory.pop();
            worker_memory.push(Reverse(worker_bytes));
        }
    }
    let active_worker_memory =
        worker_memory
            .into_iter()
            .try_fold(0_u64, |total, Reverse(value)| {
                bound_add("memory bytes", total, value, graph.limits.max_memory_bytes)
            })?;
    Ok(SymbolicInputPreflight {
        input_tiles,
        retained_input_bytes,
        generated_input_bytes,
        candidate_count,
        accepted_count,
        micro_samples,
        transfer_bytes,
        active_worker_memory,
        retained_result_bytes,
    })
}

fn estimated_input_bytes(input: &GraphEvaluationInputs, guard: PreflightGuard<'_>) -> Result<u64> {
    let mut bytes = checked_memory_sum([
        requested_vec_bytes::<EvaluationFieldTile>(input.fields.capacity())?,
        requested_vec_bytes::<QuantizedSurfaceProjectionTile>(
            input.surface_projection_tiles.capacity(),
        )?,
        requested_vec_bytes::<QuantizedSurfaceFieldQueryTile>(
            input.surface_field_query_tiles.capacity(),
        )?,
        requested_vec_bytes::<EvaluationRegion>(input.regions.capacity())?,
        requested_vec_bytes::<EvaluationSpline>(input.splines.capacity())?,
        requested_vec_bytes::<EvaluationAnchor>(input.anchors.capacity())?,
        requested_vec_bytes::<PlantPrototype>(input.plant_prototypes.capacity())?,
        requested_vec_bytes::<Arc<dyn SurfaceField>>(input.surface_providers.capacity())?,
    ])?;
    for tile in &input.fields {
        guard.check()?;
        let values = match &tile.values {
            QuantizedFieldTileValues::Scalar(values) => {
                requested_vec_bytes::<i32>(values.capacity())?
            }
            QuantizedFieldTileValues::Gradient(values) => {
                requested_vec_bytes::<[i32; 3]>(values.capacity())?
            }
            QuantizedFieldTileValues::Hessian(values) => {
                requested_vec_bytes::<[i32; 6]>(values.capacity())?
            }
        };
        bytes = bytes.checked_add(values).ok_or(Error::NumericOverflow)?;
    }
    for tile in &input.surface_projection_tiles {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<QuantizedSurfaceProjectionEntry>(
                tile.samples.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
        for entry in &tile.samples {
            guard.check()?;
            if let Some(sample) = &entry.sample {
                bytes = bytes
                    .checked_add(requested_vec_bytes::<WeightedSurfaceTag>(
                        sample.tags.capacity(),
                    )?)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
    }
    for tile in &input.surface_field_query_tiles {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<QuantizedSurfaceFieldQueryEntry>(
                tile.samples.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
    }
    for spline in &input.splines {
        guard.check()?;
        bytes = bytes
            .checked_add(requested_vec_bytes::<WorldPosition>(
                spline.points.capacity(),
            )?)
            .ok_or(Error::NumericOverflow)?;
    }
    guard.check()?;
    Ok(bytes)
}

fn input_validation_scratch_bytes(input: &GraphEvaluationInputs) -> Result<u64> {
    let projection_queries =
        input
            .surface_projection_tiles
            .iter()
            .try_fold(0_usize, |total, tile| {
                total
                    .checked_add(tile.samples.len())
                    .ok_or(Error::NumericOverflow)
            })?;
    let field_queries =
        input
            .surface_field_query_tiles
            .iter()
            .try_fold(0_usize, |total, tile| {
                total
                    .checked_add(tile.samples.len())
                    .ok_or(Error::NumericOverflow)
            })?;
    checked_memory_sum([
        requested_btree_bytes::<(u128, u32, WorldPosition), ()>(projection_queries)?,
        requested_btree_bytes::<
            (
                u128,
                u32,
                FieldChannel,
                FieldDerivative,
                CandidateIdentity,
                WorldPosition,
            ),
            (),
        >(field_queries)?,
        requested_vec_bytes::<SurfaceProviderDescriptor>(input.surface_providers.len())?,
    ])
}

fn preflight_scratch_bytes(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
) -> Result<u64> {
    let maximum_expected_global = inputs.global_stages.len();
    let guarded_expected_global = maximum_expected_global
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    let stage_count = graph.spatial_plan().global_stages().len();
    let validation_scratch = inputs
        .cells
        .iter()
        .chain(inputs.global_stages.iter().map(|stage| &stage.inputs))
        .try_fold(0_u64, |maximum, input| {
            Ok::<_, Error>(maximum.max(input_validation_scratch_bytes(input)?))
        })?;
    checked_memory_sum([
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], u8), ()>(stage_count)?,
        requested_vec_bytes::<WorldCellKey>(guarded_expected_global)?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), ()>(inputs.global_stages.len())?,
        requested_vec_bytes::<&GlobalStageEvaluationInputs>(inputs.global_stages.len())?,
        requested_btree_bytes::<WorldCellKey, ()>(inputs.cells.len())?,
        requested_vec_bytes::<u64>(worker_count)?,
        symbolic_traversal_allocation_bound(graph, graph.demand_plan().execution_slice())?,
        validation_scratch,
    ])
}

fn job_input_container_bytes(inputs: &GraphEvaluationJobInputs) -> Result<u64> {
    checked_memory_sum([
        requested_vec_bytes::<GraphEvaluationInputs>(inputs.cells.capacity())?,
        requested_vec_bytes::<GlobalStageEvaluationInputs>(inputs.global_stages.capacity())?,
    ])
}

fn runtime_job_allocation_bytes(
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
    plan_allocation_bytes: u64,
) -> Result<u64> {
    let worker_count_u64 = u64::try_from(worker_count).map_err(|_| Error::NumericOverflow)?;
    let shard_allocations = worker_count_u64
        .checked_mul(ALLOCATION_OVERHEAD_BYTES)
        .ok_or(Error::NumericOverflow)?;
    let worker_stacks = worker_count_u64
        .checked_mul(EVALUATOR_WORKER_STACK_BYTES as u64)
        .ok_or(Error::NumericOverflow)?;
    checked_memory_sum([
        plan_allocation_bytes,
        worker_stacks,
        requested_vec_bytes::<Vec<(usize, GraphEvaluationInputs)>>(worker_count)?,
        requested_slice_bytes::<(usize, GraphEvaluationInputs)>(inputs.cells.len())?,
        shard_allocations,
        requested_vec_bytes::<
            std::thread::ScopedJoinHandle<
                'static,
                Result<Vec<(usize, Result<GraphEvaluationResult>)>>,
            >,
        >(worker_count)?,
        requested_slice_bytes::<(usize, Result<GraphEvaluationResult>)>(inputs.cells.len())?,
        shard_allocations,
        requested_vec_bytes::<(usize, Result<GraphEvaluationResult>)>(inputs.cells.len())?,
        requested_vec_bytes::<GraphEvaluationResult>(inputs.cells.len())?,
        requested_vec_bytes::<GlobalStageEvaluationResult>(inputs.global_stages.len())?,
        requested_btree_bytes::<GlobalStageCacheKey, GlobalStageTile>(inputs.global_stages.len())?,
        requested_btree_bytes::<([u8; 32], WorldCellKey), GlobalStageCacheKey>(
            inputs.global_stages.len(),
        )?,
    ])
}

fn evaluation_input_tile_count(input: &GraphEvaluationInputs) -> Result<u64> {
    [
        input.fields.len(),
        input.surface_projection_tiles.len(),
        input.surface_field_query_tiles.len(),
        input.regions.len(),
        input.splines.len(),
        input.surface_providers.len(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, count| {
        total
            .checked_add(u64::try_from(count).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })
}

fn check_limit(resource: &'static str, requested: u64, limit: u64) -> Result<()> {
    if requested > limit {
        return Err(Error::GraphLimit {
            resource,
            requested,
            limit,
        });
    }
    Ok(())
}

fn global_stage_order(graph: &CompiledBiomeGraph, stage_id: [u8; 32]) -> usize {
    graph
        .spatial_plan()
        .global_stages()
        .iter()
        .position(|stage| stage.id == stage_id)
        .unwrap_or(usize::MAX)
}

fn preflight_evaluation_job(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
    plan: &GraphExecutionPlan,
    plan_allocation_bytes: u64,
    guard: PreflightGuard<'_>,
) -> Result<GraphEvaluationPreflight> {
    guard.check()?;
    let preflight_scratch = preflight_scratch_bytes(graph, inputs, worker_count)?;
    check_limit(
        "memory bytes",
        checked_memory_sum([plan_allocation_bytes, preflight_scratch])?,
        graph.limits.max_memory_bytes,
    )?;
    let mut job_identity = None;
    for input in inputs
        .cells
        .iter()
        .chain(inputs.global_stages.iter().map(|stage| &stage.inputs))
    {
        let identity = (input.map, input.biome_instance, input.ecology_tick);
        if job_identity
            .replace(identity)
            .is_some_and(|expected| expected != identity)
        {
            return Err(Error::GraphDocument {
                path: "evaluation.job".to_owned(),
                reason: "one graph job must use one map, biome instance, and ecology snapshot"
                    .to_owned(),
            });
        }
    }
    check_limit(
        "global stage tiles",
        inputs.global_stages.len() as u64,
        graph.limits.max_global_stage_tiles,
    )?;

    let expected = expected_global_stage_tiles_guarded(
        graph,
        &inputs.cells,
        Some(guard),
        inputs.global_stages.len(),
    )?;
    let mut actual = BTreeSet::new();
    let mut global_input_tiles = 0_u64;
    let mut global_retained_input_bytes = 0_u64;
    let mut global_generated_input_bytes = 0_u64;
    let mut global_resident_memory = 0_u64;
    let mut global_peak_memory = 0_u64;
    let mut global_candidates = 0_u64;
    let mut global_accepted = 0_u64;
    let mut global_micro_samples = 0_u64;
    let mut global_transfer_bytes = 0_u64;
    let mut symbolic_global_store = SymbolicGlobalStore::default();
    let mut ordered_global_inputs = Vec::new();
    crate::memory::reserve_exact(
        &mut ordered_global_inputs,
        inputs.global_stages.len(),
        "ordered global-stage inputs",
    )?;
    ordered_global_inputs.extend(inputs.global_stages.iter());
    ordered_global_inputs.sort_unstable_by_key(|input| {
        (
            global_stage_order(graph, input.stage),
            input.owner,
            input.input_snapshot,
        )
    });
    for input in ordered_global_inputs {
        guard.check()?;
        let stage = graph
            .spatial_plan()
            .global_stage(input.stage)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!("unknown global stage {}", hex_hash(input.stage)),
            })?;
        if !actual.insert((input.stage, input.owner)) {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "a global-stage owner tile is duplicated".to_owned(),
            });
        }
        let expected_solve_bounds = input.owner.bounds();
        let expected_read_bounds = expand_bounds_checked(
            expected_solve_bounds,
            fixed_meters_to_ticks(stage.upstream_halo)?.unsigned_abs() as i128,
        )?;
        if input.owner.level() != stage.owner_level
            || input.solve_bounds != expected_solve_bounds
            || input.inputs.output_cell != input.owner
            || input.inputs.output_bounds != expected_solve_bounds
            || input.inputs.read_bounds != expected_read_bounds
            || input.input_snapshot == [0; 32]
        {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!(
                    "stage {} owner, solve/read bounds, or immutable snapshot is invalid",
                    hex_hash(stage.id)
                ),
            });
        }
        global_input_tiles = bound_add(
            "input tiles",
            global_input_tiles,
            evaluation_input_tile_count(&input.inputs)?,
            graph.limits.max_input_tiles,
        )?;
        global_retained_input_bytes = bound_add(
            "memory bytes",
            global_retained_input_bytes,
            estimated_input_bytes(&input.inputs, guard)?,
            graph.limits.max_memory_bytes,
        )?;
        validate_static_inputs(graph, &input.inputs, Some(stage), guard)?;
        guard.check()?;
        let symbolic = symbolic_evaluation_bound(
            graph,
            &input.inputs,
            plan,
            SymbolicEvaluationScope::Global {
                stage,
                global_store: &symbolic_global_store,
            },
            guard,
        )?;
        global_input_tiles = bound_add(
            "input tiles",
            global_input_tiles,
            symbolic.bound.generated_input_tiles,
            graph.limits.max_input_tiles,
        )?;
        global_generated_input_bytes = bound_add(
            "memory bytes",
            global_generated_input_bytes,
            symbolic.bound.generated_input_bytes,
            graph.limits.max_memory_bytes,
        )?;
        if symbolic.materialized_outputs.len() != stage.output_pins.len() {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: format!(
                    "symbolic stage {} did not materialize every boundary output",
                    hex_hash(stage.id)
                ),
            });
        }
        let stage_retained_bytes = symbolic.global_tile_bytes;
        check_limit(
            "memory bytes",
            stage_retained_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let ancestor_references = symbolic.ancestor_references;
        let bound = symbolic.bound;
        let global_worker_bytes = checked_memory_sum([
            bound.memory_bytes,
            input_validation_scratch_bytes(&input.inputs)?,
            PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?,
            runtime_traversal_allocation_bound(
                graph,
                graph
                    .demand_plan()
                    .stage_slice(stage.id)
                    .ok_or_else(|| Error::GraphDocument {
                        path: "graph.demandPlan".to_owned(),
                        reason: "global stage has no demand slice".to_owned(),
                    })?,
                plan,
                ancestor_references,
            )?,
        ])?;
        let live_during_stage = bound_add(
            "memory bytes",
            global_resident_memory,
            global_worker_bytes,
            graph.limits.max_memory_bytes,
        )?;
        let resident_with_stage = bound_add(
            "memory bytes",
            global_resident_memory,
            stage_retained_bytes,
            graph.limits.max_memory_bytes,
        );
        let resident_with_stage = resident_with_stage?;
        global_peak_memory = global_peak_memory
            .max(live_during_stage)
            .max(resident_with_stage);
        global_candidates = bound_add(
            "candidate count",
            global_candidates,
            bound.candidate_peak,
            graph.limits.max_candidates,
        )?;
        global_accepted = bound_add(
            "accepted count",
            global_accepted,
            bound.accepted,
            graph.limits.max_macro_points,
        )?;
        global_micro_samples = bound_add(
            "micro samples",
            global_micro_samples,
            bound.micro_samples,
            graph.limits.max_micro_samples,
        )?;
        global_transfer_bytes = bound_add(
            "transfer bytes",
            global_transfer_bytes,
            bound.transfer_bytes,
            graph.limits.max_transfer_bytes,
        )?;
        global_resident_memory = resident_with_stage;
        let provenance_records = bound
            .accepted
            .checked_add(bound.rejected)
            .and_then(|records| records.checked_add(bound.imported_provenance_records))
            .ok_or(Error::NumericOverflow)?;
        let symbolic_tile = SymbolicGlobalTile {
            outputs: symbolic.materialized_outputs,
            provenance_decisions: bound.candidate_events,
            provenance_records,
            provenance_bytes: bound.provenance_bytes,
        };
        if symbolic_global_store
            .tiles
            .insert((stage.id, input.owner), symbolic_tile)
            .is_some()
        {
            return Err(Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "symbolic global-stage owner tile is duplicated".to_owned(),
            });
        }
    }
    if actual != expected {
        let missing = expected.difference(&actual).count();
        let unexpected = actual.difference(&expected).count();
        return Err(Error::GraphDocument {
            path: "evaluation.globalStages".to_owned(),
            reason: format!(
                "global-stage tile set is not closed (missing {missing}, unexpected {unexpected})"
            ),
        });
    }

    let cells = preflight_evaluation_inputs(
        graph,
        &inputs.cells,
        worker_count,
        plan,
        &symbolic_global_store,
        guard,
    )?;
    let input_tiles = bound_add(
        "input tiles",
        cells.input_tiles,
        global_input_tiles,
        graph.limits.max_input_tiles,
    )?;
    let candidate_count = bound_add(
        "candidate count",
        cells.candidate_count,
        global_candidates,
        graph.limits.max_candidates,
    )?;
    let accepted_count = bound_add(
        "accepted count",
        cells.accepted_count,
        global_accepted,
        graph.limits.max_macro_points,
    )?;
    let micro_samples = bound_add(
        "micro samples",
        cells.micro_samples,
        global_micro_samples,
        graph.limits.max_micro_samples,
    )?;
    let transfer_bytes = bound_add(
        "transfer bytes",
        cells.transfer_bytes,
        global_transfer_bytes,
        graph.limits.max_transfer_bytes,
    )?;

    let retained_results_memory = bound_add(
        "memory bytes",
        global_resident_memory,
        cells.retained_result_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let runtime_peak_memory = global_peak_memory.max(bound_add(
        "memory bytes",
        retained_results_memory,
        cells.active_worker_memory,
        graph.limits.max_memory_bytes,
    )?);
    let nested_retained_input_bytes = bound_add(
        "memory bytes",
        cells.retained_input_bytes,
        global_retained_input_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let retained_input_bytes = bound_add(
        "memory bytes",
        nested_retained_input_bytes,
        job_input_container_bytes(inputs)?,
        graph.limits.max_memory_bytes,
    )?;
    let generated_input_bytes = bound_add(
        "memory bytes",
        cells.generated_input_bytes,
        global_generated_input_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let runtime_allocations =
        runtime_job_allocation_bytes(inputs, worker_count, plan_allocation_bytes)?;
    let execution_peak_bytes = checked_memory_sum([
        retained_input_bytes,
        runtime_peak_memory,
        runtime_allocations,
    ])?;
    let preflight_peak_bytes = checked_memory_sum([
        retained_input_bytes,
        plan_allocation_bytes,
        preflight_scratch,
        symbolic_global_store.requested_memory_bytes()?,
    ])?;
    let memory_bytes = execution_peak_bytes.max(preflight_peak_bytes);
    check_limit("memory bytes", memory_bytes, graph.limits.max_memory_bytes)?;
    let preflight = GraphEvaluationPreflight {
        output_cells: inputs.cells.len() as u64,
        global_stage_tiles: inputs.global_stages.len() as u64,
        input_tiles,
        retained_input_bytes,
        generated_input_bytes,
        candidate_count,
        accepted_count,
        micro_samples,
        preflight_peak_bytes,
        execution_peak_bytes,
        memory_bytes,
        transfer_bytes,
        worker_count: u16::try_from(worker_count).map_err(|_| Error::NumericOverflow)?,
        time_limit_ms: graph.limits.max_time_ms,
        limits: graph.limits,
    };
    guard.check()?;
    Ok(preflight)
}

#[cfg(test)]
fn expected_global_stage_tiles(
    graph: &CompiledBiomeGraph,
    cells: &[GraphEvaluationInputs],
) -> Result<BTreeSet<([u8; 32], WorldCellKey)>> {
    expected_global_stage_tiles_guarded(
        graph,
        cells,
        None,
        usize::try_from(graph.limits.max_global_stage_tiles).map_err(|_| Error::NumericOverflow)?,
    )
}

fn expected_global_stage_tiles_guarded(
    graph: &CompiledBiomeGraph,
    cells: &[GraphEvaluationInputs],
    guard: Option<PreflightGuard<'_>>,
    maximum_expected: usize,
) -> Result<BTreeSet<([u8; 32], WorldCellKey)>> {
    let guarded_expected = maximum_expected
        .checked_add(1)
        .ok_or(Error::NumericOverflow)?;
    let mut expected = BTreeSet::new();
    for stage in graph.spatial_plan().global_stages() {
        for cell in cells {
            if let Some(guard) = guard {
                guard.check()?;
            }
            for owner in world_cells_covering_bounds(
                cell.read_bounds,
                stage.owner_level,
                u64::try_from(guarded_expected).map_err(|_| Error::NumericOverflow)?,
            )? {
                expected.insert((stage.id, owner));
                ensure_expected_global_tile_capacity(&expected, maximum_expected)?;
            }
        }
    }

    loop {
        let mut additions = BTreeSet::new();
        for (stage_id, owner) in expected.iter().copied() {
            if let Some(guard) = guard {
                guard.check()?;
            }
            let stage = graph.spatial_plan().global_stage(stage_id).ok_or_else(|| {
                Error::GraphDocument {
                    path: "graph.spatialPlan".to_owned(),
                    reason: "selected global stage disappeared from the compiled plan".to_owned(),
                }
            })?;
            let read_bounds = expand_bounds_checked(
                owner.bounds(),
                fixed_meters_to_ticks(stage.upstream_halo)?.unsigned_abs() as i128,
            )?;
            let prerequisite_stages = stage
                .input_pins
                .iter()
                .filter_map(|pin| graph.spatial_plan().global_stage_for_node(&pin.node))
                .map(|stage| (stage.id, stage.owner_level))
                .collect::<BTreeSet<_>>();
            for (prerequisite, owner_level) in prerequisite_stages {
                for prerequisite_owner in world_cells_covering_bounds(
                    read_bounds,
                    owner_level,
                    u64::try_from(guarded_expected).map_err(|_| Error::NumericOverflow)?,
                )? {
                    if !expected.contains(&(prerequisite, prerequisite_owner)) {
                        additions.insert((prerequisite, prerequisite_owner));
                        if expected
                            .len()
                            .checked_add(additions.len())
                            .ok_or(Error::NumericOverflow)?
                            > maximum_expected
                        {
                            return Err(Error::GraphDocument {
                                path: "evaluation.globalStages".to_owned(),
                                reason: "global-stage tile set is not closed (missing tiles)"
                                    .to_owned(),
                            });
                        }
                    }
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        expected.extend(additions);
        ensure_expected_global_tile_capacity(&expected, maximum_expected)?;
    }
    Ok(expected)
}

fn ensure_expected_global_tile_capacity(
    expected: &BTreeSet<([u8; 32], WorldCellKey)>,
    maximum_expected: usize,
) -> Result<()> {
    if expected.len() > maximum_expected {
        return Err(Error::GraphDocument {
            path: "evaluation.globalStages".to_owned(),
            reason: "global-stage tile set is not closed (missing tiles)".to_owned(),
        });
    }
    Ok(())
}

fn retained_global_tile_bytes(evaluated: &PlannedEvaluation) -> Result<u64> {
    checked_memory_sum([
        requested_btree_with(
            &evaluated.materialized_outputs,
            qualified_graph_pin_memory,
            GraphValue::requested_memory_bytes,
        )?,
        retained_result_bytes(&evaluated.result)?,
        evaluated.result.provenance.requested_memory_bytes()?,
        requested_btree_bytes::<CandidateIdentity, ProvenanceDecisionHandle>(
            evaluated.candidate_decisions.len(),
        )?,
    ])
}

fn retained_result_bytes(result: &GraphEvaluationResult) -> Result<u64> {
    graph_result_memory(result)
}
fn evaluate_job(
    graph: &CompiledBiomeGraph,
    mut inputs: GraphEvaluationJobInputs,
    worker_count: usize,
    cancellation: &GraphCancellationToken,
    compute: Option<&dyn GraphComputeExecutor>,
) -> Result<GraphEvaluationJobResult> {
    if worker_count == 0 {
        return Err(Error::GraphLimit {
            resource: "worker count",
            requested: 0,
            limit: 1,
        });
    }
    let deadline = evaluation_deadline(graph)?;
    let guard = PreflightGuard {
        cancellation,
        deadline,
        time_limit_ms: graph.limits.max_time_ms,
    };
    guard.check()?;
    check_job_collection_limits(graph, &inputs)?;
    guard.check()?;
    for input in &mut inputs.cells {
        canonicalize_evaluation_input(input);
    }
    for stage in &mut inputs.global_stages {
        canonicalize_evaluation_input(&mut stage.inputs);
    }
    inputs.cells.sort_unstable_by_key(|input| input.output_cell);
    guard.check()?;
    inputs.global_stages.sort_unstable_by_key(|input| {
        (
            global_stage_order(graph, input.stage),
            input.owner,
            input.input_snapshot,
        )
    });
    guard.check()?;
    let workers = worker_count.min(inputs.cells.len().max(1));
    let gpu = compute.map(|compute| GraphGpuScheduling {
        profile: compute.profile(),
        qualifications: compute.qualifications(),
    });
    let plans = build_evaluation_plans(graph, workers > 1, gpu, guard)?;
    let execution_plan = &plans.execution;
    let preparation_plan = plans.preparation.as_ref();
    preflight_evaluation_job(
        graph,
        &inputs,
        workers,
        execution_plan,
        plans.allocation_bytes,
        guard,
    )?;
    #[cfg(test)]
    cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::AfterPreflight,
        graph.limits.max_time_ms,
    )?;
    let mut global_store = GlobalStageStore::default();
    let mut global_results = Vec::new();
    crate::memory::reserve_exact(
        &mut global_results,
        inputs.global_stages.len(),
        "global stage results",
    )?;
    for input in inputs.global_stages {
        guard.check()?;
        let GlobalStageEvaluationInputs {
            stage: stage_id,
            owner,
            input_snapshot,
            inputs: stage_inputs,
            ..
        } = input;
        let stage =
            graph
                .spatial_plan()
                .global_stage(stage_id)
                .ok_or_else(|| Error::GraphDocument {
                    path: "evaluation.globalStages".to_owned(),
                    reason: "global-stage identity is absent from the compiled graph".to_owned(),
                })?;
        let map = stage_inputs.map.value();
        let biome_instance = stage_inputs.biome_instance;
        let context = EvaluationContext {
            graph,
            cancellation,
            compute,
            execution_plan,
            scope: EvaluationScope::Global {
                stage,
                global_store: &global_store,
            },
            deadline,
        };
        let evaluated = evaluate_cell_atomically(stage_inputs, context, preparation_plan)?;
        for output in &stage.output_pins {
            guard.check()?;
            if !evaluated.materialized_outputs.contains_key(output) {
                return Err(Error::GraphDocument {
                    path: "evaluation.globalStages".to_owned(),
                    reason: format!(
                        "stage {} did not materialize boundary pin '{}'",
                        hex_hash(stage.id),
                        output.pin
                    ),
                });
            }
        }
        guard.check()?;
        let resident_bytes = retained_global_tile_bytes(&evaluated)?;
        guard.check()?;
        let public_result = evaluated.result;
        let tile = GlobalStageTile {
            key: GlobalStageCacheKey {
                stage: stage_id,
                map,
                biome_instance,
                owner,
                input_snapshot,
            },
            outputs: evaluated.materialized_outputs,
            provenance: public_result.provenance.clone(),
            candidate_decisions: evaluated.candidate_decisions,
            resident_bytes,
        };
        global_store.insert(tile)?;
        guard.check()?;
        check_limit(
            "memory bytes",
            global_store.resident_bytes()?,
            graph.limits.max_memory_bytes,
        )?;
        global_results.push(GlobalStageEvaluationResult {
            stage: stage_id,
            owner,
            result: public_result,
            resident_bytes,
        });
    }
    guard.check()?;

    let cell_count = inputs.cells.len();
    let mut shards = Vec::new();
    crate::memory::reserve_exact(&mut shards, workers, "cell worker shards")?;
    for worker in 0..workers {
        let capacity = cell_count
            .checked_add(workers - 1 - worker)
            .ok_or(Error::NumericOverflow)?
            / workers;
        let mut shard = Vec::new();
        crate::memory::reserve_exact(&mut shard, capacity, "cell worker shard")?;
        shards.push(shard);
    }
    for (index, input) in inputs.cells.into_iter().enumerate() {
        guard.check()?;
        shards[index % workers].push((index, input));
    }
    guard.check()?;
    let global_store = &global_store;
    let mut results = std::thread::scope(|scope| -> Result<Vec<_>> {
        let mut handles = Vec::new();
        crate::memory::reserve_exact(&mut handles, workers, "cell worker handles")?;
        for (worker, shard) in shards.into_iter().enumerate() {
            let handle = std::thread::Builder::new()
                .name(format!("vegetation-graph-{worker}"))
                .stack_size(EVALUATOR_WORKER_STACK_BYTES)
                .spawn_scoped(scope, move || -> Result<Vec<_>> {
                    let mut worker_results = Vec::new();
                    crate::memory::reserve_exact(
                        &mut worker_results,
                        shard.len(),
                        "cell worker results",
                    )?;
                    for (index, input) in shard {
                        let context = EvaluationContext {
                            graph,
                            cancellation,
                            compute,
                            execution_plan,
                            scope: EvaluationScope::Cell { global_store },
                            deadline,
                        };
                        worker_results.push((
                            index,
                            evaluate_cell_atomically(input, context, preparation_plan)
                                .map(|planned| planned.result),
                        ));
                    }
                    Ok(worker_results)
                })
                .map_err(|source| Error::GraphWorkerSpawn { source })?;
            handles.push(handle);
        }
        let mut joined = Vec::new();
        crate::memory::reserve_exact(&mut joined, cell_count, "joined cell results")?;
        for handle in handles {
            let mut worker_results = handle.join().map_err(|_| Error::GraphWorkerPanicked)??;
            joined.append(&mut worker_results);
        }
        Ok(joined)
    })?;
    guard.check()?;
    results.sort_unstable_by_key(|(index, _)| *index);
    guard.check()?;
    let mut cell_results = Vec::new();
    crate::memory::reserve_exact(&mut cell_results, results.len(), "cell results")?;
    for (_, result) in results {
        guard.check()?;
        cell_results.push(result?);
    }
    guard.check()?;
    validate_result_totals(
        graph,
        global_results
            .iter()
            .map(|global| &global.result)
            .chain(cell_results.iter()),
        guard,
    )?;
    let evaluated = GraphEvaluationJobResult {
        cells: cell_results,
        global_stages: global_results,
    };
    guard.check()?;
    #[cfg(test)]
    cancellation.check_test_checkpoint(
        TestEvaluationCheckpoint::BeforePublication,
        graph.limits.max_time_ms,
    )?;
    Ok(evaluated)
}

fn validate_result_totals<'a>(
    graph: &CompiledBiomeGraph,
    results: impl IntoIterator<Item = &'a GraphEvaluationResult>,
    guard: PreflightGuard<'_>,
) -> Result<()> {
    let mut candidates = 0_u64;
    let mut accepted = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    for result in results {
        guard.check()?;
        candidates = candidates
            .checked_add(result.diagnostics.candidate_count)
            .ok_or(Error::NumericOverflow)?;
        accepted = accepted
            .checked_add(result.diagnostics.accepted_count)
            .ok_or(Error::NumericOverflow)?;
        for tile in &result.micro_fields {
            guard.check()?;
            micro_samples = micro_samples
                .checked_add(tile.density.len() as u64)
                .ok_or(Error::NumericOverflow)?;
        }
        for node in &result.diagnostics.nodes {
            guard.check()?;
            transfer_bytes = transfer_bytes
                .checked_add(node.transfer_bytes)
                .ok_or(Error::NumericOverflow)?;
        }
        for group in &result.diagnostics.gpu_groups {
            guard.check()?;
            transfer_bytes = transfer_bytes
                .checked_add(group.transfer_bytes)
                .ok_or(Error::NumericOverflow)?;
        }
    }
    check_limit("candidate count", candidates, graph.limits.max_candidates)?;
    check_limit("accepted count", accepted, graph.limits.max_macro_points)?;
    check_limit(
        "micro samples",
        micro_samples,
        graph.limits.max_micro_samples,
    )?;
    check_limit(
        "transfer bytes",
        transfer_bytes,
        graph.limits.max_transfer_bytes,
    )?;
    guard.check()
}

#[derive(Clone, Debug)]
enum ResidentInputBinding {
    CandidateMask,
    ExternalScalar((u128, String)),
    NoiseCorner { node: u128, corner: usize },
    NoiseBlend { node: u128, axis: usize },
    GradientPosition { node: u128, axis: usize },
    GradientOrigin { node: u128, axis: usize },
}

struct ResidentGroupEvaluation {
    outputs: BTreeMap<(u128, String), GraphValue>,
    base_candidate_count: u64,
    final_candidate_count: u64,
    field_importance_index: Option<usize>,
    invocation_count: u64,
}

fn resident_source_register(
    unit: &CompiledGraphUnit,
    group: &GraphExecutionGroup,
    external_registers: &BTreeMap<(u128, String), GraphGpuRegister>,
    result_registers: &BTreeMap<(u128, String), GraphGpuRegister>,
    node: u128,
    pin: &str,
) -> Result<GraphGpuRegister> {
    let edge = unit
        .edges
        .iter()
        .find(|edge| edge.to_node == node && edge.to_pin == pin)
        .ok_or_else(|| Error::GraphDocument {
            path: format!("graphGpuGroup.{node:032x}.{pin}"),
            reason: "resident input edge is missing".to_owned(),
        })?;
    if resident_group_contains(group, edge.from_node) {
        result_registers
            .get(&(edge.from_node, edge.from_pin.clone()))
            .copied()
    } else {
        external_registers
            .get(&(edge.from_node, edge.from_pin.clone()))
            .copied()
    }
    .ok_or_else(|| Error::GraphDocument {
        path: format!("graphGpuGroup.{node:032x}.{pin}"),
        reason: "resident source register is missing".to_owned(),
    })
}

fn resident_group_contains(group: &GraphExecutionGroup, node: u128) -> bool {
    group.nodes.iter().any(|member| member.address.node == node)
}

fn evaluate_resident_group(
    unit: &CompiledGraphUnit,
    group: &crate::GraphExecutionGroup,
    boundary_values: &BTreeMap<(u128, String), GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<ResidentGroupEvaluation> {
    let compute = state.compute.ok_or_else(|| Error::GraphDocument {
        path: "graphGpuGroup.executor".to_owned(),
        reason: "execution plan selected Slang without a compute executor".to_owned(),
    })?;
    let mut nodes = Vec::new();
    crate::memory::reserve_exact(&mut nodes, group.nodes.len(), "resident group nodes")?;
    nodes.extend(
        unit.nodes
            .iter()
            .filter(|node| resident_group_contains(group, node.definition.guid)),
    );
    if nodes.len() != group.nodes.len() {
        return Err(Error::GraphDocument {
            path: "graphGpuGroup.nodes".to_owned(),
            reason: "resident group does not belong to the evaluated module".to_owned(),
        });
    }

    let external_scalar_keys = boundary_values
        .iter()
        .filter_map(|(key, value)| matches!(value, GraphValue::Scalar(_)).then_some(key.clone()))
        .collect::<BTreeSet<_>>();
    let candidate_stream = boundary_values.values().find_map(|value| match value {
        GraphValue::Candidates(stream) => Some(stream),
        _ => None,
    });
    let field_lineage = boundary_values.values().find_map(|value| match value {
        GraphValue::Scalar(field) => Some(field.lineage),
        _ => None,
    });
    let lineage = candidate_stream
        .map(|stream| stream.lineage)
        .or(field_lineage)
        .ok_or_else(|| Error::GraphDocument {
            path: "graphGpuGroup.inputs".to_owned(),
            reason: "resident group has no candidate-indexed boundary input".to_owned(),
        })?;
    if boundary_values.values().any(|value| match value {
        GraphValue::Candidates(stream) => stream.lineage != lineage,
        GraphValue::Scalar(field) => field.lineage != lineage,
        _ => true,
    }) {
        return Err(Error::GraphDocument {
            path: "graphGpuGroup.inputs".to_owned(),
            reason: "resident boundary inputs do not share one candidate lineage".to_owned(),
        });
    }

    let noise_node_count = nodes
        .iter()
        .filter(|node| node.definition.operator == GraphOperator::Noise)
        .count();
    let gradient_node_count = nodes
        .iter()
        .filter(|node| node.definition.operator == GraphOperator::Gradient)
        .count();
    let input_capacity = 1_usize
        .checked_add(external_scalar_keys.len())
        .and_then(|count| count.checked_add(noise_node_count.checked_mul(11)?))
        .and_then(|count| count.checked_add(gradient_node_count.checked_mul(6)?))
        .ok_or(Error::NumericOverflow)?;
    let mut input_types = Vec::new();
    crate::memory::reserve_exact(&mut input_types, input_capacity, "resident input types")?;
    input_types.push(GraphGpuRegisterType::CandidateMask);
    let mut bindings = Vec::new();
    crate::memory::reserve_exact(&mut bindings, input_capacity, "resident input bindings")?;
    bindings.push(ResidentInputBinding::CandidateMask);
    let mut external_registers = BTreeMap::new();
    external_registers.extend(boundary_values.iter().filter_map(|(key, value)| {
        matches!(value, GraphValue::Candidates(_)).then_some((key.clone(), GraphGpuRegister(0)))
    }));
    for key in &external_scalar_keys {
        let register = GraphGpuRegister(input_types.len() as u32);
        external_registers.insert(key.clone(), register);
        input_types.push(GraphGpuRegisterType::FixedScalar);
        bindings.push(ResidentInputBinding::ExternalScalar(key.clone()));
    }
    let mut noise_inputs = BTreeMap::new();
    let mut gradient_inputs = BTreeMap::new();
    for node in &nodes {
        match node.definition.operator {
            GraphOperator::Noise => {
                let corners = std::array::from_fn(|corner| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::FixedScalar);
                    bindings.push(ResidentInputBinding::NoiseCorner {
                        node: node.definition.guid,
                        corner,
                    });
                    register
                });
                let blend = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::Unit);
                    bindings.push(ResidentInputBinding::NoiseBlend {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                noise_inputs.insert(node.definition.guid, (corners, blend));
            }
            GraphOperator::Gradient => {
                let position = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::WorldTick);
                    bindings.push(ResidentInputBinding::GradientPosition {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                let exact_origin = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::WorldTick);
                    bindings.push(ResidentInputBinding::GradientOrigin {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                gradient_inputs.insert(node.definition.guid, (position, exact_origin));
            }
            _ => {}
        }
    }

    let mut instructions = Vec::new();
    crate::memory::reserve_exact(
        &mut instructions,
        nodes.len(),
        "resident graph instructions",
    )?;
    let mut result_registers = BTreeMap::new();
    let mut field_importance_index = None;
    for (index, node) in nodes.iter().enumerate() {
        let destination = GraphGpuRegister((input_types.len() + index) as u32);
        let instruction = match node.definition.operator {
            GraphOperator::Noise => {
                let frequency =
                    fixed_parameter(node, "frequency", DecisionScalar::from_bits(65_536))?;
                if frequency.bits() <= 0 {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "noise frequency must be positive".to_owned(),
                    });
                }
                let amplitude =
                    fixed_parameter(node, "amplitude", DecisionScalar::from_bits(65_536))?;
                let (corners, blend) = noise_inputs[&node.definition.guid];
                GraphGpuInstruction::Noise {
                    destination,
                    corners,
                    blend,
                    amplitude: amplitude.bits(),
                }
            }
            GraphOperator::Gradient => {
                let (position, exact_origin) = gradient_inputs[&node.definition.guid];
                GraphGpuInstruction::Gradient {
                    destination,
                    position,
                    exact_origin,
                    direction: fixed_vec3_parameter(
                        node,
                        "direction",
                        [DecisionScalar::from_bits(0); 3],
                    )?
                    .map(DecisionScalar::bits),
                    scale: fixed_parameter(node, "scale", DecisionScalar::from_bits(0))?.bits(),
                    bias: fixed_parameter(node, "bias", DecisionScalar::from_bits(0))?.bits(),
                }
            }
            GraphOperator::Curve => {
                let curve = curve_parameter(node, "curve")?;
                DecisionCurve::validate_points(curve)?;
                GraphGpuInstruction::Curve {
                    destination,
                    input: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "field",
                    )?,
                    points: {
                        let mut points = Vec::new();
                        crate::memory::reserve_exact(
                            &mut points,
                            curve.len(),
                            "resident curve points",
                        )?;
                        points.extend(curve.iter().map(|(x, y)| (x.bits(), y.bits())));
                        points
                    },
                }
            }
            GraphOperator::Remap => {
                let input_min = fixed_parameter(node, "inputMin", DecisionScalar::from_bits(0))?;
                let input_max = fixed_parameter(node, "inputMax", DecisionScalar::from_bits(0))?;
                let output_min = fixed_parameter(node, "outputMin", DecisionScalar::from_bits(0))?;
                let output_max = fixed_parameter(node, "outputMax", DecisionScalar::from_bits(0))?;
                GraphGpuInstruction::Remap {
                    destination,
                    input: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "field",
                    )?,
                    input_min: input_min.bits(),
                    input_max: input_max.bits(),
                    output_min: output_min.bits(),
                    output_max: output_max.bits(),
                }
            }
            GraphOperator::Combine => GraphGpuInstruction::Combine {
                destination,
                left: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "left",
                )?,
                right: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "right",
                )?,
                operation: combine_operation_parameter(node, "operation")?,
            },
            GraphOperator::Clamp => GraphGpuInstruction::Clamp {
                destination,
                input: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "field",
                )?,
                minimum: fixed_parameter(node, "minimum", DecisionScalar::from_bits(0))?.bits(),
                maximum: fixed_parameter(node, "maximum", DecisionScalar::from_bits(0))?.bits(),
            },
            GraphOperator::FieldImportance => {
                field_importance_index = Some(index);
                GraphGpuInstruction::FieldImportance {
                    destination,
                    candidates: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "candidates",
                    )?,
                    weights: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "weights",
                    )?,
                    threshold: unit_parameter(node, "threshold", UnitInterval::ZERO)?.bits(),
                }
            }
            _ => {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "execution group contains an unsupported resident operator".to_owned(),
                });
            }
        };
        let output_pin = if node.definition.operator == GraphOperator::FieldImportance {
            "candidates"
        } else {
            "field"
        };
        result_registers.insert((node.definition.guid, output_pin.to_owned()), destination);
        instructions.push(instruction);
    }

    let field_boundary = group
        .outputs
        .iter()
        .find(|output| output.domain == GraphDomain::ScalarField);
    let output_register = field_boundary
        .map(|output| {
            result_registers
                .get(&(output.pin.node.node, output.pin.pin.clone()))
                .copied()
                .ok_or_else(|| Error::GraphDocument {
                    path: "graphGpuGroup.output".to_owned(),
                    reason: "resident field boundary has no result register".to_owned(),
                })
        })
        .transpose()?;
    let terminal_mask = field_importance_index.map_or(GraphGpuRegister(0), |index| {
        GraphGpuRegister((input_types.len() + index) as u32)
    });
    let program = GraphGpuProgram::new(input_types, instructions, output_register, terminal_mask)?;

    let identity_count = candidate_stream.map_or_else(
        || {
            boundary_values
                .values()
                .find_map(|value| match value {
                    GraphValue::Scalar(field) => Some(field.values.len()),
                    _ => None,
                })
                .unwrap_or(0)
        },
        |stream| stream.candidates.len(),
    );
    let mut identities = Vec::new();
    crate::memory::reserve_exact(&mut identities, identity_count, "resident identities")?;
    if let Some(stream) = candidate_stream {
        identities.extend(stream.candidates.iter().map(|candidate| candidate.identity));
    } else if let Some(field) = boundary_values.values().find_map(|value| match value {
        GraphValue::Scalar(field) => Some(field),
        _ => None,
    }) {
        identities.extend(field.values.keys().copied());
    }
    let mut invocations = GraphGpuInvocationBatch::with_capacity(&program, identities.len())?;
    let mut base_masks = Vec::new();
    crate::memory::reserve_exact(&mut base_masks, identities.len(), "resident base masks")?;
    for identity in &identities {
        let base_mask = external_scalar_keys.iter().all(|key| {
            boundary_values.get(key).is_some_and(|value| match value {
                GraphValue::Scalar(field) => field.values.contains_key(identity),
                _ => false,
            })
        });
        base_masks.push(base_mask);
        let candidate = candidate_stream.and_then(|stream| {
            stream
                .candidates
                .binary_search_by_key(identity, |candidate| candidate.identity)
                .ok()
                .and_then(|index| stream.candidates.get(index))
        });
        let mut noise_components = [None; GRAPH_GPU_MAX_INSTRUCTIONS];
        let mut noise_component_count = 0_usize;
        for node in noise_inputs.keys() {
            let compiled = nodes
                .iter()
                .find(|candidate| candidate.definition.guid == *node)
                .ok_or(Error::NumericOverflow)?;
            let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                path: compiled.debug_symbol.label.clone(),
                reason: "resident noise input has no candidate position".to_owned(),
            })?;
            let frequency =
                fixed_parameter(compiled, "frequency", DecisionScalar::from_bits(65_536))?;
            let channel = u32_parameter(compiled, "channel", 0)?;
            noise_components[noise_component_count] = Some((
                *node,
                coherent_value_noise_components(
                    compiled,
                    state,
                    candidate.position,
                    frequency,
                    channel,
                )?,
            ));
            noise_component_count += 1;
        }
        invocations.push(bindings.iter().map(|binding| -> Result<GraphGpuValue> {
            Ok(match binding {
                ResidentInputBinding::CandidateMask => GraphGpuValue::CandidateMask(base_mask),
                ResidentInputBinding::ExternalScalar(key) => {
                    let value = boundary_values.get(key).and_then(|value| match value {
                        GraphValue::Scalar(field) => field.values.get(identity),
                        _ => None,
                    });
                    GraphGpuValue::FixedScalar(value.map_or(0, |value| value.bits()))
                }
                ResidentInputBinding::NoiseCorner { node, corner } => {
                    let components = noise_components[..noise_component_count]
                        .iter()
                        .flatten()
                        .find(|(candidate, _)| candidate == node)
                        .ok_or(Error::NumericOverflow)?;
                    GraphGpuValue::FixedScalar(components.1.0[*corner].bits())
                }
                ResidentInputBinding::NoiseBlend { node, axis } => {
                    let components = noise_components[..noise_component_count]
                        .iter()
                        .flatten()
                        .find(|(candidate, _)| candidate == node)
                        .ok_or(Error::NumericOverflow)?;
                    GraphGpuValue::Unit(components.1.1[*axis].bits())
                }
                ResidentInputBinding::GradientPosition { node, axis } => {
                    let compiled = nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == *node)
                        .ok_or(Error::NumericOverflow)?;
                    let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                        path: compiled.debug_symbol.label.clone(),
                        reason: "resident gradient input has no candidate position".to_owned(),
                    })?;
                    GraphGpuValue::WorldTick(candidate.position.global_ticks()[*axis])
                }
                ResidentInputBinding::GradientOrigin { node, axis } => GraphGpuValue::WorldTick(
                    world_position_parameter(
                        nodes
                            .iter()
                            .find(|candidate| candidate.definition.guid == *node)
                            .ok_or(Error::NumericOverflow)?,
                        "exactOrigin",
                    )?[*axis],
                ),
            })
        }))?;
    }
    let allocation_shape = ResidentGroupAllocationShape {
        invocations: identities.len() as u64,
        inputs: input_capacity as u64,
        instructions: nodes.len() as u64,
        curve_instructions: nodes
            .iter()
            .filter(|node| node.definition.operator == GraphOperator::Curve)
            .count() as u64,
        curve_points: nodes
            .iter()
            .filter(|node| node.definition.operator == GraphOperator::Curve)
            .count()
            .checked_mul(GRAPH_GPU_MAX_CURVE_POINTS)
            .ok_or(Error::NumericOverflow)? as u64,
        external_inputs: boundary_values.len() as u64,
        external_pin_bytes: boundary_values.keys().try_fold(0_u64, |total, (_, pin)| {
            total
                .checked_add(pin.len() as u64)
                .ok_or(Error::NumericOverflow)
        })?,
        output_entries: group.outputs.len() as u64,
        output_pin_bytes: group.outputs.iter().try_fold(0_u64, |total, output| {
            total
                .checked_add(output.pin.pin.len() as u64)
                .ok_or(Error::NumericOverflow)
        })?,
        noise_nodes: noise_node_count as u64,
        gradient_nodes: gradient_node_count as u64,
        candidate_output: group
            .outputs
            .iter()
            .any(|output| output.domain == GraphDomain::Candidates),
        scalar_output: group
            .outputs
            .iter()
            .any(|output| output.domain == GraphDomain::ScalarField),
    };
    state.check_transient_memory(resident_group_scratch_bytes(allocation_shape)?)?;
    let gpu_outputs = execute_compute_program(state, compute, &program, &invocations)?;
    let final_candidate_count = gpu_outputs
        .iter()
        .filter(|output| output.candidate_mask)
        .count() as u64;
    let mut outputs = BTreeMap::new();
    if let Some(boundary) = field_boundary {
        let field_node_index = nodes
            .iter()
            .position(|node| node.definition.guid == boundary.pin.node.node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graphGpuGroup.output".to_owned(),
                reason: "resident field boundary node is missing".to_owned(),
            })?;
        let retain_terminal_mask =
            field_importance_index.is_some_and(|importance| importance < field_node_index);
        match boundary.domain {
            GraphDomain::ScalarField => {
                let mut values = BTreeMap::new();
                for ((identity, base_mask), output) in
                    identities.iter().zip(&base_masks).zip(&gpu_outputs)
                {
                    if *base_mask && (!retain_terminal_mask || output.candidate_mask) {
                        if output.value_type != Some(GraphGpuRegisterType::FixedScalar) {
                            return Err(compute_output_type_error(nodes[field_node_index]));
                        }
                        values.insert(*identity, DecisionScalar::from_bits(output.value as i32));
                    }
                }
                outputs.insert(
                    (boundary.pin.node.node, boundary.pin.pin.clone()),
                    GraphValue::Scalar(ScalarFieldSamples { lineage, values }),
                );
            }
            _ => unreachable!("field boundary was filtered by domain"),
        }
    }

    if let Some(importance_index) = field_importance_index {
        let node = nodes[importance_index];
        let stream = candidate_stream.ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "resident candidate mask has no candidate metadata".to_owned(),
        })?;
        let mut accepted = Vec::new();
        crate::memory::reserve_exact(
            &mut accepted,
            stream.candidates.len(),
            "resident accepted candidates",
        )?;
        for (candidate, output) in stream.candidates.iter().zip(&gpu_outputs) {
            if output.candidate_mask {
                accepted.push(candidate.clone());
            } else {
                reject_candidate(
                    node,
                    candidate,
                    stream.lineage,
                    CandidateRejectionReason::Threshold,
                    candidate.family,
                    candidate.variation,
                    state,
                )?;
            }
        }
        let accepted = CandidateStream {
            lineage: stream.lineage,
            candidates: accepted,
        };
        for candidate in &accepted.candidates {
            record_candidate_decision(node, candidate, boundary_values.values(), state);
        }
        if let Some(boundary) = group
            .outputs
            .iter()
            .find(|output| output.domain == GraphDomain::Candidates)
        {
            outputs.insert(
                (boundary.pin.node.node, boundary.pin.pin.clone()),
                GraphValue::Candidates(accepted),
            );
        }
    }

    Ok(ResidentGroupEvaluation {
        outputs,
        base_candidate_count: identities.len() as u64,
        final_candidate_count,
        field_importance_index,
        invocation_count: invocations.invocation_count() as u64,
    })
}

fn evaluate_unit(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    demand: &CompiledDemandSlice,
    interface_values: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    let mut incoming = BTreeMap::<u128, Vec<_>>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        let edge_count = unit_demand
            .edges
            .iter()
            .filter(|edge| edge.to_node == node.definition.guid)
            .count();
        let mut edges = Vec::new();
        crate::memory::reserve_exact(&mut edges, edge_count, "incoming graph edges")?;
        incoming.insert(node.definition.guid, edges);
    }
    for edge in &unit_demand.edges {
        incoming
            .get_mut(&edge.to_node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.execution".to_owned(),
                reason: "active graph node has no incoming-edge bucket".to_owned(),
            })?
            .push(edge);
    }
    let mut remaining_uses = BTreeMap::<(u128, String), u64>::new();
    for edge in &unit_demand.edges {
        *remaining_uses
            .entry((edge.from_node, edge.from_pin.clone()))
            .or_default() += 1;
    }
    for output in &unit.outputs {
        if unit_demand.outputs.contains(&output.name) {
            *remaining_uses
                .entry((output.node, output.pin.clone()))
                .or_default() += 1;
        }
    }
    let mut values: BTreeMap<(u128, String), GraphValue> = BTreeMap::new();
    let mut live_bytes = 0_u64;
    let mut executed_resident_nodes = BTreeSet::new();
    let execution_plan = state.execution_plan;
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        state.check_abort()?;
        state.current_node_live_bytes = live_bytes;
        let address = node.address();
        let loads_global = state.should_load_global_node(&address);
        let scheduled_group =
            execution_plan
                .group_for(&address)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "execution plan omitted the compiled node".to_owned(),
                })?;
        if !loads_global && scheduled_group.domain == GraphExecutionDomain::SlangCompute {
            if executed_resident_nodes.contains(&node.definition.guid) {
                continue;
            }
            if scheduled_group.nodes.iter().any(|member| {
                !unit_demand.contains_node(member.address.node)
                    || unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == member.address.node)
                        .is_none_or(|candidate| state.should_load_global_node(&candidate.address()))
            }) {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident group crosses the active spatial evaluation boundary"
                        .to_owned(),
                });
            }
            let mut boundary_values = BTreeMap::new();
            for edge in unit_demand.edges.iter().filter(|edge| {
                !resident_group_contains(scheduled_group, edge.from_node)
                    && resident_group_contains(scheduled_group, edge.to_node)
            }) {
                let value = if let Some(value) = values
                    .get(&(edge.from_node, edge.from_pin.clone()))
                    .cloned()
                {
                    value
                } else {
                    let source_node = unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == edge.from_node)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident upstream compiled node is missing".to_owned(),
                        })?;
                    state
                        .load_global_node_outputs(source_node, demand)?
                        .remove(&edge.from_pin)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident boundary input is missing".to_owned(),
                        })?
                };
                boundary_values.insert((edge.from_node, edge.from_pin.clone()), value);
            }
            let input_value_bytes = boundary_values.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            let started = Instant::now();
            let transferred_before = state.transferred_bytes;
            state.current_node_live_bytes = live_bytes
                .checked_add(input_value_bytes)
                .ok_or(Error::NumericOverflow)?;
            let result = evaluate_resident_group(unit, scheduled_group, &boundary_values, state)?;
            state.current_node_live_bytes = 0;
            state.check_abort()?;
            let transfer_bytes = state
                .transferred_bytes
                .checked_sub(transferred_before)
                .ok_or(Error::NumericOverflow)?;
            let output_bytes = result.outputs.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            let transient_bytes = live_bytes
                .checked_add(input_value_bytes)
                .and_then(|bytes| bytes.checked_add(output_bytes))
                .ok_or(Error::NumericOverflow)?;
            state.check_count(
                "candidate count",
                result
                    .base_candidate_count
                    .max(result.final_candidate_count),
                state.graph.limits.max_candidates,
            )?;
            if transient_bytes > state.graph.limits.max_memory_bytes {
                return Err(Error::GraphLimit {
                    resource: "memory bytes",
                    requested: transient_bytes,
                    limit: state.graph.limits.max_memory_bytes,
                });
            }
            if state.transferred_bytes > state.graph.limits.max_transfer_bytes {
                return Err(Error::GraphLimit {
                    resource: "transfer bytes",
                    requested: state.transferred_bytes,
                    limit: state.graph.limits.max_transfer_bytes,
                });
            }
            let elapsed_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
            state
                .diagnostics
                .gpu_groups
                .push(GpuGroupEvaluationDiagnostic {
                    nodes: scheduled_group
                        .nodes
                        .iter()
                        .map(|member| member.address.clone())
                        .collect(),
                    invocation_count: result.invocation_count,
                    transfer_bytes,
                    output_bytes,
                    elapsed_micros,
                });
            for (index, member) in scheduled_group.nodes.iter().enumerate() {
                let compiled = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == member.address.node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "resident diagnostic node is missing".to_owned(),
                    })?;
                let after_mask = result
                    .field_importance_index
                    .is_some_and(|importance| index > importance);
                let is_mask = result.field_importance_index == Some(index);
                let input_candidates = if after_mask {
                    result.final_candidate_count
                } else {
                    result.base_candidate_count
                };
                let output_candidates = if after_mask || is_mask {
                    result.final_candidate_count
                } else {
                    result.base_candidate_count
                };
                let retained_bytes = result
                    .outputs
                    .iter()
                    .filter(|((source, _), _)| *source == member.address.node)
                    .try_fold(0_u64, |total, (_, value)| {
                        total
                            .checked_add(value.requested_memory_bytes()?)
                            .ok_or(Error::NumericOverflow)
                    })?;
                state.diagnostics.nodes.push(NodeEvaluationDiagnostic {
                    module_path: compiled.debug_symbol.module_path.clone(),
                    node: compiled.definition.guid,
                    operator: compiled.definition.operator,
                    symbol: compiled.debug_symbol.label.clone(),
                    input_candidates,
                    output_candidates,
                    output_bytes: retained_bytes,
                    transfer_bytes: 0,
                    predicted_transfer_bytes: compiled.estimate.transfer_bytes,
                    elapsed_micros: 0,
                    execution_domain: GraphExecutionDomain::SlangCompute,
                });
                state.capture_resident_global_outputs(compiled, &result.outputs)?;
            }
            for ((source, pin), value) in result.outputs {
                let source_node = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == source)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "resident boundary output node is missing".to_owned(),
                    })?;
                let expected = source_node
                    .outputs
                    .iter()
                    .find(|candidate| candidate.name == pin)
                    .ok_or_else(|| Error::GraphDocument {
                        path: source_node.debug_symbol.label.clone(),
                        reason: format!("resident program emitted unknown pin '{pin}'"),
                    })?;
                if value.domain() != expected.domain {
                    return Err(Error::GraphDocument {
                        path: source_node.debug_symbol.label.clone(),
                        reason: "resident program emitted the wrong value domain".to_owned(),
                    });
                }
                let key = (source, pin);
                if remaining_uses.get(&key).copied().unwrap_or(0) != 0 {
                    live_bytes = live_bytes
                        .checked_add(value.requested_memory_bytes()?)
                        .ok_or(Error::NumericOverflow)?;
                    values.insert(key, value);
                }
            }
            for edge in unit_demand.edges.iter().filter(|edge| {
                !resident_group_contains(scheduled_group, edge.from_node)
                    && resident_group_contains(scheduled_group, edge.to_node)
            }) {
                let key = (edge.from_node, edge.from_pin.clone());
                let Some(uses) = remaining_uses.get_mut(&key) else {
                    continue;
                };
                *uses = uses.checked_sub(1).ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident input live range was consumed more than once".to_owned(),
                })?;
                if *uses == 0
                    && let Some(value) = values.remove(&key)
                {
                    live_bytes = live_bytes
                        .checked_sub(value.requested_memory_bytes()?)
                        .ok_or(Error::NumericOverflow)?;
                }
            }
            executed_resident_nodes.extend(
                scheduled_group
                    .nodes
                    .iter()
                    .map(|member| member.address.node),
            );
            continue;
        }
        let loads_global = state.should_load_global_node(&node.address());
        let mut inputs = BTreeMap::new();
        for edge in incoming
            .get(&node.definition.guid)
            .into_iter()
            .flatten()
            .filter(|_| !loads_global)
        {
            let value = if let Some(value) = values
                .get(&(edge.from_node, edge.from_pin.clone()))
                .cloned()
            {
                value
            } else {
                let source_node = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == edge.from_node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream compiled node is missing".to_owned(),
                    })?;
                let stage = state
                    .graph
                    .spatial_plan()
                    .global_stage_for_node(&source_node.address())
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream value is missing".to_owned(),
                    })?;
                if state
                    .scope
                    .current_global_stage()
                    .is_some_and(|current| current.id == stage.id)
                {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "same-stage upstream value was not produced".to_owned(),
                    });
                }
                state
                    .load_global_node_outputs(source_node, demand)?
                    .remove(&edge.from_pin)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "materialized upstream pin is missing".to_owned(),
                    })?
            };
            inputs.insert(edge.to_pin.clone(), value);
        }
        let input_candidates = inputs
            .values()
            .map(GraphValue::candidate_count)
            .max()
            .unwrap_or(0) as u64;
        let input_value_bytes = inputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.requested_memory_bytes()?)
                .ok_or(Error::NumericOverflow)
        })?;
        let started = Instant::now();
        let transferred_before = state.transferred_bytes;
        state.current_node_live_bytes = live_bytes
            .checked_add(input_value_bytes)
            .ok_or(Error::NumericOverflow)?;
        let (outputs, execution_domain, loaded_global) =
            evaluate_node_scheduled(unit, node, &inputs, interface_values, demand, state)?;
        state.check_abort()?;
        if !loaded_global {
            let decision_candidates = outputs
                .values()
                .map(GraphValue::candidate_count)
                .max()
                .unwrap_or(0) as u64;
            let decision_output_bytes = outputs.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            state.check_transient_memory(checked_memory_sum([
                decision_output_bytes,
                requested_btree_bound::<CandidateIdentity, ()>(decision_candidates)?,
            ])?)?;
            record_candidate_decisions(node, &inputs, &outputs, state)?;
            state.capture_global_outputs(node, &outputs)?;
        }
        state.current_node_live_bytes = 0;
        let output_candidates = outputs
            .values()
            .map(GraphValue::candidate_count)
            .max()
            .unwrap_or(0) as u64;
        let output_bytes = outputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.requested_memory_bytes()?)
                .ok_or(Error::NumericOverflow)
        })?;
        state.check_count(
            "candidate count",
            output_candidates,
            state.graph.limits.max_candidates,
        )?;
        let transient_bytes = live_bytes
            .checked_add(input_value_bytes)
            .and_then(|value| value.checked_add(output_bytes))
            .ok_or(Error::NumericOverflow)?;
        if transient_bytes > state.graph.limits.max_memory_bytes {
            return Err(Error::GraphLimit {
                resource: "memory bytes",
                requested: transient_bytes,
                limit: state.graph.limits.max_memory_bytes,
            });
        }
        let transfer_bytes = state
            .transferred_bytes
            .checked_sub(transferred_before)
            .ok_or(Error::NumericOverflow)?;
        if state.transferred_bytes > state.graph.limits.max_transfer_bytes {
            return Err(Error::GraphLimit {
                resource: "transfer bytes",
                requested: state.transferred_bytes,
                limit: state.graph.limits.max_transfer_bytes,
            });
        }
        if !loaded_global {
            state.diagnostics.nodes.push(NodeEvaluationDiagnostic {
                module_path: node.debug_symbol.module_path.clone(),
                node: node.definition.guid,
                operator: node.definition.operator,
                symbol: node.debug_symbol.label.clone(),
                input_candidates,
                output_candidates,
                output_bytes,
                transfer_bytes,
                predicted_transfer_bytes: node.estimate.transfer_bytes,
                elapsed_micros: started.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
                execution_domain,
            });
        }
        for (pin, value) in outputs {
            let expected = node
                .outputs
                .iter()
                .find(|candidate| candidate.name == pin)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!("operator emitted unknown pin '{pin}'"),
                })?;
            if value.domain() != expected.domain {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "operator emitted the wrong value domain".to_owned(),
                });
            }
            let key = (node.definition.guid, pin);
            if remaining_uses.get(&key).copied().unwrap_or(0) != 0 {
                live_bytes = live_bytes
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)?;
                values.insert(key, value);
            }
        }
        for edge in incoming.get(&node.definition.guid).into_iter().flatten() {
            let key = (edge.from_node, edge.from_pin.clone());
            let Some(uses) = remaining_uses.get_mut(&key) else {
                continue;
            };
            *uses = uses.checked_sub(1).ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "input live range was consumed more than once".to_owned(),
            })?;
            if *uses == 0
                && let Some(value) = values.remove(&key)
            {
                live_bytes = live_bytes
                    .checked_sub(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
    }
    let mut outputs = BTreeMap::new();
    for output in &unit.outputs {
        if !unit_demand.outputs.contains(&output.name) {
            continue;
        }
        if let Some(value) = values.get(&(output.node, output.pin.clone())).cloned() {
            outputs.insert(output.name.clone(), value);
        } else {
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "source value is missing".to_owned(),
            });
        }
    }
    Ok(outputs)
}

fn record_candidate_decisions(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    outputs: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in outputs.values() {
        match value {
            GraphValue::Candidates(stream) => {
                for candidate in &stream.candidates {
                    if seen.insert(candidate.identity) {
                        record_candidate_decision(node, candidate, inputs.values(), state);
                    }
                }
            }
            GraphValue::Surface(surface)
                if node.definition.operator == GraphOperator::SurfaceProjection =>
            {
                for identity in surface.values.keys() {
                    if !seen.insert(*identity) {
                        continue;
                    }
                    let candidate = inputs
                        .values()
                        .filter_map(|value| match value {
                            GraphValue::Candidates(stream) => Some(stream),
                            _ => None,
                        })
                        .find_map(|stream| {
                            stream
                                .candidates
                                .binary_search_by_key(identity, |candidate| candidate.identity)
                                .ok()
                                .and_then(|index| stream.candidates.get(index))
                        })
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: format!(
                                "projected surface sample {:?} has no input candidate",
                                identity
                            ),
                        })?;
                    record_candidate_decision(node, candidate, inputs.values(), state);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn record_candidate_decision<'a>(
    node: &CompiledGraphNode,
    candidate: &GraphCandidate,
    inputs: impl IntoIterator<Item = &'a GraphValue>,
    state: &mut EvaluationState<'_>,
) {
    let previous = state.candidate_decisions.get(&candidate.identity).copied();
    let mut parents = BTreeSet::new();
    if let Some(previous) = previous {
        parents.insert(previous);
    }
    for reference in [candidate.parent, candidate.colony].into_iter().flatten() {
        if let Some(parent) = state.candidate_decisions.get(&reference.identity) {
            parents.insert(*parent);
        }
    }
    if parents.is_empty() && candidate.identity.ancestor != 0 {
        for stream in inputs.into_iter().filter_map(|value| match value {
            GraphValue::Candidates(stream) => Some(stream),
            _ => None,
        }) {
            for ancestor in &stream.candidates {
                if ancestor.identity.ordinal == candidate.identity.ancestor
                    && let Some(parent) = state.candidate_decisions.get(&ancestor.identity)
                {
                    parents.insert(*parent);
                }
            }
        }
    }
    let outcome = if previous.is_none() && parents.is_empty() {
        ProvenanceDecisionOutcome::Produced
    } else {
        ProvenanceDecisionOutcome::Retained
    };
    let decision = state.provenance.intern_decision(ProvenanceDecision {
        parents: parents.into_iter().collect(),
        subgraph_path: node.debug_symbol.module_path.clone(),
        node: node.definition.guid,
        operator: node.definition.operator,
        candidate: candidate.identity.ordinal,
        outcome,
    });
    state
        .candidate_decisions
        .insert(candidate.identity, decision);
}

fn evaluate_node_scheduled(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    interface_values: &BTreeMap<String, GraphValue>,
    demand: &CompiledDemandSlice,
    state: &mut EvaluationState<'_>,
) -> Result<(BTreeMap<String, GraphValue>, GraphExecutionDomain, bool)> {
    if state.should_load_global_node(&node.address()) {
        return Ok((
            state.load_global_node_outputs(node, demand)?,
            GraphExecutionDomain::ReferenceCpu,
            true,
        ));
    }
    let domain = state
        .execution_plan
        .domain_for(&node.debug_symbol.module_path, node.definition.guid)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "execution plan omitted the compiled node".to_owned(),
        })?;
    match domain {
        GraphExecutionDomain::SlangCompute => Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "resident groups must execute at the unit scheduler boundary".to_owned(),
        }),
        GraphExecutionDomain::ReferenceCpu => Ok((
            evaluate_node(unit, node, inputs, interface_values, demand, state)?,
            GraphExecutionDomain::ReferenceCpu,
            false,
        )),
        GraphExecutionDomain::ParallelCpu => Ok((
            evaluate_node(unit, node, inputs, interface_values, demand, state)?,
            GraphExecutionDomain::ParallelCpu,
            false,
        )),
    }
}

fn execute_compute_program(
    state: &mut EvaluationState<'_>,
    compute: &dyn GraphComputeExecutor,
    program: &GraphGpuProgram,
    invocations: &GraphGpuInvocationBatch,
) -> Result<Vec<crate::GraphGpuOutput>> {
    if invocations.invocation_count() == 0 {
        return Ok(Vec::new());
    }
    state.check_abort()?;
    let invocation_words =
        u64::try_from(invocations.encoded_word_count()).map_err(|_| Error::NumericOverflow)?;
    let output_words = u64::try_from(invocations.invocation_count())
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(
            u64::try_from(crate::GRAPH_GPU_OUTPUT_WORDS).map_err(|_| Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let transfer_bytes = u64::try_from(program.encoded_word_count())
        .map_err(|_| Error::NumericOverflow)?
        .checked_add(invocation_words)
        .and_then(|words| words.checked_add(output_words))
        .and_then(|words| words.checked_mul(std::mem::size_of::<u32>() as u64))
        .ok_or(Error::NumericOverflow)?;
    let requested = state
        .transferred_bytes
        .checked_add(transfer_bytes)
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "transfer bytes",
        requested,
        state.graph.limits.max_transfer_bytes,
    )?;
    let outputs =
        compute.execute_program(program, invocations, state.cancellation, state.deadline)?;
    state.transferred_bytes = requested;
    state.check_abort()?;
    if outputs.len() != invocations.invocation_count() {
        return Err(Error::GraphDocument {
            path: "slang-compute.outputs".to_owned(),
            reason: "compute executor returned the wrong result count".to_owned(),
        });
    }
    if outputs.iter().any(|output| !output.valid) {
        return Err(Error::NumericOverflow);
    }
    Ok(outputs)
}

fn compute_output_type_error(node: &CompiledGraphNode) -> Error {
    Error::GraphDocument {
        path: node.debug_symbol.label.clone(),
        reason: "compute executor returned the wrong output type".to_owned(),
    }
}

fn evaluate_node(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    interface_values: &BTreeMap<String, GraphValue>,
    demand: &CompiledDemandSlice,
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    use GraphOperator as O;
    let output_demand = NodeOutputDemand::new(demand, node);
    let produced = match node.definition.operator {
        O::InterfaceInput => {
            let name = string_parameter(node, "name", "")?;
            let value =
                interface_values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: format!("module interface input '{name}' is missing"),
                    })?;
            singleton("value", value)
        }
        O::RegionInput => singleton(
            "regions",
            GraphValue::Regions(if state.inputs.regions.is_empty() {
                canonical_cell_regions(
                    state.inputs.read_bounds,
                    state.inputs.output_cell.level(),
                    0,
                )?
            } else {
                let count = state
                    .inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .count();
                let mut regions = Vec::new();
                crate::memory::reserve_exact(&mut regions, count, "biome input regions")?;
                regions.extend(
                    state
                        .inputs
                        .regions
                        .iter()
                        .filter(|region| region.kind == EvaluationRegionKind::Biome)
                        .copied(),
                );
                regions
            }),
        ),
        O::SplineInput => singleton("splines", GraphValue::Splines(state.inputs.splines.clone())),
        O::SpeciesInput => singleton("species", GraphValue::Species(unit.palette.clone())),
        O::CommunityInput => {
            let mut tables = CommunityTables {
                competition: unit.competition.clone(),
                companions: unit.companions.clone(),
                succession: unit.succession.clone(),
            };
            tables.canonicalize();
            singleton("communities", GraphValue::Communities(tables))
        }
        O::ExplicitAnchors => singleton(
            "candidates",
            GraphValue::Candidates(explicit_anchor_candidates(node, state)?),
        ),
        O::StratifiedCoverage => singleton(
            "candidates",
            GraphValue::Candidates(stratified_candidates(
                node,
                regions_input(inputs, "regions")?,
                state,
            )?),
        ),
        O::BlueNoisePoisson => singleton(
            "candidates",
            GraphValue::Candidates(blue_noise_candidates(
                node,
                regions_input(inputs, "regions")?,
                state,
            )?),
        ),
        O::SurfaceProjection => {
            let (outputs, retained_count) = project_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                output_demand,
                state,
            )?;
            state.diagnostics.candidate_count =
                state.diagnostics.candidate_count.max(retained_count);
            outputs
        }
        O::FieldSample => singleton(
            "field",
            sample_field(
                node,
                candidates_input(inputs, "candidates")?,
                unit.require_authoritative_fields,
                state,
            )?,
        ),
        O::PaintedTile => singleton(
            "field",
            GraphValue::Scalar(sample_painted_tile(
                node,
                candidates_input(inputs, "candidates")?,
                unit.require_authoritative_fields,
                state,
            )?),
        ),
        O::Noise => singleton(
            "field",
            GraphValue::Scalar(noise_field(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::Gradient => singleton(
            "field",
            GraphValue::Scalar(gradient_field(
                node,
                candidates_input(inputs, "candidates")?,
            )?),
        ),
        O::Curve => singleton(
            "field",
            GraphValue::Scalar(curve_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::Remap => singleton(
            "field",
            GraphValue::Scalar(remap_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::Combine => singleton(
            "field",
            GraphValue::Scalar(combine_fields(
                node,
                scalar_input(inputs, "left")?,
                scalar_input(inputs, "right")?,
            )?),
        ),
        O::Clamp => singleton(
            "field",
            GraphValue::Scalar(clamp_field(node, scalar_input(inputs, "field")?)?),
        ),
        O::DistanceField => singleton(
            "field",
            GraphValue::Scalar(distance_field(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::WeightedElimination => singleton(
            "candidates",
            GraphValue::Candidates(weighted_elimination(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                state,
            )?),
        ),
        O::VariableSpacing => singleton(
            "candidates",
            GraphValue::Candidates(variable_spacing(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "radius")?,
                state,
            )?),
        ),
        O::Competition => singleton(
            "candidates",
            GraphValue::Candidates(competition_claims(
                node,
                candidates_input(inputs, "candidates")?,
                communities_input(inputs, "communities")?,
                state,
            )?),
        ),
        O::FieldImportance => singleton(
            "candidates",
            GraphValue::Candidates(threshold_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                state,
            )?),
        ),
        O::Suitability => singleton(
            "candidates",
            GraphValue::Candidates(suitability_candidates(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                unit,
                state,
            )?),
        ),
        O::ClusterPatchColony => singleton(
            "candidates",
            GraphValue::Candidates(expand_cluster(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::RecursiveCompanion => singleton(
            "candidates",
            GraphValue::Candidates(expand_companions(
                node,
                candidates_input(inputs, "candidates")?,
                unit,
                state,
            )?),
        ),
        O::SplineFollow => singleton(
            "candidates",
            GraphValue::Candidates(follow_splines(
                node,
                splines_input(inputs, "splines")?,
                state,
            )?),
        ),
        O::Transform => singleton(
            "candidates",
            GraphValue::Candidates(transform_candidates(node, inputs, state)?),
        ),
        O::PriorityExclusion => singleton(
            "candidates",
            GraphValue::Candidates(priority_exclusion(
                node,
                candidates_input(inputs, "candidates")?,
                scalar_input(inputs, "weights")?,
                scalar_input(inputs, "radius")?,
                state,
            )?),
        ),
        O::BoundsOverlap => singleton(
            "candidates",
            GraphValue::Candidates(bounds_overlap(
                node,
                candidates_input(inputs, "candidates")?,
                state,
            )?),
        ),
        O::CommunityBlend => singleton(
            "candidates",
            GraphValue::Candidates(community_blend(node, inputs, unit, state)?),
        ),
        O::SuccessionInput => singleton(
            "candidates",
            GraphValue::Candidates(succession_input(
                node,
                candidates_input(inputs, "candidates")?,
                unit,
                state,
            )?),
        ),
        O::MacroOutput => singleton(
            "points",
            GraphValue::Macro(macro_output(node, inputs, state)?),
        ),
        O::MicroOutput => singleton(
            "micro",
            GraphValue::Micro(micro_output(node, inputs, state)?),
        ),
        O::DiagnosticOutput => singleton(
            "diagnostics",
            GraphValue::Diagnostics(vec![diagnostic_output(node, inputs, state)?]),
        ),
        O::ModuleCall => {
            let module = node.module.as_ref().ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "compiled module is missing".to_owned(),
            })?;
            evaluate_unit(
                module,
                child_demand_unit(demand, node)?,
                demand,
                inputs,
                state,
            )?
        }
    };
    let mut outputs = NodeOutputBuilder::new(output_demand);
    outputs.extend(produced)?;
    outputs.finish()
}

fn explicit_anchor_candidates(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let layer = guid_parameter(node, "layer", 0)?;
    state.check_count(
        "candidate count",
        state.inputs.anchors.len() as u64,
        state.graph.limits.max_candidates,
    )?;
    let candidate_count = state
        .inputs
        .anchors
        .iter()
        .filter(|anchor| {
            anchor.layer == layer && state.inputs.read_bounds.contains(anchor.point.position)
        })
        .count();
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        candidate_count,
        "explicit anchor candidates",
    )?;
    for anchor in state.inputs.anchors.iter().filter(|anchor| {
        anchor.layer == layer && state.inputs.read_bounds.contains(anchor.point.position)
    }) {
        let point = &anchor.point;
        point.validate()?;
        let radius = point_radius(point)?;
        candidates.push(GraphCandidate {
            identity: CandidateIdentity {
                node: node.definition.guid,
                node_address: node_execution_address(node, state),
                node_semantic_revision: node.definition.semantic_revision,
                ordinal: stable_ordinal(&[
                    &node_execution_address(node, state).to_be_bytes(),
                    &node.definition.semantic_revision.to_be_bytes(),
                    &point.id.bytes(),
                ])?,
                ancestor: 0,
            },
            owner: canonical_owner(point.position, node.definition.spatial.level())?,
            source_layer: anchor.layer,
            position: point.position,
            orientation: point.orientation,
            scale: point.scale,
            family: Some(point.family),
            variation: point.variation,
            parent: None,
            colony: None,
            priority: DecisionScalar::from_bits(i32::MAX),
            ecology_tick: point.ecology_tick,
            crown_radius: radius,
            root_radius: radius,
            attachment: point.attachment,
            surface_normal: None,
            surface_projection: point.surface_projection,
            authored_point: Some(point.clone()),
        });
    }
    let mut stream = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    stream.canonicalize()?;
    Ok(stream)
}

fn stratified_candidates(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let count = u64::from(u32_parameter(node, "count", 0)?);
    state.check_transient_memory(stage_region_scratch_bytes(regions.len() as u64)?)?;
    let regions = stage_regions(node, regions, state)?;
    if count == 0 || regions.is_empty() {
        return Ok(CandidateStream {
            lineage: candidate_lineage(node, state),
            candidates: Vec::new(),
        });
    }
    let jitter = unit_parameter(node, "jitter", UnitInterval::ZERO)?;
    let requested = count
        .checked_mul(regions.len() as u64)
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "candidate count",
        requested,
        state.graph.limits.max_candidates,
    )?;
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        usize::try_from(requested).map_err(|_| Error::NumericOverflow)?,
        "stratified candidates",
    )?;
    for region in regions {
        let side = integer_sqrt_ceil(count);
        for local in 0..count {
            state.check_abort()?;
            let ordinal = candidate_ordinal(node, state, region.id, local, 0)?;
            let x = local % side;
            let z = local / side;
            let stream = random_stream(
                node,
                state,
                "sampling",
                RandomSampleAddress::new(region.seed_cell, ordinal),
            )?;
            let position = stratified_position(region.bounds, x, z, side, jitter, stream)?;
            let mut candidate = default_candidate(node, state, ordinal, 0, position)?;
            candidate.source_layer = region.layer;
            candidates.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    result.canonicalize()?;
    Ok(result)
}

fn blue_noise_candidates(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let count = u64::from(u32_parameter(node, "count", 0)?);
    let radius = fixed_parameter(node, "radius", DecisionScalar::from_bits(0))?;
    let attempts = u64::from(u32_parameter(node, "attempts", 30)?).max(1);
    state.check_transient_memory(stage_region_scratch_bytes(regions.len() as u64)?)?;
    let regions = stage_regions(node, regions, state)?;
    if count == 0 || regions.is_empty() || radius.bits() <= 0 {
        return Ok(CandidateStream {
            lineage: candidate_lineage(node, state),
            candidates: Vec::new(),
        });
    }
    state.check_count(
        "candidate count",
        count
            .checked_mul(regions.len() as u64)
            .ok_or(Error::NumericOverflow)?,
        state.graph.limits.max_candidates,
    )?;
    let radius_ticks = fixed_meters_to_ticks(radius)?.unsigned_abs() as i128;
    let radius_squared = radius_ticks
        .checked_mul(radius_ticks)
        .ok_or(Error::NumericOverflow)?;
    let count = usize::try_from(count).map_err(|_| Error::NumericOverflow)?;
    let maximum_candidates = count
        .checked_mul(regions.len())
        .ok_or(Error::NumericOverflow)?;
    state.check_transient_memory(blue_noise_scratch_bytes(
        u64::try_from(maximum_candidates).map_err(|_| Error::NumericOverflow)?,
    )?)?;
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        maximum_candidates,
        "blue-noise accepted candidates",
    )?;
    for region in regions {
        let seed_ordinal = candidate_ordinal(node, state, region.id, 0, 1)?;
        let seed_stream = random_stream(
            node,
            state,
            "sampling",
            RandomSampleAddress::new(region.seed_cell, seed_ordinal).with_channel(1),
        )?;
        let mut seed = default_candidate(
            node,
            state,
            seed_ordinal,
            0,
            uniform_position(region.bounds, seed_stream, 0)?,
        )?;
        seed.source_layer = region.layer;
        let mut region_points = Vec::new();
        crate::memory::reserve_exact(&mut region_points, count, "blue-noise region candidates")?;
        region_points.push(seed.clone());
        let mut active = Vec::new();
        crate::memory::reserve_exact(&mut active, count, "blue-noise active candidates")?;
        active.push(seed);
        let mut proposal = 1_u64;
        while !active.is_empty() && region_points.len() < count {
            state.check_abort()?;
            let parent = active.remove(0);
            let mut produced = false;
            for attempt in 0..attempts {
                let ordinal = candidate_ordinal(node, state, region.id, proposal, 1)?;
                proposal = proposal.checked_add(1).ok_or(Error::NumericOverflow)?;
                let stream = random_stream(
                    node,
                    state,
                    "sampling",
                    RandomSampleAddress::new(region.seed_cell, ordinal)
                        .with_ancestor(parent.identity.ordinal)
                        .with_channel(u32::try_from(attempt).map_err(|_| Error::NumericOverflow)?),
                )?;
                let position = poisson_annulus_position(parent.position, radius_ticks, stream)?;
                let mut conflicts = false;
                for other in &region_points {
                    if distance_squared_xz(other.position, position)? < radius_squared {
                        conflicts = true;
                        break;
                    }
                }
                if !region.bounds.contains(position) || conflicts {
                    continue;
                }
                let mut candidate =
                    default_candidate(node, state, ordinal, parent.identity.ordinal, position)?;
                candidate.source_layer = region.layer;
                region_points.push(candidate.clone());
                active.push(candidate);
                produced = true;
                if region_points.len() >= count {
                    break;
                }
            }
            if produced {
                active.push(parent);
            }
        }
        accepted.extend(region_points);
    }
    accepted.sort_unstable_by_key(|candidate| candidate.identity);
    let mut globally_spaced: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut globally_spaced,
        accepted.len(),
        "blue-noise globally spaced candidates",
    )?;
    for candidate in accepted {
        let mut conflicts = false;
        for other in &globally_spaced {
            if distance_squared_xz(other.position, candidate.position)? < radius_squared {
                conflicts = true;
                break;
            }
        }
        if !conflicts {
            globally_spaced.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: globally_spaced,
    };
    result.canonicalize()?;
    Ok(result)
}

fn project_candidates(
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

fn select_surface_hit(
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

fn validate_surface_hit_contract(
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

fn quantize_surface_normal(normal: [f32; 3]) -> Result<[SignedUnit; 3]> {
    let [x, y, z] = normal.map(|value| SignedUnit::from_f64(f64::from(value)));
    Ok([x?, y?, z?])
}

fn quantize_surface_projection(projection: [f64; 3]) -> Result<[DecisionScalar; 3]> {
    let [x, y, z] = projection.map(DecisionScalar::from_f64);
    Ok([x?, y?, z?])
}

fn validate_canonical_tags(tags: &[WeightedSurfaceTag]) -> Result<()> {
    if tags.windows(2).any(|pair| pair[0].tag >= pair[1].tag) {
        return Err(Error::GraphDocument {
            path: "surface.tags".to_owned(),
            reason: "surface tags must be sorted and unique".to_owned(),
        });
    }
    Ok(())
}

fn sample_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    require_authoritative: bool,
    state: &mut EvaluationState<'_>,
) -> Result<GraphValue> {
    let channel = field_parameter(node, "channel")?;
    let derivative = field_derivative_parameter(node, "derivative", FieldDerivative::Value)?;
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let mut sampling = FieldSamplingContext {
        node,
        candidates,
        authoritative,
        require_authoritative,
        channel,
        derivative,
        state,
    };
    match derivative {
        FieldDerivative::Value => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_scalar,
                |provider, position| {
                    provider
                        .sample_scalar(channel, derivative, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Scalar(value) => {
                        Some(DecisionScalar::from_bits(value))
                    }
                    _ => None,
                },
                |value| QuantizedSurfaceFieldValue::Scalar(value.bits()),
                DecisionScalar::from_bits(0),
            )?;
            Ok(GraphValue::Scalar(ScalarFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
        FieldDerivative::Gradient => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_vector,
                |provider, position| {
                    provider
                        .sample_vector(channel, derivative, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Gradient(value) => Some(DecisionVec3 {
                        x: DecisionScalar::from_bits(value[0]),
                        y: DecisionScalar::from_bits(value[1]),
                        z: DecisionScalar::from_bits(value[2]),
                    }),
                    _ => None,
                },
                |value| {
                    QuantizedSurfaceFieldValue::Gradient([
                        value.x.bits(),
                        value.y.bits(),
                        value.z.bits(),
                    ])
                },
                DecisionVec3::default(),
            )?;
            Ok(GraphValue::Vector(VectorFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
        FieldDerivative::Hessian => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_hessian,
                |provider, position| {
                    provider
                        .sample_hessian(channel, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Hessian(value) => Some(DecisionHessian3 {
                        xx: DecisionScalar::from_bits(value[0]),
                        xy: DecisionScalar::from_bits(value[1]),
                        xz: DecisionScalar::from_bits(value[2]),
                        yy: DecisionScalar::from_bits(value[3]),
                        yz: DecisionScalar::from_bits(value[4]),
                        zz: DecisionScalar::from_bits(value[5]),
                    }),
                    _ => None,
                },
                |value| {
                    QuantizedSurfaceFieldValue::Hessian([
                        value.xx.bits(),
                        value.xy.bits(),
                        value.xz.bits(),
                        value.yy.bits(),
                        value.yz.bits(),
                        value.zz.bits(),
                    ])
                },
                DecisionHessian3::default(),
            )?;
            Ok(GraphValue::Hessian(HessianFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
    }
}

struct FieldSamplingContext<'a, 'b> {
    node: &'a CompiledGraphNode,
    candidates: &'a CandidateStream,
    authoritative: bool,
    require_authoritative: bool,
    channel: FieldChannel,
    derivative: FieldDerivative,
    state: &'a mut EvaluationState<'b>,
}

impl FieldSamplingContext<'_, '_> {
    fn sample<T: Copy + PartialEq>(
        &mut self,
        sample_tile: impl Fn(&EvaluationFieldTile, WorldPosition) -> Option<T>,
        sample_provider: impl Fn(&dyn SurfaceField, WorldPosition) -> saffron_spatial::Result<T>,
        decode_query: impl Fn(QuantizedSurfaceFieldValue) -> Option<T>,
        encode_query: impl Fn(T) -> QuantizedSurfaceFieldValue,
        cosmetic_default: T,
    ) -> Result<BTreeMap<CandidateIdentity, T>> {
        let mut values = BTreeMap::new();
        for candidate in &self.candidates.candidates {
            let sample = if self.authoritative {
                let exact = self
                    .state
                    .inputs
                    .surface_field_query_tiles
                    .iter()
                    .filter(|tile| {
                        tile.node == self.node.definition.guid
                            && tile.node_semantic_revision == self.node.definition.semantic_revision
                            && tile.channel == self.channel
                            && tile.derivative == self.derivative
                    })
                    .find_map(|tile| tile.sample(candidate.identity, candidate.position))
                    .and_then(&decode_query);
                let tiled = self
                    .state
                    .inputs
                    .fields
                    .iter()
                    .filter(|tile| {
                        matches!(tile.source, EvaluationFieldSource::SurfaceProvider { .. })
                            && tile.channel == self.channel
                            && tile.derivative == self.derivative
                    })
                    .find_map(|tile| sample_tile(tile, candidate.position));
                if exact.is_some() && tiled.is_some() && exact != tiled {
                    return Err(Error::GraphDocument {
                        path: self.node.debug_symbol.label.clone(),
                        reason: "exact and lattice field inputs disagree".to_owned(),
                    });
                }
                if let Some(value) = exact {
                    self.retain_query(candidate, encode_query(value))?;
                    Some(value)
                } else if let Some(value) = tiled {
                    Some(value)
                } else if self.state.pass == EvaluationPass::PrepareCanonicalInputs {
                    let mut prepared = None;
                    for provider in &self.state.inputs.surface_providers {
                        let descriptor = provider.descriptor();
                        if !descriptor.capabilities.authoritative_fields
                            || provider.availability(
                                self.channel,
                                self.derivative,
                                self.state.inputs.read_bounds,
                            ) == FieldAvailability::Unavailable
                        {
                            continue;
                        }
                        prepared = Some(sample_provider(provider.as_ref(), candidate.position)?);
                        break;
                    }
                    if let Some(value) = prepared {
                        self.retain_query(candidate, encode_query(value))?;
                        Some(value)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                self.state
                    .inputs
                    .surface_providers
                    .iter()
                    .find_map(|provider| {
                        if provider.availability(
                            self.channel,
                            self.derivative,
                            self.state.inputs.read_bounds,
                        ) == FieldAvailability::Unavailable
                        {
                            return None;
                        }
                        sample_provider(provider.as_ref(), candidate.position).ok()
                    })
            };
            if let Some(value) = sample {
                if self.authoritative && self.state.pass == EvaluationPass::PrepareCanonicalInputs {
                    self.retain_query(candidate, encode_query(value))?;
                }
                values.insert(candidate.identity, value);
            } else if self.authoritative || self.require_authoritative {
                return Err(Error::GraphAuthoritativeInput {
                    node: self.node.definition.guid,
                    input: format!(
                        "{} {:?} tile",
                        field_channel_name(self.channel),
                        self.derivative
                    ),
                });
            } else {
                values.insert(candidate.identity, cosmetic_default);
            }
        }
        Ok(values)
    }

    fn retain_query(
        &mut self,
        candidate: &GraphCandidate,
        encoded: QuantizedSurfaceFieldValue,
    ) -> Result<()> {
        let previous = self
            .state
            .prepared_surface_fields
            .entry((
                self.node.definition.guid,
                self.node.definition.semantic_revision,
                self.channel,
                self.derivative,
                self.state.inputs.surface_provider_set_hash,
            ))
            .or_default()
            .insert((candidate.identity, candidate.position), encoded);
        if previous.is_some_and(|previous| previous != encoded) {
            return Err(Error::GraphDocument {
                path: self.node.debug_symbol.label.clone(),
                reason: "canonical field replay produced conflicting values".to_owned(),
            });
        }
        Ok(())
    }
}

fn sample_painted_tile(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    require_authoritative: bool,
    state: &mut EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let channel = field_parameter(node, "channel")?;
    let layer = guid_parameter(node, "layer", 0)?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        if let Some(value) = sample_ordered_scalar_tiles(
            state.inputs.fields.iter().filter(|tile| {
                tile.source == EvaluationFieldSource::MapLayer(layer)
                    && tile.channel == channel
                    && tile.derivative == FieldDerivative::Value
            }),
            candidate.position,
        )? {
            values.insert(candidate.identity, value);
        } else if authoritative || require_authoritative {
            return Err(Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!(
                    "painted {} tile for layer {layer:032x}",
                    field_channel_name(channel)
                ),
            });
        } else {
            values.insert(candidate.identity, DecisionScalar::from_bits(0));
        }
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn sample_ordered_scalar_tiles<'a>(
    tiles: impl IntoIterator<Item = &'a EvaluationFieldTile>,
    position: WorldPosition,
) -> Result<Option<DecisionScalar>> {
    let mut result = None;
    for tile in tiles {
        let Some(value) = tile.sample_scalar(position) else {
            continue;
        };
        let weighted = scale_field_value(value, tile.weight)?;
        result = Some(match (result, tile.blend) {
            (None, FieldBlendOperator::Multiply) => {
                lerp_field_value(DecisionScalar::from_bits(65_536), value, tile.weight)?
            }
            (None, _) => weighted,
            (Some(current), FieldBlendOperator::Replace) => {
                lerp_field_value(current, value, tile.weight)?
            }
            (Some(current), FieldBlendOperator::Add) => current.checked_add(weighted)?,
            (Some(current), FieldBlendOperator::Multiply) => current.checked_mul(
                lerp_field_value(DecisionScalar::from_bits(65_536), value, tile.weight)?,
            )?,
            (Some(current), FieldBlendOperator::Minimum) => current.min(weighted),
            (Some(current), FieldBlendOperator::Maximum) => current.max(weighted),
        });
    }
    Ok(result)
}

fn scale_field_value(value: DecisionScalar, weight: UnitInterval) -> Result<DecisionScalar> {
    let bits = div_round_ties_even(
        i128::from(value.bits()) * i128::from(weight.bits()),
        i128::from(u16::MAX),
    )?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn lerp_field_value(
    start: DecisionScalar,
    end: DecisionScalar,
    weight: UnitInterval,
) -> Result<DecisionScalar> {
    let delta = i128::from(end.bits()) - i128::from(start.bits());
    let bits = i128::from(start.bits())
        + div_round_ties_even(delta * i128::from(weight.bits()), i128::from(u16::MAX))?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn noise_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let frequency = fixed_parameter(node, "frequency", DecisionScalar::from_bits(65_536))?;
    let amplitude = fixed_parameter(node, "amplitude", DecisionScalar::from_bits(65_536))?;
    let channel = u32_parameter(node, "channel", 0)?;
    if frequency.bits() <= 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "noise frequency must be positive".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        values.insert(
            candidate.identity,
            coherent_value_noise(node, state, candidate.position, frequency, channel)?
                .checked_mul(amplitude)?,
        );
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn coherent_value_noise(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    position: WorldPosition,
    frequency: DecisionScalar,
    channel: u32,
) -> Result<DecisionScalar> {
    let (corners, blend) =
        coherent_value_noise_components(node, state, position, frequency, channel)?;
    let x00 = corners[0].lerp(corners[1], blend[0])?;
    let x10 = corners[2].lerp(corners[3], blend[0])?;
    let x01 = corners[4].lerp(corners[5], blend[0])?;
    let x11 = corners[6].lerp(corners[7], blend[0])?;
    let y0 = x00.lerp(x10, blend[1])?;
    let y1 = x01.lerp(x11, blend[1])?;
    y0.lerp(y1, blend[2]).map_err(Into::into)
}

fn coherent_value_noise_components(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    position: WorldPosition,
    frequency: DecisionScalar,
    channel: u32,
) -> Result<([DecisionScalar; 8], [UnitInterval; 3])> {
    let ticks = position.global_ticks();
    let mut lattice = [0_i128; 3];
    let mut blend = [UnitInterval::ZERO; 3];
    for axis in 0..3 {
        let scaled = ticks[axis]
            .checked_mul(i128::from(frequency.bits()))
            .ok_or(Error::NumericOverflow)?;
        let coordinate = div_round_ties_even(scaled, i128::from(LOCAL_TICKS_PER_METER))?;
        lattice[axis] = coordinate.div_euclid(65_536);
        let fraction =
            u16::try_from(coordinate.rem_euclid(65_536)).map_err(|_| Error::NumericOverflow)?;
        blend[axis] = smooth_unit(UnitInterval::from_bits(fraction))?;
    }
    let mut corners = [DecisionScalar::from_bits(0); 8];
    for (index, value) in corners.iter_mut().enumerate() {
        let coordinate = [
            lattice[0] + i128::from((index & 1) as u8),
            lattice[1] + i128::from(((index >> 1) & 1) as u8),
            lattice[2] + i128::from(((index >> 2) & 1) as u8),
        ];
        let address = node_execution_address(node, state).to_be_bytes();
        let x = coordinate[0].to_be_bytes();
        let y = coordinate[1].to_be_bytes();
        let z = coordinate[2].to_be_bytes();
        let channel_bytes = channel.to_be_bytes();
        let ordinal = stable_ordinal(&[&address, &x, &y, &z, &channel_bytes])?;
        let stream = RandomStream::new(RandomDomain {
            map: u128::from(state.inputs.map.value()),
            node_guid: node_execution_address(node, state),
            node_semantic_revision: node.definition.semantic_revision,
            seed_namespace: seed_namespace(node, "noise")?,
            cell: WorldCellKey::base(0, 0, 0),
            candidate: ordinal,
            ancestor: 0,
            species: 0,
            channel,
        });
        let unit = i32::from(stream.unit(0, 0).bits());
        *value = DecisionScalar::from_bits(
            unit.checked_mul(2)
                .and_then(|value| value.checked_sub(i32::from(u16::MAX)))
                .ok_or(Error::NumericOverflow)?,
        );
    }
    Ok((corners, blend))
}

fn smooth_unit(value: UnitInterval) -> Result<UnitInterval> {
    let fixed = DecisionScalar::from_bits(i32::from(value.bits()));
    let squared = fixed.checked_mul(fixed)?;
    let factor = DecisionScalar::from_bits(3 * 65_536)
        .checked_sub(DecisionScalar::from_bits(2 * 65_536).checked_mul(fixed)?)?;
    let bits = squared
        .checked_mul(factor)?
        .bits()
        .clamp(0, i32::from(u16::MAX));
    Ok(UnitInterval::from_bits(
        u16::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn gradient_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
) -> Result<ScalarFieldSamples> {
    let direction = fixed_vec3_parameter(node, "direction", [DecisionScalar::from_bits(0); 3])?;
    let exact_origin = world_position_parameter(node, "exactOrigin")?;
    let scale = fixed_parameter(node, "scale", DecisionScalar::from_bits(0))?;
    let bias = fixed_parameter(node, "bias", DecisionScalar::from_bits(0))?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        values.insert(
            candidate.identity,
            DecisionScalar::from_bits(crate::evaluate_gradient_ramp(
                candidate.position.global_ticks(),
                exact_origin,
                direction.map(DecisionScalar::bits),
                scale.bits(),
                bias.bits(),
            )?),
        );
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn curve_field(node: &CompiledGraphNode, input: &ScalarFieldSamples) -> Result<ScalarFieldSamples> {
    let curve = curve_parameter(node, "curve")?;
    let values = input
        .values
        .iter()
        .map(|(identity, value)| {
            let bits = value.bits().clamp(0, i32::from(u16::MAX)) as u16;
            Ok((
                *identity,
                DecisionCurve::sample_points(curve, UnitInterval::from_bits(bits))?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values,
    })
}

fn remap_field(node: &CompiledGraphNode, input: &ScalarFieldSamples) -> Result<ScalarFieldSamples> {
    let input_min = fixed_parameter(node, "inputMin", DecisionScalar::from_bits(0))?;
    let input_max = fixed_parameter(node, "inputMax", DecisionScalar::from_bits(0))?;
    let output_min = fixed_parameter(node, "outputMin", DecisionScalar::from_bits(0))?;
    let output_max = fixed_parameter(node, "outputMax", DecisionScalar::from_bits(0))?;
    if input_min >= input_max {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "remap input range is empty".to_owned(),
        });
    }
    let input_span = input_max.checked_sub(input_min)?;
    let output_span = output_max.checked_sub(output_min)?;
    let values = input
        .values
        .iter()
        .map(|(identity, value)| {
            let clamped = (*value).clamp(input_min, input_max);
            let ratio = clamped.checked_sub(input_min)?.checked_div(input_span)?;
            Ok((
                *identity,
                output_min.checked_add(output_span.checked_mul(ratio)?)?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values,
    })
}

fn combine_fields(
    node: &CompiledGraphNode,
    left: &ScalarFieldSamples,
    right: &ScalarFieldSamples,
) -> Result<ScalarFieldSamples> {
    ensure_lineage(node, "right", left.lineage, right.lineage)?;
    let operation = combine_operation_parameter(node, "operation")?;
    let mut values = BTreeMap::new();
    for (identity, left) in &left.values {
        let Some(right) = right.values.get(identity) else {
            continue;
        };
        let value = match operation {
            GraphCombineOperation::Add => left.checked_add(*right)?,
            GraphCombineOperation::Multiply => left.checked_mul(*right)?,
            GraphCombineOperation::Minimum => (*left).min(*right),
            GraphCombineOperation::Maximum => (*left).max(*right),
        };
        values.insert(*identity, value);
    }
    Ok(ScalarFieldSamples {
        lineage: left.lineage,
        values,
    })
}

fn clamp_field(node: &CompiledGraphNode, input: &ScalarFieldSamples) -> Result<ScalarFieldSamples> {
    let minimum = fixed_parameter(node, "minimum", DecisionScalar::from_bits(0))?;
    let maximum = fixed_parameter(node, "maximum", DecisionScalar::from_bits(0))?;
    if minimum > maximum {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "clamp minimum exceeds maximum".to_owned(),
        });
    }
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values: input
            .values
            .iter()
            .map(|(identity, value)| (*identity, (*value).clamp(minimum, maximum)))
            .collect(),
    })
}

fn distance_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let source = distance_source_parameter(node, "source")?;
    let source_guid = guid_parameter(node, "sourceGuid", 0)?;
    let maximum_distance_value =
        fixed_parameter(node, "maximumDistance", DecisionScalar::from_bits(0))?;
    if maximum_distance_value.bits() < 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "maximum distance must be nonnegative".to_owned(),
        });
    }
    let maximum_distance = fixed_meters_to_ticks(maximum_distance_value)?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        let value = match source {
            GraphDistanceSource::Spline => {
                let distance_ticks = state
                    .inputs
                    .splines
                    .iter()
                    .filter(|spline| {
                        source_guid == 0 || spline.id == source_guid || spline.layer == source_guid
                    })
                    .flat_map(|spline| spline.points.windows(2))
                    .try_fold(None, |minimum, segment| {
                        let distance = point_segment_distance_ticks(
                            candidate.position,
                            segment[0],
                            segment[1],
                        )?;
                        Ok::<_, Error>(Some(
                            minimum.map_or(distance, |value: i128| value.min(distance)),
                        ))
                    })?
                    .unwrap_or(i128::MAX);
                DecisionScalar::from_bits(ticks_to_fixed_meters(
                    distance_ticks.min(maximum_distance),
                )?)
            }
            GraphDistanceSource::Shape => {
                let distance_ticks = state
                    .inputs
                    .regions
                    .iter()
                    .filter(|region| {
                        region.kind == EvaluationRegionKind::Shape
                            && (source_guid == 0
                                || region.id == source_guid
                                || region.layer == source_guid)
                    })
                    .try_fold(None, |minimum, region| {
                        let distance =
                            point_bounds_distance_ticks(candidate.position, region.bounds)?;
                        Ok::<_, Error>(Some(
                            minimum.map_or(distance, |value: i128| value.min(distance)),
                        ))
                    })?
                    .unwrap_or(i128::MAX);
                DecisionScalar::from_bits(ticks_to_fixed_meters(
                    distance_ticks.min(maximum_distance),
                )?)
            }
            GraphDistanceSource::Water | GraphDistanceSource::Blocker => {
                let channel = if source == GraphDistanceSource::Water {
                    FieldChannel::WaterDistance
                } else {
                    FieldChannel::SignedBlocker
                };
                let tiles = state.inputs.fields.iter().filter(|tile| {
                    tile.channel == channel
                        && tile.derivative == FieldDerivative::Value
                        && match tile.source {
                            EvaluationFieldSource::MapLayer(layer) => {
                                source_guid == 0 || layer == source_guid
                            }
                            EvaluationFieldSource::SurfaceProvider { .. } => source_guid == 0,
                        }
                });
                let sampled =
                    sample_ordered_scalar_tiles(tiles, candidate.position)?.ok_or_else(|| {
                        Error::GraphAuthoritativeInput {
                            node: node.definition.guid,
                            input: format!("{} distance field", field_channel_name(channel)),
                        }
                    })?;
                if source == GraphDistanceSource::Blocker {
                    sampled.clamp(
                        DecisionScalar::from_bits(
                            maximum_distance_value
                                .bits()
                                .checked_neg()
                                .ok_or(Error::NumericOverflow)?,
                        ),
                        maximum_distance_value,
                    )
                } else {
                    sampled.clamp(DecisionScalar::from_bits(0), maximum_distance_value)
                }
            }
        };
        values.insert(candidate.identity, value);
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn weighted_elimination(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let target = usize::try_from(u32_parameter(node, "targetCount", 0)?).unwrap_or(usize::MAX);
    let maximum_neighbours = usize::try_from(u32_parameter(node, "maximumNeighbours", 0)?)
        .map_err(|_| Error::NumericOverflow)?;
    if maximum_neighbours == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination maximum neighbours must be positive".to_owned(),
        });
    }
    if target >= candidates.candidates.len() {
        return Ok(candidates.clone());
    }
    let radius = fixed_meters_to_ticks(fixed_parameter(
        node,
        "eliminationRadius",
        DecisionScalar::from_bits(0),
    )?)?
    .checked_abs()
    .ok_or(Error::NumericOverflow)?;
    if radius == 0 {
        return Err(Error::GraphUnboundedInfluence {
            node: node.definition.guid,
        });
    }
    ensure_support_ticks(node, radius)?;
    let candidate_count = candidates.candidates.len();
    state.check_transient_memory(weighted_elimination_scratch_bytes(
        candidate_count as u64,
        target as u64,
        maximum_neighbours as u64,
    )?)?;
    let mut candidate_weights = Vec::new();
    crate::memory::reserve_exact(
        &mut candidate_weights,
        candidate_count,
        "weighted elimination weights",
    )?;
    for candidate in &candidates.candidates {
        let weight = weights
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "weighted-elimination sample".to_owned(),
            })?;
        let weight = u32::try_from(weight.bits()).map_err(|_| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination weights must be positive".to_owned(),
        })?;
        if weight == 0 {
            return Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "weighted-elimination weights must be positive".to_owned(),
            });
        }
        candidate_weights.push(weight);
    }
    let radius_squared = radius.checked_mul(radius).ok_or(Error::NumericOverflow)?;
    let mut bucket_entries = Vec::new();
    crate::memory::reserve_exact(
        &mut bucket_entries,
        candidate_count,
        "weighted elimination spatial index",
    )?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        bucket_entries.push((
            (ticks[0].div_euclid(radius), ticks[2].div_euclid(radius)),
            index,
        ));
    }
    bucket_entries.sort_unstable();
    let mut adjacency = Vec::new();
    crate::memory::reserve_exact(
        &mut adjacency,
        candidate_count,
        "weighted elimination adjacency headers",
    )?;
    for _ in 0..candidate_count {
        let mut neighbours = Vec::new();
        crate::memory::reserve_exact(
            &mut neighbours,
            maximum_neighbours,
            "weighted elimination adjacency",
        )?;
        adjacency.push(neighbours);
    }
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        let bucket = (ticks[0].div_euclid(radius), ticks[2].div_euclid(radius));
        for x in -1..=1 {
            for z in -1..=1 {
                let neighbour_bucket = (bucket.0 + x, bucket.1 + z);
                let start = bucket_entries.partition_point(|(key, _)| *key < neighbour_bucket);
                let end = bucket_entries.partition_point(|(key, _)| *key <= neighbour_bucket);
                for &(_, other_index) in bucket_entries[start..end]
                    .iter()
                    .filter(|(_, other)| *other > index)
                {
                    let other = &candidates.candidates[other_index];
                    let distance_squared = distance_squared_xz(candidate.position, other.position)?;
                    if distance_squared >= radius_squared {
                        continue;
                    }
                    let distance = integer_sqrt(distance_squared);
                    let contribution =
                        u64::try_from(radius.checked_sub(distance).ok_or(Error::NumericOverflow)?)
                            .map_err(|_| Error::NumericOverflow)?;
                    if adjacency[index].len() == maximum_neighbours
                        || adjacency[other_index].len() == maximum_neighbours
                    {
                        return Err(Error::GraphLimit {
                            resource: "weighted-elimination neighbours",
                            requested: u64::try_from(maximum_neighbours)
                                .unwrap_or(u64::MAX)
                                .saturating_add(1),
                            limit: u64::try_from(maximum_neighbours).unwrap_or(u64::MAX),
                        });
                    }
                    adjacency[index].push((other_index, contribution));
                    adjacency[other_index].push((index, contribution));
                }
            }
        }
    }
    let baseline = u64::try_from(radius).map_err(|_| Error::NumericOverflow)?;
    let mut crowding = Vec::new();
    crate::memory::reserve_exact(
        &mut crowding,
        candidate_count,
        "weighted elimination crowding",
    )?;
    for neighbours in &adjacency {
        crowding.push(
            neighbours
                .iter()
                .try_fold(baseline, |sum, (_, contribution)| {
                    sum.checked_add(*contribution).ok_or(Error::NumericOverflow)
                })?,
        );
    }
    let mut active = Vec::new();
    crate::memory::reserve_exact(
        &mut active,
        candidate_count,
        "weighted elimination active mask",
    )?;
    active.resize(candidate_count, true);
    let mut queue = IndexedEliminationQueue::with_capacity(candidate_count)?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        queue.push(EliminationScore {
            crowding: crowding[index],
            weight: candidate_weights[index],
            identity: candidate.identity,
            index,
        })?;
    }
    let mut active_count = candidates.candidates.len();
    while active_count > target {
        state.check_abort()?;
        let score = queue.pop_max().ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "weighted-elimination queue exhausted".to_owned(),
        })?;
        active[score.index] = false;
        active_count -= 1;
        for &(neighbour, contribution) in &adjacency[score.index] {
            if !active[neighbour] {
                continue;
            }
            let next_crowding = crowding[neighbour]
                .checked_sub(contribution)
                .ok_or(Error::NumericOverflow)?;
            crowding[neighbour] = next_crowding;
            queue.update(EliminationScore {
                crowding: next_crowding,
                weight: candidate_weights[neighbour],
                identity: candidates.candidates[neighbour].identity,
                index: neighbour,
            })?;
        }
    }
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        if !active[index] {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::WeightedElimination,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    let mut retained = Vec::new();
    crate::memory::reserve_exact(&mut retained, target, "weighted elimination result")?;
    retained.extend(
        candidates
            .candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| active[*index])
            .map(|(_, candidate)| candidate.clone()),
    );
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: retained,
    };
    result.canonicalize()?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EliminationScore {
    crowding: u64,
    weight: u32,
    identity: CandidateIdentity,
    index: usize,
}

impl Ord for EliminationScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (u128::from(self.crowding) * u128::from(other.weight))
            .cmp(&(u128::from(other.crowding) * u128::from(self.weight)))
            .then_with(|| self.identity.cmp(&other.identity))
            .then_with(|| self.index.cmp(&other.index))
    }
}

struct IndexedEliminationQueue {
    heap: Vec<EliminationScore>,
    positions: Vec<usize>,
}

impl IndexedEliminationQueue {
    fn with_capacity(capacity: usize) -> Result<Self> {
        let mut heap = Vec::new();
        crate::memory::reserve_exact(&mut heap, capacity, "weighted elimination queue")?;
        let mut positions = Vec::new();
        crate::memory::reserve_exact(&mut positions, capacity, "weighted elimination queue index")?;
        positions.resize(capacity, usize::MAX);
        Ok(Self { heap, positions })
    }

    fn push(&mut self, score: EliminationScore) -> Result<()> {
        if score.index >= self.positions.len() || self.positions[score.index] != usize::MAX {
            return Err(elimination_queue_error());
        }
        let position = self.heap.len();
        self.heap.push(score);
        self.positions[score.index] = position;
        self.sift_up(position);
        Ok(())
    }

    fn pop_max(&mut self) -> Option<EliminationScore> {
        let last = self.heap.len().checked_sub(1)?;
        self.swap_nodes(0, last);
        let removed = self.heap.pop()?;
        self.positions[removed.index] = usize::MAX;
        if !self.heap.is_empty() {
            self.sift_down(0);
        }
        Some(removed)
    }

    fn update(&mut self, score: EliminationScore) -> Result<()> {
        let position = *self
            .positions
            .get(score.index)
            .ok_or_else(elimination_queue_error)?;
        let previous = *self
            .heap
            .get(position)
            .ok_or_else(elimination_queue_error)?;
        self.heap[position] = score;
        if score > previous {
            self.sift_up(position);
        } else {
            self.sift_down(position);
        }
        Ok(())
    }

    fn sift_up(&mut self, mut position: usize) {
        while position > 0 {
            let parent = (position - 1) / 2;
            if self.heap[parent] >= self.heap[position] {
                break;
            }
            self.swap_nodes(parent, position);
            position = parent;
        }
    }

    fn sift_down(&mut self, mut position: usize) {
        loop {
            let left = position * 2 + 1;
            if left >= self.heap.len() {
                break;
            }
            let right = left + 1;
            let child = if right < self.heap.len() && self.heap[right] > self.heap[left] {
                right
            } else {
                left
            };
            if self.heap[position] >= self.heap[child] {
                break;
            }
            self.swap_nodes(position, child);
            position = child;
        }
    }

    fn swap_nodes(&mut self, left: usize, right: usize) {
        self.heap.swap(left, right);
        self.positions[self.heap[left].index] = left;
        self.positions[self.heap[right].index] = right;
    }
}

fn elimination_queue_error() -> Error {
    Error::GraphDocument {
        path: "weightedElimination.queue".to_owned(),
        reason: "indexed elimination queue is inconsistent".to_owned(),
    }
}

impl PartialOrd for EliminationScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn variable_spacing(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    radii: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "radius", candidates.lineage, radii.lineage)?;
    state.check_transient_memory(xz_filter_scratch_bytes(candidates.candidates.len() as u64)?)?;
    let prototype_aware = bool_parameter(node, "prototypeAware", false)?;
    let mut effective_radii = Vec::new();
    crate::memory::reserve_exact(
        &mut effective_radii,
        candidates.candidates.len(),
        "variable-spacing radii",
    )?;
    for candidate in &candidates.candidates {
        let authored = radii
            .values
            .get(&candidate.identity)
            .copied()
            .unwrap_or(candidate.crown_radius.max(candidate.root_radius));
        let radius = if prototype_aware {
            let family = candidate
                .family
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "plant family before prototype-aware spacing".to_owned(),
                })?;
            let prototype = prototype_for_family(state, family)?;
            authored.max(
                prototype
                    .crown_radius
                    .into_iter()
                    .chain(prototype.root_radius)
                    .max()
                    .unwrap(),
            )
        } else {
            authored
        };
        effective_radii.push((
            candidate.identity,
            nonnegative_radius_ticks(node, "variable-spacing radius sample", radius)?,
        ));
    }
    effective_radii.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_radius = effective_radii
        .iter()
        .map(|(_, radius)| *radius)
        .max()
        .unwrap_or(0);
    let maximum_support = if prototype_aware {
        maximum_radius
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?
    } else {
        maximum_radius
    };
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut immutable = Vec::new();
    crate::memory::reserve_exact(
        &mut immutable,
        candidates.candidates.len(),
        "variable-spacing candidates",
    )?;
    immutable.extend(candidates.candidates.iter().cloned());
    immutable.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "variable-spacing accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "variable-spacing bucket links",
    )?;
    for candidate in immutable {
        let radius_ticks = lookup_candidate_radius(
            &effective_radii,
            candidate.identity,
            node,
            "variable-spacing radius sample",
        )?;
        ensure_support_ticks(node, radius_ticks)?;
        let mut wins = true;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let mut index = bucket_heads.get(&key).copied();
                while let Some(current) = index {
                    let other = accepted.get(current).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "variable-spacing spatial index is invalid".to_owned(),
                    })?;
                    let other_radius_ticks = lookup_candidate_radius(
                        &effective_radii,
                        other.identity,
                        node,
                        "variable-spacing radius sample",
                    )?;
                    let required = if prototype_aware {
                        radius_ticks
                            .checked_add(other_radius_ticks)
                            .ok_or(Error::NumericOverflow)?
                    } else {
                        radius_ticks.max(other_radius_ticks)
                    };
                    ensure_support_ticks(node, required)?;
                    let required_squared = required
                        .checked_mul(required)
                        .ok_or(Error::NumericOverflow)?;
                    if distance_squared_xz(candidate.position, other.position)? < required_squared {
                        wins = false;
                        break 'neighbours;
                    }
                    index = *bucket_links
                        .get(current)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "variable-spacing spatial index is invalid".to_owned(),
                        })?;
                }
            }
        }
        if wins {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push(candidate);
        } else {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    };
    result.canonicalize()?;
    Ok(result)
}

fn lookup_candidate_radius(
    radii: &[(CandidateIdentity, i128)],
    identity: CandidateIdentity,
    node: &CompiledGraphNode,
    input: &'static str,
) -> Result<i128> {
    radii
        .binary_search_by_key(&identity, |(candidate, _)| *candidate)
        .ok()
        .map(|index| radii[index].1)
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: input.to_owned(),
        })
}

fn competition_claims(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    communities: &CommunityTables,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    state.check_transient_memory(competition_scratch_bytes(
        candidates.candidates.len() as u64
    )?)?;
    let crown_weight = unit_parameter(node, "crownWeight", UnitInterval::ZERO)?;
    let root_weight = unit_parameter(node, "rootWeight", UnitInterval::ZERO)?;
    if crown_weight == UnitInterval::ZERO && root_weight == UnitInterval::ZERO {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "competition crown and root weights cannot both be zero".to_owned(),
        });
    }
    let mut radii = Vec::new();
    crate::memory::reserve_exact(&mut radii, candidates.candidates.len(), "competition radii")?;
    for candidate in &candidates.candidates {
        let family = candidate
            .family
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "plant family before competition".to_owned(),
            })?;
        let prototype = prototype_for_family(state, family)?;
        let crown = prototype.crown_radius[0].max(prototype.crown_radius[1]);
        let root = prototype.root_radius[0].max(prototype.root_radius[1]);
        let crown = crown.checked_mul(DecisionScalar::from_bits(i32::from(crown_weight.bits())))?;
        let root = root.checked_mul(DecisionScalar::from_bits(i32::from(root_weight.bits())))?;
        radii.push((
            candidate.identity,
            fixed_meters_to_ticks(crown.checked_add(root)?)?.unsigned_abs() as i128,
        ));
    }
    radii.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_radius = radii.iter().map(|(_, radius)| *radius).max().unwrap_or(0);
    let maximum_pair_spacing =
        communities
            .competition
            .iter()
            .try_fold(0_i128, |maximum, rule| {
                Ok::<_, Error>(
                    maximum.max(fixed_meters_to_ticks(rule.spacing)?.unsigned_abs() as i128),
                )
            })?;
    let maximum_support = maximum_radius
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?
        .max(maximum_pair_spacing);
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut buckets = Vec::<((i128, i128), usize)>::new();
    crate::memory::reserve_exact(
        &mut buckets,
        candidates.candidates.len(),
        "competition spatial index",
    )?;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        buckets.push((xz_bucket(candidate.position, bucket_size), index));
    }
    buckets.sort_unstable();
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "competition accepted candidates",
    )?;
    for candidate in &candidates.candidates {
        let candidate_radius =
            lookup_candidate_radius(&radii, candidate.identity, node, "competition radius")?;
        let mut loses = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let start = buckets.partition_point(|(bucket, _)| *bucket < key);
                let end = buckets.partition_point(|(bucket, _)| *bucket <= key);
                for (_, index) in &buckets[start..end] {
                    let other =
                        candidates
                            .candidates
                            .get(*index)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "competition spatial index is invalid".to_owned(),
                            })?;
                    if candidate.identity == other.identity {
                        continue;
                    }
                    let other_radius = lookup_candidate_radius(
                        &radii,
                        other.identity,
                        node,
                        "competition radius",
                    )?;
                    let mut required = candidate_radius
                        .checked_add(other_radius)
                        .ok_or(Error::NumericOverflow)?;
                    let pair_rule = match (candidate.family, other.family) {
                        (Some(candidate_family), Some(other_family)) => communities
                            .competition
                            .iter()
                            .filter_map(|rule| {
                                if rule.first == candidate_family && rule.second == other_family {
                                    Some((rule.spacing, rule.priority))
                                } else if rule.first == other_family
                                    && rule.second == candidate_family
                                {
                                    Some((rule.spacing, rule.priority.checked_neg()?))
                                } else {
                                    None
                                }
                            })
                            .max_by_key(|(spacing, priority)| (*spacing, *priority)),
                        _ => None,
                    };
                    if let Some((spacing, _)) = pair_rule {
                        required =
                            required.max(fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128);
                    }
                    ensure_support_ticks(node, required)?;
                    if distance_squared_xz(candidate.position, other.position)?
                        >= required
                            .checked_mul(required)
                            .ok_or(Error::NumericOverflow)?
                    {
                        continue;
                    }
                    let oriented_priority = pair_rule.map_or(0, |(_, priority)| priority);
                    let other_wins = oriented_priority < 0
                        || (oriented_priority == 0
                            && (other.priority > candidate.priority
                                || (other.priority == candidate.priority
                                    && other.identity < candidate.identity)));
                    if other_wins {
                        loses = true;
                        break 'neighbours;
                    }
                }
            }
        }
        if loses {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            accepted.push(candidate.clone());
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    };
    result.canonicalize()?;
    Ok(result)
}

fn threshold_candidates(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let threshold = i32::from(unit_parameter(node, "threshold", UnitInterval::ZERO)?.bits());
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "threshold retained candidates",
    )?;
    for candidate in &candidates.candidates {
        if weights
            .values
            .get(&candidate.identity)
            .is_some_and(|value| value.bits() >= threshold)
        {
            accepted.push(candidate.clone());
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Threshold,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

fn suitability_candidates(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    unit: &CompiledGraphUnit,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    let binding = unit
        .suitability
        .iter()
        .find(|binding| binding.node_guid == node.definition.guid)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "suitability node requires one matching biome suitability binding".to_owned(),
        })?;
    let threshold = unit_parameter(node, "threshold", UnitInterval::ZERO)?;
    let mut accepted = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "suitability retained candidates",
    )?;
    for candidate in &candidates.candidates {
        let value = weights
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!("{:?} suitability sample", binding.channel),
            })?;
        if suitability_score(value, *binding)? >= threshold {
            accepted.push(candidate.clone());
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::Threshold,
                candidate.family,
                candidate.variation,
                state,
            )?;
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

fn suitability_score(
    value: DecisionScalar,
    binding: crate::SuitabilityBinding,
) -> Result<UnitInterval> {
    if value >= binding.minimum && value <= binding.maximum {
        return Ok(UnitInterval::ONE);
    }
    if binding.falloff.bits() <= 0 {
        return Ok(UnitInterval::ZERO);
    }
    let distance = if value < binding.minimum {
        binding.minimum.checked_sub(value)?
    } else {
        value.checked_sub(binding.maximum)?
    };
    if distance >= binding.falloff {
        return Ok(UnitInterval::ZERO);
    }
    let remaining = binding.falloff.checked_sub(distance)?;
    let bits = div_round_ties_even(
        i128::from(remaining.bits()) * i128::from(u16::MAX),
        i128::from(binding.falloff.bits()),
    )?;
    Ok(UnitInterval::from_bits(
        u16::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn expand_cluster(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let children = u64::from(u32_parameter(node, "children", 0)?);
    let radius = fixed_parameter(node, "radius", DecisionScalar::from_bits(0))?;
    let radius_ticks = fixed_meters_to_ticks(radius)?.unsigned_abs() as i128;
    let mode = cluster_mode_parameter(node, "mode")?;
    let maximum_output = candidates
        .candidates
        .len()
        .checked_mul(
            usize::try_from(children)
                .map_err(|_| Error::NumericOverflow)?
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "candidate count",
        u64::try_from(maximum_output).map_err(|_| Error::NumericOverflow)?,
        state.graph.limits.max_candidates,
    )?;
    let mut expanded = Vec::new();
    crate::memory::reserve_exact(&mut expanded, maximum_output, "expanded cluster candidates")?;
    expanded.extend(candidates.candidates.iter().cloned());
    for parent in &candidates.candidates {
        for child in 0..children {
            let ordinal =
                candidate_ordinal(node, state, candidate_key_u128(parent.identity), child, 3)?;
            let stream = random_stream(
                node,
                state,
                "cluster",
                RandomSampleAddress::new(parent.owner, ordinal)
                    .with_ancestor(parent.identity.ordinal)
                    .with_species(parent.family.map_or(0, |family| u128::from(family.value()))),
            )?;
            let position = annulus_position(parent.position, 0, radius_ticks, stream)?;
            if !state.inputs.read_bounds.contains(position) {
                continue;
            }
            let mut candidate = parent.clone();
            candidate.identity = CandidateIdentity {
                node: node.definition.guid,
                node_address: node_execution_address(node, state),
                node_semantic_revision: node.definition.semantic_revision,
                ordinal,
                ancestor: parent.identity.ordinal,
            };
            candidate.position = position;
            candidate.owner = canonical_owner(position, node.definition.spatial.level())?;
            match mode {
                GraphClusterMode::Cluster => {
                    candidate.parent = Some(CandidateReference::from_candidate(parent));
                    candidate.colony = parent.colony;
                }
                GraphClusterMode::Patch => {
                    candidate.parent = None;
                    candidate.colony = None;
                }
                GraphClusterMode::Colony => {
                    candidate.parent = Some(CandidateReference::from_candidate(parent));
                    candidate.colony = parent
                        .colony
                        .or(Some(CandidateReference::from_candidate(parent)));
                }
            }
            candidate.authored_point = None;
            expanded.push(candidate);
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: expanded,
    };
    result.canonicalize()?;
    Ok(result)
}

fn expand_companions(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    unit: &CompiledGraphUnit,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let children = u64::from(u32_parameter(node, "children", 0)?);
    let maximum_depth = u64::from(u32_parameter(node, "maximumDepth", 0)?);
    let fallback_radius = fixed_meters_to_ticks(fixed_parameter(
        node,
        "radius",
        DecisionScalar::from_bits(0),
    )?)?
    .unsigned_abs() as i128;
    let mut rules: Vec<&crate::CompanionRule> = Vec::new();
    crate::memory::reserve_exact(&mut rules, unit.companions.len(), "companion rules")?;
    rules.extend(&unit.companions);
    rules.sort_unstable_by_key(|rule| {
        (
            rule.parent.value(),
            rule.child.value(),
            rule.minimum_distance,
            rule.maximum_distance,
            rule.probability,
        )
    });
    let mut maximum_output = candidates.candidates.len() as u64;
    let mut generation = candidates.candidates.len() as u64;
    for _ in 0..maximum_depth {
        generation = generation
            .checked_mul(children)
            .ok_or(Error::NumericOverflow)?;
        maximum_output = maximum_output
            .checked_add(generation)
            .ok_or(Error::NumericOverflow)?;
    }
    state.check_count(
        "candidate count",
        maximum_output,
        state.graph.limits.max_candidates,
    )?;
    state.check_transient_memory(companion_scratch_bytes(
        candidates.candidates.len() as u64,
        maximum_output,
        unit.companions.len() as u64,
    )?)?;
    let maximum_output = usize::try_from(maximum_output).map_err(|_| Error::NumericOverflow)?;
    let mut expanded = Vec::new();
    crate::memory::reserve_exact(&mut expanded, maximum_output, "expanded companions")?;
    expanded.extend(candidates.candidates.iter().cloned());
    let mut frontier = Vec::new();
    crate::memory::reserve_exact(
        &mut frontier,
        candidates.candidates.len(),
        "companion frontier",
    )?;
    frontier.extend(candidates.candidates.iter().cloned());
    for depth in 1..=maximum_depth {
        let mut next = Vec::new();
        let next_capacity = frontier
            .len()
            .checked_mul(usize::try_from(children).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)?;
        crate::memory::reserve_exact(&mut next, next_capacity, "companion frontier")?;
        for parent in &frontier {
            let eligible_count = rules
                .iter()
                .filter(|rule| parent.family.is_some_and(|family| rule.parent == family))
                .count();
            for child in 0..children {
                let ordinal = candidate_ordinal(
                    node,
                    state,
                    candidate_key_u128(parent.identity),
                    child,
                    u32::try_from(depth).map_err(|_| Error::NumericOverflow)?,
                )?;
                let stream = random_stream(
                    node,
                    state,
                    "companions",
                    RandomSampleAddress::new(parent.owner, ordinal)
                        .with_ancestor(parent.identity.ordinal)
                        .with_species(parent.family.map_or(0, |family| u128::from(family.value())))
                        .with_channel(u32::try_from(depth).map_err(|_| Error::NumericOverflow)?),
                )?;
                let rule = if eligible_count == 0 {
                    None
                } else {
                    let selected =
                        usize::try_from(u64::from(stream.lane(0, 0)) % eligible_count as u64)
                            .map_err(|_| Error::NumericOverflow)?;
                    rules
                        .iter()
                        .filter(|rule| parent.family.is_some_and(|family| rule.parent == family))
                        .nth(selected)
                        .copied()
                };
                if rule.is_some_and(|rule| !stream.chance(0, 1, rule.probability)) {
                    continue;
                }
                let (minimum, maximum) = if let Some(rule) = rule {
                    (
                        fixed_meters_to_ticks(rule.minimum_distance)?.unsigned_abs() as i128,
                        fixed_meters_to_ticks(rule.maximum_distance)?.unsigned_abs() as i128,
                    )
                } else {
                    (0, fallback_radius)
                };
                if minimum > maximum {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "companion distance range is inverted".to_owned(),
                    });
                }
                let position = annulus_position(parent.position, minimum, maximum, stream)?;
                if !state.inputs.read_bounds.contains(position) {
                    continue;
                }
                let mut candidate = parent.clone();
                candidate.identity = CandidateIdentity {
                    node: node.definition.guid,
                    node_address: node_execution_address(node, state),
                    node_semantic_revision: node.definition.semantic_revision,
                    ordinal,
                    ancestor: parent.identity.ordinal,
                };
                candidate.position = position;
                candidate.owner = canonical_owner(position, node.definition.spatial.level())?;
                candidate.family = rule.map(|rule| rule.child).or(parent.family);
                candidate.parent = Some(CandidateReference::from_candidate(parent));
                candidate.colony = parent
                    .colony
                    .or(Some(CandidateReference::from_candidate(parent)));
                candidate.authored_point = None;
                next.push(candidate);
            }
        }
        next.sort_unstable_by_key(|candidate| candidate.identity);
        expanded.extend(next.iter().cloned());
        frontier = next;
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates: expanded,
    };
    result.canonicalize()?;
    Ok(result)
}

fn follow_splines(
    node: &CompiledGraphNode,
    splines: &[EvaluationSpline],
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let spacing = fixed_parameter(node, "spacing", DecisionScalar::from_bits(0))?;
    let spacing_ticks = fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128;
    if spacing_ticks == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline spacing must be positive".to_owned(),
        });
    }
    let edge_offset = fixed_meters_to_ticks(fixed_parameter(
        node,
        "edgeOffset",
        DecisionScalar::from_bits(0),
    )?)?;
    let region_upper = if state.inputs.regions.is_empty() {
        cell_region_count(state.inputs.read_bounds, state.inputs.output_cell.level())? as u64
    } else {
        state.inputs.regions.len() as u64
    };
    state.check_transient_memory(stage_region_scratch_bytes(region_upper)?)?;
    let stage_regions = stage_regions(node, &state.inputs.regions, state)?;
    if splines.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline identities must be unique".to_owned(),
        });
    }
    let mut maximum_candidates = 0_usize;
    for spline in splines {
        let segments = spline_segments(&spline.points)?;
        let total_length = segments.iter().try_fold(0_i128, |total, segment| {
            total
                .checked_add(segment.length)
                .ok_or(Error::NumericOverflow)
        })?;
        maximum_candidates = maximum_candidates
            .checked_add(
                usize::try_from(total_length / spacing_ticks)
                    .map_err(|_| Error::NumericOverflow)?
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?,
            )
            .ok_or(Error::NumericOverflow)?;
    }
    state.check_count(
        "candidate count",
        maximum_candidates as u64,
        state.graph.limits.max_candidates,
    )?;
    let spline_points = splines.iter().try_fold(0_u64, |total, spline| {
        total
            .checked_add(spline.points.len() as u64)
            .ok_or(Error::NumericOverflow)
    })?;
    state.check_transient_memory(checked_memory_sum([
        requested_vec_bound::<EvaluationRegion>(stage_regions.len() as u64)?,
        requested_vec_bound::<[i128; 3]>(spline_points)?,
        requested_vec_bound::<SplineSegment>(spline_points)?,
        requested_vec_bound::<[i128; 3]>(maximum_candidates as u64)?,
        requested_vec_bound::<GraphCandidate>(maximum_candidates as u64)?,
    ])?)?;
    let mut candidates = Vec::new();
    crate::memory::reserve_exact(
        &mut candidates,
        maximum_candidates,
        "spline-follow candidates",
    )?;
    for spline in splines {
        let segments = spline_segments(&spline.points)?;
        for (sample_index, ticks) in sample_spline_segments(&segments, spacing_ticks, edge_offset)?
            .into_iter()
            .enumerate()
        {
            let position = WorldPosition::from_global_ticks(ticks)?;
            if stage_regions
                .iter()
                .any(|region| region.bounds.contains(position))
            {
                let ordinal = candidate_ordinal(
                    node,
                    state,
                    spline.id,
                    u64::try_from(sample_index).map_err(|_| Error::NumericOverflow)?,
                    2,
                )?;
                let mut candidate = default_candidate(node, state, ordinal, 0, position)?;
                candidate.source_layer = spline.layer;
                candidates.push(candidate);
            }
        }
    }
    let mut result = CandidateStream {
        lineage: candidate_lineage(node, state),
        candidates,
    };
    result.canonicalize()?;
    Ok(result)
}

fn sample_spline_segments(
    segments: &[SplineSegment],
    spacing_ticks: i128,
    edge_offset: i128,
) -> Result<Vec<[i128; 3]>> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    let total_length = segments.iter().try_fold(0_i128, |total, segment| {
        total
            .checked_add(segment.length)
            .ok_or(Error::NumericOverflow)
    })?;
    let mut segment_index = 0_usize;
    let mut segment_start_distance = 0_i128;
    let sample_count = total_length / spacing_ticks;
    let sample_capacity = usize::try_from(sample_count)
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or(Error::NumericOverflow)?;
    let mut samples = Vec::new();
    crate::memory::reserve_exact(&mut samples, sample_capacity, "spline samples")?;
    for sample_index in 0..=sample_count {
        let distance = sample_index
            .checked_mul(spacing_ticks)
            .ok_or(Error::NumericOverflow)?;
        while segment_index + 1 < segments.len()
            && distance
                >= segment_start_distance
                    .checked_add(segments[segment_index].length)
                    .ok_or(Error::NumericOverflow)?
        {
            segment_start_distance = segment_start_distance
                .checked_add(segments[segment_index].length)
                .ok_or(Error::NumericOverflow)?;
            segment_index += 1;
        }
        let segment = &segments[segment_index];
        let local_distance = distance
            .checked_sub(segment_start_distance)
            .ok_or(Error::NumericOverflow)?;
        let mut ticks = [0_i128; 3];
        for (axis, tick) in ticks.iter_mut().enumerate() {
            let along = div_round_ties_even(
                segment.direction[axis]
                    .checked_mul(local_distance)
                    .ok_or(Error::NumericOverflow)?,
                segment.length,
            )?;
            let lateral = div_round_ties_even(
                segment.lateral[axis]
                    .checked_mul(edge_offset)
                    .ok_or(Error::NumericOverflow)?,
                segment.lateral_length,
            )?;
            *tick = segment.start[axis]
                .checked_add(along)
                .and_then(|value| value.checked_add(lateral))
                .ok_or(Error::NumericOverflow)?;
        }
        samples.push(ticks);
    }
    Ok(samples)
}

#[derive(Clone, Copy)]
struct SplineSegment {
    start: [i128; 3],
    direction: [i128; 3],
    length: i128,
    lateral: [i128; 3],
    lateral_length: i128,
}

fn spline_segments(points: &[WorldPosition]) -> Result<Vec<SplineSegment>> {
    let mut canonical = Vec::<[i128; 3]>::new();
    crate::memory::reserve_exact(&mut canonical, points.len(), "canonical spline points")?;
    for point in points {
        let point = point.global_ticks();
        if canonical.last() == Some(&point) {
            continue;
        }
        while canonical.len() >= 2 {
            let previous = canonical[canonical.len() - 2];
            let current = canonical[canonical.len() - 1];
            let incoming = subtract_position(current, previous)?;
            let outgoing = subtract_position(point, current)?;
            if primitive_direction(incoming)? != primitive_direction(outgoing)? {
                break;
            }
            canonical.pop();
        }
        canonical.push(point);
    }

    let mut segments = Vec::new();
    crate::memory::reserve_exact(
        &mut segments,
        canonical.len().saturating_sub(1),
        "spline segments",
    )?;
    let mut previous_lateral = None;
    for points in canonical.windows(2) {
        let start = points[0];
        let end = points[1];
        let direction = subtract_position(end, start)?;
        let length = integer_sqrt(distance_squared(start, end)?);
        if length == 0 {
            continue;
        }
        let mut lateral = if direction[0] == 0 && direction[2] == 0 {
            [
                direction[1].checked_neg().ok_or(Error::NumericOverflow)?,
                direction[0],
                0,
            ]
        } else {
            [
                direction[2],
                0,
                direction[0].checked_neg().ok_or(Error::NumericOverflow)?,
            ]
        };
        if let Some(previous) = previous_lateral
            && vector_dot(previous, lateral)? < 0
        {
            for value in &mut lateral {
                *value = value.checked_neg().ok_or(Error::NumericOverflow)?;
            }
        }
        let lateral_length = integer_sqrt(vector_length_squared(lateral)?);
        if lateral_length == 0 {
            return Err(Error::NumericOverflow);
        }
        previous_lateral = Some(lateral);
        segments.push(SplineSegment {
            start,
            direction,
            length,
            lateral,
            lateral_length,
        });
    }
    Ok(segments)
}

fn subtract_position(left: [i128; 3], right: [i128; 3]) -> Result<[i128; 3]> {
    let mut result = [0_i128; 3];
    for axis in 0..3 {
        result[axis] = left[axis]
            .checked_sub(right[axis])
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(result)
}

fn primitive_direction(direction: [i128; 3]) -> Result<[i128; 3]> {
    let divisor = direction
        .iter()
        .map(|value| value.unsigned_abs())
        .fold(0_u128, greatest_common_divisor);
    if divisor == 0 {
        return Ok([0; 3]);
    }
    let mut primitive = [0_i128; 3];
    for axis in 0..3 {
        let magnitude = direction[axis].unsigned_abs() / divisor;
        let magnitude = i128::try_from(magnitude).map_err(|_| Error::NumericOverflow)?;
        primitive[axis] = if direction[axis] < 0 {
            magnitude.checked_neg().ok_or(Error::NumericOverflow)?
        } else {
            magnitude
        };
    }
    Ok(primitive)
}

fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn vector_dot(left: [i128; 3], right: [i128; 3]) -> Result<i128> {
    (0..3).try_fold(0_i128, |sum, axis| {
        sum.checked_add(
            left[axis]
                .checked_mul(right[axis])
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)
    })
}

fn vector_length_squared(vector: [i128; 3]) -> Result<i128> {
    vector_dot(vector, vector)
}

fn transform_candidates(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    state: &EvaluationState<'_>,
) -> Result<CandidateStream> {
    let candidates = candidates_input(inputs, "candidates")?;
    let surface = optional_surface_input(inputs, "surface")?;
    let scales = optional_scalar_input(inputs, "scale")?;
    let offsets = optional_vector_input(inputs, "offset")?;
    let orient_to_surface = bool_parameter(node, "orientToSurface", false)?;
    let yaw_minimum = unit_parameter(node, "yawMinimum", UnitInterval::ZERO)?;
    let yaw_maximum = unit_parameter(node, "yawMaximum", UnitInterval::ONE)?;
    let scale_minimum = fixed_parameter(node, "scaleMinimum", DecisionScalar::from_bits(65_536))?;
    let scale_maximum = fixed_parameter(node, "scaleMaximum", DecisionScalar::from_bits(65_536))?;
    let variation_count = u32_parameter(node, "variationCount", 1)?.max(1);
    if yaw_minimum > yaw_maximum || scale_minimum.bits() <= 0 || scale_minimum > scale_maximum {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "transform yaw/scale range is invalid".to_owned(),
        });
    }
    for (name, lineage) in [
        ("surface", surface.map(|value| value.lineage)),
        ("scale", scales.map(|value| value.lineage)),
        ("offset", offsets.map(|value| value.lineage)),
    ] {
        if let Some(lineage) = lineage {
            ensure_lineage(node, name, candidates.lineage, lineage)?;
        }
    }
    let mut transformed = Vec::new();
    crate::memory::reserve_exact(
        &mut transformed,
        candidates.candidates.len(),
        "transformed candidates",
    )?;
    for candidate in &candidates.candidates {
        let mut candidate = candidate.clone();
        if let Some(projected) = surface.and_then(|surface| surface.values.get(&candidate.identity))
        {
            candidate.position = projected.position;
            candidate.owner = canonical_owner(projected.position, node.definition.spatial.level())?;
            candidate.attachment = projected.attachment;
            candidate.surface_normal = Some(projected.normal);
            candidate.surface_projection = projected.projection;
        }
        if let Some(offset) = offsets.and_then(|field| field.values.get(&candidate.identity)) {
            let [offset_x, offset_y, offset_z] =
                [offset.x, offset.y, offset.z].map(fixed_meters_to_ticks);
            let offset_ticks = [offset_x?, offset_y?, offset_z?];
            let offset_length = integer_sqrt(distance_squared(
                [0; 3],
                [offset_ticks[0], offset_ticks[1], offset_ticks[2]],
            )?);
            ensure_support_ticks(node, offset_length)?;
            candidate.position = offset_fixed(candidate.position, [offset.x, offset.y, offset.z])?;
            candidate.owner = canonical_owner(candidate.position, node.definition.spatial.level())?;
        }
        let stream = random_stream(
            node,
            state,
            "variation",
            RandomSampleAddress::new(candidate.owner, candidate.identity.ordinal)
                .with_ancestor(candidate.identity.ancestor)
                .with_species(
                    candidate
                        .family
                        .map_or(0, |family| u128::from(family.value())),
                )
                .with_channel(2),
        )?;
        let yaw = interpolate_unit(yaw_minimum, yaw_maximum, stream.unit(0, 0))?;
        let random_scale = scale_minimum.lerp(scale_maximum, stream.unit(0, 1))?;
        let sampled_scale = scales
            .and_then(|field| field.values.get(&candidate.identity))
            .copied()
            .unwrap_or(DecisionScalar::from_bits(65_536));
        let scale = random_scale.checked_mul(sampled_scale)?;
        candidate.scale = [scale; 3];
        candidate.variation = stream.lane(0, 2) % variation_count;
        candidate.orientation = if orient_to_surface {
            orientation_from_normal_and_yaw(candidate.surface_normal, yaw)?
        } else {
            yaw_orientation(yaw)?
        };
        transformed.push(candidate);
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: transformed,
    };
    result.canonicalize()?;
    Ok(result)
}

fn priority_exclusion(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    weights: &ScalarFieldSamples,
    radii: &ScalarFieldSamples,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    ensure_lineage(node, "weights", candidates.lineage, weights.lineage)?;
    ensure_lineage(node, "radius", candidates.lineage, radii.lineage)?;
    state.check_transient_memory(xz_filter_scratch_bytes(candidates.candidates.len() as u64)?)?;
    let keep_highest = bool_parameter(node, "keepHighest", true)?;
    let mut transformed = Vec::new();
    crate::memory::reserve_exact(
        &mut transformed,
        candidates.candidates.len(),
        "priority-exclusion candidates",
    )?;
    transformed.extend(candidates.candidates.iter().cloned());
    for candidate in &mut transformed {
        if let Some(weight) = weights.values.get(&candidate.identity) {
            candidate.priority = if keep_highest {
                *weight
            } else {
                DecisionScalar::from_bits(
                    weight.bits().checked_neg().ok_or(Error::NumericOverflow)?,
                )
            };
        }
    }
    let mut radius_ticks = Vec::new();
    crate::memory::reserve_exact(
        &mut radius_ticks,
        candidates.candidates.len(),
        "priority-exclusion radii",
    )?;
    for candidate in &candidates.candidates {
        let radius = radii
            .values
            .get(&candidate.identity)
            .copied()
            .ok_or_else(|| Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "priority-exclusion radius sample".to_owned(),
            })?;
        radius_ticks.push((
            candidate.identity,
            nonnegative_radius_ticks(node, "priority-exclusion radius sample", radius)?,
        ));
    }
    radius_ticks.sort_unstable_by_key(|(identity, _)| *identity);
    let maximum_support = radius_ticks
        .iter()
        .map(|(_, radius)| *radius)
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    transformed.sort_unstable_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "priority-exclusion accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "priority-exclusion bucket links",
    )?;
    for candidate in transformed {
        let radius = lookup_candidate_radius(
            &radius_ticks,
            candidate.identity,
            node,
            "priority-exclusion radius sample",
        )?;
        let mut excluded = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                let mut index = bucket_heads.get(&key).copied();
                while let Some(current) = index {
                    let other = accepted.get(current).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "priority-exclusion spatial index is invalid".to_owned(),
                    })?;
                    let other_radius = lookup_candidate_radius(
                        &radius_ticks,
                        other.identity,
                        node,
                        "priority-exclusion radius sample",
                    )?;
                    let required = radius
                        .checked_add(other_radius)
                        .ok_or(Error::NumericOverflow)?;
                    ensure_support_ticks(node, required)?;
                    if distance_squared_xz(candidate.position, other.position)?
                        < required
                            .checked_mul(required)
                            .ok_or(Error::NumericOverflow)?
                    {
                        excluded = true;
                        break 'neighbours;
                    }
                    index = *bucket_links
                        .get(current)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "priority-exclusion spatial index is invalid".to_owned(),
                        })?;
                }
            }
        }
        if excluded {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::PriorityExclusion,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push(candidate);
        }
    }
    Ok(CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    })
}

fn nonnegative_radius_ticks(
    node: &CompiledGraphNode,
    input: &'static str,
    radius: DecisionScalar,
) -> Result<i128> {
    if radius.bits() < 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!("{input} must be nonnegative"),
        });
    }
    fixed_meters_to_ticks(radius)
}

fn bounds_overlap(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    state.check_transient_memory(bounds_overlap_scratch_bytes(
        candidates.candidates.len() as u64
    )?)?;
    let padding = fixed_parameter(node, "padding", DecisionScalar::from_bits(0))?;
    let padding_ticks = fixed_meters_to_ticks(padding)?.unsigned_abs() as i128;
    let mut ordered = Vec::new();
    crate::memory::reserve_exact(
        &mut ordered,
        candidates.candidates.len(),
        "bounds-overlap candidates",
    )?;
    for candidate in candidates.candidates.iter().cloned() {
        let bounds = expand_bounds_checked(candidate_bounds(&candidate, state)?, padding_ticks)?;
        let radius = bounds_support_radius(candidate.position, bounds)?;
        ensure_support_ticks(node, radius)?;
        ordered.push((candidate, bounds, radius));
    }
    let maximum_support = ordered
        .iter()
        .map(|(_, _, radius)| *radius)
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    ordered.sort_unstable_by(|left, right| {
        right
            .0
            .priority
            .cmp(&left.0.priority)
            .then_with(|| left.0.identity.cmp(&right.0.identity))
    });
    let mut accepted: Vec<(GraphCandidate, WorldBounds, i128)> = Vec::new();
    crate::memory::reserve_exact(
        &mut accepted,
        candidates.candidates.len(),
        "bounds-overlap accepted candidates",
    )?;
    let mut bucket_heads = BTreeMap::<(i128, i128, i128), usize>::new();
    let mut bucket_links = Vec::<Option<usize>>::new();
    crate::memory::reserve_exact(
        &mut bucket_links,
        candidates.candidates.len(),
        "bounds-overlap bucket links",
    )?;
    for (candidate, bounds, radius) in ordered {
        let mut overlaps = false;
        let bucket = xyz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let key = offset_xyz_bucket(bucket, x, y, z)?;
                    let mut index = bucket_heads.get(&key).copied();
                    while let Some(current) = index {
                        let (_, other_bounds, other_radius) =
                            accepted.get(current).ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "bounds-overlap spatial index is invalid".to_owned(),
                            })?;
                        let total = radius
                            .checked_add(*other_radius)
                            .ok_or(Error::NumericOverflow)?;
                        ensure_support_ticks(node, total)?;
                        if bounds_intersect(bounds, *other_bounds) {
                            overlaps = true;
                            break 'neighbours;
                        }
                        index = *bucket_links
                            .get(current)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: "bounds-overlap spatial index is invalid".to_owned(),
                            })?;
                    }
                }
            }
        }
        if overlaps {
            reject_candidate(
                node,
                &candidate,
                candidates.lineage,
                CandidateRejectionReason::Competition,
                candidate.family,
                candidate.variation,
                state,
            )?;
        } else {
            let index = accepted.len();
            bucket_links.push(bucket_heads.insert(bucket, index));
            accepted.push((candidate, bounds, radius));
        }
    }
    let mut retained = Vec::new();
    crate::memory::reserve_exact(
        &mut retained,
        accepted.len(),
        "bounds-overlap retained candidates",
    )?;
    retained.extend(accepted.into_iter().map(|(candidate, _, _)| candidate));
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: retained,
    };
    result.canonicalize()?;
    Ok(result)
}

fn community_blend(
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

fn succession_input(
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

fn diagnostic_output(
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

fn macro_output(
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
                PlantId::procedural(ProceduralPlantIdentity {
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
            interaction_policy: authored.map_or(InteractionPolicy::Decorative, |point| {
                point.interaction_policy
            }),
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

fn micro_output(
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

impl EvaluationState<'_> {
    fn check_transient_memory(&self, additional_bytes: u64) -> Result<()> {
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

    fn should_load_global_node(&self, address: &GraphNodeAddress) -> bool {
        let Some(owner) = self.graph.spatial_plan().global_stage_for_node(address) else {
            return false;
        };
        self.scope
            .current_global_stage()
            .is_none_or(|current| current.id != owner.id)
    }

    fn load_global_node_outputs(
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

    fn capture_global_outputs(
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

    fn capture_resident_global_outputs(
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

    fn check_abort(&self) -> Result<()> {
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

    fn check_count(&self, resource: &'static str, requested: u64, limit: u64) -> Result<()> {
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

fn clip_global_value(mut value: GraphValue, solve_bounds: WorldBounds) -> Result<GraphValue> {
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

fn import_global_value(
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

fn graph_value_candidate_identity_upper_bound(value: &GraphValue) -> Result<u64> {
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
    streams: u64,
    candidates: u64,
    fields: u64,
    rejected: u64,
    module_path_items: u64,
    label_bytes: u64,
}

impl DiagnosticMergeShape {
    fn symbolic(value: SymbolicValueBound) -> Self {
        Self {
            streams: value.items,
            candidates: value.diagnostic_candidates,
            fields: value.diagnostic_fields,
            rejected: value.diagnostic_rejected,
            module_path_items: value.diagnostic_module_path_items,
            label_bytes: value.diagnostic_label_bytes,
        }
    }

    fn actual(streams: &[NamedDiagnosticStream]) -> Result<Self> {
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

    fn checked_add(self, other: Self) -> Result<Self> {
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
            requested_btree_bound::<DiagnosticStreamMergeKey, NamedDiagnosticStream>(self.streams)?,
            disjoint_vec_bound::<u128>(self.module_path_items, self.streams)?,
            disjoint_vec_bound::<u8>(self.label_bytes, self.streams)?,
            requested_vec_bound::<NamedDiagnosticStream>(self.streams)?,
            requested_btree_bound::<CandidateIdentity, DiagnosticCandidateSample>(self.candidates)?,
            disjoint_vec_bound::<DiagnosticCandidateSample>(self.candidates, self.streams)?,
            requested_btree_bound::<CandidateIdentity, DiagnosticScalarSample>(self.fields)?,
            disjoint_vec_bound::<DiagnosticScalarSample>(self.fields, self.streams)?,
            disjoint_vec_bound::<RejectedCandidate>(self.rejected, self.streams)?,
        ])
    }
}

fn disjoint_vec_bound<T>(items: u64, allocations: u64) -> Result<u64> {
    items
        .checked_mul(std::mem::size_of::<T>() as u64)
        .and_then(|bytes| {
            allocations
                .checked_mul(ALLOCATION_OVERHEAD_BYTES)
                .and_then(|overhead| bytes.checked_add(overhead))
        })
        .ok_or(Error::NumericOverflow)
}

fn symbolic_global_merge_scratch_bytes(
    left: SymbolicValueBound,
    right: SymbolicValueBound,
) -> Result<u64> {
    let items = left
        .items
        .checked_add(right.items)
        .ok_or(Error::NumericOverflow)?;
    match left.domain {
        Some(GraphDomain::Candidates) => checked_memory_sum([
            requested_btree_bound::<CandidateIdentity, GraphCandidate>(items)?,
            requested_vec_bound::<GraphCandidate>(items)?,
        ]),
        Some(GraphDomain::MacroPoints) => checked_memory_sum([
            requested_btree_bound::<PlantId, PlantPoint>(items)?,
            requested_vec_bound::<PlantPoint>(items)?,
        ]),
        Some(GraphDomain::MicroField) => checked_memory_sum([
            requested_btree_bound::<(WorldCellKey, u64), MicroFieldTile>(items)?,
            requested_vec_bound::<MicroFieldTile>(items)?,
        ]),
        Some(GraphDomain::Regions) => checked_memory_sum([
            requested_btree_bound::<(u128, WorldCellKey), EvaluationRegion>(items)?,
            requested_vec_bound::<EvaluationRegion>(items)?,
        ]),
        Some(GraphDomain::Splines) => checked_memory_sum([
            requested_btree_bound::<u128, EvaluationSpline>(items)?,
            requested_vec_bound::<EvaluationSpline>(items)?,
        ]),
        Some(GraphDomain::Diagnostics) => DiagnosticMergeShape::symbolic(left)
            .checked_add(DiagnosticMergeShape::symbolic(right))?
            .scratch_bytes(),
        _ => Ok(0),
    }
}

fn graph_value_merge_scratch_bytes(left: &GraphValue, right: &GraphValue) -> Result<u64> {
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

fn merge_graph_value(
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

fn hex_hash(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn reject_candidate(
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

fn select_species(
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

fn species_seed_namespace(
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

fn resolve_candidate_reference(
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
    Ok(Some(PlantId::procedural(ProceduralPlantIdentity {
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

fn prototype_for_family<'a>(
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

fn random_stream(
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

fn seed_namespace(node: &CompiledGraphNode, name: &str) -> Result<u128> {
    node.definition
        .seed_namespaces
        .get(name)
        .copied()
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: format!("stochastic operation has no '{name}' seed namespace"),
        })
}

fn default_candidate(
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

fn point_radius(point: &PlantPoint) -> Result<DecisionScalar> {
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

fn canonical_owner(position: WorldPosition, level: u8) -> Result<WorldCellKey> {
    position.cell().ancestor(level).map_err(Into::into)
}

fn cell_region_count(bounds: WorldBounds, level: u8) -> Result<usize> {
    let edge = i128::from(BASE_CELL_TICKS)
        .checked_mul(
            1_i128
                .checked_shl(u32::from(level))
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let minimum = bounds.min_ticks().map(|value| value.div_euclid(edge));
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| (value - 1).div_euclid(edge));
    minimum
        .into_iter()
        .zip(maximum)
        .try_fold(1_i128, |count, (minimum, maximum)| {
            count
                .checked_mul(
                    maximum
                        .checked_sub(minimum)
                        .and_then(|extent| extent.checked_add(1))
                        .ok_or(Error::NumericOverflow)?,
                )
                .ok_or(Error::NumericOverflow)
        })?
        .try_into()
        .map_err(|_| Error::NumericOverflow)
}

fn canonical_cell_regions(
    bounds: WorldBounds,
    level: u8,
    hierarchy_namespace: u128,
) -> Result<Vec<EvaluationRegion>> {
    let edge = i128::from(BASE_CELL_TICKS)
        .checked_mul(
            1_i128
                .checked_shl(u32::from(level))
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let minimum = bounds.min_ticks().map(|value| value.div_euclid(edge));
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| (value - 1).div_euclid(edge));
    let mut regions = Vec::new();
    crate::memory::reserve_exact(
        &mut regions,
        cell_region_count(bounds, level)?,
        "canonical cell regions",
    )?;
    for x in minimum[0]..=maximum[0] {
        for y in minimum[1]..=maximum[1] {
            for z in minimum[2]..=maximum[2] {
                let cell = WorldCellKey::new(
                    i64::try_from(x).map_err(|_| Error::NumericOverflow)?,
                    i64::try_from(y).map_err(|_| Error::NumericOverflow)?,
                    i64::try_from(z).map_err(|_| Error::NumericOverflow)?,
                    level,
                )?;
                let cell_bounds = intersect_bounds(bounds, cell.bounds())?.ok_or_else(|| {
                    Error::GraphDocument {
                        path: "evaluation.regions".to_owned(),
                        reason: "enumerated region cell does not intersect its source bounds"
                            .to_owned(),
                    }
                })?;
                regions.push(EvaluationRegion {
                    id: hierarchical_region_identity(hierarchy_namespace, cell),
                    kind: EvaluationRegionKind::Biome,
                    layer: hierarchy_namespace,
                    hierarchy_namespace: Some(hierarchy_namespace),
                    seed_cell: cell,
                    bounds: cell_bounds,
                });
            }
        }
    }
    Ok(regions)
}

fn stage_regions(
    node: &CompiledGraphNode,
    regions: &[EvaluationRegion],
    state: &EvaluationState<'_>,
) -> Result<Vec<EvaluationRegion>> {
    let level = node.definition.spatial.level();
    let minimum_input_level = state
        .scope
        .current_global_stage()
        .map_or(state.inputs.output_cell.level(), |stage| {
            stage.minimum_input_level
        });
    if level < minimum_input_level {
        return Ok(Vec::new());
    }
    let mut result = BTreeMap::<(u8, u128, WorldCellKey), EvaluationRegion>::new();
    for region in regions {
        if region.seed_cell.level() > level {
            return Err(Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "region seed cell is coarser than the node stage".to_owned(),
            });
        }
        let stage_cell = region.seed_cell.ancestor(level)?;
        let (kind, source, id) = if let Some(namespace) = region.hierarchy_namespace {
            (
                0,
                namespace,
                hierarchical_region_identity(namespace, stage_cell),
            )
        } else {
            (
                1,
                region.id,
                independent_stage_region_identity(region.id, stage_cell),
            )
        };
        let bounds = intersect_bounds(region.bounds, stage_cell.bounds())?;
        if let Some(bounds) = bounds {
            let key = (kind, source, stage_cell);
            if let Some(existing) = result.get_mut(&key) {
                existing.bounds = union_bounds(existing.bounds, bounds)?;
            } else {
                result.insert(
                    key,
                    EvaluationRegion {
                        id,
                        kind: region.kind,
                        layer: region.layer,
                        hierarchy_namespace: region.hierarchy_namespace,
                        seed_cell: stage_cell,
                        bounds,
                    },
                );
            }
        }
    }
    let mut regions = Vec::new();
    crate::memory::reserve_exact(&mut regions, result.len(), "canonical stage regions")?;
    regions.extend(result.into_values());
    Ok(regions)
}

fn hierarchical_region_identity(namespace: u128, cell: WorldCellKey) -> u128 {
    let hash = sha256(
        &[
            b"saffron-anima/hierarchical-region/v1\0".as_slice(),
            namespace.to_be_bytes().as_slice(),
            cell.canonical_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

fn independent_stage_region_identity(region: u128, cell: WorldCellKey) -> u128 {
    let hash = sha256(
        &[
            b"saffron-anima/independent-stage-region/v1\0".as_slice(),
            region.to_be_bytes().as_slice(),
            cell.canonical_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

fn union_bounds(left: WorldBounds, right: WorldBounds) -> Result<WorldBounds> {
    let minimum = std::array::from_fn(|axis| left.min_ticks()[axis].min(right.min_ticks()[axis]));
    let maximum = std::array::from_fn(|axis| {
        left.max_ticks_exclusive()[axis].max(right.max_ticks_exclusive()[axis])
    });
    WorldBounds::new(minimum, maximum).map_err(Into::into)
}

fn intersect_bounds(left: WorldBounds, right: WorldBounds) -> Result<Option<WorldBounds>> {
    let minimum = std::array::from_fn(|axis| left.min_ticks()[axis].max(right.min_ticks()[axis]));
    let maximum = std::array::from_fn(|axis| {
        left.max_ticks_exclusive()[axis].min(right.max_ticks_exclusive()[axis])
    });
    if (0..3).any(|axis| minimum[axis] >= maximum[axis]) {
        Ok(None)
    } else {
        WorldBounds::new(minimum, maximum)
            .map(Some)
            .map_err(Into::into)
    }
}

fn expand_bounds_checked(bounds: WorldBounds, radius: i128) -> Result<WorldBounds> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut expanded_minimum = [0_i128; 3];
    let mut expanded_maximum = [0_i128; 3];
    for axis in 0..3 {
        expanded_minimum[axis] = minimum[axis]
            .checked_sub(radius)
            .ok_or(Error::NumericOverflow)?;
        expanded_maximum[axis] = maximum[axis]
            .checked_add(radius)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldBounds::new(expanded_minimum, expanded_maximum).map_err(Into::into)
}

fn bounds_contains_bounds(container: WorldBounds, contained: WorldBounds) -> bool {
    (0..3).all(|axis| {
        container.min_ticks()[axis] <= contained.min_ticks()[axis]
            && container.max_ticks_exclusive()[axis] >= contained.max_ticks_exclusive()[axis]
    })
}

fn stable_ordinal(parts: &[&[u8]]) -> Result<u64> {
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

fn candidate_ordinal(
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

fn candidate_lineage(node: &CompiledGraphNode, state: &EvaluationState<'_>) -> CandidateLineage {
    CandidateLineage(node_execution_address(node, state))
}

fn ensure_lineage(
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

fn node_execution_address(node: &CompiledGraphNode, state: &EvaluationState<'_>) -> u128 {
    node.address().execution_identity(state.graph.biome)
}

fn poisson_annulus_position(
    center: WorldPosition,
    minimum_radius: i128,
    stream: RandomStream,
) -> Result<WorldPosition> {
    annulus_position(
        center,
        minimum_radius,
        minimum_radius
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?,
        stream,
    )
}

fn annulus_position(
    center: WorldPosition,
    minimum_radius: i128,
    maximum_radius: i128,
    stream: RandomStream,
) -> Result<WorldPosition> {
    if minimum_radius < 0 || maximum_radius < minimum_radius {
        return Err(Error::NumericOverflow);
    }
    let radius_span = maximum_radius
        .checked_sub(minimum_radius)
        .ok_or(Error::NumericOverflow)?;
    let radius_offset = div_round_ties_even(
        radius_span
            .checked_mul(i128::from(stream.lane(0, 1)))
            .ok_or(Error::NumericOverflow)?,
        i128::from(u32::MAX),
    )?;
    let radius = minimum_radius
        .checked_add(radius_offset)
        .ok_or(Error::NumericOverflow)?;
    let (cosine, sine) = cordic_sin_cos(stream.lane(0, 0));
    let dx = div_round_ties_even(
        radius
            .checked_mul(i128::from(cosine))
            .ok_or(Error::NumericOverflow)?,
        1_i128 << 30,
    )?;
    let dz = div_round_ties_even(
        radius
            .checked_mul(i128::from(sine))
            .ok_or(Error::NumericOverflow)?,
        1_i128 << 30,
    )?;
    offset_ticks(center, [dx, 0, dz])
}

fn stratified_position(
    bounds: WorldBounds,
    x: u64,
    z: u64,
    side: u64,
    jitter: UnitInterval,
    stream: RandomStream,
) -> Result<WorldPosition> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut ticks = minimum;
    for (axis, stratum, lane) in [(0, x, 0), (2, z, 1)] {
        let span = maximum[axis] - minimum[axis];
        let center_numerator = i128::from(stratum)
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .ok_or(Error::NumericOverflow)?;
        let center = minimum[axis]
            + div_round_ties_even(
                span.checked_mul(center_numerator)
                    .ok_or(Error::NumericOverflow)?,
                i128::from(side)
                    .checked_mul(2)
                    .ok_or(Error::NumericOverflow)?,
            )?;
        let cell_span = span / i128::from(side.max(1));
        let signed = i128::from(stream.lane(0, lane)) - i128::from(u32::MAX) / 2;
        let jitter_ticks = div_round_ties_even(
            signed
                .checked_mul(cell_span)
                .and_then(|value| value.checked_mul(i128::from(jitter.bits())))
                .ok_or(Error::NumericOverflow)?,
            i128::from(u32::MAX)
                .checked_mul(i128::from(u16::MAX))
                .ok_or(Error::NumericOverflow)?,
        )?;
        ticks[axis] = center
            .checked_add(jitter_ticks)
            .ok_or(Error::NumericOverflow)?
            .clamp(minimum[axis], maximum[axis] - 1);
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

fn uniform_position(
    bounds: WorldBounds,
    stream: RandomStream,
    sample: u64,
) -> Result<WorldPosition> {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let random = stream.sample(sample);
    let mut ticks = [0_i128; 3];
    for axis in 0..3 {
        let span = maximum[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let offset = span
            .checked_mul(i128::from(random[axis]))
            .ok_or(Error::NumericOverflow)?
            / (i128::from(u32::MAX) + 1);
        ticks[axis] = minimum[axis]
            .checked_add(offset)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

fn contains_required_tags(tags: &[WeightedSurfaceTag], required: &[u64]) -> bool {
    required.iter().all(|required| {
        tags.iter()
            .any(|tag| tag.tag.0 == *required && tag.weight.bits() != 0)
    })
}

fn distance_key(value: f64) -> Result<u64> {
    if !value.is_finite() || value < 0.0 {
        return Err(Error::NumericOverflow);
    }
    let bits = value.to_bits();
    Ok(bits)
}

fn prototype_bounds(prototype: &PlantPrototype, candidate: &GraphCandidate) -> Result<WorldBounds> {
    let [x, y, z, w] = candidate.orientation.bits().map(i128::from);
    let scale = i128::from(i16::MAX);
    let scale_squared = scale.checked_mul(scale).ok_or(Error::NumericOverflow)?;
    let twice = |value: i128| value.checked_mul(2).ok_or(Error::NumericOverflow);
    let matrix = [
        [
            scale_squared
                .checked_sub(twice(y * y + z * z)?)
                .ok_or(Error::NumericOverflow)?,
            twice(x * y - z * w)?,
            twice(x * z + y * w)?,
        ],
        [
            twice(x * y + z * w)?,
            scale_squared
                .checked_sub(twice(x * x + z * z)?)
                .ok_or(Error::NumericOverflow)?,
            twice(y * z - x * w)?,
        ],
        [
            twice(x * z - y * w)?,
            twice(y * z + x * w)?,
            scale_squared
                .checked_sub(twice(x * x + y * y)?)
                .ok_or(Error::NumericOverflow)?,
        ],
    ];
    let position = candidate.position.global_ticks();
    let mut minimum = [i128::MAX; 3];
    let mut maximum = [i128::MIN; 3];
    for corner in 0..8 {
        let local: [DecisionScalar; 3] = std::array::from_fn(|axis| {
            if corner & (1 << axis) == 0 {
                prototype.local_bounds_min[axis]
            } else {
                prototype.local_bounds_max[axis]
            }
        });
        let scaled = [
            local[0].checked_mul(candidate.scale[0])?.bits(),
            local[1].checked_mul(candidate.scale[1])?.bits(),
            local[2].checked_mul(candidate.scale[2])?.bits(),
        ];
        for axis in 0..3 {
            let rotated =
                matrix[axis]
                    .iter()
                    .zip(scaled)
                    .try_fold(0_i128, |sum, (coefficient, value)| {
                        sum.checked_add(
                            coefficient
                                .checked_mul(i128::from(value))
                                .ok_or(Error::NumericOverflow)?,
                        )
                        .ok_or(Error::NumericOverflow)
                    })?;
            let fixed = DecisionScalar::from_bits(
                i32::try_from(div_round_ties_even(rotated, scale_squared)?)
                    .map_err(|_| Error::NumericOverflow)?,
            );
            let world = position[axis]
                .checked_add(fixed_meters_to_ticks(fixed)?)
                .ok_or(Error::NumericOverflow)?;
            minimum[axis] = minimum[axis].min(world);
            maximum[axis] = maximum[axis].max(world);
        }
    }
    let [maximum_x, maximum_y, maximum_z] =
        maximum.map(|value| value.checked_add(1).ok_or(Error::NumericOverflow));
    let maximum = [maximum_x?, maximum_y?, maximum_z?];
    WorldBounds::new(minimum, maximum).map_err(Into::into)
}

fn candidate_bounds(
    candidate: &GraphCandidate,
    state: &EvaluationState<'_>,
) -> Result<WorldBounds> {
    if let Some(point) = &candidate.authored_point {
        return Ok(point.bounds);
    }
    let family = candidate
        .family
        .ok_or_else(|| Error::GraphAuthoritativeInput {
            node: candidate.identity.node,
            input: "plant family before bounds-overlap".to_owned(),
        })?;
    prototype_bounds(prototype_for_family(state, family)?, candidate)
}

fn bounds_support_radius(position: WorldPosition, bounds: WorldBounds) -> Result<i128> {
    let point = position.global_ticks();
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    (0..3).try_fold(0_i128, |radius, axis| {
        let low = point[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let high = maximum[axis]
            .checked_sub(1)
            .and_then(|value| value.checked_sub(point[axis]))
            .ok_or(Error::NumericOverflow)?;
        Ok(radius
            .max(low.checked_abs().ok_or(Error::NumericOverflow)?)
            .max(high.checked_abs().ok_or(Error::NumericOverflow)?))
    })
}

fn bounds_intersect(left: WorldBounds, right: WorldBounds) -> bool {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    (0..3).all(|axis| {
        left_minimum[axis] < right_maximum[axis] && right_minimum[axis] < left_maximum[axis]
    })
}

fn touches_boundary(bounds: WorldBounds, cell: WorldBounds) -> bool {
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let cell_minimum = cell.min_ticks();
    let cell_maximum = cell.max_ticks_exclusive();
    (0..3).any(|axis| minimum[axis] <= cell_minimum[axis] || maximum[axis] >= cell_maximum[axis])
}

fn encode_world_bounds<S: CanonicalSink>(sink: &mut S, bounds: WorldBounds) -> Result<()> {
    for tick in bounds.min_ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    for tick in bounds.max_ticks_exclusive() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

fn encode_world_position<S: CanonicalSink>(sink: &mut S, position: WorldPosition) -> Result<()> {
    sink.write(&position.cell().canonical_bytes())?;
    for tick in position.local().ticks() {
        sink.write(&tick.to_be_bytes())?;
    }
    Ok(())
}

fn tile_index(position: WorldPosition, bounds: WorldBounds, dimensions: [u32; 3]) -> Result<usize> {
    if dimensions.contains(&0) || !bounds.contains(position) {
        return Err(Error::GraphDocument {
            path: "micro-output".to_owned(),
            reason: "invalid dimensions or foreign point".to_owned(),
        });
    }
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let point = position.global_ticks();
    let mut coordinate = [0_u32; 3];
    for axis in 0..3 {
        let span = maximum[axis] - minimum[axis];
        let offset = point[axis] - minimum[axis];
        let scaled = offset
            .checked_mul(i128::from(dimensions[axis]))
            .ok_or(Error::NumericOverflow)?
            / span;
        coordinate[axis] = u32::try_from(scaled)
            .map_err(|_| Error::NumericOverflow)?
            .min(dimensions[axis] - 1);
    }
    let index = u64::from(coordinate[0])
        .checked_mul(u64::from(dimensions[1]))
        .and_then(|value| value.checked_add(u64::from(coordinate[1])))
        .and_then(|value| value.checked_mul(u64::from(dimensions[2])))
        .and_then(|value| value.checked_add(u64::from(coordinate[2])))
        .ok_or(Error::NumericOverflow)?;
    usize::try_from(index).map_err(|_| Error::NumericOverflow)
}

fn packed_sample_count(dimensions: [u32; 3]) -> Result<u64> {
    let count = dimensions.into_iter().try_fold(1_u64, |product, value| {
        product
            .checked_mul(u64::from(value))
            .ok_or(Error::NumericOverflow)
    })?;
    if count == 0 {
        return Err(Error::GraphDocument {
            path: "evaluation.tile.dimensions".to_owned(),
            reason: "tile dimensions must be non-zero".to_owned(),
        });
    }
    Ok(count)
}

fn tile_sample_position(
    bounds: WorldBounds,
    dimensions: [u32; 3],
    index: u64,
) -> Result<WorldPosition> {
    let count = packed_sample_count(dimensions)?;
    if index >= count {
        return Err(Error::NumericOverflow);
    }
    let yz = u64::from(dimensions[1])
        .checked_mul(u64::from(dimensions[2]))
        .ok_or(Error::NumericOverflow)?;
    let coordinates = [
        index / yz,
        (index % yz) / u64::from(dimensions[2]),
        index % u64::from(dimensions[2]),
    ];
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let mut ticks = [0_i128; 3];
    for axis in 0..3 {
        let span = maximum[axis]
            .checked_sub(minimum[axis])
            .ok_or(Error::NumericOverflow)?;
        let numerator = i128::from(coordinates[axis])
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| value.checked_mul(span))
            .ok_or(Error::NumericOverflow)?;
        let denominator = i128::from(dimensions[axis])
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?;
        let offset = div_round_ties_even(numerator, denominator)?.min(span - 1);
        ticks[axis] = minimum[axis]
            .checked_add(offset)
            .ok_or(Error::NumericOverflow)?;
    }
    WorldPosition::from_global_ticks(ticks).map_err(Into::into)
}

fn field_channel_name(channel: FieldChannel) -> &'static str {
    match channel {
        FieldChannel::Altitude => "altitude",
        FieldChannel::Slope => "slope",
        FieldChannel::Curvature => "curvature",
        FieldChannel::Concavity => "concavity",
        FieldChannel::Drainage => "drainage",
        FieldChannel::Moisture => "moisture",
        FieldChannel::Temperature => "temperature",
        FieldChannel::Precipitation => "precipitation",
        FieldChannel::Sunlight => "sunlight",
        FieldChannel::Exposure => "exposure",
        FieldChannel::WaterDistance => "water-distance",
        FieldChannel::WaterDepth => "water-depth",
        FieldChannel::SignedBlocker => "signed-blocker",
        FieldChannel::SplineDistance => "spline-distance",
        FieldChannel::User(_) => "user",
    }
}

fn orientation_from_normal_and_yaw(
    normal: Option<[SignedUnit; 3]>,
    yaw: UnitInterval,
) -> Result<QuantizedOrientation> {
    let Some(normal) = normal else {
        return yaw_orientation(yaw);
    };
    let n = normal.map(|value| i64::from(value.bits()));
    let unit = i64::from(i16::MAX);
    let align = if n[1] <= -unit + 1 {
        [unit, 0, 0, 0]
    } else {
        normalize_quaternion_q15([n[2], 0, -n[0], unit + n[1]])?
    };
    let yaw = yaw_quaternion_q15(yaw)?;
    let product = multiply_quaternion_q15(align, yaw)?;
    QuantizedOrientation::new(
        product
            .map(i16::try_from)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::NumericOverflow)?
            .try_into()
            .map_err(|_| Error::NumericOverflow)?,
    )
}

fn yaw_orientation(yaw: UnitInterval) -> Result<QuantizedOrientation> {
    let quaternion = yaw_quaternion_q15(yaw)?;
    QuantizedOrientation::new(
        quaternion
            .map(i16::try_from)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::NumericOverflow)?
            .try_into()
            .map_err(|_| Error::NumericOverflow)?,
    )
}

fn yaw_quaternion_q15(yaw: UnitInterval) -> Result<[i64; 4]> {
    let half_angle = div_round_ties_even(
        i128::from(yaw.bits()) * (1_i128 << 32),
        i128::from(u16::MAX) * 2,
    )?;
    let (cosine, sine) =
        cordic_sin_cos(u32::try_from(half_angle).map_err(|_| Error::NumericOverflow)?);
    normalize_quaternion_q15([0, q30_to_q15(sine)?, 0, q30_to_q15(cosine)?])
}

fn cordic_sin_cos(mut angle: u32) -> (i64, i64) {
    const ATAN: [i64; 16] = [
        0x2000_0000,
        0x12E4_051D,
        0x09FB_385B,
        0x0511_11D4,
        0x028B_0D43,
        0x0145_D7E1,
        0x00A2_F61E,
        0x0051_7C55,
        0x0028_BE53,
        0x0014_5F2F,
        0x000A_2F98,
        0x0005_17CC,
        0x0002_8BE6,
        0x0001_45F3,
        0x0000_A2FA,
        0x0000_517D,
    ];
    let mut negate = false;
    if angle > 0x4000_0000 && angle < 0xC000_0000 {
        angle = angle.wrapping_sub(0x8000_0000);
        negate = true;
    }
    let mut x = 652_032_874_i64;
    let mut y = 0_i64;
    let mut z = i64::from(angle as i32);
    for (index, atan) in ATAN.into_iter().enumerate() {
        let direction = if z >= 0 { 1 } else { -1 };
        let next_x = x - direction * (y >> index);
        let next_y = y + direction * (x >> index);
        x = next_x;
        y = next_y;
        z -= direction * atan;
    }
    if negate { (-x, -y) } else { (x, y) }
}

fn q30_to_q15(value: i64) -> Result<i64> {
    let rounded = div_round_ties_even(i128::from(value) * i128::from(i16::MAX), 1_i128 << 30)?;
    i64::try_from(rounded).map_err(|_| Error::NumericOverflow)
}

fn normalize_quaternion_q15(value: [i64; 4]) -> Result<[i64; 4]> {
    let length_squared = value.iter().try_fold(0_i128, |sum, lane| {
        sum.checked_add(i128::from(*lane) * i128::from(*lane))
            .ok_or(Error::NumericOverflow)
    })?;
    if length_squared == 0 {
        return Err(Error::NumericOverflow);
    }
    let length = integer_sqrt(length_squared);
    let mut result = [0_i64; 4];
    for index in 0..4 {
        let lane = div_round_ties_even(i128::from(value[index]) * i128::from(i16::MAX), length)?;
        result[index] = i64::try_from(lane)
            .map_err(|_| Error::NumericOverflow)?
            .clamp(-i64::from(i16::MAX), i64::from(i16::MAX));
    }
    Ok(result)
}

fn multiply_quaternion_q15(left: [i64; 4], right: [i64; 4]) -> Result<[i64; 4]> {
    let [lx, ly, lz, lw] = left;
    let [rx, ry, rz, rw] = right;
    let raw = [
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ];
    let scaled = raw.map(|value| {
        div_round_ties_even(i128::from(value), i128::from(i16::MAX)).and_then(|value| {
            i64::try_from(value).map_err(|_| saffron_spatial::Error::NumericOverflow)
        })
    });
    let scaled = scaled
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .try_into()
        .unwrap();
    normalize_quaternion_q15(scaled)
}

fn interpolate_unit(
    start: UnitInterval,
    end: UnitInterval,
    weight: UnitInterval,
) -> Result<UnitInterval> {
    let delta = i128::from(end.bits()) - i128::from(start.bits());
    let value = i128::from(start.bits())
        + div_round_ties_even(delta * i128::from(weight.bits()), i128::from(u16::MAX))?;
    Ok(UnitInterval::from_bits(
        u16::try_from(value).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn offset_fixed(position: WorldPosition, offset: [DecisionScalar; 3]) -> Result<WorldPosition> {
    offset_ticks(
        position,
        [
            fixed_meters_to_ticks(offset[0])?,
            fixed_meters_to_ticks(offset[1])?,
            fixed_meters_to_ticks(offset[2])?,
        ],
    )
}

fn offset_ticks(position: WorldPosition, offset: [i128; 3]) -> Result<WorldPosition> {
    let ticks = position.global_ticks();
    WorldPosition::from_global_ticks([
        ticks[0]
            .checked_add(offset[0])
            .ok_or(Error::NumericOverflow)?,
        ticks[1]
            .checked_add(offset[1])
            .ok_or(Error::NumericOverflow)?,
        ticks[2]
            .checked_add(offset[2])
            .ok_or(Error::NumericOverflow)?,
    ])
    .map_err(Into::into)
}

fn fixed_meters_to_ticks(value: DecisionScalar) -> Result<i128> {
    div_round_ties_even(
        i128::from(value.bits()) * i128::from(LOCAL_TICKS_PER_METER),
        65_536,
    )
    .map_err(Into::into)
}

fn ensure_support_ticks(node: &CompiledGraphNode, requested: i128) -> Result<()> {
    let crate::NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.definition.spatial
    else {
        return Ok(());
    };
    let limit = fixed_meters_to_ticks(influence_radius)?
        .checked_abs()
        .ok_or(Error::NumericOverflow)?;
    let requested = requested.checked_abs().ok_or(Error::NumericOverflow)?;
    if requested <= limit {
        return Ok(());
    }
    Err(Error::GraphLimit {
        resource: "node influence radius ticks",
        requested: u64::try_from(requested).unwrap_or(u64::MAX),
        limit: u64::try_from(limit).unwrap_or(u64::MAX),
    })
}

fn ticks_to_fixed_meters(value: i128) -> Result<i32> {
    let bits = div_round_ties_even(
        value.checked_mul(65_536).ok_or(Error::NumericOverflow)?,
        i128::from(LOCAL_TICKS_PER_METER),
    )?;
    i32::try_from(bits).map_err(|_| Error::NumericOverflow)
}

fn distance_squared_xz(left: WorldPosition, right: WorldPosition) -> Result<i128> {
    let left = left.global_ticks();
    let right = right.global_ticks();
    let dx = left[0]
        .checked_sub(right[0])
        .ok_or(Error::NumericOverflow)?;
    let dz = left[2]
        .checked_sub(right[2])
        .ok_or(Error::NumericOverflow)?;
    dx.checked_mul(dx)
        .and_then(|value| value.checked_add(dz.checked_mul(dz)?))
        .ok_or(Error::NumericOverflow)
}

fn xz_bucket(position: WorldPosition, edge: i128) -> (i128, i128) {
    let ticks = position.global_ticks();
    (ticks[0].div_euclid(edge), ticks[2].div_euclid(edge))
}

fn xyz_bucket(position: WorldPosition, edge: i128) -> (i128, i128, i128) {
    let ticks = position.global_ticks();
    (
        ticks[0].div_euclid(edge),
        ticks[1].div_euclid(edge),
        ticks[2].div_euclid(edge),
    )
}

fn offset_xz_bucket(bucket: (i128, i128), x: i8, z: i8) -> Result<(i128, i128)> {
    Ok((
        bucket
            .0
            .checked_add(i128::from(x))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .1
            .checked_add(i128::from(z))
            .ok_or(Error::NumericOverflow)?,
    ))
}

fn offset_xyz_bucket(
    bucket: (i128, i128, i128),
    x: i8,
    y: i8,
    z: i8,
) -> Result<(i128, i128, i128)> {
    Ok((
        bucket
            .0
            .checked_add(i128::from(x))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .1
            .checked_add(i128::from(y))
            .ok_or(Error::NumericOverflow)?,
        bucket
            .2
            .checked_add(i128::from(z))
            .ok_or(Error::NumericOverflow)?,
    ))
}

fn distance_squared(left: [i128; 3], right: [i128; 3]) -> Result<i128> {
    (0..3).try_fold(0_i128, |sum, axis| {
        let delta = left[axis]
            .checked_sub(right[axis])
            .ok_or(Error::NumericOverflow)?;
        sum.checked_add(delta.checked_mul(delta).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })
}

fn point_segment_distance_ticks(
    point: WorldPosition,
    start: WorldPosition,
    end: WorldPosition,
) -> Result<i128> {
    let point = point.global_ticks();
    let start = start.global_ticks();
    let end = end.global_ticks();
    let direction = [
        end[0].checked_sub(start[0]).ok_or(Error::NumericOverflow)?,
        end[1].checked_sub(start[1]).ok_or(Error::NumericOverflow)?,
        end[2].checked_sub(start[2]).ok_or(Error::NumericOverflow)?,
    ];
    let length_squared = direction.iter().try_fold(0_i128, |sum, value| {
        sum.checked_add(value.checked_mul(*value).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })?;
    if length_squared == 0 {
        return Ok(integer_sqrt(distance_squared(point, start)?));
    }
    let offset = [
        point[0]
            .checked_sub(start[0])
            .ok_or(Error::NumericOverflow)?,
        point[1]
            .checked_sub(start[1])
            .ok_or(Error::NumericOverflow)?,
        point[2]
            .checked_sub(start[2])
            .ok_or(Error::NumericOverflow)?,
    ];
    let dot = (0..3).try_fold(0_i128, |sum, axis| {
        sum.checked_add(
            offset[axis]
                .checked_mul(direction[axis])
                .ok_or(Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)
    })?;
    let numerator = dot.clamp(0, length_squared);
    let mut closest = [0_i128; 3];
    for axis in 0..3 {
        let displacement = direction[axis]
            .checked_mul(numerator)
            .ok_or(Error::NumericOverflow)?
            / length_squared;
        closest[axis] = start[axis]
            .checked_add(displacement)
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(integer_sqrt(distance_squared(point, closest)?))
}

fn point_bounds_distance_ticks(point: WorldPosition, bounds: WorldBounds) -> Result<i128> {
    let point = point.global_ticks();
    let minimum = bounds.min_ticks();
    let maximum = bounds.max_ticks_exclusive();
    let squared = (0..3).try_fold(0_i128, |sum, axis| {
        let delta = if point[axis] < minimum[axis] {
            minimum[axis]
                .checked_sub(point[axis])
                .ok_or(Error::NumericOverflow)?
        } else if point[axis] >= maximum[axis] {
            point[axis]
                .checked_sub(maximum[axis])
                .and_then(|value| value.checked_add(1))
                .ok_or(Error::NumericOverflow)?
        } else {
            0
        };
        sum.checked_add(delta.checked_mul(delta).ok_or(Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })?;
    Ok(integer_sqrt(squared))
}

fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }
    let mut low = 1_i128;
    let mut high = value.min(1_i128 << 64);
    while low <= high {
        let middle = low + (high - low) / 2;
        if middle <= value / middle {
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    high
}

fn integer_sqrt_ceil(value: u64) -> u64 {
    let floor = integer_sqrt(i128::from(value)) as u64;
    if floor * floor == value {
        floor
    } else {
        floor + 1
    }
}

fn candidate_key_u128(identity: CandidateIdentity) -> u128 {
    let hash = sha256(
        &[
            identity.node_address.to_be_bytes().as_slice(),
            identity.node.to_be_bytes().as_slice(),
            identity.node_semantic_revision.to_be_bytes().as_slice(),
            identity.ordinal.to_be_bytes().as_slice(),
            identity.ancestor.to_be_bytes().as_slice(),
        ]
        .concat(),
    );
    u128::from_be_bytes(hash[..16].try_into().unwrap())
}

fn push_len<S: CanonicalSink>(sink: &mut S, value: usize) -> Result<()> {
    sink.write(
        &u64::try_from(value)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    )
}

fn singleton(name: &str, value: GraphValue) -> BTreeMap<String, GraphValue> {
    BTreeMap::from([(name.to_owned(), value)])
}

fn candidates_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a CandidateStream> {
    match inputs.get(name) {
        Some(GraphValue::Candidates(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Candidates),
    }
}

fn optional_candidates_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a CandidateStream>> {
    match inputs.get(name) {
        Some(GraphValue::Candidates(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::Candidates),
        None => Ok(None),
    }
}

fn scalar_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a ScalarFieldSamples> {
    match inputs.get(name) {
        Some(GraphValue::Scalar(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::ScalarField),
    }
}

fn optional_scalar_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a ScalarFieldSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Scalar(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::ScalarField),
        None => Ok(None),
    }
}

fn optional_vector_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a VectorFieldSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Vector(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::VectorField),
        None => Ok(None),
    }
}

fn optional_surface_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a ProjectedSurfaceSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Surface(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::SurfaceField),
        None => Ok(None),
    }
}

fn regions_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [EvaluationRegion]> {
    match inputs.get(name) {
        Some(GraphValue::Regions(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Regions),
    }
}

fn splines_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [EvaluationSpline]> {
    match inputs.get(name) {
        Some(GraphValue::Splines(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Splines),
    }
}

fn species_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [crate::BiomePaletteEntry]> {
    match inputs.get(name) {
        Some(GraphValue::Species(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::SpeciesTable),
    }
}

fn communities_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a CommunityTables> {
    match inputs.get(name) {
        Some(GraphValue::Communities(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::CommunityTable),
    }
}

fn missing_input<T>(name: &str, domain: GraphDomain) -> Result<T> {
    Err(Error::GraphDocument {
        path: format!("input.{name}"),
        reason: format!("expected {}", domain.as_wire()),
    })
}

fn u32_parameter(node: &CompiledGraphNode, name: &str, fallback: u32) -> Result<u32> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U32(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn u64_parameter(node: &CompiledGraphNode, name: &str, fallback: u64) -> Result<u64> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U64(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn guid_parameter(node: &CompiledGraphNode, name: &str, fallback: u128) -> Result<u128> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Guid(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn bool_parameter(node: &CompiledGraphNode, name: &str, fallback: bool) -> Result<bool> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Boolean(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn fixed_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: DecisionScalar,
) -> Result<DecisionScalar> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn unit_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: UnitInterval,
) -> Result<UnitInterval> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Unit(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn fixed_vec3_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: [DecisionScalar; 3],
) -> Result<[DecisionScalar; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FixedVec3(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn world_position_parameter(node: &CompiledGraphNode, name: &str) -> Result<[i128; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::WorldPosition(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn u32_vec3_parameter(node: &CompiledGraphNode, name: &str) -> Result<[u32; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U32Vec3(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn string_parameter<'a>(
    node: &'a CompiledGraphNode,
    name: &str,
    fallback: &'a str,
) -> Result<&'a str> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::String(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn curve_parameter<'a>(
    node: &'a CompiledGraphNode,
    name: &str,
) -> Result<&'a [(UnitInterval, DecisionScalar)]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Curve(value)) => Ok(value),
        _ => wrong_parameter(node, name),
    }
}

fn field_parameter(node: &CompiledGraphNode, name: &str) -> Result<FieldChannel> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FieldChannel(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn field_derivative_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: FieldDerivative,
) -> Result<FieldDerivative> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FieldDerivative(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

fn combine_operation_parameter(
    node: &CompiledGraphNode,
    name: &str,
) -> Result<GraphCombineOperation> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::CombineOperation(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn distance_source_parameter(node: &CompiledGraphNode, name: &str) -> Result<GraphDistanceSource> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::DistanceSource(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn cluster_mode_parameter(node: &CompiledGraphNode, name: &str) -> Result<GraphClusterMode> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::ClusterMode(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

fn tag_list_parameter<'a>(node: &'a CompiledGraphNode, name: &str) -> Result<&'a [u64]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::TagList(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(&[]),
    }
}

fn guid_list_parameter<'a>(node: &'a CompiledGraphNode, name: &str) -> Result<&'a [u128]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::GuidList(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(&[]),
    }
}

fn wrong_parameter<T>(node: &CompiledGraphNode, name: &str) -> Result<T> {
    Err(Error::GraphDocument {
        path: format!(
            "graph.nodes.{:032x}.parameters.{name}",
            node.definition.guid
        ),
        reason: "parameter has the wrong type".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BIOME_ASSET_VERSION, BIOME_GRAPH_VERSION, BIOME_NODE_VERSION, BiomeAsset,
        BiomeGraphDocument, BiomeGraphPolicy, BiomeGraphResolver, BiomeModuleReference,
        BiomePaletteEntry, BiomeRole, GpuExecutionProfile, GpuQualificationRegistry,
        GpuShaderArtifactIdentity, GraphCompileOptions, GraphDependencySource, GraphEdge,
        GraphInterfaceInput, GraphInterfaceOutput, GraphNodeDefinition, GraphParameterValue,
        GraphSink, NodeSpatialPolicy, VegetationCellFacet, compile_biome_graph,
        decode_vegetation_cell_facet, evaluate_gpu_program_reference,
    };
    use saffron_spatial::{
        FieldSample, HessianFieldSample, SurfaceCapabilities, SurfaceDirtyRegion,
        SurfaceNearestQuery, SurfaceRay, VectorFieldSample,
    };
    use std::sync::atomic::AtomicUsize;

    struct NoDependencies;

    struct ReferenceCompute {
        profile: GpuExecutionProfile,
        qualifications: GpuQualificationRegistry,
    }

    struct CountingCompute {
        profile: GpuExecutionProfile,
        qualifications: GpuQualificationRegistry,
        dispatches: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct TestSurfaceField {
        descriptor: SurfaceProviderDescriptor,
        failing_cell_x: Option<i64>,
        project_hits: bool,
        successful_samples: Arc<AtomicUsize>,
    }

    impl SurfaceField for TestSurfaceField {
        fn descriptor(&self) -> SurfaceProviderDescriptor {
            self.descriptor.clone()
        }

        fn field_channels(&self) -> Vec<FieldChannel> {
            vec![FieldChannel::Moisture]
        }

        fn raycast(&self, _query: &SurfaceRay) -> saffron_spatial::Result<Option<SurfaceHit>> {
            Ok(None)
        }

        fn project(
            &self,
            query: &SurfaceProjection,
        ) -> saffron_spatial::Result<Option<SurfaceHit>> {
            if !self.project_hits {
                return Ok(None);
            }
            self.successful_samples.fetch_add(1, Ordering::SeqCst);
            Ok(Some(SurfaceHit {
                provider: self.descriptor.id,
                position: query.origin,
                distance_m: 0.0,
                frame: saffron_spatial::SurfaceFrame::from_normal(saffron_geometry::glam::Vec3::Y)?,
                coordinates: saffron_spatial::SurfaceCoordinates::default(),
                attachment: Some(SurfaceAttachment::new(
                    self.descriptor.id,
                    saffron_spatial::SurfacePrimitiveId(0),
                    [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                    self.descriptor.revision,
                )?),
                tags: Vec::new(),
                revision: self.descriptor.revision,
            }))
        }

        fn nearest(
            &self,
            _query: &SurfaceNearestQuery,
        ) -> saffron_spatial::Result<Option<SurfaceHit>> {
            Ok(None)
        }

        fn availability(
            &self,
            channel: FieldChannel,
            derivative: FieldDerivative,
            _bounds: WorldBounds,
        ) -> FieldAvailability {
            if channel == FieldChannel::Moisture && derivative == FieldDerivative::Value {
                FieldAvailability::Complete
            } else {
                FieldAvailability::Unavailable
            }
        }

        fn estimated_samples(&self, _channel: FieldChannel, _bounds: WorldBounds) -> u64 {
            24
        }

        fn sample_scalar(
            &self,
            channel: FieldChannel,
            derivative: FieldDerivative,
            position: WorldPosition,
        ) -> saffron_spatial::Result<FieldSample> {
            if self
                .failing_cell_x
                .is_some_and(|x| position.cell().coordinates()[0] == x)
            {
                return Err(saffron_spatial::Error::FieldUnavailable);
            }
            self.successful_samples.fetch_add(1, Ordering::SeqCst);
            Ok(FieldSample {
                channel,
                derivative,
                value: DecisionScalar::from_bits(65_535),
                revision: self.descriptor.revision,
            })
        }

        fn sample_vector(
            &self,
            channel: FieldChannel,
            derivative: FieldDerivative,
            _position: WorldPosition,
        ) -> saffron_spatial::Result<VectorFieldSample> {
            Ok(VectorFieldSample {
                channel,
                derivative,
                value: DecisionVec3::default(),
                revision: self.descriptor.revision,
            })
        }

        fn sample_hessian(
            &self,
            channel: FieldChannel,
            _position: WorldPosition,
        ) -> saffron_spatial::Result<HessianFieldSample> {
            Ok(HessianFieldSample {
                channel,
                derivative: FieldDerivative::Hessian,
                value: DecisionHessian3::default(),
                revision: self.descriptor.revision,
            })
        }

        fn authoritative_tiles(
            &self,
            _channel: FieldChannel,
            _bounds: WorldBounds,
        ) -> Vec<SurfaceTileDescriptor> {
            Vec::new()
        }

        fn changes_since(&self, _revision: SurfaceRevision) -> Vec<SurfaceDirtyRegion> {
            Vec::new()
        }

        fn reproject_attachment(
            &self,
            _attachment: SurfaceAttachment,
        ) -> saffron_spatial::Result<Option<SurfaceHit>> {
            Ok(None)
        }
    }

    struct SurfaceDependencies {
        provider_hash: [u8; 32],
    }

    impl BiomeGraphResolver for SurfaceDependencies {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            Err(Error::GraphDocument {
                path: "test.resolver".to_owned(),
                reason: format!("unexpected module {}", id.value()),
            })
        }

        fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
            match source {
                GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
                GraphDependencySource::Field(FieldChannel::Moisture) => Ok([2; 32]),
                GraphDependencySource::SurfaceProvider(77) => Ok(self.provider_hash),
                _ => Err(Error::GraphDocument {
                    path: "test.resolver".to_owned(),
                    reason: format!("unexpected dependency {source:?}"),
                }),
            }
        }

        fn available_dependencies(&self) -> Vec<GraphDependencySource> {
            vec![GraphDependencySource::SurfaceProvider(77)]
        }
    }

    impl GraphComputeExecutor for ReferenceCompute {
        fn profile(&self) -> &GpuExecutionProfile {
            &self.profile
        }

        fn qualifications(&self) -> &GpuQualificationRegistry {
            &self.qualifications
        }

        fn execute_program(
            &self,
            program: &GraphGpuProgram,
            invocations: &GraphGpuInvocationBatch,
            cancellation: &GraphCancellationToken,
            deadline: Instant,
        ) -> Result<Vec<crate::GraphGpuOutput>> {
            if cancellation.is_cancelled() {
                return Err(Error::GraphCancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::GraphLimit {
                    resource: "time milliseconds",
                    requested: 1,
                    limit: 0,
                });
            }
            evaluate_gpu_program_reference(program, invocations)
        }
    }

    impl GraphComputeExecutor for CountingCompute {
        fn profile(&self) -> &GpuExecutionProfile {
            &self.profile
        }

        fn qualifications(&self) -> &GpuQualificationRegistry {
            &self.qualifications
        }

        fn execute_program(
            &self,
            program: &GraphGpuProgram,
            invocations: &GraphGpuInvocationBatch,
            cancellation: &GraphCancellationToken,
            deadline: Instant,
        ) -> Result<Vec<crate::GraphGpuOutput>> {
            if cancellation.is_cancelled() {
                return Err(Error::GraphCancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::GraphLimit {
                    resource: "time milliseconds",
                    requested: 1,
                    limit: 0,
                });
            }
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            evaluate_gpu_program_reference(program, invocations)
        }
    }

    fn reference_compute() -> Arc<dyn GraphComputeExecutor> {
        let profile = GpuExecutionProfile {
            name: "reference-test".to_owned(),
            vendor_id: 1,
            device_id: 2,
            driver_version: 3,
            api_version: 4,
            driver_id: 5,
            device_uuid: [6; 16],
            driver_uuid: [7; 16],
            molten_vk: false,
        };
        let qualifications = GpuQualificationRegistry::qualify(
            profile.clone(),
            GpuShaderArtifactIdentity {
                record_hash: [8; 32],
                compile_input_hash: [9; 32],
                spirv_hash: [10; 32],
                compiler_identity_hash: [11; 32],
            },
            evaluate_gpu_program_reference,
        )
        .unwrap();
        Arc::new(ReferenceCompute {
            profile,
            qualifications,
        })
    }

    fn counting_compute(dispatches: Arc<AtomicUsize>) -> Arc<dyn GraphComputeExecutor> {
        let profile = GpuExecutionProfile {
            name: "counting-test".to_owned(),
            vendor_id: 1,
            device_id: 2,
            driver_version: 3,
            api_version: 4,
            driver_id: 5,
            device_uuid: [6; 16],
            driver_uuid: [7; 16],
            molten_vk: false,
        };
        let qualifications = GpuQualificationRegistry::qualify(
            profile.clone(),
            GpuShaderArtifactIdentity {
                record_hash: [8; 32],
                compile_input_hash: [9; 32],
                spirv_hash: [10; 32],
                compiler_identity_hash: [11; 32],
            },
            evaluate_gpu_program_reference,
        )
        .unwrap();
        Arc::new(CountingCompute {
            profile,
            qualifications,
            dispatches,
        })
    }

    impl BiomeGraphResolver for NoDependencies {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            Err(Error::GraphDocument {
                path: "test.resolver".to_owned(),
                reason: format!("unexpected module {}", id.value()),
            })
        }

        fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
            match source {
                GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
                GraphDependencySource::MapLayer(42) => Ok([4; 32]),
                _ => Err(Error::GraphDocument {
                    path: "test.resolver".to_owned(),
                    reason: format!("unexpected dependency {source:?}"),
                }),
            }
        }
    }

    fn node(guid: u128, operator: GraphOperator, level: u8) -> GraphNodeDefinition {
        GraphNodeDefinition {
            guid,
            version: BIOME_NODE_VERSION,
            semantic_revision: 1,
            operator,
            authority: GraphAuthority::Authoritative,
            spatial: NodeSpatialPolicy::Partitioned {
                level,
                influence_radius: DecisionScalar::from_bits(0),
            },
            dependencies: Vec::new(),
            seed_namespaces: BTreeMap::new(),
            parameters: BTreeMap::new(),
        }
    }

    fn fixture_document(level: u8) -> BiomeGraphDocument {
        let region = node(1, GraphOperator::RegionInput, level);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(24));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::from_bits(32_768)),
        );
        let species = node(3, GraphOperator::SpeciesInput, level);
        let mut output = node(4, GraphOperator::MacroOutput, level);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let unrelated = node(5, GraphOperator::RegionInput, level);
        let mut noise = node(6, GraphOperator::Noise, level);
        noise.authority = GraphAuthority::EquivalentGpu;
        noise.seed_namespaces.insert("noise".to_owned(), 17);
        noise.parameters.insert(
            "frequency".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
        );
        noise.parameters.insert(
            "amplitude".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise
            .parameters
            .insert("channel".to_owned(), GraphParameterValue::U32(3));
        let communities = node(7, GraphOperator::CommunityInput, level);
        let mut blend = node(8, GraphOperator::CommunityBlend, level);
        blend.seed_namespaces.insert("community".to_owned(), 19);
        let mut competition = node(9, GraphOperator::Competition, level);
        competition.spatial = NodeSpatialPolicy::Partitioned {
            level,
            influence_radius: DecisionScalar::from_bits(4 * 65_536),
        };
        competition.parameters.insert(
            "crownWeight".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ONE),
        );
        competition.parameters.insert(
            "rootWeight".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ONE),
        );
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![
                output,
                unrelated,
                noise,
                competition,
                blend,
                communities,
                species,
                coverage,
                region,
            ],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 9,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 8,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 7,
                    from_pin: "communities".to_owned(),
                    to_node: 8,
                    to_pin: "communities".to_owned(),
                },
                GraphEdge {
                    from_node: 8,
                    from_pin: "candidates".to_owned(),
                    to_node: 9,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 7,
                    from_pin: "communities".to_owned(),
                    to_node: 9,
                    to_pin: "communities".to_owned(),
                },
            ],
        }
    }

    fn fixture_asset(level: u8) -> BiomeAsset {
        BiomeAsset {
            version: BIOME_ASSET_VERSION,
            id: Uuid(701),
            name: "Determinism fixture".to_owned(),
            role: BiomeRole::Root,
            parameters: Vec::new(),
            palette: vec![BiomePaletteEntry {
                plant: Uuid(702),
                weight: UnitInterval::ONE,
                seed_namespace: 13,
            }],
            density: DecisionScalar::from_bits(65_536),
            clustering: UnitInterval::ZERO,
            suitability: Vec::new(),
            competition: Vec::new(),
            companions: Vec::new(),
            succession: Vec::new(),
            seed_namespaces: vec![
                ("poisson".to_owned(), 11),
                ("species".to_owned(), 13),
                ("noise".to_owned(), 17),
                ("community".to_owned(), 19),
            ],
            modules: Vec::new(),
            policy: BiomeGraphPolicy {
                maximum_recursion: 8,
                maximum_influence_radius: DecisionScalar::from_bits(6 * 65_536),
                require_authoritative_fields: true,
            },
            graph: fixture_document(level).to_json(),
        }
    }

    fn compile_fixture(level: u8) -> CompiledBiomeGraph {
        compile_biome_graph(
            &fixture_asset(level),
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_resident_branch_fixture() -> CompiledBiomeGraph {
        let level = 0;
        let region = node(1, GraphOperator::RegionInput, level);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(24));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::from_bits(32_768)),
        );
        let species = node(3, GraphOperator::SpeciesInput, level);
        let mut output = node(4, GraphOperator::MacroOutput, level);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let mut noise = node(6, GraphOperator::Noise, level);
        noise.authority = GraphAuthority::EquivalentGpu;
        noise.seed_namespaces.insert("noise".to_owned(), 17);
        noise.parameters.insert(
            "frequency".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
        );
        noise.parameters.insert(
            "amplitude".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise
            .parameters
            .insert("channel".to_owned(), GraphParameterValue::U32(3));
        let mut curve = node(10, GraphOperator::Curve, level);
        curve.authority = GraphAuthority::EquivalentGpu;
        curve.parameters.insert(
            "curve".to_owned(),
            GraphParameterValue::Curve(vec![
                (UnitInterval::ZERO, DecisionScalar::from_bits(0)),
                (UnitInterval::ONE, DecisionScalar::from_bits(65_535)),
            ]),
        );
        let mut remap = node(11, GraphOperator::Remap, level);
        remap.authority = GraphAuthority::EquivalentGpu;
        for (name, value) in [
            ("inputMin", -65_536),
            ("inputMax", 65_536),
            ("outputMin", 0),
            ("outputMax", 32_768),
        ] {
            remap.parameters.insert(
                name.to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(value)),
            );
        }
        let mut combine = node(12, GraphOperator::Combine, level);
        combine.authority = GraphAuthority::EquivalentGpu;
        combine.parameters.insert(
            "operation".to_owned(),
            GraphParameterValue::CombineOperation(GraphCombineOperation::Add),
        );
        let mut clamp = node(13, GraphOperator::Clamp, level);
        clamp.authority = GraphAuthority::EquivalentGpu;
        clamp.parameters.insert(
            "minimum".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
        );
        clamp.parameters.insert(
            "maximum".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_535)),
        );
        let mut importance = node(14, GraphOperator::FieldImportance, level);
        importance.authority = GraphAuthority::EquivalentGpu;
        importance.parameters.insert(
            "threshold".to_owned(),
            GraphParameterValue::Unit(UnitInterval::from_bits(16_384)),
        );
        let mut dead_clamp = node(15, GraphOperator::Clamp, level);
        dead_clamp.authority = GraphAuthority::EquivalentGpu;
        dead_clamp.parameters.insert(
            "minimum".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
        );
        dead_clamp.parameters.insert(
            "maximum".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_535)),
        );
        let document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![
                dead_clamp, output, importance, clamp, combine, remap, curve, noise, species,
                coverage, region,
            ],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "field".to_owned(),
                    to_node: 10,
                    to_pin: "field".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "field".to_owned(),
                    to_node: 11,
                    to_pin: "field".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "field".to_owned(),
                    to_node: 15,
                    to_pin: "field".to_owned(),
                },
                GraphEdge {
                    from_node: 10,
                    from_pin: "field".to_owned(),
                    to_node: 12,
                    to_pin: "left".to_owned(),
                },
                GraphEdge {
                    from_node: 11,
                    from_pin: "field".to_owned(),
                    to_node: 12,
                    to_pin: "right".to_owned(),
                },
                GraphEdge {
                    from_node: 12,
                    from_pin: "field".to_owned(),
                    to_node: 13,
                    to_pin: "field".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 14,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 13,
                    from_pin: "field".to_owned(),
                    to_node: 14,
                    to_pin: "weights".to_owned(),
                },
                GraphEdge {
                    from_node: 14,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut asset = fixture_asset(level);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_global_fixture() -> CompiledBiomeGraph {
        let mut asset = fixture_asset(0);
        let mut document = fixture_document(0);
        let sampler = document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap();
        sampler.operator = GraphOperator::BlueNoisePoisson;
        sampler.spatial = NodeSpatialPolicy::Global { level: 1 };
        sampler.parameters.remove("jitter");
        sampler.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(2 * 65_536)),
        );
        sampler
            .parameters
            .insert("attempts".to_owned(), GraphParameterValue::U32(24));
        for guid in [4_u128, 8, 9] {
            document
                .nodes
                .iter_mut()
                .find(|node| node.guid == guid)
                .unwrap()
                .spatial = NodeSpatialPolicy::Global { level: 1 };
        }
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_same_level_macro_stages_fixture() -> CompiledBiomeGraph {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut outputs = Vec::new();
        for branch in 0_u128..2 {
            let base = branch * 10;
            let region = node(base + 1, GraphOperator::RegionInput, 0);
            let mut coverage = node(base + 2, GraphOperator::StratifiedCoverage, 1);
            coverage.spatial = NodeSpatialPolicy::Global { level: 1 };
            coverage.seed_namespaces.insert("sampling".to_owned(), 11);
            coverage
                .parameters
                .insert("count".to_owned(), GraphParameterValue::U32(1));
            coverage.parameters.insert(
                "jitter".to_owned(),
                GraphParameterValue::Unit(UnitInterval::ZERO),
            );
            let species = node(base + 3, GraphOperator::SpeciesInput, 0);
            let mut output = node(base + 4, GraphOperator::MacroOutput, 1);
            output.spatial = NodeSpatialPolicy::Global { level: 1 };
            output
                .seed_namespaces
                .insert("species-selection".to_owned(), 13);
            nodes.extend([output, species, coverage, region]);
            edges.extend([
                GraphEdge {
                    from_node: base + 1,
                    from_pin: "regions".to_owned(),
                    to_node: base + 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: base + 2,
                    from_pin: "candidates".to_owned(),
                    to_node: base + 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: base + 3,
                    from_pin: "species".to_owned(),
                    to_node: base + 4,
                    to_pin: "species".to_owned(),
                },
            ]);
            outputs.push(GraphInterfaceOutput {
                id: 1_004 + base,
                name: format!("macro-{branch}"),
                domain: GraphDomain::MacroPoints,
                node: base + 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            });
        }
        compile_document(BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs,
            nodes,
            edges,
        })
    }

    fn compile_candidate_only_global_fixture() -> CompiledBiomeGraph {
        let mut document = recursive_document();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 3)
            .unwrap()
            .spatial = NodeSpatialPolicy::Global { level: 1 };
        compile_document(document)
    }

    fn compile_split_surface_projection_fixture(provider_hash: [u8; 32]) -> CompiledBiomeGraph {
        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut projection = node(3, GraphOperator::SurfaceProjection, 0);
        projection.spatial = NodeSpatialPolicy::Global { level: 0 };
        projection.parameters.insert(
            "direction".to_owned(),
            GraphParameterValue::FixedVec3([
                DecisionScalar::from_bits(0),
                DecisionScalar::from_bits(-65_536),
                DecisionScalar::from_bits(0),
            ]),
        );
        projection.parameters.insert(
            "maxDistance".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_integer(10).unwrap()),
        );
        projection
            .parameters
            .insert("provider".to_owned(), GraphParameterValue::U64(77));
        let mut transform = node(4, GraphOperator::Transform, 2);
        transform.spatial = NodeSpatialPolicy::Global { level: 2 };
        transform.seed_namespaces.insert("variation".to_owned(), 13);
        transform.parameters.insert(
            "orientToSurface".to_owned(),
            GraphParameterValue::Boolean(true),
        );
        let species = node(5, GraphOperator::SpeciesInput, 0);
        let mut direct_output = node(6, GraphOperator::MacroOutput, 0);
        direct_output
            .seed_namespaces
            .insert("species-selection".to_owned(), 17);
        let mut transformed_output = node(7, GraphOperator::MacroOutput, 0);
        transformed_output
            .seed_namespaces
            .insert("species-selection".to_owned(), 19);
        let document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![
                GraphInterfaceOutput {
                    id: 1006,
                    name: "direct".to_owned(),
                    domain: GraphDomain::MacroPoints,
                    node: 6,
                    pin: "points".to_owned(),
                    sink: Some(GraphSink::Macro),
                },
                GraphInterfaceOutput {
                    id: 1007,
                    name: "transformed".to_owned(),
                    domain: GraphDomain::MacroPoints,
                    node: 7,
                    pin: "points".to_owned(),
                    sink: Some(GraphSink::Macro),
                },
            ],
            nodes: vec![
                transformed_output,
                direct_output,
                species,
                transform,
                projection,
                coverage,
                regions,
            ],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "surface".to_owned(),
                    to_node: 4,
                    to_pin: "surface".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "candidates".to_owned(),
                    to_node: 7,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "species".to_owned(),
                    to_node: 6,
                    to_pin: "species".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "species".to_owned(),
                    to_node: 7,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut asset = fixture_asset(0);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &SurfaceDependencies { provider_hash },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_projection_output_demand_fixture(
        provider_hash: [u8; 32],
        surface_only: bool,
    ) -> CompiledBiomeGraph {
        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut projection = node(3, GraphOperator::SurfaceProjection, 0);
        projection.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_integer(1).unwrap(),
        };
        projection.parameters.insert(
            "direction".to_owned(),
            GraphParameterValue::FixedVec3([
                DecisionScalar::from_bits(0),
                DecisionScalar::from_bits(-65_536),
                DecisionScalar::from_bits(0),
            ]),
        );
        projection.parameters.insert(
            "maxDistance".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_integer(1).unwrap()),
        );
        projection
            .parameters
            .insert("provider".to_owned(), GraphParameterValue::U64(77));
        let species = node(4, GraphOperator::SpeciesInput, 0);
        let mut output = node(5, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let mut nodes = vec![output, species, projection, coverage, regions];
        let mut edges = vec![
            GraphEdge {
                from_node: 1,
                from_pin: "regions".to_owned(),
                to_node: 2,
                to_pin: "regions".to_owned(),
            },
            GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 3,
                to_pin: "candidates".to_owned(),
            },
            GraphEdge {
                from_node: 4,
                from_pin: "species".to_owned(),
                to_node: 5,
                to_pin: "species".to_owned(),
            },
        ];
        if surface_only {
            let mut transform = node(6, GraphOperator::Transform, 0);
            transform.seed_namespaces.insert("variation".to_owned(), 17);
            transform.parameters.insert(
                "orientToSurface".to_owned(),
                GraphParameterValue::Boolean(true),
            );
            nodes.push(transform);
            edges.extend([
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "surface".to_owned(),
                    to_node: 6,
                    to_pin: "surface".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
            ]);
        } else {
            edges.push(GraphEdge {
                from_node: 3,
                from_pin: "candidates".to_owned(),
                to_node: 5,
                to_pin: "candidates".to_owned(),
            });
        }
        let document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes,
            edges,
        };
        let mut asset = fixture_asset(0);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &SurfaceDependencies { provider_hash },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_document(document: BiomeGraphDocument) -> CompiledBiomeGraph {
        let mut asset = fixture_asset(0);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_surface_field_fixture(provider_hash: [u8; 32]) -> CompiledBiomeGraph {
        let level = 0;
        let region = node(1, GraphOperator::RegionInput, level);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, level);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let species = node(3, GraphOperator::SpeciesInput, level);
        let mut output = node(4, GraphOperator::MacroOutput, level);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let mut field = node(5, GraphOperator::FieldSample, level);
        field.parameters.insert(
            "channel".to_owned(),
            GraphParameterValue::FieldChannel(FieldChannel::Moisture),
        );
        field.parameters.insert(
            "derivative".to_owned(),
            GraphParameterValue::FieldDerivative(FieldDerivative::Value),
        );
        let mut importance = node(6, GraphOperator::FieldImportance, level);
        importance.parameters.insert(
            "threshold".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, importance, field, species, coverage, region],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "field".to_owned(),
                    to_node: 6,
                    to_pin: "weights".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut asset = fixture_asset(level);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &SurfaceDependencies { provider_hash },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_dead_surface_branch_fixture(
        provider_hash: [u8; 32],
        include_dead_branch: bool,
    ) -> CompiledBiomeGraph {
        let mut document = fixture_document(0);
        if include_dead_branch {
            let mut field = node(20, GraphOperator::FieldSample, 0);
            field.parameters.insert(
                "channel".to_owned(),
                GraphParameterValue::FieldChannel(FieldChannel::Moisture),
            );
            field.parameters.insert(
                "derivative".to_owned(),
                GraphParameterValue::FieldDerivative(FieldDerivative::Value),
            );
            document.nodes.push(field);
            document.edges.push(GraphEdge {
                from_node: 2,
                from_pin: "candidates".to_owned(),
                to_node: 20,
                to_pin: "candidates".to_owned(),
            });
        }
        let mut asset = fixture_asset(0);
        asset.graph = document.to_json();
        compile_biome_graph(
            &asset,
            &[],
            &SurfaceDependencies { provider_hash },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn explicit_anchor_document() -> BiomeGraphDocument {
        let mut anchors = node(1, GraphOperator::ExplicitAnchors, 0);
        anchors
            .parameters
            .insert("layer".to_owned(), GraphParameterValue::Guid(42));
        let species = node(2, GraphOperator::SpeciesInput, 0);
        let mut output = node(3, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1003,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 3,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, anchors],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "species".to_owned(),
                    to_node: 3,
                    to_pin: "species".to_owned(),
                },
            ],
        }
    }

    fn spline_document() -> BiomeGraphDocument {
        let splines = node(1, GraphOperator::SplineInput, 0);
        let mut follow = node(2, GraphOperator::SplineFollow, 0);
        follow.parameters.insert(
            "spacing".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_integer(1).unwrap()),
        );
        follow.parameters.insert(
            "edgeOffset".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(0)),
        );
        let species = node(3, GraphOperator::SpeciesInput, 0);
        let mut output = node(4, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 4,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, follow, splines],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "splines".to_owned(),
                    to_node: 2,
                    to_pin: "splines".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "species".to_owned(),
                    to_node: 4,
                    to_pin: "species".to_owned(),
                },
            ],
        }
    }

    fn recursive_document() -> BiomeGraphDocument {
        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(2));
        let mut recursive = node(3, GraphOperator::RecursiveCompanion, 0);
        recursive.spatial = NodeSpatialPolicy::Global { level: 0 };
        recursive
            .seed_namespaces
            .insert("companions".to_owned(), 17);
        recursive
            .parameters
            .insert("children".to_owned(), GraphParameterValue::U32(2));
        recursive
            .parameters
            .insert("maximumDepth".to_owned(), GraphParameterValue::U32(2));
        recursive.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(1)),
        );
        let species = node(4, GraphOperator::SpeciesInput, 0);
        let mut output = node(5, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, recursive, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "species".to_owned(),
                    to_node: 5,
                    to_pin: "species".to_owned(),
                },
            ],
        }
    }

    fn micro_document() -> BiomeGraphDocument {
        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(1));
        let mut output = node(3, GraphOperator::MicroOutput, 0);
        output
            .seed_namespaces
            .insert("reconstruction".to_owned(), 13);
        output.parameters.insert(
            "dimensions".to_owned(),
            GraphParameterValue::U32Vec3([4, 4, 4]),
        );
        output.parameters.insert(
            "attributeChannels".to_owned(),
            GraphParameterValue::GuidList(Vec::new()),
        );
        let communities = node(4, GraphOperator::CommunityInput, 0);
        let mut blend = node(5, GraphOperator::CommunityBlend, 0);
        blend.seed_namespaces.insert("community".to_owned(), 19);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1003,
                name: "micro".to_owned(),
                domain: GraphDomain::MicroField,
                node: 3,
                pin: "micro".to_owned(),
                sink: Some(GraphSink::Micro),
            }],
            nodes: vec![blend, communities, output, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "communities".to_owned(),
                    to_node: 5,
                    to_pin: "communities".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
            ],
        }
    }

    fn explicit_family_micro_document() -> BiomeGraphDocument {
        let mut anchors = node(1, GraphOperator::ExplicitAnchors, 0);
        anchors
            .parameters
            .insert("layer".to_owned(), GraphParameterValue::Guid(42));
        let mut output = node(2, GraphOperator::MicroOutput, 0);
        output
            .seed_namespaces
            .insert("reconstruction".to_owned(), 13);
        output.parameters.insert(
            "dimensions".to_owned(),
            GraphParameterValue::U32Vec3([2, 1, 1]),
        );
        output.parameters.insert(
            "attributeChannels".to_owned(),
            GraphParameterValue::GuidList(Vec::new()),
        );
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1002,
                name: "micro".to_owned(),
                domain: GraphDomain::MicroField,
                node: 2,
                pin: "micro".to_owned(),
                sink: Some(GraphSink::Micro),
            }],
            nodes: vec![output, anchors],
            edges: vec![GraphEdge {
                from_node: 1,
                from_pin: "candidates".to_owned(),
                to_node: 2,
                to_pin: "candidates".to_owned(),
            }],
        }
    }

    fn micro_attribute_document(channel: u128) -> BiomeGraphDocument {
        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(1));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut noise = node(3, GraphOperator::Noise, 0);
        noise.seed_namespaces.insert("noise".to_owned(), 13);
        noise.parameters.insert(
            "frequency".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise.parameters.insert(
            "amplitude".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
        );
        noise
            .parameters
            .insert("channel".to_owned(), GraphParameterValue::U32(7));
        let mut output = node(4, GraphOperator::MicroOutput, 0);
        output
            .seed_namespaces
            .insert("reconstruction".to_owned(), 17);
        output.parameters.insert(
            "dimensions".to_owned(),
            GraphParameterValue::U32Vec3([2, 2, 2]),
        );
        output.parameters.insert(
            "attributeChannels".to_owned(),
            GraphParameterValue::GuidList(vec![channel]),
        );
        let communities = node(5, GraphOperator::CommunityInput, 0);
        let mut blend = node(6, GraphOperator::CommunityBlend, 0);
        blend.seed_namespaces.insert("community".to_owned(), 19);
        BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 1004,
                name: "micro".to_owned(),
                domain: GraphDomain::MicroField,
                node: 4,
                pin: "micro".to_owned(),
                sink: Some(GraphSink::Micro),
            }],
            nodes: vec![blend, communities, output, noise, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 5,
                    from_pin: "communities".to_owned(),
                    to_node: 6,
                    to_pin: "communities".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "candidates".to_owned(),
                    to_node: 4,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "field".to_owned(),
                    to_node: 4,
                    to_pin: format!("attribute-{channel:032x}"),
                },
            ],
        }
    }

    struct OneModuleResolver {
        module: BiomeAsset,
    }

    impl BiomeGraphResolver for OneModuleResolver {
        fn resolve_biome(&self, id: Uuid) -> Result<BiomeAsset> {
            if id == self.module.id {
                Ok(self.module.clone())
            } else {
                Err(Error::GraphDocument {
                    path: "test.resolver".to_owned(),
                    reason: "unknown module".to_owned(),
                })
            }
        }

        fn resolve_dependency_hash(&self, source: GraphDependencySource) -> Result<[u8; 32]> {
            match source {
                GraphDependencySource::Asset(Uuid(702)) => Ok([7; 32]),
                _ => Ok([9; 32]),
            }
        }
    }

    fn compile_module_fixture() -> CompiledBiomeGraph {
        compile_module_fixture_with_dead_sibling(false)
    }

    fn compile_module_fixture_with_dead_sibling(include_dead_sibling: bool) -> CompiledBiomeGraph {
        let mut interface = node(10, GraphOperator::InterfaceInput, 0);
        interface.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("candidates".to_owned()),
        );
        let mut cluster = node(11, GraphOperator::ClusterPatchColony, 0);
        cluster.spatial = NodeSpatialPolicy::Partitioned {
            level: 0,
            influence_radius: DecisionScalar::from_bits(1),
        };
        cluster.seed_namespaces.insert("cluster".to_owned(), 17);
        cluster
            .parameters
            .insert("children".to_owned(), GraphParameterValue::U32(2));
        cluster.parameters.insert(
            "radius".to_owned(),
            GraphParameterValue::Fixed(DecisionScalar::from_bits(1)),
        );
        cluster.parameters.insert(
            "mode".to_owned(),
            GraphParameterValue::ClusterMode(GraphClusterMode::Cluster),
        );
        let mut module_outputs = vec![GraphInterfaceOutput {
            id: 2011,
            name: "candidates".to_owned(),
            domain: GraphDomain::Candidates,
            node: 11,
            pin: "candidates".to_owned(),
            sink: None,
        }];
        if include_dead_sibling {
            module_outputs.push(GraphInterfaceOutput {
                id: 2012,
                name: "unused-sibling-output-with-an-intentionally-long-name".to_owned(),
                domain: GraphDomain::Candidates,
                node: 10,
                pin: "value".to_owned(),
                sink: None,
            });
        }
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: vec![GraphInterfaceInput {
                id: 2010,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
            }],
            outputs: module_outputs,
            nodes: vec![cluster, interface],
            edges: vec![GraphEdge {
                from_node: 10,
                from_pin: "value".to_owned(),
                to_node: 11,
                to_pin: "candidates".to_owned(),
            }],
        };
        let mut module = fixture_asset(0);
        module.id = Uuid(880);
        module.role = BiomeRole::Module;
        module.graph = module_document.to_json();

        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(3));
        let mut call = node(3, GraphOperator::ModuleCall, 0);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let species = node(4, GraphOperator::SpeciesInput, 0);
        let mut output = node(5, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, call, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "species".to_owned(),
                    to_node: 5,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut root = fixture_asset(0);
        root.graph = root_document.to_json();
        root.modules = vec![BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        }];
        compile_biome_graph(
            &root,
            &[],
            &OneModuleResolver { module },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_module_global_prerequisite_fixture(resident_source: bool) -> CompiledBiomeGraph {
        let mut interface = node(10, GraphOperator::InterfaceInput, 0);
        interface.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("candidates".to_owned()),
        );
        let (module_nodes, module_edges, module_output_node) = if resident_source {
            let mut noise = node(11, GraphOperator::Noise, 0);
            noise.authority = GraphAuthority::EquivalentGpu;
            noise.spatial = NodeSpatialPolicy::Global { level: 0 };
            noise.seed_namespaces.insert("noise".to_owned(), 17);
            noise.parameters.insert(
                "frequency".to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(32_768)),
            );
            noise.parameters.insert(
                "amplitude".to_owned(),
                GraphParameterValue::Fixed(DecisionScalar::from_bits(65_536)),
            );
            noise
                .parameters
                .insert("channel".to_owned(), GraphParameterValue::U32(3));
            let mut importance = node(12, GraphOperator::FieldImportance, 0);
            importance.authority = GraphAuthority::EquivalentGpu;
            importance.spatial = NodeSpatialPolicy::Global { level: 0 };
            importance.parameters.insert(
                "threshold".to_owned(),
                GraphParameterValue::Unit(UnitInterval::ZERO),
            );
            (
                vec![importance, noise, interface],
                vec![
                    GraphEdge {
                        from_node: 10,
                        from_pin: "value".to_owned(),
                        to_node: 11,
                        to_pin: "candidates".to_owned(),
                    },
                    GraphEdge {
                        from_node: 10,
                        from_pin: "value".to_owned(),
                        to_node: 12,
                        to_pin: "candidates".to_owned(),
                    },
                    GraphEdge {
                        from_node: 11,
                        from_pin: "field".to_owned(),
                        to_node: 12,
                        to_pin: "weights".to_owned(),
                    },
                ],
                12,
            )
        } else {
            let mut transform = node(11, GraphOperator::Transform, 0);
            transform.spatial = NodeSpatialPolicy::Global { level: 0 };
            transform.seed_namespaces.insert("variation".to_owned(), 17);
            (
                vec![transform, interface],
                vec![GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 11,
                    to_pin: "candidates".to_owned(),
                }],
                11,
            )
        };
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: vec![GraphInterfaceInput {
                id: 2010,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
            }],
            outputs: vec![GraphInterfaceOutput {
                id: 2011,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
                node: module_output_node,
                pin: "candidates".to_owned(),
                sink: None,
            }],
            nodes: module_nodes,
            edges: module_edges,
        };
        let mut module = fixture_asset(0);
        module.id = Uuid(881);
        module.role = BiomeRole::Module;
        module.graph = module_document.to_json();

        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut call = node(3, GraphOperator::ModuleCall, 0);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let mut coarse = node(6, GraphOperator::Transform, 2);
        coarse.spatial = NodeSpatialPolicy::Global { level: 2 };
        coarse.seed_namespaces.insert("variation".to_owned(), 17);
        let species = node(4, GraphOperator::SpeciesInput, 0);
        let mut output = node(5, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, coarse, call, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 6,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 6,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "species".to_owned(),
                    to_node: 5,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut root = fixture_asset(0);
        root.graph = root_document.to_json();
        root.modules = vec![BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        }];
        compile_biome_graph(
            &root,
            &[],
            &OneModuleResolver { module },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_module_global_surface_fixture() -> CompiledBiomeGraph {
        let mut interface = node(10, GraphOperator::InterfaceInput, 0);
        interface.parameters.insert(
            "name".to_owned(),
            GraphParameterValue::String("candidates".to_owned()),
        );
        let mut field = node(11, GraphOperator::FieldSample, 0);
        field.spatial = NodeSpatialPolicy::Global { level: 0 };
        field.parameters.insert(
            "channel".to_owned(),
            GraphParameterValue::FieldChannel(FieldChannel::Moisture),
        );
        field.parameters.insert(
            "derivative".to_owned(),
            GraphParameterValue::FieldDerivative(FieldDerivative::Value),
        );
        let mut importance = node(12, GraphOperator::FieldImportance, 0);
        importance.spatial = NodeSpatialPolicy::Global { level: 0 };
        importance.parameters.insert(
            "threshold".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: vec![GraphInterfaceInput {
                id: 2010,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
            }],
            outputs: vec![GraphInterfaceOutput {
                id: 2012,
                name: "candidates".to_owned(),
                domain: GraphDomain::Candidates,
                node: 12,
                pin: "candidates".to_owned(),
                sink: None,
            }],
            nodes: vec![importance, field, interface],
            edges: vec![
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 11,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 10,
                    from_pin: "value".to_owned(),
                    to_node: 12,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 11,
                    from_pin: "field".to_owned(),
                    to_node: 12,
                    to_pin: "weights".to_owned(),
                },
            ],
        };
        let mut module = fixture_asset(0);
        module.id = Uuid(883);
        module.role = BiomeRole::Module;
        module.graph = module_document.to_json();

        let regions = node(1, GraphOperator::RegionInput, 0);
        let mut coverage = node(2, GraphOperator::StratifiedCoverage, 0);
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let mut call = node(3, GraphOperator::ModuleCall, 0);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let species = node(4, GraphOperator::SpeciesInput, 0);
        let mut output = node(5, GraphOperator::MacroOutput, 0);
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3005,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 5,
                pin: "points".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![output, species, call, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 1,
                    from_pin: "regions".to_owned(),
                    to_node: 2,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 2,
                    from_pin: "candidates".to_owned(),
                    to_node: 3,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 3,
                    from_pin: "candidates".to_owned(),
                    to_node: 5,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 4,
                    from_pin: "species".to_owned(),
                    to_node: 5,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut root = fixture_asset(0);
        root.graph = root_document.to_json();
        root.modules = vec![BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        }];
        compile_biome_graph(
            &root,
            &[],
            &OneModuleResolver { module },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn compile_stage_materialized_module_output_fixture() -> CompiledBiomeGraph {
        let regions = node(10, GraphOperator::RegionInput, 0);
        let mut coverage = node(11, GraphOperator::StratifiedCoverage, 0);
        coverage.spatial = NodeSpatialPolicy::Global { level: 2 };
        coverage.seed_namespaces.insert("sampling".to_owned(), 11);
        coverage
            .parameters
            .insert("count".to_owned(), GraphParameterValue::U32(4));
        coverage.parameters.insert(
            "jitter".to_owned(),
            GraphParameterValue::Unit(UnitInterval::ZERO),
        );
        let species = node(12, GraphOperator::SpeciesInput, 0);
        let unrelated = node(14, GraphOperator::RegionInput, 0);
        let mut output = node(13, GraphOperator::MacroOutput, 0);
        output.spatial = NodeSpatialPolicy::Global { level: 2 };
        output
            .seed_namespaces
            .insert("species-selection".to_owned(), 13);
        let module_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 2013,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 13,
                pin: "points".to_owned(),
                sink: None,
            }],
            nodes: vec![unrelated, output, species, coverage, regions],
            edges: vec![
                GraphEdge {
                    from_node: 10,
                    from_pin: "regions".to_owned(),
                    to_node: 11,
                    to_pin: "regions".to_owned(),
                },
                GraphEdge {
                    from_node: 11,
                    from_pin: "candidates".to_owned(),
                    to_node: 13,
                    to_pin: "candidates".to_owned(),
                },
                GraphEdge {
                    from_node: 12,
                    from_pin: "species".to_owned(),
                    to_node: 13,
                    to_pin: "species".to_owned(),
                },
            ],
        };
        let mut module = fixture_asset(0);
        module.id = Uuid(882);
        module.role = BiomeRole::Module;
        module.graph = module_document.to_json();

        let mut call = node(3, GraphOperator::ModuleCall, 0);
        call.parameters
            .insert("callGuid".to_owned(), GraphParameterValue::Guid(99));
        let root_document = BiomeGraphDocument {
            version: BIOME_GRAPH_VERSION,
            interface_version: crate::BIOME_INTERFACE_VERSION,
            inputs: Vec::new(),
            outputs: vec![GraphInterfaceOutput {
                id: 3003,
                name: "macro".to_owned(),
                domain: GraphDomain::MacroPoints,
                node: 3,
                pin: "macro".to_owned(),
                sink: Some(GraphSink::Macro),
            }],
            nodes: vec![call],
            edges: Vec::new(),
        };
        let mut root = fixture_asset(0);
        root.graph = root_document.to_json();
        root.modules = vec![BiomeModuleReference {
            biome: module.id,
            call_guid: 99,
            bindings: Vec::new(),
        }];
        compile_biome_graph(
            &root,
            &[],
            &OneModuleResolver { module },
            GraphCompileOptions::canonical(),
        )
        .unwrap()
    }

    fn explicit_point(candidate: u64, layer: u128, position: WorldPosition) -> EvaluationAnchor {
        let ticks = position.global_ticks();
        EvaluationAnchor {
            layer,
            point: PlantPoint {
                id: PlantId::explicit([candidate as u8 + 1; 16]).unwrap(),
                owner: position.cell(),
                position,
                orientation: QuantizedOrientation::identity(),
                scale: [DecisionScalar::from_integer(1).unwrap(); 3],
                bounds: WorldBounds::new(
                    [ticks[0] - 1, ticks[1] - 1, ticks[2] - 1],
                    [ticks[0] + 2, ticks[1] + 2, ticks[2] + 2],
                )
                .unwrap(),
                family: Uuid(702),
                variation: 0,
                lifecycle: PlantLifecycle::Mature,
                phenotype: 0,
                representation_class: 0,
                deterministic_key: u128::from(candidate),
                candidate,
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
            },
        }
    }

    fn input(cell: WorldCellKey, halo: DecisionScalar) -> GraphEvaluationInputs {
        let mut input = GraphEvaluationInputs::for_cell(Uuid(801), 91, cell, halo).unwrap();
        input.plant_prototypes.push(PlantPrototype {
            family: Uuid(702),
            crown_radius: [DecisionScalar::from_bits(65_536); 2],
            root_radius: [DecisionScalar::from_bits(65_536); 2],
            local_bounds_min: [
                DecisionScalar::from_bits(-65_536),
                DecisionScalar::from_bits(0),
                DecisionScalar::from_bits(-65_536),
            ],
            local_bounds_max: [
                DecisionScalar::from_bits(65_536),
                DecisionScalar::from_bits(4 * 65_536),
                DecisionScalar::from_bits(65_536),
            ],
            shade_tolerance: UnitInterval::from_bits(32_768),
        });
        input
    }

    fn projection_provider() -> (Arc<dyn SurfaceField>, [u8; 32]) {
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: WorldCellKey::new(0, 0, 0, 2).unwrap().bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                project: true,
                authoritative_attachments: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: None,
            project_hits: true,
            successful_samples: Arc::new(AtomicUsize::new(0)),
        });
        let provider_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        (provider, provider_hash)
    }

    fn projection_job(
        graph: &CompiledBiomeGraph,
        provider: Arc<dyn SurfaceField>,
        provider_hash: [u8; 32],
    ) -> GraphEvaluationJobInputs {
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs.surface_provider_set_hash = provider_hash;
        inputs.surface_providers.push(provider);
        job(vec![inputs])
    }

    fn job(cells: Vec<GraphEvaluationInputs>) -> GraphEvaluationJobInputs {
        GraphEvaluationJobInputs {
            cells,
            global_stages: Vec::new(),
        }
    }

    fn assert_graph_limit(error: Error, resource: &'static str) {
        assert!(
            matches!(error, Error::GraphLimit { resource: actual, .. } if actual == resource),
            "expected {resource} limit, got {error:?}"
        );
    }

    fn global_job(
        graph: &CompiledBiomeGraph,
        cells: Vec<GraphEvaluationInputs>,
    ) -> GraphEvaluationJobInputs {
        let global_stages = expected_global_stage_tiles(graph, &cells)
            .unwrap()
            .into_iter()
            .map(|(stage_id, owner)| {
                let stage = graph.spatial_plan().global_stage(stage_id).unwrap();
                let mut inputs = input(owner, stage.upstream_halo);
                inputs.output_bounds = owner.bounds();
                inputs.read_bounds = expand_bounds_checked(
                    owner.bounds(),
                    fixed_meters_to_ticks(stage.upstream_halo)
                        .unwrap()
                        .unsigned_abs() as i128,
                )
                .unwrap();
                inputs.regions =
                    canonical_cell_regions(inputs.read_bounds, stage.minimum_input_level, 0)
                        .unwrap();
                let mut snapshot = b"global-stage-test-input/v1\0".to_vec();
                snapshot.extend_from_slice(&stage_id);
                snapshot.extend_from_slice(&owner.canonical_bytes());
                GlobalStageEvaluationInputs {
                    stage: stage_id,
                    owner,
                    solve_bounds: owner.bounds(),
                    input_snapshot: sha256(&snapshot),
                    inputs,
                }
            })
            .collect();
        GraphEvaluationJobInputs {
            cells,
            global_stages,
        }
    }

    fn position(columns: &PlantPointColumns, row: usize) -> WorldPosition {
        columns.positions[row]
    }

    #[test]
    fn reference_is_stable_under_input_order_origin_and_repetition() {
        let graph = compile_fixture(0);
        let halo = graph.required_halo(0);
        let mut original = input(WorldCellKey::base(0, 0, 0), halo);
        let baseline_result =
            evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default()).unwrap();
        let plant = baseline_result.macro_points.ids[0];
        let explanation = baseline_result.explain_plant(plant).unwrap();
        assert_eq!(explanation.record.plant, Some(plant));
        assert_eq!(explanation.record.family, Some(Uuid(702)));
        assert_eq!(
            explanation
                .decisions
                .last()
                .map(|(_, decision)| decision.outcome),
            Some(ProvenanceDecisionOutcome::Accepted)
        );
        let baseline = baseline_result.canonical_bytes().unwrap();

        original.regions.reverse();
        original.render_origin =
            WorldPosition::from_global_ticks([9_000_000_000, -2_000_000_000, 4_000_000_000])
                .unwrap();
        let shuffled =
            evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default())
                .unwrap()
                .canonical_bytes()
                .unwrap();
        let repeated =
            evaluate_cell_reference(&graph, &original, &GraphCancellationToken::default())
                .unwrap()
                .canonical_bytes()
                .unwrap();
        assert_eq!(baseline, shuffled);
        assert_eq!(baseline, repeated);
    }

    #[test]
    fn symbolic_bound_is_above_the_actual_reference_evaluation() {
        let graph = compile_fixture(0);
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let plan = build_execution_plan(&graph, false, None).unwrap();
        let global_store = SymbolicGlobalStore::default();
        let cancellation = GraphCancellationToken::default();
        let guard = PreflightGuard {
            cancellation: &cancellation,
            deadline: evaluation_deadline(&graph).unwrap(),
            time_limit_ms: graph.limits.max_time_ms,
        };
        let bound = symbolic_evaluation_bound(
            &graph,
            &inputs,
            &plan,
            SymbolicEvaluationScope::Cell {
                global_store: &global_store,
            },
            guard,
        )
        .unwrap()
        .bound;
        let actual =
            evaluate_cell_reference(&graph, &inputs, &GraphCancellationToken::default()).unwrap();
        let actual_node_bytes = actual
            .diagnostics
            .nodes
            .iter()
            .map(|node| node.output_bytes)
            .sum::<u64>();
        let canonical_bytes = actual.canonical_bytes().unwrap();
        let mut hasher = VegetationContentHasher::new();
        actual.update_content_hasher(&mut hasher).unwrap();

        assert_eq!(actual.canonical_byte_len().unwrap(), canonical_bytes.len());
        assert_eq!(hasher.finalize().unwrap(), sha256(&canonical_bytes));
        assert!(bound.candidate_peak >= actual.diagnostics.candidate_count);
        assert!(bound.accepted >= actual.macro_points.row_count().unwrap() as u64);
        assert!(bound.memory_bytes >= actual_node_bytes);
        assert_eq!(bound.rejected, bound.candidate_peak * 3);
        assert!(bound.rejected >= actual.diagnostics.rejected.len() as u64);
        assert_eq!(bound.transfer_bytes, 0);
    }

    #[test]
    fn public_preflight_matches_anchor_spline_recursive_and_module_bounds() {
        let cancellation = GraphCancellationToken::default();

        let graph = Arc::new(compile_document(explicit_anchor_document()));
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs.anchors = vec![
            explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
            explicit_point(
                1,
                42,
                WorldPosition::from_global_ticks([i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                    .unwrap(),
            ),
            explicit_point(
                2,
                7,
                WorldPosition::from_global_ticks([2 * i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                    .unwrap(),
            ),
        ];
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 4).unwrap();
        let anchor_job = job(vec![inputs]);
        let bound = evaluator.preflight(&anchor_job, &cancellation).unwrap();
        let actual = evaluator.evaluate(anchor_job, &cancellation).unwrap();
        assert_eq!((bound.candidate_count, bound.accepted_count), (2, 2));
        assert_eq!(bound.worker_count, 1);
        assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 2);

        let graph = Arc::new(compile_document(spline_document()));
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs.splines.push(EvaluationSpline {
            id: 1,
            layer: 1,
            points: vec![
                WorldPosition::from_global_ticks([0, 0, 0]).unwrap(),
                WorldPosition::from_global_ticks([10 * i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                    .unwrap(),
            ],
        });
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        let spline_job = job(vec![inputs]);
        let bound = evaluator.preflight(&spline_job, &cancellation).unwrap();
        let actual = evaluator.evaluate(spline_job, &cancellation).unwrap();
        assert_eq!((bound.candidate_count, bound.accepted_count), (11, 11));
        assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 11);

        let graph = Arc::new(compile_document(recursive_document()));
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs
            .set_hierarchical_region(71, inputs.output_bounds)
            .unwrap();
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        let recursive_job = global_job(&graph, vec![inputs]);
        let bound = evaluator.preflight(&recursive_job, &cancellation).unwrap();
        let actual = evaluator.evaluate(recursive_job, &cancellation).unwrap();
        assert_eq!((bound.candidate_count, bound.accepted_count), (28, 14));
        assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 14);

        let graph = Arc::new(compile_module_fixture());
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs
            .set_hierarchical_region(72, inputs.output_bounds)
            .unwrap();
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        let module_job = job(vec![inputs]);
        let bound = evaluator.preflight(&module_job, &cancellation).unwrap();
        let actual = evaluator.evaluate(module_job, &cancellation).unwrap();
        assert_eq!((bound.candidate_count, bound.accepted_count), (9, 9));
        assert_eq!(actual.cells[0].macro_points.row_count().unwrap(), 9);
    }

    #[test]
    fn module_output_demand_reaches_nested_earlier_global_stage_sources() {
        let graph = Arc::new(compile_module_global_prerequisite_fixture(false));
        let stages = graph.spatial_plan().global_stages();
        assert_eq!(stages.len(), 2);
        assert_eq!((stages[0].owner_level, stages[1].owner_level), (0, 2));
        assert!(stages[1].input_pins.contains(&QualifiedGraphPin {
            node: GraphNodeAddress {
                module_path: vec![99],
                node: 11,
            },
            pin: "candidates".to_owned(),
        }));

        let inputs = global_job(
            &graph,
            vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
        );
        let cancellation = GraphCancellationToken::default();
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        evaluator.preflight(&inputs, &cancellation).unwrap();
        let result = evaluator.evaluate(inputs, &cancellation).unwrap();
        let later_stage = result
            .global_stages
            .iter()
            .find(|tile| tile.stage == stages[1].id)
            .unwrap();
        assert!(later_stage.result.diagnostics.nodes.iter().any(|node| {
            node.module_path.is_empty()
                && node.node == 6
                && node.operator == GraphOperator::Transform
        }));
        assert!(!later_stage.result.diagnostics.nodes.iter().any(|node| {
            node.module_path == [99] && node.node == 11 && node.operator == GraphOperator::Transform
        }));
    }

    #[test]
    fn global_module_load_boundary_does_not_request_cell_preparation() {
        let graph = compile_module_global_surface_fixture();
        assert!(
            demand_requires_canonical_preparation(
                &graph.root,
                graph.demand_plan().execution_slice(),
            )
            .unwrap()
        );
        assert!(
            !demand_requires_canonical_preparation(
                &graph.root,
                graph.demand_plan().public_slice(),
            )
            .unwrap()
        );
        let stage = &graph.spatial_plan().global_stages()[0];
        assert!(
            demand_requires_canonical_preparation(
                &graph.root,
                graph.demand_plan().stage_slice(stage.id).unwrap(),
            )
            .unwrap()
        );
    }

    #[test]
    fn split_scope_global_loads_only_the_demanded_surface_projection_pin() {
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: WorldCellKey::new(0, 0, 0, 2).unwrap().bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                project: true,
                authoritative_attachments: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: None,
            project_hits: false,
            successful_samples: Arc::new(AtomicUsize::new(0)),
        });
        let provider_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        let graph = compile_split_surface_projection_fixture(provider_hash);
        let projection = GraphNodeAddress {
            module_path: Vec::new(),
            node: 3,
        };
        let transform = GraphNodeAddress {
            module_path: Vec::new(),
            node: 4,
        };
        let projection_stage = graph
            .spatial_plan()
            .global_stage_for_node(&projection)
            .unwrap();
        let transform_stage = graph
            .spatial_plan()
            .global_stage_for_node(&transform)
            .unwrap();
        assert_ne!(projection_stage.id, transform_stage.id);
        let projection_candidates = QualifiedGraphPin {
            node: projection.clone(),
            pin: "candidates".to_owned(),
        };
        let projection_surface = QualifiedGraphPin {
            node: projection,
            pin: "surface".to_owned(),
        };
        assert!(
            graph
                .demand_plan()
                .public_slice()
                .output_pins
                .contains(&projection_candidates)
        );
        assert!(
            !graph
                .demand_plan()
                .public_slice()
                .output_pins
                .contains(&projection_surface)
        );
        let later_demand = graph.demand_plan().stage_slice(transform_stage.id).unwrap();
        assert!(later_demand.output_pins.contains(&projection_candidates));
        assert!(later_demand.output_pins.contains(&projection_surface));

        let cell = WorldCellKey::base(0, 0, 0);
        let mut cell_input = input(cell, graph.required_halo(0));
        cell_input.surface_provider_set_hash = provider_hash;
        let mut inputs = global_job(&graph, vec![cell_input]);
        for stage_input in &mut inputs.global_stages {
            stage_input.inputs.surface_provider_set_hash = provider_hash;
            if stage_input.stage == projection_stage.id {
                stage_input
                    .inputs
                    .surface_providers
                    .push(Arc::clone(&provider));
            }
        }
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap();
        let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        assert!(result.cells[0].surface_projection_tiles.is_empty());
        assert!(
            result
                .global_stages
                .iter()
                .filter(|tile| tile.stage == transform_stage.id)
                .all(|tile| tile
                    .result
                    .diagnostics
                    .nodes
                    .iter()
                    .any(|node| { node.node == 4 && node.operator == GraphOperator::Transform }))
        );

        let mut exact_graph = graph.clone();
        exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
        BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        let mut rejected_graph = graph;
        rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "memory bytes",
                requested,
                limit,
            } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
        ));
    }

    #[test]
    fn surface_projection_candidates_only_materializes_exact_demand() {
        let (provider, provider_hash) = projection_provider();
        let graph = compile_projection_output_demand_fixture(provider_hash, false);
        let projection = graph
            .root
            .nodes
            .iter()
            .find(|node| node.definition.guid == 3)
            .unwrap();
        let demand = NodeOutputDemand::new(graph.demand_plan().public_slice(), projection);
        assert!(demand.contains("candidates"));
        assert!(!demand.contains("surface"));

        let inputs = projection_job(&graph, provider, provider_hash);
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap();
        let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        let cell = &result.cells[0];
        assert_eq!(cell.macro_points.row_count().unwrap(), 4);
        assert_eq!(cell.surface_projection_tiles.len(), 1);
        let diagnostic = cell
            .diagnostics
            .nodes
            .iter()
            .find(|diagnostic| diagnostic.node == 3)
            .unwrap();
        assert_eq!(diagnostic.output_candidates, 108);
        assert_eq!(
            diagnostic.output_bytes,
            requested_vec_bytes::<GraphCandidate>(
                usize::try_from(diagnostic.output_candidates).unwrap()
            )
            .unwrap()
        );
        let explanation = cell.explain_plant(cell.macro_points.ids[0]).unwrap();
        assert!(explanation.decisions.iter().any(|(_, decision)| {
            decision.node == 3 && decision.operator == GraphOperator::SurfaceProjection
        }));

        let mut exact_graph = graph.clone();
        exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
        BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        let mut rejected_graph = graph;
        rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "memory bytes",
                requested,
                limit,
            } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
        ));
    }

    #[test]
    fn surface_projection_surface_only_materializes_exact_demand() {
        let (provider, provider_hash) = projection_provider();
        let graph = compile_projection_output_demand_fixture(provider_hash, true);
        let projection = graph
            .root
            .nodes
            .iter()
            .find(|node| node.definition.guid == 3)
            .unwrap();
        let demand = NodeOutputDemand::new(graph.demand_plan().public_slice(), projection);
        assert!(!demand.contains("candidates"));
        assert!(demand.contains("surface"));

        let inputs = projection_job(&graph, provider, provider_hash);
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap();
        let result = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        let cell = &result.cells[0];
        assert_eq!(cell.macro_points.row_count().unwrap(), 4);
        assert_eq!(cell.surface_projection_tiles.len(), 1);
        let diagnostic = cell
            .diagnostics
            .nodes
            .iter()
            .find(|diagnostic| diagnostic.node == 3)
            .unwrap();
        assert_eq!(diagnostic.output_candidates, 108);
        assert_eq!(
            diagnostic.output_bytes,
            requested_btree_bytes::<CandidateIdentity, ProjectedSurfaceSample>(
                usize::try_from(diagnostic.output_candidates).unwrap()
            )
            .unwrap()
        );
        let explanation = cell.explain_plant(cell.macro_points.ids[0]).unwrap();
        assert!(explanation.decisions.iter().any(|(_, decision)| {
            decision.node == 3 && decision.operator == GraphOperator::SurfaceProjection
        }));

        let mut exact_graph = graph.clone();
        exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
        BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
            .unwrap()
            .evaluate(inputs.clone(), &cancellation)
            .unwrap();
        let mut rejected_graph = graph;
        rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "memory bytes",
                requested,
                limit,
            } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
        ));
    }

    #[test]
    fn micro_output_replays_its_live_dynamic_attribute_channel() {
        let channel = 77_u128;
        let graph = Arc::new(compile_document(micro_attribute_document(channel)));
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let tile = &result.micro_fields[0];
        assert_eq!(tile.density.len(), 8);
        assert_eq!(tile.attributes[&channel].len(), 8);
        assert!(result.diagnostics.nodes.iter().any(|node| {
            node.node == 3 && node.operator == GraphOperator::Noise && node.output_bytes > 0
        }));
    }

    #[test]
    fn micro_output_emits_one_canonical_tile_per_assigned_family() {
        let graph = Arc::new(compile_document(explicit_family_micro_document()));
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let mut second =
            explicit_point(1, 42, WorldPosition::from_global_ticks([1, 0, 0]).unwrap());
        second.point.family = Uuid(703);
        inputs.anchors = vec![
            explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
            second,
        ];

        let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap()
            .cells
            .pop()
            .unwrap();

        assert_eq!(
            result
                .micro_fields
                .iter()
                .map(|tile| tile.family.value())
                .collect::<Vec<_>>(),
            vec![702, 703]
        );
        assert_ne!(
            result.micro_fields[0].reconstruction_seed,
            result.micro_fields[1].reconstruction_seed
        );
        assert!(result.canonical_bytes().is_ok());
    }

    #[test]
    fn stage_materialized_module_output_is_demanded_without_a_consumer_edge() {
        let graph = Arc::new(compile_stage_materialized_module_output_fixture());
        assert!(graph.root.edges.is_empty());
        let stages = graph.spatial_plan().global_stages();
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].owner_level, 2);
        assert_eq!(
            stages[0].nodes.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 11,
                },
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 13,
                },
            ])
        );
        assert!(stages[0].output_pins.contains(&QualifiedGraphPin {
            node: GraphNodeAddress {
                module_path: vec![99],
                node: 13,
            },
            pin: "points".to_owned(),
        }));

        let inputs = global_job(
            &graph,
            vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
        );
        let cancellation = GraphCancellationToken::default();
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        evaluator.preflight(&inputs, &cancellation).unwrap();
        let result = evaluator.evaluate(inputs, &cancellation).unwrap();
        assert!(result.global_stages.iter().all(|tile| {
            tile.result
                .diagnostics
                .nodes
                .iter()
                .any(|node| node.module_path == [99] && node.node == 13)
        }));
        assert!(result.global_stages.iter().all(|tile| {
            tile.result
                .diagnostics
                .nodes
                .iter()
                .all(|node| !(node.module_path == [99] && node.node == 14))
        }));
        assert!(
            result
                .global_stages
                .iter()
                .all(|tile| tile.resident_bytes > 0)
        );
        assert!(
            result.cells[0]
                .diagnostics
                .nodes
                .iter()
                .all(|node| node.module_path != [99])
        );
    }

    #[test]
    fn dead_module_output_sibling_has_zero_bytes_and_preserves_the_exact_cap() {
        let baseline_graph = compile_module_fixture_with_dead_sibling(false);
        let sibling_graph = compile_module_fixture_with_dead_sibling(true);
        let baseline_inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            baseline_graph.required_halo(0),
        )]);
        let sibling_inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            sibling_graph.required_halo(0),
        )]);
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::new(baseline_graph), 1)
            .unwrap()
            .preflight(&baseline_inputs, &cancellation)
            .unwrap();
        let sibling = BiomeGraphEvaluator::new(Arc::new(sibling_graph.clone()), 1)
            .unwrap()
            .preflight(&sibling_inputs, &cancellation)
            .unwrap();
        assert_eq!(sibling, baseline);

        let mut exact_graph = sibling_graph.clone();
        exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
        let exact = BiomeGraphEvaluator::new(Arc::new(exact_graph), 1).unwrap();
        assert_eq!(
            exact
                .evaluate(sibling_inputs.clone(), &cancellation)
                .unwrap()
                .cells[0]
                .macro_points
                .row_count()
                .unwrap(),
            9
        );

        let mut rejected_graph = sibling_graph;
        rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
            .unwrap()
            .preflight(&sibling_inputs, &cancellation)
            .unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "memory bytes",
                requested,
                limit,
            } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
        ));
    }

    #[test]
    fn resident_earlier_module_stage_is_loaded_without_redispatch() {
        let graph = Arc::new(compile_module_global_prerequisite_fixture(true));
        let stages = graph.spatial_plan().global_stages();
        assert_eq!(stages.len(), 2);
        assert_eq!((stages[0].owner_level, stages[1].owner_level), (0, 2));
        assert_eq!(
            stages[0].nodes.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 11,
                },
                GraphNodeAddress {
                    module_path: vec![99],
                    node: 12,
                },
            ])
        );

        let inputs = global_job(
            &graph,
            vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
        );
        let earlier_stage_tiles = inputs
            .global_stages
            .iter()
            .filter(|tile| tile.stage == stages[0].id)
            .count();
        let dispatches = Arc::new(AtomicUsize::new(0));
        let cancellation = GraphCancellationToken::default();
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .with_compute_executor(counting_compute(Arc::clone(&dispatches)));
        evaluator.preflight(&inputs, &cancellation).unwrap();
        let result = evaluator.evaluate(inputs, &cancellation).unwrap();

        assert_eq!(dispatches.load(Ordering::SeqCst), earlier_stage_tiles);
        assert!(
            result
                .global_stages
                .iter()
                .filter(|tile| tile.stage == stages[0].id)
                .all(|tile| tile.result.diagnostics.gpu_groups.len() == 1)
        );
        assert!(
            result
                .global_stages
                .iter()
                .filter(|tile| tile.stage == stages[1].id)
                .all(|tile| tile.result.diagnostics.gpu_groups.is_empty())
        );
        assert!(
            result
                .cells
                .iter()
                .all(|cell| cell.diagnostics.gpu_groups.is_empty())
        );
    }

    #[test]
    fn public_preflight_global_work_is_aggregated_once_per_owner() {
        let graph = Arc::new(compile_global_fixture());
        let halo = graph.required_halo(0);
        let first = input(WorldCellKey::base(0, 0, 0), halo);
        let second = input(WorldCellKey::base(1, 0, 0), halo);
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 2).unwrap();
        let cancellation = GraphCancellationToken::default();
        let first_bound = evaluator
            .preflight(&global_job(&graph, vec![first.clone()]), &cancellation)
            .unwrap();
        let second_bound = evaluator
            .preflight(&global_job(&graph, vec![second.clone()]), &cancellation)
            .unwrap();
        let combined_job = global_job(&graph, vec![first, second]);
        let combined_bound = evaluator.preflight(&combined_job, &cancellation).unwrap();
        let actual = evaluator.evaluate(combined_job, &cancellation).unwrap();

        assert_eq!(
            combined_bound.global_stage_tiles as usize,
            actual.global_stages.len()
        );
        assert!(
            combined_bound.global_stage_tiles
                < first_bound.global_stage_tiles + second_bound.global_stage_tiles
        );
        assert!(
            combined_bound.candidate_count
                < first_bound.candidate_count + second_bound.candidate_count
        );
    }

    #[test]
    fn public_preflight_bounds_complete_retained_job_results() {
        let graph = Arc::new(compile_global_fixture());
        let halo = graph.required_halo(0);
        let inputs = global_job(
            &graph,
            vec![
                input(WorldCellKey::base(0, 0, 0), halo),
                input(WorldCellKey::base(1, 0, 0), halo),
            ],
        );
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 2).unwrap();
        let cancellation = GraphCancellationToken::default();
        let bound = evaluator.preflight(&inputs, &cancellation).unwrap();
        let actual = evaluator.evaluate(inputs, &cancellation).unwrap();
        let result_bytes = actual
            .cells
            .iter()
            .map(|result| result.canonical_bytes().unwrap().len() as u64)
            .chain(actual.global_stages.iter().map(|tile| {
                tile.resident_bytes + tile.result.canonical_bytes().unwrap().len() as u64
            }))
            .sum::<u64>();
        let candidates = actual
            .cells
            .iter()
            .map(|result| result.diagnostics.candidate_count)
            .chain(
                actual
                    .global_stages
                    .iter()
                    .map(|tile| tile.result.diagnostics.candidate_count),
            )
            .sum::<u64>();
        assert!(bound.memory_bytes >= bound.retained_input_bytes + result_bytes);
        assert!(bound.candidate_count >= candidates);
    }

    #[test]
    fn public_preflight_enforces_every_job_resource_cap() {
        let cancellation = GraphCancellationToken::default();

        let mut graph = compile_fixture(0);
        graph.limits.max_workers = 1;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 2).err().unwrap();
        assert_graph_limit(error, "worker count");

        let mut graph = compile_fixture(0);
        let halo = graph.required_halo(0);
        let inputs = job(vec![
            input(WorldCellKey::base(0, 0, 0), halo),
            input(WorldCellKey::base(1, 0, 0), halo),
        ]);
        graph.limits.max_output_cells = 1;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "output cells");

        let mut graph = compile_document(recursive_document());
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs
            .set_hierarchical_region(81, inputs.output_bounds)
            .unwrap();
        graph.limits.max_candidates = 13;
        let inputs = global_job(&graph, vec![inputs]);
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "candidate count");

        let mut graph = compile_document(explicit_anchor_document());
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        inputs.anchors = vec![
            explicit_point(0, 42, WorldPosition::from_global_ticks([0, 0, 0]).unwrap()),
            explicit_point(
                1,
                42,
                WorldPosition::from_global_ticks([i128::from(LOCAL_TICKS_PER_METER), 0, 0])
                    .unwrap(),
            ),
        ];
        graph.limits.max_macro_points = 1;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&job(vec![inputs]), &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "accepted count");

        let mut graph = compile_document(micro_document());
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        graph.limits.max_micro_samples = 63;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&job(vec![inputs]), &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "micro samples");

        let graph = Arc::new(compile_fixture(0));
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        let memory = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap()
            .memory_bytes;
        let mut limited = (*graph).clone();
        limited.limits.max_memory_bytes = memory - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(limited), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "memory bytes");

        let graph = Arc::new(compile_resident_branch_fixture());
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        let compute = reference_compute();
        let transfer = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .with_compute_executor(Arc::clone(&compute))
            .preflight(&inputs, &cancellation)
            .unwrap()
            .transfer_bytes;
        assert!(transfer > 0);
        let mut limited = (*graph).clone();
        limited.limits.max_transfer_bytes = transfer - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(limited), 1)
            .unwrap()
            .with_compute_executor(compute)
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "transfer bytes");

        let mut graph = compile_fixture(0);
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        graph.limits.max_time_ms = 0;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "time milliseconds");

        let mut graph = compile_fixture(0);
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        graph.limits.max_input_tiles = 0;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "input tiles");

        let mut graph = compile_global_fixture();
        let inputs = global_job(
            &graph,
            vec![input(WorldCellKey::base(0, 0, 0), graph.required_halo(0))],
        );
        graph.limits.max_global_stage_tiles = inputs.global_stages.len() as u64 - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "global stage tiles");
    }

    #[test]
    fn ancestor_reference_memory_is_input_specific_and_keeps_the_exact_gate() {
        let graph = compile_fixture(0);
        assert_eq!(graph.limits.max_global_stage_tiles, 1_000_000);
        let cell = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        assert_eq!(
            ancestor_reference_upper_bound(&graph, &cell, true).unwrap(),
            0
        );
        let inputs = job(vec![cell]);
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::new(graph.clone()), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap();

        let mut no_global_tiles = graph;
        no_global_tiles.limits.max_global_stage_tiles = 0;
        let uncharged = BiomeGraphEvaluator::new(Arc::new(no_global_tiles.clone()), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap();
        assert_eq!(uncharged.memory_bytes, baseline.memory_bytes);

        let mut exact = no_global_tiles.clone();
        exact.limits.max_memory_bytes = uncharged.memory_bytes;
        assert!(
            BiomeGraphEvaluator::new(Arc::new(exact), 1)
                .unwrap()
                .preflight(&inputs, &cancellation)
                .is_ok()
        );

        no_global_tiles.limits.max_memory_bytes = uncharged.memory_bytes - 1;
        let error = BiomeGraphEvaluator::new(Arc::new(no_global_tiles), 1)
            .unwrap()
            .preflight(&inputs, &cancellation)
            .unwrap_err();
        assert_graph_limit(error, "memory bytes");
    }

    #[test]
    fn ancestor_reference_bound_deduplicates_same_level_macro_stages() {
        let graph = compile_same_level_macro_stages_fixture();
        let stages = graph.spatial_plan().global_stages();
        assert_eq!(stages.len(), 2);
        assert!(
            stages
                .iter()
                .all(|stage| global_stage_has_macro_output(&graph, stage).unwrap())
        );
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let covered = world_cell_count_covering_bounds(
            inputs.read_bounds,
            stages[0].owner_level,
            graph.limits.max_global_stage_tiles,
        )
        .unwrap();
        assert_eq!(
            ancestor_reference_upper_bound(&graph, &inputs, true).unwrap(),
            covered
        );
    }

    #[test]
    fn candidate_global_stage_charges_one_coarse_owner_and_global_scope_charges_no_imports() {
        let candidate_graph = compile_candidate_only_global_fixture();
        let candidate_stage = &candidate_graph.spatial_plan().global_stages()[0];
        assert!(!global_stage_has_macro_output(&candidate_graph, candidate_stage).unwrap());
        let mut cell = input(
            WorldCellKey::base(0, 0, 0),
            candidate_graph.required_halo(0),
        );
        let edge = i128::from(BASE_CELL_TICKS);
        cell.read_bounds = WorldBounds::new([-1; 3], [edge + 1; 3]).unwrap();
        assert!(
            world_cell_count_covering_bounds(
                cell.read_bounds,
                candidate_stage.owner_level,
                candidate_graph.limits.max_global_stage_tiles,
            )
            .unwrap()
                > 1
        );
        assert_eq!(
            ancestor_reference_upper_bound(&candidate_graph, &cell, true).unwrap(),
            1
        );

        let macro_graph = compile_global_fixture();
        let macro_job = global_job(
            &macro_graph,
            vec![input(
                WorldCellKey::base(0, 0, 0),
                macro_graph.required_halo(0),
            )],
        );
        for stage in &macro_job.global_stages {
            assert_eq!(
                ancestor_reference_upper_bound(&macro_graph, &stage.inputs, false).unwrap(),
                0
            );
        }
    }

    #[test]
    fn memory_peak_boundary_is_exact_and_rejects_before_gpu_dispatch() {
        let graph = Arc::new(compile_resident_branch_fixture());
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        let cancellation = GraphCancellationToken::default();
        let baseline = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .with_compute_executor(reference_compute())
            .preflight(&inputs, &cancellation)
            .unwrap();
        assert_eq!(
            baseline.memory_bytes,
            baseline
                .preflight_peak_bytes
                .max(baseline.execution_peak_bytes)
        );
        assert!(baseline.preflight_peak_bytes > 0);
        assert!(baseline.execution_peak_bytes > 0);

        let mut exact_graph = (*graph).clone();
        exact_graph.limits.max_memory_bytes = baseline.memory_bytes;
        let exact_dispatches = Arc::new(AtomicUsize::new(0));
        let exact = BiomeGraphEvaluator::new(Arc::new(exact_graph), 1)
            .unwrap()
            .with_compute_executor(counting_compute(Arc::clone(&exact_dispatches)));
        let exact_preflight = exact.preflight(&inputs, &cancellation).unwrap();
        assert_eq!(exact_preflight.memory_bytes, baseline.memory_bytes);
        exact.evaluate(inputs.clone(), &cancellation).unwrap();
        assert_eq!(exact_dispatches.load(Ordering::SeqCst), 1);

        let mut rejected_graph = (*graph).clone();
        rejected_graph.limits.max_memory_bytes = baseline.memory_bytes - 1;
        let rejected_dispatches = Arc::new(AtomicUsize::new(0));
        let rejected = BiomeGraphEvaluator::new(Arc::new(rejected_graph), 1)
            .unwrap()
            .with_compute_executor(counting_compute(Arc::clone(&rejected_dispatches)));
        let error = rejected.preflight(&inputs, &cancellation).unwrap_err();
        assert!(matches!(
            error,
            Error::GraphLimit {
                resource: "memory bytes",
                requested,
                limit,
            } if requested == baseline.memory_bytes && limit == baseline.memory_bytes - 1
        ));
        assert!(matches!(
            rejected.evaluate(inputs, &cancellation),
            Err(Error::GraphLimit {
                resource: "memory bytes",
                ..
            })
        ));
        assert_eq!(rejected_dispatches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn preflight_separates_live_provider_generation_from_replay_retention() {
        let cell = WorldCellKey::base(0, 0, 0);
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: cell.bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                authoritative_fields: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: None,
            project_hits: false,
            successful_samples: Arc::new(AtomicUsize::new(0)),
        });
        let provider_set_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        let graph = Arc::new(compile_surface_field_fixture(provider_set_hash));
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        let cancellation = GraphCancellationToken::default();

        let mut live_input = input(cell, graph.required_halo(0));
        live_input.surface_provider_set_hash = provider_set_hash;
        live_input.surface_providers.push(provider);
        let retained_live_tiles = evaluation_input_tile_count(&live_input).unwrap();
        let live_job = job(vec![live_input]);
        let live_preflight = evaluator.preflight(&live_job, &cancellation).unwrap();
        assert!(live_preflight.generated_input_bytes > 0);
        assert_eq!(live_preflight.input_tiles, retained_live_tiles + 1);
        let live_result = evaluator
            .evaluate(live_job, &cancellation)
            .unwrap()
            .cells
            .pop()
            .unwrap();
        assert_eq!(live_result.surface_field_query_tiles.len(), 1);

        let mut replay_input = input(cell, graph.required_halo(0));
        replay_input.surface_provider_set_hash = provider_set_hash;
        replay_input.surface_field_query_tiles = live_result.surface_field_query_tiles.clone();
        let retained_replay_tiles = evaluation_input_tile_count(&replay_input).unwrap();
        let replay_job = job(vec![replay_input]);
        let replay_preflight = evaluator.preflight(&replay_job, &cancellation).unwrap();
        assert_eq!(replay_preflight.generated_input_bytes, 0);
        assert_eq!(replay_preflight.input_tiles, retained_replay_tiles);
        assert!(replay_preflight.retained_input_bytes > live_preflight.retained_input_bytes);
        let replay_result = evaluator
            .evaluate(replay_job, &cancellation)
            .unwrap()
            .cells
            .pop()
            .unwrap();
        assert_eq!(
            live_result.canonical_bytes().unwrap(),
            replay_result.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn dead_authoritative_surface_branch_has_zero_work_and_zero_plan_cost() {
        let cell = WorldCellKey::base(0, 0, 0);
        let successful_samples = Arc::new(AtomicUsize::new(0));
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: cell.bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                authoritative_fields: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: None,
            project_hits: false,
            successful_samples: Arc::clone(&successful_samples),
        });
        let provider_set_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        let baseline_graph = Arc::new(compile_dead_surface_branch_fixture(
            provider_set_hash,
            false,
        ));
        let dead_graph = Arc::new(compile_dead_surface_branch_fixture(provider_set_hash, true));
        let make_job = |graph: &CompiledBiomeGraph| {
            let mut inputs = input(cell, graph.required_halo(0));
            inputs.surface_provider_set_hash = provider_set_hash;
            inputs.surface_providers.push(Arc::clone(&provider));
            job(vec![inputs])
        };
        let cancellation = GraphCancellationToken::default();
        let baseline_evaluator = BiomeGraphEvaluator::new(Arc::clone(&baseline_graph), 1).unwrap();
        let dead_evaluator = BiomeGraphEvaluator::new(Arc::clone(&dead_graph), 1).unwrap();
        let baseline_job = make_job(&baseline_graph);
        let dead_job = make_job(&dead_graph);
        assert_eq!(
            baseline_evaluator
                .preflight(&baseline_job, &cancellation)
                .unwrap(),
            dead_evaluator.preflight(&dead_job, &cancellation).unwrap()
        );
        let baseline = baseline_evaluator
            .evaluate(baseline_job, &cancellation)
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let dead = dead_evaluator
            .evaluate(dead_job, &cancellation)
            .unwrap()
            .cells
            .pop()
            .unwrap();
        assert_eq!(successful_samples.load(Ordering::SeqCst), 0);
        assert!(dead.surface_projection_tiles.is_empty());
        assert!(dead.surface_field_query_tiles.is_empty());
        assert_eq!(
            baseline.canonical_bytes().unwrap(),
            dead.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn cancellation_and_deadline_abort_deterministically_at_every_publication_phase() {
        let cell = WorldCellKey::base(0, 0, 0);
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: cell.bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                authoritative_fields: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: None,
            project_hits: false,
            successful_samples: Arc::new(AtomicUsize::new(0)),
        });
        let provider_set_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        let graph = Arc::new(compile_surface_field_fixture(provider_set_hash));
        let evaluator = BiomeGraphEvaluator::new(Arc::clone(&graph), 1).unwrap();
        let mut inputs = input(cell, graph.required_halo(0));
        inputs.surface_provider_set_hash = provider_set_hash;
        inputs.surface_providers.push(provider);
        let inputs = job(vec![inputs]);

        for checkpoint in [
            TestEvaluationCheckpoint::AfterPreflight,
            TestEvaluationCheckpoint::AfterPreparation,
            TestEvaluationCheckpoint::AfterTraversal,
            TestEvaluationCheckpoint::BeforeFinalValidation,
            TestEvaluationCheckpoint::BeforePublication,
        ] {
            let cancellation = GraphCancellationToken::default();
            cancellation.abort_at_checkpoint(checkpoint, TestAbortKind::Cancelled);
            assert!(matches!(
                evaluator.evaluate(inputs.clone(), &cancellation),
                Err(Error::GraphCancelled)
            ));

            let deadline = GraphCancellationToken::default();
            deadline.abort_at_checkpoint(checkpoint, TestAbortKind::Deadline);
            assert!(matches!(
                evaluator.evaluate(inputs.clone(), &deadline),
                Err(Error::GraphLimit {
                    resource: "time milliseconds",
                    ..
                })
            ));
        }
    }

    #[test]
    fn check_budget_cancels_the_final_tail_without_timing() {
        let graph = Arc::new(compile_fixture(0));
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        let evaluator = BiomeGraphEvaluator::new(graph, 1).unwrap();
        let observed = GraphCancellationToken::default();
        evaluator.evaluate(inputs.clone(), &observed).unwrap();
        let check_count = observed.observed_checks();
        assert!(check_count > 1);

        let cancellation = GraphCancellationToken::default();
        cancellation.cancel_after_checks(check_count - 1);
        assert!(matches!(
            evaluator.evaluate(inputs, &cancellation),
            Err(Error::GraphCancelled)
        ));
    }

    #[test]
    fn multi_cell_runtime_failure_discards_earlier_completed_work() {
        let successful_samples = Arc::new(AtomicUsize::new(0));
        let descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: WorldCellKey::new(0, 0, 0, 1).unwrap().bounds(),
            primitive_count: 1,
            max_tags_per_hit: 0,
            capabilities: SurfaceCapabilities {
                authoritative_fields: true,
                ..SurfaceCapabilities::default()
            },
        };
        let provider: Arc<dyn SurfaceField> = Arc::new(TestSurfaceField {
            descriptor,
            failing_cell_x: Some(1),
            project_hits: false,
            successful_samples: Arc::clone(&successful_samples),
        });
        let provider_set_hash = canonical_surface_provider_set_hash(
            &[Arc::clone(&provider)],
            crate::GraphSafetyLimits::default().max_input_tiles,
        )
        .unwrap();
        let graph = Arc::new(compile_surface_field_fixture(provider_set_hash));
        let mut cells = Vec::new();
        for cell in [WorldCellKey::base(0, 0, 0), WorldCellKey::base(1, 0, 0)] {
            let mut cell_input = input(cell, graph.required_halo(0));
            cell_input.surface_provider_set_hash = provider_set_hash;
            cell_input.surface_providers.push(Arc::clone(&provider));
            cells.push(cell_input);
        }
        let error = BiomeGraphEvaluator::new(graph, 1)
            .unwrap()
            .evaluate(job(cells), &GraphCancellationToken::default())
            .unwrap_err();
        assert!(matches!(
            error,
            Error::Spatial(saffron_spatial::Error::FieldUnavailable)
        ));
        assert_eq!(successful_samples.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn stable_ordinal_streaming_hash_is_pinned() {
        assert_eq!(
            stable_ordinal(&[b"a", b"bc", b""]).unwrap(),
            0xc5db_f3ec_4ecc_a82f
        );
    }

    #[test]
    fn diagnostic_merge_structurally_bounds_small_stream_maps() {
        let identity = |ordinal| CandidateIdentity {
            node: 1,
            node_address: 2,
            node_semantic_revision: 1,
            ordinal,
            ancestor: 0,
        };
        let stream = |ordinal| NamedDiagnosticStream {
            node: GraphNodeAddress {
                module_path: vec![7],
                node: 1,
            },
            label: "x".to_owned(),
            scope: DiagnosticStreamScope::GlobalSnapshot,
            candidates: Some(vec![DiagnosticCandidateSample {
                identity: identity(ordinal),
                owner: WorldCellKey::base(0, 0, 0),
                position: WorldPosition::origin(),
                family: Some(Uuid(702)),
                variation: 0,
                priority: DecisionScalar::from_bits(1),
                ecology_tick: 0,
            }]),
            field: Some(vec![DiagnosticScalarSample {
                candidate: identity(ordinal),
                value: DecisionScalar::from_bits(1),
            }]),
            rejected: Vec::new(),
        };
        let left_stream = stream(1);
        let right_stream = stream(2);
        let left = GraphValue::Diagnostics(vec![left_stream.clone()]);
        let right = GraphValue::Diagnostics(vec![right_stream.clone()]);
        let runtime_scratch = graph_value_merge_scratch_bytes(&left, &right).unwrap();
        let previous_payload_heuristic = left
            .requested_memory_bytes()
            .unwrap()
            .checked_add(right.requested_memory_bytes().unwrap())
            .and_then(|bytes| bytes.checked_mul(3))
            .unwrap();
        assert!(runtime_scratch > previous_payload_heuristic);

        let symbolic = |stream: &NamedDiagnosticStream| SymbolicValueBound {
            domain: Some(GraphDomain::Diagnostics),
            items: 1,
            bytes: diagnostic_stream_memory(stream).unwrap(),
            diagnostic_candidates: 1,
            diagnostic_fields: 1,
            diagnostic_rejected: 0,
            diagnostic_module_path_items: 1,
            diagnostic_label_bytes: 1,
        };
        assert_eq!(
            symbolic_global_merge_scratch_bytes(symbolic(&left_stream), symbolic(&right_stream))
                .unwrap(),
            runtime_scratch
        );

        let graph = compile_fixture(0);
        let mut destination = Some(left);
        merge_graph_value(&mut destination, right, &graph.root.nodes[0]).unwrap();
        let Some(GraphValue::Diagnostics(streams)) = destination else {
            panic!("diagnostic merge returned another domain");
        };
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].candidates.as_ref().unwrap().len(), 2);
        assert_eq!(streams[0].field.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn canonical_validation_rejects_duplicate_result_and_provider_query_entries() {
        let position = WorldPosition::origin();
        let projection = QuantizedSurfaceProjectionTile {
            node: 1,
            node_semantic_revision: 1,
            samples: vec![
                QuantizedSurfaceProjectionEntry {
                    query: position,
                    sample: None,
                },
                QuantizedSurfaceProjectionEntry {
                    query: position,
                    sample: None,
                },
            ],
            provider_set_hash: [1; 32],
        };
        assert!(matches!(
            projection.validate(),
            Err(Error::GraphDocument { path, .. })
                if path == "evaluation.surfaceProjectionTiles"
        ));
        let later_position = WorldPosition::from_global_ticks([1, 0, 0]).unwrap();
        let reversed_projection = QuantizedSurfaceProjectionTile {
            node: 1,
            node_semantic_revision: 1,
            samples: vec![
                QuantizedSurfaceProjectionEntry {
                    query: later_position,
                    sample: None,
                },
                QuantizedSurfaceProjectionEntry {
                    query: position,
                    sample: None,
                },
            ],
            provider_set_hash: [1; 32],
        };
        assert!(matches!(
            reversed_projection.validate(),
            Err(Error::GraphDocument { path, .. })
                if path == "evaluation.surfaceProjectionTiles"
        ));

        let identity = CandidateIdentity {
            node: 1,
            node_address: 1,
            node_semantic_revision: 1,
            ordinal: 1,
            ancestor: 0,
        };
        let field = QuantizedSurfaceFieldQueryTile {
            node: 1,
            node_semantic_revision: 1,
            channel: FieldChannel::Moisture,
            derivative: FieldDerivative::Value,
            samples: vec![
                QuantizedSurfaceFieldQueryEntry {
                    candidate: identity,
                    query: position,
                    value: QuantizedSurfaceFieldValue::Scalar(1),
                },
                QuantizedSurfaceFieldQueryEntry {
                    candidate: identity,
                    query: position,
                    value: QuantizedSurfaceFieldValue::Scalar(1),
                },
            ],
            provider_set_hash: [1; 32],
        };
        assert!(matches!(
            validate_field_query_tile_with_guard(&field, None),
            Err(Error::GraphDocument { path, .. })
                if path == "evaluation.surfaceFieldQueryTiles"
        ));
        let mut later_identity = identity;
        later_identity.ordinal = 2;
        let reversed_field = QuantizedSurfaceFieldQueryTile {
            samples: vec![
                QuantizedSurfaceFieldQueryEntry {
                    candidate: later_identity,
                    query: position,
                    value: QuantizedSurfaceFieldValue::Scalar(1),
                },
                QuantizedSurfaceFieldQueryEntry {
                    candidate: identity,
                    query: position,
                    value: QuantizedSurfaceFieldValue::Scalar(1),
                },
            ],
            ..field
        };
        assert!(matches!(
            validate_field_query_tile_with_guard(&reversed_field, None),
            Err(Error::GraphDocument { path, .. })
                if path == "evaluation.surfaceFieldQueryTiles"
        ));

        let graph = Arc::new(compile_document(micro_document()));
        let mut result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(
                job(vec![input(
                    WorldCellKey::base(0, 0, 0),
                    graph.required_halo(0),
                )]),
                &GraphCancellationToken::default(),
            )
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let mut duplicate_result = result.clone();
        duplicate_result
            .micro_fields
            .push(duplicate_result.micro_fields[0].clone());
        assert!(matches!(
            duplicate_result.canonical_bytes(),
            Err(Error::GraphDocument { path, .. }) if path == "evaluation.canonicalEncoding"
        ));

        let mut later_tile = result.micro_fields[0].clone();
        later_tile.cell = WorldCellKey::base(1, 0, 0);
        result.micro_fields.push(later_tile);
        assert!(result.canonical_bytes().is_ok());
        let sections = result.cell_artifact_sections().unwrap();
        assert_eq!(
            sections
                .iter()
                .map(|section| section.kind)
                .collect::<Vec<_>>(),
            vec![
                VegetationCellSectionKind::MacroPoints,
                VegetationCellSectionKind::MicroFields,
                VegetationCellSectionKind::Provenance,
                VegetationCellSectionKind::RejectionDiagnostics,
                VegetationCellSectionKind::SurfaceAttachments,
                VegetationCellSectionKind::SurfaceDependencies,
                VegetationCellSectionKind::RenderReferences,
                VegetationCellSectionKind::RenderBounds,
                VegetationCellSectionKind::CollisionInputs,
                VegetationCellSectionKind::NavigationContributions,
                VegetationCellSectionKind::EcologyBoundary,
                VegetationCellSectionKind::EcologyCheckpoint,
            ]
        );
        assert_eq!(
            sections[0].bytes,
            result.macro_points.canonical_bytes().unwrap()
        );
        assert!(sections[1].bytes.starts_with(b"SVEGMIC2"));
        assert!(sections[6].bytes.starts_with(b"SVEGRRF1"));
        assert!(sections[7].bytes.starts_with(b"SVEGRBD1"));
        assert!(sections[8].bytes.starts_with(b"SVEGCOL1"));
        assert!(sections[9].bytes.starts_with(b"SVEGNAV1"));
        assert!(sections[10].bytes.starts_with(b"SVEGEBD1"));
        assert!(sections[11].bytes.starts_with(b"SVEGECP1"));
        result.micro_fields.reverse();
        assert!(matches!(
            result.canonical_bytes(),
            Err(Error::GraphDocument { path, .. }) if path == "evaluation.canonicalEncoding"
        ));
    }

    #[test]
    fn every_cell_facet_strictly_decodes_the_canonical_encoder_output() {
        let graph = Arc::new(compile_fixture(0));
        let mut result = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(
                job(vec![input(
                    WorldCellKey::base(0, 0, 0),
                    graph.required_halo(0),
                )]),
                &GraphCancellationToken::default(),
            )
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let micro_graph = Arc::new(compile_document(micro_document()));
        let micro_result = BiomeGraphEvaluator::new(Arc::clone(&micro_graph), 1)
            .unwrap()
            .evaluate(
                job(vec![input(
                    WorldCellKey::base(0, 0, 0),
                    micro_graph.required_halo(0),
                )]),
                &GraphCancellationToken::default(),
            )
            .unwrap()
            .cells
            .pop()
            .unwrap();
        result.micro_fields = micro_result.micro_fields;
        let query = WorldPosition::origin();
        let candidate = CandidateIdentity {
            node: 1,
            node_address: 1,
            node_semantic_revision: 1,
            ordinal: 1,
            ancestor: 0,
        };
        result.surface_projection_tiles = vec![QuantizedSurfaceProjectionTile {
            node: 1,
            node_semantic_revision: 1,
            samples: vec![QuantizedSurfaceProjectionEntry {
                query,
                sample: Some(QuantizedSurfaceProjectionSample {
                    position: query,
                    attachment: SurfaceAttachment::new(
                        SurfaceProviderId(7),
                        saffron_spatial::SurfacePrimitiveId(1),
                        [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                        SurfaceRevision(3),
                    )
                    .unwrap(),
                    normal: [
                        SignedUnit::from_bits(0).unwrap(),
                        SignedUnit::from_bits(i16::MAX).unwrap(),
                        SignedUnit::from_bits(0).unwrap(),
                    ],
                    projection: [DecisionScalar::from_bits(3); 3],
                    tags: vec![WeightedSurfaceTag {
                        tag: saffron_spatial::SurfaceTagId(5),
                        weight: UnitInterval::ONE,
                    }],
                }),
            }],
            provider_set_hash: [1; 32],
        }];
        result.surface_field_query_tiles = vec![QuantizedSurfaceFieldQueryTile {
            node: 2,
            node_semantic_revision: 1,
            channel: FieldChannel::Moisture,
            derivative: FieldDerivative::Value,
            samples: vec![QuantizedSurfaceFieldQueryEntry {
                candidate,
                query,
                value: QuantizedSurfaceFieldValue::Scalar(7),
            }],
            provider_set_hash: [1; 32],
        }];
        result.diagnostics.streams = vec![NamedDiagnosticStream {
            node: GraphNodeAddress {
                module_path: vec![3],
                node: 4,
            },
            label: "accepted".to_owned(),
            scope: DiagnosticStreamScope::CandidateLineage(CandidateLineage(5)),
            candidates: Some(vec![DiagnosticCandidateSample {
                identity: candidate,
                owner: result.cell,
                position: query,
                family: Some(Uuid(702)),
                variation: 1,
                priority: DecisionScalar::from_bits(7),
                ecology_tick: 9,
            }]),
            field: Some(vec![DiagnosticScalarSample {
                candidate,
                value: DecisionScalar::from_bits(11),
            }]),
            rejected: Vec::new(),
        }];

        let sections = result.cell_artifact_sections().unwrap();
        assert_eq!(sections.len(), VegetationCellSectionKind::ALL.len());
        for section in &sections {
            let decoded = decode_vegetation_cell_facet(section.kind, &section.bytes).unwrap();
            match decoded {
                VegetationCellFacet::MacroPoints(points) => {
                    assert_eq!(*points, result.macro_points);
                    assert_eq!(points.point(0).unwrap().id, points.ids[0]);
                }
                VegetationCellFacet::MicroFields(tiles) => {
                    assert_eq!(tiles, result.micro_fields);
                }
                VegetationCellFacet::Provenance(table) => {
                    assert_eq!(table, result.provenance);
                }
                VegetationCellFacet::RejectionDiagnostics(diagnostics) => {
                    assert_eq!(
                        diagnostics.candidate_count,
                        result.diagnostics.candidate_count
                    );
                    assert_eq!(
                        diagnostics.accepted_count,
                        result.diagnostics.accepted_count
                    );
                    assert_eq!(diagnostics.rejected, result.diagnostics.rejected);
                    assert_eq!(diagnostics.streams, result.diagnostics.streams);
                }
                VegetationCellFacet::SurfaceAttachments(tiles) => {
                    assert_eq!(tiles, result.surface_projection_tiles);
                }
                VegetationCellFacet::SurfaceDependencies(tiles) => {
                    assert_eq!(tiles, result.surface_field_query_tiles);
                }
                VegetationCellFacet::RenderReferences(rows) => {
                    assert_eq!(rows.len(), result.macro_points.ids.len());
                    assert_eq!(rows[0].plant, result.macro_points.ids[0]);
                    assert_eq!(rows[0].family, result.macro_points.families[0]);
                }
                VegetationCellFacet::RenderBounds(rows) => {
                    assert_eq!(rows.len(), result.macro_points.ids.len());
                    assert_eq!(rows[0].bounds, result.macro_points.bounds[0]);
                }
                VegetationCellFacet::CollisionInputs(rows) => {
                    assert_eq!(rows.len(), result.macro_points.ids.len());
                    assert_eq!(rows[0].position, result.macro_points.positions[0]);
                }
                VegetationCellFacet::NavigationContributions(rows) => {
                    assert_eq!(rows.len(), result.macro_points.ids.len());
                    assert_eq!(rows[0].bounds, result.macro_points.bounds[0]);
                }
                VegetationCellFacet::EcologyBoundary(rows) => {
                    assert!(rows.len() <= result.macro_points.ids.len());
                    for row in rows {
                        assert!(result.macro_points.ids.contains(&row.plant));
                    }
                }
                VegetationCellFacet::EcologyCheckpoint(rows) => {
                    assert_eq!(rows.len(), result.macro_points.ids.len());
                    assert_eq!(rows[0].ecology_tick, result.macro_points.ecology_ticks[0]);
                }
            }

            let mut corrupt = section.bytes.clone();
            corrupt[0] ^= 0xff;
            assert!(matches!(
                decode_vegetation_cell_facet(section.kind, &corrupt),
                Err(Error::ArtifactFormat { field, .. }) if field == "magic"
            ));
            let mut truncated = section.bytes.clone();
            truncated.pop();
            assert!(decode_vegetation_cell_facet(section.kind, &truncated).is_err());
            let mut trailing = section.bytes.clone();
            trailing.push(0);
            assert!(matches!(
                decode_vegetation_cell_facet(section.kind, &trailing),
                Err(Error::ArtifactFormat { field, .. }) if field == "trailingBytes"
            ));
        }

        let dependencies = sections
            .iter()
            .find(|section| section.kind == VegetationCellSectionKind::SurfaceDependencies)
            .unwrap();
        let mut wrong_domain = dependencies.bytes.clone();
        wrong_domain[37] = 1;
        assert!(matches!(
            decode_vegetation_cell_facet(dependencies.kind, &wrong_domain),
            Err(Error::ArtifactFormat { field, .. }) if field == "samples.valueType"
        ));
    }

    #[test]
    fn rejection_facet_summary_validates_the_complete_payload() {
        let candidate = |ordinal| CandidateIdentity {
            node: 1,
            node_address: 2,
            node_semantic_revision: 1,
            ordinal,
            ancestor: 0,
        };
        let result = GraphEvaluationResult {
            cell: WorldCellKey::base(-1, 0, 2),
            macro_points: PlantPointColumns::default(),
            micro_fields: Vec::new(),
            surface_projection_tiles: Vec::new(),
            surface_field_query_tiles: Vec::new(),
            ancestor_references: Vec::new(),
            provenance: ProvenanceTable::default(),
            diagnostics: GraphEvaluationDiagnostics {
                rejected: vec![
                    RejectedCandidate {
                        candidate: candidate(1),
                        reason: CandidateRejectionReason::Threshold,
                        provenance: ProvenanceHandle(0),
                    },
                    RejectedCandidate {
                        candidate: candidate(2),
                        reason: CandidateRejectionReason::NoSpecies,
                        provenance: ProvenanceHandle(0),
                    },
                ],
                candidate_count: 2,
                accepted_count: 0,
                ..GraphEvaluationDiagnostics::default()
            },
        };
        let mut bytes = result
            .cell_artifact_sections()
            .unwrap()
            .into_iter()
            .find(|section| section.kind == VegetationCellSectionKind::RejectionDiagnostics)
            .unwrap()
            .bytes;
        assert_eq!(
            vegetation_rejection_totals(&bytes).unwrap(),
            vec![
                (CandidateRejectionReason::Threshold, 1),
                (CandidateRejectionReason::NoSpecies, 1),
            ]
        );
        bytes.push(0);
        assert!(matches!(
            vegetation_rejection_totals(&bytes),
            Err(Error::ArtifactFormat { field, .. }) if field == "trailingBytes"
        ));
    }

    #[test]
    fn public_preflight_observes_entry_cancellation() {
        let graph = Arc::new(compile_fixture(0));
        let inputs = job(vec![input(
            WorldCellKey::base(0, 0, 0),
            graph.required_halo(0),
        )]);
        let cancellation = GraphCancellationToken::default();
        cancellation.cancel();
        assert!(matches!(
            BiomeGraphEvaluator::new(graph, 1)
                .unwrap()
                .preflight(&inputs, &cancellation),
            Err(Error::GraphCancelled)
        ));
    }

    #[test]
    fn surface_hit_contract_enforces_declared_tags_and_identity() {
        let graph = compile_document(explicit_anchor_document());
        let node = &graph.root.nodes[0];
        let mut descriptor = SurfaceProviderDescriptor {
            id: SurfaceProviderId(77),
            revision: SurfaceRevision(3),
            bounds: WorldCellKey::base(0, 0, 0).bounds(),
            primitive_count: 1,
            max_tags_per_hit: 1,
            capabilities: saffron_spatial::SurfaceCapabilities::default(),
        };
        let mut hit = SurfaceHit {
            provider: descriptor.id,
            position: WorldPosition::origin(),
            distance_m: 0.0,
            frame: saffron_spatial::SurfaceFrame::from_normal(saffron_geometry::glam::Vec3::Y)
                .unwrap(),
            coordinates: saffron_spatial::SurfaceCoordinates::default(),
            attachment: Some(
                SurfaceAttachment::new(
                    descriptor.id,
                    saffron_spatial::SurfacePrimitiveId(1),
                    [UnitInterval::ONE, UnitInterval::ZERO, UnitInterval::ZERO],
                    descriptor.revision,
                )
                .unwrap(),
            ),
            tags: vec![
                WeightedSurfaceTag {
                    tag: saffron_spatial::SurfaceTagId(5),
                    weight: UnitInterval::ONE,
                },
                WeightedSurfaceTag {
                    tag: saffron_spatial::SurfaceTagId(9),
                    weight: UnitInterval::ONE,
                },
            ],
            revision: descriptor.revision,
        };
        assert_graph_limit(
            validate_surface_hit_contract(node, &descriptor, &hit).unwrap_err(),
            "surface tags per hit",
        );
        descriptor.max_tags_per_hit = 2;
        validate_surface_hit_contract(node, &descriptor, &hit).unwrap();

        let valid = hit.clone();
        hit.provider = SurfaceProviderId(78);
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));

        hit = valid.clone();
        hit.revision = SurfaceRevision(4);
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));

        hit = valid.clone();
        hit.attachment.as_mut().unwrap().provider = SurfaceProviderId(78);
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));

        hit = valid.clone();
        hit.attachment.as_mut().unwrap().revision = SurfaceRevision(4);
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));

        hit = valid.clone();
        hit.tags.swap(0, 1);
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));

        hit = valid;
        hit.tags[1].tag = hit.tags[0].tag;
        assert!(matches!(
            validate_surface_hit_contract(node, &descriptor, &hit),
            Err(Error::GraphDocument { .. })
        ));
    }

    #[test]
    fn symbolic_preflight_uses_the_concrete_region_total() {
        let mut graph = compile_fixture(0);
        graph.limits.max_candidates = 30;
        let mut inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let mut second = inputs.regions[0];
        second.id = second.id.checked_add(1).unwrap();
        second.hierarchy_namespace = None;
        inputs.regions.push(second);
        let expected = inputs
            .regions
            .iter()
            .filter(|region| region.kind == EvaluationRegionKind::Biome)
            .count() as u64
            * 24;

        let error = BiomeGraphEvaluator::new(Arc::new(graph), 1)
            .unwrap()
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap_err();
        assert!(
            matches!(
                &error,
                Error::GraphLimit {
                    resource: "candidate count",
                    requested,
                    limit: 30,
                } if *requested == expected
            ),
            "{error:?}"
        );
    }

    #[test]
    fn symbolic_arithmetic_reports_a_typed_limit_before_u64_overflow() {
        assert!(matches!(
            bound_mul("candidate count", u64::MAX, 2, u64::MAX),
            Err(Error::GraphLimit {
                resource: "candidate count",
                requested: u64::MAX,
                limit: u64::MAX,
            })
        ));
    }

    #[test]
    fn symbolic_gpu_transfer_matches_the_resident_program_abi() {
        let graph = Arc::new(compile_resident_branch_fixture());
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let compute = reference_compute();
        let plan = build_execution_plan(
            &graph,
            false,
            Some(GraphGpuScheduling {
                profile: compute.profile(),
                qualifications: compute.qualifications(),
            }),
        )
        .unwrap();
        let global_store = SymbolicGlobalStore::default();
        let cancellation = GraphCancellationToken::default();
        let guard = PreflightGuard {
            cancellation: &cancellation,
            deadline: evaluation_deadline(&graph).unwrap(),
            time_limit_ms: graph.limits.max_time_ms,
        };
        let bound = symbolic_evaluation_bound(
            &graph,
            &inputs,
            &plan,
            SymbolicEvaluationScope::Cell {
                global_store: &global_store,
            },
            guard,
        )
        .unwrap()
        .bound;
        let actual = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .with_compute_executor(compute)
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let actual_transfer = actual
            .diagnostics
            .gpu_groups
            .iter()
            .map(|group| group.transfer_bytes)
            .sum::<u64>();

        assert_eq!(bound.transfer_bytes, actual_transfer);
    }

    #[test]
    fn worker_counts_and_cell_request_order_are_byte_identical() {
        let graph = Arc::new(compile_fixture(0));
        let halo = graph.required_halo(0);
        let cells = [
            WorldCellKey::base(0, 0, 0),
            WorldCellKey::base(1, 0, 0),
            WorldCellKey::base(-1, 0, 1),
            WorldCellKey::base(2, 0, -1),
        ];
        let serial = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(
                job(cells.iter().map(|cell| input(*cell, halo)).collect()),
                &GraphCancellationToken::default(),
            )
            .unwrap();
        let mut reversed = cells
            .iter()
            .rev()
            .map(|cell| input(*cell, halo))
            .collect::<Vec<_>>();
        reversed[0].regions.reverse();
        let parallel = BiomeGraphEvaluator::new(graph, 4)
            .unwrap()
            .evaluate(job(reversed), &GraphCancellationToken::default())
            .unwrap();
        let serial = serial
            .cells
            .into_iter()
            .map(|result| (result.cell, result.canonical_bytes().unwrap()))
            .collect::<BTreeMap<_, _>>();
        let parallel = parallel
            .cells
            .into_iter()
            .map(|result| (result.cell, result.canonical_bytes().unwrap()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(serial, parallel);
    }

    #[test]
    fn global_stages_publish_once_and_cells_only_replay_immutable_tiles() {
        let graph = Arc::new(compile_global_fixture());
        let halo = graph.required_halo(0);
        let cells = vec![
            input(WorldCellKey::base(0, 0, 0), halo),
            input(WorldCellKey::base(1, 0, 0), halo),
        ];
        let reference_bounds = cells
            .iter()
            .map(|inputs| {
                (
                    inputs.output_cell,
                    ancestor_reference_upper_bound(&graph, inputs, true).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let expected_tiles = expected_global_stage_tiles(&graph, &cells).unwrap();
        let result = BiomeGraphEvaluator::new(Arc::clone(&graph), 2)
            .unwrap()
            .evaluate(
                global_job(&graph, cells),
                &GraphCancellationToken::default(),
            )
            .unwrap();

        assert_eq!(result.global_stages.len(), expected_tiles.len());
        assert!(result.global_stages.iter().all(|tile| {
            tile.resident_bytes > 0
                && tile
                    .result
                    .diagnostics
                    .nodes
                    .iter()
                    .any(|node| node.operator == GraphOperator::BlueNoisePoisson)
        }));
        assert!(
            result
                .global_stages
                .iter()
                .any(|tile| { tile.result.macro_points.row_count().unwrap() > 0 })
        );
        for cell in &result.cells {
            assert!(
                !cell
                    .diagnostics
                    .nodes
                    .iter()
                    .any(|node| node.operator == GraphOperator::BlueNoisePoisson)
            );
            assert_eq!(cell.macro_points.row_count().unwrap(), 0);
            assert!(!cell.ancestor_references.is_empty());
            assert!(cell.ancestor_references.capacity() as u64 <= reference_bounds[&cell.cell]);
        }
        let mut plant_ids = BTreeSet::new();
        for tile in &result.global_stages {
            for plant in &tile.result.macro_points.ids {
                assert!(plant_ids.insert(*plant));
                tile.result.explain_plant(*plant).unwrap();
            }
        }
    }

    #[test]
    fn global_stage_set_must_be_complete_before_any_result_is_published() {
        let graph = Arc::new(compile_global_fixture());
        let halo = graph.required_halo(0);
        let mut inputs = global_job(&graph, vec![input(WorldCellKey::base(0, 0, 0), halo)]);
        inputs.global_stages.pop().unwrap();
        let error = BiomeGraphEvaluator::new(graph, 1)
            .unwrap()
            .evaluate(inputs, &GraphCancellationToken::default())
            .unwrap_err();
        assert!(
            matches!(error, Error::GraphDocument { path, .. } if path == "evaluation.globalStages")
        );
    }

    #[test]
    fn qualified_compute_nodes_match_reference_bytes_through_the_single_facade() {
        let graph = Arc::new(compile_resident_branch_fixture());
        let halo = graph.required_halo(0);
        let inputs = input(WorldCellKey::base(0, 0, 0), halo);
        let reference = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(
                job(vec![inputs.clone()]),
                &GraphCancellationToken::default(),
            )
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let compute = BiomeGraphEvaluator::new(graph, 1)
            .unwrap()
            .with_compute_executor(reference_compute())
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap()
            .cells
            .pop()
            .unwrap();
        assert_eq!(
            reference.canonical_bytes().unwrap(),
            compute.canonical_bytes().unwrap()
        );
        assert!(compute.diagnostics.nodes.iter().any(|node| {
            node.symbol.starts_with("noise:")
                && node.execution_domain == GraphExecutionDomain::SlangCompute
                && node.transfer_bytes == 0
                && node.elapsed_micros == 0
        }));
        assert!(
            compute
                .diagnostics
                .gpu_groups
                .iter()
                .any(|group| { group.transfer_bytes > 0 && group.invocation_count > 0 })
        );
    }

    #[test]
    fn branched_resident_subgraph_dispatches_once_and_matches_reference() {
        let graph = Arc::new(compile_resident_branch_fixture());
        let qualification = reference_compute();
        let plan = build_execution_plan(
            &graph,
            false,
            Some(GraphGpuScheduling {
                profile: qualification.profile(),
                qualifications: qualification.qualifications(),
            }),
        )
        .unwrap();
        assert_eq!(plan.domain_for(&[], 15), None);
        let halo = graph.required_halo(0);
        let inputs = input(WorldCellKey::base(0, 0, 0), halo);
        let reference = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .evaluate(
                job(vec![inputs.clone()]),
                &GraphCancellationToken::default(),
            )
            .unwrap()
            .cells
            .pop()
            .unwrap();
        let dispatches = Arc::new(AtomicUsize::new(0));
        let compute = BiomeGraphEvaluator::new(graph, 1)
            .unwrap()
            .with_compute_executor(counting_compute(Arc::clone(&dispatches)))
            .evaluate(job(vec![inputs]), &GraphCancellationToken::default())
            .unwrap()
            .cells
            .pop()
            .unwrap();
        assert_eq!(
            reference.canonical_bytes().unwrap(),
            compute.canonical_bytes().unwrap()
        );
        assert_eq!(
            dispatches.load(Ordering::SeqCst),
            compute.diagnostics.gpu_groups.len()
        );
        assert_eq!(
            compute.diagnostics.gpu_groups.len(),
            1,
            "{:?}",
            compute.diagnostics.gpu_groups
        );
        assert_eq!(
            compute.diagnostics.gpu_groups[0]
                .nodes
                .iter()
                .map(|node| node.node)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([6, 10, 11, 12, 13, 14])
        );
    }

    #[test]
    fn halo_faces_and_corners_publish_unique_spaced_owned_points() {
        let graph = compile_fixture(0);
        let halo = graph.required_halo(0);
        let results = evaluate_job(
            &graph,
            job(vec![
                input(WorldCellKey::base(0, 0, 0), halo),
                input(WorldCellKey::base(1, 0, 0), halo),
                input(WorldCellKey::base(0, 0, 1), halo),
                input(WorldCellKey::base(1, 0, 1), halo),
            ]),
            4,
            &GraphCancellationToken::default(),
            None,
        )
        .unwrap()
        .cells;
        let mut ids = BTreeSet::new();
        let mut positions = Vec::new();
        for result in &results {
            for row in 0..result.macro_points.row_count().unwrap() {
                assert_eq!(result.macro_points.owner_cells[row], result.cell);
                assert!(ids.insert(result.macro_points.ids[row]));
                positions.push(position(&result.macro_points, row));
            }
        }
        let required = i128::from(2 * LOCAL_TICKS_PER_METER);
        for left in 0..positions.len() {
            for right in left + 1..positions.len() {
                assert!(
                    distance_squared_xz(positions[left], positions[right]).unwrap()
                        >= required * required
                );
            }
        }
    }

    #[test]
    fn coarse_outputs_publish_once_and_fine_cells_reference_the_ancestor() {
        let graph = compile_fixture(1);
        let coarse_cell = WorldCellKey::new(0, 0, 0, 1).unwrap();
        let coarse = evaluate_cell_reference(
            &graph,
            &input(coarse_cell, graph.required_halo(1)),
            &GraphCancellationToken::default(),
        )
        .unwrap();
        assert!(coarse.macro_points.row_count().unwrap() > 0);
        assert!(
            coarse
                .macro_points
                .owner_cells
                .iter()
                .all(|owner| *owner == coarse_cell)
        );

        let fine = evaluate_cell_reference(
            &graph,
            &input(WorldCellKey::base(0, 0, 0), graph.required_halo(0)),
            &GraphCancellationToken::default(),
        )
        .unwrap();
        assert_eq!(fine.macro_points.row_count().unwrap(), 0);
        assert_eq!(fine.ancestor_references, vec![coarse_cell]);
    }

    #[test]
    fn unrelated_node_edits_preserve_accepted_ids_and_preview_reports_destructive_edits() {
        let graph = compile_fixture(0);
        let inputs = input(WorldCellKey::base(0, 0, 0), graph.required_halo(0));
        let previous =
            evaluate_cell_reference(&graph, &inputs, &GraphCancellationToken::default()).unwrap();

        let mut asset = fixture_asset(0);
        let mut document = BiomeGraphDocument::from_json(&asset.graph).unwrap();
        document
            .nodes
            .iter_mut()
            .find(|node| node.guid == 5)
            .unwrap()
            .semantic_revision = 2;
        asset.graph = document.to_json();
        let unrelated = compile_biome_graph(
            &asset,
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let unchanged =
            evaluate_cell_reference(&unrelated, &inputs, &GraphCancellationToken::default())
                .unwrap();
        assert_eq!(previous.macro_points.ids, unchanged.macro_points.ids);

        let mut changed_asset = fixture_asset(0);
        let mut changed = BiomeGraphDocument::from_json(&changed_asset.graph).unwrap();
        changed
            .nodes
            .iter_mut()
            .find(|node| node.guid == 2)
            .unwrap()
            .semantic_revision = 2;
        changed_asset.graph = changed.to_json();
        let changed_graph = compile_biome_graph(
            &changed_asset,
            &[],
            &NoDependencies,
            GraphCompileOptions::canonical(),
        )
        .unwrap();
        let proposed =
            evaluate_cell_reference(&changed_graph, &inputs, &GraphCancellationToken::default())
                .unwrap();
        let pin = previous.macro_points.ids[0];
        let preview = preview_graph_identity_edit(&previous, &proposed, &[pin], &[pin]).unwrap();
        assert!(!preview.accepted.is_empty());
        assert!(!preview.removed.is_empty());
        assert_eq!(preview.conflicts.invalidated_pins, vec![pin]);
        assert_eq!(preview.conflicts.invalidated_overrides, vec![pin]);
    }

    #[test]
    fn cancellation_and_missing_halo_abort_without_a_result() {
        let graph = compile_fixture(0);
        let cancellation = GraphCancellationToken::default();
        cancellation.cancel();
        assert!(matches!(
            evaluate_cell_reference(
                &graph,
                &input(WorldCellKey::base(0, 0, 0), graph.required_halo(0)),
                &cancellation,
            ),
            Err(Error::GraphCancelled)
        ));
        assert!(matches!(
            evaluate_cell_reference(
                &graph,
                &input(WorldCellKey::base(0, 0, 0), DecisionScalar::from_bits(0),),
                &GraphCancellationToken::default(),
            ),
            Err(Error::GraphAuthoritativeInput { .. })
        ));
    }

    fn spline_point(ticks: [i128; 3]) -> WorldPosition {
        WorldPosition::from_global_ticks(ticks).unwrap()
    }

    #[test]
    fn spline_arclength_samples_a_joint_once() {
        let points = [
            spline_point([0, 0, 0]),
            spline_point([10, 0, 0]),
            spline_point([10, 0, 10]),
        ];
        let samples = sample_spline_segments(&spline_segments(&points).unwrap(), 5, 0).unwrap();
        assert_eq!(
            samples,
            vec![[0, 0, 0], [5, 0, 0], [10, 0, 0], [10, 0, 5], [10, 0, 10],]
        );
    }

    #[test]
    fn spline_samples_and_ordinals_are_invariant_to_collinear_subdivision() {
        let direct = [spline_point([0, 0, 0]), spline_point([20, 0, 0])];
        let subdivided = [
            spline_point([0, 0, 0]),
            spline_point([7, 0, 0]),
            spline_point([13, 0, 0]),
            spline_point([20, 0, 0]),
        ];
        let direct = sample_spline_segments(&spline_segments(&direct).unwrap(), 4, 2).unwrap();
        let subdivided =
            sample_spline_segments(&spline_segments(&subdivided).unwrap(), 4, 2).unwrap();
        assert_eq!(direct, subdivided);
        assert_eq!(direct.len(), 6);
    }

    #[test]
    fn spline_lateral_frame_tracks_direction_and_transports_through_turns() {
        let forward = [spline_point([0, 0, 0]), spline_point([10, 0, 0])];
        let reverse = [spline_point([10, 0, 0]), spline_point([0, 0, 0])];
        assert_eq!(
            sample_spline_segments(&spline_segments(&forward).unwrap(), 10, 2).unwrap(),
            vec![[0, 0, -2], [10, 0, -2]]
        );
        assert_eq!(
            sample_spline_segments(&spline_segments(&reverse).unwrap(), 10, 2).unwrap(),
            vec![[10, 0, 2], [0, 0, 2]]
        );

        let turn = [
            spline_point([0, 0, 0]),
            spline_point([10, 0, 0]),
            spline_point([10, 0, 10]),
        ];
        assert_eq!(
            sample_spline_segments(&spline_segments(&turn).unwrap(), 10, 2).unwrap(),
            vec![[0, 0, -2], [12, 0, 0], [12, 0, 10]]
        );
    }
}
