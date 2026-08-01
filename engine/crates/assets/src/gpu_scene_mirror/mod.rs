//! Journal-driven synchronization of the renderer's persistent GPU scene.
//!
//! [`GpuSceneMirror`] is the sole bridge from canonical engine state to the derived render
//! mirror: it consumes the scene mutation journal and the asset mutation journal from
//! retained cursors, resolves render-relevant entities (mesh instances and punctual
//! lights) and their assets (geometry, hierarchy pages, materials, textures, coverage)
//! into device records inserted into [`GlobalGpuData`], and applies typed
//! [`PersistentGpuScene`] deltas referencing those records. Preparation therefore scales
//! with changes, not with scene size.
//!
//! A cursor that fell out of retained history, a replaced catalog, or a different
//! [`Scene`] instance bound to a world (the play duplicate, a preview scene, a thumbnail
//! scene) falls back to a complete rebuild from live state — the mirror never trusts a
//! cursor across journal streams.
//!
//! Device-record byte staging, arena uploads, and tombstones flow through the target's
//! [`GpuScenePendingUploads`] queue into the frame's graph-owned transfer passes.

use std::any::TypeId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use saffron_core::{BlendMode, Uuid};
use saffron_geometry::glam::{Mat4, Vec3};
use saffron_geometry::{PortableHierarchyPage, Vertex};
use saffron_material::{
    AlphaClassification, CoverageMipMetadata, CoverageSource, MaterialSurface,
    OpacityMicromapDerivation, SurfaceModel,
};
use saffron_rendering::{
    DEFAULT_WHITE_SLOT, GPU_PAGE_FLAG_GUARANTEED_ROOT, GPU_SCENE_INSTANCE_FLAG_ATTACHED,
    GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS, GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD,
    GPU_SCENE_INSTANCE_FLAG_WIND, GPU_SCENE_INSTANCE_POLICY_SHIFT, GlobalGpuData,
    GlobalGpuTableKind, GpuArenaRange, GpuArenaUploadRequest, GpuCoverageRecord,
    GpuFieldDirectoryEntry, GpuFieldTileRecord, GpuGeometryRecord, GpuHandle, GpuMaterialClass,
    GpuMaterialTableRecord, GpuMesh, GpuPageRecord, GpuSceneAttachmentColumns,
    GpuSceneDynamicTransform, GpuSceneInstanceHandle, GpuSceneInstanceRecord, GpuSceneLightHandle,
    GpuSceneLightRecord, GpuSceneMaterialHandle, GpuSceneMaterialOverride, GpuSceneMaterialRecord,
    GpuScenePageBounds, GpuScenePageHandle, GpuScenePageRecord, GpuScenePendingUploads,
    GpuScenePrototypeHandle, GpuScenePrototypeRecord, GpuSceneSharedDelta,
    GpuSceneSharedDeltaResult, GpuSceneStaticTransform, GpuSceneTransform,
    GpuSceneVegetationColumns, GpuSceneWorldDelta, GpuSceneWorldDeltaResult, GpuSceneWorldId,
    GpuSidedness, GpuSubmeshRecord, GpuTexture, GpuTextureTableRecord, GpuTransparency,
    PageResidency, PersistentGpuScene, Renderer, SubmeshMaterial, Uploader,
    resolve_material_params,
};
use saffron_scene::{
    Entity, MaterialSet, MaterialSlot, Mesh as MeshComponent, PlantVariant, PointLight, Scene,
    SceneJournalCursor, SceneJournalRead, SceneMutationKind, SceneRevision, SkinnedMesh, SpotLight,
    WorldTransform,
};
use saffron_spatial::{UnitInterval, WorldCellKey};
use saffron_vegetation::{ContentHash, MicroFieldTile, PlantId, PlantLifecycle, VegetationWorld};

use crate::gpu::{GpuUploader, RendererUploader};
use crate::page_stream::{PageLoadRequest, PagePayloadSource, PageStreamWorker};
use crate::render_scene::{gpu_point_light, gpu_spot_light};
use crate::{
    AssetInvalidations, AssetJournalCursor, AssetJournalRead, AssetMutationKind,
    AssetMutationTarget, AssetServer, Error, MaterialAsset, Result,
};

mod facts;
mod field_tiles;
mod pages;
mod resolve;
mod shared;
mod sync;
mod vegetation;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use shared::packed_mechanics;

use field_tiles::{
    pack_field_tiles, rebuild_field_directory, reconcile_field_instances, remove_cell_fields,
};
use resolve::{resolve_entity, retire_instance_deformation};
use sync::{light_record, remove_instance};
use vegetation::remove_cell_plants;

/// The renderer-owned halves the mirror writes into during one sync.
pub struct GpuSceneMirrorTarget<'a> {
    /// Device-global arenas and immutable metadata tables.
    pub gpu_data: &'a mut GlobalGpuData,
    /// The persistent derived scene mirror.
    pub gpu_scene: &'a mut PersistentGpuScene,
    /// The shared 1×1 default-white texture backing texture-less material slots.
    pub default_white: &'a Arc<GpuTexture>,
    /// Resident-record stages, retirements, and arena uploads for the frame translation.
    pub pending: &'a mut GpuScenePendingUploads,
    /// The page-payload residency authority pages register with.
    pub residency: &'a mut PageResidency,
}

/// Mirror population and rebuild counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuSceneMirrorStats {
    /// Mirrored mesh assets (prototypes).
    pub meshes: usize,
    /// Interned resolved material variants.
    pub materials: usize,
    /// Interned texture-table records.
    pub textures: usize,
    /// Instances across every synced world.
    pub instances: usize,
    /// Punctual lights across every synced world.
    pub lights: usize,
    /// Entities whose referenced mesh is currently unresolvable.
    pub unresolved_instances: usize,
    /// Host bytes the mirrored meshes retain for surface queries.
    pub retained_mesh_bytes: u64,
    /// Complete shared-record rebuilds (asset journal overflow or catalog replacement).
    pub shared_rebuilds: u64,
    /// Complete world rebuilds (scene journal overflow or a rebound scene instance).
    pub world_rebuilds: u64,
    /// Cooked density upper bound of micro blade candidates across every synced
    /// world's resident-tile directory (no view term; the per-frame generated count
    /// never exceeds it).
    pub micro_predicted: u64,
}

/// Which component produced a mirrored instance; one entity may carry both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum InstanceSource {
    Static,
    Skinned,
}

/// Which punctual-light component produced a mirrored light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MirrorLightKind {
    Point,
    Spot,
}

/// Content identity of one resolved material variant: the referenced `.smat` plus the
/// canonical sorted form of the per-object JSON overrides.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MaterialKey {
    material: u64,
    overrides: String,
    /// The plant family whose packed atlas replaces this material's own textures, or 0.
    ///
    /// Two families may bind the same material and pack different atlases, so the family has to
    /// be part of the key — without it the first family's record would be handed to the second,
    /// and its UVs address a rectangle in a different atlas.
    atlas_family: u64,
}

impl MaterialKey {
    const EMPTY_OVERRIDES: &'static str = "{}";

    fn new(slot: &MaterialSlot) -> Self {
        Self {
            material: slot.material.value(),
            overrides: saffron_json::dump_json_sorted(&slot.overrides, -1),
            atlas_family: 0,
        }
    }

    fn from_material(material: Uuid) -> Self {
        Self {
            material: material.value(),
            overrides: Self::EMPTY_OVERRIDES.to_owned(),
            atlas_family: 0,
        }
    }

    /// The same material as read through one family's packed atlas.
    fn from_atlased_material(material: Uuid, family: Uuid) -> Self {
        Self {
            material: material.value(),
            overrides: Self::EMPTY_OVERRIDES.to_owned(),
            atlas_family: family.value(),
        }
    }

    fn default_key() -> Self {
        Self {
            material: 0,
            overrides: Self::EMPTY_OVERRIDES.to_owned(),
            atlas_family: 0,
        }
    }

    fn is_default(&self) -> bool {
        self.material == 0 && self.overrides == Self::EMPTY_OVERRIDES && self.atlas_family == 0
    }

    fn overrides_value(&self) -> saffron_json::Value {
        saffron_json::parse_json(&self.overrides)
            .unwrap_or(saffron_json::Value::Object(saffron_json::Map::new()))
    }
}

struct TextureEntry {
    handle: GpuHandle,
    texture: Arc<GpuTexture>,
    refs: usize,
}

/// The device records backing one interned material variant.
struct MaterialDeviceRecords {
    table: GpuHandle,
    coverage: Option<GpuHandle>,
    parameters: GpuArenaRange,
    textures: Vec<u32>,
}

struct MaterialEntry {
    device: MaterialDeviceRecords,
    scene_handle: GpuSceneMaterialHandle,
    refs: usize,
}

#[derive(Clone, Copy)]
struct MeshPage {
    device: GpuHandle,
    scene: GpuScenePageHandle,
}

/// One baked field's device-table record and its scene reference — retained so mesh
/// refresh and removal can retire both halves.
#[derive(Clone, Copy)]
struct MeshSdf {
    scene: saffron_rendering::GpuSceneSdfHandle,
    device: GpuHandle,
}

struct MeshEntry {
    mesh: Arc<GpuMesh>,
    geometry: GpuHandle,
    vertex_range: GpuArenaRange,
    index_range: GpuArenaRange,
    submesh_range: GpuArenaRange,
    parts_range: GpuArenaRange,
    pages: Vec<MeshPage>,
    payload_source: Option<PagePayloadSource>,
    /// One entry per baked field, in [`GpuMesh::sdfs`] order.
    sdfs: Vec<MeshSdf>,
    prototype: GpuScenePrototypeHandle,
    slot_count: u32,
    /// The mesh's authored mechanical response as published to its prototype record,
    /// retained so a debug capture can report what the prepass was given.
    mechanics: [u32; 4],
    refs: usize,
}

struct InstanceEntry {
    handle: GpuSceneInstanceHandle,
    mesh: u64,
    overrides: Vec<(u32, MaterialKey)>,
    deformation: Option<DeformationEntry>,
    world_revision: SceneRevision,
    previous_world_revision: SceneRevision,
    facts: InstanceFacts,
}

/// One mirrored instance's derived render facts, resolved when the instance resolves so the
/// frame driver reads them instead of re-deriving them from the ECS and the asset server on
/// every frame. Everything here is a pure function of the instance's mesh, world transform,
/// and resolved materials — the three things a journal touch already re-resolves.
#[derive(Clone)]
struct InstanceFacts {
    /// The mesh supplying the instance's acceleration structure and bind-space bounds.
    mesh: Arc<GpuMesh>,
    /// The instance's world transform.
    model: Mat4,
    /// World-space bounds of `mesh` under `model`, the reach cut's test subject.
    bounds: (Vec3, Vec3),
    /// Opacity the instance forces over what its structure was built with, or `None` when
    /// the resolved materials agree with the cooked ones (see [`RtInstanceInput`]).
    opacity_override: Option<bool>,
    /// The assembly combination the instance places.
    combination: u32,
    /// The displacement the resolved materials select, when any submesh displaces.
    displace: Option<saffron_rendering::DisplaceInfo>,
}

/// One world's ray-instance cut, and what it was cut for.
///
/// A frame reuses it whole while neither term moved: the mirror clears it on any resolve, and
/// `window` pins the reach window it was cut against. That window is snapped to the coarsest
/// distance-field cascade's voxels, so ordinary camera motion does not move it.
struct RayCut {
    window: (Vec3, Vec3),
    instances: Arc<[saffron_rendering::RtInstanceInput]>,
    culled: u32,
}

/// A frame's ray instances for one scene, and what deriving them cost.
pub struct FrameRayInstances {
    /// The instances inside the reach window, for the frame's top-level structure.
    pub instances: Arc<[saffron_rendering::RtInstanceInput]>,
    /// Instances dropped for sitting outside it.
    pub culled: u32,
    /// Mirrored instances this call walked; zero when the cached cut was reused whole.
    pub derived: u32,
}

/// One mirrored instance's deformation inputs, as the frame driver needs them.
pub struct MirroredInstance {
    /// The mesh supplying its geometry.
    pub mesh: Arc<GpuMesh>,
    /// Its world transform.
    pub model: Mat4,
    /// Its stable GPU-scene instance slot.
    pub instance_slot: u32,
    /// The displacement its resolved materials select, when any submesh displaces.
    pub displace: Option<saffron_rendering::DisplaceInfo>,
}

/// One skinned instance's stable deformation identity: the scene record plus the
/// provider element and its parameter words (patched per frame with the skinning
/// plan's palette and deformed offsets).
#[derive(Clone, Copy)]
struct DeformationEntry {
    scene: saffron_rendering::GpuSceneDeformationHandle,
    provider_range: GpuArenaRange,
    params_range: GpuArenaRange,
}

struct LightEntry {
    handle: GpuSceneLightHandle,
    record: GpuSceneLightRecord,
}

/// What one mirrored GPU-scene instance slot was translated from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorInstanceIdentity {
    /// A scene entity.
    Entity(Entity),
    /// A resident macro plant, addressed by its owning cell and stable identity.
    Plant {
        /// The cell the plant streams from.
        cell: WorldCellKey,
        /// The plant's stable identity.
        plant: PlantId,
    },
    /// A micro vegetation field's anchor instance. Its blade records are regenerated every
    /// frame and are never persistent objects.
    MicroField,
}

#[derive(Default)]
struct WorldMirror {
    scene_instance: Uuid,
    cursor: SceneJournalCursor,
    /// The skinning toggle this world last resolved under; a flip re-resolves every
    /// skinned entity so its deformation slot appears or retires.
    skinning: Option<bool>,
    instances: HashMap<(Entity, InstanceSource), InstanceEntry>,
    /// Static-source entities whose resolved materials displace, maintained as each instance
    /// resolves. Kept as a set rather than filtered out of `instances` per frame so the frame
    /// driver's displacement candidate list costs what displaces, not what the world holds.
    displaced: HashSet<Entity>,
    lights: HashMap<(Entity, MirrorLightKind), LightEntry>,
    unresolved: HashMap<(Entity, InstanceSource), u64>,
    dirty: HashSet<Entity>,
    /// Mirrored macro-plant instances keyed by stable identity — never a slot index.
    plants: HashMap<(WorldCellKey, PlantId), PlantInstanceEntry>,
    /// The vegetation cell generation and bulk-suppression revision each mirrored cell was
    /// translated from; a republished cell, or one whose promoted-plant set moved,
    /// re-translates atomically in one sync pass.
    plant_cells: HashMap<WorldCellKey, (u64, u64)>,
    /// Each mirrored cell's packed micro field tiles in the fields arena (absent when
    /// the cell carries none).
    plant_fields: HashMap<WorldCellKey, CellFieldEntry>,
    /// One flagged identity instance per family with resident field tiles; its blade
    /// records anchor material and identity resolution.
    field_instances: HashMap<u64, GpuSceneInstanceHandle>,
    /// The packed resident-tile directory the micro pass dispatches over.
    field_directory: Option<(GpuArenaRange, u32)>,
    /// Cooked density upper bound of blade candidates across the directory.
    micro_predicted: u64,
    /// The seasonal phase the mirrored combinations were resolved at.
    season_mille: u16,
    /// The world's ray-instance cut, absent until a frame asks for one and cleared by any
    /// mutation that could move an instance in or out of it.
    rays: Option<RayCut>,
}

/// Per-family and per-cell vegetation population rows for diagnostics.
#[derive(Clone, Debug, Default)]
pub struct VegetationRenderBreakdown {
    /// One row per family with resident plants or field tiles.
    pub families: Vec<VegetationFamilyRenderRow>,
    /// One row per cell with resident plants or field tiles.
    pub cells: Vec<VegetationCellRenderRow>,
}

/// One family's resident render population.
#[derive(Clone, Copy, Debug)]
pub struct VegetationFamilyRenderRow {
    /// The family asset id value.
    pub family: u64,
    /// Mirrored plant instances of the family.
    pub instances: u32,
    /// Resident micro field tiles of the family.
    pub field_tiles: u32,
    /// Cooked density upper bound of the family's blade candidates.
    pub micro_predicted: u64,
}

/// What a resident vegetation population is expected to stay inside, per owner.
///
/// A breach names the cell or family that is over; the renderer sees passes and counters, not
/// cells and families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VegetationBudgets {
    /// Mirrored plants one cell may hold.
    pub cell_plants: u32,
    /// Mirrored instances one family may hold across every resident cell.
    pub family_instances: u32,
    /// Cooked blade-candidate upper bound one family may contribute.
    pub family_micro_predicted: u64,
}

impl Default for VegetationBudgets {
    fn default() -> Self {
        // Generous by design: the point is to catch a runaway cell or family, not to narrate an
        // ordinary scene.
        Self {
            cell_plants: 4_096,
            family_instances: 16_384,
            family_micro_predicted: 4_000_000,
        }
    }
}

/// One cell's resident render population.
#[derive(Clone, Copy, Debug)]
pub struct VegetationCellRenderRow {
    /// The owning cell.
    pub cell: WorldCellKey,
    /// Mirrored plant instances in the cell.
    pub plants: u32,
    /// Resident micro field tiles in the cell.
    pub field_tiles: u32,
}

/// One mirrored cell's packed micro field tiles: the arena blob plus each tile's
/// family and offset within it.
struct CellFieldEntry {
    range: GpuArenaRange,
    /// Per tile: the family, the tile's byte offset within `range`, and the cooked
    /// density upper bound of its blade candidates (no view term).
    tiles: Vec<(u64, u32, u32)>,
}

/// One mirrored macro plant: its persistent-scene instance plus the shared records it
/// holds references on.
struct PlantInstanceEntry {
    handle: GpuSceneInstanceHandle,
    mesh: u64,
    overrides: Vec<(u32, MaterialKey)>,
    /// The live instance record, kept for in-place combination-flip updates.
    record: GpuSceneInstanceRecord,
}

#[derive(Default)]
struct SharedMirror {
    textures: HashMap<u32, TextureEntry>,
    materials: HashMap<MaterialKey, MaterialEntry>,
    meshes: HashMap<u64, MeshEntry>,
    /// Resident page-table slot → (mesh key, cook page id) for streaming lookups.
    page_lookup: HashMap<u32, (u64, u32)>,
    generation: u32,
    content_revision: u64,
}

/// Journal-driven bridge from a [`Scene`] + [`AssetServer`] to the renderer's persistent
/// GPU scene. One mirror serves every world; each host and player owns exactly one.
#[derive(Default)]
pub struct GpuSceneMirror {
    asset_cursor: Option<AssetJournalCursor>,
    shared: SharedMirror,
    worlds: BTreeMap<u64, WorldMirror>,
    shared_rebuilds: u64,
    world_rebuilds: u64,
    worker: PageStreamWorker,
    vegetation_budgets: VegetationBudgets,
    /// Measures each world's eye between frames so page demand leads the camera. Reuses the
    /// residency tracker so there is one smoothing rule for predicted demand in the engine.
    /// Keyed by world because the views share this mirror: a thumbnail excursion between two
    /// scene frames would otherwise read as the camera teleporting and zero its estimate.
    eye_motion: BTreeMap<u64, saffron_spatial::SourceMotion>,
}

struct SyncCtx<'a, 'b> {
    scene: &'a Scene,
    assets: &'a mut AssetServer,
    gpu: &'a dyn GpuUploader,
    target: &'a mut GpuSceneMirrorTarget<'b>,
}

fn gpu_scene_error(error: saffron_rendering::GpuSceneError) -> Error {
    Error::Render(saffron_rendering::Error::GpuScene(error))
}

fn mirror_error(message: impl Into<String>) -> Error {
    Error::GpuSceneMirror(message.into())
}

const EMPTY_RANGE: GpuArenaRange = GpuArenaRange { first: 0, count: 0 };

/// Ceiling on page loads queued at the worker at once.
const PAGE_STREAM_MAX_IN_FLIGHT: usize = 64;

/// Half-diagonal of a cooked page's static bounds, in metres.
fn page_bounds_radius(page: &PortableHierarchyPage) -> f32 {
    let extent = [
        (page.bounds.max_bits[0] - page.bounds.min_bits[0]) as f32 / 65_536.0,
        (page.bounds.max_bits[1] - page.bounds.min_bits[1]) as f32 / 65_536.0,
        (page.bounds.max_bits[2] - page.bounds.min_bits[2]) as f32 / 65_536.0,
    ];
    0.5 * (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2]).sqrt()
}

/// Deterministic FNV-1a over the material identity, anchoring stochastic coverage for
/// materials whose `.smat` carries no authored salt.
fn coverage_salt(key: &MaterialKey) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ key.material;
    for byte in key.overrides.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn material_class(material: &SubmeshMaterial, unlit: bool) -> GpuMaterialClass {
    let classification = material.thin_sheet.as_ref().map_or_else(
        || match material.blend_mode {
            BlendMode::Opaque => AlphaClassification::Opaque,
            BlendMode::Masked => AlphaClassification::Masked,
            BlendMode::Blend => AlphaClassification::Transmissive,
        },
        |sheet| sheet.coverage_classification,
    );
    let sidedness = if material.double_sided {
        GpuSidedness::Double
    } else {
        GpuSidedness::Single
    };
    let surface_model = if material.thin_sheet.is_some() {
        SurfaceModel::ThinSheetFoliage
    } else {
        SurfaceModel::Standard
    };
    let transparency = if material.blend_mode == BlendMode::Blend {
        GpuTransparency::AlphaBlended
    } else {
        GpuTransparency::Opaque
    };
    GpuMaterialClass::new(
        classification,
        sidedness,
        surface_model,
        transparency,
        unlit,
    )
}

/// Journal mutation classes the mirror reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Touch {
    Content,
    Transform,
}

fn classify_mutation(kind: SceneMutationKind) -> Option<Touch> {
    match kind {
        SceneMutationKind::EntityCreated | SceneMutationKind::EntityDestroyed => {
            Some(Touch::Content)
        }
        SceneMutationKind::ComponentUpdated(type_id) => {
            if type_id == TypeId::of::<WorldTransform>() {
                Some(Touch::Transform)
            } else if is_content_component(type_id) {
                Some(Touch::Content)
            } else {
                None
            }
        }
        SceneMutationKind::ComponentAdded(type_id)
        | SceneMutationKind::ComponentRemoved(type_id) => {
            (type_id == TypeId::of::<WorldTransform>() || is_content_component(type_id))
                .then_some(Touch::Content)
        }
    }
}

fn is_content_component(type_id: TypeId) -> bool {
    type_id == TypeId::of::<MeshComponent>()
        || type_id == TypeId::of::<SkinnedMesh>()
        || type_id == TypeId::of::<MaterialSet>()
        || type_id == TypeId::of::<PointLight>()
        || type_id == TypeId::of::<SpotLight>()
}

impl GpuSceneMirror {
    /// Constructs an empty mirror; every world populates on its first sync.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A sound upper bound on the draw records the visibility pass can emit: one record per
    /// mirrored instance per submesh slot.
    ///
    /// It over-counts and never under-counts, which is the direction that matters — a bound below
    /// the real count would drop geometry on a device without `drawIndirectCount`, where the
    /// fixed-slice draws are issued per buffer slot rather than per live record.
    #[must_use]
    pub fn live_draw_record_bound(&self) -> u32 {
        let instances: usize = self
            .worlds
            .values()
            .map(|world| world.instances.len() + world.plants.len())
            .sum();
        let widest = self
            .shared
            .meshes
            .values()
            .map(|entry| entry.slot_count)
            .max()
            .unwrap_or(1)
            .max(1);
        u32::try_from(instances)
            .unwrap_or(u32::MAX)
            .saturating_mul(widest)
    }

    /// Current population and rebuild counters.
    #[must_use]
    pub fn stats(&self) -> GpuSceneMirrorStats {
        GpuSceneMirrorStats {
            meshes: self.shared.meshes.len(),
            materials: self.shared.materials.len(),
            textures: self.shared.textures.len(),
            instances: self
                .worlds
                .values()
                .map(|w| w.instances.len() + w.plants.len())
                .sum(),
            lights: self.worlds.values().map(|w| w.lights.len()).sum(),
            unresolved_instances: self.worlds.values().map(|w| w.unresolved.len()).sum(),
            retained_mesh_bytes: self
                .shared
                .meshes
                .values()
                .map(|entry| entry.mesh.retained_query_cpu_bytes())
                .sum(),
            shared_rebuilds: self.shared_rebuilds,
            world_rebuilds: self.world_rebuilds,
            micro_predicted: self.worlds.values().map(|w| w.micro_predicted).sum(),
        }
    }

    /// The GPU-scene instance slot mirroring `entity` in the world bound to the scene
    /// whose [`Scene::instance_id`] is `scene_instance`, for stable TLAS references.
    #[must_use]
    pub fn instance_slot(
        &self,
        scene_instance: Uuid,
        entity: Entity,
        skinned: bool,
    ) -> Option<u32> {
        let world = self
            .worlds
            .values()
            .find(|world| world.scene_instance == scene_instance)?;
        let source = if skinned {
            InstanceSource::Skinned
        } else {
            InstanceSource::Static
        };
        world
            .instances
            .get(&(entity, source))
            .map(|entry| entry.handle.raw().index)
    }

    /// The GPU-scene instance slot mirroring one resident plant, and the family's
    /// authored mechanical response, for the wind-record capture.
    ///
    /// A plant is not an ECS entity — it streams from the cell store — so the entity lookup
    /// cannot reach it.
    #[must_use]
    pub fn plant_instance_slot(
        &self,
        cell: WorldCellKey,
        plant: PlantId,
    ) -> Option<(u32, [u32; 4])> {
        let entry = self
            .worlds
            .values()
            .find_map(|world| world.plants.get(&(cell, plant)))?;
        let mechanics = self
            .shared
            .meshes
            .get(&entry.mesh)
            .map_or([0; 4], |mesh| mesh.mechanics);
        Some((entry.handle.raw().index, mechanics))
    }

    /// What a GPU-scene instance slot mirrors, for the selection readback: the entity it was
    /// translated from, the plant it streams from, or a micro field's anchor.
    ///
    /// The mirror is keyed by identity in both directions on purpose — a slot is a device
    /// address, so a pick answers with the entity or the [`PlantId`] behind it, never the slot.
    #[must_use]
    pub fn identify_instance_slot(
        &self,
        world: GpuSceneWorldId,
        slot: u32,
    ) -> Option<MirrorInstanceIdentity> {
        let world = self.worlds.get(&world.0)?;
        if let Some((entity, _)) = world
            .instances
            .iter()
            .find(|(_, entry)| entry.handle.raw().index == slot)
            .map(|((entity, source), _)| (*entity, *source))
        {
            return Some(MirrorInstanceIdentity::Entity(entity));
        }
        if let Some((cell, plant)) = world
            .plants
            .iter()
            .find(|(_, entry)| entry.handle.raw().index == slot)
            .map(|(key, _)| *key)
        {
            return Some(MirrorInstanceIdentity::Plant { cell, plant });
        }
        world
            .field_instances
            .values()
            .any(|handle| handle.raw().index == slot)
            .then_some(MirrorInstanceIdentity::MicroField)
    }

    /// Host bytes retained across mirrored meshes for exact surface and deformation
    /// queries (the render-stats `retained_mesh_cpu_bytes` source).
    #[must_use]
    pub fn retained_mesh_cpu_bytes(&self) -> u64 {
        self.shared
            .meshes
            .values()
            .map(|entry| entry.mesh.retained_query_cpu_bytes())
            .sum()
    }
}
