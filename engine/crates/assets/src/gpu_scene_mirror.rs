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
//! Deformation providers, skeletons, and SDF references stay unbound here until the
//! deformation rehoming slice of the renderer cutover; instances carry `None` for both
//! optional references. Device-record byte staging, arena uploads, and tombstones flow
//! through the target's [`GpuScenePendingUploads`] queue into the frame's graph-owned
//! transfer passes.

use std::any::TypeId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use saffron_core::{BlendMode, Uuid};
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
    GpuScenePageHandle, GpuScenePageRecord, GpuScenePendingUploads, GpuScenePrototypeHandle,
    GpuScenePrototypeRecord, GpuSceneSharedDelta, GpuSceneSharedDeltaResult,
    GpuSceneStaticTransform, GpuSceneTransform, GpuSceneVegetationColumns, GpuSceneWorldDelta,
    GpuSceneWorldDeltaResult, GpuSceneWorldId, GpuSidedness, GpuSubmeshRecord, GpuTexture,
    GpuTextureTableRecord, GpuTransparency, PageResidency, PersistentGpuScene, Renderer,
    SubmeshMaterial, Uploader, resolve_material_params,
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
}

impl MaterialKey {
    const EMPTY_OVERRIDES: &'static str = "{}";

    fn new(slot: &MaterialSlot) -> Self {
        Self {
            material: slot.material.value(),
            overrides: saffron_json::dump_json_sorted(&slot.overrides, -1),
        }
    }

    fn from_material(material: Uuid) -> Self {
        Self {
            material: material.value(),
            overrides: Self::EMPTY_OVERRIDES.to_owned(),
        }
    }

    fn default_key() -> Self {
        Self {
            material: 0,
            overrides: Self::EMPTY_OVERRIDES.to_owned(),
        }
    }

    fn is_default(&self) -> bool {
        self.material == 0 && self.overrides == Self::EMPTY_OVERRIDES
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

struct MeshEntry {
    mesh: Arc<GpuMesh>,
    geometry: GpuHandle,
    vertex_range: GpuArenaRange,
    index_range: GpuArenaRange,
    submesh_range: GpuArenaRange,
    parts_range: GpuArenaRange,
    pages: Vec<MeshPage>,
    payload_source: Option<PagePayloadSource>,
    prototype: GpuScenePrototypeHandle,
    slot_count: u32,
    refs: usize,
}

struct InstanceEntry {
    handle: GpuSceneInstanceHandle,
    mesh: u64,
    overrides: Vec<(u32, MaterialKey)>,
    deformation: Option<DeformationEntry>,
    world_revision: SceneRevision,
    previous_world_revision: SceneRevision,
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

#[derive(Default)]
struct WorldMirror {
    scene_instance: Uuid,
    cursor: SceneJournalCursor,
    /// The skinning toggle this world last resolved under; a flip re-resolves every
    /// skinned entity so its deformation slot appears or retires.
    skinning: Option<bool>,
    instances: HashMap<(Entity, InstanceSource), InstanceEntry>,
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

    /// Current population and rebuild counters.
    /// A sound upper bound on the draw records the visibility pass can emit for this mirror: one
    /// record per mirrored instance per submesh slot.
    ///
    /// A device without `drawIndirectCount` cannot read the real per-bucket count on the GPU, so
    /// its fixed-slice draws would otherwise have to issue one command per *buffer slot* —
    /// tens of thousands of no-op draws per frame regardless of how little is on screen. This is
    /// the bound that keeps those draws proportional to the scene. It over-counts (not every
    /// instance is visible, and the densest mesh's slot count stands in for all of them) and never
    /// under-counts, which is the direction that matters: a bound below the real count would drop
    /// geometry.
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

    /// The global material-parameter arena index of `entity`'s slot-0 material — its
    /// slot-0 override when present, else the default material — for the tessellation
    /// seam's instance rows (the mesh fragments index the arena at set 2, binding 2).
    #[must_use]
    pub fn material_parameter_index(&self, scene_instance: Uuid, entity: Entity) -> Option<u32> {
        let world = self
            .worlds
            .values()
            .find(|world| world.scene_instance == scene_instance)?;
        let entry = world
            .instances
            .get(&(entity, InstanceSource::Static))
            .or_else(|| world.instances.get(&(entity, InstanceSource::Skinned)))?;
        let key = entry
            .overrides
            .iter()
            .find(|(slot, _)| *slot == 0)
            .map_or_else(MaterialKey::default_key, |(_, key)| key.clone());
        self.shared
            .materials
            .get(&key)
            .map(|material| material.device.parameters.first)
    }

    /// Synchronizes `world` from `scene` and the shared asset state through a live
    /// renderer: the production entry point for host and player frame loops.
    ///
    /// # Errors
    ///
    /// Propagates GPU-scene delta validation, device-table, and resolution failures.
    pub fn sync_renderer_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        vegetation: Option<&VegetationWorld>,
        assets: &mut AssetServer,
        renderer: &mut Renderer,
        uploader: &Uploader,
    ) -> Result<bool> {
        let descriptors = renderer.descriptors_arc();
        let skinning = renderer.skinning_enabled();
        let default_white = Arc::clone(renderer.default_white());
        let gpu = RendererUploader::new(uploader, &descriptors, skinning);
        let view = renderer.page_demand_view();
        let flip_stamp = renderer.frame_serial() as u32;
        let (gpu_data, gpu_scene, pending, residency) = renderer.gpu_scene_parts_mut();
        let mut target = GpuSceneMirrorTarget {
            gpu_data,
            gpu_scene,
            default_white: &default_white,
            pending,
            residency,
        };
        self.sync_world(world, scene, assets, &gpu, &mut target)?;
        let mut vegetation_mutated = false;
        if let Some(vegetation) = vegetation {
            let calendar = &scene.environment.time_of_day;
            let season_mille = saffron_vegetation::season_phase_mille(
                calendar.year,
                calendar.month,
                calendar.day,
                calendar.latitude,
            );
            vegetation_mutated = self.sync_vegetation(
                world,
                vegetation,
                assets,
                &gpu,
                season_mille,
                flip_stamp,
                &mut target,
            )?;
        }
        self.drive_page_streaming(world, scene, Some(view), &mut target)?;
        let bins = self.live_executor_bins(target.gpu_data);
        let GpuSceneMirrorTarget { .. } = target;
        renderer.set_live_executor_bins(bins);
        renderer.set_micro_field_directory(self.micro_field_directory(world));
        renderer.set_live_draw_record_bound(self.live_draw_record_bound());
        Ok(vegetation_mutated)
    }

    /// The world's resident micro-field tile directory (fields-arena byte offset +
    /// entry count), or `None` while no field tiles are resident.
    #[must_use]
    pub fn micro_field_directory(&self, world: GpuSceneWorldId) -> Option<(u32, u32)> {
        self.worlds
            .get(&world.0)
            .and_then(|state| state.field_directory)
            .map(|(range, count)| (range.first, count))
    }

    /// Pushes the frame's deformation-provider parameter patches: each skinned
    /// instance's palette and deformed offsets from the submitted draw list land in
    /// its stable provider params. Unknown entities (not yet mirrored this frame) are
    /// skipped; they patch on a later frame once mirrored.
    pub fn patch_frame_deformations(
        &self,
        world: GpuSceneWorldId,
        scene: &Scene,
        deformations: &[saffron_rendering::SkinnedDeformation],
        pending: &mut GpuScenePendingUploads,
    ) -> Vec<saffron_rendering::GpuSceneInstanceHandle> {
        let Some(world_state) = self.worlds.get(&world.0) else {
            return Vec::new();
        };
        let mut deformed = Vec::with_capacity(deformations.len());
        for deformation in deformations {
            let Some(entry) = scene
                .find_entity_by_uuid(Uuid(deformation.entity))
                .and_then(|entity| {
                    world_state
                        .instances
                        .get(&(entity, InstanceSource::Skinned))
                })
            else {
                continue;
            };
            let Some(allocation) = entry.deformation else {
                continue;
            };
            pending.upload_arena(GpuArenaUploadRequest::DeformationParameters {
                range: allocation.params_range,
                data: vec![
                    deformation.deformed_offset,
                    deformation.deformed_offset,
                    deformation.joint_offset,
                    deformation.joint_count,
                    deformation.vertex_count,
                ],
            });
            deformed.push(entry.handle);
        }
        deformed
    }

    /// Every live (executor shader index, material class bits) pair across the
    /// interned materials — the populated executor bins the draw sites iterate.
    #[must_use]
    pub fn live_executor_bins(&self, gpu_data: &GlobalGpuData) -> Vec<(u32, u32)> {
        let mut bins: Vec<(u32, u32)> = self
            .shared
            .materials
            .values()
            .filter_map(|entry| gpu_data.materials.get(entry.device.table))
            .map(|record| (record.shader_index, record.material_class.bits()))
            .collect();
        bins.sort_unstable();
        bins.dedup();
        bins
    }

    /// Drives page-payload streaming for one frame: drains completed loads into the
    /// residency authority (patching child-handle tables), scores the refinement
    /// frontier against the view (projected transition error, frustum probability,
    /// motion), and feeds the load worker within its in-flight budget.
    ///
    /// # Errors
    ///
    /// Propagates payload patch failures; a per-page load failure is reported to the
    /// residency authority and logged, never fatal.
    pub fn drive_page_streaming(
        &mut self,
        world: GpuSceneWorldId,
        scene: &Scene,
        view: Option<saffron_rendering::PageDemandView>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for result in self.worker.drain() {
            let Some(entry) = self.shared.meshes.get(&result.mesh) else {
                target.residency.fail_load(result.handle);
                continue;
            };
            match result.payload {
                Ok(mut payload) => {
                    let mut resolved = true;
                    for (index, cook_child) in payload.child_pages.clone().iter().enumerate() {
                        match entry.pages.get(*cook_child as usize) {
                            Some(page) => payload
                                .patch_child(index, page.device)
                                .map_err(Error::Render)?,
                            None => {
                                resolved = false;
                                break;
                            }
                        }
                    }
                    if resolved {
                        target.residency.complete_load(result.handle, payload.bytes);
                    } else {
                        target.residency.fail_load(result.handle);
                    }
                }
                Err(err) => {
                    tracing::warn!("page stream: {err}");
                    target.residency.fail_load(result.handle);
                }
            }
        }

        if let Some(view) = view {
            struct MeshDemand {
                distance: f32,
                in_frustum: bool,
                moved: bool,
            }
            let mut mesh_stats: HashMap<u64, MeshDemand> = HashMap::new();
            if let Some(world_state) = self.worlds.get(&world.0) {
                for ((entity, _), instance) in &world_state.instances {
                    let Some(state) = scene.world_transform_state(*entity) else {
                        continue;
                    };
                    let position = state.current.col(3).truncate();
                    let distance = (position - view.eye).length().max(0.05);
                    let clip = view.view_proj * position.extend(1.0);
                    let in_frustum = clip.w > 0.0
                        && clip.x.abs() <= clip.w * 1.2
                        && clip.y.abs() <= clip.w * 1.2;
                    let moved = instance.world_revision != instance.previous_world_revision;
                    let stats = mesh_stats.entry(instance.mesh).or_insert(MeshDemand {
                        distance: f32::INFINITY,
                        in_frustum: false,
                        moved: false,
                    });
                    stats.distance = stats.distance.min(distance);
                    stats.in_frustum |= in_frustum;
                    stats.moved |= moved;
                }
            }
            for handle in target.residency.frontier() {
                let Some((mesh, cook)) = self.shared.page_lookup.get(&handle.index) else {
                    continue;
                };
                let Some(stats) = mesh_stats.get(mesh) else {
                    continue;
                };
                let Some(entry) = self.shared.meshes.get(mesh) else {
                    continue;
                };
                let Some(page) = entry.mesh.hierarchy_pages.get(*cook as usize) else {
                    continue;
                };
                let radius = page_bounds_radius(page);
                // The transition total is a composite Q15.16 error; its silhouette
                // component dominates, so metres is the conservative reading.
                let error_metres = page.transition_error.total as f32 / 65_536.0;
                let distance = (stats.distance - radius).max(0.05);
                let mut projected = error_metres * view.proj_scale / distance;
                if !stats.in_frustum {
                    projected *= 0.25;
                }
                if stats.moved {
                    projected *= 2.0;
                }
                if projected > 0.25 {
                    let priority = (projected * 1024.0).min(1e18) as u64;
                    target.residency.demand(handle, priority);
                }
            }
        }

        let budget = PAGE_STREAM_MAX_IN_FLIGHT.saturating_sub(self.worker.in_flight());
        if budget > 0 {
            let mut requests = Vec::new();
            for handle in target.residency.take_load_requests(budget) {
                let Some((mesh, cook)) = self.shared.page_lookup.get(&handle.index).copied() else {
                    target.residency.fail_load(handle);
                    continue;
                };
                let source = self
                    .shared
                    .meshes
                    .get(&mesh)
                    .and_then(|entry| entry.payload_source.clone());
                let Some(source) = source else {
                    target.residency.fail_load(handle);
                    continue;
                };
                requests.push(PageLoadRequest {
                    mesh,
                    page_id: cook,
                    handle,
                    source,
                });
            }
            self.worker.enqueue(requests);
        }
        Ok(())
    }

    /// Synchronizes `world` from `scene` and the shared asset state.
    ///
    /// Reads both journals from the retained cursors, resolves every touched entity and
    /// invalidated asset, and applies only the resulting deltas. Journal overflow, a
    /// replaced catalog, or a rebound scene instance triggers the matching complete
    /// rebuild.
    ///
    /// # Errors
    ///
    /// Propagates GPU-scene delta validation, device-table, and resolution failures.
    pub fn sync_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        scene.update_world_transforms();
        self.consume_asset_journal(assets, gpu, target)?;

        let world_state = self.worlds.entry(world.0).or_default();
        let mut rebuild = world_state.scene_instance != scene.instance_id();
        let skinning = gpu.skinning_enabled();
        if world_state.skinning != Some(skinning) {
            world_state.skinning = Some(skinning);
            if !rebuild {
                scene.for_each::<&SkinnedMesh, _>(|entity, _| {
                    world_state.dirty.insert(entity);
                });
            }
        }
        if !rebuild {
            match scene.read_journal(world_state.cursor) {
                SceneJournalRead::Delta { mutations, next } => {
                    world_state.cursor = next;
                    for mutation in mutations {
                        match classify_mutation(mutation.kind) {
                            Some(Touch::Content) => {
                                world_state.dirty.insert(mutation.entity);
                            }
                            Some(Touch::Transform)
                                if !world_state.dirty.contains(&mutation.entity) =>
                            {
                                apply_transform_update(
                                    world_state,
                                    world,
                                    mutation.entity,
                                    scene,
                                    target,
                                )?;
                            }
                            _ => {}
                        }
                    }
                }
                SceneJournalRead::SnapshotRequired { next } => {
                    world_state.cursor = next;
                    rebuild = true;
                }
            }
        }

        if rebuild {
            self.rebuild_world(world, scene, assets, gpu, target)?;
            return Ok(());
        }

        let world_state = self.worlds.get_mut(&world.0).expect("world synced");
        let dirty = std::mem::take(&mut world_state.dirty);
        let mut ctx = SyncCtx {
            scene,
            assets,
            gpu,
            target,
        };
        for entity in dirty {
            resolve_entity(&mut self.shared, world_state, world, entity, &mut ctx)?;
        }
        Ok(())
    }

    /// Translates the authoritative macro-plant snapshot into persistent-scene instance
    /// deltas: cells diff purely by published generation id, and a changed cell's
    /// instances remove + recreate in one pass, keyed `(cell, PlantId)`. Families
    /// resolve through the manifest's exact `.splantc` identity into the shared mesh
    /// path, so a plant prototype is a mesh prototype. Returns whether any cell
    /// translated or retired — the caller's repaint signal.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_vegetation(
        &mut self,
        world: GpuSceneWorldId,
        vegetation: &VegetationWorld,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        season_mille: u16,
        flip_stamp: u32,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<bool> {
        let mut mutated = false;
        let shared = &mut self.shared;
        let world_state = self.worlds.entry(world.0).or_default();
        let families: HashMap<u64, ContentHash> = vegetation
            .manifest()
            .plants
            .iter()
            .map(|plant| (plant.family.value(), plant.artifact_hash))
            .collect();

        let resident: Vec<_> = vegetation.resident_cells().collect();
        let resident_keys: HashSet<WorldCellKey> = resident.iter().map(|(cell, _)| *cell).collect();
        let stale: Vec<WorldCellKey> = world_state
            .plant_cells
            .keys()
            .filter(|cell| !resident_keys.contains(cell))
            .copied()
            .collect();
        for cell in stale {
            remove_cell_plants(shared, world_state, world, cell, target)?;
            remove_cell_fields(world_state, cell, target)?;
            world_state.plant_cells.remove(&cell);
            mutated = true;
        }

        for (cell, generation) in &resident {
            let cell = *cell;
            let current = (
                generation.id().generation,
                vegetation.cell_bulk_revision(cell),
            );
            if world_state.plant_cells.get(&cell) == Some(&current) {
                continue;
            }
            remove_cell_plants(shared, world_state, world, cell, target)?;
            remove_cell_fields(world_state, cell, target)?;
            mutated = true;
            if let Some(tiles) = generation.micro_fields().filter(|tiles| !tiles.is_empty()) {
                let (packed, tile_slots) = pack_field_tiles(tiles)?;
                let byte_len = u32::try_from(packed.len())
                    .map_err(|_| mirror_error("micro field tiles exceed the fields arena"))?;
                let (range, _) = target.gpu_data.fields.allocate(byte_len, 16)?;
                target.pending.upload_arena(GpuArenaUploadRequest::Fields {
                    range,
                    data: packed,
                });
                world_state.plant_fields.insert(
                    cell,
                    CellFieldEntry {
                        range,
                        tiles: tile_slots,
                    },
                );
            }
            let points = generation.macro_points();
            for index in 0..points.ids.len() {
                if matches!(
                    points.lifecycles[index],
                    PlantLifecycle::Seed | PlantLifecycle::Removed
                ) {
                    continue;
                }
                // A promoted plant renders as its entity view instead, so the bulk instance
                // stays absent for exactly as long as that entity owns it.
                if vegetation.is_bulk_suppressed(points.ids[index]) {
                    continue;
                }
                let family = points.families[index];
                let Some(artifact) = families.get(&family.value()) else {
                    tracing::warn!(
                        "gpu scene mirror: plant family {family:?} is not in the manifest"
                    );
                    continue;
                };
                let Some(render) = assets.load_plant_family(gpu, family, *artifact) else {
                    continue;
                };
                if !shared.ensure_mesh(family.value(), assets, gpu, target)? {
                    continue;
                }
                let slot_count = shared.meshes[&family.value()].slot_count;
                // The rendered phenotype derives from typed lifecycle state and the
                // seasonal phase — never inferred from the active mesh.
                let rendered_phenotype = saffron_vegetation::resolve_rendered_phenotype(
                    render
                        .phenotypes
                        .iter()
                        .map(|row| (row.id, row.role, row.season_window)),
                    points.phenotypes[index],
                    points.lifecycles[index],
                    season_mille,
                );
                // The rendered phenotype remaps material slots before slot resolution.
                let phenotype_remap = render
                    .phenotypes
                    .iter()
                    .find(|phenotype| phenotype.id == rendered_phenotype)
                    .map(|phenotype| Arc::clone(&phenotype.material_remap));
                let mut overrides: Vec<(u32, MaterialKey)> = Vec::new();
                for slot in 0..slot_count {
                    let resolved_slot = phenotype_remap
                        .as_ref()
                        .and_then(|remap| {
                            remap
                                .iter()
                                .find(|(from, _)| *from == slot)
                                .map(|(_, to)| *to)
                        })
                        .unwrap_or(slot);
                    let Some(material) = render.materials.get(resolved_slot as usize) else {
                        continue;
                    };
                    if material.value() == 0 {
                        continue;
                    }
                    overrides.push((slot, MaterialKey::from_material(*material)));
                }
                let mut override_records = Vec::with_capacity(overrides.len());
                for (slot, material_key) in &overrides {
                    let handle = shared.intern_material(material_key, assets, gpu, target)?;
                    override_records.push(GpuSceneMaterialOverride {
                        slot: *slot,
                        material: handle,
                    });
                }
                // The point's (variation, rendered phenotype) resolves to the
                // family's combination-mask index — an exact pair match wins, else
                // the phenotype alone, else the first authored combination.
                let combination = render
                    .combinations
                    .iter()
                    .position(|entry| *entry == (points.variations[index], rendered_phenotype))
                    .or_else(|| {
                        render
                            .combinations
                            .iter()
                            .position(|entry| entry.1 == rendered_phenotype)
                    })
                    .unwrap_or(0) as u32;
                // The point's conservative world bounds become an instance-local
                // pre-scale sphere the cull composes through the transform; previous
                // equals current at rest (deformation drift writes the delta).
                let bounds = plant_bounds_sphere(
                    points.positions[index],
                    points.orientations[index],
                    points.scales[index],
                    points.bounds[index],
                );
                let attachment = points.attachments[index].as_ref().map(|attachment| {
                    GpuSceneAttachmentColumns {
                        provider: attachment.provider.0,
                        primitive: attachment.primitive.0,
                        barycentric: attachment.barycentric.map(|value| value.bits()),
                    }
                });
                let flags = ((points.interaction_policies[index] as u32)
                    << GPU_SCENE_INSTANCE_POLICY_SHIFT)
                    | GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS
                    | GPU_SCENE_INSTANCE_FLAG_WIND
                    | if attachment.is_some() {
                        GPU_SCENE_INSTANCE_FLAG_ATTACHED
                    } else {
                        0
                    };
                let record = GpuSceneInstanceRecord {
                    prototype: shared.meshes[&family.value()].prototype,
                    transform: GpuSceneTransform::Static(GpuSceneStaticTransform::new(
                        points.positions[index],
                        points.orientations[index],
                        points.scales[index],
                        0,
                    )),
                    material_overrides: Arc::from(override_records),
                    deformation: None,
                    sdf: None,
                    source_generation: shared.generation,
                    flags,
                    combination,
                    vegetation: Some(GpuSceneVegetationColumns {
                        bounds_current: bounds,
                        bounds_previous: bounds,
                        attachment,
                        combination_previous: combination,
                        flip_stamp: 0,
                    }),
                };
                for (_, material_key) in &overrides {
                    shared.ref_material(material_key);
                }
                shared.ref_mesh(family.value());
                let result = target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record.clone()))
                    .map_err(gpu_scene_error)?;
                let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
                    unreachable!("create instance returns InstanceCreated");
                };
                world_state.plants.insert(
                    (cell, points.ids[index]),
                    PlantInstanceEntry {
                        handle,
                        mesh: family.value(),
                        overrides,
                        record,
                    },
                );
            }
            world_state.plant_cells.insert(cell, current);
        }
        // A seasonal phase change flips combinations IN PLACE: every live plant
        // whose resolved combination moved gets one UpdateInstance carrying the
        // previous combination and the flip stamp, so the traversal crossfades the
        // assembly uses instead of popping. Structural cell changes above already
        // resolved with the new season.
        if world_state.season_mille != season_mille {
            for (cell, generation) in &resident {
                let cell = *cell;
                let points = generation.macro_points();
                for index in 0..points.ids.len() {
                    let Some(entry) = world_state.plants.get_mut(&(cell, points.ids[index])) else {
                        continue;
                    };
                    let family = points.families[index];
                    let Some(artifact) = families.get(&family.value()) else {
                        continue;
                    };
                    let Some(render) = assets.load_plant_family(gpu, family, *artifact) else {
                        continue;
                    };
                    let rendered_phenotype = saffron_vegetation::resolve_rendered_phenotype(
                        render
                            .phenotypes
                            .iter()
                            .map(|row| (row.id, row.role, row.season_window)),
                        points.phenotypes[index],
                        points.lifecycles[index],
                        season_mille,
                    );
                    let combination = render
                        .combinations
                        .iter()
                        .position(|candidate| {
                            *candidate == (points.variations[index], rendered_phenotype)
                        })
                        .or_else(|| {
                            render
                                .combinations
                                .iter()
                                .position(|candidate| candidate.1 == rendered_phenotype)
                        })
                        .unwrap_or(0) as u32;
                    if entry.record.combination == combination {
                        continue;
                    }
                    let mut record = entry.record.clone();
                    if let Some(vegetation) = record.vegetation.as_mut() {
                        vegetation.combination_previous = entry.record.combination;
                        vegetation.flip_stamp = flip_stamp;
                    }
                    record.combination = combination;
                    target
                        .gpu_scene
                        .apply_world_delta(
                            world,
                            GpuSceneWorldDelta::UpdateInstance {
                                handle: entry.handle,
                                record: record.clone(),
                            },
                        )
                        .map_err(gpu_scene_error)?;
                    entry.record = record;
                    mutated = true;
                }
            }
            world_state.season_mille = season_mille;
        }
        if mutated {
            reconcile_field_instances(shared, world_state, world, &families, assets, gpu, target)?;
            rebuild_field_directory(world_state, target)?;
        }
        Ok(mutated)
    }

    /// Per-family and per-cell vegetation population across every synced world:
    /// `(family, plant instances, field tiles, predicted micro candidates)` rows and
    /// `(cell, plants, field tiles)` rows, both in stable sorted order.
    #[must_use]
    pub fn vegetation_breakdown(&self) -> VegetationRenderBreakdown {
        use std::collections::BTreeMap;
        let mut families: BTreeMap<u64, (u32, u32, u64)> = BTreeMap::new();
        let mut cells: BTreeMap<WorldCellKey, (u32, u32)> = BTreeMap::new();
        for world in self.worlds.values() {
            for ((cell, _), entry) in &world.plants {
                let family = families.entry(entry.mesh).or_default();
                family.0 += 1;
                cells.entry(*cell).or_default().0 += 1;
            }
            for (cell, entry) in &world.plant_fields {
                for (family, _, predicted) in &entry.tiles {
                    let row = families.entry(*family).or_default();
                    row.1 += 1;
                    row.2 += u64::from(*predicted);
                }
                cells.entry(*cell).or_default().1 += entry.tiles.len() as u32;
            }
        }
        VegetationRenderBreakdown {
            families: families
                .into_iter()
                .map(
                    |(family, (instances, tiles, predicted))| VegetationFamilyRenderRow {
                        family,
                        instances,
                        field_tiles: tiles,
                        micro_predicted: predicted,
                    },
                )
                .collect(),
            cells: cells
                .into_iter()
                .map(|(cell, (plants, tiles))| VegetationCellRenderRow {
                    cell,
                    plants,
                    field_tiles: tiles,
                })
                .collect(),
        }
    }

    fn consume_asset_journal(
        &mut self,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(cursor) = self.asset_cursor else {
            self.asset_cursor = Some(assets.asset_journal_cursor());
            return Ok(());
        };
        let mut rebuild = false;
        let mut refresh_materials = false;
        let mut invalidated_meshes: HashSet<u64> = HashSet::new();
        let mut dropped_meshes: HashSet<u64> = HashSet::new();
        match assets.read_asset_journal(cursor) {
            AssetJournalRead::Delta { mutations, next } => {
                self.asset_cursor = Some(next);
                for mutation in mutations {
                    match mutation.target {
                        AssetMutationTarget::All => rebuild = true,
                        AssetMutationTarget::Asset { id, .. } => {
                            if mutation
                                .invalidations
                                .contains(AssetInvalidations::MATERIAL)
                                || mutation.invalidations.contains(AssetInvalidations::TEXTURE)
                            {
                                refresh_materials = true;
                            }
                            if mutation
                                .invalidations
                                .contains(AssetInvalidations::PROTOTYPE)
                                || mutation.invalidations.contains(AssetInvalidations::PAGE)
                            {
                                if mutation.kind == AssetMutationKind::Deleted {
                                    dropped_meshes.insert(id.value());
                                } else {
                                    invalidated_meshes.insert(id.value());
                                }
                            }
                        }
                    }
                }
            }
            AssetJournalRead::SnapshotRequired { next } => {
                self.asset_cursor = Some(next);
                rebuild = true;
            }
        }

        if rebuild {
            self.rebuild_shared(assets, target)?;
            return Ok(());
        }
        if refresh_materials {
            self.refresh_all_materials(assets, gpu, target)?;
        }
        for id in dropped_meshes {
            self.drop_mesh(id, target)?;
        }
        for id in invalidated_meshes {
            self.refresh_mesh(id, assets, gpu, target)?;
        }
        Ok(())
    }

    /// Tears down every mirrored record and forces each world to rebuild from live state
    /// at its next sync.
    fn rebuild_shared(
        &mut self,
        assets: &mut AssetServer,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for (id, world_state) in &mut self.worlds {
            let world = GpuSceneWorldId(*id);
            for entry in world_state.instances.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.plants.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.plant_fields.values() {
                target.gpu_data.fields.retire(entry.range)?;
            }
            if let Some((range, _)) = world_state.field_directory.take() {
                target.gpu_data.fields.retire(range)?;
            }
            for handle in world_state.field_instances.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(*handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.lights.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            *world_state = WorldMirror::default();
        }
        let meshes: Vec<u64> = self.shared.meshes.keys().copied().collect();
        for id in meshes {
            self.shared.remove_mesh_records(id, target)?;
        }
        let materials: Vec<MaterialKey> = self.shared.materials.keys().cloned().collect();
        for key in materials {
            self.shared.remove_material_records(&key, target)?;
        }
        debug_assert!(self.shared.textures.is_empty());
        self.shared.generation = self.shared.generation.wrapping_add(1);
        self.asset_cursor = Some(assets.asset_journal_cursor());
        self.shared_rebuilds += 1;
        Ok(())
    }

    /// Re-resolves every interned material variant in place; prototype and instance
    /// references stay valid because the persistent-scene handles are stable.
    fn refresh_all_materials(
        &mut self,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let keys: Vec<MaterialKey> = self.shared.materials.keys().cloned().collect();
        for key in keys {
            self.shared
                .refresh_material_content(&key, assets, gpu, target)?;
        }
        Ok(())
    }

    /// Removes every instance of a deleted mesh, then the mesh's shared records; the
    /// affected entities become unresolved and re-resolve if the asset returns.
    fn drop_mesh(&mut self, id: u64, target: &mut GpuSceneMirrorTarget<'_>) -> Result<()> {
        if !self.shared.meshes.contains_key(&id) {
            return Ok(());
        }
        for (world_id, world_state) in &mut self.worlds {
            let world = GpuSceneWorldId(*world_id);
            let affected: Vec<(Entity, InstanceSource)> = world_state
                .instances
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|(key, _)| *key)
                .collect();
            for key in affected {
                remove_instance(&mut self.shared, world_state, world, key, target)?;
                world_state.unresolved.insert(key, id);
            }
            let affected_cells: HashSet<WorldCellKey> = world_state
                .plants
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|((cell, _), _)| *cell)
                .collect();
            for cell in affected_cells {
                remove_cell_plants(&mut self.shared, world_state, world, cell, target)?;
                // Dropping the generation marker re-translates the cell at the next
                // vegetation sync, so its plants recreate if the family returns.
                world_state.plant_cells.remove(&cell);
            }
        }
        self.shared.remove_mesh_records(id, target)
    }

    /// Reloads an invalidated mesh and swaps its geometry, page, and prototype content in
    /// place; entities using it re-resolve at their world's next sync.
    fn refresh_mesh(
        &mut self,
        id: u64,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for world_state in self.worlds.values_mut() {
            for ((entity, _), mesh_id) in &world_state.unresolved {
                if *mesh_id == id {
                    world_state.dirty.insert(*entity);
                }
            }
            for ((entity, _), entry) in &world_state.instances {
                if entry.mesh == id {
                    world_state.dirty.insert(*entity);
                }
            }
            let refreshed_cells: Vec<WorldCellKey> = world_state
                .plants
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|((cell, _), _)| *cell)
                .collect();
            for cell in refreshed_cells {
                world_state.plant_cells.remove(&cell);
            }
        }
        if !self.shared.meshes.contains_key(&id) {
            return Ok(());
        }
        let reloaded = assets
            .load_mesh_asset(gpu, Uuid(id))
            .filter(|mesh| !mesh.hierarchy_pages.is_empty());
        match reloaded {
            Some(mesh) => {
                let payload_source = assets.page_payload_source(Uuid(id));
                self.shared
                    .replace_mesh_content(id, mesh, payload_source, target)
            }
            None => self.drop_mesh(id, target),
        }
    }

    /// Rebuilds one world from the live scene: teardown of every mirrored entry, then a
    /// complete re-resolve of every render-relevant entity.
    fn rebuild_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let world_state = self.worlds.get_mut(&world.0).expect("world synced");
        let existing: Vec<(Entity, InstanceSource)> =
            world_state.instances.keys().copied().collect();
        for key in existing {
            remove_instance(&mut self.shared, world_state, world, key, target)?;
        }
        for entry in world_state.lights.values() {
            target
                .gpu_scene
                .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                .map_err(gpu_scene_error)?;
        }
        world_state.lights.clear();
        world_state.unresolved.clear();
        world_state.dirty.clear();
        world_state.scene_instance = scene.instance_id();
        world_state.cursor = scene.journal_cursor();

        let mut targets: HashSet<Entity> = HashSet::new();
        scene.for_each::<&MeshComponent, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&SkinnedMesh, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&PointLight, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&SpotLight, _>(|entity, _| {
            targets.insert(entity);
        });

        let mut ctx = SyncCtx {
            scene,
            assets,
            gpu,
            target,
        };
        for entity in targets {
            resolve_entity(&mut self.shared, world_state, world, entity, &mut ctx)?;
        }
        self.world_rebuilds += 1;
        Ok(())
    }
}

/// Applies a transform-only update to the entity's mirrored instances and lights.
fn apply_transform_update(
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    scene: &Scene,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let Some(state) = scene.world_transform_state(entity) else {
        return Ok(());
    };
    for source in [InstanceSource::Static, InstanceSource::Skinned] {
        let Some(entry) = world_state.instances.get_mut(&(entity, source)) else {
            continue;
        };
        if entry.world_revision == state.current_revision
            && entry.previous_world_revision == state.previous_revision
        {
            continue;
        }
        let transform = match GpuSceneDynamicTransform::new(state.current, state.previous) {
            Ok(transform) => transform,
            Err(error) => {
                tracing::warn!(
                    "gpu scene mirror: non-finite world transform for entity {entity:?}: {error}"
                );
                continue;
            }
        };
        let Some(record) = target.gpu_scene.instance(world, entry.handle).cloned() else {
            continue;
        };
        let mut record = record;
        record.transform = GpuSceneTransform::Dynamic(transform);
        target
            .gpu_scene
            .apply_world_delta(
                world,
                GpuSceneWorldDelta::UpdateInstance {
                    handle: entry.handle,
                    record,
                },
            )
            .map_err(gpu_scene_error)?;
        entry.world_revision = state.current_revision;
        entry.previous_world_revision = state.previous_revision;
    }
    for kind in [MirrorLightKind::Point, MirrorLightKind::Spot] {
        let Some(entry) = world_state.lights.get_mut(&(entity, kind)) else {
            continue;
        };
        let Some(record) = light_record(scene, entity, kind) else {
            continue;
        };
        if record != entry.record {
            target
                .gpu_scene
                .apply_world_delta(
                    world,
                    GpuSceneWorldDelta::UpdateLight {
                        handle: entry.handle,
                        record,
                    },
                )
                .map_err(gpu_scene_error)?;
            entry.record = record;
        }
    }
    Ok(())
}

/// Builds the current punctual-light record for `entity`, or `None` when the component
/// or a finite world transform is absent.
fn light_record(
    scene: &Scene,
    entity: Entity,
    kind: MirrorLightKind,
) -> Option<GpuSceneLightRecord> {
    let state = scene.world_transform_state(entity)?;
    let position = state.current.w_axis.truncate();
    let light = match kind {
        MirrorLightKind::Point => {
            let light = scene.component::<PointLight>(entity).ok()?;
            gpu_point_light(&light, position)
        }
        MirrorLightKind::Spot => {
            let light = scene.component::<SpotLight>(entity).ok()?;
            let direction = (scene.world_rotation(entity) * light.direction).normalize();
            gpu_spot_light(&light, position, direction)
        }
    };
    Some(GpuSceneLightRecord {
        light,
        source_revision: state.current_revision.get(),
    })
}

/// Re-resolves one entity's complete mirrored state (instances of both sources plus both
/// punctual-light kinds) against the live scene.
fn resolve_entity(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    ctx: &mut SyncCtx<'_, '_>,
) -> Result<()> {
    let alive = ctx.scene.valid(entity);
    for source in [InstanceSource::Static, InstanceSource::Skinned] {
        resolve_instance(shared, world_state, world, entity, source, alive, ctx)?;
    }
    for kind in [MirrorLightKind::Point, MirrorLightKind::Spot] {
        let record = if alive {
            light_record(ctx.scene, entity, kind)
        } else {
            None
        };
        let key = (entity, kind);
        match (world_state.lights.get_mut(&key), record) {
            (None, None) => {}
            (Some(entry), None) => {
                ctx.target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                    .map_err(gpu_scene_error)?;
                world_state.lights.remove(&key);
            }
            (None, Some(record)) => {
                let result = ctx
                    .target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::CreateLight(record))
                    .map_err(gpu_scene_error)?;
                let GpuSceneWorldDeltaResult::LightCreated(handle) = result else {
                    unreachable!("create light returns LightCreated");
                };
                world_state
                    .lights
                    .insert(key, LightEntry { handle, record });
            }
            (Some(entry), Some(record)) => {
                if record != entry.record {
                    ctx.target
                        .gpu_scene
                        .apply_world_delta(
                            world,
                            GpuSceneWorldDelta::UpdateLight {
                                handle: entry.handle,
                                record,
                            },
                        )
                        .map_err(gpu_scene_error)?;
                    entry.record = record;
                }
            }
        }
    }
    Ok(())
}

/// Allocates a skinned instance's stable deformation state: deformed + prev vertex
/// ranges sized to the mesh, five provider parameter words, the provider record, and
/// the scene deformation record.
fn allocate_instance_deformation(
    vertex_count: u32,
    generation: u32,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<DeformationEntry> {
    let (params_range, _) = target.gpu_data.deformation_parameters.allocate(5, 1)?;
    // Words 0-3 (deformed/prev first vertex, joint first/count) patch per frame with
    // the skinning plan's offsets; only the vertex count is load-time state.
    let params = vec![0, 0, 0, 0, vertex_count];
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::DeformationParameters {
            range: params_range,
            data: params,
        });
    let (provider_range, _) = target.gpu_data.deformation_providers.allocate(1, 1)?;
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::DeformationProviders {
            range: provider_range,
            data: vec![saffron_rendering::GpuDeformationProviderRecord {
                provider_mask: saffron_rendering::GPU_DEFORMATION_PROVIDER_SKINNING,
                first_parameter: params_range.first,
                parameter_count: 5,
                flags: 0,
            }],
        });
    let result = target
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateDeformation(
            saffron_rendering::GpuSceneDeformationRecord {
                provider: GpuHandle {
                    index: provider_range.first,
                    generation: 1,
                },
                source_revision: u64::from(generation),
            },
        ))
        .map_err(gpu_scene_error)?;
    let GpuSceneSharedDeltaResult::DeformationCreated(scene) = result else {
        unreachable!("create deformation returns DeformationCreated");
    };
    Ok(DeformationEntry {
        scene,
        provider_range,
        params_range,
    })
}

/// Retires a deformation allocation (reverse of [`allocate_instance_deformation`]).
fn retire_instance_deformation(
    entry: DeformationEntry,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    target
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::RemoveDeformation(entry.scene))
        .map_err(gpu_scene_error)?;
    target
        .gpu_data
        .deformation_providers
        .retire(entry.provider_range)?;
    target
        .gpu_data
        .deformation_parameters
        .retire(entry.params_range)?;
    Ok(())
}

fn resolve_instance(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    source: InstanceSource,
    alive: bool,
    ctx: &mut SyncCtx<'_, '_>,
) -> Result<()> {
    let key = (entity, source);
    let mesh_id = if alive {
        match source {
            InstanceSource::Static => ctx
                .scene
                .component::<MeshComponent>(entity)
                .ok()
                .map(|m| m.mesh.value()),
            InstanceSource::Skinned => ctx
                .scene
                .with_component::<SkinnedMesh, _>(entity, |skin| skin.mesh.value())
                .ok(),
        }
    } else {
        None
    };
    let state = ctx.scene.world_transform_state(entity);
    let desired = match (mesh_id, state) {
        (Some(mesh), Some(state)) => {
            match GpuSceneDynamicTransform::new(state.current, state.previous) {
                Ok(transform) => Some((mesh, state, transform)),
                Err(error) => {
                    tracing::warn!(
                        "gpu scene mirror: non-finite world transform for entity {entity:?}: {error}"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let Some((mesh_id, state, transform)) = desired else {
        world_state.unresolved.remove(&key);
        if world_state.instances.contains_key(&key) {
            remove_instance(shared, world_state, world, key, ctx.target)?;
        }
        return Ok(());
    };

    if !shared.ensure_mesh(mesh_id, ctx.assets, ctx.gpu, ctx.target)? {
        if world_state.instances.contains_key(&key) {
            remove_instance(shared, world_state, world, key, ctx.target)?;
        }
        world_state.unresolved.insert(key, mesh_id);
        return Ok(());
    }
    world_state.unresolved.remove(&key);

    let slot_count = shared.meshes[&mesh_id].slot_count;
    let slots: Vec<MaterialSlot> = ctx
        .scene
        .with_component::<MaterialSet, _>(entity, |set| set.slots.clone())
        .unwrap_or_default();
    let mut overrides: Vec<(u32, MaterialKey)> = Vec::new();
    for slot in 0..slot_count {
        let material_key = if slots.is_empty() {
            MaterialKey::default_key()
        } else {
            let index = (slot as usize).min(slots.len() - 1);
            MaterialKey::new(&slots[index])
        };
        if material_key.is_default() {
            continue;
        }
        overrides.push((slot, material_key));
    }
    let mut override_records = Vec::with_capacity(overrides.len());
    for (slot, material_key) in &overrides {
        let handle = shared.intern_material(material_key, ctx.assets, ctx.gpu, ctx.target)?;
        override_records.push(GpuSceneMaterialOverride {
            slot: *slot,
            material: handle,
        });
    }

    let prototype = shared.meshes[&mesh_id].prototype;
    // A skinned instance owns a stable deformation allocation, reused while its mesh
    // is unchanged and reallocated (old retired) on a mesh swap.
    let existing_deformation = world_state
        .instances
        .get(&key)
        .filter(|entry| entry.mesh == mesh_id)
        .and_then(|entry| entry.deformation);
    let deformation = if source == InstanceSource::Skinned && ctx.gpu.skinning_enabled() {
        match existing_deformation {
            Some(entry) => Some(entry),
            None => Some(allocate_instance_deformation(
                shared.meshes[&mesh_id].mesh.vertex_count,
                shared.generation,
                ctx.target,
            )?),
        }
    } else {
        None
    };
    // The entity's PlantVariant selects an assembly combination exactly like a
    // cooked point: exact (variation, phenotype) pair, else the phenotype alone,
    // else the first authored combination.
    let combination = ctx
        .scene
        .component::<PlantVariant>(entity)
        .ok()
        .and_then(|variant| {
            shared.meshes[&mesh_id]
                .mesh
                .assembly
                .as_ref()
                .map(|assembly| {
                    assembly
                        .combinations
                        .iter()
                        .position(|entry| *entry == (variant.variation, variant.phenotype))
                        .or_else(|| {
                            assembly
                                .combinations
                                .iter()
                                .position(|entry| entry.1 == variant.phenotype)
                        })
                        .unwrap_or(0) as u32
                })
        })
        .unwrap_or(0);
    let record = GpuSceneInstanceRecord {
        prototype,
        transform: GpuSceneTransform::Dynamic(transform),
        material_overrides: Arc::from(override_records),
        deformation: deformation.map(|entry| entry.scene),
        sdf: None,
        source_generation: shared.generation,
        flags: 0,
        combination,
        vegetation: None,
    };

    if let Some(entry) = world_state.instances.get(&key) {
        let unchanged = ctx
            .target
            .gpu_scene
            .instance(world, entry.handle)
            .is_some_and(|current| *current == record);
        if unchanged {
            let entry = world_state.instances.get_mut(&key).expect("entry present");
            entry.world_revision = state.current_revision;
            entry.previous_world_revision = state.previous_revision;
            return Ok(());
        }
        let handle = entry.handle;
        let old_mesh = entry.mesh;
        let old_overrides = entry.overrides.clone();
        for (_, material_key) in &overrides {
            shared.ref_material(material_key);
        }
        if old_mesh != mesh_id {
            shared.ref_mesh(mesh_id);
        }
        ctx.target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::UpdateInstance { handle, record })
            .map_err(gpu_scene_error)?;
        for (_, material_key) in &old_overrides {
            shared.unref_material(material_key, ctx.target)?;
        }
        if old_mesh != mesh_id {
            shared.unref_mesh(old_mesh);
        }
        let entry = world_state.instances.get_mut(&key).expect("entry present");
        let old_deformation = entry.deformation;
        entry.mesh = mesh_id;
        entry.overrides = overrides;
        entry.deformation = deformation;
        entry.world_revision = state.current_revision;
        entry.previous_world_revision = state.previous_revision;
        if let Some(old) = old_deformation
            && deformation.map(|entry| entry.scene) != Some(old.scene)
        {
            retire_instance_deformation(old, ctx.target)?;
        }
    } else {
        for (_, material_key) in &overrides {
            shared.ref_material(material_key);
        }
        shared.ref_mesh(mesh_id);
        let result = ctx
            .target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
            .map_err(gpu_scene_error)?;
        let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
            unreachable!("create instance returns InstanceCreated");
        };
        world_state.instances.insert(
            key,
            InstanceEntry {
                handle,
                mesh: mesh_id,
                overrides,
                deformation,
                world_revision: state.current_revision,
                previous_world_revision: state.previous_revision,
            },
        );
    }
    Ok(())
}

/// Retires one cell's packed micro field tiles from the fields arena.
fn remove_cell_fields(
    world_state: &mut WorldMirror,
    cell: WorldCellKey,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    if let Some(entry) = world_state.plant_fields.remove(&cell) {
        target.gpu_data.fields.retire(entry.range)?;
    }
    Ok(())
}

/// The packed field-tile blob plus each tile's family and byte offset within it.
type PackedFieldTiles = (Vec<u8>, Vec<(u64, u32, u32)>);

/// Packs one cell's micro field tiles for the fields arena: per tile, a
/// [`GpuFieldTileRecord`] header, the `u16` density samples (padded to four bytes),
/// then each attribute channel's 16-byte id + `i32` values.
/// One plant's conservative bounds as an instance-local pre-scale sphere: the world
/// AABB's containing sphere, its center carried into the point's local frame
/// (inverse-rotated, de-scaled) so the cull composes it exactly like a prototype
/// sphere.
fn plant_bounds_sphere(
    position: saffron_spatial::WorldPosition,
    orientation: saffron_vegetation::QuantizedOrientation,
    scale: [saffron_spatial::DecisionScalar; 3],
    bounds: saffron_spatial::WorldBounds,
) -> [f32; 4] {
    use saffron_geometry::glam::{DQuat, DVec3};
    let tick = 1.0 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
    let minimum = bounds.min_ticks().map(|value| value as f64 * tick);
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| value as f64 * tick);
    let center = DVec3::new(
        (minimum[0] + maximum[0]) * 0.5,
        (minimum[1] + maximum[1]) * 0.5,
        (minimum[2] + maximum[2]) * 0.5,
    );
    let extent = DVec3::new(
        maximum[0] - minimum[0],
        maximum[1] - minimum[1],
        maximum[2] - minimum[2],
    );
    let radius = extent.length() * 0.5;
    let quantized = orientation.bits();
    let rotation = DQuat::from_xyzw(
        f64::from(quantized[0]) / 32_767.0,
        f64::from(quantized[1]) / 32_767.0,
        f64::from(quantized[2]) / 32_767.0,
        f64::from(quantized[3]) / 32_767.0,
    )
    .normalize();
    let uniform_scale = scale
        .iter()
        .map(|value| f64::from(value.bits()) / 65_536.0)
        .fold(f64::MIN, f64::max)
        .max(1e-4);
    let local = rotation.inverse() * (center - position.world_meters()) / uniform_scale;
    [
        local.x as f32,
        local.y as f32,
        local.z as f32,
        (radius / uniform_scale) as f32,
    ]
}

fn pack_field_tiles(tiles: &[MicroFieldTile]) -> Result<PackedFieldTiles> {
    let mut packed = Vec::new();
    let mut offsets = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let offset = u32::try_from(packed.len())
            .map_err(|_| mirror_error("micro field tiles exceed the fields arena"))?;
        // The predicted budget: blades a fully visible frame would reconstruct,
        // summed from the same density → blade-count derivation the GPU uses.
        let predicted = tile
            .density
            .iter()
            .map(|density| (u32::from(*density) * 4 + 0xffff) >> 16)
            .sum::<u32>();
        offsets.push((tile.family.value(), offset, predicted));
        let sample_count = u32::try_from(tile.density.len())
            .map_err(|_| mirror_error("micro field tile density exceeds u32 samples"))?;
        let attribute_count = u32::try_from(tile.attributes.len())
            .map_err(|_| mirror_error("micro field tile channels exceed u32"))?;
        let seed = tile.reconstruction_seed.to_le_bytes();
        let header = GpuFieldTileRecord {
            cell: tile.cell.coordinates(),
            dims: tile.dimensions,
            sample_count,
            seed: [
                u32::from_le_bytes(seed[0..4].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[4..8].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[8..12].try_into().expect("4 bytes")),
                u32::from_le_bytes(seed[12..16].try_into().expect("4 bytes")),
            ],
            attribute_count,
            reserved: 0,
        };
        packed.extend_from_slice(bytemuck::bytes_of(&header));
        packed.extend_from_slice(bytemuck::cast_slice(&tile.density));
        if tile.density.len() % 2 != 0 {
            packed.extend_from_slice(&[0, 0]);
        }
        for (channel, values) in &tile.attributes {
            packed.extend_from_slice(&channel.to_le_bytes());
            packed.extend_from_slice(bytemuck::cast_slice(values));
        }
    }
    Ok((packed, offsets))
}

/// Reconciles the per-family field instances against the resident tiles: families that
/// gained tiles get one flagged identity instance; families with none left release it.
fn reconcile_field_instances(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    families: &HashMap<u64, ContentHash>,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let referenced: HashSet<u64> = world_state
        .plant_fields
        .values()
        .flat_map(|entry| entry.tiles.iter().map(|(family, _, _)| *family))
        .collect();
    let stale: Vec<u64> = world_state
        .field_instances
        .keys()
        .filter(|family| !referenced.contains(family))
        .copied()
        .collect();
    for family in stale {
        let handle = world_state
            .field_instances
            .remove(&family)
            .expect("field instance present");
        target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(handle))
            .map_err(gpu_scene_error)?;
        shared.unref_mesh(family);
    }
    for family in referenced {
        if world_state.field_instances.contains_key(&family) {
            continue;
        }
        let Some(artifact) = families.get(&family) else {
            tracing::warn!("gpu scene mirror: field family {family} is not in the manifest");
            continue;
        };
        if assets
            .load_plant_family(gpu, Uuid(family), *artifact)
            .is_none()
        {
            continue;
        }
        if !shared.ensure_mesh(family, assets, gpu, target)? {
            continue;
        }
        let record = GpuSceneInstanceRecord {
            prototype: shared.meshes[&family].prototype,
            transform: GpuSceneTransform::Static(GpuSceneStaticTransform::new(
                saffron_spatial::WorldPosition::origin(),
                saffron_vegetation::QuantizedOrientation::identity(),
                [saffron_spatial::DecisionScalar::from_bits(1 << 16); 3],
                0,
            )),
            material_overrides: Arc::from([]),
            deformation: None,
            sdf: None,
            source_generation: shared.generation,
            flags: GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD,
            vegetation: None,
            combination: 0,
        };
        shared.ref_mesh(family);
        let result = target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
            .map_err(gpu_scene_error)?;
        let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
            unreachable!("create instance returns InstanceCreated");
        };
        world_state.field_instances.insert(family, handle);
    }
    Ok(())
}

/// Rebuilds the packed resident-tile directory the micro pass dispatches over, in
/// cell order for a deterministic stream.
fn rebuild_field_directory(
    world_state: &mut WorldMirror,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    if let Some((range, _)) = world_state.field_directory.take() {
        target.gpu_data.fields.retire(range)?;
    }
    let mut cells: Vec<(&WorldCellKey, &CellFieldEntry)> =
        world_state.plant_fields.iter().collect();
    cells.sort_by_key(|(cell, _)| **cell);
    let mut entries = Vec::new();
    let mut predicted_total: u64 = 0;
    for (_, entry) in cells {
        for (family, offset, predicted) in &entry.tiles {
            let Some(instance) = world_state.field_instances.get(family) else {
                continue;
            };
            predicted_total += u64::from(*predicted);
            entries.push(GpuFieldDirectoryEntry {
                instance: instance.raw(),
                tile_offset: entry.range.first + offset,
                predicted: *predicted,
            });
        }
    }
    world_state.micro_predicted = predicted_total;
    if entries.is_empty() {
        return Ok(());
    }
    let bytes = bytemuck::cast_slice(&entries).to_vec();
    let byte_len = u32::try_from(bytes.len())
        .map_err(|_| mirror_error("micro field directory exceeds the fields arena"))?;
    let (range, _) = target.gpu_data.fields.allocate(byte_len, 16)?;
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Fields { range, data: bytes });
    let count = u32::try_from(entries.len())
        .map_err(|_| mirror_error("micro field directory exceeds u32 entries"))?;
    world_state.field_directory = Some((range, count));
    Ok(())
}

/// Removes every mirrored plant instance of one cell, releasing its shared references.
fn remove_cell_plants(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    cell: WorldCellKey,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let keys: Vec<(WorldCellKey, PlantId)> = world_state
        .plants
        .keys()
        .filter(|(owner, _)| *owner == cell)
        .copied()
        .collect();
    for key in keys {
        let entry = world_state
            .plants
            .remove(&key)
            .expect("plant entry present");
        target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
            .map_err(gpu_scene_error)?;
        for (_, material_key) in &entry.overrides {
            shared.unref_material(material_key, target)?;
        }
        shared.unref_mesh(entry.mesh);
    }
    Ok(())
}

fn remove_instance(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    key: (Entity, InstanceSource),
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let Some(entry) = world_state.instances.remove(&key) else {
        return Ok(());
    };
    target
        .gpu_scene
        .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
        .map_err(gpu_scene_error)?;
    if let Some(deformation) = entry.deformation {
        retire_instance_deformation(deformation, target)?;
    }
    for (_, material_key) in &entry.overrides {
        shared.unref_material(material_key, target)?;
    }
    shared.unref_mesh(entry.mesh);
    Ok(())
}

impl SharedMirror {
    fn next_revision(&mut self) -> u64 {
        self.content_revision += 1;
        self.content_revision
    }

    /// Makes the mesh's shared records resident, returning whether the mesh resolved.
    fn ensure_mesh(
        &mut self,
        id: u64,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<bool> {
        if self.meshes.contains_key(&id) {
            return Ok(true);
        }
        let Some(mesh) = assets.load_mesh_asset(gpu, Uuid(id)) else {
            return Ok(false);
        };
        if mesh.hierarchy_pages.is_empty() {
            tracing::warn!("gpu scene mirror: mesh {id} carries no hierarchy pages; skipped");
            return Ok(false);
        }
        let entry = self.build_mesh_entry(id, mesh, assets, gpu, target)?;
        self.meshes.insert(id, entry);
        Ok(true)
    }

    fn build_mesh_entry(
        &mut self,
        id: u64,
        mesh: Arc<GpuMesh>,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<MeshEntry> {
        let InsertedGeometry {
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
        } = insert_geometry(&mesh, target)?;
        let (pages, root_page) = insert_pages(&mesh.hierarchy_pages, self.generation, target)?;
        for (cook_id, page) in pages.iter().enumerate() {
            self.page_lookup
                .insert(page.device.index, (id, cook_id as u32));
        }
        let payload_source = assets.page_payload_source(Uuid(id));
        if payload_source.is_none() {
            tracing::warn!("gpu scene mirror: mesh {id} has no page payload source");
        }

        let slot_count = mesh
            .submeshes
            .iter()
            .map(|submesh| submesh.material_slot + 1)
            .max()
            .unwrap_or(1)
            .max(1);
        let default_key = MaterialKey::default_key();
        let default_handle = self.intern_material(&default_key, assets, gpu, target)?;
        for _ in 0..slot_count {
            self.ref_material(&default_key);
        }
        let materials: Arc<[GpuSceneMaterialHandle]> =
            std::iter::repeat_n(default_handle, slot_count as usize).collect();

        let bounds = prototype_bounds(&mesh);
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                GpuScenePrototypeRecord {
                    geometry,
                    materials,
                    deformation: None,
                    sdf: None,
                    root_page,
                    bounds,
                    source_generation: self.generation,
                    flags: 0,
                },
            ))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::PrototypeCreated(prototype) = result else {
            unreachable!("create prototype returns PrototypeCreated");
        };
        Ok(MeshEntry {
            mesh,
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
            pages,
            payload_source,
            prototype,
            slot_count,
            refs: 0,
        })
    }

    /// Swaps a refreshed mesh's geometry, pages, bounds, and slot table in place while
    /// keeping the prototype handle stable for every live instance.
    fn replace_mesh_content(
        &mut self,
        id: u64,
        mesh: Arc<GpuMesh>,
        payload_source: Option<PagePayloadSource>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let InsertedGeometry {
            geometry,
            vertex_range,
            index_range,
            submesh_range,
            parts_range,
        } = insert_geometry(&mesh, target)?;
        let (pages, root_page) = insert_pages(&mesh.hierarchy_pages, self.generation, target)?;
        for (cook_id, page) in pages.iter().enumerate() {
            self.page_lookup
                .insert(page.device.index, (id, cook_id as u32));
        }

        let new_slot_count = mesh
            .submeshes
            .iter()
            .map(|submesh| submesh.material_slot + 1)
            .max()
            .unwrap_or(1)
            .max(1);

        let entry = self.meshes.get(&id).expect("refreshed mesh entry present");
        let prototype = entry.prototype;
        let old_slot_count = entry.slot_count;
        let old_geometry = entry.geometry;
        let old_vertex_range = entry.vertex_range;
        let old_index_range = entry.index_range;
        let old_submesh_range = entry.submesh_range;
        let old_parts_range = entry.parts_range;
        let old_pages: Vec<MeshPage> = entry.pages.clone();

        let default_key = MaterialKey::default_key();
        let default_handle = self.materials[&default_key].scene_handle;
        let materials: Arc<[GpuSceneMaterialHandle]> =
            std::iter::repeat_n(default_handle, new_slot_count as usize).collect();
        let bounds = prototype_bounds(&mesh);

        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdatePrototype {
                handle: prototype,
                record: GpuScenePrototypeRecord {
                    geometry,
                    materials,
                    deformation: None,
                    sdf: None,
                    root_page,
                    bounds,
                    source_generation: self.generation,
                    flags: 0,
                },
            })
            .map_err(gpu_scene_error)?;

        for page in old_pages.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemovePage(page.scene))
                .map_err(gpu_scene_error)?;
            target
                .residency
                .unregister_page(page.device, target.gpu_data)?;
            self.page_lookup.remove(&page.device.index);
            target
                .pending
                .retire_record(GlobalGpuTableKind::Page, page.device);
        }
        target
            .pending
            .retire_record(GlobalGpuTableKind::Geometry, old_geometry);
        target.gpu_data.vertices.retire(old_vertex_range)?;
        target.gpu_data.indices.retire(old_index_range)?;
        target.gpu_data.submesh_table.retire(old_submesh_range)?;
        if old_parts_range.count != 0 {
            target.gpu_data.parts.retire(old_parts_range)?;
        }

        if new_slot_count > old_slot_count {
            for _ in old_slot_count..new_slot_count {
                self.ref_material(&default_key);
            }
        } else {
            for _ in new_slot_count..old_slot_count {
                self.unref_material(&default_key, target)?;
            }
        }

        let entry = self
            .meshes
            .get_mut(&id)
            .expect("refreshed mesh entry present");
        entry.mesh = mesh;
        entry.geometry = geometry;
        entry.vertex_range = vertex_range;
        entry.index_range = index_range;
        entry.submesh_range = submesh_range;
        entry.parts_range = parts_range;
        entry.pages = pages;
        entry.payload_source = payload_source;
        entry.slot_count = new_slot_count;
        Ok(())
    }

    /// Removes a mesh's prototype, pages, and geometry records. Callers guarantee no
    /// instance references the prototype.
    fn remove_mesh_records(
        &mut self,
        id: u64,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(entry) = self.meshes.remove(&id) else {
            return Ok(());
        };
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::RemovePrototype(entry.prototype))
            .map_err(gpu_scene_error)?;
        for page in entry.pages.iter().rev() {
            target
                .gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::RemovePage(page.scene))
                .map_err(gpu_scene_error)?;
            target
                .residency
                .unregister_page(page.device, target.gpu_data)?;
            self.page_lookup.remove(&page.device.index);
            target
                .pending
                .retire_record(GlobalGpuTableKind::Page, page.device);
        }
        target
            .pending
            .retire_record(GlobalGpuTableKind::Geometry, entry.geometry);
        target.gpu_data.vertices.retire(entry.vertex_range)?;
        target.gpu_data.indices.retire(entry.index_range)?;
        target.gpu_data.submesh_table.retire(entry.submesh_range)?;
        if entry.parts_range.count != 0 {
            target.gpu_data.parts.retire(entry.parts_range)?;
        }
        let default_key = MaterialKey::default_key();
        for _ in 0..entry.slot_count {
            self.unref_material(&default_key, target)?;
        }
        Ok(())
    }

    fn ref_mesh(&mut self, id: u64) {
        if let Some(entry) = self.meshes.get_mut(&id) {
            entry.refs += 1;
        }
    }

    /// Releases one instance reference; the entry stays interned for reuse until the
    /// asset itself is deleted or the shared state rebuilds.
    fn unref_mesh(&mut self, id: u64) {
        if let Some(entry) = self.meshes.get_mut(&id) {
            entry.refs = entry.refs.saturating_sub(1);
        }
    }

    fn intern_material(
        &mut self,
        key: &MaterialKey,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<GpuSceneMaterialHandle> {
        if let Some(entry) = self.materials.get(key) {
            return Ok(entry.scene_handle);
        }
        let overrides = key.overrides_value();
        let asset = assets.resolve_slot_material(Uuid(key.material), &overrides);
        let submesh = assets.resolve_material_asset(gpu, &asset);
        let codegen_shader = assets.codegen_shader_for(Uuid(key.material));
        let device = self.build_material_device_records(
            key,
            &asset,
            &submesh,
            codegen_shader.as_deref(),
            target,
        )?;
        let source_revision = self.next_revision();
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: device.table,
                    source_revision,
                },
            ))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::MaterialCreated(scene_handle) = result else {
            unreachable!("create material returns MaterialCreated");
        };
        self.materials.insert(
            key.clone(),
            MaterialEntry {
                device,
                scene_handle,
                refs: 0,
            },
        );
        Ok(scene_handle)
    }

    /// Re-resolves one interned material's content behind its stable persistent-scene
    /// handle: new device records replace the old ones, which retire.
    fn refresh_material_content(
        &mut self,
        key: &MaterialKey,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        if !self.materials.contains_key(key) {
            return Ok(());
        }
        let overrides = key.overrides_value();
        let asset = assets.resolve_slot_material(Uuid(key.material), &overrides);
        let submesh = assets.resolve_material_asset(gpu, &asset);
        let codegen_shader = assets.codegen_shader_for(Uuid(key.material));
        let device = self.build_material_device_records(
            key,
            &asset,
            &submesh,
            codegen_shader.as_deref(),
            target,
        )?;
        let source_revision = self.next_revision();

        let entry = self.materials.get_mut(key).expect("entry present");
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::UpdateMaterial {
                handle: entry.scene_handle,
                record: GpuSceneMaterialRecord {
                    table: device.table,
                    source_revision,
                },
            })
            .map_err(gpu_scene_error)?;
        let old = std::mem::replace(&mut entry.device, device);
        self.retire_material_device_records(old, target)
    }

    fn build_material_device_records(
        &mut self,
        key: &MaterialKey,
        asset: &MaterialAsset,
        submesh: &SubmeshMaterial,
        codegen_shader: Option<&str>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<MaterialDeviceRecords> {
        let mut textures = Vec::new();
        let albedo_arc = submesh
            .albedo_texture
            .clone()
            .unwrap_or_else(|| Arc::clone(target.default_white));
        let albedo = self.intern_texture(&albedo_arc, target)?;
        textures.push(albedo_arc.bindless_index());
        let normal = match submesh.normal_texture.as_ref() {
            Some(texture) => {
                let handle = self.intern_texture(texture, target)?;
                textures.push(texture.bindless_index());
                handle
            }
            None => GpuHandle::INVALID,
        };
        let coverage_texture = match submesh.coverage_texture.as_ref() {
            Some(texture) => {
                let handle = self.intern_texture(texture, target)?;
                textures.push(texture.bindless_index());
                Some((handle, texture.extent.width, texture.extent.height))
            }
            None => None,
        };

        let coverage_handle = coverage_texture.map_or(albedo, |(handle, ..)| handle);
        let coverage_extent = coverage_texture.map_or_else(
            || {
                [
                    albedo_arc.extent.width.max(1),
                    albedo_arc.extent.height.max(1),
                ]
            },
            |(_, width, height)| [width.max(1), height.max(1)],
        );
        let coverage =
            build_coverage_record(key, asset, submesh, coverage_handle, coverage_extent)?;
        let coverage = match coverage {
            Some(record) => {
                let handle = target.gpu_data.coverage.insert(record)?;
                target
                    .pending
                    .stage_record(GlobalGpuTableKind::Coverage, handle);
                Some(handle)
            }
            None => None,
        };

        let (parameters, _) = target.gpu_data.material_parameters.allocate(1, 1)?;
        // The temporal coverage phase is per-frame state, not table content; the packed
        // block carries phase zero.
        let mut pinned = Vec::new();
        let (params, ..) = resolve_material_params(submesh, DEFAULT_WHITE_SLOT, 0, &mut pinned);
        target
            .pending
            .upload_arena(GpuArenaUploadRequest::MaterialParams {
                range: parameters,
                data: Box::new(params),
            });
        let table = target.gpu_data.materials.insert(GpuMaterialTableRecord {
            base_color_texture: albedo,
            normal_texture: normal,
            coverage: coverage.unwrap_or(GpuHandle::INVALID),
            parameter_index: parameters.first,
            material_class: material_class(submesh, asset.unlit),
            shader_index: codegen_shader.map_or(0, |shader| {
                target.gpu_data.executor_shaders.register(shader)
            }),
            // A displacing material's records are skipped by the traversal while the
            // tessellation seam draws the amplified geometry.
            flags: if submesh.height_texture.is_some()
                && submesh.height_mode == saffron_core::HeightMode::Displacement
            {
                saffron_rendering::GPU_MATERIAL_TABLE_FLAG_TESSELLATED
            } else {
                0
            },
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Material, table);
        Ok(MaterialDeviceRecords {
            table,
            coverage,
            parameters,
            textures,
        })
    }

    fn retire_material_device_records(
        &mut self,
        records: MaterialDeviceRecords,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        target
            .pending
            .retire_record(GlobalGpuTableKind::Material, records.table);
        if let Some(coverage) = records.coverage {
            target
                .pending
                .retire_record(GlobalGpuTableKind::Coverage, coverage);
        }
        target
            .gpu_data
            .material_parameters
            .retire(records.parameters)?;
        for index in records.textures {
            self.unref_texture(index, target);
        }
        Ok(())
    }

    fn ref_material(&mut self, key: &MaterialKey) {
        if let Some(entry) = self.materials.get_mut(key) {
            entry.refs += 1;
        }
    }

    fn unref_material(
        &mut self,
        key: &MaterialKey,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let remove = {
            let Some(entry) = self.materials.get_mut(key) else {
                return Ok(());
            };
            entry.refs = entry.refs.saturating_sub(1);
            entry.refs == 0
        };
        if remove {
            self.remove_material_records(key, target)?;
        }
        Ok(())
    }

    fn remove_material_records(
        &mut self,
        key: &MaterialKey,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(entry) = self.materials.remove(key) else {
            return Ok(());
        };
        target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::RemoveMaterial(entry.scene_handle))
            .map_err(gpu_scene_error)?;
        self.retire_material_device_records(entry.device, target)
    }

    fn intern_texture(
        &mut self,
        texture: &Arc<GpuTexture>,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<GpuHandle> {
        let index = texture.bindless_index();
        if let Some(entry) = self.textures.get_mut(&index) {
            entry.refs += 1;
            return Ok(entry.handle);
        }
        let handle = target.gpu_data.textures.insert(GpuTextureTableRecord {
            descriptor_index: index,
            width: texture.extent.width,
            height: texture.extent.height,
            mip_count: texture.mip_count,
            flags: 0,
            reserved: [0; 3],
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Texture, handle);
        self.textures.insert(
            index,
            TextureEntry {
                handle,
                texture: Arc::clone(texture),
                refs: 1,
            },
        );
        Ok(handle)
    }

    fn unref_texture(&mut self, index: u32, target: &mut GpuSceneMirrorTarget<'_>) {
        let remove = {
            let Some(entry) = self.textures.get_mut(&index) else {
                return;
            };
            entry.refs = entry.refs.saturating_sub(1);
            entry.refs == 0
        };
        if remove && let Some(entry) = self.textures.remove(&index) {
            target
                .pending
                .retire_record(GlobalGpuTableKind::Texture, entry.handle);
            drop(entry.texture);
        }
    }
}

fn prototype_bounds(mesh: &GpuMesh) -> [f32; 4] {
    let center = (mesh.bounds_min + mesh.bounds_max) * 0.5;
    let radius = (mesh.bounds_max - center).length();
    [center.x, center.y, center.z, radius.max(0.0)]
}

/// The device ranges one mesh's geometry occupies in the global arenas.
struct InsertedGeometry {
    geometry: GpuHandle,
    vertex_range: GpuArenaRange,
    index_range: GpuArenaRange,
    submesh_range: GpuArenaRange,
    parts_range: GpuArenaRange,
}

fn insert_geometry(
    mesh: &GpuMesh,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<InsertedGeometry> {
    let vertex_stride = size_of::<Vertex>() as u32;
    let vertex_bytes = mesh
        .vertex_count
        .checked_mul(vertex_stride)
        .ok_or_else(|| mirror_error("mesh vertex stream exceeds the global vertex arena"))?;
    let index_bytes = mesh
        .index_count
        .checked_mul(4)
        .ok_or_else(|| mirror_error("mesh index stream exceeds the global index arena"))?;
    let (vertex_range, _) = target.gpu_data.vertices.allocate(vertex_bytes, 16)?;
    let (index_range, _) = target.gpu_data.indices.allocate(index_bytes, 4)?;
    let submesh_records: Vec<GpuSubmeshRecord> = if mesh.submeshes.is_empty() {
        vec![GpuSubmeshRecord {
            first_index: 0,
            index_count: mesh.index_count,
            material_slot: 0,
            reserved: 0,
        }]
    } else {
        mesh.submeshes
            .iter()
            .map(|submesh| GpuSubmeshRecord {
                first_index: submesh.first_index,
                index_count: submesh.index_count,
                material_slot: submesh.material_slot,
                reserved: 0,
            })
            .collect()
    };
    let submesh_count = u32::try_from(submesh_records.len())
        .map_err(|_| mirror_error("mesh submesh table exceeds the submesh arena"))?;
    let (submesh_range, _) = target.gpu_data.submesh_table.allocate(submesh_count, 1)?;
    // An assembly (a multi-prototype plant family) packs its prototype + use records
    // into the parts arena; the prototype count rides the geometry's reserved word so
    // the shaders can split the two tables.
    let (parts_range, prototype_count) = match &mesh.assembly {
        Some(assembly) => {
            let byte_len = u32::try_from(assembly.byte_len())
                .map_err(|_| mirror_error("mesh assembly table exceeds the parts arena"))?;
            let (range, _) = target.gpu_data.parts.allocate(byte_len, 16)?;
            let prototype_count = u32::try_from(assembly.prototypes.len())
                .map_err(|_| mirror_error("mesh assembly table exceeds the parts arena"))?;
            (range, prototype_count)
        }
        None => (EMPTY_RANGE, 0),
    };
    let geometry = target.gpu_data.geometries.insert(GpuGeometryRecord {
        vertices: vertex_range,
        indices: index_range,
        clusters: EMPTY_RANGE,
        parts: parts_range,
        voxels: EMPTY_RANGE,
        submeshes: submesh_range,
        flags: 0,
        vertex_stride,
        index_stride: 4,
        reserved: prototype_count,
    })?;
    if let Some(assembly) = &mesh.assembly {
        target.pending.upload_arena(GpuArenaUploadRequest::Parts {
            range: parts_range,
            data: assembly.packed_bytes(),
        });
    }
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Submeshes {
            range: submesh_range,
            data: submesh_records,
        });
    target
        .pending
        .stage_record(GlobalGpuTableKind::Geometry, geometry);
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::Vertices {
            range: vertex_range,
            data: Arc::clone(&mesh.cpu_vertices),
        });
    target.pending.upload_arena(GpuArenaUploadRequest::Indices {
        range: index_range,
        data: Arc::clone(&mesh.cpu_indices),
    });
    Ok(InsertedGeometry {
        geometry,
        vertex_range,
        index_range,
        submesh_range,
        parts_range,
    })
}

fn insert_pages(
    hierarchy_pages: &[PortableHierarchyPage],
    generation: u32,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<(Vec<MeshPage>, GpuScenePageHandle)> {
    let mut ordered: Vec<&PortableHierarchyPage> = hierarchy_pages.iter().collect();
    ordered.sort_by_key(|page| page.id);
    let mut by_id: HashMap<u32, MeshPage> = HashMap::new();
    let mut pages = Vec::with_capacity(ordered.len());
    let mut root_page = None;
    for page in ordered {
        let parent = match page.dependency {
            Some(dependency) => Some(*by_id.get(&dependency).ok_or_else(|| {
                mirror_error(format!(
                    "hierarchy page {} depends on unseen page {dependency}",
                    page.id
                ))
            })?),
            None => None,
        };
        let flags = if page.guaranteed_root {
            GPU_PAGE_FLAG_GUARANTEED_ROOT
        } else {
            0
        };
        let device = target.gpu_data.page_table.insert(GpuPageRecord {
            parent: parent.map_or(GpuHandle::INVALID, |p| p.device),
            dependencies: EMPTY_RANGE,
            byte_offset: 0,
            byte_length: 0,
            resident_generation: generation,
            flags,
            reserved: 0,
        })?;
        target
            .pending
            .stage_record(GlobalGpuTableKind::Page, device);
        target
            .residency
            .register_page(device, parent.map(|p| p.device), page.guaranteed_root);
        let result = target
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device,
                parent: parent.map(|p| p.scene),
                source_generation: generation,
                flags,
            }))
            .map_err(gpu_scene_error)?;
        let GpuSceneSharedDeltaResult::PageCreated(scene_handle) = result else {
            unreachable!("create page returns PageCreated");
        };
        let entry = MeshPage {
            device,
            scene: scene_handle,
        };
        if root_page.is_none() && page.guaranteed_root {
            root_page = Some(scene_handle);
        }
        by_id.insert(page.id, entry);
        pages.push(entry);
    }
    let root_page = root_page
        .or_else(|| pages.first().map(|page| page.scene))
        .ok_or_else(|| mirror_error("hierarchy has no pages"))?;
    Ok((pages, root_page))
}

/// Builds the canonical coverage record for a material, or `None` for a surface the
/// classifier never samples (standard opaque and standard alpha-blended surfaces).
fn build_coverage_record(
    key: &MaterialKey,
    asset: &MaterialAsset,
    submesh: &SubmeshMaterial,
    texture: GpuHandle,
    extent: [u32; 2],
) -> Result<Option<GpuCoverageRecord>> {
    if let MaterialSurface::ThinSheetFoliage(params) = &asset.surface {
        return Ok(Some(GpuCoverageRecord::from_metadata(
            texture,
            &params.coverage_source,
            &params.coverage,
            params.opacity_micromap,
        )));
    }
    if submesh.blend_mode != BlendMode::Masked {
        return Ok(None);
    }
    let cutoff = f64::from(submesh.alpha_cutoff.clamp(0.0, 1.0));
    let metadata = CoverageMipMetadata {
        reference_cutoff: UnitInterval::from_f64(cutoff)
            .map_err(|error| mirror_error(format!("alpha cutoff out of range: {error}")))?,
        source_extent: extent,
        spatial_hash_salt: coverage_salt(key),
        classification: AlphaClassification::Masked,
        mip_hashes: Vec::new(),
    };
    Ok(Some(GpuCoverageRecord::from_metadata(
        texture,
        &CoverageSource::AlbedoAlpha,
        &metadata,
        OpacityMicromapDerivation::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use saffron_rendering::{
        BindlessFreeList, Descriptors, Device, GpuSceneUploadLimits, SurfaceSource,
        validation_issue_count,
    };
    use saffron_scene::{AssetEntry, AssetType, Transform};

    const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

    struct GpuFixture {
        uploader: Uploader,
        descriptors: Descriptors,
        device: Device,
    }

    fn gpu_or_skip() -> Option<GpuFixture> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        Some(GpuFixture {
            uploader,
            descriptors,
            device,
        })
    }

    impl GpuFixture {
        fn teardown(
            self,
            mirror: GpuSceneMirror,
            gpu_scene: PersistentGpuScene,
            gpu_data: GlobalGpuData,
            mut assets: AssetServer,
        ) {
            let GpuFixture {
                device,
                descriptors,
                uploader,
            } = self;
            device.wait_idle().expect("idle before teardown");
            drop(mirror);
            drop(gpu_scene);
            drop(gpu_data);
            assets.clear_asset_caches();
            drop(assets);
            drop(uploader);
            drop(descriptors);
            drop(device);
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "saffron-gpu-scene-mirror-{tag}-{}",
            Uuid::new().value()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes a standalone `.smesh` of one forward-facing triangle and registers a Mesh
    /// catalog row for `id`.
    fn write_triangle_mesh(assets: &mut AssetServer, id: Uuid, name: &str) {
        use saffron_geometry::glam::Vec2;
        use saffron_geometry::{Mesh, Submesh, Vertex, save_mesh_to_buffer};
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let rel = format!("models/{name}.smesh");
        let full = format!("{}/{rel}", assets.root.display());
        std::fs::create_dir_all(format!("{}/models", assets.root.display())).unwrap();
        std::fs::write(&full, save_mesh_to_buffer(&mesh, &[], None).unwrap()).unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: rel,
            chunk: -1,
            ..AssetEntry::default()
        });
    }

    /// Field order is the drop order: every GPU-resource holder precedes `fixture`, so an
    /// assertion unwind tears down buffers and textures before the device.
    struct MirrorHarness {
        mirror: GpuSceneMirror,
        gpu_scene: PersistentGpuScene,
        gpu_data: GlobalGpuData,
        pending: GpuScenePendingUploads,
        residency: PageResidency,
        default_white: Arc<GpuTexture>,
        assets: AssetServer,
        fixture: GpuFixture,
    }

    fn harness(tag: &str) -> Option<MirrorHarness> {
        let fixture = gpu_or_skip()?;
        let assets = AssetServer::new(scratch(tag));
        let gpu_data = GlobalGpuData::new(&fixture.device).expect("GlobalGpuData::new");
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene mirror");
        gpu_scene.create_world(WORLD).expect("create world");
        let default_white = fixture
            .uploader
            .upload_default_white(&fixture.descriptors)
            .expect("default white");
        Some(MirrorHarness {
            fixture,
            assets,
            gpu_data,
            gpu_scene,
            pending: GpuScenePendingUploads::default(),
            residency: PageResidency::default(),
            default_white,
            mirror: GpuSceneMirror::new(),
        })
    }

    impl MirrorHarness {
        fn sync(&mut self, scene: &mut Scene) {
            let gpu =
                RendererUploader::new(&self.fixture.uploader, &self.fixture.descriptors, false);
            let mut target = GpuSceneMirrorTarget {
                gpu_data: &mut self.gpu_data,
                gpu_scene: &mut self.gpu_scene,
                default_white: &self.default_white,
                pending: &mut self.pending,
                residency: &mut self.residency,
            };
            self.mirror
                .sync_world(WORLD, scene, &mut self.assets, &gpu, &mut target)
                .expect("mirror sync");
        }

        fn drive(&mut self, scene: &Scene) {
            let mut target = GpuSceneMirrorTarget {
                gpu_data: &mut self.gpu_data,
                gpu_scene: &mut self.gpu_scene,
                default_white: &self.default_white,
                pending: &mut self.pending,
                residency: &mut self.residency,
            };
            self.mirror
                .drive_page_streaming(WORLD, scene, None, &mut target)
                .expect("drive page streaming");
            self.residency
                .publish_ready(&mut self.gpu_data, &mut self.pending)
                .expect("publish ready pages");
        }

        fn finish(self) {
            let MirrorHarness {
                fixture,
                assets,
                gpu_data,
                gpu_scene,
                pending,
                residency,
                default_white,
                mirror,
            } = self;
            drop(pending);
            drop(residency);
            drop(default_white);
            fixture.teardown(mirror, gpu_scene, gpu_data, assets);
        }
    }

    /// A two-prototype assembly family: each single-triangle prototype placed once at
    /// identity, plus the flattened family mesh (prototype streams concatenated in id
    /// order).
    fn assembly_family_fixture() -> (
        saffron_geometry::Mesh,
        saffron_geometry::PortableVirtualHierarchy,
    ) {
        use saffron_geometry::glam::Vec2;
        use saffron_geometry::{
            Mesh, PortableAggregationMode, PortableHierarchyInput, PortableSourceMesh,
            PortableSourceSubmesh, PortableSourceVertex, Submesh, Vertex, VirtualHierarchyMaterial,
            cook_portable_virtual_hierarchy,
        };
        let quantized = |x: f32, y: f32| PortableSourceVertex {
            position_bits: [(x * 65_536.0) as i32, (y * 65_536.0) as i32, 0],
            normal_snorm: [0, 0, 32_767],
            uv_bits: [0, 0],
            tangent_snorm: [32_767, 0, 0, 32_767],
        };
        let source_mesh = |source: u128| PortableSourceMesh {
            source,
            selector_hash: [source as u8; 32],
            vertices: vec![
                quantized(-1.0, -1.0),
                quantized(1.0, -1.0),
                quantized(0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![PortableSourceSubmesh {
                first_index: 0,
                index_count: 3,
                material: VirtualHierarchyMaterial::opaque(0),
            }],
            skin: Vec::new(),
            aggregation: PortableAggregationMode::Contiguous,
        };
        const IDENTITY: [i32; 16] = [
            65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
        ];
        let bounds = saffron_geometry::PortableBounds {
            min_bits: [-2 * 65_536; 3],
            max_bits: [2 * 65_536; 3],
        };
        let input = PortableHierarchyInput {
            combinations: Vec::new(),
            meshes: vec![source_mesh(1), source_mesh(2)],
            micro_instances: vec![
                saffron_geometry::MicroInstance {
                    part: 1,
                    prototype: 0,
                    transform_bits: IDENTITY,
                },
                saffron_geometry::MicroInstance {
                    part: 2,
                    prototype: 1,
                    transform_bits: IDENTITY,
                },
            ],
            deformation: Vec::new(),
            bounds,
            root_material: VirtualHierarchyMaterial::opaque(0),
            deformation_padding: 0,
        };
        let hierarchy = cook_portable_virtual_hierarchy(&input).expect("cook family hierarchy");

        let flat_vertex = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::Z,
            uv0: Vec2::ZERO,
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let flat = Mesh {
            vertices: vec![
                flat_vertex(-1.0, -1.0),
                flat_vertex(1.0, -1.0),
                flat_vertex(0.0, 1.0),
                flat_vertex(-1.0, -1.0),
                flat_vertex(1.0, -1.0),
                flat_vertex(0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            submeshes: vec![
                Submesh {
                    first_index: 0,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 0,
                },
                Submesh {
                    first_index: 3,
                    index_count: 3,
                    vertex_offset: 0,
                    material_slot: 0,
                },
            ],
        };
        (flat, hierarchy)
    }

    /// A registered assembly mesh (two prototypes placed by identity uses) mirrors into
    /// a geometry record whose parts range holds the packed prototype + use tables and
    /// whose reserved word carries the prototype count — the executor's table split.
    #[test]
    fn assembly_mesh_mirrors_its_parts_range_and_prototype_count() {
        let Some(mut harness) = harness("assembly") else {
            return;
        };
        let before = validation_issue_count();
        let (flat, hierarchy) = assembly_family_fixture();
        let gpu = RendererUploader::new(
            &harness.fixture.uploader,
            &harness.fixture.descriptors,
            false,
        );
        let uploaded = gpu
            .upload_mesh(&flat, &hierarchy, &[], None, None)
            .expect("assembly upload");
        let assembly = uploaded.assembly.as_ref().expect("assembly table");
        let expected_bytes = u32::try_from(assembly.byte_len()).unwrap();

        // Register the family the way the plant loader does, then mirror an instance.
        let family_id = Uuid(9_777);
        harness.assets.register_family_render(
            family_id,
            Arc::clone(&uploaded),
            Arc::new(hierarchy),
        );
        let mut scene = Scene::new();
        let entity = scene.create_entity("Family");
        scene
            .add_component(entity, MeshComponent { mesh: family_id })
            .unwrap();
        harness.sync(&mut scene);

        let entry = &harness.mirror.shared.meshes[&family_id.value()];
        assert_eq!(entry.parts_range.count, expected_bytes);
        let record = harness
            .gpu_data
            .geometries
            .get(entry.geometry)
            .expect("geometry record");
        assert_eq!(
            record.reserved, 2,
            "prototype count rides the reserved word"
        );
        assert_eq!(record.parts, entry.parts_range);

        drop(uploaded);
        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    /// The macro snapshot adapter: a resident vegetation cell's plants become
    /// persistent-scene instances keyed `(cell, PlantId)`, diffed purely by published
    /// generation — an unchanged cell re-syncs to the identical instance, and a
    /// republished cell (a tombstone mutation) removes its plant atomically.
    #[test]
    fn vegetation_sync_translates_resident_cells_by_generation() {
        use saffron_spatial::{
            QuantizedLocalPosition, ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource,
            SpatialSourceId,
        };
        use saffron_spatial::{WorldBounds, WorldPosition};
        use saffron_vegetation::QuantizedOrientation;
        use saffron_vegetation::{
            CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
            InteractionPolicy, ManifestCellSection, ManifestSpeciesCount, PlantFlags,
            PlantLifecycle, PlantPoint, PlantPointColumns, VegetationBaseManifest,
            VegetationCellArtifactHeader, VegetationCellArtifactIndex, VegetationCellSection,
            VegetationCellSectionKind, VegetationManifestCell, VegetationManifestPlant,
            VegetationMutation, VegetationMutationRecord, VegetationResidencyBudgets,
            VegetationWorld, write_vegetation_cell_artifact,
        };

        let Some(mut harness) = harness("vegetation") else {
            return;
        };
        let before = validation_issue_count();

        // Register the assembly family under its id and seed the loader cache with the
        // manifest's exact artifact identity, so the sync resolves it without a store.
        let family_id = Uuid(9_888);
        let artifact_hash = saffron_vegetation::ContentHash::new([6; 32]);
        let (flat, hierarchy) = assembly_family_fixture();
        let gpu = RendererUploader::new(
            &harness.fixture.uploader,
            &harness.fixture.descriptors,
            false,
        );
        let uploaded = gpu
            .upload_mesh(&flat, &hierarchy, &[], None, None)
            .expect("assembly upload");
        harness.assets.register_family_render(
            family_id,
            Arc::clone(&uploaded),
            Arc::new(hierarchy),
        );
        harness.assets.plant_render_by_hash.insert(
            artifact_hash,
            Some(crate::PlantFamilyRender {
                mesh: Arc::clone(&uploaded),
                materials: Arc::from([] as [Uuid; 0]),
                combinations: Arc::from([(0_u32, 0_u32), (0, 1)]),
                phenotypes: Arc::from([
                    crate::PlantPhenotypeRender {
                        id: 0,
                        role: saffron_vegetation::PhenotypeRole::Healthy,
                        season_window: None,
                        variation: 0,
                        material_remap: Arc::from([]),
                    },
                    crate::PlantPhenotypeRender {
                        id: 1,
                        role: saffron_vegetation::PhenotypeRole::Senescent,
                        season_window: None,
                        variation: 0,
                        material_remap: Arc::from([]),
                    },
                ]),
            }),
        );

        // One resident cell with one mature plant of that family.
        let cell = saffron_spatial::WorldCellKey::base(0, 0, 0);
        let plant_id = saffron_vegetation::PlantId::explicit([1; 16]).unwrap();
        let position =
            WorldPosition::new(cell, QuantizedLocalPosition::new([10, 20, 30]).unwrap()).unwrap();
        let point = PlantPoint {
            id: plant_id,
            owner: cell,
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [saffron_spatial::DecisionScalar::from_bits(65_536); 3],
            bounds: WorldBounds::new([0, 0, 0], [100, 100, 100]).unwrap(),
            family: family_id,
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: 3,
            candidate: 4,
            parent: None,
            colony: None,
            ecology_tick: 5,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ONE,
            fuel: UnitInterval::ONE,
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::AUTHORED,
            interaction_policy: InteractionPolicy::Decorative,
            provenance: 0,
            attachment: None,
            surface_projection: [saffron_spatial::DecisionScalar::from_bits(0); 3],
        };
        let platform = CookPlatformProfile {
            target: "test-target".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-test".to_owned(),
            features: vec!["canonical-fixed".to_owned()],
        };
        let columns = PlantPointColumns::from_points(vec![point.clone()]).unwrap();
        let tile = MicroFieldTile {
            cell,
            family: family_id,
            dimensions: [4, 1, 4],
            density: vec![32_768; 16],
            attributes: std::collections::BTreeMap::from([(7_u128, vec![1_i32; 16])]),
            reconstruction_seed: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
        };
        let sections = vec![
            VegetationCellSection::new(
                VegetationCellSectionKind::MacroPoints,
                columns.canonical_bytes().unwrap(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::MicroFields,
                saffron_vegetation::encode_vegetation_micro_fields(std::slice::from_ref(&tile))
                    .unwrap(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderReferences,
                [b"SVEGRRF1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderBounds,
                [b"SVEGRBD1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
        ];
        let artifact = write_vegetation_cell_artifact(
            VegetationCellArtifactHeader {
                cell,
                cook_key: saffron_vegetation::ContentHash::new([8; 32]),
                platform_profile: platform.identity().unwrap(),
            },
            &sections,
        )
        .unwrap();
        let index = VegetationCellArtifactIndex::open(
            &artifact,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .unwrap();
        let mut manifest = VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            saffron_vegetation::ContentHash::new([3; 32]),
            CookVersionSet::current(),
            platform,
            saffron_vegetation::ContentHash::new([4; 32]),
        );
        manifest.plants.push(VegetationManifestPlant {
            family: family_id,
            tags: Vec::new(),
            source_hash: saffron_vegetation::ContentHash::new([5; 32]),
            artifact_hash,
            local_bounds_min: [saffron_spatial::DecisionScalar::from_bits(-65_536); 3],
            local_bounds_max: [saffron_spatial::DecisionScalar::from_bits(65_536); 3],
            variation_count: 1,
            phenotype_count: 1,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        });
        manifest.cells.push(VegetationManifestCell {
            cell,
            bounds: cell.bounds(),
            artifact_hash: saffron_vegetation::ContentHash::of(&artifact),
            payload_hash: index.payload_hash,
            dependencies: Vec::new(),
            species_counts: vec![ManifestSpeciesCount {
                family: family_id,
                macro_count: 1,
                micro_count: 0,
            }],
            macro_count: 1,
            micro_count: 0,
            resident_memory_bytes: index.sections.iter().map(|value| value.decoded_size).sum(),
            stored_bytes: artifact.len() as u64,
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
            sections: index
                .sections
                .iter()
                .map(|section| ManifestCellSection {
                    kind: section.kind,
                    version: section.version,
                    codec: section.codec,
                    alignment: section.alignment,
                    stored_size: section.stored_size,
                    decoded_size: section.decoded_size,
                    content_hash: section.content_hash,
                })
                .collect(),
        });
        let mut vegetation =
            VegetationWorld::new(manifest, VegetationResidencyBudgets::UNLIMITED).unwrap();
        vegetation
            .update_source(SpatialSource {
                id: SpatialSourceId(1),
                revision: 1,
                position: WorldPosition::origin(),
                velocity_mps: saffron_geometry::glam::DVec3::ZERO,
                prediction_seconds: 0.0,
                levels: vec![SourceLevel {
                    level: 0,
                    load_radius_cells: 0,
                    cleanup_radius_cells: 1,
                }],
                facets: ResidencyMask::one(ResidencyFacet::Render),
                priority: 10,
            })
            .unwrap();
        let staged = vegetation
            .begin_load(cell, ResidencyMask::one(ResidencyFacet::Render))
            .unwrap()
            .stage(&artifact)
            .unwrap();
        assert!(vegetation.publish_staged(staged).unwrap());

        let sync =
            |harness: &mut MirrorHarness, vegetation: &VegetationWorld, season: u16, stamp: u32| {
                let gpu = RendererUploader::new(
                    &harness.fixture.uploader,
                    &harness.fixture.descriptors,
                    false,
                );
                let mut target = GpuSceneMirrorTarget {
                    gpu_data: &mut harness.gpu_data,
                    gpu_scene: &mut harness.gpu_scene,
                    default_white: &harness.default_white,
                    pending: &mut harness.pending,
                    residency: &mut harness.residency,
                };
                harness
                    .mirror
                    .sync_vegetation(
                        WORLD,
                        vegetation,
                        &mut harness.assets,
                        &gpu,
                        season,
                        stamp,
                        &mut target,
                    )
                    .expect("vegetation sync");
            };
        sync(&mut harness, &vegetation, 0, 0);

        assert_eq!(harness.mirror.stats().instances, 1, "one mirrored plant");
        // The cell's micro tile packed into the fields arena: 64 B header + 16 padded
        // u16 density samples + one channel (16 B id + 16 i32 values).
        let field_range = harness.mirror.worlds[&WORLD.0].plant_fields[&cell].range;
        assert_eq!(field_range.count, 64 + 32 + 16 + 64);
        // The family gained one flagged identity field instance, and the resident-tile
        // directory carries one entry referencing it.
        let field_handle = harness.mirror.worlds[&WORLD.0].field_instances[&family_id.value()];
        let field_record = harness
            .gpu_scene
            .instance(WORLD, field_handle)
            .expect("field instance record");
        assert_eq!(
            field_record.flags,
            saffron_rendering::GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD
        );
        let (directory_range, directory_count) = harness.mirror.worlds[&WORLD.0]
            .field_directory
            .expect("directory");
        assert_eq!(directory_count, 1);
        assert_eq!(directory_range.count, 16);
        // The predicted budget: 16 texels at density 32768 → two blades each.
        assert_eq!(harness.mirror.stats().micro_predicted, 32);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        let handle = entry.handle;
        let record = harness
            .gpu_scene
            .instance(WORLD, handle)
            .expect("plant instance record");
        assert!(
            matches!(record.transform, GpuSceneTransform::Static(_)),
            "macro plants place with the compact exact transform"
        );
        // The vegetation columns: the fixture's decorative unattached point uploads
        // explicit conservative bounds (previous equals current at rest) and rides
        // the wind deformation prepass.
        assert_eq!(
            record.flags,
            GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS | GPU_SCENE_INSTANCE_FLAG_WIND
        );
        let columns = record.vegetation.expect("vegetation columns");
        assert!(columns.bounds_current[3] > 0.0, "a real bounds radius");
        assert_eq!(columns.bounds_current, columns.bounds_previous);
        assert!(columns.attachment.is_none());

        // An unchanged generation is a no-op: the same handle survives.
        sync(&mut harness, &vegetation, 0, 1);
        assert_eq!(
            harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)].handle,
            handle
        );

        // An autumn season resolves the mature plant to the Senescent phenotype:
        // the flip updates the SAME instance in place, carrying the previous
        // combination and the flip stamp for the traversal's crossfade.
        sync(&mut harness, &vegetation, 800, 42);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        assert_eq!(
            entry.handle, handle,
            "the flip never recreates the instance"
        );
        assert_eq!(entry.record.combination, 1, "the Senescent combination");
        let flipped = entry
            .record
            .vegetation
            .as_ref()
            .expect("vegetation columns");
        assert_eq!(flipped.combination_previous, 0);
        assert_eq!(flipped.flip_stamp, 42);
        let device_record = harness
            .gpu_scene
            .instance(WORLD, handle)
            .expect("updated plant record");
        assert_eq!(device_record.combination, 1);

        // Scrubbing back to summer flips again, previous now the autumn pair.
        sync(&mut harness, &vegetation, 100, 60);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        assert_eq!(entry.handle, handle);
        assert_eq!(entry.record.combination, 0);
        let restored = entry
            .record
            .vegetation
            .as_ref()
            .expect("vegetation columns");
        assert_eq!(restored.combination_previous, 1);
        assert_eq!(restored.flip_stamp, 60);

        // A tombstone republishes the cell; the re-translation removes the plant.
        vegetation
            .apply_confirmed_mutations(&[VegetationMutationRecord {
                header: saffron_vegetation::MutationHeader {
                    cell,
                    transaction: 1,
                    authority: 2,
                    logical_tick: 3,
                    idempotency_key: 4,
                    base_revision: None,
                },
                mutation: VegetationMutation::Tombstone { plant: plant_id },
            }])
            .unwrap();
        sync(&mut harness, &vegetation, 100, 61);
        assert_eq!(
            harness.mirror.stats().instances,
            0,
            "the tombstoned plant left the scene"
        );
        assert!(harness.gpu_scene.instance(WORLD, handle).is_none());
        // The republished cell re-packed its (unchanged) micro tile into a fresh range;
        // the field instance and directory survive (the family still has tiles).
        let republished_range = harness.mirror.worlds[&WORLD.0].plant_fields[&cell].range;
        assert_eq!(republished_range.count, field_range.count);
        assert_ne!(republished_range.first, field_range.first);
        assert_eq!(
            harness.mirror.worlds[&WORLD.0].field_instances[&family_id.value()],
            field_handle
        );
        assert!(harness.mirror.worlds[&WORLD.0].field_directory.is_some());

        drop(uploaded);
        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn page_payloads_stream_to_residency_through_the_worker() {
        let Some(mut harness) = harness("stream") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9601);
        write_triangle_mesh(&mut harness.assets, mesh_id, "stream-tri");

        let mut scene = Scene::new();
        let entity = scene.create_entity("Streamed");
        scene
            .add_component(entity, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);
        assert!(
            harness.residency.stats().registered >= 1,
            "mirrored pages register with residency"
        );

        // Guaranteed roots are demanded at registration; each drive hands requests to
        // the worker, drains its results, and publishes ready payloads.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            harness.drive(&scene);
            if harness.residency.stats().resident >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "page stream stalled: {:?}",
                harness.residency.stats()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let stats = harness.residency.stats();
        assert!(stats.resident_bytes > 0, "payload bytes accounted");
        let published = harness
            .gpu_data
            .page_table
            .iter()
            .filter(|(_, record)| record.byte_length > 0)
            .count();
        assert!(
            published >= 1,
            "the published page record carries its payload span"
        );

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn mirror_populates_instances_lights_and_shared_records() {
        let Some(mut harness) = harness("populate") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9001);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let b = scene.create_entity("B");
        scene
            .add_component(b, MeshComponent { mesh: mesh_id })
            .unwrap();
        let light = scene.create_entity("Light");
        scene.add_component(light, PointLight::default()).unwrap();
        let spot = scene.create_entity("Spot");
        scene.add_component(spot, SpotLight::default()).unwrap();

        harness.sync(&mut scene);

        let stats = harness.mirror.stats();
        assert_eq!(stats.meshes, 1, "one prototype for the shared mesh");
        assert_eq!(stats.instances, 2);
        assert_eq!(stats.lights, 2);
        assert_eq!(stats.materials, 1, "only the default material variant");
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 2);
        assert_eq!(harness.gpu_scene.lights(WORLD).unwrap().count(), 2);
        assert_eq!(harness.gpu_scene.prototypes().count(), 1);
        assert!(harness.gpu_scene.pages().count() >= 1);
        assert_eq!(
            stats.world_rebuilds, 1,
            "first sync rebuilds from live state"
        );

        let revision = harness.gpu_scene.revision();
        harness.sync(&mut scene);
        assert_eq!(
            harness.gpu_scene.revision(),
            revision,
            "an unchanged scene applies no deltas"
        );

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn transform_change_updates_only_the_moved_instance() {
        let Some(mut harness) = harness("move") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9002);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let b = scene.create_entity("B");
        scene
            .add_component(b, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);

        let moved_handle = {
            let entry = &harness.mirror.worlds[&WORLD.0].instances[&(a, InstanceSource::Static)];
            entry.handle
        };
        let old_transform = match &harness
            .gpu_scene
            .instance(WORLD, moved_handle)
            .expect("instance resident")
            .transform
        {
            GpuSceneTransform::Dynamic(dynamic) => *dynamic,
            GpuSceneTransform::Static(_) => panic!("scene entities mirror dynamically"),
        };

        let revision = harness.gpu_scene.revision();
        scene
            .with_component_mut::<Transform, _>(a, |t| t.translation = Vec3::new(3.0, 0.0, 0.0))
            .unwrap();
        harness.sync(&mut scene);

        assert_eq!(
            harness.gpu_scene.revision(),
            revision + 1,
            "exactly one instance record re-uploads"
        );
        let updated = match &harness
            .gpu_scene
            .instance(WORLD, moved_handle)
            .expect("instance resident")
            .transform
        {
            GpuSceneTransform::Dynamic(dynamic) => *dynamic,
            GpuSceneTransform::Static(_) => panic!("scene entities mirror dynamically"),
        };
        assert_eq!(
            updated.previous, old_transform.current,
            "the previous transform is the prior current"
        );
        assert_ne!(updated.current, old_transform.current);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn destroy_and_material_overrides_release_shared_records() {
        let Some(mut harness) = harness("release") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9003);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let mut overrides = saffron_json::Map::new();
        overrides.insert(
            "baseColor".to_owned(),
            saffron_json::parse_json("[1.0, 0.0, 0.0, 1.0]").unwrap(),
        );
        scene
            .add_component(
                a,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        material: Uuid(0),
                        overrides: saffron_json::Value::Object(overrides),
                    }],
                },
            )
            .unwrap();
        harness.sync(&mut scene);

        assert_eq!(
            harness.mirror.stats().materials,
            2,
            "the default variant plus the overridden variant"
        );
        let instance = harness
            .gpu_scene
            .instances(WORLD)
            .unwrap()
            .next()
            .map(|(_, record)| record.clone())
            .expect("instance resident");
        assert_eq!(instance.material_overrides.len(), 1);

        scene.add_component(a, MaterialSet::default()).unwrap();
        harness.sync(&mut scene);
        assert_eq!(
            harness.mirror.stats().materials,
            1,
            "clearing the override frees the variant"
        );

        scene.destroy_entity(a);
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 0);
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 0);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn rebinding_a_different_scene_instance_rebuilds_the_world() {
        let Some(mut harness) = harness("rebind") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9004);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut authored = Scene::new();
        let a = authored.create_entity("A");
        authored
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut authored);
        assert_eq!(harness.mirror.stats().instances, 1);

        let mut play = Scene::new();
        for name in ["P1", "P2"] {
            let entity = play.create_entity(name);
            play.add_component(entity, MeshComponent { mesh: mesh_id })
                .unwrap();
        }
        harness.sync(&mut play);
        assert_eq!(
            harness.mirror.stats().instances,
            2,
            "the play duplicate replaces the authored world's instances"
        );
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 2);
        assert_eq!(harness.mirror.stats().world_rebuilds, 2);

        harness.sync(&mut authored);
        assert_eq!(harness.mirror.stats().instances, 1);
        assert_eq!(harness.mirror.stats().world_rebuilds, 3);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn missing_mesh_is_unresolved_until_it_appears() {
        let Some(mut harness) = harness("unresolved") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9005);

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 0);
        assert_eq!(harness.mirror.stats().unresolved_instances, 1);

        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");
        let entry = harness
            .assets
            .catalog
            .remove(mesh_id)
            .expect("catalog row present");
        harness.assets.register_imported_asset(entry);
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 1);
        assert_eq!(harness.mirror.stats().unresolved_instances, 0);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }
}
