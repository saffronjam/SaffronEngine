//! Candidate, sample, region, prototype, and quantized tile value types.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_spatial::{
    DecisionHessian3, DecisionScalar, DecisionVec3, FieldChannel, FieldDerivative, SignedUnit,
    SurfaceAttachment, SurfaceProviderId, SurfaceRevision, UnitInterval, WeightedSurfaceTag,
    WorldBounds, WorldCellKey, WorldPosition,
};

use crate::{
    Error, FieldBlendOperator, InteractionPolicy, PlantId, PlantPoint, QuantizedOrientation, Result,
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
    pub identity: CandidateIdentity,
    pub owner: WorldCellKey,
    /// Stable authored layer that owns the candidate lineage.
    pub source_layer: u128,
    /// Family already assigned by an upstream community decision.
    pub family: Option<Uuid>,
    pub variation: u32,
    /// Authored identity when the source is an explicit plant.
    pub authored_id: Option<PlantId>,
}

impl CandidateReference {
    pub(super) fn from_candidate(candidate: &GraphCandidate) -> Self {
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
    pub identity: CandidateIdentity,
    pub owner: WorldCellKey,
    /// Stable authored layer that owns this candidate lineage.
    pub source_layer: u128,
    pub position: WorldPosition,
    pub orientation: QuantizedOrientation,
    /// Q15.16 scale.
    pub scale: [DecisionScalar; 3],
    /// Selected plant family, when assigned.
    pub family: Option<Uuid>,
    pub variation: u32,
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

/// Interned identity of one candidate stream.
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
    pub(super) fn canonicalize(&mut self) -> Result<()> {
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
    pub attachment: Option<SurfaceAttachment>,
    pub normal: [SignedUnit; 3],
    /// Canonical provider-local projection coordinate.
    pub projection: [DecisionScalar; 3],
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
    /// Gameplay interaction policy a procedurally scattered point inherits.
    pub interaction_policy: InteractionPolicy,
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
            interaction_policy: asset.interaction_policy,
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
    pub dimensions: [u32; 3],
    /// Typed Q15.16 values in X-major, then Y, then Z order.
    pub values: QuantizedFieldTileValues,
}

/// Packed canonical field values whose lane shape matches the derivative domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuantizedFieldTileValues {
    Scalar(Vec<i32>),
    Gradient(Vec<[i32; 3]>),
    /// Symmetric Hessians in `xx, xy, xz, yy, yz, zz` order.
    Hessian(Vec<[i32; 6]>),
}

/// One exact quantized surface-field value prepared for an authoritative candidate query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantizedSurfaceFieldValue {
    Scalar(i32),
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
    pub node_semantic_revision: u32,
    pub channel: FieldChannel,
    pub derivative: FieldDerivative,
    /// Sorted exact candidate queries.
    pub samples: Vec<QuantizedSurfaceFieldQueryEntry>,
    /// Content identity of the complete provider set used for preparation.
    pub provider_set_hash: [u8; 32],
}

impl QuantizedSurfaceFieldQueryTile {
    pub(super) fn sample(
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
    pub attachment: SurfaceAttachment,
    /// Quantized geometric normal.
    pub normal: [SignedUnit; 3],
    /// Canonical provider-local projection coordinate.
    pub projection: [DecisionScalar; 3],
    pub tags: Vec<WeightedSurfaceTag>,
}

/// Runtime-authoritative, node-specific quantized surface projection tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuantizedSurfaceProjectionTile {
    /// Surface-projection node GUID.
    pub node: u128,
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
