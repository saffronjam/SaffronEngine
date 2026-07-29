//! The three authored vegetation asset models.

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_spatial::{
    DecisionScalar, FieldChannel, SurfaceProviderId, UnitInterval, WorldBounds, WorldCellKey,
};

use crate::{
    BrushGestureMetadata, Error, InteractionPolicy, PlantId, PlantPoint, PlantStateOverride,
    PlantTransformOverride, ProvenanceTable, Result, VegetationLayer,
};

/// Current `.splant` document version.
pub const PLANT_ASSET_VERSION: u32 = 5;
/// Current `.sbiome` document version.
pub const BIOME_ASSET_VERSION: u32 = 1;
/// Current `.svegmap` manifest version.
pub const VEGETATION_MAP_VERSION: u32 = 2;
/// Current sparse authored map-chunk version.
pub const VEGETATION_MAP_CHUNK_VERSION: u32 = 3;

/// Asset/source licensing and provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceProvenance {
    /// Source tool/provider identifier.
    pub source: String,
    /// Source page/file URI retained for attribution and reimport.
    pub source_uri: String,
    /// SPDX-style license identifier.
    pub license_id: String,
    /// Canonical license document URI.
    pub license_uri: String,
    /// Human author/creator.
    pub author: String,
    /// Attribution text shipped with an exported product when required.
    pub attribution: String,
    /// Whether visible attribution is required.
    pub requires_attribution: bool,
}

/// Exactly one durable locator for a plant-family source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlantSourceLocator {
    /// Project catalog asset containing canonical imported bytes.
    Asset(Uuid),
    /// Canonical source URI or project-relative file URI.
    File(String),
}

/// Semantic contribution made by one imported-family source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantSourceRole {
    /// Renderable plant geometry and its material-slot topology.
    Geometry,
    /// Material definitions or atlases referenced by geometry.
    Material,
    /// Structural joints, spines, and vertex weights.
    Skeleton,
    /// Collision and breakage derivation geometry.
    Collision,
    /// Navigation footprint or cost derivation geometry.
    Navigation,
}

/// Format-erased stable selection within one source snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlantSourceSelector {
    /// The complete source contribution.
    #[default]
    Whole,
    /// One stable source element, with a human-readable path for diagnostics.
    Element {
        /// Stable source-local element identity.
        id: u128,
        /// Canonical source hierarchy path.
        path: String,
    },
    /// One material-homogeneous submesh of a stable source element.
    Submesh {
        /// Stable source-local element identity.
        element: u128,
        /// Zero-based submesh index.
        index: u32,
    },
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

/// Source coordinate-system handedness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceHandedness {
    /// Right-handed source basis.
    #[default]
    Right,
    /// Left-handed source basis.
    Left,
}

/// Source triangle winding before coordinate normalization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceWinding {
    /// Counter-clockwise front faces.
    #[default]
    CounterClockwise,
    /// Clockwise front faces.
    Clockwise,
}

/// Source UV vertical origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceUvOrigin {
    /// V=0 is the top edge.
    #[default]
    TopLeft,
    /// V=0 is the bottom edge and is flipped during normalization.
    BottomLeft,
}

/// Tangent-frame treatment during normalization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlantTangentPolicy {
    /// Require every source tangent frame to be finite and orthonormal.
    Require,
    /// Generate tangent frames only when the source frame is missing or invalid.
    #[default]
    GenerateMissing,
    /// Rebuild every tangent frame from normalized geometry and UVs.
    Regenerate,
}

/// Family-local origin selected after source coordinates become canonical metres.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PlantPivot {
    /// Preserve the normalized source origin.
    SourceOrigin,
    /// Center X/Z on the family bounds and place the lowest point at Y=0.
    #[default]
    BoundsBaseCenter,
    /// Subtract an explicit canonical-metre position.
    Explicit([DecisionScalar; 3]),
    /// Use the base center of geometry mapped to this semantic part.
    SemanticPart(u128),
}

/// One source asset referenced by an imported-family recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantSourceReference {
    /// Stable source identity inside the recipe.
    pub id: u128,
    /// Durable source location.
    pub locator: PlantSourceLocator,
    /// Semantic contribution supplied by this source.
    pub role: PlantSourceRole,
    /// Stable source subset selected for this contribution.
    pub selector: PlantSourceSelector,
    /// SHA-256 content identity accepted by the authored recipe.
    pub content_hash: [u8; 32],
    /// Source-specific coordinate and attribute normalization.
    pub settings: PlantImportSettings,
    /// Source-specific licensing and attribution.
    pub provenance: SourceProvenance,
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
    /// Source coordinate-system handedness.
    pub handedness: SourceHandedness,
    /// Uniform Q15.16 post-unit scale.
    pub scale: DecisionScalar,
    /// Family origin policy after coordinate normalization.
    pub pivot: PlantPivot,
    /// Source front-face winding.
    pub winding: SourceWinding,
    /// Source UV vertical origin.
    pub uv_origin: SourceUvOrigin,
    /// Q15.16 UV scale applied after vertical-origin normalization.
    pub uv_scale: [DecisionScalar; 2],
    /// Q15.16 UV offset applied after scaling.
    pub uv_offset: [DecisionScalar; 2],
    /// Tangent-frame normalization policy.
    pub tangent_policy: PlantTangentPolicy,
}

impl Default for PlantImportSettings {
    fn default() -> Self {
        Self {
            units: SourceUnits::Meters,
            up_axis: SourceAxis::PositiveY,
            forward_axis: SourceAxis::NegativeZ,
            handedness: SourceHandedness::Right,
            scale: DecisionScalar::from_bits(1 << 16),
            pivot: PlantPivot::BoundsBaseCenter,
            winding: SourceWinding::CounterClockwise,
            uv_origin: SourceUvOrigin::TopLeft,
            uv_scale: [DecisionScalar::from_bits(1 << 16); 2],
            uv_offset: [DecisionScalar::from_bits(0); 2],
            tangent_policy: PlantTangentPolicy::GenerateMissing,
        }
    }
}

/// Authored semantic destination of one stable source selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlantSemanticDestination {
    /// Normalized semantic part.
    Part(u128),
    /// Structural spine.
    Spine(u128),
    /// Family material slot.
    MaterialSlot(u32),
    /// Collision proxy declaration.
    CollisionProxy(u128),
    /// Navigation proxy declaration.
    NavigationProxy(u128),
    /// Phenotype declaration.
    Phenotype(u32),
}

/// One manual source-to-family semantic binding that reimport must preserve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantManualSemanticTarget {
    /// Stable binding identity.
    pub id: u128,
    /// Referenced imported source.
    pub source: u128,
    /// Stable source element or submesh.
    pub selector: PlantSourceSelector,
    /// Authored family destination.
    pub destination: PlantSemanticDestination,
}

/// One imported-family source recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedPlantFamilyRecipe {
    /// Source files/assets.
    pub sources: Vec<PlantSourceReference>,
    /// Manual semantic bindings preserved across reimport.
    pub semantic_targets: Vec<PlantManualSemanticTarget>,
}

/// Exactly one source for a plant family.
#[derive(Clone, Debug, PartialEq)]
pub enum PlantFamilySource {
    /// Imported-family recipe normalized during cooking.
    Imported(ImportedPlantFamilyRecipe),
    /// Embedded native botanical graph.
    Native {
        /// The authored graph, including its manual edit layer.
        graph: crate::BotanicalGraphDocument,
        /// External geometry the graph's grafts substitute in, in canonical source order.
        ///
        /// A graft names one of these by identity rather than carrying it, because a recook writes
        /// the resolved content hash back into the reference — and a hash living inside the graph
        /// document would change the graph's identity every time the hero mesh was re-read.
        grafts: Vec<PlantSourceReference>,
    },
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
    /// Flowering seasonal appearance.
    Flowering,
    /// Fruiting seasonal appearance.
    Fruiting,
    /// Senescent (autumn) seasonal appearance.
    Senescent,
    /// Wet appearance.
    Wet,
}

/// One plant phenotype/life-state variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPhenotype {
    /// Stable family-local phenotype identity.
    pub id: u32,
    /// Semantic role.
    pub role: PhenotypeRole,
    /// Seasonal window `(start, end)` in per-mille of the year in which a seasonal
    /// role shows, wrapping through 1000; `None` derives the role's default window.
    pub season_window: Option<(u16, u16)>,
    /// Family variation available to the phenotype.
    pub variation: u32,
    /// Material slot remap `(from, to)`.
    pub material_remap: Vec<(u32, u32)>,
    /// Parts active in this phenotype; empty means all.
    pub active_parts: Vec<u128>,
}

/// One coherent family variation available to lifecycle phenotypes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantVariation {
    /// Stable family-local variation identity.
    pub id: u32,
    /// Human-readable variation name.
    pub name: String,
    /// Imported source identities contributing to the variation.
    pub sources: Vec<u128>,
    /// Parts present in the variation; empty means all parts.
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

/// How one species relates to another growing nearby.
///
/// Relations are declared by the species, not by a biome: an oak shades out what it shades out
/// wherever it grows. The simulation reads them from the compiled family, so a rule never needs the
/// asset catalog at tick time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantRelationKind {
    /// Grows better near the other family — nurse plants, nitrogen fixers, shared mycorrhizae.
    Companion,
    /// Suppressed by the other family — allelopathy, root crowding, canopy exclusion.
    Antagonist,
    /// Establishes under the other family's canopy and takes its place as that canopy fails.
    Successor,
    /// Lives permanently beneath the other family and needs its shade.
    Understory,
}

impl TryFrom<u32> for PlantRelationKind {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Companion),
            1 => Ok(Self::Antagonist),
            2 => Ok(Self::Successor),
            3 => Ok(Self::Understory),
            _ => Err(Error::ArtifactFormat {
                format: "splant",
                field: "ecology.relations.kind".to_owned(),
            }),
        }
    }
}

/// One declared relation to another plant family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantSpeciesRelation {
    /// The other family.
    pub family: Uuid,
    /// What the relation does.
    pub kind: PlantRelationKind,
    /// How strongly it applies.
    pub strength: UnitInterval,
}

/// What a species does over biological time, and how it responds to its neighbours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlantEcologyDeclaration {
    /// The fixed-tick rules the ecology simulation runs for this family.
    pub rules: crate::EcologySpeciesRules,
    /// Relations to other families, in canonical family order.
    pub relations: Vec<PlantSpeciesRelation>,
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
    /// Canonically sorted unique family classification identities.
    pub tags: Vec<crate::PlantTagId>,
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
    /// Coherent family variations.
    pub variations: Vec<PlantVariation>,
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
    /// Species ecology rules and relations.
    pub ecology: PlantEcologyDeclaration,
    /// Whether the family places in a world or exists to be called by one.
    pub role: PlantFamilyRole,
    /// The `.splant` modules this family's graph calls, by call-site GUID.
    pub modules: Vec<PlantModuleReference>,
    /// How deep the module chain below this family may reach.
    pub module_recursion_limit: u16,
}

/// Whether a family places in a world or exists as a reusable preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlantFamilyRole {
    /// An ordinary family a biome may place.
    #[default]
    Family,
    /// A reusable preset another family's graph calls. It is still an ordinary `.splant` — it
    /// opens, previews, and cooks like any family, which is what keeps a preset editable rather
    /// than a second document format.
    Module,
}

/// One call site's binding of a `.splant` module.
///
/// The interface is deliberately small: which module, which of its variations, and what to scale
/// it by. Each has a consumer in the evaluator, which is the test of whether a parameter is real
/// rather than a knob that reads nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantModuleReference {
    /// The referenced `.splant`, which must carry [`PlantFamilyRole::Module`].
    pub plant: Uuid,
    /// Stable call-site GUID, named by the graph's `ModuleCall` node.
    pub call_guid: u128,
    /// Which of the module's variations to grow.
    pub variation: u32,
    /// Uniform scale applied when placing it, where one is the module's authored size.
    pub scale: DecisionScalar,
}

/// The deepest module chain any family may declare.
///
/// A bound rather than a budget: a preset that calls a preset is ordinary authoring, and a chain
/// that keeps going is a cycle the author cannot see. The resolver rejects at the declared limit,
/// and this caps what may be declared.
pub const MAX_PLANT_MODULE_RECURSION: u16 = 8;

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
    /// Maximum descendant module-call edges relative to this asset's entry depth.
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Address class for one immutable authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VegetationMapChunkKind {
    /// Quantized field and blocker samples.
    Field,
    /// Explicit anchors, pins, state, transforms, and their provenance.
    AnchorOverride,
    /// One local biome-graph instance.
    GraphInstance,
    /// One ordered layer definition.
    LayerMetadata,
    /// Optional non-authoritative editor gesture metadata.
    EditorMetadata,
}

/// Spatial address of one immutable authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VegetationMapTileKey {
    /// Map-global metadata.
    Global,
    /// One sparse authored world tile.
    Cell(WorldCellKey),
}

/// Stable logical address resolved through the `.svegmap` root inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VegetationMapChunkKey {
    /// Stable layer or instance identity.
    pub layer: u128,
    /// Global or spatial tile address.
    pub tile: VegetationMapTileKey,
    /// Typed payload vocabulary.
    pub kind: VegetationMapChunkKind,
}

/// Root reference to one immutable content-addressed authored map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationMapChunkReference {
    /// Logical address replaced by a newer transaction.
    pub key: VegetationMapChunkKey,
    /// SHA-256 of the exact canonical object bytes.
    pub content_hash: [u8; 32],
    /// Exact canonical byte length.
    pub byte_length: u64,
    /// Monotonic authored object revision.
    pub revision: u64,
}

impl VegetationMapChunkReference {
    /// Canonical inventory order.
    #[must_use]
    pub fn order_key(&self) -> VegetationMapChunkKey {
        self.key
    }
}

/// One logical `.svegmap` catalog root.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Monotonic committed root generation.
    pub generation: u64,
    /// Canonically ordered logical-address to immutable-object references.
    pub inventory: Vec<VegetationMapChunkReference>,
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

/// Quantized authored truth for one layer and spatial tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapFieldChunk {
    /// Quantized scalar/vector/species fields.
    pub fields: Vec<AuthoredFieldTile>,
    /// Signed blocker tile/category data.
    pub blockers: Vec<AuthoredFieldTile>,
}

/// Anchors, pins, authored overrides, and provenance for one layer and spatial tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapAnchorChunk {
    /// Explicit authored plants/anchors.
    pub explicit_plants: Vec<ExplicitPlantAnchor>,
    /// Procedural pins.
    pub pins: Vec<PlantId>,
    /// Authored transform overrides.
    pub transform_overrides: Vec<PlantTransformOverride>,
    /// Authored state overrides.
    pub state_overrides: Vec<PlantStateOverride>,
    /// Chunk-local compact provenance.
    pub provenance: ProvenanceTable,
}

/// One typed immutable authored map-object payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationMapChunkPayload {
    /// Quantized field and blocker samples.
    Field(VegetationMapFieldChunk),
    /// Explicit anchors, pins, overrides, and provenance.
    AnchorOverride(VegetationMapAnchorChunk),
    /// One local root-biome graph instance.
    GraphInstance(LocalBiomeInstance),
    /// One ordered layer definition.
    LayerMetadata(VegetationLayer),
    /// Optional non-authoritative editor gesture metadata.
    EditorMetadata(Vec<BrushGestureMetadata>),
}

impl VegetationMapChunkPayload {
    /// Address kind required by this payload.
    #[must_use]
    pub fn kind(&self) -> VegetationMapChunkKind {
        match self {
            Self::Field(_) => VegetationMapChunkKind::Field,
            Self::AnchorOverride(_) => VegetationMapChunkKind::AnchorOverride,
            Self::GraphInstance(_) => VegetationMapChunkKind::GraphInstance,
            Self::LayerMetadata(_) => VegetationMapChunkKind::LayerMetadata,
            Self::EditorMetadata(_) => VegetationMapChunkKind::EditorMetadata,
        }
    }
}

/// One immutable content-addressed authored `.svegmap` object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapChunk {
    /// Chunk format version.
    pub version: u32,
    /// Owning map.
    pub map: Uuid,
    /// Exact logical object address.
    pub key: VegetationMapChunkKey,
    /// Monotonic authored revision.
    pub revision: u64,
    /// Typed object payload.
    pub payload: VegetationMapChunkPayload,
}

impl VegetationMapChunk {
    /// Builds the exact root reference for these canonical object bytes.
    pub fn reference(&self) -> Result<VegetationMapChunkReference> {
        let bytes = crate::write_vegetation_map_chunk(self)?;
        Ok(VegetationMapChunkReference {
            key: self.key,
            content_hash: crate::vegetation_content_hash(&bytes),
            byte_length: u64::try_from(bytes.len()).map_err(|_| crate::Error::NumericOverflow)?,
            revision: self.revision,
        })
    }
}

/// Fully resolved logical map used by authoring and evaluation callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapSnapshot {
    /// Atomically visible root generation.
    pub root: VegetationMapAsset,
    /// Ordered layer algebra reconstructed from layer-metadata objects.
    pub layers: Vec<VegetationLayer>,
    /// Local biome instances reconstructed from graph-instance objects.
    pub biome_instances: Vec<LocalBiomeInstance>,
    /// Optional editor-only gesture records.
    pub brush_history: Vec<BrushGestureMetadata>,
    /// Canonically addressed immutable object set.
    pub chunks: Vec<VegetationMapChunk>,
}

impl std::ops::Deref for VegetationMapSnapshot {
    type Target = VegetationMapAsset;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

/// Resolved spatial authored truth assembled from typed objects at one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationMapTileSnapshot {
    /// Owning map.
    pub map: Uuid,
    /// Exact canonical tile.
    pub cell: WorldCellKey,
    /// Quantized scalar/vector/species fields.
    pub fields: Vec<AuthoredFieldTile>,
    /// Signed blocker tile/category data.
    pub blockers: Vec<AuthoredFieldTile>,
    /// Explicit authored plants/anchors.
    pub explicit_plants: Vec<ExplicitPlantAnchor>,
    /// Procedural pins.
    pub pins: Vec<PlantId>,
    /// Authored transform overrides.
    pub transform_overrides: Vec<PlantTransformOverride>,
    /// Authored state overrides.
    pub state_overrides: Vec<PlantStateOverride>,
    /// Tile-local compact provenance.
    pub provenance: ProvenanceTable,
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
    if asset.id.value() == 0 || asset.name.trim().is_empty() || asset.parts.is_empty() {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "id/name/parts".to_owned(),
        });
    }
    if asset.tags.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "tags".to_owned(),
        });
    }
    let mut part_ids = std::collections::BTreeSet::new();
    for part in &asset.parts {
        if part.id == 0 || !part_ids.insert(part.id) {
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
    validate_parent_forest(
        asset.parts.iter().map(|part| (part.id, part.parent)),
        "parts.parent",
    )?;
    match &asset.source {
        PlantFamilySource::Imported(recipe) => {
            let source_by_id = recipe
                .sources
                .iter()
                .map(|source| (source.id, source))
                .collect::<std::collections::BTreeMap<_, _>>();
            let source_ids = source_by_id
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let duplicate_contribution =
                recipe.sources.iter().enumerate().any(|(index, source)| {
                    recipe.sources[..index].iter().any(|previous| {
                        previous.locator == source.locator
                            && previous.role == source.role
                            && previous.selector == source.selector
                    })
                });
            if recipe.sources.is_empty()
                || source_ids.len() != recipe.sources.len()
                || duplicate_contribution
                || recipe.sources.iter().any(|source| {
                    source.id == 0
                        || source.content_hash == [0; 32]
                        || source.settings.scale.bits() <= 0
                        || source
                            .settings
                            .uv_scale
                            .iter()
                            .any(|value| value.bits() == 0)
                        || !source_axes_are_orthogonal(
                            source.settings.up_axis,
                            source.settings.forward_axis,
                        )
                        || !valid_source_locator(&source.locator)
                        || !valid_source_selector(&source.selector)
                        || !valid_source_provenance(&source.provenance)
                })
            {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: "source.imported.sources".to_owned(),
                });
            }
            let target_ids = recipe
                .semantic_targets
                .iter()
                .map(|target| target.id)
                .collect::<std::collections::BTreeSet<_>>();
            let duplicate_binding =
                recipe
                    .semantic_targets
                    .iter()
                    .enumerate()
                    .any(|(index, target)| {
                        recipe.semantic_targets[..index].iter().any(|previous| {
                            previous.source == target.source
                                && previous.selector == target.selector
                                && previous.destination == target.destination
                        })
                    });
            if target_ids.len() != recipe.semantic_targets.len()
                || target_ids.contains(&0)
                || duplicate_binding
                || recipe.semantic_targets.iter().any(|target| {
                    !valid_source_selector(&target.selector)
                        || !source_by_id.get(&target.source).is_some_and(|source| {
                            source_selector_contains(&source.selector, &target.selector)
                                && source_role_supports_destination(source.role, target.destination)
                        })
                        || !semantic_destination_exists(target.destination, &part_ids, asset)
                })
                || recipe.sources.iter().any(|source| {
                    !recipe
                        .semantic_targets
                        .iter()
                        .any(|target| target.source == source.id)
                        || matches!(source.settings.pivot, PlantPivot::SemanticPart(part) if
                        !part_ids.contains(&part)
                            || !recipe.semantic_targets.iter().any(|target| {
                                target.source == source.id
                                    && target.destination
                                        == PlantSemanticDestination::Part(part)
                            }))
                })
                || asset.parts.iter().any(|part| {
                    part.sources.is_empty()
                        || part
                            .sources
                            .iter()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            != part.sources.len()
                        || part.sources.iter().any(|source| {
                            !source_by_id
                                .get(source)
                                .is_some_and(|source| source.role == PlantSourceRole::Geometry)
                        })
                        || !recipe.semantic_targets.iter().any(|target| {
                            part.sources.contains(&target.source)
                                && target.destination == PlantSemanticDestination::Part(part.id)
                        })
                })
            {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: "source.imported.semanticTargets".to_owned(),
                });
            }
        }
        PlantFamilySource::Native { graph, grafts } => {
            // A native family's geometry is grown, so no part may reference an external source.
            if asset.parts.iter().any(|part| !part.sources.is_empty()) {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: "source.native.parts".to_owned(),
                });
            }
            graph.validate().map_err(|_| crate::Error::InvalidFormat {
                format: ".splant",
                field: "source.native.graph".to_owned(),
            })?;
            let field = |name: &str| crate::Error::InvalidFormat {
                format: ".splant",
                field: name.to_owned(),
            };
            let mut declared = std::collections::BTreeSet::new();
            for pair in grafts.windows(2) {
                if pair[0].id >= pair[1].id {
                    return Err(field("source.native.grafts.order"));
                }
            }
            for graft in grafts {
                if graft.id == 0 || !declared.insert(graft.id) {
                    return Err(field("source.native.grafts.id"));
                }
                if graft.role != PlantSourceRole::Geometry {
                    return Err(field("source.native.grafts.role"));
                }
                // The two top bits are reserved for the identities a native family derives: its own
                // graph source and one per variation.
                if graft.id >> 126 != 0 {
                    return Err(field("source.native.grafts.reserved"));
                }
            }
            // A graft naming a source the family does not declare has no geometry to substitute,
            // and there is nothing sensible to fall back to.
            for edit in &graph.edits {
                if let crate::BotanicalEditAction::Graft { source, .. } = &edit.action
                    && !declared.contains(source)
                {
                    return Err(field("source.native.grafts.reference"));
                }
            }
        }
    }
    if asset.material_slots.is_empty()
        || asset
            .parts
            .iter()
            .any(|part| part.material_slot as usize >= asset.material_slots.len())
        || asset.material_slots.contains(&Uuid(0))
        || asset
            .material_slots
            .iter()
            .map(|material| material.value())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != asset.material_slots.len()
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
        || asset.dimensions.local_bounds_min[1].bits() > 0
        || asset.dimensions.local_bounds_max[1] < asset.dimensions.height
        || asset.dimensions.crown_radius[0]
            > asset.dimensions.local_bounds_max[0]
                .checked_sub(asset.dimensions.local_bounds_min[0])?
        || asset.dimensions.crown_radius[1]
            > asset.dimensions.local_bounds_max[2]
                .checked_sub(asset.dimensions.local_bounds_min[2])?
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "dimensions/materialSlots".to_owned(),
        });
    }
    let spine_ids: std::collections::BTreeSet<_> =
        asset.spines.iter().map(|spine| spine.id).collect();
    if spine_ids.len() != asset.spines.len()
        || spine_ids.contains(&0)
        || asset.spines.iter().any(|spine| {
            !part_ids.contains(&spine.part)
                || spine.rest_points.len() < 2
                || spine.rest_points.len() != spine.radii.len()
                || spine.radii.iter().any(|radius| radius.bits() <= 0)
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
    validate_parent_forest(
        asset.spines.iter().map(|spine| (spine.id, spine.parent)),
        "spines.parent",
    )?;
    if asset.mechanics.stiffness.bits() < 0
        || asset.mechanics.drag.bits() < 0
        || asset.mechanics.flutter.bits() < 0
        || asset.mechanics.damage_threshold.bits() < 0
        || asset.mechanics.break_threshold < asset.mechanics.damage_threshold
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "mechanics".to_owned(),
        });
    }
    let variation_ids = asset
        .variations
        .iter()
        .map(|variation| variation.id)
        .collect::<std::collections::BTreeSet<_>>();
    let imported_source_ids = match &asset.source {
        PlantFamilySource::Imported(recipe) => Some(
            recipe
                .sources
                .iter()
                .map(|source| source.id)
                .collect::<std::collections::BTreeSet<_>>(),
        ),
        PlantFamilySource::Native { .. } => None,
    };
    // A native family's variation sources are the per-individual geometry the graph grows, one
    // source per declared variation, so they are derived rather than authored.
    let native_source_ids = match &asset.source {
        PlantFamilySource::Native { graph, .. } => Some(
            (0..graph.variations.len())
                .map(crate::native_variation_source_id)
                .collect::<std::collections::BTreeSet<_>>(),
        ),
        PlantFamilySource::Imported(_) => None,
    };
    if asset.variations.is_empty()
        || variation_ids.len() != asset.variations.len()
        || asset.variations.iter().any(|variation| {
            let variation_sources = variation
                .sources
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let active_parts = variation
                .active_parts
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            variation.name.trim().is_empty()
                || variation_sources.len() != variation.sources.len()
                || active_parts.len() != variation.active_parts.len()
                || variation
                    .active_parts
                    .iter()
                    .any(|part| !part_ids.contains(part))
                || imported_source_ids.as_ref().is_some_and(|sources| {
                    variation.sources.is_empty()
                        || variation
                            .sources
                            .iter()
                            .any(|source| !sources.contains(source))
                        || asset.parts.iter().any(|part| {
                            (variation.active_parts.is_empty() || active_parts.contains(&part.id))
                                && !part
                                    .sources
                                    .iter()
                                    .any(|source| variation_sources.contains(source))
                        })
                })
                || native_source_ids.as_ref().is_some_and(|sources| {
                    variation.sources.len() != 1
                        || variation
                            .sources
                            .iter()
                            .any(|source| !sources.contains(source))
                })
        })
        || imported_source_ids.as_ref().is_some_and(|sources| {
            sources.iter().any(|source| {
                !asset
                    .variations
                    .iter()
                    .any(|variation| variation.sources.contains(source))
            })
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "variations".to_owned(),
        });
    }
    let phenotype_ids: std::collections::BTreeSet<_> = asset
        .phenotypes
        .iter()
        .map(|phenotype| phenotype.id)
        .collect();
    let duplicate_phenotype_role = asset
        .phenotypes
        .iter()
        .enumerate()
        .any(|(index, phenotype)| {
            asset.phenotypes[..index].iter().any(|previous| {
                previous.variation == phenotype.variation && previous.role == phenotype.role
            })
        });
    if asset.phenotypes.is_empty()
        || phenotype_ids.len() != asset.phenotypes.len()
        || duplicate_phenotype_role
        || !asset
            .phenotypes
            .iter()
            .any(|phenotype| phenotype.role == PhenotypeRole::Healthy)
        || asset.variations.iter().any(|variation| {
            !asset
                .phenotypes
                .iter()
                .any(|phenotype| phenotype.variation == variation.id)
        })
        || asset.phenotypes.iter().any(|phenotype| {
            let material_sources = phenotype
                .material_remap
                .iter()
                .map(|(from, _)| *from)
                .collect::<std::collections::BTreeSet<_>>();
            let active_parts = phenotype
                .active_parts
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let variation = asset
                .variations
                .iter()
                .find(|variation| variation.id == phenotype.variation);
            !variation_ids.contains(&phenotype.variation)
                || material_sources.len() != phenotype.material_remap.len()
                || active_parts.len() != phenotype.active_parts.len()
                || phenotype.material_remap.iter().any(|(from, to)| {
                    *from as usize >= asset.material_slots.len()
                        || *to as usize >= asset.material_slots.len()
                        || from == to
                })
                || phenotype
                    .active_parts
                    .iter()
                    .any(|part| !part_ids.contains(part))
                || variation.is_some_and(|variation| {
                    !variation.active_parts.is_empty()
                        && phenotype
                            .active_parts
                            .iter()
                            .any(|part| !variation.active_parts.contains(part))
                })
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "phenotypes".to_owned(),
        });
    }
    let collision_ids = asset
        .collision_proxies
        .iter()
        .map(|proxy| proxy.id)
        .collect::<std::collections::BTreeSet<_>>();
    if collision_ids.len() != asset.collision_proxies.len()
        || collision_ids.contains(&0)
        || asset.collision_proxies.iter().any(|proxy| {
            !part_ids.contains(&proxy.part)
                || proxy
                    .dimensions
                    .iter()
                    .any(|dimension| dimension.bits() <= 0)
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "collisionProxies".to_owned(),
        });
    }
    let navigation_ids = asset
        .navigation_proxies
        .iter()
        .map(|proxy| proxy.id)
        .collect::<std::collections::BTreeSet<_>>();
    if navigation_ids.len() != asset.navigation_proxies.len()
        || navigation_ids.contains(&0)
        || asset.navigation_proxies.iter().any(|proxy| {
            proxy.footprint.len() < 3
                || proxy.height.bits() <= 0
                || polygon_area_twice(&proxy.footprint) == 0
        })
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "navigationProxies".to_owned(),
        });
    }
    if let Some(habitat) = &asset.habitat
        && habitat
            .fields
            .iter()
            .any(|(_, minimum, maximum)| minimum > maximum)
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "habitat.fields".to_owned(),
        });
    }
    // Stages only ever advance, and a species cannot relate to itself or name a family twice: both
    // would make the tick's answer depend on which duplicate it read.
    let stages = asset.ecology.rules.stage_ticks;
    if stages.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "ecology.rules.stageTicks".to_owned(),
        });
    }
    let related = asset
        .ecology
        .relations
        .iter()
        .map(|relation| relation.family.value())
        .collect::<std::collections::BTreeSet<_>>();
    if related.len() != asset.ecology.relations.len()
        || related.contains(&asset.id.value())
        || asset
            .ecology
            .relations
            .windows(2)
            .any(|pair| pair[0].family.value() >= pair[1].family.value())
    {
        return Err(crate::Error::InvalidFormat {
            format: ".splant",
            field: "ecology.relations".to_owned(),
        });
    }
    validate_plant_modules(asset)?;
    Ok(())
}

/// The module table's own invariants: a bounded declared depth, unique non-zero call GUIDs, no
/// self-reference, and exactly one reference per `ModuleCall` node in the graph.
///
/// The last one matters in both directions. A call with no reference would resolve to nothing, and
/// a reference with no call is a binding an author edits expecting an effect it cannot have.
fn validate_plant_modules(asset: &PlantFamilyAsset) -> Result<()> {
    let field = |name: &str| crate::Error::InvalidFormat {
        format: ".splant",
        field: name.to_owned(),
    };
    if asset.module_recursion_limit > MAX_PLANT_MODULE_RECURSION {
        return Err(field("moduleRecursionLimit"));
    }
    let call_guids: std::collections::BTreeSet<_> = asset
        .modules
        .iter()
        .map(|module| module.call_guid)
        .collect();
    if call_guids.len() != asset.modules.len() || call_guids.contains(&0) {
        return Err(field("modules.callGuid"));
    }
    if asset.modules.iter().any(|module| {
        module.plant.value() == 0 || module.plant == asset.id || module.scale.bits() <= 0
    }) {
        return Err(field("modules"));
    }
    let PlantFamilySource::Native { graph, .. } = &asset.source else {
        // An imported family has no graph to call from, so a module table on one is a binding
        // nothing can read.
        return if asset.modules.is_empty() {
            Ok(())
        } else {
            Err(field("modules.source"))
        };
    };
    let mut called = std::collections::BTreeSet::new();
    for node in &graph.nodes {
        if let crate::BotanicalOperator::ModuleCall { call_guid } = &node.operator
            && (!called.insert(*call_guid) || !call_guids.contains(call_guid))
        {
            return Err(field("modules.callGuid"));
        }
    }
    if called.len() != asset.modules.len() {
        return Err(field("modules"));
    }
    Ok(())
}

fn source_axis_vector(axis: SourceAxis) -> [i8; 3] {
    match axis {
        SourceAxis::PositiveX => [1, 0, 0],
        SourceAxis::NegativeX => [-1, 0, 0],
        SourceAxis::PositiveY => [0, 1, 0],
        SourceAxis::NegativeY => [0, -1, 0],
        SourceAxis::PositiveZ => [0, 0, 1],
        SourceAxis::NegativeZ => [0, 0, -1],
    }
}

fn source_axes_are_orthogonal(up: SourceAxis, forward: SourceAxis) -> bool {
    let up = source_axis_vector(up);
    let forward = source_axis_vector(forward);
    up.into_iter()
        .zip(forward)
        .map(|(first, second)| i16::from(first) * i16::from(second))
        .sum::<i16>()
        == 0
}

fn valid_source_locator(locator: &PlantSourceLocator) -> bool {
    match locator {
        PlantSourceLocator::Asset(id) => id.value() != 0,
        PlantSourceLocator::File(uri) => !uri.trim().is_empty(),
    }
}

fn valid_source_provenance(provenance: &SourceProvenance) -> bool {
    !provenance.source.trim().is_empty()
        && !provenance.source_uri.trim().is_empty()
        && !provenance.license_id.trim().is_empty()
        && !provenance.license_uri.trim().is_empty()
        && (!provenance.requires_attribution
            || (!provenance.author.trim().is_empty() && !provenance.attribution.trim().is_empty()))
}

fn valid_source_selector(selector: &PlantSourceSelector) -> bool {
    match selector {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { id, path } => *id != 0 && !path.trim().is_empty(),
        PlantSourceSelector::Submesh { element, .. } => *element != 0,
    }
}

fn source_selector_contains(source: &PlantSourceSelector, target: &PlantSourceSelector) -> bool {
    match source {
        PlantSourceSelector::Whole => true,
        PlantSourceSelector::Element { id, .. } => match target {
            PlantSourceSelector::Whole => false,
            PlantSourceSelector::Element { id: target, .. }
            | PlantSourceSelector::Submesh {
                element: target, ..
            } => id == target,
        },
        PlantSourceSelector::Submesh { element, index } => {
            matches!(target, PlantSourceSelector::Submesh {
                element: target_element,
                index: target_index,
            } if element == target_element && index == target_index)
        }
    }
}

fn source_role_supports_destination(
    role: PlantSourceRole,
    destination: PlantSemanticDestination,
) -> bool {
    matches!(
        (role, destination),
        (
            PlantSourceRole::Geometry,
            PlantSemanticDestination::Part(_) | PlantSemanticDestination::Phenotype(_)
        ) | (
            PlantSourceRole::Material,
            PlantSemanticDestination::MaterialSlot(_) | PlantSemanticDestination::Phenotype(_)
        ) | (
            PlantSourceRole::Skeleton,
            PlantSemanticDestination::Spine(_)
        ) | (
            PlantSourceRole::Collision,
            PlantSemanticDestination::CollisionProxy(_)
        ) | (
            PlantSourceRole::Navigation,
            PlantSemanticDestination::NavigationProxy(_)
        )
    )
}

fn semantic_destination_exists(
    destination: PlantSemanticDestination,
    part_ids: &std::collections::BTreeSet<u128>,
    asset: &PlantFamilyAsset,
) -> bool {
    match destination {
        PlantSemanticDestination::Part(id) => part_ids.contains(&id),
        PlantSemanticDestination::Spine(id) => asset.spines.iter().any(|spine| spine.id == id),
        PlantSemanticDestination::MaterialSlot(slot) => {
            (slot as usize) < asset.material_slots.len()
        }
        PlantSemanticDestination::CollisionProxy(id) => {
            asset.collision_proxies.iter().any(|proxy| proxy.id == id)
        }
        PlantSemanticDestination::NavigationProxy(id) => {
            asset.navigation_proxies.iter().any(|proxy| proxy.id == id)
        }
        PlantSemanticDestination::Phenotype(id) => {
            asset.phenotypes.iter().any(|phenotype| phenotype.id == id)
        }
    }
}

fn validate_parent_forest(
    entries: impl IntoIterator<Item = (u128, Option<u128>)>,
    field: &str,
) -> Result<()> {
    let parents = entries
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    for start in parents.keys().copied() {
        let mut active = std::collections::BTreeSet::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if !active.insert(id) {
                return Err(crate::Error::InvalidFormat {
                    format: ".splant",
                    field: field.to_owned(),
                });
            }
            current = parents.get(&id).copied().flatten();
        }
    }
    Ok(())
}

fn polygon_area_twice(points: &[[DecisionScalar; 2]]) -> i128 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(first, second)| {
            i128::from(first[0].bits()) * i128::from(second[1].bits())
                - i128::from(second[0].bits()) * i128::from(first[1].bits())
        })
        .sum()
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

/// Validates one map root without reading its immutable authored objects.
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
    let mut previous = None;
    for reference in &asset.inventory {
        if reference.key.layer == 0
            || reference.byte_length == 0
            || previous.is_some_and(|key| key >= reference.key)
        {
            return Err(crate::Error::InvalidFormat {
                format: ".svegmap",
                field: "inventory".to_owned(),
            });
        }
        match (reference.key.kind, reference.key.tile) {
            (
                VegetationMapChunkKind::Field | VegetationMapChunkKind::AnchorOverride,
                VegetationMapTileKey::Cell(cell),
            ) if cell.level() == asset.chunk_layout.level => {}
            (
                VegetationMapChunkKind::GraphInstance
                | VegetationMapChunkKind::LayerMetadata
                | VegetationMapChunkKind::EditorMetadata,
                VegetationMapTileKey::Global,
            ) => {}
            _ => {
                return Err(crate::Error::InvalidFormat {
                    format: ".svegmap",
                    field: "inventory.key".to_owned(),
                });
            }
        }
        previous = Some(reference.key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProceduralPlantIdentity;
    use crate::identity::derive_procedural_plant_id;

    #[test]
    fn topology_changes_report_affected_pins_and_overrides() {
        let id = |candidate| {
            derive_procedural_plant_id(ProceduralPlantIdentity {
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
