use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval};

use crate::{Error, InteractionPolicy, Result};

/// Current `.splant` document version.
pub const PLANT_ASSET_VERSION: u32 = 5;

/// The deepest module chain any family may declare.
pub const MAX_PLANT_MODULE_RECURSION: u16 = 8;

/// Asset/source licensing and provenance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceProvenance {
    /// Source tool/provider identifier.
    pub source: String,
    /// Source page/file URI, kept for attribution and reimport.
    pub source_uri: String,
    /// SPDX-style license identifier.
    pub license_id: String,
    pub license_uri: String,
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
    #[default]
    Meters,
    Centimeters,
    Millimeters,
    Feet,
}

/// Source up/forward axes normalized during import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceAxis {
    PositiveX,
    NegativeX,
    #[default]
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

/// Source coordinate-system handedness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceHandedness {
    #[default]
    Right,
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
    pub locator: PlantSourceLocator,
    pub role: PlantSourceRole,
    pub selector: PlantSourceSelector,
    /// SHA-256 content identity accepted by the authored recipe.
    pub content_hash: [u8; 32],
    pub settings: PlantImportSettings,
    pub provenance: SourceProvenance,
}

/// Settings that normalize an imported family into Anima's plant vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantImportSettings {
    pub units: SourceUnits,
    pub up_axis: SourceAxis,
    pub forward_axis: SourceAxis,
    pub handedness: SourceHandedness,
    /// Uniform Q15.16 post-unit scale.
    pub scale: DecisionScalar,
    /// Family origin policy after coordinate normalization.
    pub pivot: PlantPivot,
    pub winding: SourceWinding,
    pub uv_origin: SourceUvOrigin,
    /// Q15.16 UV scale applied after vertical-origin normalization.
    pub uv_scale: [DecisionScalar; 2],
    /// Q15.16 UV offset applied after scaling.
    pub uv_offset: [DecisionScalar; 2],
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
    Part(u128),
    Spine(u128),
    MaterialSlot(u32),
    CollisionProxy(u128),
    NavigationProxy(u128),
    Phenotype(u32),
}

/// One manual source-to-family semantic binding that reimport must preserve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantManualSemanticTarget {
    /// Stable binding identity.
    pub id: u128,
    pub source: u128,
    pub selector: PlantSourceSelector,
    pub destination: PlantSemanticDestination,
}

/// One imported-family source recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedPlantFamilyRecipe {
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
        /// External geometry the graph's grafts substitute in, in canonical source order. A graft
        /// names one by identity rather than carrying it, so a recook can write the resolved
        /// content hash back without changing the graph's identity.
        grafts: Vec<PlantSourceReference>,
    },
}

/// Semantic role of a plant part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlantPartSemantic {
    Trunk,
    Branch,
    Root,
    Frond,
    Leaf,
    Flower,
    Fruit,
    Blade,
}

/// One normalized semantic part of a family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPart {
    /// Stable family-local part identity.
    pub id: u128,
    pub parent: Option<u128>,
    pub semantic: PlantPartSemantic,
    pub material_slot: u32,
    /// Source identities contributing geometry or recipe data.
    pub sources: Vec<u128>,
}

/// Physical family dimensions and conservative crown/root footprints, in Q15.16 metres.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlantDimensions {
    pub height: DecisionScalar,
    pub trunk_radius: DecisionScalar,
    /// Crown radius along X/Z.
    pub crown_radius: [DecisionScalar; 2],
    /// Root radius along X/Z.
    pub root_radius: [DecisionScalar; 2],
    /// Conservative local bounds.
    pub local_bounds_min: [DecisionScalar; 3],
    pub local_bounds_max: [DecisionScalar; 3],
}

/// One structural skeleton/spine used by cook, deformation, damage, and proxy derivation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralSpine {
    /// Stable spine identity.
    pub id: u128,
    /// Owning semantic part.
    pub part: u128,
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
    pub damping: UnitInterval,
    pub drag: DecisionScalar,
    /// High-frequency flutter response.
    pub flutter: DecisionScalar,
    /// Maximum bend angle as a signed normalized half-turn.
    pub bend_limit: UnitInterval,
    pub damage_threshold: DecisionScalar,
    pub break_threshold: DecisionScalar,
}

/// Species-declared phenotype role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhenotypeRole {
    Healthy,
    Harvested,
    Damaged,
    Burned,
    Dead,
    Flowering,
    Fruiting,
    Senescent,
    Wet,
}

/// One plant phenotype/life-state variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantPhenotype {
    /// Stable family-local phenotype identity.
    pub id: u32,
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
    Sphere,
    Capsule,
    /// Convex hull derived from the named part set.
    ConvexHull,
}

/// One collision/breakage proxy declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantCollisionProxy {
    /// Stable proxy identity.
    pub id: u128,
    pub shape: PlantCollisionShape,
    /// Owning semantic part/spine.
    pub part: u128,
    /// Q15.16 local center.
    pub center: [DecisionScalar; 3],
    /// Q15.16 half extents, or radius plus half-height.
    pub dimensions: [DecisionScalar; 3],
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
    /// Preferred `(channel, minimum, maximum)` range per shared field channel.
    pub fields: Vec<(FieldChannel, DecisionScalar, DecisionScalar)>,
    pub surface_tags: Vec<u64>,
    /// Closed shade amount tolerated without reducing community suitability.
    pub shade_tolerance: UnitInterval,
}

/// How one species relates to another growing nearby.
///
/// Relations are declared by the species and read from the compiled family, so a rule never needs
/// the asset catalog at tick time.
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
    pub kind: PlantRelationKind,
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

/// Whether a family places in a world or exists as a reusable preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlantFamilyRole {
    /// An ordinary family a biome may place.
    #[default]
    Family,
    /// A reusable preset another family's graph calls, authored as an ordinary `.splant`.
    Module,
}

/// One call site's binding of a `.splant` module.
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

/// One normalized `.splant` plant-family asset.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantFamilyAsset {
    pub version: u32,
    /// Catalog identity.
    pub id: Uuid,
    pub name: String,
    /// Canonically sorted unique family classification identities.
    pub tags: Vec<crate::PlantTagId>,
    /// Exactly one recook source.
    pub source: PlantFamilySource,
    pub parts: Vec<PlantPart>,
    pub dimensions: PlantDimensions,
    /// Material asset slots.
    pub material_slots: Vec<Uuid>,
    pub spines: Vec<StructuralSpine>,
    pub mechanics: MechanicalResponse,
    pub variations: Vec<PlantVariation>,
    pub phenotypes: Vec<PlantPhenotype>,
    pub collision_proxies: Vec<PlantCollisionProxy>,
    pub navigation_proxies: Vec<PlantNavigationProxy>,
    /// Default interaction policy.
    pub interaction_policy: InteractionPolicy,
    pub habitat: Option<HabitatPreferences>,
    pub ecology: PlantEcologyDeclaration,
    /// Whether the family places in a world or exists to be called by one.
    pub role: PlantFamilyRole,
    /// The `.splant` modules this family's graph calls, by call-site GUID.
    pub modules: Vec<PlantModuleReference>,
    /// How deep the module chain below this family may reach.
    pub module_recursion_limit: u16,
}
