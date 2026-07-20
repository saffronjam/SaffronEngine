//! The three authored vegetation asset models and immutable base-manifest contract.

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_spatial::{
    DecisionScalar, FieldChannel, SurfaceProviderId, UnitInterval, WorldBounds, WorldCellKey,
};

use crate::hash::sha256;
use crate::{
    BrushGestureMetadata, InteractionPolicy, PlantId, PlantPoint, PlantStateOverride,
    PlantTransformOverride, ProvenanceTable, Result, VegetationLayer, point_schema_hash,
};

/// Current `.splant` document version.
pub const PLANT_ASSET_VERSION: u32 = 2;
/// Current `.sbiome` document version.
pub const BIOME_ASSET_VERSION: u32 = 1;
/// Current `.svegmap` manifest version.
pub const VEGETATION_MAP_VERSION: u32 = 1;
/// Current sparse authored map-chunk version.
pub const VEGETATION_MAP_CHUNK_VERSION: u32 = 2;
/// Current immutable base-manifest version.
pub const VEGETATION_BASE_MANIFEST_VERSION: u32 = 1;

/// Asset/source licensing and provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceProvenance {
    /// Source tool/provider identifier.
    pub source: String,
    /// Source page/file URI retained for attribution and reimport.
    pub source_uri: String,
    /// SPDX-style license identifier.
    pub license_id: String,
    /// Human author/creator.
    pub author: String,
    /// Whether visible attribution is required.
    pub requires_attribution: bool,
}

/// Coordinate units declared by an imported plant-family recipe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceUnits {
    /// Metres.
    #[default]
    Meters,
    /// Centimetres.
    Centimeters,
    /// Millimetres.
    Millimeters,
    /// Imperial feet.
    Feet,
}

/// Source up/forward axes normalized during import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceAxis {
    /// Positive X.
    PositiveX,
    /// Negative X.
    NegativeX,
    /// Positive Y.
    #[default]
    PositiveY,
    /// Negative Y.
    NegativeY,
    /// Positive Z.
    PositiveZ,
    /// Negative Z.
    NegativeZ,
}

/// One source asset referenced by an imported-family recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantSourceReference {
    /// Stable source identity inside the recipe.
    pub id: u128,
    /// Project/catalog source asset when imported into the project.
    pub asset: Option<Uuid>,
    /// Canonical source URI/path.
    pub uri: String,
    /// SHA-256 content identity.
    pub content_hash: [u8; 32],
}

/// Settings that normalize an imported family into Anima's plant vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantImportSettings {
    /// Source coordinate units.
    pub units: SourceUnits,
    /// Source up axis.
    pub up_axis: SourceAxis,
    /// Source forward axis.
    pub forward_axis: SourceAxis,
    /// Uniform Q15.16 post-unit scale.
    pub scale: DecisionScalar,
    /// Merge geometrically identical semantic parts.
    pub merge_identical_parts: bool,
    /// Generate missing tangent frames at cook time.
    pub generate_tangents: bool,
}

/// One imported-family source recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedPlantFamilyRecipe {
    /// Source files/assets.
    pub sources: Vec<PlantSourceReference>,
    /// Reimport normalization settings.
    pub settings: PlantImportSettings,
    /// Source semantic-name to stable part ID mapping.
    pub semantic_part_mapping: Vec<(String, u128)>,
    /// Licensing and origin.
    pub provenance: SourceProvenance,
}

/// Embedded native botanical graph source.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeBotanicalGraph {
    /// Graph schema identity.
    pub schema_hash: [u8; 32],
    /// Canonical graph document. Nodes carry stable GUIDs and typed ports.
    pub graph: Value,
}

/// Exactly one source for a plant family.
#[derive(Clone, Debug, PartialEq)]
pub enum PlantFamilySource {
    /// Imported-family recipe normalized during cooking.
    Imported(ImportedPlantFamilyRecipe),
    /// Embedded native botanical graph.
    Native(NativeBotanicalGraph),
}

/// Semantic role of a plant part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantPartSemantic {
    /// Primary trunk/stem.
    Trunk,
    /// Branch or secondary stem.
    Branch,
    /// Root structure.
    Root,
    /// Frond.
    Frond,
    /// Leaf.
    Leaf,
    /// Flower.
    Flower,
    /// Fruit/seed body.
    Fruit,
    /// Grass/reed blade.
    Blade,
}

/// One normalized semantic part of a family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPart {
    /// Stable family-local part identity.
    pub id: u128,
    /// Optional stable parent part.
    pub parent: Option<u128>,
    /// Semantic role.
    pub semantic: PlantPartSemantic,
    /// Material slot used by the part.
    pub material_slot: u32,
    /// Source references contributing geometry/recipe data.
    pub sources: Vec<u128>,
}

/// Physical family dimensions and conservative crown/root footprints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantDimensions {
    /// Nominal physical height in Q15.16 metres.
    pub height: DecisionScalar,
    /// Nominal trunk/stem radius.
    pub trunk_radius: DecisionScalar,
    /// Crown radius along X/Z.
    pub crown_radius: [DecisionScalar; 2],
    /// Root radius along X/Z.
    pub root_radius: [DecisionScalar; 2],
    /// Conservative local bounds in Q15.16 metres.
    pub local_bounds_min: [DecisionScalar; 3],
    /// Conservative local bounds maximum.
    pub local_bounds_max: [DecisionScalar; 3],
}

/// One structural skeleton/spine used by cook, deformation, damage, and proxy derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralSpine {
    /// Stable spine identity.
    pub id: u128,
    /// Owning semantic part.
    pub part: u128,
    /// Parent spine/joint.
    pub parent: Option<u128>,
    /// Ordered rest points in family-local Q15.16 metres.
    pub rest_points: Vec<[DecisionScalar; 3]>,
    /// Radius at each rest point.
    pub radii: Vec<DecisionScalar>,
}

/// Structural wind, bend, flutter, and damage response defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MechanicalResponse {
    /// Bending stiffness.
    pub stiffness: DecisionScalar,
    /// Damping.
    pub damping: UnitInterval,
    /// Aerodynamic drag.
    pub drag: DecisionScalar,
    /// High-frequency flutter response.
    pub flutter: DecisionScalar,
    /// Maximum bend angle as a signed normalized half-turn.
    pub bend_limit: UnitInterval,
    /// Damage threshold.
    pub damage_threshold: DecisionScalar,
    /// Break threshold.
    pub break_threshold: DecisionScalar,
}

/// Species-declared phenotype role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhenotypeRole {
    /// Healthy lifecycle appearance.
    Healthy,
    /// Harvested appearance.
    Harvested,
    /// Damaged appearance.
    Damaged,
    /// Burned/charred appearance.
    Burned,
    /// Dead appearance.
    Dead,
}

/// One plant phenotype/life-state variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPhenotype {
    /// Stable family-local phenotype identity.
    pub id: u32,
    /// Semantic role.
    pub role: PhenotypeRole,
    /// Family variation available to the phenotype.
    pub variation: u32,
    /// Material slot remap `(from, to)`.
    pub material_remap: Vec<(u32, u32)>,
    /// Parts active in this phenotype; empty means all.
    pub active_parts: Vec<u128>,
}

/// Authoritative collision-proxy primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantCollisionShape {
    /// Oriented box.
    Box,
    /// Sphere.
    Sphere,
    /// Capsule.
    Capsule,
    /// Convex hull derived from the named part set.
    ConvexHull,
}

/// One collision/breakage proxy declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCollisionProxy {
    /// Stable proxy identity.
    pub id: u128,
    /// Shape family.
    pub shape: PlantCollisionShape,
    /// Owning semantic part/spine.
    pub part: u128,
    /// Q15.16 local center.
    pub center: [DecisionScalar; 3],
    /// Q15.16 half extents/radius+half-height.
    pub dimensions: [DecisionScalar; 3],
    /// Breakable proxy.
    pub breakable: bool,
}

/// Navigation obstacle/cost proxy declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantNavigationProxy {
    /// Stable proxy identity.
    pub id: u128,
    /// Q15.16 local footprint polygon X/Z pairs.
    pub footprint: Vec<[DecisionScalar; 2]>,
    /// Q15.16 obstacle height.
    pub height: DecisionScalar,
    /// Unit traversal cost; one is neutral, max is blocked by policy.
    pub cost: UnitInterval,
}

/// Optional habitat preference defaults owned by the species, not biome-local density rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HabitatPreferences {
    /// Preferred ranges per shared field channel.
    pub fields: Vec<(FieldChannel, DecisionScalar, DecisionScalar)>,
    /// Preferred surface tags.
    pub surface_tags: Vec<u64>,
    /// Closed shade amount tolerated without reducing community suitability.
    pub shade_tolerance: UnitInterval,
}

/// One normalized `.splant` plant-family asset.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantFamilyAsset {
    /// Format version.
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    /// Human-readable family name.
    pub name: String,
    /// Exactly one recook source.
    pub source: PlantFamilySource,
    /// Normalized semantic parts.
    pub parts: Vec<PlantPart>,
    /// Physical dimensions/footprints.
    pub dimensions: PlantDimensions,
    /// Material asset slots.
    pub material_slots: Vec<Uuid>,
    /// Structural skeleton/spines.
    pub spines: Vec<StructuralSpine>,
    /// Mechanical response.
    pub mechanics: MechanicalResponse,
    /// Lifecycle/phenotype variants.
    pub phenotypes: Vec<PlantPhenotype>,
    /// Collision and breakage proxies.
    pub collision_proxies: Vec<PlantCollisionProxy>,
    /// Navigation proxies.
    pub navigation_proxies: Vec<PlantNavigationProxy>,
    /// Default interaction policy.
    pub interaction_policy: InteractionPolicy,
    /// Optional species habitat defaults.
    pub habitat: Option<HabitatPreferences>,
}

/// Root biome or reusable typed module role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BiomeRole {
    /// Root world/community graph.
    #[default]
    Root,
    /// Reusable typed subgraph module.
    Module,
}

/// Typed biome module parameter kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiomeParameterType {
    /// Q15.16 scalar.
    Scalar,
    /// Q15.16 vector.
    Vector,
    /// Closed normalized scalar.
    Unit,
    /// Plant-family asset reference.
    Plant,
    /// Shared field-channel reference.
    Field,
    /// Boolean gate.
    Boolean,
}

/// One declared module/root parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeParameter {
    /// Stable parameter identity.
    pub id: u128,
    /// Human-readable parameter name.
    pub name: String,
    /// Declared type.
    pub parameter_type: BiomeParameterType,
    /// Canonical typed default document.
    pub default_value: Value,
}

/// One family entry in a biome palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomePaletteEntry {
    /// Plant-family asset.
    pub plant: Uuid,
    /// Base community weight.
    pub weight: UnitInterval,
    /// Named seed namespace.
    pub seed_namespace: u128,
}

/// Field-to-suitability binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuitabilityBinding {
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Suitable inclusive minimum.
    pub minimum: DecisionScalar,
    /// Suitable inclusive maximum.
    pub maximum: DecisionScalar,
    /// Edge softness.
    pub falloff: DecisionScalar,
    /// Stable rule/node identity.
    pub node_guid: u128,
}

/// Pairwise competition rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompetitionRule {
    /// First family.
    pub first: Uuid,
    /// Second family.
    pub second: Uuid,
    /// Required spacing in Q15.16 metres.
    pub spacing: DecisionScalar,
    /// Relative priority when exclusion overlaps.
    pub priority: i32,
}

/// Companion/child relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionRule {
    /// Parent/host family.
    pub parent: Uuid,
    /// Companion/child family.
    pub child: Uuid,
    /// Minimum distance.
    pub minimum_distance: DecisionScalar,
    /// Maximum distance.
    pub maximum_distance: DecisionScalar,
    /// Spawn probability.
    pub probability: UnitInterval,
}

/// Ecological succession rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuccessionRule {
    /// Source family/lifecycle community.
    pub from: Uuid,
    /// Successor family.
    pub to: Uuid,
    /// Earliest ecology tick.
    pub minimum_tick: u64,
    /// Canonical chance at an eligible tick.
    pub probability: UnitInterval,
}

/// One ordinary `.sbiome` module reference.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeModuleReference {
    /// Referenced `.sbiome` asset.
    pub biome: Uuid,
    /// Stable call-site node GUID.
    pub call_guid: u128,
    /// Typed parameter bindings by parameter GUID.
    pub bindings: Vec<(u128, Value)>,
}

/// Graph-level evaluator policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomeGraphPolicy {
    /// Maximum permitted module recursion depth.
    pub maximum_recursion: u16,
    /// Maximum finite influence radius in Q15.16 metres.
    pub maximum_influence_radius: DecisionScalar,
    /// Reject unavailable authoritative fields instead of substituting defaults.
    pub require_authoritative_fields: bool,
}

/// One `.sbiome` root/module asset.
#[derive(Clone, Debug, PartialEq)]
pub struct BiomeAsset {
    /// Format version.
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Root or reusable module.
    pub role: BiomeRole,
    /// Typed public parameter interface.
    pub parameters: Vec<BiomeParameter>,
    /// Plant/community palette.
    pub palette: Vec<BiomePaletteEntry>,
    /// Base density in points per square metre, Q15.16.
    pub density: DecisionScalar,
    /// Clustering strength.
    pub clustering: UnitInterval,
    /// Suitability field bindings.
    pub suitability: Vec<SuitabilityBinding>,
    /// Pairwise spacing/competition.
    pub competition: Vec<CompetitionRule>,
    /// Companion and child relations.
    pub companions: Vec<CompanionRule>,
    /// Succession rules.
    pub succession: Vec<SuccessionRule>,
    /// Stable seed namespaces declared by this graph.
    pub seed_namespaces: Vec<(String, u128)>,
    /// Ordinary `.sbiome` module references.
    pub modules: Vec<BiomeModuleReference>,
    /// Bounded/cycle-checked graph policy.
    pub policy: BiomeGraphPolicy,
    /// Canonical typed graph document with stable node GUIDs.
    pub graph: Value,
}

/// One local biome instance and its typed parameter bindings.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalBiomeInstance {
    /// Stable layer/instance identity.
    pub id: u128,
    /// Referenced root biome.
    pub biome: Uuid,
    /// Exact affected bounds.
    pub bounds: WorldBounds,
    /// Typed parameter bindings.
    pub bindings: Vec<(u128, Value)>,
    /// Instance revision.
    pub revision: u64,
}

/// Stable sparse chunk address policy for a vegetation map package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationMapChunkLayout {
    /// World-cell level owning authored chunks.
    pub level: u8,
    /// Canonical chunk schema identity.
    pub schema_hash: [u8; 32],
}

/// One logical `.svegmap` catalog manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct VegetationMapAsset {
    /// Format version.
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Exact world coverage.
    pub bounds: WorldBounds,
    /// Sparse authored chunk policy. Chunk inventory is intentionally external.
    pub chunk_layout: VegetationMapChunkLayout,
    /// One ordered authored layer algebra.
    pub layers: Vec<VegetationLayer>,
    /// Local root-biome instances.
    pub biome_instances: Vec<LocalBiomeInstance>,
    /// Non-authoritative optional brush history.
    pub brush_history: Vec<BrushGestureMetadata>,
}

/// One quantized authored field tile inside a sparse map chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredFieldTile {
    /// Shared field channel.
    pub channel: FieldChannel,
    /// Stable layer owning the tile.
    pub layer: u128,
    /// Tile dimensions.
    pub dimensions: [u32; 3],
    /// Quantization step in Q15.16 bits.
    pub quantum_bits: i32,
    /// Canonical packed signed values.
    pub values: Vec<i32>,
}

/// One explicit authored point anchor stored in a map chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplicitPlantAnchor {
    /// Explicit-namespace identity.
    pub id: PlantId,
    /// Stable authored vegetation layer owning this anchor.
    pub layer: u128,
    /// Plant-family asset.
    pub family: Uuid,
    /// Complete point-row data, projected into canonical columns during cooking.
    pub point: PlantPoint,
}

/// One sparse authored `.svegmap` internal chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapChunk {
    /// Chunk format version.
    pub version: u32,
    /// Owning map.
    pub map: Uuid,
    /// Exact canonical owner cell.
    pub cell: WorldCellKey,
    /// Monotonic authored revision.
    pub revision: u64,
    /// Quantized scalar/vector/species fields.
    pub fields: Vec<AuthoredFieldTile>,
    /// Explicit authored plants/anchors.
    pub explicit_plants: Vec<ExplicitPlantAnchor>,
    /// Procedural pins.
    pub pins: Vec<PlantId>,
    /// Authored transform overrides.
    pub transform_overrides: Vec<PlantTransformOverride>,
    /// Authored state overrides.
    pub state_overrides: Vec<PlantStateOverride>,
    /// Signed blocker tile/category data.
    pub blockers: Vec<AuthoredFieldTile>,
    /// Chunk-local compact provenance.
    pub provenance: ProvenanceTable,
}

/// One immutable input dependency in a cooked base manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestDependency {
    /// Asset/source identity.
    pub id: Uuid,
    /// Exact canonical content hash.
    pub content_hash: [u8; 32],
}

/// Immutable identity binding authored sources, schemas, and cooked base cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationBaseManifest {
    /// Manifest format version.
    pub version: u32,
    /// Vegetation map.
    pub map: Uuid,
    /// Map manifest hash.
    pub map_hash: [u8; 32],
    /// Plant/biome/surface/source dependencies, sorted by identity.
    pub dependencies: Vec<ManifestDependency>,
    /// Canonical point-schema hash.
    pub point_schema_hash: [u8; 32],
    /// Evaluator semantic version.
    pub evaluator_version: u32,
    /// Cooker semantic version.
    pub cooker_version: u32,
}

impl VegetationBaseManifest {
    /// Canonical identity of the complete base manifest.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let mut bytes = b"saffron-anima/vegetation-base-manifest/v1\0".to_vec();
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.map.value().to_be_bytes());
        bytes.extend_from_slice(&self.map_hash);
        let mut dependencies = self.dependencies.clone();
        dependencies.sort_by_key(|dependency| dependency.id.value());
        for dependency in dependencies {
            bytes.extend_from_slice(&dependency.id.value().to_be_bytes());
            bytes.extend_from_slice(&dependency.content_hash);
        }
        bytes.extend_from_slice(&self.point_schema_hash);
        bytes.extend_from_slice(&self.evaluator_version.to_be_bytes());
        bytes.extend_from_slice(&self.cooker_version.to_be_bytes());
        sha256(&bytes)
    }

    /// Constructs a manifest with the current canonical point schema.
    #[must_use]
    pub fn current(map: Uuid, map_hash: [u8; 32]) -> Self {
        Self {
            version: VEGETATION_BASE_MANIFEST_VERSION,
            map,
            map_hash,
            dependencies: Vec::new(),
            point_schema_hash: point_schema_hash(),
            evaluator_version: 1,
            cooker_version: 1,
        }
    }
}

/// Pins/overrides invalidated by an identity-affecting seed/topology edit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentityConflictReport {
    /// Pins whose old procedural identities no longer exist.
    pub invalidated_pins: Vec<PlantId>,
    /// Transform/state overrides whose old targets no longer exist.
    pub invalidated_overrides: Vec<PlantId>,
}

/// Computes an explicit conflict report between the old/new accepted identity sets.
#[must_use]
pub fn identity_conflicts(
    old_ids: &[PlantId],
    new_ids: &[PlantId],
    pins: &[PlantId],
    overrides: &[PlantId],
) -> IdentityConflictReport {
    let old: std::collections::BTreeSet<_> = old_ids.iter().copied().collect();
    let new: std::collections::BTreeSet<_> = new_ids.iter().copied().collect();
    let disappeared: std::collections::BTreeSet<_> = old.difference(&new).copied().collect();
    let mut report = IdentityConflictReport {
        invalidated_pins: pins
            .iter()
            .copied()
            .filter(|id| disappeared.contains(id))
            .collect(),
        invalidated_overrides: overrides
            .iter()
            .copied()
            .filter(|id| disappeared.contains(id))
            .collect(),
    };
    report.invalidated_pins.sort();
    report.invalidated_pins.dedup();
    report.invalidated_overrides.sort();
    report.invalidated_overrides.dedup();
    report
}

/// Stable provider dependency used by map chunks/attachments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceDependency {
    /// Provider identity.
    pub provider: SurfaceProviderId,
    /// Canonical provider/tile content hash.
    pub content_hash: [u8; 32],
}

/// Validates one plant family without performing any I/O or cooking.
pub fn validate_plant_family(asset: &PlantFamilyAsset) -> Result<()> {
    if asset.version != PLANT_ASSET_VERSION {
        return Err(crate::Error::FormatVersion {
            format: ".splant",
            found: asset.version,
            expected: PLANT_ASSET_VERSION,
        });
    }
    if asset.id.value() == 0 || asset.name.is_empty() || asset.parts.is_empty() {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "id/name/parts".to_owned(),
        });
    }
    let mut part_ids = std::collections::BTreeSet::new();
    for part in &asset.parts {
        if !part_ids.insert(part.id) {
            return Err(crate::Error::InvalidFormat {
                format: ".splant",
                field: "parts.id".to_owned(),
            });
        }
    }
    if asset.parts.iter().any(|part| {
        part.parent
            .is_some_and(|parent| !part_ids.contains(&parent))
    }) {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "parts.parent".to_owned(),
        });
    }
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            let source_ids: std::collections::BTreeSet<_> =
                recipe.sources.iter().map(|source| source.id).collect();
            if recipe.sources.is_empty()
                || source_ids.len() != recipe.sources.len()
                || recipe.sources.iter().any(|source| {
                    source.id == 0 || source.uri.is_empty() || source.content_hash == [0; 32]
                })
            {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: "source.imported.sources".to_owned(),
                });
            }
        }
        PlantFamilySource::Native(graph) => {
            if graph.schema_hash == [0; 32] || !graph.graph.is_object() {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: "source.native.graph".to_owned(),
                });
            }
        }
    }
    if asset.material_slots.is_empty()
        || asset
            .parts
            .iter()
            .any(|part| part.material_slot as usize >= asset.material_slots.len())
        || asset.dimensions.height.bits() <= 0
        || asset.dimensions.trunk_radius.bits() < 0
        || asset
            .dimensions
            .crown_radius
            .iter()
            .chain(&asset.dimensions.root_radius)
            .any(|radius| radius.bits() <= 0)
        || (0..3).any(|axis| {
            asset.dimensions.local_bounds_min[axis] >= asset.dimensions.local_bounds_max[axis]
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "dimensions/materialSlots".to_owned(),
        });
    }
    let spine_ids: std::collections::BTreeSet<_> =
        asset.spines.iter().map(|spine| spine.id).collect();
    if spine_ids.len() != asset.spines.len()
        || asset.spines.iter().any(|spine| {
            !part_ids.contains(&spine.part)
                || spine.rest_points.len() < 2
                || spine.rest_points.len() != spine.radii.len()
                || spine
                    .parent
                    .is_some_and(|parent| !spine_ids.contains(&parent))
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "spines".to_owned(),
        });
    }
    let phenotype_ids: std::collections::BTreeSet<_> = asset
        .phenotypes
        .iter()
        .map(|phenotype| phenotype.id)
        .collect();
    if phenotype_ids.len() != asset.phenotypes.len()
        || asset
            .collision_proxies
            .iter()
            .any(|proxy| !part_ids.contains(&proxy.part))
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "phenotypes/collisionProxies".to_owned(),
        });
    }
    Ok(())
}

/// Validates one biome asset's interface and bounded module policy.
pub fn validate_biome(asset: &BiomeAsset) -> Result<()> {
    if asset.version != BIOME_ASSET_VERSION {
        return Err(crate::Error::FormatVersion {
            format: ".sbiome",
            found: asset.version,
            expected: BIOME_ASSET_VERSION,
        });
    }
    if asset.id.value() == 0
        || asset.name.is_empty()
        || asset.policy.maximum_recursion == 0
        || asset.policy.maximum_influence_radius.bits() < 0
    {
        return Err(crate::Error::InvalidFormat {
            format: ".sbiome",
            field: "identity/policy".to_owned(),
        });
    }
    let parameter_ids: std::collections::BTreeSet<_> = asset
        .parameters
        .iter()
        .map(|parameter| parameter.id)
        .collect();
    if parameter_ids.len() != asset.parameters.len()
        || parameter_ids.contains(&0)
        || asset
            .parameters
            .iter()
            .any(|parameter| parameter.name.is_empty())
    {
        return Err(crate::Error::InvalidFormat {
            format: ".sbiome",
            field: "parameters.id".to_owned(),
        });
    }
    let call_ids: std::collections::BTreeSet<_> = asset
        .modules
        .iter()
        .map(|module| module.call_guid)
        .collect();
    let seed_names: std::collections::BTreeSet<_> = asset
        .seed_namespaces
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let seed_ids: std::collections::BTreeSet<_> =
        asset.seed_namespaces.iter().map(|(_, id)| *id).collect();
    let palette_ids: std::collections::BTreeSet<_> = asset
        .palette
        .iter()
        .map(|entry| entry.plant.value())
        .collect();
    let competition_keys: std::collections::BTreeSet<_> = asset
        .competition
        .iter()
        .map(|rule| (rule.first.value(), rule.second.value()))
        .collect();
    let companion_keys: std::collections::BTreeSet<_> = asset
        .companions
        .iter()
        .map(|rule| (rule.parent.value(), rule.child.value()))
        .collect();
    let succession_keys: std::collections::BTreeSet<_> = asset
        .succession
        .iter()
        .map(|rule| (rule.from.value(), rule.to.value(), rule.minimum_tick))
        .collect();
    let suitability_ids: std::collections::BTreeSet<_> = asset
        .suitability
        .iter()
        .map(|binding| binding.node_guid)
        .collect();
    if asset.density.bits() < 0
        || call_ids.len() != asset.modules.len()
        || asset.modules.iter().any(|module| module.biome == asset.id)
        || seed_names.len() != asset.seed_namespaces.len()
        || seed_ids.len() != asset.seed_namespaces.len()
        || seed_names.contains("")
        || seed_ids.contains(&0)
        || palette_ids.len() != asset.palette.len()
        || asset.palette.iter().any(|entry| {
            entry.plant.value() == 0
                || entry.seed_namespace == 0
                || !seed_ids.contains(&entry.seed_namespace)
        })
        || competition_keys.len() != asset.competition.len()
        || asset.competition.iter().any(|rule| {
            rule.first.value() > rule.second.value()
                || !palette_ids.contains(&rule.first.value())
                || !palette_ids.contains(&rule.second.value())
                || rule.spacing.bits() < 0
        })
        || companion_keys.len() != asset.companions.len()
        || asset.companions.iter().any(|rule| {
            !palette_ids.contains(&rule.parent.value())
                || !palette_ids.contains(&rule.child.value())
                || rule.minimum_distance.bits() < 0
                || rule.maximum_distance < rule.minimum_distance
        })
        || succession_keys.len() != asset.succession.len()
        || asset.succession.iter().any(|rule| {
            !palette_ids.contains(&rule.from.value()) || !palette_ids.contains(&rule.to.value())
        })
        || suitability_ids.len() != asset.suitability.len()
        || suitability_ids.contains(&0)
        || asset
            .suitability
            .iter()
            .any(|binding| binding.minimum > binding.maximum || binding.falloff.bits() < 0)
    {
        return Err(crate::Error::InvalidFormat {
            format: ".sbiome",
            field: "graph/palette/seedNamespaces".to_owned(),
        });
    }
    Ok(())
}

/// Validates one map manifest without reading its sparse chunks.
pub fn validate_vegetation_map(asset: &VegetationMapAsset) -> Result<()> {
    if asset.version != VEGETATION_MAP_VERSION {
        return Err(crate::Error::FormatVersion {
            format: ".svegmap",
            found: asset.version,
            expected: VEGETATION_MAP_VERSION,
        });
    }
    if asset.id.value() == 0
        || asset.name.is_empty()
        || asset.chunk_layout.level > 62
        || asset.chunk_layout.schema_hash != crate::vegetation_map_chunk_schema_hash()
    {
        return Err(crate::Error::InvalidFormat {
            format: ".svegmap",
            field: "identity/chunkLayout".to_owned(),
        });
    }
    let mut layer_ids = std::collections::BTreeSet::new();
    for layer in &asset.layers {
        if layer.id == 0 || !layer_ids.insert(layer.id) {
            return Err(crate::Error::InvalidFormat {
                format: ".svegmap",
                field: "layers.id".to_owned(),
            });
        }
    }
    let instance_ids: std::collections::BTreeSet<_> = asset
        .biome_instances
        .iter()
        .map(|instance| instance.id)
        .collect();
    if instance_ids.len() != asset.biome_instances.len()
        || asset
            .biome_instances
            .iter()
            .any(|instance| instance.id == 0 || instance.biome.value() == 0)
    {
        return Err(crate::Error::InvalidFormat {
            format: ".svegmap",
            field: "biomeInstances".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlantId, ProceduralPlantIdentity};

    #[test]
    fn manifest_identity_sorts_dependencies() {
        let mut a = VegetationBaseManifest::current(Uuid(1), [2; 32]);
        a.dependencies = vec![
            ManifestDependency {
                id: Uuid(9),
                content_hash: [3; 32],
            },
            ManifestDependency {
                id: Uuid(4),
                content_hash: [5; 32],
            },
        ];
        let mut b = a.clone();
        b.dependencies.reverse();
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn topology_changes_report_affected_pins_and_overrides() {
        let id = |candidate| {
            PlantId::procedural(ProceduralPlantIdentity {
                map: Uuid(1),
                layer_guid: 2,
                node_address: 3,
                node_semantic_revision: 4,
                candidate,
                ancestor: 0,
                seed_namespace: 5,
                owner: WorldCellKey::base(0, 0, 0),
                family: Uuid(6),
            })
        };
        let report = identity_conflicts(&[id(1), id(2)], &[id(2), id(3)], &[id(1)], &[id(1)]);
        assert_eq!(report.invalidated_pins, vec![id(1)]);
        assert_eq!(report.invalidated_overrides, vec![id(1)]);
    }
}
