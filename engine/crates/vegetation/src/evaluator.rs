//! Canonical reference and parallel biome-graph evaluation.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use saffron_core::Uuid;
use saffron_geometry::glam::DVec3;
use saffron_spatial::{
    BASE_CELL_TICKS, DecisionCurve, DecisionHessian3, DecisionScalar, DecisionVec3,
    FieldAvailability, FieldChannel, FieldDerivative, LOCAL_TICKS_PER_METER, RandomDomain,
    RandomStream, SignedUnit, SurfaceAttachment, SurfaceField, SurfaceHit, SurfaceProjection,
    SurfaceProviderId, SurfaceRevision, SurfaceTileDescriptor, UnitInterval, WeightedSurfaceTag,
    WorldBounds, WorldCellKey, WorldPosition, div_round_ties_even, world_cells_covering_bounds,
};

use crate::hash::sha256;
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphNode, CompiledGraphUnit, Error,
    FieldBlendOperator, GraphAuthority, GraphClusterMode, GraphCombineOperation,
    GraphComputeExecutor, GraphDependencySource, GraphDistanceSource, GraphDomain,
    GraphExecutionDomain, GraphExecutionPlan, GraphGpuInstruction, GraphGpuInvocation,
    GraphGpuProgram, GraphGpuRegister, GraphGpuRegisterType, GraphGpuScheduling, GraphGpuValue,
    GraphNodeAddress, GraphOperator, GraphParameterValue, IdentityConflictReport,
    InteractionPolicy, PlantFlags, PlantId, PlantIdCollisionTable, PlantLifecycle, PlantPoint,
    PlantPointColumns, ProceduralPlantIdentity, ProvenanceDecision, ProvenanceDecisionHandle,
    ProvenanceDecisionOutcome, ProvenanceHandle, ProvenanceRecord, ProvenanceTable,
    QualifiedGraphPin, QuantizedOrientation, Result, build_execution_plan, identity_conflicts,
    GRAPH_GPU_INSTRUCTION_WORDS, GRAPH_GPU_INVOCATION_WORDS, GRAPH_GPU_OUTPUT_WORDS,
    GRAPH_GPU_PROGRAM_HEADER_WORDS,
};

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
        self.candidates.sort_by_key(|candidate| candidate.identity);
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
    fn validate(&self) -> Result<()> {
        let correct_values = self.samples.iter().all(|entry| {
            matches!(
                (self.derivative, entry.value),
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
            )
        });
        if self.node == 0
            || self.node_semantic_revision == 0
            || self.provider_set_hash == [0; 32]
            || !correct_values
            || self.samples.windows(2).any(|pair| {
                (pair[0].candidate, pair[0].query) >= (pair[1].candidate, pair[1].query)
            })
        {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile identity, type, or exact query ordering is invalid"
                    .to_owned(),
            });
        }
        Ok(())
    }

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
) -> Result<[u8; 32]> {
    if providers.is_empty() {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviders".to_owned(),
            reason: "surface provider set cannot be empty".to_owned(),
        });
    }
    let mut descriptors = providers
        .iter()
        .map(|provider| provider.descriptor())
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|descriptor| descriptor.id);
    if descriptors.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviders".to_owned(),
            reason: "surface provider identities must be unique".to_owned(),
        });
    }
    let mut bytes = b"saffron-anima/surface-provider-set/v1\0".to_vec();
    for descriptor in descriptors {
        bytes.extend_from_slice(&descriptor.id.0.to_be_bytes());
        bytes.extend_from_slice(&descriptor.revision.0.to_be_bytes());
        for value in descriptor.bounds.min_ticks() {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in descriptor.bounds.max_ticks_exclusive() {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&descriptor.primitive_count.to_be_bytes());
        let capabilities = descriptor.capabilities;
        bytes.push(u8::from(capabilities.ray));
        bytes.push(u8::from(capabilities.project));
        bytes.push(u8::from(capabilities.nearest));
        bytes.push(u8::from(capabilities.uv));
        bytes.push(u8::from(capabilities.authoritative_attachments));
        bytes.push(u8::from(capabilities.authoritative_fields));
    }
    Ok(sha256(&bytes))
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
    let mut queries = queries.to_vec();
    queries.sort();
    queries.dedup();
    let mut samples = Vec::with_capacity(queries.len());
    for position in queries {
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
            let mut values = Vec::with_capacity(capacity);
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
            let mut values = Vec::with_capacity(capacity);
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
            let mut values = Vec::with_capacity(capacity);
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

    /// Stable result bytes used by determinism and scheduling tests.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = b"SVEGEVAL04".to_vec();
        bytes.extend_from_slice(&self.cell.canonical_bytes());
        let macro_bytes = self.macro_points.canonical_bytes()?;
        push_len(&mut bytes, macro_bytes.len())?;
        bytes.extend_from_slice(&macro_bytes);
        let mut micro = self.micro_fields.clone();
        micro.sort_by_key(|tile| tile.cell);
        push_len(&mut bytes, micro.len())?;
        for tile in micro {
            bytes.extend_from_slice(&tile.cell.canonical_bytes());
            for dimension in tile.dimensions {
                bytes.extend_from_slice(&dimension.to_be_bytes());
            }
            push_len(&mut bytes, tile.density.len())?;
            for value in tile.density {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            push_len(&mut bytes, tile.attributes.len())?;
            for (channel, values) in tile.attributes {
                bytes.extend_from_slice(&channel.to_be_bytes());
                push_len(&mut bytes, values.len())?;
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            bytes.extend_from_slice(&tile.reconstruction_seed.to_be_bytes());
        }
        let mut projection_tiles = self.surface_projection_tiles.clone();
        projection_tiles.sort_by_key(|tile| {
            (
                tile.node,
                tile.node_semantic_revision,
                tile.samples.first().map(|entry| entry.query),
            )
        });
        push_len(&mut bytes, projection_tiles.len())?;
        for tile in projection_tiles {
            bytes.extend_from_slice(&tile.node.to_be_bytes());
            bytes.extend_from_slice(&tile.node_semantic_revision.to_be_bytes());
            bytes.extend_from_slice(&tile.provider_set_hash);
            push_len(&mut bytes, tile.samples.len())?;
            for entry in tile.samples {
                push_world_position(&mut bytes, entry.query);
                match entry.sample {
                    Some(sample) => {
                        bytes.push(1);
                        push_world_position(&mut bytes, sample.position);
                        bytes.extend_from_slice(&sample.attachment.provider.0.to_be_bytes());
                        bytes.extend_from_slice(&sample.attachment.primitive.0.to_be_bytes());
                        for barycentric in sample.attachment.barycentric {
                            bytes.extend_from_slice(&barycentric.bits().to_be_bytes());
                        }
                        bytes.extend_from_slice(&sample.attachment.revision.0.to_be_bytes());
                        for normal in sample.normal {
                            bytes.extend_from_slice(&normal.bits().to_be_bytes());
                        }
                        for projection in sample.projection {
                            bytes.extend_from_slice(&projection.bits().to_be_bytes());
                        }
                        push_len(&mut bytes, sample.tags.len())?;
                        for tag in sample.tags {
                            bytes.extend_from_slice(&tag.tag.0.to_be_bytes());
                            bytes.extend_from_slice(&tag.weight.bits().to_be_bytes());
                        }
                    }
                    None => bytes.push(0),
                }
            }
        }
        let mut field_query_tiles = self.surface_field_query_tiles.clone();
        field_query_tiles.sort_by_key(|tile| {
            (
                tile.node,
                tile.node_semantic_revision,
                tile.channel,
                tile.derivative,
                tile.samples
                    .first()
                    .map(|entry| (entry.candidate, entry.query)),
            )
        });
        push_len(&mut bytes, field_query_tiles.len())?;
        for tile in field_query_tiles {
            bytes.extend_from_slice(&tile.node.to_be_bytes());
            bytes.extend_from_slice(&tile.node_semantic_revision.to_be_bytes());
            push_field_channel(&mut bytes, tile.channel);
            bytes.push(match tile.derivative {
                FieldDerivative::Value => 0,
                FieldDerivative::Gradient => 1,
                FieldDerivative::Hessian => 2,
            });
            bytes.extend_from_slice(&tile.provider_set_hash);
            push_len(&mut bytes, tile.samples.len())?;
            for entry in tile.samples {
                push_candidate_identity(&mut bytes, entry.candidate);
                push_world_position(&mut bytes, entry.query);
                match entry.value {
                    QuantizedSurfaceFieldValue::Scalar(value) => {
                        bytes.push(0);
                        bytes.extend_from_slice(&value.to_be_bytes());
                    }
                    QuantizedSurfaceFieldValue::Gradient(value) => {
                        bytes.push(1);
                        for lane in value {
                            bytes.extend_from_slice(&lane.to_be_bytes());
                        }
                    }
                    QuantizedSurfaceFieldValue::Hessian(value) => {
                        bytes.push(2);
                        for lane in value {
                            bytes.extend_from_slice(&lane.to_be_bytes());
                        }
                    }
                }
            }
        }
        let mut references = self.ancestor_references.clone();
        references.sort();
        references.dedup();
        push_len(&mut bytes, references.len())?;
        for reference in references {
            bytes.extend_from_slice(&reference.canonical_bytes());
        }
        push_len(&mut bytes, self.provenance.decisions().len())?;
        for decision in self.provenance.decisions() {
            push_len(&mut bytes, decision.parents.len())?;
            for parent in &decision.parents {
                bytes.extend_from_slice(&parent.0.to_be_bytes());
            }
            push_len(&mut bytes, decision.subgraph_path.len())?;
            for call in &decision.subgraph_path {
                bytes.extend_from_slice(&call.to_be_bytes());
            }
            bytes.extend_from_slice(&decision.node.to_be_bytes());
            let operator = decision.operator.as_wire().as_bytes();
            push_len(&mut bytes, operator.len())?;
            bytes.extend_from_slice(operator);
            bytes.extend_from_slice(&decision.candidate.to_be_bytes());
            bytes.push(provenance_outcome_byte(decision.outcome));
        }
        push_len(&mut bytes, self.provenance.records().len())?;
        for record in self.provenance.records() {
            bytes.extend_from_slice(&record.map.value().to_be_bytes());
            bytes.extend_from_slice(&record.layer.to_be_bytes());
            bytes.extend_from_slice(&record.biome.value().to_be_bytes());
            bytes.extend_from_slice(&record.decision.0.to_be_bytes());
            bytes.extend_from_slice(&record.candidate.to_be_bytes());
            push_optional_uuid(&mut bytes, record.family);
            match record.plant {
                Some(plant) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&plant.bytes());
                }
                None => bytes.push(0),
            }
            bytes.extend_from_slice(&record.variation.to_be_bytes());
        }
        bytes.extend_from_slice(&self.diagnostics.candidate_count.to_be_bytes());
        bytes.extend_from_slice(&self.diagnostics.accepted_count.to_be_bytes());
        let mut rejected = self.diagnostics.rejected.clone();
        rejected.sort_by_key(|candidate| {
            (
                candidate.candidate,
                rejection_reason_byte(candidate.reason),
                candidate.provenance,
            )
        });
        push_len(&mut bytes, rejected.len())?;
        for candidate in rejected {
            push_candidate_identity(&mut bytes, candidate.candidate);
            bytes.push(rejection_reason_byte(candidate.reason));
            bytes.extend_from_slice(&candidate.provenance.0.to_be_bytes());
        }
        let mut streams = self.diagnostics.streams.clone();
        streams.sort_by_key(|stream| (stream.node.clone(), stream.label.clone(), stream.scope));
        push_len(&mut bytes, streams.len())?;
        for mut stream in streams {
            let node = stream.node.canonical_bytes();
            push_len(&mut bytes, node.len())?;
            bytes.extend_from_slice(&node);
            push_len(&mut bytes, stream.label.len())?;
            bytes.extend_from_slice(stream.label.as_bytes());
            match stream.scope {
                DiagnosticStreamScope::GlobalSnapshot => bytes.push(0),
                DiagnosticStreamScope::CandidateLineage(lineage) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&lineage.0.to_be_bytes());
                }
            }
            match stream.candidates {
                Some(mut candidates) => {
                    bytes.push(1);
                    candidates.sort_by_key(|sample| sample.identity);
                    push_len(&mut bytes, candidates.len())?;
                    for sample in candidates {
                        push_candidate_identity(&mut bytes, sample.identity);
                        bytes.extend_from_slice(&sample.owner.canonical_bytes());
                        push_world_position(&mut bytes, sample.position);
                        push_optional_uuid(&mut bytes, sample.family);
                        bytes.extend_from_slice(&sample.variation.to_be_bytes());
                        bytes.extend_from_slice(&sample.priority.bits().to_be_bytes());
                        bytes.extend_from_slice(&sample.ecology_tick.to_be_bytes());
                    }
                }
                None => bytes.push(0),
            }
            match stream.field {
                Some(mut field) => {
                    bytes.push(1);
                    field.sort_by_key(|sample| sample.candidate);
                    push_len(&mut bytes, field.len())?;
                    for sample in field {
                        push_candidate_identity(&mut bytes, sample.candidate);
                        bytes.extend_from_slice(&sample.value.bits().to_be_bytes());
                    }
                }
                None => bytes.push(0),
            }
            stream.rejected.sort_by_key(|candidate| {
                (
                    candidate.candidate,
                    rejection_reason_byte(candidate.reason),
                    candidate.provenance,
                )
            });
            push_len(&mut bytes, stream.rejected.len())?;
            for candidate in stream.rejected {
                push_candidate_identity(&mut bytes, candidate.candidate);
                bytes.push(rejection_reason_byte(candidate.reason));
                bytes.extend_from_slice(&candidate.provenance.0.to_be_bytes());
            }
        }
        Ok(bytes)
    }
}

fn push_optional_uuid(bytes: &mut Vec<u8>, value: Option<Uuid>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.value().to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_world_position(bytes: &mut Vec<u8>, position: WorldPosition) {
    for tick in position.global_ticks() {
        bytes.extend_from_slice(&tick.to_be_bytes());
    }
}

fn push_candidate_identity(bytes: &mut Vec<u8>, identity: CandidateIdentity) {
    bytes.extend_from_slice(&identity.node.to_be_bytes());
    bytes.extend_from_slice(&identity.node_address.to_be_bytes());
    bytes.extend_from_slice(&identity.node_semantic_revision.to_be_bytes());
    bytes.extend_from_slice(&identity.ordinal.to_be_bytes());
    bytes.extend_from_slice(&identity.ancestor.to_be_bytes());
}

fn push_field_channel(bytes: &mut Vec<u8>, channel: FieldChannel) {
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
    bytes.push(tag);
    if let Some(value) = user {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
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
#[derive(Clone, Debug, Default)]
pub struct GraphCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl GraphCancellationToken {
    /// Cancels every evaluator sharing this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
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

/// Published result and retained-memory accounting for one global-stage tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalStageEvaluationResult {
    /// Stable compiled stage identity.
    pub stage: [u8; 32],
    /// Canonical ancestor cell that owns the solved tile.
    pub owner: WorldCellKey,
    /// Public macro, micro, provenance, and diagnostic products of the stage.
    pub result: GraphEvaluationResult,
    /// Exact evaluator-owned bytes retained for downstream replay in this job.
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

    fn estimated_bytes(&self) -> u64 {
        match self {
            Self::Candidates(stream) => stream.candidates.len() as u64 * 192,
            Self::Surface(surface) => surface.values.len() as u64 * 128,
            Self::Scalar(field) => field.values.len() as u64 * 40,
            Self::Vector(field) => field.values.len() as u64 * 48,
            Self::Hessian(field) => field.values.len() as u64 * 60,
            Self::Macro(points) => points.len() as u64 * 256,
            Self::Micro(tiles) => tiles
                .iter()
                .map(|tile| {
                    let density = (tile.density.len() as u64).saturating_mul(2);
                    let attributes = tile.attributes.values().fold(0_u64, |total, values| {
                        total.saturating_add((values.len() as u64).saturating_mul(4))
                    });
                    density.saturating_add(attributes)
                })
                .fold(0_u64, u64::saturating_add),
            Self::Regions(regions) => regions.len() as u64 * 64,
            Self::Splines(splines) => splines
                .iter()
                .map(|spline| spline.points.len() as u64 * 64)
                .sum(),
            Self::Species(species) => species.len() as u64 * 32,
            Self::Communities(communities) => communities.estimated_bytes(),
            Self::Diagnostics(streams) => streams.iter().fold(0_u64, |total, stream| {
                let candidates = stream.candidates.as_ref().map_or(0, |values| {
                    (values.len() as u64).saturating_mul(128)
                });
                let field = stream.field.as_ref().map_or(0, |values| {
                    (values.len() as u64).saturating_mul(48)
                });
                total
                    .saturating_add(candidates)
                    .saturating_add(field)
                    .saturating_add((stream.rejected.len() as u64).saturating_mul(160))
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GlobalStageCacheKey {
    graph: [u8; 32],
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
        self.tiles.values().try_fold(0_u64, |total, tile| {
            total
                .checked_add(tile.resident_bytes)
                .ok_or(Error::NumericOverflow)
        })
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

impl EvaluationScope<'_> {
    fn global_store(&self) -> &GlobalStageStore {
        match self {
            Self::Cell { global_store } | Self::Global { global_store, .. } => global_store,
        }
    }

    fn current_global_stage(&self) -> Option<&CompiledGlobalStage> {
        match self {
            Self::Cell { .. } => None,
            Self::Global { stage, .. } => Some(stage),
        }
    }

    fn is_cell(&self) -> bool {
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
    fn estimated_bytes(&self) -> u64 {
        (self.competition.len() + self.companions.len() + self.succession.len()) as u64 * 48
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
    let required_halo = fixed_meters_to_ticks(scope.current_global_stage().map_or_else(
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
        field.validate()?;
        validate_field_dependency(graph, inputs, field)?;
    }
    let mut projection_queries = BTreeSet::new();
    for tile in &inputs.surface_projection_tiles {
        tile.validate()?;
        for entry in &tile.samples {
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
        tile.validate()?;
        if tile.provider_set_hash != inputs.surface_provider_set_hash {
            return Err(Error::GraphDocument {
                path: "evaluation.surfaceFieldQueryTiles".to_owned(),
                reason: "field query tile does not match the declared provider set".to_owned(),
            });
        }
        for entry in &tile.samples {
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
        && canonical_surface_provider_set_hash(&inputs.surface_providers)?
            != inputs.surface_provider_set_hash
    {
        return Err(Error::GraphDocument {
            path: "evaluation.surfaceProviderSetHash".to_owned(),
            reason: "surface provider descriptors do not match the declared set identity"
                .to_owned(),
        });
    }
    for provider in &inputs.surface_providers {
        let descriptor = provider.descriptor();
        let source = GraphDependencySource::SurfaceProvider(descriptor.id.0);
        let Some(dependency) = graph
            .dependencies()
            .iter()
            .find(|dependency| dependency.source == source)
        else {
            continue;
        };
        let actual = canonical_surface_provider_set_hash(&[Arc::clone(provider)])?;
        if actual != dependency.content_hash {
            return Err(Error::GraphAuthoritativeInput {
                node: 0,
                input: format!("content hash for {source:?}"),
            });
        }
    }
    for prototype in &inputs.plant_prototypes {
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
    };
    state.check_abort()?;
    let outputs = evaluate_unit(&graph.root, &BTreeMap::new(), &mut state)?;
    state.check_abort()?;
    let mut macro_points = Vec::new();
    let mut micro_fields = Vec::new();
    let mut diagnostic_streams = Vec::new();
    for output in &graph.root.outputs {
        let Some(value) = outputs.get(&output.name) else {
            if state.scope.current_global_stage().is_some() {
                continue;
            }
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "evaluator did not produce output".to_owned(),
            });
        };
        match value {
            GraphValue::Macro(points) => macro_points.extend(points.iter().cloned()),
            GraphValue::Micro(tiles) => micro_fields.extend(tiles.iter().cloned()),
            GraphValue::Diagnostics(streams) => {
                diagnostic_streams.extend(streams.iter().cloned());
            }
            _ => {}
        }
    }
    macro_points.sort_by_key(|point| point.id);
    let mut collision = PlantIdCollisionTable::default();
    for point in &macro_points {
        collision.insert(point.id, point_source_fingerprint(point))?;
    }
    state.diagnostics.accepted_count = macro_points.len() as u64;
    state
        .diagnostics
        .rejected
        .sort_by_key(|value| value.candidate);
    diagnostic_streams.sort_by_key(|stream| (stream.node.clone(), stream.label.clone()));
    state.diagnostics.streams = diagnostic_streams;
    state.check_count(
        "accepted count",
        macro_points.len() as u64,
        graph.limits.max_macro_points,
    )?;
    let columns = PlantPointColumns::from_points(&macro_points)?;
    state.check_abort()?;
    let surface_projection_tiles = state
        .prepared_surface_projections
        .into_iter()
        .map(
            |((node, node_semantic_revision, provider_set_hash), samples)| {
                QuantizedSurfaceProjectionTile {
                    node,
                    node_semantic_revision,
                    samples: samples
                        .into_iter()
                        .map(|(query, sample)| QuantizedSurfaceProjectionEntry { query, sample })
                        .collect(),
                    provider_set_hash,
                }
            },
        )
        .collect();
    let surface_field_query_tiles = state
        .prepared_surface_fields
        .into_iter()
        .map(
            |((node, node_semantic_revision, channel, derivative, provider_set_hash), samples)| {
                QuantizedSurfaceFieldQueryTile {
                    node,
                    node_semantic_revision,
                    channel,
                    derivative,
                    samples: samples
                        .into_iter()
                        .map(
                            |((candidate, query), value)| QuantizedSurfaceFieldQueryEntry {
                                candidate,
                                query,
                                value,
                            },
                        )
                        .collect(),
                    provider_set_hash,
                }
            },
        )
        .collect();
    Ok(PlannedEvaluation {
        result: GraphEvaluationResult {
            cell: inputs.output_cell,
            macro_points: columns,
            micro_fields,
            surface_projection_tiles,
            surface_field_query_tiles,
            ancestor_references: state.ancestor_references.into_iter().collect(),
            provenance: state.provenance,
            diagnostics: state.diagnostics,
        },
        materialized_outputs: state.materialized_outputs,
        candidate_decisions: state.candidate_decisions,
    })
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
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    cancellation: &GraphCancellationToken,
    compute: Option<&dyn GraphComputeExecutor>,
    execution_plan: &GraphExecutionPlan,
    scope: EvaluationScope<'_>,
    deadline: Instant,
) -> Result<PlannedEvaluation> {
    if !inputs.surface_providers.is_empty() && unit_requires_canonical_preparation(&graph.root) {
        let preparation_plan = build_execution_plan(graph, false, None)?;
        let preparation_context = EvaluationContext {
            graph,
            cancellation,
            compute: None,
            execution_plan: &preparation_plan,
            scope,
            deadline,
        };
        let prepared = evaluate_cell_planned(
            &preparation_context,
            inputs,
            EvaluationPass::PrepareCanonicalInputs,
        )?;
        let mut replay_inputs = inputs.clone();
        replay_inputs.surface_projection_tiles = prepared.result.surface_projection_tiles;
        replay_inputs.surface_field_query_tiles = prepared.result.surface_field_query_tiles;
        check_limit(
            "input tiles",
            evaluation_input_tile_count(&replay_inputs)?,
            graph.limits.max_input_tiles,
        )?;
        let replay_context = EvaluationContext {
            graph,
            cancellation,
            compute,
            execution_plan,
            scope,
            deadline,
        };
        return evaluate_cell_planned(
            &replay_context,
            &replay_inputs,
            EvaluationPass::AuthoritativeReplay,
        );
    }
    let context = EvaluationContext {
        graph,
        cancellation,
        compute,
        execution_plan,
        scope,
        deadline,
    };
    evaluate_cell_planned(&context, inputs, EvaluationPass::AuthoritativeReplay)
}

fn unit_requires_canonical_preparation(unit: &CompiledGraphUnit) -> bool {
    unit.nodes.iter().any(|node| {
        (node.definition.authority != GraphAuthority::Cosmetic
            && matches!(
                node.definition.operator,
                GraphOperator::SurfaceProjection | GraphOperator::FieldSample
            ))
            || node
                .module
                .as_deref()
                .is_some_and(unit_requires_canonical_preparation)
    })
}

fn evaluation_deadline(graph: &CompiledBiomeGraph) -> Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_millis(graph.limits.max_time_ms))
        .ok_or(Error::NumericOverflow)
}

#[derive(Clone, Copy, Debug, Default)]
struct SymbolicValueBound {
    domain: Option<GraphDomain>,
    items: u64,
    bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct SymbolicEvaluationBound {
    candidate_peak: u64,
    accepted: u64,
    micro_samples: u64,
    memory_bytes: u64,
    transfer_bytes: u64,
    rejected: u64,
}

fn bound_add(
    resource: &'static str,
    left: u64,
    right: u64,
    limit: u64,
) -> Result<u64> {
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

fn bound_mul(
    resource: &'static str,
    left: u64,
    right: u64,
    limit: u64,
) -> Result<u64> {
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

fn symbolic_linear_value(
    domain: GraphDomain,
    items: u64,
    bytes_per_item: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    Ok(SymbolicValueBound {
        domain: Some(domain),
        items,
        bytes: bound_mul(
            "memory bytes",
            items,
            bytes_per_item,
            limits.max_memory_bytes,
        )?,
    })
}

fn symbolic_candidate_value(
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    check_limit("candidate count", items, limits.max_candidates)?;
    symbolic_linear_value(GraphDomain::Candidates, items, 192, limits)
}

fn symbolic_field_value(
    domain: GraphDomain,
    items: u64,
    limits: crate::GraphSafetyLimits,
) -> Result<SymbolicValueBound> {
    let bytes_per_item = match domain {
        GraphDomain::ScalarField => 40,
        GraphDomain::VectorField => 48,
        GraphDomain::HessianField => 60,
        GraphDomain::SurfaceField => 128,
        _ => {
            return Err(Error::GraphDocument {
                path: "graph.symbolicBound".to_owned(),
                reason: "field bound has a non-field domain".to_owned(),
            });
        }
    };
    symbolic_linear_value(domain, items, bytes_per_item, limits)
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

fn symbolic_spline_candidate_count(
    node: &CompiledGraphNode,
    inputs: &GraphEvaluationInputs,
    limits: crate::GraphSafetyLimits,
) -> Result<u64> {
    let spacing = fixed_parameter(node, "spacing", DecisionScalar::from_bits(0))?;
    let spacing_ticks = fixed_meters_to_ticks(spacing)?.unsigned_abs() as i128;
    if spacing_ticks == 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline spacing must be positive".to_owned(),
        });
    }
    inputs.splines.iter().try_fold(0_u64, |total, spline| {
        let segments = spline_segments(&spline.points)?;
        if segments.is_empty() {
            return Ok(total);
        }
        let length = segments.iter().try_fold(0_i128, |sum, segment| {
            sum.checked_add(segment.length).ok_or(Error::NumericOverflow)
        })?;
        let samples = u64::try_from(length / spacing_ticks)
            .map_err(|_| Error::NumericOverflow)?
            .checked_add(1)
            .ok_or(Error::NumericOverflow)?;
        bound_add("candidate count", total, samples, limits.max_candidates)
    })
}

fn symbolic_node_outputs(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    incoming: &BTreeMap<u128, Vec<&crate::GraphEdge>>,
    values: &BTreeMap<(u128, String), SymbolicValueBound>,
    inputs: &GraphEvaluationInputs,
    bound: &mut SymbolicEvaluationBound,
    limits: crate::GraphSafetyLimits,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    use GraphOperator as O;
    let candidate_input = || required_symbolic_items(node, incoming, values, "candidates");
    let field_input = |pin| required_symbolic_items(node, incoming, values, pin);
    let candidate = |items| symbolic_candidate_value(items, limits);
    let scalar = |items| symbolic_field_value(GraphDomain::ScalarField, items, limits);
    let singleton = |name: &str, value| BTreeMap::from([(name.to_owned(), value)]);
    let outputs = match node.definition.operator {
        O::InterfaceInput | O::ModuleCall => unreachable!("handled by symbolic unit traversal"),
        O::RegionInput => {
            let regions = if inputs.regions.is_empty() {
                canonical_cell_regions(inputs.read_bounds, inputs.output_cell.level(), 0)?.len()
            } else {
                inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .count()
            };
            singleton(
                "regions",
                symbolic_linear_value(GraphDomain::Regions, regions as u64, 64, limits)?,
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
                    bytes: bound_mul("memory bytes", points, 64, limits.max_memory_bytes)?,
                },
            )
        }
        O::SpeciesInput => singleton(
            "species",
            symbolic_linear_value(
                GraphDomain::SpeciesTable,
                unit.palette.len() as u64,
                32,
                limits,
            )?,
        ),
        O::CommunityInput => {
            let tables = CommunityTables {
                competition: unit.competition.clone(),
                companions: unit.companions.clone(),
                succession: unit.succession.clone(),
            };
            singleton(
                "communities",
                SymbolicValueBound {
                    domain: Some(GraphDomain::CommunityTable),
                    items: 1,
                    bytes: tables.estimated_bytes(),
                },
            )
        }
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
        O::StratifiedCoverage | O::BlueNoisePoisson => {
            let regions = required_symbolic_items(node, incoming, values, "regions")?;
            let count = u64::from(u32_parameter(node, "count", 0)?);
            let items = bound_mul("candidate count", regions, count, limits.max_candidates)?;
            singleton("candidates", candidate(items)?)
        }
        O::SurfaceProjection => {
            let items = candidate_input()?;
            bound.rejected = bound_add(
                "diagnostic samples",
                bound.rejected,
                items,
                u64::MAX,
            )?;
            BTreeMap::from([
                ("candidates".to_owned(), candidate(items)?),
                (
                    "surface".to_owned(),
                    symbolic_field_value(GraphDomain::SurfaceField, items, limits)?,
                ),
            ])
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
            let adjacency = bound_mul(
                "memory bytes",
                input,
                u64::from(u32_parameter(node, "maximumNeighbours", 0)?),
                limits.max_memory_bytes / 16,
            )?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                bound_mul("memory bytes", adjacency, 16, limits.max_memory_bytes)?,
                limits.max_memory_bytes,
            )?;
            bound.rejected = bound_add(
                "diagnostic samples",
                bound.rejected,
                input,
                u64::MAX,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::VariableSpacing
        | O::FieldImportance
        | O::PriorityExclusion
        | O::BoundsOverlap
        | O::Competition
        | O::Suitability
        | O::CommunityBlend => {
            let items = candidate_input()?;
            bound.rejected = bound_add(
                "diagnostic samples",
                bound.rejected,
                items,
                u64::MAX,
            )?;
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
                factor = bound_add(
                    "candidate count",
                    factor,
                    generation,
                    limits.max_candidates,
                )?;
            }
            let items = bound_mul(
                "candidate count",
                candidate_input()?,
                factor,
                limits.max_candidates,
            )?;
            singleton("candidates", candidate(items)?)
        }
        O::SplineFollow => singleton(
            "candidates",
            candidate(symbolic_spline_candidate_count(node, inputs, limits)?)?,
        ),
        O::Transform | O::SuccessionInput => {
            singleton("candidates", candidate(candidate_input()?)?)
        }
        O::MacroOutput => {
            let items = candidate_input()?;
            check_limit("accepted count", items, limits.max_macro_points)?;
            bound.rejected = bound_add(
                "diagnostic samples",
                bound.rejected,
                items,
                u64::MAX,
            )?;
            singleton(
                "points",
                symbolic_linear_value(GraphDomain::MacroPoints, items, 256, limits)?,
            )
        }
        O::MicroOutput => {
            let dimensions = u32_vec3_parameter(node, "dimensions")?;
            let mut samples = 1_u64;
            for dimension in dimensions {
                samples = bound_mul(
                    "micro samples",
                    samples,
                    u64::from(dimension),
                    limits.max_micro_samples,
                )?;
            }
            let channels = guid_list_parameter(node, "attributeChannels")?.len() as u64;
            let bytes_per_sample = bound_add(
                "memory bytes",
                10,
                bound_mul("memory bytes", channels, 20, limits.max_memory_bytes)?,
                limits.max_memory_bytes,
            )?;
            singleton(
                "micro",
                SymbolicValueBound {
                    domain: Some(GraphDomain::MicroField),
                    items: samples,
                    bytes: bound_mul(
                        "memory bytes",
                        samples,
                        bytes_per_sample,
                        limits.max_memory_bytes,
                    )?,
                },
            )
        }
        O::DiagnosticOutput => {
            let candidates = symbolic_input(node, incoming, values, "candidates")?
                .map_or(0, |value| value.items);
            let field = symbolic_input(node, incoming, values, "field")?
                .map_or(0, |value| value.items);
            let candidate_bytes = bound_mul(
                "memory bytes",
                candidates,
                128,
                limits.max_memory_bytes,
            )?;
            let field_bytes =
                bound_mul("memory bytes", field, 48, limits.max_memory_bytes)?;
            let rejected_bytes = bound_mul(
                "memory bytes",
                bound.rejected,
                160,
                limits.max_memory_bytes,
            )?;
            let bytes = bound_add(
                "memory bytes",
                bound_add(
                    "memory bytes",
                    candidate_bytes,
                    field_bytes,
                    limits.max_memory_bytes,
                )?,
                rejected_bytes,
                limits.max_memory_bytes,
            )?;
            singleton(
                "diagnostics",
                SymbolicValueBound {
                    domain: Some(GraphDomain::Diagnostics),
                    items: 1,
                    bytes,
                },
            )
        }
    };
    Ok(outputs)
}

fn symbolic_evaluate_unit(
    unit: &CompiledGraphUnit,
    interface_values: &BTreeMap<String, SymbolicValueBound>,
    inputs: &GraphEvaluationInputs,
    plan: &GraphExecutionPlan,
    limits: crate::GraphSafetyLimits,
    bound: &mut SymbolicEvaluationBound,
    values_by_pin: &mut BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    let incoming = unit
        .edges
        .iter()
        .fold(BTreeMap::<u128, Vec<_>>::new(), |mut map, edge| {
            map.entry(edge.to_node).or_default().push(edge);
            map
        });
    let mut values = BTreeMap::<(u128, String), SymbolicValueBound>::new();
    for node in &unit.nodes {
        let outputs = match node.definition.operator {
            GraphOperator::InterfaceInput => {
                let name = string_parameter(node, "name", "")?;
                BTreeMap::from([(
                    "value".to_owned(),
                    *interface_values.get(&name).ok_or_else(|| Error::GraphDocument {
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
                symbolic_evaluate_unit(
                    module,
                    &module_inputs,
                    inputs,
                    plan,
                    limits,
                    bound,
                    values_by_pin,
                )?
            }
            _ => symbolic_node_outputs(unit, node, &incoming, &values, inputs, bound, limits)?,
        };
        for (pin, value) in outputs {
            let candidate_items = if value.domain == Some(GraphDomain::Candidates) {
                value.items
            } else {
                0
            };
            bound.candidate_peak = bound.candidate_peak.max(candidate_items);
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
    let outputs = unit
        .outputs
        .iter()
        .map(|output| {
            Ok((
                output.name.clone(),
                *values
                    .get(&(output.node, output.pin.clone()))
                    .ok_or_else(|| Error::GraphDocument {
                        path: format!("graph.outputs.{}", output.name),
                        reason: "symbolic output is missing".to_owned(),
                    })?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
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
    let _ = plan;
    Ok(outputs)
}

fn symbolic_evaluation_bound(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    plan: &GraphExecutionPlan,
) -> Result<SymbolicEvaluationBound> {
    let mut bound = SymbolicEvaluationBound::default();
    let mut values_by_pin = BTreeMap::new();
    symbolic_evaluate_unit(
        &graph.root,
        &BTreeMap::new(),
        inputs,
        plan,
        graph.limits,
        &mut bound,
        &mut values_by_pin,
    )?;
    let rejection_bytes = bound_mul(
        "memory bytes",
        bound.rejected,
        160,
        graph.limits.max_memory_bytes,
    )?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        rejection_bytes,
        graph.limits.max_memory_bytes,
    )?;
    for group in plan
        .groups
        .iter()
        .filter(|group| group.domain == GraphExecutionDomain::SlangCompute)
    {
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
        let program_words = (GRAPH_GPU_PROGRAM_HEADER_WORDS as u64)
            .checked_add(input_count)
            .and_then(|words| {
                words.checked_add(
                    (group.nodes.len() as u64) * (GRAPH_GPU_INSTRUCTION_WORDS as u64),
                )
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
        let bytes = bound_mul(
            "transfer bytes",
            words,
            4,
            graph.limits.max_transfer_bytes,
        )?;
        bound.transfer_bytes = bound_add(
            "transfer bytes",
            bound.transfer_bytes,
            bytes,
            graph.limits.max_transfer_bytes,
        )?;
    }
    Ok(bound)
}

fn preflight_evaluation_inputs(
    graph: &CompiledBiomeGraph,
    inputs: &[GraphEvaluationInputs],
    worker_count: usize,
    plan: &GraphExecutionPlan,
) -> Result<()> {
    check_limit(
        "output cells",
        inputs.len() as u64,
        graph.limits.max_output_cells,
    )?;
    let mut cells = BTreeSet::new();
    let mut input_tiles = 0_u64;
    let mut input_bytes = 0_u64;
    let mut candidate_count = 0_u64;
    let mut accepted_count = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    let mut worker_memory = Vec::with_capacity(inputs.len());
    for input in inputs {
        if !cells.insert(input.output_cell) {
            return Err(Error::GraphDocument {
                path: "evaluation.outputCells".to_owned(),
                reason: format!("output cell {} is duplicated", input.output_cell),
            });
        }
        input_tiles = input_tiles
            .checked_add(evaluation_input_tile_count(input)?)
            .ok_or(Error::NumericOverflow)?;
        input_bytes = input_bytes
            .checked_add(estimated_input_bytes(input)?)
            .ok_or(Error::NumericOverflow)?;
        let bound = symbolic_evaluation_bound(graph, input, plan)?;
        let estimated_candidates = bound.candidate_peak;
        candidate_count = candidate_count
            .checked_add(estimated_candidates)
            .ok_or(Error::NumericOverflow)?;
        accepted_count = accepted_count
            .checked_add(
                bound.accepted,
            )
            .ok_or(Error::NumericOverflow)?;
        micro_samples = micro_samples
            .checked_add(bound.micro_samples)
            .ok_or(Error::NumericOverflow)?;
        transfer_bytes = transfer_bytes
            .checked_add(
                bound.transfer_bytes,
            )
            .ok_or(Error::NumericOverflow)?;
        worker_memory.push(
            bound.memory_bytes,
        );
    }
    check_limit("input tiles", input_tiles, graph.limits.max_input_tiles)?;
    check_limit(
        "candidate count",
        candidate_count,
        graph.limits.max_candidates,
    )?;
    check_limit(
        "accepted count",
        accepted_count,
        graph.limits.max_macro_points,
    )?;
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
    worker_memory.sort_unstable_by(|left, right| right.cmp(left));
    let active_memory = worker_memory
        .into_iter()
        .take(worker_count)
        .try_fold(input_bytes, |total, value| {
            total.checked_add(value).ok_or(Error::NumericOverflow)
        })?;
    check_limit("memory bytes", active_memory, graph.limits.max_memory_bytes)
}

fn estimated_input_bytes(input: &GraphEvaluationInputs) -> Result<u64> {
    let field_bytes = input.fields.iter().try_fold(0_u64, |total, tile| {
        let bytes = match &tile.values {
            QuantizedFieldTileValues::Scalar(values) => checked_len_bytes(values.len(), 4)?,
            QuantizedFieldTileValues::Gradient(values) => checked_len_bytes(values.len(), 12)?,
            QuantizedFieldTileValues::Hessian(values) => checked_len_bytes(values.len(), 24)?,
        };
        total.checked_add(bytes).ok_or(Error::NumericOverflow)
    })?;
    let projection_bytes =
        input
            .surface_projection_tiles
            .iter()
            .try_fold(0_u64, |total, tile| {
                tile.samples.iter().try_fold(total, |total, entry| {
                    let tags = entry
                        .sample
                        .as_ref()
                        .map(|sample| checked_len_bytes(sample.tags.len(), 16))
                        .transpose()?
                        .unwrap_or(0);
                    total
                        .checked_add(192)
                        .and_then(|value| value.checked_add(tags))
                        .ok_or(Error::NumericOverflow)
                })
            })?;
    let field_query_bytes =
        input
            .surface_field_query_tiles
            .iter()
            .try_fold(0_u64, |total, tile| {
                total
                    .checked_add(
                        (tile.samples.len() as u64)
                            .checked_mul(160)
                            .ok_or(Error::NumericOverflow)?,
                    )
                    .ok_or(Error::NumericOverflow)
            })?;
    let spline_points = input.splines.iter().try_fold(0_u64, |total, spline| {
        total
            .checked_add(u64::try_from(spline.points.len()).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })?;
    let region_bytes = checked_len_bytes(input.regions.len(), 64)?;
    let spline_bytes = spline_points
        .checked_mul(64)
        .ok_or(Error::NumericOverflow)?;
    let anchor_bytes = checked_len_bytes(input.anchors.len(), 256)?;
    let prototype_bytes = checked_len_bytes(input.plant_prototypes.len(), 128)?;
    field_bytes
        .checked_add(projection_bytes)
        .and_then(|value| value.checked_add(field_query_bytes))
        .and_then(|value| value.checked_add(region_bytes))
        .and_then(|value| value.checked_add(spline_bytes))
        .and_then(|value| value.checked_add(anchor_bytes))
        .and_then(|value| value.checked_add(prototype_bytes))
        .ok_or(Error::NumericOverflow)
}

fn evaluation_input_tile_count(input: &GraphEvaluationInputs) -> Result<u64> {
    [
        input.fields.len(),
        input.surface_projection_tiles.len(),
        input.surface_field_query_tiles.len(),
        input.regions.len(),
        input.splines.len(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, count| {
        total
            .checked_add(u64::try_from(count).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)
    })
}

fn checked_len_bytes(len: usize, bytes_per_item: u64) -> Result<u64> {
    u64::try_from(len)
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(bytes_per_item)
        .ok_or(Error::NumericOverflow)
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

fn preflight_evaluation_job(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationJobInputs,
    worker_count: usize,
    plan: &GraphExecutionPlan,
) -> Result<()> {
    preflight_evaluation_inputs(graph, &inputs.cells, worker_count, plan)?;
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

    let expected = expected_global_stage_tiles(graph, &inputs.cells)?;
    let mut actual = BTreeSet::new();
    let mut global_input_tiles = 0_u64;
    let mut global_input_bytes = 0_u64;
    let mut global_memory = 0_u64;
    let mut global_candidates = 0_u64;
    let mut global_accepted = 0_u64;
    let mut global_micro_samples = 0_u64;
    let mut global_transfer_bytes = 0_u64;
    for input in &inputs.global_stages {
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
        global_input_tiles = global_input_tiles
            .checked_add(evaluation_input_tile_count(&input.inputs)?)
            .ok_or(Error::NumericOverflow)?;
        global_input_bytes = global_input_bytes
            .checked_add(estimated_input_bytes(&input.inputs)?)
            .ok_or(Error::NumericOverflow)?;
        let bound = symbolic_evaluation_bound(graph, &input.inputs, plan)?;
        global_memory = global_memory
            .checked_add(bound.memory_bytes)
            .ok_or(Error::NumericOverflow)?;
        global_candidates = global_candidates
            .checked_add(bound.candidate_peak)
            .ok_or(Error::NumericOverflow)?;
        global_accepted = global_accepted
            .checked_add(bound.accepted)
            .ok_or(Error::NumericOverflow)?;
        global_micro_samples = global_micro_samples
            .checked_add(bound.micro_samples)
            .ok_or(Error::NumericOverflow)?;
        global_transfer_bytes = global_transfer_bytes
            .checked_add(bound.transfer_bytes)
            .ok_or(Error::NumericOverflow)?;
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

    let cell_input_tiles = inputs.cells.iter().try_fold(0_u64, |total, input| {
        total
            .checked_add(evaluation_input_tile_count(input)?)
            .ok_or(Error::NumericOverflow)
    })?;
    check_limit(
        "input tiles",
        cell_input_tiles
            .checked_add(global_input_tiles)
            .ok_or(Error::NumericOverflow)?,
        graph.limits.max_input_tiles,
    )?;
    check_limit(
        "candidate count",
        global_candidates,
        graph.limits.max_candidates,
    )?;
    check_limit(
        "accepted count",
        global_accepted,
        graph.limits.max_macro_points,
    )?;
    check_limit(
        "micro samples",
        global_micro_samples,
        graph.limits.max_micro_samples,
    )?;
    check_limit(
        "transfer bytes",
        global_transfer_bytes,
        graph.limits.max_transfer_bytes,
    )?;

    let cell_input_bytes = inputs.cells.iter().try_fold(0_u64, |total, input| {
        total
            .checked_add(estimated_input_bytes(input)?)
            .ok_or(Error::NumericOverflow)
    })?;
    let mut worker_memory = inputs
        .cells
        .iter()
        .map(|input| symbolic_evaluation_bound(graph, input, plan).map(|bound| bound.memory_bytes))
        .collect::<Result<Vec<_>>>()?;
    worker_memory.sort_unstable_by(|left, right| right.cmp(left));
    let active_memory = worker_memory.into_iter().take(worker_count).try_fold(
        cell_input_bytes
            .checked_add(global_input_bytes)
            .and_then(|value| value.checked_add(global_memory))
            .ok_or(Error::NumericOverflow)?,
        |total, value| total.checked_add(value).ok_or(Error::NumericOverflow),
    )?;
    check_limit("memory bytes", active_memory, graph.limits.max_memory_bytes)
}

fn expected_global_stage_tiles(
    graph: &CompiledBiomeGraph,
    cells: &[GraphEvaluationInputs],
) -> Result<BTreeSet<([u8; 32], WorldCellKey)>> {
    let mut expected = BTreeSet::new();
    for stage in graph.spatial_plan().global_stages() {
        for cell in cells {
            for owner in world_cells_covering_bounds(
                cell.read_bounds,
                stage.owner_level,
                graph.limits.max_global_stage_tiles,
            )? {
                expected.insert((stage.id, owner));
            }
        }
    }

    loop {
        let mut additions = Vec::new();
        for (stage_id, owner) in expected.iter().copied() {
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
                    graph.limits.max_global_stage_tiles,
                )? {
                    if !expected.contains(&(prerequisite, prerequisite_owner)) {
                        additions.push((prerequisite, prerequisite_owner));
                    }
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        expected.extend(additions);
        check_limit(
            "global stage tiles",
            expected.len() as u64,
            graph.limits.max_global_stage_tiles,
        )?;
    }
    Ok(expected)
}

fn retained_global_tile_bytes(evaluated: &PlannedEvaluation) -> Result<u64> {
    let outputs = evaluated
        .materialized_outputs
        .values()
        .try_fold(0_u64, |total, value| {
            total
                .checked_add(value.estimated_bytes())
                .ok_or(Error::NumericOverflow)
        })?;
    let result_bytes = u64::try_from(evaluated.result.canonical_bytes()?.len())
        .map_err(|_| Error::NumericOverflow)?;
    let decisions = checked_len_bytes(evaluated.candidate_decisions.len(), 64)?;
    outputs
        .checked_add(result_bytes)
        .and_then(|value| value.checked_add(decisions))
        .ok_or(Error::NumericOverflow)
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
    inputs.cells.sort_by_key(|input| input.output_cell);
    let stage_order = graph
        .spatial_plan()
        .global_stages()
        .iter()
        .enumerate()
        .map(|(index, stage)| (stage.id, index))
        .collect::<BTreeMap<_, _>>();
    inputs.global_stages.sort_by_key(|input| {
        (
            stage_order.get(&input.stage).copied().unwrap_or(usize::MAX),
            input.owner,
            input.input_snapshot,
        )
    });
    let workers = worker_count.min(inputs.cells.len().max(1));
    let gpu = compute.map(|compute| GraphGpuScheduling {
        profile: compute.profile(),
        qualifications: compute.qualifications(),
    });
    let execution_plan = build_execution_plan(graph, workers > 1, gpu)?;
    preflight_evaluation_job(graph, &inputs, worker_count, &execution_plan)?;
    let deadline = evaluation_deadline(graph)?;
    let mut global_store = GlobalStageStore::default();
    let mut global_results = Vec::with_capacity(inputs.global_stages.len());
    for input in inputs.global_stages {
        if cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        let stage = graph
            .spatial_plan()
            .global_stage(input.stage)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.globalStages".to_owned(),
                reason: "global-stage identity is absent from the compiled graph".to_owned(),
            })?;
        let evaluated = evaluate_cell_atomically(
            graph,
            &input.inputs,
            cancellation,
            compute,
            &execution_plan,
            EvaluationScope::Global {
                stage,
                global_store: &global_store,
            },
            deadline,
        )?;
        for output in &stage.output_pins {
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
        let resident_bytes = retained_global_tile_bytes(&evaluated)?;
        let public_result = evaluated.result;
        let tile = GlobalStageTile {
            key: GlobalStageCacheKey {
                graph: graph.identity,
                stage: input.stage,
                map: input.inputs.map.value(),
                biome_instance: input.inputs.biome_instance,
                owner: input.owner,
                input_snapshot: input.input_snapshot,
            },
            outputs: evaluated.materialized_outputs,
            provenance: public_result.provenance.clone(),
            candidate_decisions: evaluated.candidate_decisions,
            resident_bytes,
        };
        global_store.insert(tile)?;
        check_limit(
            "memory bytes",
            global_store.resident_bytes()?,
            graph.limits.max_memory_bytes,
        )?;
        global_results.push(GlobalStageEvaluationResult {
            stage: input.stage,
            owner: input.owner,
            result: public_result,
            resident_bytes,
        });
    }

    let mut indexed = inputs.cells.into_iter().enumerate().collect::<Vec<_>>();
    let mut shards = (0..workers).map(|_| Vec::new()).collect::<Vec<_>>();
    for (index, input) in indexed.drain(..) {
        shards[index % workers].push((index, input));
    }
    let execution_plan = &execution_plan;
    let global_store = &global_store;
    let mut results = std::thread::scope(|scope| {
        let handles = shards
            .into_iter()
            .map(|shard| {
                scope.spawn(move || {
                    shard
                        .into_iter()
                        .map(|(index, input)| {
                            (
                                index,
                                evaluate_cell_atomically(
                                    graph,
                                    &input,
                                    cancellation,
                                    compute,
                                    execution_plan,
                                    EvaluationScope::Cell { global_store },
                                    deadline,
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().map_err(|_| Error::GraphDocument {
                    path: "parallel-evaluator".to_owned(),
                    reason: "worker thread panicked".to_owned(),
                })
            })
            .collect::<Result<Vec<_>>>()
    })?
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    results.sort_by_key(|(index, _)| *index);
    let cell_results = results
        .into_iter()
        .map(|(_, result)| result.map(|evaluated| evaluated.result))
        .collect::<Result<Vec<_>>>()?;
    let mut all_results = global_results
        .iter()
        .map(|global| &global.result)
        .collect::<Vec<_>>();
    all_results.extend(cell_results.iter());
    validate_result_totals(
        graph,
        all_results
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
            .as_slice(),
    )?;
    Ok(GraphEvaluationJobResult {
        cells: cell_results,
        global_stages: global_results,
    })
}

fn validate_result_totals(
    graph: &CompiledBiomeGraph,
    results: &[GraphEvaluationResult],
) -> Result<()> {
    let mut candidates = 0_u64;
    let mut accepted = 0_u64;
    let mut micro_samples = 0_u64;
    let mut transfer_bytes = 0_u64;
    for result in results {
        candidates = candidates
            .checked_add(result.diagnostics.candidate_count)
            .ok_or(Error::NumericOverflow)?;
        accepted = accepted
            .checked_add(result.diagnostics.accepted_count)
            .ok_or(Error::NumericOverflow)?;
        for tile in &result.micro_fields {
            micro_samples = micro_samples
                .checked_add(tile.density.len() as u64)
                .ok_or(Error::NumericOverflow)?;
        }
        for node in &result.diagnostics.nodes {
            transfer_bytes = transfer_bytes
                .checked_add(node.transfer_bytes)
                .ok_or(Error::NumericOverflow)?;
        }
        for group in &result.diagnostics.gpu_groups {
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
    )
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
    members: &BTreeSet<u128>,
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
    if members.contains(&edge.from_node) {
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
    let members = group
        .nodes
        .iter()
        .map(|node| node.address.node)
        .collect::<BTreeSet<_>>();
    let nodes = unit
        .nodes
        .iter()
        .filter(|node| members.contains(&node.definition.guid))
        .collect::<Vec<_>>();
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

    let mut input_types = vec![GraphGpuRegisterType::CandidateMask];
    let mut bindings = vec![ResidentInputBinding::CandidateMask];
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

    let mut instructions = Vec::with_capacity(nodes.len());
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
                let curve = DecisionCurve::new(curve_parameter(node, "curve")?)?;
                GraphGpuInstruction::Curve {
                    destination,
                    input: resident_source_register(
                        unit,
                        &members,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "field",
                    )?,
                    points: curve
                        .points()
                        .iter()
                        .map(|(x, y)| (x.bits(), y.bits()))
                        .collect(),
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
                        &members,
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
                    &members,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "left",
                )?,
                right: resident_source_register(
                    unit,
                    &members,
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
                    &members,
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
                        &members,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "candidates",
                    )?,
                    weights: resident_source_register(
                        unit,
                        &members,
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

    let identities = if let Some(stream) = candidate_stream {
        stream
            .candidates
            .iter()
            .map(|candidate| candidate.identity)
            .collect::<Vec<_>>()
    } else {
        boundary_values
            .values()
            .find_map(|value| match value {
                GraphValue::Scalar(field) => Some(field.values.keys().copied().collect()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let candidates = candidate_stream
        .into_iter()
        .flat_map(|stream| &stream.candidates)
        .map(|candidate| (candidate.identity, candidate))
        .collect::<BTreeMap<_, _>>();
    let node_by_guid = nodes
        .iter()
        .map(|node| (node.definition.guid, *node))
        .collect::<BTreeMap<_, _>>();
    let mut invocations = Vec::with_capacity(identities.len());
    let mut base_masks = Vec::with_capacity(identities.len());
    for identity in &identities {
        let base_mask = external_scalar_keys.iter().all(|key| {
            boundary_values.get(key).is_some_and(|value| match value {
                GraphValue::Scalar(field) => field.values.contains_key(identity),
                _ => false,
            })
        });
        base_masks.push(base_mask);
        let candidate = candidates.get(identity).copied();
        let mut noise_components = BTreeMap::new();
        for node in noise_inputs.keys() {
            let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                path: node_by_guid[node].debug_symbol.label.clone(),
                reason: "resident noise input has no candidate position".to_owned(),
            })?;
            let frequency = fixed_parameter(
                node_by_guid[node],
                "frequency",
                DecisionScalar::from_bits(65_536),
            )?;
            let channel = u32_parameter(node_by_guid[node], "channel", 0)?;
            noise_components.insert(
                *node,
                coherent_value_noise_components(
                    node_by_guid[node],
                    state,
                    candidate.position,
                    frequency,
                    channel,
                )?,
            );
        }
        let mut values = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            values.push(match binding {
                ResidentInputBinding::CandidateMask => GraphGpuValue::CandidateMask(base_mask),
                ResidentInputBinding::ExternalScalar(key) => {
                    let value = boundary_values.get(key).and_then(|value| match value {
                        GraphValue::Scalar(field) => field.values.get(identity),
                        _ => None,
                    });
                    GraphGpuValue::FixedScalar(value.map_or(0, |value| value.bits()))
                }
                ResidentInputBinding::NoiseCorner { node, corner } => {
                    GraphGpuValue::FixedScalar(noise_components[node].0[*corner].bits())
                }
                ResidentInputBinding::NoiseBlend { node, axis } => {
                    GraphGpuValue::Unit(noise_components[node].1[*axis].bits())
                }
                ResidentInputBinding::GradientPosition { node, axis } => {
                    let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                        path: node_by_guid[node].debug_symbol.label.clone(),
                        reason: "resident gradient input has no candidate position".to_owned(),
                    })?;
                    GraphGpuValue::WorldTick(candidate.position.global_ticks()[*axis])
                }
                ResidentInputBinding::GradientOrigin { node, axis } => GraphGpuValue::WorldTick(
                    world_position_parameter(node_by_guid[node], "exactOrigin")?[*axis],
                ),
            });
        }
        invocations.push(GraphGpuInvocation::new(&program, values)?);
    }
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
                );
            }
        }
        let accepted = CandidateStream {
            lineage: stream.lineage,
            candidates: accepted,
        };
        let decision_inputs = boundary_values
            .values()
            .filter_map(|value| match value {
                GraphValue::Candidates(stream) => Some((
                    "candidates".to_owned(),
                    GraphValue::Candidates(stream.clone()),
                )),
                GraphValue::Scalar(field) => {
                    Some(("weights".to_owned(), GraphValue::Scalar(field.clone())))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let decision_outputs = singleton("candidates", GraphValue::Candidates(accepted.clone()));
        record_candidate_decisions(node, &decision_inputs, &decision_outputs, state);
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
        invocation_count: invocations.len() as u64,
    })
}

fn evaluate_unit(
    unit: &CompiledGraphUnit,
    interface_values: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    let node_addresses = unit
        .nodes
        .iter()
        .map(|node| (node.definition.guid, node.address()))
        .collect::<BTreeMap<_, _>>();
    let mut active_nodes = unit
        .nodes
        .iter()
        .filter(|node| state.should_visit_node(node))
        .map(|node| node.definition.guid)
        .collect::<BTreeSet<_>>();
    loop {
        let mut changed = false;
        let active_destinations = active_nodes.clone();
        for edge in unit
            .edges
            .iter()
            .filter(|edge| active_destinations.contains(&edge.to_node))
        {
            let address =
                node_addresses
                    .get(&edge.from_node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: "graph.spatialPlan".to_owned(),
                        reason: "upstream node address is missing".to_owned(),
                    })?;
            if state.should_load_global_node(address) {
                continue;
            }
            changed |= active_nodes.insert(edge.from_node);
        }
        if !changed {
            break;
        }
    }
    let incoming = unit
        .edges
        .iter()
        .filter(|edge| active_nodes.contains(&edge.to_node))
        .fold(BTreeMap::<u128, Vec<_>>::new(), |mut map, edge| {
            map.entry(edge.to_node).or_default().push(edge);
            map
        });
    let mut remaining_uses = BTreeMap::<(u128, String), u64>::new();
    for edge in unit.edges.iter().filter(|edge| {
        active_nodes.contains(&edge.from_node) && active_nodes.contains(&edge.to_node)
    }) {
        *remaining_uses
            .entry((edge.from_node, edge.from_pin.clone()))
            .or_default() += 1;
    }
    for output in unit
        .outputs
        .iter()
        .filter(|output| active_nodes.contains(&output.node))
    {
        *remaining_uses
            .entry((output.node, output.pin.clone()))
            .or_default() += 1;
    }
    let mut values: BTreeMap<(u128, String), GraphValue> = BTreeMap::new();
    let mut live_bytes = 0_u64;
    let mut executed_resident_nodes = BTreeSet::new();
    for node in &unit.nodes {
        if !active_nodes.contains(&node.definition.guid) {
            continue;
        }
        state.check_abort()?;
        let address = node.address();
        let scheduled_group = state
            .execution_plan
            .group_for(&address)
            .cloned()
            .ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "execution plan omitted the compiled node".to_owned(),
            })?;
        if scheduled_group.domain == GraphExecutionDomain::SlangCompute {
            if executed_resident_nodes.contains(&node.definition.guid) {
                continue;
            }
            let members = scheduled_group
                .nodes
                .iter()
                .map(|member| member.address.node)
                .collect::<BTreeSet<_>>();
            if members.iter().any(|member| {
                !active_nodes.contains(member)
                    || unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == *member)
                        .is_none_or(|candidate| state.should_load_global_node(&candidate.address()))
            }) {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident group crosses the active spatial evaluation boundary"
                        .to_owned(),
                });
            }
            let mut boundary_values = BTreeMap::new();
            for edge in unit.edges.iter().filter(|edge| {
                !members.contains(&edge.from_node) && members.contains(&edge.to_node)
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
                        .load_global_node_outputs(source_node)?
                        .remove(&edge.from_pin)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident boundary input is missing".to_owned(),
                        })?
                };
                boundary_values.insert((edge.from_node, edge.from_pin.clone()), value);
            }
            let input_bytes = boundary_values.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.estimated_bytes())
                    .ok_or(Error::NumericOverflow)
            })?;
            let started = Instant::now();
            let transferred_before = state.transferred_bytes;
            let result = evaluate_resident_group(unit, &scheduled_group, &boundary_values, state)?;
            state.check_abort()?;
            let transfer_bytes = state
                .transferred_bytes
                .checked_sub(transferred_before)
                .ok_or(Error::NumericOverflow)?;
            let output_bytes = result.outputs.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.estimated_bytes())
                    .ok_or(Error::NumericOverflow)
            })?;
            let transient_bytes = live_bytes
                .checked_add(input_bytes)
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
                            .checked_add(value.estimated_bytes())
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
                let node_outputs = result
                    .outputs
                    .iter()
                    .filter(|((source, _), _)| *source == member.address.node)
                    .map(|((_, pin), value)| (pin.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>();
                state.capture_global_outputs(compiled, &node_outputs)?;
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
                        .checked_add(value.estimated_bytes())
                        .ok_or(Error::NumericOverflow)?;
                    values.insert(key, value);
                }
            }
            for edge in unit.edges.iter().filter(|edge| {
                !members.contains(&edge.from_node) && members.contains(&edge.to_node)
            }) {
                let key = (edge.from_node, edge.from_pin.clone());
                let Some(uses) = remaining_uses.get_mut(&key) else {
                    continue;
                };
                *uses = uses.checked_sub(1).ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident input live range was consumed more than once".to_owned(),
                })?;
                if *uses == 0 {
                    if let Some(value) = values.remove(&key) {
                        live_bytes = live_bytes
                            .checked_sub(value.estimated_bytes())
                            .ok_or(Error::NumericOverflow)?;
                    }
                }
            }
            executed_resident_nodes.extend(members);
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
                let address =
                    node_addresses
                        .get(&edge.from_node)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "upstream node address is missing".to_owned(),
                        })?;
                let stage = state
                    .graph
                    .spatial_plan()
                    .global_stage_for_node(address)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream value is missing".to_owned(),
                    })?;
                let source_node = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == edge.from_node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream compiled node is missing".to_owned(),
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
                    .load_global_node_outputs(source_node)?
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
        let input_bytes = inputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.estimated_bytes())
                .ok_or(Error::NumericOverflow)
        })?;
        let started = Instant::now();
        let transferred_before = state.transferred_bytes;
        let (outputs, execution_domain, loaded_global) =
            evaluate_node_scheduled(unit, node, &inputs, interface_values, state)?;
        state.check_abort()?;
        if !loaded_global {
            record_candidate_decisions(node, &inputs, &outputs, state);
            state.capture_global_outputs(node, &outputs)?;
        }
        let output_candidates = outputs
            .values()
            .map(GraphValue::candidate_count)
            .max()
            .unwrap_or(0) as u64;
        let output_bytes = outputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.estimated_bytes())
                .ok_or(Error::NumericOverflow)
        })?;
        state.check_count(
            "candidate count",
            output_candidates,
            state.graph.limits.max_candidates,
        )?;
        let transient_bytes = live_bytes
            .checked_add(input_bytes)
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
                    .checked_add(value.estimated_bytes())
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
            if *uses == 0 {
                if let Some(value) = values.remove(&key) {
                    live_bytes = live_bytes
                        .checked_sub(value.estimated_bytes())
                        .ok_or(Error::NumericOverflow)?;
                }
            }
        }
    }
    let mut outputs = BTreeMap::new();
    for output in &unit.outputs {
        if let Some(value) = values.get(&(output.node, output.pin.clone())).cloned() {
            outputs.insert(output.name.clone(), value);
        } else if state.scope.current_global_stage().is_none() {
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
) {
    let mut candidates = BTreeMap::new();
    for value in outputs.values() {
        if let GraphValue::Candidates(stream) = value {
            for candidate in &stream.candidates {
                candidates.entry(candidate.identity).or_insert(candidate);
            }
        }
    }
    for candidate in candidates.into_values() {
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
            for stream in inputs.values().filter_map(|value| match value {
                GraphValue::Candidates(stream) => Some(stream),
                _ => None,
            }) {
                for ancestor in &stream.candidates {
                    if ancestor.identity.ordinal == candidate.identity.ancestor {
                        if let Some(parent) = state.candidate_decisions.get(&ancestor.identity) {
                            parents.insert(*parent);
                        }
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
}

fn evaluate_node_scheduled(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    interface_values: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<(BTreeMap<String, GraphValue>, GraphExecutionDomain, bool)> {
    if state.should_load_global_node(&node.address()) {
        return Ok((
            state.load_global_node_outputs(node)?,
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
            evaluate_node(unit, node, inputs, interface_values, state)?,
            GraphExecutionDomain::ReferenceCpu,
            false,
        )),
        GraphExecutionDomain::ParallelCpu => Ok((
            evaluate_node(unit, node, inputs, interface_values, state)?,
            GraphExecutionDomain::ParallelCpu,
            false,
        )),
    }
}

fn execute_compute_program(
    state: &mut EvaluationState<'_>,
    compute: &dyn GraphComputeExecutor,
    program: &GraphGpuProgram,
    invocations: &[GraphGpuInvocation],
) -> Result<Vec<crate::GraphGpuOutput>> {
    if invocations.is_empty() {
        return Ok(Vec::new());
    }
    state.check_abort()?;
    let invocation_words = invocations.iter().try_fold(0_u64, |total, invocation| {
        total
            .checked_add(
                u64::try_from(invocation.words().len()).map_err(|_| Error::NumericOverflow)?,
            )
            .ok_or(Error::NumericOverflow)
    })?;
    let output_words = u64::try_from(invocations.len())
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(
            u64::try_from(crate::GRAPH_GPU_OUTPUT_WORDS).map_err(|_| Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let transfer_bytes = u64::try_from(program.words().len())
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
    if outputs.len() != invocations.len() {
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
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    use GraphOperator as O;
    let output = match node.definition.operator {
        O::InterfaceInput => {
            let name = string_parameter(node, "name", "")?;
            let value =
                interface_values
                    .get(&name)
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
                state
                    .inputs
                    .regions
                    .iter()
                    .filter(|region| region.kind == EvaluationRegionKind::Biome)
                    .copied()
                    .collect()
            }),
        ),
        O::SplineInput => singleton("splines", GraphValue::Splines(state.inputs.splines.clone())),
        O::SpeciesInput => singleton("species", GraphValue::Species(unit.palette.clone())),
        O::CommunityInput => singleton(
            "communities",
            GraphValue::Communities(CommunityTables {
                competition: unit.competition.clone(),
                companions: unit.companions.clone(),
                succession: unit.succession.clone(),
            }),
        ),
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
            let (surface, retained) =
                project_candidates(node, candidates_input(inputs, "candidates")?, state)?;
            state.diagnostics.candidate_count = state
                .diagnostics
                .candidate_count
                .max(retained.candidates.len() as u64);
            BTreeMap::from([
                ("candidates".to_owned(), GraphValue::Candidates(retained)),
                ("surface".to_owned(), GraphValue::Surface(surface)),
            ])
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
            evaluate_unit(module, inputs, state)?
        }
    };
    Ok(output)
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
    let mut candidates = Vec::new();
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
                ]),
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
    let mut candidates =
        Vec::with_capacity(usize::try_from(requested).map_err(|_| Error::NumericOverflow)?);
    for region in regions {
        let side = integer_sqrt_ceil(count);
        for local in 0..count {
            state.check_abort()?;
            let ordinal = candidate_ordinal(node, state, region.id, local, 0);
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
    let mut accepted = Vec::new();
    for region in regions {
        let seed_ordinal = candidate_ordinal(node, state, region.id, 0, 1);
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
        let mut region_points = vec![seed.clone()];
        let mut active = vec![seed];
        let mut proposal = 1_u64;
        while !active.is_empty() && (region_points.len() as u64) < count {
            state.check_abort()?;
            let parent = active.remove(0);
            let mut produced = false;
            for attempt in 0..attempts {
                let ordinal = candidate_ordinal(node, state, region.id, proposal, 1);
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
                if region_points.len() as u64 >= count {
                    break;
                }
            }
            if produced {
                active.push(parent);
            }
        }
        accepted.extend(region_points);
    }
    accepted.sort_by_key(|candidate| candidate.identity);
    let mut globally_spaced: Vec<GraphCandidate> = Vec::new();
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
    state: &mut EvaluationState<'_>,
) -> Result<(ProjectedSurfaceSamples, CandidateStream)> {
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let mut tiles = state
        .inputs
        .surface_projection_tiles
        .iter()
        .filter(|tile| {
            tile.node == node.definition.guid
                && tile.node_semantic_revision == node.definition.semantic_revision
        })
        .collect::<Vec<_>>();
    tiles.sort_by_key(|tile| {
        (
            tile.samples.first().map(|entry| entry.query),
            tile.samples.last().map(|entry| entry.query),
        )
    });
    if authoritative
        && (state.inputs.surface_provider_set_hash == [0; 32]
            || tiles
                .iter()
                .any(|tile| tile.provider_set_hash != state.inputs.surface_provider_set_hash))
    {
        return Err(Error::GraphAuthoritativeInput {
            node: node.definition.guid,
            input: "canonical surface projection tiles".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    let mut retained = Vec::new();
    for candidate in &candidates.candidates {
        state.check_abort()?;
        let projected = if authoritative {
            let sample = if let Some(sample) = tiles
                .iter()
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
            values.insert(candidate.identity, projected);
            retained.push(candidate.clone());
        } else {
            reject_candidate(
                node,
                candidate,
                candidates.lineage,
                CandidateRejectionReason::SurfaceMiss,
                candidate.family,
                candidate.variation,
                state,
            );
        }
    }
    Ok((
        ProjectedSurfaceSamples {
            lineage: candidates.lineage,
            values,
        },
        CandidateStream {
            lineage: candidates.lineage,
            candidates: retained,
        },
    ))
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
    let mut ordered = providers.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|provider| provider.descriptor().id);
    let mut best: Option<((u64, u64), SurfaceHit)> = None;
    for provider in ordered {
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
        if !contains_required_tags(&hit.tags, &required_tags)
            || !contains_required_tags(&hit.tags, &required_material_tags)
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

fn quantize_surface_normal(normal: [f32; 3]) -> Result<[SignedUnit; 3]> {
    normal
        .map(|value| SignedUnit::from_f64(f64::from(value)))
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| Error::NumericOverflow)
}

fn quantize_surface_projection(projection: [f64; 3]) -> Result<[DecisionScalar; 3]> {
    projection
        .map(DecisionScalar::from_f64)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| Error::NumericOverflow)
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
    let mut tiles = state
        .inputs
        .fields
        .iter()
        .filter(|tile| {
            matches!(tile.source, EvaluationFieldSource::SurfaceProvider { .. })
                && tile.channel == channel
                && tile.derivative == derivative
        })
        .collect::<Vec<_>>();
    tiles.sort_by_key(|tile| {
        (
            tile.bounds.min_ticks(),
            tile.bounds.max_ticks_exclusive(),
            tile.source,
            tile.source_hash,
        )
    });
    let mut sampling = FieldSamplingContext {
        node,
        candidates,
        authoritative,
        require_authoritative,
        channel,
        derivative,
        tiles: &tiles,
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
    tiles: &'a [&'a EvaluationFieldTile],
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
        let mut providers = self
            .state
            .inputs
            .surface_providers
            .iter()
            .collect::<Vec<_>>();
        providers.sort_by_key(|provider| provider.descriptor().id);
        let mut query_tiles = self
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
            .collect::<Vec<_>>();
        query_tiles.sort_by_key(|tile| {
            (
                tile.samples
                    .first()
                    .map(|entry| (entry.candidate, entry.query)),
                tile.samples
                    .last()
                    .map(|entry| (entry.candidate, entry.query)),
            )
        });
        let mut values = BTreeMap::new();
        for candidate in &self.candidates.candidates {
            let sample = if self.authoritative {
                let exact = query_tiles
                    .iter()
                    .find_map(|tile| tile.sample(candidate.identity, candidate.position))
                    .and_then(&decode_query);
                let tiled = self
                    .tiles
                    .iter()
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
                    for provider in &providers {
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
                providers.iter().find_map(|provider| {
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
    let mut tiles = state
        .inputs
        .fields
        .iter()
        .filter(|tile| {
            tile.source == EvaluationFieldSource::MapLayer(layer)
                && tile.channel == channel
                && tile.derivative == FieldDerivative::Value
        })
        .collect::<Vec<_>>();
    tiles.sort_by_key(|tile| {
        (
            tile.layer_order,
            tile.bounds.min_ticks(),
            tile.bounds.max_ticks_exclusive(),
            tile.source,
            tile.source_hash,
        )
    });
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        if let Some(value) = sample_ordered_scalar_tiles(&tiles, candidate.position)? {
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

fn sample_ordered_scalar_tiles(
    tiles: &[&EvaluationFieldTile],
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
        let ordinal = stable_ordinal(&[&address, &x, &y, &z, &channel_bytes]);
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
    let curve = DecisionCurve::new(curve)?;
    let values = input
        .values
        .iter()
        .map(|(identity, value)| {
            let bits = value.bits().clamp(0, i32::from(u16::MAX)) as u16;
            Ok((*identity, curve.sample(UnitInterval::from_bits(bits))?))
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
                    .map(|segment| {
                        point_segment_distance_ticks(candidate.position, segment[0], segment[1])
                    })
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .min()
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
                    .map(|region| point_bounds_distance_ticks(candidate.position, region.bounds))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .min()
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
                let mut tiles = state
                    .inputs
                    .fields
                    .iter()
                    .filter(|tile| {
                        tile.channel == channel
                            && tile.derivative == FieldDerivative::Value
                            && match tile.source {
                                EvaluationFieldSource::MapLayer(layer) => {
                                    source_guid == 0 || layer == source_guid
                                }
                                EvaluationFieldSource::SurfaceProvider { .. } => source_guid == 0,
                            }
                    })
                    .collect::<Vec<_>>();
                tiles.sort_by_key(|tile| tile.layer_order);
                let sampled =
                    sample_ordered_scalar_tiles(&tiles, candidate.position)?.ok_or_else(|| {
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
    let candidate_weights = candidates
        .candidates
        .iter()
        .map(|candidate| {
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
            Ok(weight)
        })
        .collect::<Result<Vec<_>>>()?;
    let radius_squared = radius.checked_mul(radius).ok_or(Error::NumericOverflow)?;
    let mut buckets: BTreeMap<(i128, i128), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        buckets
            .entry((ticks[0].div_euclid(radius), ticks[2].div_euclid(radius)))
            .or_default()
            .push(index);
    }
    let mut adjacency = vec![Vec::<(usize, u64)>::new(); candidates.candidates.len()];
    let mut edge_count = 0_u64;
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        let ticks = candidate.position.global_ticks();
        let bucket = (ticks[0].div_euclid(radius), ticks[2].div_euclid(radius));
        for x in -1..=1 {
            for z in -1..=1 {
                let Some(neighbours) = buckets.get(&(bucket.0 + x, bucket.1 + z)) else {
                    continue;
                };
                for &other_index in neighbours.iter().filter(|other| **other > index) {
                    let other = &candidates.candidates[other_index];
                    let distance_squared = distance_squared_xz(candidate.position, other.position)?;
                    if distance_squared >= radius_squared {
                        continue;
                    }
                    let distance = integer_sqrt(distance_squared);
                    let contribution =
                        u64::try_from(radius.checked_sub(distance).ok_or(Error::NumericOverflow)?)
                            .map_err(|_| Error::NumericOverflow)?;
                    adjacency[index].push((other_index, contribution));
                    adjacency[other_index].push((index, contribution));
                    if adjacency[index].len() > maximum_neighbours
                        || adjacency[other_index].len() > maximum_neighbours
                    {
                        return Err(Error::GraphLimit {
                            resource: "weighted-elimination neighbours",
                            requested: u64::try_from(
                                adjacency[index].len().max(adjacency[other_index].len()),
                            )
                            .unwrap_or(u64::MAX),
                            limit: u64::try_from(maximum_neighbours).unwrap_or(u64::MAX),
                        });
                    }
                    edge_count = edge_count.checked_add(1).ok_or(Error::NumericOverflow)?;
                    let adjacency_bytes =
                        edge_count.checked_mul(32).ok_or(Error::NumericOverflow)?;
                    if adjacency_bytes > state.graph.limits.max_memory_bytes {
                        return Err(Error::GraphLimit {
                            resource: "weighted-elimination adjacency bytes",
                            requested: adjacency_bytes,
                            limit: state.graph.limits.max_memory_bytes,
                        });
                    }
                }
            }
        }
    }
    let baseline = u64::try_from(radius).map_err(|_| Error::NumericOverflow)?;
    let mut crowding = adjacency
        .iter()
        .map(|neighbours| {
            neighbours
                .iter()
                .try_fold(baseline, |sum, (_, contribution)| {
                    sum.checked_add(*contribution).ok_or(Error::NumericOverflow)
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut generations = vec![0_u32; candidates.candidates.len()];
    let mut active = vec![true; candidates.candidates.len()];
    let mut heap = BinaryHeap::new();
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        heap.push(EliminationScore {
            crowding: crowding[index],
            weight: candidate_weights[index],
            identity: candidate.identity,
            generation: 0,
            index,
        });
    }
    let mut active_count = candidates.candidates.len();
    while active_count > target {
        state.check_abort()?;
        let score = loop {
            let score = heap.pop().ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "weighted-elimination heap exhausted".to_owned(),
            })?;
            if active[score.index] && generations[score.index] == score.generation {
                break score;
            }
        };
        active[score.index] = false;
        active_count -= 1;
        for &(neighbour, contribution) in &adjacency[score.index] {
            if !active[neighbour] {
                continue;
            }
            crowding[neighbour] = crowding[neighbour]
                .checked_sub(contribution)
                .ok_or(Error::NumericOverflow)?;
            generations[neighbour] = generations[neighbour]
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?;
            heap.push(EliminationScore {
                crowding: crowding[neighbour],
                weight: candidate_weights[neighbour],
                identity: candidates.candidates[neighbour].identity,
                generation: generations[neighbour],
                index: neighbour,
            });
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
            );
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: candidates
            .candidates
            .iter()
            .enumerate()
            .filter(|(index, _)| active[*index])
            .map(|(_, candidate)| candidate)
            .cloned()
            .collect(),
    };
    result.canonicalize()?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EliminationScore {
    crowding: u64,
    weight: u32,
    identity: CandidateIdentity,
    generation: u32,
    index: usize,
}

impl Ord for EliminationScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (u128::from(self.crowding) * u128::from(other.weight))
            .cmp(&(u128::from(other.crowding) * u128::from(self.weight)))
            .then_with(|| self.identity.cmp(&other.identity))
            .then_with(|| self.generation.cmp(&other.generation))
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
    let prototype_aware = bool_parameter(node, "prototypeAware", false)?;
    let effective_radii = candidates
        .candidates
        .iter()
        .map(|candidate| {
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
            Ok((
                candidate.identity,
                nonnegative_radius_ticks(node, "variable-spacing radius sample", radius)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let maximum_radius = effective_radii.values().copied().max().unwrap_or(0);
    let maximum_support = if prototype_aware {
        maximum_radius
            .checked_mul(2)
            .ok_or(Error::NumericOverflow)?
    } else {
        maximum_radius
    };
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut immutable = candidates.candidates.clone();
    immutable.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    let mut buckets = BTreeMap::<(i128, i128), Vec<usize>>::new();
    for candidate in immutable {
        let radius_ticks = *effective_radii.get(&candidate.identity).ok_or_else(|| {
            Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "variable-spacing radius sample".to_owned(),
            }
        })?;
        ensure_support_ticks(node, radius_ticks)?;
        let mut wins = true;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                for index in buckets.get(&key).into_iter().flatten() {
                    let other = accepted.get(*index).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "variable-spacing spatial index is invalid".to_owned(),
                    })?;
                    let other_radius_ticks =
                        *effective_radii.get(&other.identity).ok_or_else(|| {
                            Error::GraphAuthoritativeInput {
                                node: node.definition.guid,
                                input: "variable-spacing radius sample".to_owned(),
                            }
                        })?;
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
                }
            }
        }
        if wins {
            buckets.entry(bucket).or_default().push(accepted.len());
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
            );
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted,
    };
    result.canonicalize()?;
    Ok(result)
}

fn competition_claims(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    communities: &CommunityTables,
    state: &mut EvaluationState<'_>,
) -> Result<CandidateStream> {
    let crown_weight = unit_parameter(node, "crownWeight", UnitInterval::ZERO)?;
    let root_weight = unit_parameter(node, "rootWeight", UnitInterval::ZERO)?;
    if crown_weight == UnitInterval::ZERO && root_weight == UnitInterval::ZERO {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "competition crown and root weights cannot both be zero".to_owned(),
        });
    }
    let radii = candidates
        .candidates
        .iter()
        .map(|candidate| {
            let family = candidate
                .family
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "plant family before competition".to_owned(),
                })?;
            let prototype = prototype_for_family(state, family)?;
            let crown = prototype.crown_radius[0].max(prototype.crown_radius[1]);
            let root = prototype.root_radius[0].max(prototype.root_radius[1]);
            let crown =
                crown.checked_mul(DecisionScalar::from_bits(i32::from(crown_weight.bits())))?;
            let root =
                root.checked_mul(DecisionScalar::from_bits(i32::from(root_weight.bits())))?;
            Ok((
                candidate.identity,
                fixed_meters_to_ticks(crown.checked_add(root)?)?.unsigned_abs() as i128,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let maximum_radius = radii.values().copied().max().unwrap_or(0);
    let maximum_pair_spacing = communities
        .competition
        .iter()
        .map(|rule| fixed_meters_to_ticks(rule.spacing).map(|value| value.unsigned_abs() as i128))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let maximum_support = maximum_radius
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?
        .max(maximum_pair_spacing);
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    let mut buckets = BTreeMap::<(i128, i128), Vec<usize>>::new();
    for (index, candidate) in candidates.candidates.iter().enumerate() {
        buckets
            .entry(xz_bucket(candidate.position, bucket_size))
            .or_default()
            .push(index);
    }
    let mut accepted = Vec::new();
    for candidate in &candidates.candidates {
        let candidate_radius =
            *radii
                .get(&candidate.identity)
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "competition radius".to_owned(),
                })?;
        let mut loses = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                for index in buckets.get(&key).into_iter().flatten() {
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
                    let other_radius = *radii.get(&other.identity).ok_or_else(|| {
                        Error::GraphAuthoritativeInput {
                            node: node.definition.guid,
                            input: "competition radius".to_owned(),
                        }
                    })?;
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
            );
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
            );
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
            );
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
    let mut expanded = candidates.candidates.clone();
    for parent in &candidates.candidates {
        for child in 0..children {
            let ordinal =
                candidate_ordinal(node, state, candidate_key_u128(parent.identity), child, 3);
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
    let mut rules = unit.companions.clone();
    rules.sort_by_key(|rule| (rule.parent.value(), rule.child.value()));
    let mut expanded = candidates.candidates.clone();
    let mut frontier = candidates.candidates.clone();
    for depth in 1..=maximum_depth {
        let mut next = Vec::new();
        for parent in &frontier {
            let eligible = rules
                .iter()
                .filter(|rule| parent.family.is_some_and(|family| rule.parent == family))
                .collect::<Vec<_>>();
            for child in 0..children {
                let ordinal = candidate_ordinal(
                    node,
                    state,
                    candidate_key_u128(parent.identity),
                    child,
                    u32::try_from(depth).map_err(|_| Error::NumericOverflow)?,
                );
                let stream = random_stream(
                    node,
                    state,
                    "companions",
                    RandomSampleAddress::new(parent.owner, ordinal)
                        .with_ancestor(parent.identity.ordinal)
                        .with_species(parent.family.map_or(0, |family| u128::from(family.value())))
                        .with_channel(u32::try_from(depth).map_err(|_| Error::NumericOverflow)?),
                )?;
                let rule = if eligible.is_empty() {
                    None
                } else {
                    Some(
                        eligible[usize::try_from(
                            u64::from(stream.lane(0, 0)) % eligible.len() as u64,
                        )
                        .map_err(|_| Error::NumericOverflow)?],
                    )
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
        next.sort_by_key(|candidate| candidate.identity);
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
    let stage_regions = stage_regions(node, &state.inputs.regions, state)?;
    let mut ordered = splines.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|spline| spline.id);
    if ordered.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "spline identities must be unique".to_owned(),
        });
    }
    let mut candidates = Vec::new();
    for spline in ordered {
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
                );
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
    let mut samples = Vec::with_capacity(
        usize::try_from(sample_count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or(Error::NumericOverflow)?,
    );
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
            let offset_ticks = [offset.x, offset.y, offset.z]
                .map(fixed_meters_to_ticks)
                .into_iter()
                .collect::<Result<Vec<_>>>()?;
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
    let keep_highest = bool_parameter(node, "keepHighest", true)?;
    let mut transformed = candidates.candidates.clone();
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
    let radius_ticks = candidates
        .candidates
        .iter()
        .map(|candidate| {
            let radius = radii
                .values
                .get(&candidate.identity)
                .copied()
                .ok_or_else(|| Error::GraphAuthoritativeInput {
                    node: node.definition.guid,
                    input: "priority-exclusion radius sample".to_owned(),
                })?;
            Ok((
                candidate.identity,
                nonnegative_radius_ticks(node, "priority-exclusion radius sample", radius)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let maximum_support = radius_ticks
        .values()
        .copied()
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    transformed.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let mut accepted: Vec<GraphCandidate> = Vec::new();
    let mut buckets = BTreeMap::<(i128, i128), Vec<usize>>::new();
    for candidate in transformed {
        let radius = *radius_ticks.get(&candidate.identity).ok_or_else(|| {
            Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: "priority-exclusion radius sample".to_owned(),
            }
        })?;
        let mut excluded = false;
        let bucket = xz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for z in -1..=1 {
                let key = offset_xz_bucket(bucket, x, z)?;
                for index in buckets.get(&key).into_iter().flatten() {
                    let other = accepted.get(*index).ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "priority-exclusion spatial index is invalid".to_owned(),
                    })?;
                    let other_radius = *radius_ticks.get(&other.identity).ok_or_else(|| {
                        Error::GraphAuthoritativeInput {
                            node: node.definition.guid,
                            input: "priority-exclusion radius sample".to_owned(),
                        }
                    })?;
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
            );
        } else {
            buckets.entry(bucket).or_default().push(accepted.len());
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
    let padding = fixed_parameter(node, "padding", DecisionScalar::from_bits(0))?;
    let padding_ticks = fixed_meters_to_ticks(padding)?.unsigned_abs() as i128;
    let mut ordered = candidates
        .candidates
        .iter()
        .cloned()
        .map(|candidate| {
            let bounds =
                expand_bounds_checked(candidate_bounds(&candidate, state)?, padding_ticks)?;
            let radius = bounds_support_radius(candidate.position, bounds)?;
            ensure_support_ticks(node, radius)?;
            Ok((candidate, bounds, radius))
        })
        .collect::<Result<Vec<_>>>()?;
    let maximum_support = ordered
        .iter()
        .map(|(_, _, radius)| *radius)
        .max()
        .unwrap_or(0)
        .checked_mul(2)
        .ok_or(Error::NumericOverflow)?;
    ensure_support_ticks(node, maximum_support)?;
    let bucket_size = maximum_support.max(1);
    ordered.sort_by(|left, right| {
        right
            .0
            .priority
            .cmp(&left.0.priority)
            .then_with(|| left.0.identity.cmp(&right.0.identity))
    });
    let mut accepted: Vec<(GraphCandidate, WorldBounds, i128)> = Vec::new();
    let mut buckets = BTreeMap::<(i128, i128, i128), Vec<usize>>::new();
    for (candidate, bounds, radius) in ordered {
        let mut overlaps = false;
        let bucket = xyz_bucket(candidate.position, bucket_size);
        'neighbours: for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let key = offset_xyz_bucket(bucket, x, y, z)?;
                    for index in buckets.get(&key).into_iter().flatten() {
                        let (_, other_bounds, other_radius) =
                            accepted.get(*index).ok_or_else(|| Error::GraphDocument {
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
            );
        } else {
            buckets.entry(bucket).or_default().push(accepted.len());
            accepted.push((candidate, bounds, radius));
        }
    }
    let mut result = CandidateStream {
        lineage: candidates.lineage,
        candidates: accepted
            .into_iter()
            .map(|(candidate, _, _)| candidate)
            .collect(),
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
    let mut result = Vec::new();
    for source_candidate in &candidates.candidates {
        let mut candidate = source_candidate.clone();
        let shade_value = shade
            .and_then(|field| field.values.get(&candidate.identity))
            .map_or(0, |value| value.bits().clamp(0, i32::from(u16::MAX)) as u16);
        let mut weights =
            unit.palette
                .iter()
                .try_fold(BTreeMap::<u64, u64>::new(), |mut weights, entry| {
                    let prototype = prototype_for_family(state, entry.plant)?;
                    let tolerance = prototype
                        .shade_tolerance
                        .bits()
                        .saturating_add(shade_bias.bits());
                    let shade_scale =
                        u64::from(u16::MAX.saturating_sub(shade_value.saturating_sub(tolerance)));
                    weights.insert(
                        entry.plant.value(),
                        u64::from(entry.weight.bits())
                            .checked_mul(shade_scale)
                            .ok_or(Error::NumericOverflow)?,
                    );
                    Ok::<_, Error>(weights)
                })?;
        let mut succession = communities.succession.clone();
        succession.sort_by_key(|rule| (rule.minimum_tick, rule.from.value(), rule.to.value()));
        for rule in succession
            .iter()
            .filter(|rule| state.inputs.ecology_tick >= rule.minimum_tick)
        {
            let from = weights.get(&rule.from.value()).copied().unwrap_or(0);
            let transfer = from
                .checked_mul(u64::from(rule.probability.bits()))
                .ok_or(Error::NumericOverflow)?
                / u64::from(u16::MAX);
            if transfer == 0 {
                continue;
            }
            weights.insert(rule.from.value(), from - transfer);
            let to = weights.get(&rule.to.value()).copied().unwrap_or(0);
            weights.insert(
                rule.to.value(),
                to.checked_add(transfer).ok_or(Error::NumericOverflow)?,
            );
        }
        if let Some(parent) = candidate.parent.and_then(|parent| parent.family) {
            let mut companions = communities.companions.clone();
            companions.sort_by_key(|rule| (rule.parent.value(), rule.child.value()));
            for rule in companions.iter().filter(|rule| rule.parent == parent) {
                let child = weights.get(&rule.child.value()).copied().unwrap_or(0);
                let boost = child
                    .checked_mul(u64::from(rule.probability.bits()))
                    .ok_or(Error::NumericOverflow)?
                    / u64::from(u16::MAX);
                weights.insert(
                    rule.child.value(),
                    child.checked_add(boost).ok_or(Error::NumericOverflow)?,
                );
            }
        }
        let total = weights.values().try_fold(0_u64, |total, weight| {
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
            );
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
        for (family, weight) in weights {
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
    let mut rules = unit.succession.clone();
    rules.sort_by_key(|rule| (rule.from.value(), rule.minimum_tick, rule.to.value()));
    let mut result = candidates.clone();
    for candidate in &mut result.candidates {
        let Some(family) = candidate.family else {
            candidate.ecology_tick = state.inputs.ecology_tick;
            continue;
        };
        if let Some(rule) = rules
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
            ]);
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
                candidate.crown_radius =
                    prototype.crown_radius[0].max(prototype.crown_radius[1]);
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
        node.debug_symbol.label.clone()
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
        label,
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
    let mut owned = Vec::new();
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
            );
            continue;
        }
        owned.push(candidate);
    }

    let mut resolved = Vec::with_capacity(owned.len());
    for candidate in owned {
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
            );
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
    let ids_by_candidate = resolved
        .iter()
        .map(|(candidate, _, id)| (candidate.identity, *id))
        .collect::<BTreeMap<_, _>>();

    let mut points = Vec::with_capacity(resolved.len());
    for (candidate, family, id) in resolved {
        let authored = candidate.authored_point.as_ref();
        let parent = match candidate.parent {
            Some(reference) => ids_by_candidate
                .get(&reference.identity)
                .copied()
                .map(Some)
                .unwrap_or(resolve_candidate_reference(
                    node, reference, species, state,
                )?),
            None => authored.and_then(|point| point.parent),
        };
        let colony = match candidate.colony {
            Some(reference) => ids_by_candidate
                .get(&reference.identity)
                .copied()
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
    let count = dimensions.iter().try_fold(1_u64, |product, value| {
        product
            .checked_mul(u64::from(*value))
            .ok_or(Error::NumericOverflow)
    })?;
    state.check_count("micro samples", count, state.graph.limits.max_micro_samples)?;
    let bytes_per_sample = (channels.len() as u64)
        .checked_mul(20)
        .and_then(|value| value.checked_add(10))
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "memory bytes",
        count
            .checked_mul(bytes_per_sample)
            .ok_or(Error::NumericOverflow)?,
        state.graph.limits.max_memory_bytes,
    )?;
    let sample_count = usize::try_from(count).map_err(|_| Error::NumericOverflow)?;
    let mut samples = vec![0_u16; sample_count];
    let mut attribute_weights = vec![0_u64; sample_count];
    let attribute_fields = channels
        .iter()
        .map(|channel| {
            let name = format!("attribute-{channel:032x}");
            let field = scalar_input(inputs, &name)?;
            ensure_lineage(node, &name, candidates.lineage, field.lineage)?;
            Ok((*channel, field))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut attribute_sums = channels
        .iter()
        .map(|channel| (*channel, vec![0_i128; sample_count]))
        .collect::<BTreeMap<_, _>>();
    let bounds = state.inputs.output_bounds;
    for candidate in &candidates.candidates {
        if candidate.owner != state.inputs.output_cell {
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
            let values = sums
                .into_iter()
                .zip(&attribute_weights)
                .map(|(sum, weight)| {
                    if *weight == 0 {
                        return Ok(0);
                    }
                    i32::try_from(div_round_ties_even(sum, i128::from(*weight))?)
                        .map_err(|_| Error::NumericOverflow)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((channel, values))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(vec![MicroFieldTile {
        cell: state.inputs.output_cell,
        dimensions,
        density: samples,
        attributes,
        reconstruction_seed: seed_namespace(node, "reconstruction")?,
    }])
}

impl EvaluationState<'_> {
    fn should_visit_node(&self, node: &CompiledGraphNode) -> bool {
        let address = node.address();
        let Some(stage) = self.scope.current_global_stage() else {
            return self
                .graph
                .spatial_plan()
                .global_stage_for_node(&address)
                .is_none_or(|global| {
                    global
                        .output_pins
                        .iter()
                        .any(|output| output.node == address)
                });
        };
        if stage.closure.contains(&address) {
            return true;
        }
        if node.definition.operator != GraphOperator::ModuleCall {
            return false;
        }
        let mut nested_path = address.module_path;
        nested_path.push(address.node);
        stage
            .closure
            .iter()
            .any(|candidate| candidate.module_path.starts_with(&nested_path))
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
        let tiles = owners
            .into_iter()
            .map(|owner| self.scope.global_store().tile(stage.id, owner).cloned())
            .collect::<Result<Vec<_>>>()?;
        let mut outputs = BTreeMap::new();
        for output in node.outputs.iter().filter(|output| {
            stage.output_pins.contains(&QualifiedGraphPin {
                node: address.clone(),
                pin: output.name.clone(),
            })
        }) {
            let pin = QualifiedGraphPin {
                node: address.clone(),
                pin: output.name.clone(),
            };
            let mut merged = None;
            for tile in &tiles {
                let value = tile.outputs.get(&pin).ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!(
                        "materialized global stage omitted boundary pin '{}'",
                        output.name
                    ),
                })?;
                let value = import_global_value(value.clone(), tile, self)?;
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

    fn check_abort(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(Error::GraphCancelled);
        }
        if Instant::now() > self.deadline {
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
    let decision_roots = candidate_ids
        .iter()
        .filter_map(|identity| tile.candidate_decisions.get(identity).copied())
        .collect::<Vec<_>>();
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
            } else if !provenance_decision_descends_from(
                &state.provenance,
                existing,
                destination,
            )? {
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

    let record_handles = match &value {
        GraphValue::Macro(points) => points
            .iter()
            .map(|point| ProvenanceHandle(point.provenance))
            .collect::<Vec<_>>(),
        GraphValue::Diagnostics(streams) => streams
            .iter()
            .flat_map(|stream| stream.rejected.iter())
            .map(|rejected| rejected.provenance)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
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
    let mut pending = vec![descendant];
    let mut visited = BTreeSet::new();
    while let Some(handle) = pending.pop() {
        if handle == ancestor {
            return Ok(true);
        }
        if !visited.insert(handle) {
            continue;
        }
        let decision = table
            .decision(handle)
            .ok_or_else(|| Error::GraphDocument {
                path: "evaluation.globalStages.provenance".to_owned(),
                reason: "candidate decision handle is missing".to_owned(),
            })?;
        pending.extend(decision.parents.iter().copied());
    }
    Ok(false)
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
                    .chain(
                        stream
                            .field
                            .iter()
                            .flatten()
                            .map(|sample| sample.candidate),
                    )
            })
            .collect(),
        _ => BTreeSet::new(),
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
            left.candidates = candidates.into_values().collect();
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
            *left = points.into_values().collect();
        }
        (GraphValue::Micro(left), GraphValue::Micro(right)) => {
            let mut tiles = left
                .drain(..)
                .map(|tile| (tile.cell, tile))
                .collect::<BTreeMap<_, _>>();
            for tile in right {
                insert_identical(&mut tiles, tile.cell, tile, node)?;
            }
            *left = tiles.into_values().collect();
        }
        (GraphValue::Regions(left), GraphValue::Regions(right)) => {
            let mut regions = left
                .drain(..)
                .map(|region| ((region.id, region.seed_cell), region))
                .collect::<BTreeMap<_, _>>();
            for region in right {
                insert_identical(&mut regions, (region.id, region.seed_cell), region, node)?;
            }
            *left = regions.into_values().collect();
        }
        (GraphValue::Splines(left), GraphValue::Splines(right)) => {
            let mut splines = left
                .drain(..)
                .map(|spline| (spline.id, spline))
                .collect::<BTreeMap<_, _>>();
            for spline in right {
                insert_identical(&mut splines, spline.id, spline, node)?;
            }
            *left = splines.into_values().collect();
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
                *left = candidates.into_values().collect();
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
                *left = field.into_values().collect();
            }
            (None, None) => {}
            _ => return Err(global_merge_conflict(node)),
        }
        existing.rejected.append(&mut stream.rejected);
        existing.rejected.sort_by_key(|value| {
            (
                value.candidate,
                rejection_reason_byte(value.reason),
                value.provenance,
            )
        });
        existing.rejected.dedup();
    }
    *destination = streams.into_values().collect();
    Ok(())
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
) {
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
    state.diagnostics.rejected.push(rejected.clone());
    state
        .rejected_by_lineage
        .entry(lineage)
        .or_default()
        .push(rejected);
}

fn select_species(
    node: &CompiledGraphNode,
    owner: WorldCellKey,
    identity: CandidateIdentity,
    species: &[crate::BiomePaletteEntry],
    state: &EvaluationState<'_>,
) -> Result<Option<Uuid>> {
    let mut species = species.to_vec();
    species.sort_by_key(|entry| entry.plant.value());
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
    for entry in species {
        let weight = u64::from(entry.weight.bits());
        if selection < weight {
            return Ok(Some(entry.plant));
        }
        selection -= weight;
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
    Ok(result.into_values().collect())
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

fn stable_ordinal(parts: &[&[u8]]) -> u64 {
    let mut bytes = b"saffron-anima/vegetation-candidate/v1\0".to_vec();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    u64::from_be_bytes(sha256(&bytes)[..8].try_into().unwrap())
}

fn candidate_ordinal(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    source: u128,
    local: u64,
    channel: u32,
) -> u64 {
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
    let maximum = maximum.map(|value| value.checked_add(1).ok_or(Error::NumericOverflow));
    let maximum = maximum
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .map_err(|_| Error::NumericOverflow)?;
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

fn point_source_fingerprint(point: &PlantPoint) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(&point.deterministic_key.to_be_bytes());
    bytes[16..24].copy_from_slice(&point.candidate.to_be_bytes());
    bytes[24..32].copy_from_slice(&point.family.value().to_be_bytes());
    bytes
}

fn push_len(bytes: &mut Vec<u8>, value: usize) -> Result<()> {
    bytes.extend_from_slice(
        &u64::try_from(value)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    );
    Ok(())
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

fn string_parameter(node: &CompiledGraphNode, name: &str, fallback: &str) -> Result<String> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::String(value)) => Ok(value.clone()),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback.to_owned()),
    }
}

fn curve_parameter(
    node: &CompiledGraphNode,
    name: &str,
) -> Result<Vec<(UnitInterval, DecisionScalar)>> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Curve(value)) => Ok(value.clone()),
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

fn tag_list_parameter(node: &CompiledGraphNode, name: &str) -> Result<Vec<u64>> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::TagList(value)) => Ok(value.clone()),
        Some(_) => wrong_parameter(node, name),
        None => Ok(Vec::new()),
    }
}

fn guid_list_parameter(node: &CompiledGraphNode, name: &str) -> Result<Vec<u128>> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::GuidList(value)) => Ok(value.clone()),
        Some(_) => wrong_parameter(node, name),
        None => Ok(Vec::new()),
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
        BiomeGraphDocument, BiomeGraphPolicy, BiomeGraphResolver, BiomePaletteEntry, BiomeRole,
        GpuExecutionProfile, GpuQualificationRegistry, GpuShaderArtifactIdentity,
        GraphCompileOptions, GraphDependencySource, GraphEdge, GraphInterfaceOutput,
        GraphNodeDefinition, GraphParameterValue, GraphSink, NodeSpatialPolicy,
        compile_biome_graph, evaluate_gpu_program_reference,
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
            invocations: &[GraphGpuInvocation],
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
            Ok(evaluate_gpu_program_reference(program, invocations))
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
            invocations: &[GraphGpuInvocation],
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
            Ok(evaluate_gpu_program_reference(program, invocations))
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
            |program, invocations| Ok(evaluate_gpu_program_reference(program, invocations)),
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
            |program, invocations| Ok(evaluate_gpu_program_reference(program, invocations)),
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
                output, importance, clamp, combine, remap, curve, noise, species, coverage, region,
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

    fn job(cells: Vec<GraphEvaluationInputs>) -> GraphEvaluationJobInputs {
        GraphEvaluationJobInputs {
            cells,
            global_stages: Vec::new(),
        }
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
        let bound = symbolic_evaluation_bound(&graph, &inputs, &plan).unwrap();
        let actual = evaluate_cell_reference(
            &graph,
            &inputs,
            &GraphCancellationToken::default(),
        )
        .unwrap();
        let actual_node_bytes = actual
            .diagnostics
            .nodes
            .iter()
            .map(|node| node.output_bytes)
            .sum::<u64>();

        assert!(bound.candidate_peak >= actual.diagnostics.candidate_count);
        assert!(bound.accepted >= actual.macro_points.row_count().unwrap() as u64);
        assert!(bound.memory_bytes >= actual_node_bytes);
        assert_eq!(bound.transfer_bytes, 0);
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
        assert!(matches!(
            &error,
            Error::GraphLimit {
                resource: "candidate count",
                requested,
                limit: 30,
            } if *requested == expected
        ), "{error:?}");
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
        let bound = symbolic_evaluation_bound(&graph, &inputs, &plan).unwrap();
        let actual = BiomeGraphEvaluator::new(Arc::clone(&graph), 1)
            .unwrap()
            .with_compute_executor(compute)
            .evaluate(
                job(vec![inputs]),
                &GraphCancellationToken::default(),
            )
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
        let graph = Arc::new(compile_fixture(0));
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
            vec![
                [0, 0, 0],
                [5, 0, 0],
                [10, 0, 0],
                [10, 0, 5],
                [10, 0, 10],
            ]
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
