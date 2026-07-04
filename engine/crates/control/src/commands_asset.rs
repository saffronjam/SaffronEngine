//! The 51 asset/project-domain control commands: project lifecycle (get/new/open/save/
//! load/reload + create-script), model + texture import + instantiation, the catalog
//! (list/scan/clean/delete-unused), sub-asset extraction, model info + references + the
//! asset model, the asset-preview enter/exit + active-view switch, asset/folder management
//! (rename/move/create-folder/delete), usages + metadata + assignment, the material system
//! (create/assign/import/list/get/update/preview/set-graph/instance/override/compile/cook),
//! scene save+load, screenshot, and thumbnails (get/view/cache).
//!
//! The highest-coupling domain: a handler holds `&mut` to [`AssetServer`][saffron_assets::AssetServer], the
//! [`SceneEditContext`], and the renderer at once through the disjoint
//! [`EngineContext`] fields. Heavy lifting (the importers, the node-graph→Slang codegen,
//! the preview render) lives in `saffron-assets`; these handlers stay thin
//! orchestration.

use std::path::{Path, PathBuf};

use saffron_assets::{
    AssetServer, BUILTIN_SPHERE_MESH_ID, BuiltinMesh, ContainerMetadata, MaterialAsset,
    PREVIEW_DISPLACE_SPHERE_MESH_ID, PREVIEW_MATERIAL_ID, ProjectHost, ProjectInfo, analyze_clean,
    asset_bytes, asset_type_name, build_dependency_graph, clear_extraction,
    colorspace_for_role_explicit, colorspace_name, create_project_script, default_display_name,
    default_material_asset, delete_unused, exposed_parameter, extract_sub_asset,
    import_material_folder, load_catalog_material_asset, load_catalog_material_asset_raw,
    load_material_asset, load_material_asset_raw, lower_graph_to_params, model_render_aabb,
    pbr_exposed_parameters, pick_scene_surface, reimport_model, request_thumbnail,
    save_material_asset, texture_role_from_hint, texture_role_name, update_material_asset,
    valid_project_name, viewport_ray,
};
use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3 as MathVec3};
use saffron_protocol::{
    AnimationClipDto, AssetAttributionDto, AssetCapabilitiesDto, AssetEntryDto, AssetList,
    AssetMetadataDto, AssetMetadataParams, AssetModelResult, AssetPlacementParams,
    AssetPlacementPhaseDto, AssetPlacementResult, AssetRef, AssetReferencesParams,
    AssetReferencesResult, AssetSlotDto, AssetTypeDto, AssetUsageDto, AssetUsagesParams,
    AssetUsagesResult, AssignAssetParams, AssignAssetResult, BoneDto, BootStageDto,
    CleanAssetsParams, CleanCandidateDto, CleanReport, ClearExtractionParams,
    CreateAssetFolderParams, CreateScriptParams, CreateScriptResult, DeleteAssetFolderParams,
    DeleteAssetParams, DeleteAssetResult, DeleteUnusedParams, DeleteUnusedResult, EmptyParams,
    EntityRef, ExportAppParams, ExportAppResult, ExposedParamDto, ExtractSubAssetParams,
    GetAssetModelParams, ImportModelParams, ImportModelResult, ImportTextureParams,
    ImportTextureResult, InstantiateModelParams, MaterialAssignParams, MaterialAssignResult,
    MaterialCompileParams, MaterialCompileResult, MaterialCookResult, MaterialCreateInstanceParams,
    MaterialCreateParams, MaterialCreateResult, MaterialGetParams, MaterialGetResult,
    MaterialImportParams, MaterialImportResultDto, MaterialListResult, MaterialRefDto,
    MaterialSchemaParams, MaterialSchemaResult, MaterialSetGraphParams, MaterialSetGraphResult,
    MaterialSetOverrideParams, MaterialSetOverrideResult, MaterialUpdateParams,
    MaterialUpdateResult, ModelInfoParams, ModelInfoResult, ModelSubAssetDto, MoveAssetParams,
    NewProjectParams, OptionalPathParams, PathParams, PathResult, PlacementTransformDto,
    PlayStateResult, PreviewRenderParams, PreviewRenderResult, ProjectInfoDto, ProjectPhaseDto,
    ProjectStatusDto, ProjectStoresDto, QuitResult, ReimportModelParams, ReimportModelResult,
    RenameAssetFolderParams, RenameAssetParams, ScanAssetsResult, ScreenshotParams,
    ScreenshotResult, ScreenshotTargetDto, SetActiveViewParams, SetActiveViewResult,
    ThumbnailCacheParams, ThumbnailCacheResult, ThumbnailParams, ThumbnailResult, Uuid as WireUuid,
    Vec3, Vec4,
};
use saffron_rendering::{PngTransfer, ViewId};
use saffron_scene::{
    AnimationPlayer, AssetEntry, AssetType, Attribution, Colorspace, DirectionalLight, Entity,
    IdComponent, MaterialSet, MaterialSlot, Mesh, Name, PreviewGhost, Scene, SkinnedMesh, SkyMode,
    TextureRole, Transform,
};
use saffron_sceneedit::{
    BootStage, NewProjectSpec, PlacementPreview, PlayState, ProjectLoadRequest, ProjectPhase,
    SceneEditCamera, SceneEditContext,
};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, ControlRenderer, EngineContext};
use crate::selector::{entity_ref_dto, entity_uuid, resolve_entity};

/// The `base64` standard encoder, used by `preview-render` and the thumbnail commands to
/// ship PNG bytes inside a JSON string.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// The wire `AssetTypeDto` for a catalog kind.
fn asset_type_dto(asset_type: AssetType) -> AssetTypeDto {
    match asset_type {
        AssetType::Texture => AssetTypeDto::Texture,
        AssetType::Other => AssetTypeDto::Other,
        AssetType::Animation => AssetTypeDto::Animation,
        AssetType::Material => AssetTypeDto::Material,
        AssetType::Model => AssetTypeDto::Model,
        AssetType::Mesh => AssetTypeDto::Mesh,
    }
}

/// Reads an id-or-name selector value as its string form, treating any non-string as empty.
fn selector_string(selector: &Value) -> String {
    selector.as_str().unwrap_or_default().to_owned()
}

/// Wraps a folder path as an optional, mapping an empty path to `None`.
fn optional_folder(folder: &str) -> Option<String> {
    if folder.is_empty() {
        None
    } else {
        Some(folder.to_owned())
    }
}

/// The uuid an id-or-name selector resolves to: an unsigned number, a non-negative signed
/// number, or a whole-string decimal parse.
fn selector_id(selector: &Value) -> u64 {
    if let Some(value) = selector.as_u64() {
        return value;
    }
    if let Some(value) = selector.as_i64() {
        return u64::try_from(value).unwrap_or(0);
    }
    selector_string(selector).parse::<u64>().unwrap_or(0)
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to a catalog entry id,
/// by id or name.
fn resolve_asset(ctx: &EngineContext<'_>, selector: &Value) -> Result<Uuid> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for entry in &ctx.assets.catalog.entries {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(entry.id);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to its index in the
/// catalog `entries`.
fn resolve_asset_index(ctx: &EngineContext<'_>, selector: &Value) -> Result<usize> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for (i, entry) in ctx.assets.catalog.entries.iter().enumerate() {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(i);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

fn preview_asset_placement(
    ctx: &mut EngineContext<'_>,
    params: AssetPlacementParams,
) -> Result<AssetPlacementResult> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        return Err(Error::command(
            "asset placement is only available in Edit mode",
        ));
    }
    if ctx.scene_edit.preview_active_view {
        return Err(Error::command(
            "asset placement targets the scene view, not the asset preview",
        ));
    }
    let selector = params
        .asset
        .as_ref()
        .ok_or_else(|| Error::command("missing 'asset'"))?;
    let asset = resolve_asset(ctx, selector)?;
    let entry = ctx
        .assets
        .catalog
        .find(asset)
        .ok_or_else(|| Error::command(format!("no asset '{}'", asset.value())))?;
    if entry.asset_type != AssetType::Model {
        return Err(Error::command(format!(
            "asset {} is not a model",
            asset.value()
        )));
    }

    let u = params.u.unwrap_or(0.5).clamp(0.0, 1.0);
    let v = params.v.unwrap_or(0.5).clamp(0.0, 1.0);
    let ndc = Vec2::new(u * 2.0 - 1.0, v * 2.0 - 1.0);
    let viewport = (
        ctx.renderer.viewport_width(),
        ctx.renderer.viewport_height(),
    );
    let cam = ctx.scene_edit.render_camera_view();
    let name = entry.name.clone();

    // Reuse the existing ghost if it already previews this asset; otherwise (re)instantiate one
    // into the authored scene and tag its whole subtree so it is excluded from save / pick /
    // outliner while it renders.
    let reuse = ctx
        .scene_edit
        .placement_preview
        .as_ref()
        .filter(|p| p.asset == asset)
        .map(|p| p.root)
        .filter(|&root| ctx.scene_edit.scene.has_component::<IdComponent>(root));
    let root = match reuse {
        Some(root) => root,
        None => {
            clear_placement_ghost(ctx);
            let root = ctx
                .assets
                .instantiate_model(&mut ctx.scene_edit.scene, asset, &name)
                .map_err(|e| Error::command(e.to_string()))?;
            for node in ctx.scene_edit.scene.subtree_entities(root) {
                let _ = ctx
                    .scene_edit
                    .scene
                    .add_component(node, PreviewGhost::default());
            }
            ctx.scene_edit.placement_preview = Some(PlacementPreview {
                asset,
                root,
                rest_bounds: None,
            });
            root
        }
    };

    let mut placement = None;
    {
        let scene = &mut ctx.scene_edit.scene;
        let assets = &mut ctx.assets;
        let preview_slot = &mut ctx.scene_edit.placement_preview;
        let renderer = &mut ctx.renderer;
        renderer.with_gpu_uploader(&mut |gpu| {
            scene.update_world_transforms();
            // Measure the rest-pose bounds once (before any placement transform skews them);
            // a later resolve (Phase 2 async upload) fills them in if the mesh wasn't ready.
            if let Some(preview) = preview_slot.as_mut()
                && preview.rest_bounds.is_none()
            {
                preview.rest_bounds = model_render_aabb(gpu, scene, assets, root);
            }
            let bounds = preview_slot.as_ref().and_then(|p| p.rest_bounds);
            placement = Some(compute_asset_placement(
                gpu, viewport, scene, assets, bounds, &cam, ndc,
            ));
        });
    }
    let transform = match placement.ok_or_else(|| Error::command("upload seam unavailable"))? {
        Ok(transform) => transform,
        Err(reason) => {
            return Ok(AssetPlacementResult {
                active: true,
                valid: false,
                transform: None,
                entity: None,
                reason: Some(reason),
            });
        }
    };
    apply_transform(&mut ctx.scene_edit.scene, root, transform);

    Ok(AssetPlacementResult {
        active: true,
        valid: true,
        transform: Some(placement_transform_dto(&transform)),
        entity: None,
        reason: None,
    })
}

/// Destroys the current placement ghost subtree (if any) and clears the preview slot.
fn clear_placement_ghost(ctx: &mut EngineContext<'_>) {
    if let Some(preview) = ctx.scene_edit.placement_preview.take() {
        ctx.scene_edit.scene.destroy_entity(preview.root);
    }
}

fn commit_asset_placement(ctx: &mut EngineContext<'_>) -> Result<AssetPlacementResult> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        clear_placement_ghost(ctx);
        return Err(Error::command(
            "asset placement is only available in Edit mode",
        ));
    }
    if ctx.scene_edit.preview_active_view {
        clear_placement_ghost(ctx);
        return Err(Error::command(
            "asset placement targets the scene view, not the asset preview",
        ));
    }
    let Some(preview) = ctx.scene_edit.placement_preview.take() else {
        return Ok(AssetPlacementResult {
            active: false,
            valid: false,
            transform: None,
            entity: None,
            reason: Some("no active placement preview".to_owned()),
        });
    };
    // The ghost already sits in the scene at the placement transform with its geometry uploaded;
    // committing is just dropping the tag from its subtree so it persists and selects.
    let root = preview.root;
    for node in ctx.scene_edit.scene.subtree_entities(root) {
        ctx.scene_edit.scene.remove_component::<PreviewGhost>(node);
    }
    let transform = ctx
        .scene_edit
        .scene
        .with_component::<Transform, _>(root, |t| *t)
        .unwrap_or_default();
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.set_selection(root);
    let entity = {
        let scene = &mut ctx.scene_edit.scene;
        entity_ref_dto(scene, root)
    };
    Ok(AssetPlacementResult {
        active: false,
        valid: true,
        transform: Some(placement_transform_dto(&transform)),
        entity: Some(entity),
        reason: None,
    })
}

/// Computes the placement transform that drops a model with the given rest-pose world AABB
/// `rest_bounds` onto the surface (or ground plane) under the cursor. The placement ray skips
/// [`PreviewGhost`]-tagged geometry, so it sees only the authored scene, never the ghost itself.
fn compute_asset_placement(
    gpu: &dyn saffron_assets::GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    rest_bounds: Option<(MathVec3, MathVec3)>,
    cam: &saffron_scene::CameraView,
    ndc: Vec2,
) -> std::result::Result<Transform, String> {
    if viewport.0 == 0 || viewport.1 == 0 {
        return Err("viewport has zero size".to_owned());
    }
    let ray = viewport_ray(viewport, cam, ndc);
    let target = pick_scene_surface(gpu, viewport, scene, assets, cam, ndc)
        .map(|hit| hit.point)
        .or_else(|| ground_plane_hit(ray))
        .ok_or_else(|| "placement ray did not hit the scene or ground plane".to_owned())?;
    let (min, max) = rest_bounds.ok_or_else(|| "model has no renderable bounds".to_owned())?;
    let bottom_center = MathVec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
    Ok(Transform {
        translation: target - bottom_center,
        rotation: MathVec3::ZERO,
        scale: MathVec3::ONE,
    })
}

fn ground_plane_hit(ray: saffron_geometry::Ray) -> Option<MathVec3> {
    if ray.dir.y.abs() < 0.0001 {
        return None;
    }
    let t = -ray.origin.y / ray.dir.y;
    (t >= 0.0).then_some(ray.origin + ray.dir * t)
}

fn apply_transform(scene: &mut Scene, entity: Entity, transform: Transform) {
    let _ = scene.with_component_mut::<Transform, _>(entity, |t| *t = transform);
}

fn placement_transform_dto(transform: &Transform) -> PlacementTransformDto {
    PlacementTransformDto {
        translation: vec3(transform.translation),
        rotation: vec3(transform.rotation),
        scale: vec3(transform.scale),
    }
}

/// Parses the opaque `stores` sidecar block into the wire DTO, defaulting when absent.
fn stores_dto_from_value(value: &serde_json::Value) -> ProjectStoresDto {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Maps an `import-texture` colorspace hint to a `Colorspace`; `auto`/absent → heuristic.
fn colorspace_from_str(value: Option<&str>) -> Option<Colorspace> {
    match value.map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("srgb") => Some(Colorspace::Srgb),
        Some("linear") => Some(Colorspace::Linear),
        Some("hdr") => Some(Colorspace::Hdr),
        _ => None,
    }
}

/// Converts a wire attribution DTO into the catalog's `Attribution`.
fn attribution_from_dto(dto: AssetAttributionDto) -> Attribution {
    Attribution {
        license_id: dto.license_id,
        requires_attribution: dto.requires_attribution,
        license_url: dto.license_url,
        author: dto.author,
        source_url: dto.source_url,
        store_id: dto.store_id,
    }
}

/// Rebuilds the catalog's `by_id` index after an in-place `entries` mutation.
fn rebuild_asset_index(catalog: &mut saffron_scene::AssetCatalog) {
    catalog.by_id.clear();
    for (i, entry) in catalog.entries.iter().enumerate() {
        catalog.by_id.insert(entry.id.value(), i);
    }
}

/// Creation time (seconds since the Unix epoch) of the file backing a catalog entry, preferring
/// the filesystem birth time and falling back to the modified time; `0` if neither is available.
fn asset_created_at(root: &Path, rel_path: &str) -> i64 {
    let metadata = match std::fs::metadata(root.join(rel_path)) {
        Ok(metadata) => metadata,
        Err(_) => return 0,
    };
    metadata
        .created()
        .or_else(|_| metadata.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The wire DTO for one catalog entry.
fn asset_dto(root: &Path, entry: &AssetEntry) -> AssetEntryDto {
    let is_texture = entry.asset_type == AssetType::Texture;
    // Resolve the effective upload space (never surface `Auto`), matching the loader/sidecar.
    let colorspace = is_texture.then(|| {
        let cs = if entry.colorspace != Colorspace::Auto {
            entry.colorspace
        } else if entry.hdr {
            Colorspace::Hdr
        } else if entry.linear {
            Colorspace::Linear
        } else {
            Colorspace::Srgb
        };
        colorspace_name(cs).to_owned()
    });
    let role = (is_texture && entry.role != TextureRole::Unknown)
        .then(|| texture_role_name(entry.role).to_owned());
    AssetEntryDto {
        id: WireUuid(entry.id.value()),
        name: entry.name.clone(),
        r#type: asset_type_dto(entry.asset_type),
        path: entry.path.clone(),
        folder: optional_folder(&entry.folder),
        container: (entry.container.value() != 0).then(|| WireUuid(entry.container.value())),
        duration: (entry.asset_type == AssetType::Animation).then_some(entry.duration),
        rigged: entry.rigged.then_some(true),
        colorspace,
        role,
        created_at: asset_created_at(root, &entry.path),
        attribution: entry.attribution.as_ref().map(attribution_to_dto),
    }
}

/// Converts catalog `Attribution` into the wire DTO.
fn attribution_to_dto(attribution: &Attribution) -> AssetAttributionDto {
    AssetAttributionDto {
        license_id: attribution.license_id.clone(),
        requires_attribution: attribution.requires_attribution,
        license_url: attribution.license_url.clone(),
        author: attribution.author.clone(),
        source_url: attribution.source_url.clone(),
        store_id: attribution.store_id.clone(),
    }
}

/// The compact `{ id, name, folder? }` reply for a catalog entry.
fn asset_ref(entry: &AssetEntry) -> AssetRef {
    AssetRef {
        id: WireUuid(entry.id.value()),
        name: entry.name.clone(),
        folder: optional_folder(&entry.folder),
    }
}

/// Rewrites the durable `.smeta` sidecar for each id after a folder mutation touched many
/// rows. Best-effort: an IO failure is logged, not surfaced, so the command still succeeds.
fn write_asset_sidecars(assets: &AssetServer, ids: &[Uuid], label: &str) {
    for &id in ids {
        if let Err(err) = assets.write_asset_sidecar(id) {
            tracing::warn!("{label}: could not write .smeta for {}: {err}", id.value());
        }
    }
}

/// The full catalog as an [`AssetList`].
fn asset_list_dto(root: &Path, catalog: &saffron_scene::AssetCatalog) -> AssetList {
    AssetList {
        assets: catalog
            .entries
            .iter()
            .map(|entry| asset_dto(root, entry))
            .collect(),
        folders: catalog.folders.clone(),
    }
}

/// Whether a folder path is well-formed: non-empty, no leading/trailing `/`, no `\`, and no
/// empty `//` segment.
fn valid_folder_path(folder: &str) -> bool {
    if folder.is_empty()
        || folder.starts_with('/')
        || folder.ends_with('/')
        || folder.contains('\\')
        || folder.contains("//")
    {
        return false;
    }
    true
}

/// Whether the catalog already carries `folder`.
fn has_folder(catalog: &saffron_scene::AssetCatalog, folder: &str) -> bool {
    catalog.folders.iter().any(|existing| existing == folder)
}

/// Whether `candidate` is a strict descendant folder of `folder`.
fn is_folder_descendant(candidate: &str, folder: &str) -> bool {
    candidate.len() > folder.len()
        && candidate.starts_with(folder)
        && candidate.as_bytes()[folder.len()] == b'/'
}

/// Re-roots `value` from the `from` folder prefix onto `to`.
fn replace_folder_prefix(value: &str, from: &str, to: &str) -> String {
    if value == from {
        return to.to_owned();
    }
    if is_folder_descendant(value, from) {
        return format!("{to}{}", &value[from.len()..]);
    }
    value.to_owned()
}

/// The entity's `Name`, or empty.
fn entity_name(scene: &Scene, entity: Entity) -> String {
    scene
        .with_component::<Name, _>(entity, |n| n.name.clone())
        .unwrap_or_default()
}

/// The entity's `IdComponent` uuid as an optional wire uuid.
fn entity_id(scene: &Scene, entity: Entity) -> Option<WireUuid> {
    let id = entity_uuid(scene, entity);
    (id != 0).then_some(WireUuid(id))
}

/// One `(entity, slot)` reference, collected during a scene scan and resolved to a usage DTO
/// after — so the scan's `&mut` scene borrow does not overlap the per-entity name/id reads.
type Reference = (Entity, &'static str);

/// Collects every `(entity, slot)` reference to `asset` in the scene (the scan half of
/// [`collect_asset_usages`] / [`clear_asset_usages`]): mesh slots + material albedo /
/// metallic-roughness slots. The environment sky-texture hit is the boolean second tuple.
fn scan_asset_references(scene: &mut Scene, asset: Uuid) -> (Vec<Reference>, bool) {
    let mut refs = Vec::new();
    scene.for_each::<(&Mesh,), _>(|entity, (mesh,)| {
        if mesh.mesh.value() == asset.value() {
            refs.push((entity, "mesh"));
        }
    });
    scene.for_each::<(&MaterialSet,), _>(|entity, (set,)| {
        if set
            .slots
            .iter()
            .any(|s| s.material.value() == asset.value())
        {
            refs.push((entity, "material"));
        }
    });
    let sky = scene.environment.sky_texture.value() == asset.value();
    (refs, sky)
}

/// One collected `(entity, slot)` reference as a usage DTO (the name/id read after the scan).
fn usage_dto(scene: &Scene, entity: Entity, slot: &str) -> AssetUsageDto {
    AssetUsageDto {
        entity: entity_id(scene, entity),
        entity_name: Some(entity_name(scene, entity)),
        slot: slot.to_owned(),
    }
}

/// Every place `asset` is referenced in the active scene: `Mesh` slots, `MaterialSet` slot
/// material references, and the environment sky texture.
fn collect_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
    let (refs, sky) = scan_asset_references(scene, asset);
    let mut usages: Vec<AssetUsageDto> = refs
        .iter()
        .map(|&(entity, slot)| usage_dto(scene, entity, slot))
        .collect();
    if sky {
        usages.push(AssetUsageDto {
            entity: None,
            entity_name: None,
            slot: "environment.skyTexture".to_owned(),
        });
    }
    usages
}

/// Clears every reference to `asset` in the scene and returns the cleared usages (the
/// `delete-asset` cascade). The DTOs are built (name/id read) before the slot is zeroed.
fn clear_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
    let (refs, sky) = scan_asset_references(scene, asset);
    let mut cleared: Vec<AssetUsageDto> = refs
        .iter()
        .map(|&(entity, slot)| usage_dto(scene, entity, slot))
        .collect();
    for &(entity, slot) in &refs {
        match slot {
            "mesh" => {
                let _ = scene.with_component_mut::<Mesh, _>(entity, |m| m.mesh = Uuid(0));
            }
            // "material": clear every slot that referenced the deleted material to the
            // built-in default.
            _ => {
                let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
                    for s in &mut set.slots {
                        if s.material.value() == asset.value() {
                            s.material = Uuid(0);
                        }
                    }
                });
            }
        }
    }
    if sky {
        cleared.push(AssetUsageDto {
            entity: None,
            entity_name: None,
            slot: "environment.skyTexture".to_owned(),
        });
        scene.environment.sky_texture = Uuid(0);
    }
    cleared
}

/// The current project's identity, read from the editor.
fn current_project_info(ctx: &EngineContext<'_>) -> ProjectInfo {
    ProjectInfo {
        loaded: ctx.scene_edit.project_ready(),
        root: ctx.scene_edit.project_root.clone(),
        path: ctx.scene_edit.project_path.clone(),
        name: ctx.scene_edit.project_name.clone(),
        display_name: ctx.scene_edit.project_display_name.clone(),
    }
}

/// Writes a [`ProjectInfo`] back onto the editor, also resetting the scene path.
fn apply_project_info(ctx: &mut EngineContext<'_>, project: &ProjectInfo) {
    ctx.scene_edit.project_phase = if project.loaded {
        ProjectPhase::Ready
    } else {
        ProjectPhase::Unloaded
    };
    ctx.scene_edit.project_root = project.root.clone();
    ctx.scene_edit.project_path = project.path.clone();
    ctx.scene_edit.project_name = project.name.clone();
    ctx.scene_edit.project_display_name = project.display_name.clone();
    ctx.scene_edit.scene_path = project.path.clone();
}

/// Brings the host's project up from the editor-set environment at startup by seeding the loader
/// inbox — the same non-blocking path the lifecycle commands use, so there is one project
/// bring-up code path: `SAFFRON_PROJECT` selects a project to open (or create when the name is
/// valid and unborn), else `SAFFRON_SCRATCH_PROJECT` makes a deterministic per-shell scratch
/// project, else a `project.json` in the working directory is opened. With none of those set the
/// host waits for the editor's project picker (phase stays `Unloaded`).
pub fn bootstrap_project_from_env(scene_edit: &mut SceneEditContext) {
    let request = if let Some(selected) = std::env::var_os("SAFFRON_PROJECT")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
    {
        let create_new =
            valid_project_name(&selected) && !saffron_assets::project_json_path(&selected).exists();
        if create_new {
            ProjectLoadRequest::New(NewProjectSpec {
                name: selected,
                display_name: String::new(),
                root: String::new(),
            })
        } else {
            ProjectLoadRequest::Open(selected)
        }
    } else if std::env::var_os("SAFFRON_SCRATCH_PROJECT").is_some() {
        ProjectLoadRequest::New(NewProjectSpec {
            name: saffron_assets::scratch_project_name(),
            display_name: "Scratch Project".to_owned(),
            root: String::new(),
        })
    } else if Path::new("project.json").exists() {
        ProjectLoadRequest::Open("project.json".to_owned())
    } else {
        return; // Nothing to bring up — wait for the editor's project picker.
    };

    scene_edit.project_load_inbox = Some(request);
    scene_edit.project_phase = ProjectPhase::Loading;
}

/// The wire DTO for a [`ProjectInfo`].
fn project_dto(project: &ProjectInfo) -> ProjectInfoDto {
    ProjectInfoDto {
        loaded: project.loaded,
        root: project.root.clone(),
        path: project.path.clone(),
        name: project.name.clone(),
        display_name: project.display_name.clone(),
    }
}

/// The wire DTO for the live project-load phase + progress.
fn project_status_dto(ctx: &EngineContext<'_>) -> ProjectStatusDto {
    let sc = &ctx.scene_edit;
    let p = &sc.project_load;
    ProjectStatusDto {
        phase: match sc.project_phase {
            ProjectPhase::Unloaded => ProjectPhaseDto::Unloaded,
            ProjectPhase::Loading => ProjectPhaseDto::Loading,
            ProjectPhase::Ready => ProjectPhaseDto::Ready,
            ProjectPhase::Failed => ProjectPhaseDto::Failed,
        },
        stage: boot_stage_dto(p.stage),
        done: i32::try_from(p.done).unwrap_or(i32::MAX),
        total: i32::try_from(p.total).unwrap_or(i32::MAX),
        label: p.label.clone(),
        current_item: p.current_item.clone(),
        error: p.error.clone(),
        version: i64::try_from(p.version).unwrap_or(i64::MAX),
        name: sc.project_name.clone(),
        path: sc.project_path.clone(),
    }
}

/// Maps a [`BootStage`] onto its wire enum.
fn boot_stage_dto(stage: BootStage) -> BootStageDto {
    match stage {
        BootStage::Manifest => BootStageDto::Manifest,
        BootStage::Catalog => BootStageDto::Catalog,
        BootStage::Scene => BootStageDto::Scene,
        BootStage::Install => BootStageDto::Install,
        BootStage::Assets => BootStageDto::Assets,
        BootStage::Skybox => BootStageDto::Skybox,
        BootStage::Accel => BootStageDto::Accel,
        BootStage::Ready => BootStageDto::Ready,
        BootStage::Failed => BootStageDto::Failed,
    }
}

/// The "no project loaded" guard.
fn require_project_loaded(ctx: &EngineContext<'_>) -> Result<()> {
    if ctx.scene_edit.project_ready() {
        Ok(())
    } else {
        Err(Error::command("no project loaded"))
    }
}

/// The [`ProjectHost`] adapter over the renderer seam: the project lifecycle commands hand
/// it to [`AssetServer::create_project`][saffron_assets::AssetServer::create_project] /
/// `load_project` / `save_project`, which need the
/// GPU-idle + render-settings serde the renderer owns. Wraps `&mut dyn ControlRenderer`,
/// so it borrows only the renderer field of the [`EngineContext`] — disjoint from `assets`
/// and `scene_edit`.
pub(crate) struct RendererProjectHost<'a> {
    pub(crate) renderer: &'a mut dyn ControlRenderer,
}

impl ProjectHost for RendererProjectHost<'_> {
    fn wait_gpu_idle(&mut self) {
        self.renderer.wait_gpu_idle();
    }

    fn render_settings_to_json(&self) -> Value {
        self.renderer.render_settings_to_json()
    }

    fn apply_render_settings(&mut self, settings: &Value) {
        self.renderer.apply_render_settings(settings);
    }
}

/// Cooks the loaded project into a standalone app folder at `params.output_dir`: pre-bakes every
/// material's mesh SPIR-V (so the shipped player never needs `slangc`), then stages the player
/// binary + the project data (`project.json`, `assets/`, `src/`) + the engine `shaders/` + an
/// `app.json` manifest. Side-effecting (writes to disk); returns the staged path and any non-fatal
/// warnings (a material that failed to bake, a missing player binary).
fn export_app(ctx: &mut EngineContext<'_>, params: &ExportAppParams) -> Result<ExportAppResult> {
    if params.output_dir.trim().is_empty() {
        return Err(Error::command("missing 'outputDir'"));
    }
    let project_root = ctx.scene_edit.project_root.clone();
    if project_root.is_empty() {
        return Err(Error::command("no project loaded to export"));
    }
    let project_root = PathBuf::from(&project_root);
    let mut warnings: Vec<String> = Vec::new();

    // 1. Pre-bake every material's mesh shader into the project assets (the player loads only the
    //    baked `.spv`; it never invokes `slangc`). Mirrors the `material-cook` command's loop.
    let material_ids: Vec<Uuid> = ctx
        .assets
        .catalog
        .entries
        .iter()
        .filter(|e| e.asset_type == AssetType::Material)
        .map(|e| e.id)
        .collect();
    for id in material_ids {
        let Ok(raw) = load_material_asset_raw(ctx.assets, id) else {
            continue;
        };
        if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
            continue; // a factor-only material has no node graph to bake.
        }
        let mut probe = raw.clone();
        if lower_graph_to_params(&raw.graph, &mut probe) {
            continue; // the graph lowers to plain params — no codegen shader needed.
        }
        if let Err(err) = ctx.assets.compile_material_mesh_shader(&raw.graph, id) {
            warnings.push(format!("material {id}: shader bake failed: {err}"));
        }
    }

    // 2. Stage the folder. The player binary + engine shaders sit beside the running host binary.
    let out = PathBuf::from(&params.output_dir);
    std::fs::create_dir_all(&out)
        .map_err(|e| Error::command(format!("create output dir '{}': {e}", out.display())))?;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| Error::command("cannot resolve the engine binary directory"))?;

    copy_file(
        &project_root.join("project.json"),
        &out.join("project.json"),
    )
    .map_err(|e| Error::command(format!("copy project.json: {e}")))?;
    copy_dir_recursive(&project_root.join("assets"), &out.join("assets"))
        .map_err(|e| Error::command(format!("copy assets/: {e}")))?;
    let src = project_root.join("src");
    if src.is_dir() {
        copy_dir_recursive(&src, &out.join("src"))
            .map_err(|e| Error::command(format!("copy src/: {e}")))?;
    }
    let shaders = exe_dir.join("shaders");
    if shaders.is_dir() {
        copy_dir_recursive(&shaders, &out.join("shaders"))
            .map_err(|e| Error::command(format!("copy shaders/: {e}")))?;
    } else {
        warnings.push(format!("engine shaders not found at {}", shaders.display()));
    }
    let player = exe_dir.join("saffron-player");
    if player.is_file() {
        copy_file(&player, &out.join("saffron-player"))
            .map_err(|e| Error::command(format!("copy saffron-player: {e}")))?;
    } else {
        warnings.push(format!(
            "saffron-player binary not found at {} (build it before export)",
            player.display()
        ));
    }
    // Bundle the C++ runtime libs the player links through the vendored Jolt physics (built
    // against libc++ for deterministic physics). The player's `$ORIGIN` rpath finds them here, so
    // the folder runs on a host without the toolbox's libc++.
    for lib in ["libc++.so.1", "libc++abi.so.1"] {
        match find_runtime_lib(lib) {
            Some(src) => copy_file(&src, &out.join(lib))
                .map_err(|e| Error::command(format!("copy {lib}: {e}")))?,
            None => warnings.push(format!(
                "{lib} not found on the host; the exported app needs it beside saffron-player"
            )),
        }
    }
    let app_json = serde_json::to_string_pretty(&params.app)
        .map_err(|e| Error::command(format!("serialize app.json: {e}")))?;
    std::fs::write(out.join("app.json"), app_json)
        .map_err(|e| Error::command(format!("write app.json: {e}")))?;

    Ok(ExportAppResult {
        path: out.to_string_lossy().into_owned(),
        warnings,
    })
}

/// Resolves a shared library by SONAME from the usual Linux library directories (honoring a
/// `LD_LIBRARY_PATH` override first), returning the first match — for bundling the C++ runtime
/// into a standalone export.
fn find_runtime_lib(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("LD_LIBRARY_PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    dirs.extend(["/usr/lib64", "/usr/lib", "/lib64", "/lib"].map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|p| p.exists())
}

/// Copies one file, creating the destination's parent directory first.
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Recursively copies a directory tree (files + subdirectories) into `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Resolves `{asset, size?}` to a base64-PNG thumbnail reply, driving
/// [`request_thumbnail`] through the renderer's [`ThumbnailGpu`](saffron_assets::ThumbnailGpu)
/// seam. A cache hit returns the PNG; a cold miss replies `pending`. Shared by
/// `get-thumbnail` (128) + `view-asset` (512).
fn thumbnail_result(
    ctx: &mut EngineContext<'_>,
    params: &ThumbnailParams,
    default_size: u32,
) -> Result<ThumbnailResult> {
    let id = resolve_asset(ctx, &params.asset)?;
    let size = u32::try_from(params.size.unwrap_or(default_size as i32)).unwrap_or(default_size);
    let assets = &mut *ctx.assets;
    let mut reply = None;
    ctx.renderer.with_thumbnail_gpu(&mut |gpu| {
        reply = Some(request_thumbnail(assets, gpu, id, size));
    });
    let reply = reply
        .ok_or_else(|| Error::command("thumbnail seam unavailable"))?
        .map_err(|e| Error::command(e.to_string()))?;
    if reply.pending {
        return Ok(ThumbnailResult {
            id: WireUuid(id.value()),
            format: "png".to_owned(),
            width: 0,
            height: 0,
            base64: String::new(),
            pending: true,
        });
    }
    Ok(ThumbnailResult {
        id: WireUuid(id.value()),
        format: "png".to_owned(),
        width: i32::try_from(reply.width).unwrap_or(0),
        height: i32::try_from(reply.height).unwrap_or(0),
        base64: base64_encode(&reply.png),
        pending: false,
    })
}

/// Drops the asset preview and restores the authored edit state. A no-op when no preview
/// is alive.
fn leave_asset_preview(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() {
        return;
    }
    let was_active = ctx.preview_active_view;
    ctx.preview_scene = None;
    ctx.preview_asset = Uuid(0);
    ctx.preview_root_entity = Entity::NULL;
    ctx.preview_floor_entity = Entity::NULL;
    ctx.preview_bone_by_node.clear();
    ctx.preview_active_view = false;
    if was_active {
        ctx.camera = ctx.saved_camera;
        ctx.skeleton_overlay = ctx.saved_overlay;
        let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection)
        {
            ctx.saved_selection
        } else {
            Entity::NULL
        };
        ctx.saved_selection = Entity::NULL;
        ctx.set_selection(restore);
    }
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Parks the preview orbit and restores the authored fly-cam/overlay/selection so the scene
/// view shows the authored scene. A no-op unless the preview is the active view.
fn deactivate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || !ctx.preview_active_view {
        return;
    }
    ctx.parked_preview_camera = ctx.camera;
    ctx.camera = ctx.saved_camera;
    ctx.skeleton_overlay = ctx.saved_overlay;
    let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection) {
        ctx.saved_selection
    } else {
        Entity::NULL
    };
    ctx.set_selection(restore);
    ctx.preview_active_view = false;
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Re-stashes the authored view and restores the parked preview orbit + overlay + selected
/// root. A no-op unless a preview scene is alive but not currently active.
fn activate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || ctx.preview_active_view {
        return;
    }
    ctx.saved_camera = ctx.camera;
    ctx.saved_selection = ctx.selected;
    ctx.saved_overlay = ctx.skeleton_overlay;
    ctx.camera = ctx.parked_preview_camera;
    ctx.skeleton_overlay.show = true;
    ctx.skeleton_overlay.highlight_joint = -1;
    ctx.preview_active_view = true;
    let root = ctx.preview_root_entity;
    ctx.set_selection(root);
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// The flat parent-indexed bone tree for a skinned container's nodes (the
/// `get-asset-model` rig walk): the joints plus their ancestor chains bounded at the
/// skeleton root, with node indices preserved.
fn build_bone_tree(meta: &ContainerMetadata) -> Vec<BoneDto> {
    let node_count = meta.nodes.as_array().map_or(0, Vec::len);
    let nodes = meta.nodes.as_array();
    let parents: Vec<i32> = (0..node_count)
        .map(|i| {
            nodes
                .and_then(|n| n[i].get("parent"))
                .and_then(Value::as_i64)
                .map_or(-1, |v| v as i32)
        })
        .collect();
    let mut is_joint = vec![false; node_count];
    let skeleton_root = meta
        .skin
        .get("skeletonRoot")
        .and_then(Value::as_i64)
        .map_or(-1, |v| v as i32);
    if let Some(joints) = meta.skin.get("joints").and_then(Value::as_array) {
        for joint in joints {
            if let Some(index) = joint.as_i64()
                && index >= 0
                && (index as usize) < node_count
            {
                is_joint[index as usize] = true;
            }
        }
    }
    let mut in_rig = vec![false; node_count];
    if skeleton_root >= 0 && (skeleton_root as usize) < node_count {
        in_rig[skeleton_root as usize] = true;
    }
    for start in is_joint
        .iter()
        .enumerate()
        .filter_map(|(i, &joint)| joint.then_some(i as i32))
    {
        let mut node = start;
        while node >= 0 && (node as usize) < node_count && !in_rig[node as usize] {
            in_rig[node as usize] = true;
            if node == skeleton_root {
                break;
            }
            node = parents[node as usize];
        }
    }
    let mut bones = Vec::new();
    for i in 0..node_count {
        if !in_rig[i] {
            continue;
        }
        let parent = parents[i];
        let parent = if parent >= 0 && (parent as usize) < node_count && in_rig[parent as usize] {
            parent
        } else {
            -1
        };
        bones.push(BoneDto {
            index: i as i32,
            name: nodes
                .and_then(|n| n[i].get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent,
            joint: is_joint[i],
        });
    }
    bones
}

/// The animation sub-asset clips a container carries (the `get-asset-model` clip walk), each
/// with its real per-channel keyframe data loaded from the clip. No live forest here, so
/// channel labels are the raw glTF target names.
fn container_clips(
    assets: &mut saffron_assets::AssetServer,
    meta: &ContainerMetadata,
) -> Vec<AnimationClipDto> {
    let subs: Vec<(u64, String, f32)> = meta
        .sub_assets
        .iter()
        .filter(|sub| sub.asset_type == AssetType::Animation)
        .map(|sub| (sub.sub_id.value(), sub.name.clone(), sub.duration))
        .collect();
    subs.into_iter()
        .map(|(sub_id, name, duration)| {
            let channels = assets
                .load_anim_clip(saffron_core::Uuid(sub_id))
                .map(|clip| {
                    crate::commands_animation::channels_of(&clip, |track| track.target_name.clone())
                })
                .unwrap_or_default();
            AnimationClipDto {
                id: WireUuid(sub_id),
                name,
                duration,
                channels,
            }
        })
        .collect()
}

/// Registers the asset/project-domain commands in the frozen manifest order
/// (`get-project` … `quit`).
pub fn register_asset_commands(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, ProjectInfoDto>(
        "get-project",
        "get-project — active project metadata",
        |ctx, _params| Ok(project_dto(&current_project_info(ctx))),
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "project-status",
        "project-status — project-load phase + progress",
        |ctx, _params| Ok(project_status_dto(ctx)),
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "cancel-load",
        "cancel-load — abort the in-flight project load",
        |ctx, _params| {
            ctx.scene_edit.project_cancel = true;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<NewProjectParams, ProjectStatusDto>(
        "new-project",
        "new-project {name, displayName?, root?}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::New(NewProjectSpec {
                name: params.name.unwrap_or_default(),
                display_name: params.display_name.unwrap_or_default(),
                root: params.root.unwrap_or_default(),
            }));
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<CreateScriptParams, CreateScriptResult>(
        "create-script",
        "create-script {name} — boilerplate .lua under the project src/",
        |ctx, params| {
            if !ctx.scene_edit.project_ready() {
                return Err(Error::command("no project loaded"));
            }
            let path = create_project_script(&ctx.scene_edit.project_root, &params.name)
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(CreateScriptResult { path })
        },
    );

    reg.register::<PathParams, ProjectStatusDto>(
        "open-project",
        "open-project {path}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(params.path.clone()));
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<ImportModelParams, ImportModelResult>(
        "import-model",
        "import-model {path} — optional store attribution",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let bake = ctx
                .assets
                .import_model(&params.path, saffron_assets::ImportOptions::default())
                .map_err(|e| Error::command(e.to_string()))?;
            if let Some(attribution) = params.attribution {
                ctx.assets
                    .catalog
                    .set_attribution(bake.model_id, attribution_from_dto(attribution));
            }
            let name = ctx
                .assets
                .catalog
                .find(bake.model_id)
                .map(|entry| entry.name.clone())
                .unwrap_or_default();
            Ok(ImportModelResult {
                id: WireUuid(bake.model_id.value()),
                name,
                r#type: "model".to_owned(),
            })
        },
    );

    reg.register::<InstantiateModelParams, EntityRef>(
        "instantiate-model",
        "instantiate-model {asset} [name]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let entry_type = ctx.assets.catalog.find(id).map(|e| e.asset_type);
            let entry_name = ctx
                .assets
                .catalog
                .find(id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            if entry_type != Some(AssetType::Model) {
                return Err(Error::command(format!(
                    "asset {} is not a model",
                    id.value()
                )));
            }
            let name = match &params.name {
                Some(name) if !name.is_empty() => name.clone(),
                _ => entry_name,
            };
            let root = ctx
                .assets
                .instantiate_model(ctx.scene_edit.active_scene(), id, &name)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(root);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, root))
        },
    );

    reg.register::<AssetPlacementParams, AssetPlacementResult>(
        "asset-placement",
        "asset-placement {phase, asset?, u?, v?}",
        |ctx, params| match params.phase {
            AssetPlacementPhaseDto::Preview => preview_asset_placement(ctx, params),
            AssetPlacementPhaseDto::Commit => commit_asset_placement(ctx),
            AssetPlacementPhaseDto::Clear => {
                clear_placement_ghost(ctx);
                Ok(AssetPlacementResult {
                    active: false,
                    valid: true,
                    transform: None,
                    entity: None,
                    reason: None,
                })
            }
        },
    );

    reg.register::<ImportTextureParams, ImportTextureResult>(
        "import-texture",
        "import-texture {path} [colorspace] [role]",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            // A `role` hint (from an import connector, or manual) resolves both the stored role
            // and — absent an explicit `colorspace` override — the upload colorspace. No role and
            // no override leaves colorspace `None`, so the loader falls back to its ext heuristic.
            let role = params.role.as_deref().map(texture_role_from_hint);
            let colorspace = colorspace_from_str(params.colorspace.as_deref())
                .or_else(|| role.map(colorspace_for_role_explicit));
            let role = role.unwrap_or(TextureRole::Unknown);
            let assets = &mut *ctx.assets;
            let path = params.path.clone();
            let mut result = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = Some(assets.import_texture(gpu, &path, colorspace, role));
            });
            let id = result
                .ok_or_else(|| Error::command("upload seam unavailable"))?
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(ImportTextureResult {
                texture: WireUuid(id.value()),
            })
        },
    );

    reg.register::<EmptyParams, AssetList>(
        "list-assets",
        "list the project asset catalog",
        |ctx, _params| Ok(asset_list_dto(&ctx.assets.root, &ctx.assets.catalog)),
    );

    reg.register::<EmptyParams, ScanAssetsResult>("scan-assets", "scan-assets", |ctx, _params| {
        require_project_loaded(ctx)?;
        ctx.renderer.wait_gpu_idle();
        ctx.assets.clear_asset_caches();
        let delta = ctx
            .assets
            .scan_assets()
            .map_err(|e| Error::command(e.to_string()))?;
        ctx.assets.write_catalog_cache();
        Ok(ScanAssetsResult {
            added: i32::try_from(delta.added.len()).unwrap_or(i32::MAX),
            removed: i32::try_from(delta.removed.len()).unwrap_or(i32::MAX),
        })
    });

    reg.register::<ExtractSubAssetParams, AssetRef>(
        "extract-subasset",
        "extract-subasset {asset} {subAsset} [dest]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let model_id = resolve_asset(ctx, &params.asset)?;
            let dest = params.dest.clone().unwrap_or_default();
            let extracted =
                extract_sub_asset(ctx.assets, model_id, Uuid(params.sub_asset.0), &dest)
                    .map_err(|e| Error::command(e.to_string()))?;
            let name = ctx
                .assets
                .catalog
                .find(extracted)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            Ok(AssetRef {
                id: WireUuid(extracted.value()),
                name,
                folder: None,
            })
        },
    );

    reg.register::<ClearExtractionParams, AssetRef>(
        "clear-extraction",
        "clear-extraction {asset} {subAsset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let model_id = resolve_asset(ctx, &params.asset)?;
            let sub_id = Uuid(params.sub_asset.0);
            clear_extraction(ctx.assets, model_id, sub_id)
                .map_err(|e| Error::command(e.to_string()))?;
            let name = ctx
                .assets
                .catalog
                .find(sub_id)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            Ok(AssetRef {
                id: WireUuid(sub_id.value()),
                name,
                folder: None,
            })
        },
    );

    reg.register::<ReimportModelParams, ReimportModelResult>(
        "reimport-model",
        "reimport-model {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            ctx.renderer.wait_gpu_idle();
            let delta =
                reimport_model(ctx.assets, id).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(ReimportModelResult {
                updated: i32::try_from(delta.updated.len()).unwrap_or(i32::MAX),
                added: i32::try_from(delta.added.len()).unwrap_or(i32::MAX),
                removed_from_source: i32::try_from(delta.removed_from_source.len())
                    .unwrap_or(i32::MAX),
                skipped: delta.skipped,
            })
        },
    );

    reg.register::<ModelInfoParams, ModelInfoResult>(
        "model-info",
        "model-info {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            if ctx.assets.catalog.find(id).map(|e| e.asset_type) != Some(AssetType::Model) {
                return Err(Error::command(format!(
                    "asset {} is not a model",
                    id.value()
                )));
            }
            let path = ctx
                .assets
                .catalog
                .find(id)
                .map(|e| e.path.clone())
                .unwrap_or_default();
            let model = ctx
                .assets
                .load_model_asset(id)
                .ok_or_else(|| Error::command(format!("model {} is not loadable", id.value())))?;
            let meta = model.meta.clone();
            let total_bytes = std::fs::metadata(ctx.assets.root.join(&path))
                .map(|m| m.len())
                .unwrap_or(0);
            let mut material_count = 0;
            let mut sub_assets = Vec::new();
            for sub in &meta.sub_assets {
                if sub.asset_type == AssetType::Material {
                    material_count += 1;
                }
                let sub_row = AssetEntry {
                    id: sub.sub_id,
                    asset_type: sub.asset_type,
                    container: id,
                    ..AssetEntry::default()
                };
                let bytes = asset_bytes(ctx.assets, &sub_row);
                sub_assets.push(ModelSubAssetDto {
                    id: WireUuid(sub.sub_id.value()),
                    name: sub.name.clone(),
                    r#type: asset_type_name(sub.asset_type).to_owned(),
                    bytes,
                });
            }
            Ok(ModelInfoResult {
                id: WireUuid(id.value()),
                name: meta.name.clone(),
                source_path: meta.import.source_path.clone(),
                source_hash: meta.import.source_hash.clone(),
                material_count,
                has_skin: !meta.skin.is_null(),
                node_count: i32::try_from(meta.nodes.as_array().map_or(0, Vec::len))
                    .unwrap_or(i32::MAX),
                total_bytes,
                sub_assets,
            })
        },
    );

    reg.register::<AssetReferencesParams, AssetReferencesResult>(
        "asset-references",
        "asset-references {asset}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let graph = build_dependency_graph(scene, assets);
            Ok(AssetReferencesResult {
                referenced_by: graph
                    .referenced_by(id)
                    .iter()
                    .map(|u| u.value().to_string())
                    .collect(),
                references: graph
                    .references_of(id)
                    .iter()
                    .map(|u| u.value().to_string())
                    .collect(),
                footprint: graph.footprint(id),
            })
        },
    );

    reg.register::<GetAssetModelParams, AssetModelResult>(
        "get-asset-model",
        "get-asset-model {asset} — a model's capabilities + bone tree + clips, from its .smodel container",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let id = resolve_asset(ctx, &params.asset)?;
            let entry = ctx
                .assets
                .catalog
                .find(id)
                .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?;
            let container_id = if entry.asset_type == AssetType::Model {
                id
            } else {
                entry.container
            };
            if container_id.value() == 0 {
                return Err(Error::command(format!(
                    "asset {} is not part of a model container",
                    id.value()
                )));
            }
            let model = ctx.assets.load_model_asset(container_id).ok_or_else(|| {
                Error::command(format!("model {} is not loadable", container_id.value()))
            })?;
            let meta = model.meta.clone();
            let node_count = meta.nodes.as_array().map_or(0, Vec::len);
            let has_rig = !meta.skin.is_null();
            let bones = if has_rig { build_bone_tree(&meta) } else { Vec::new() };
            let clips = container_clips(ctx.assets, &meta);
            let mesh_count = meta
                .sub_assets
                .iter()
                .filter(|s| s.asset_type == AssetType::Mesh)
                .count();
            let material_count = meta
                .sub_assets
                .iter()
                .filter(|s| s.asset_type == AssetType::Material)
                .count();
            Ok(AssetModelResult {
                mesh: WireUuid(container_id.value()),
                name: meta.name.clone(),
                capabilities: AssetCapabilitiesDto {
                    mesh_count: i32::try_from(mesh_count).unwrap_or(i32::MAX),
                    material_count: i32::try_from(material_count).unwrap_or(i32::MAX),
                    node_count: i32::try_from(node_count).unwrap_or(i32::MAX),
                    has_rig,
                    bone_count: i32::try_from(bones.len()).unwrap_or(i32::MAX),
                    clip_count: i32::try_from(clips.len()).unwrap_or(i32::MAX),
                },
                bones,
                clips,
            })
        },
    );

    reg.register::<GetAssetModelParams, AssetPreviewResultWrap>(
        "enter-asset-preview",
        "enter-asset-preview {asset} — open any model in an isolated preview scene",
        enter_asset_preview,
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "exit-asset-preview",
        "exit-asset-preview — close the asset preview and restore the authored scene + camera",
        |ctx, _params| {
            if ctx.scene_edit.previewing() {
                ctx.renderer.set_active_view(ViewId::Scene);
                // Restore the authored tonemap exposure the HDRI preview's EV sweep may have moved.
                ctx.renderer.set_exposure(ctx.scene_edit.saved_exposure);
            }
            leave_asset_preview(ctx.scene_edit);
            Ok(play_state_result(ctx))
        },
    );

    reg.register::<SetActiveViewParams, SetActiveViewResult>(
        "set-active-view",
        "set-active-view {view} — switch the rendered view (scene | assetPreview)",
        |ctx, params| {
            let view = ViewId::from_wire(&params.view).ok_or_else(|| {
                Error::command(format!(
                    "unknown view '{}' (expected 'scene' or 'assetPreview')",
                    params.view
                ))
            })?;
            if view == ViewId::AssetPreview
                && ctx.renderer.view_desired_size(ViewId::AssetPreview).0 == 0
            {
                let (w, h) = (
                    ctx.renderer.viewport_width(),
                    ctx.renderer.viewport_height(),
                );
                let _ = ctx
                    .renderer
                    .set_view_desired_size(ViewId::AssetPreview, w, h);
            }
            ctx.renderer.set_active_view(view);
            if view == ViewId::AssetPreview {
                activate_preview_view(ctx.scene_edit);
            } else {
                // Leaving the preview for the scene: restore the authored exposure so an HDRI EV
                // sweep never bleeds into the scene tab (the preview workspace re-applies its EV on
                // return).
                if ctx.scene_edit.previewing() {
                    ctx.renderer.set_exposure(ctx.scene_edit.saved_exposure);
                }
                deactivate_preview_view(ctx.scene_edit);
            }
            Ok(SetActiveViewResult {
                view: view.wire().to_owned(),
            })
        },
    );

    reg.register::<CleanAssetsParams, CleanReport>(
        "clean-assets",
        "clean-assets [exclude...]",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let exclude: Vec<Uuid> = params
                .exclude
                .unwrap_or_default()
                .iter()
                .map(|id| Uuid(id.parse::<u64>().unwrap_or(0)))
                .collect();
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let data = analyze_clean(scene, assets, &exclude);
            Ok(CleanReport {
                reclaimable_bytes: data.reclaimable_bytes,
                candidates: data
                    .candidates
                    .into_iter()
                    .map(|c| CleanCandidateDto {
                        id: WireUuid(c.id.value()),
                        path: c.path,
                        category: c.category.name().to_owned(),
                        bytes: c.bytes,
                        reason: c.reason,
                    })
                    .collect(),
            })
        },
    );

    reg.register::<DeleteUnusedParams, DeleteUnusedResult>(
        "delete-unused",
        "delete-unused {ids...} {confirm}",
        |ctx, params| {
            require_project_loaded(ctx)?;
            let ids: Vec<Uuid> = params
                .ids
                .iter()
                .map(|id| Uuid(id.parse::<u64>().unwrap_or(0)))
                .collect();
            ctx.renderer.wait_gpu_idle();
            ctx.assets.clear_asset_caches();
            let confirm = params.confirm.unwrap_or(false);
            let assets = &mut *ctx.assets;
            let scene = ctx.scene_edit.active_scene();
            let deleted = delete_unused(assets, scene, &ids, confirm)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(DeleteUnusedResult {
                deleted: deleted.deleted,
                reclaimed_bytes: deleted.reclaimed_bytes,
            })
        },
    );

    reg.register::<RenameAssetParams, AssetRef>(
        "rename-asset",
        "rename-asset {id|name, newName}",
        |ctx, params| {
            let selector = selector_string(&params.asset);
            if selector.is_empty() || params.name.is_empty() {
                return Err(Error::command("usage: rename-asset {id|name} {newName}"));
            }
            let by_id = selector.parse::<u64>().unwrap_or(0);
            let mut renamed = None;
            for entry in &mut ctx.assets.catalog.entries {
                if entry.id.value() == by_id || entry.name == selector {
                    entry.name = params.name.clone();
                    renamed = Some(entry.id);
                    break;
                }
            }
            let Some(id) = renamed else {
                return Err(Error::command(format!("no asset '{selector}'")));
            };
            // Persist the new name to the durable sidecar so it survives a cold scan without a save.
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "rename-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(
                ctx.assets.catalog.find(id).expect("just renamed"),
            ))
        },
    );

    reg.register::<CreateAssetFolderParams, AssetList>(
        "create-asset-folder",
        "create-asset-folder {folder}",
        |ctx, params| {
            if !valid_folder_path(&params.folder) {
                return Err(Error::command(
                    "folder must be a non-empty path without empty segments",
                ));
            }
            if !has_folder(&ctx.assets.catalog, &params.folder) {
                ctx.assets.catalog.folders.push(params.folder.clone());
                ctx.scene_edit.scene_version += 1;
            }
            Ok(asset_list_dto(&ctx.assets.root, &ctx.assets.catalog))
        },
    );

    reg.register::<RenameAssetFolderParams, AssetList>(
        "rename-asset-folder",
        "rename-asset-folder {folder, name}",
        |ctx, params| {
            if !valid_folder_path(&params.name) {
                return Err(Error::command(
                    "folder path must be non-empty and cannot contain empty segments",
                ));
            }
            if !has_folder(&ctx.assets.catalog, &params.folder) {
                return Err(Error::command(format!(
                    "no asset folder '{}'",
                    params.folder
                )));
            }
            if params.folder == params.name {
                return Ok(asset_list_dto(&ctx.assets.root, &ctx.assets.catalog));
            }
            if is_folder_descendant(&params.name, &params.folder) {
                return Err(Error::command("asset folder cannot be moved inside itself"));
            }
            if has_folder(&ctx.assets.catalog, &params.name) {
                return Err(Error::command(format!(
                    "asset folder '{}' already exists",
                    params.name
                )));
            }
            for folder in &mut ctx.assets.catalog.folders {
                if *folder == params.folder || is_folder_descendant(folder, &params.folder) {
                    *folder = replace_folder_prefix(folder, &params.folder, &params.name);
                }
            }
            let mut touched = Vec::new();
            for entry in &mut ctx.assets.catalog.entries {
                if entry.folder == params.folder
                    || is_folder_descendant(&entry.folder, &params.folder)
                {
                    entry.folder =
                        replace_folder_prefix(&entry.folder, &params.folder, &params.name);
                    touched.push(entry.id);
                }
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "rename-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, &ctx.assets.catalog))
        },
    );

    reg.register::<DeleteAssetFolderParams, AssetList>(
        "delete-asset-folder",
        "delete-asset-folder {folder}",
        |ctx, params| {
            let mut removed = false;
            let mut folders = Vec::with_capacity(ctx.assets.catalog.folders.len());
            for folder in &ctx.assets.catalog.folders {
                if *folder == params.folder || is_folder_descendant(folder, &params.folder) {
                    removed = true;
                } else {
                    folders.push(folder.clone());
                }
            }
            if !removed {
                return Err(Error::command(format!(
                    "no asset folder '{}'",
                    params.folder
                )));
            }
            ctx.assets.catalog.folders = folders;
            let mut touched = Vec::new();
            for entry in &mut ctx.assets.catalog.entries {
                if entry.folder == params.folder
                    || is_folder_descendant(&entry.folder, &params.folder)
                {
                    entry.folder.clear();
                    touched.push(entry.id);
                }
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "delete-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, &ctx.assets.catalog))
        },
    );

    reg.register::<MoveAssetParams, AssetRef>(
        "move-asset",
        "move-asset {asset, folder?}",
        |ctx, params| {
            let index = resolve_asset_index(ctx, &params.asset)?;
            let folder = params.folder.clone().unwrap_or_default();
            if !folder.is_empty() && !has_folder(&ctx.assets.catalog, &folder) {
                return Err(Error::command(format!("no asset folder '{folder}'")));
            }
            ctx.assets.catalog.entries[index].folder = folder;
            let id = ctx.assets.catalog.entries[index].id;
            ctx.scene_edit.scene_version += 1;
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "move-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(&ctx.assets.catalog.entries[index]))
        },
    );

    reg.register::<AssetUsagesParams, AssetUsagesResult>(
        "asset-usages",
        "asset-usages {asset}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.asset)?;
            let usages = collect_asset_usages(ctx.scene_edit.active_scene(), id);
            Ok(AssetUsagesResult { usages })
        },
    );

    reg.register::<AssetMetadataParams, AssetMetadataDto>(
        "probe-asset",
        "probe-asset {asset}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.asset)?;
            let entry = ctx
                .assets
                .catalog
                .find(id)
                .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?
                .clone();
            let abs = ctx.assets.root.join(&entry.path);
            let size_bytes = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            let created_at = asset_created_at(&ctx.assets.root, &entry.path);
            let mut vertex_count = None;
            let mut triangle_count = None;
            if entry.asset_type == AssetType::Mesh
                && let Ok(counts) = ctx.assets.mesh_counts_for_asset(&entry)
            {
                vertex_count = Some(counts.vertex_count);
                triangle_count = Some(counts.index_count / 3);
            }
            Ok(AssetMetadataDto {
                id: WireUuid(entry.id.value()),
                name: entry.name.clone(),
                r#type: asset_type_dto(entry.asset_type),
                path: entry.path.clone(),
                folder: optional_folder(&entry.folder),
                size_bytes,
                vertex_count,
                triangle_count,
                created_at,
            })
        },
    );

    reg.register::<DeleteAssetParams, DeleteAssetResult>(
        "delete-asset",
        "delete-asset {asset}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let index = resolve_asset_index(ctx, &params.asset)?;
            let entry = ctx.assets.catalog.entries[index].clone();
            let cleared = clear_asset_usages(&mut ctx.scene_edit.scene, entry.id);
            ctx.assets.catalog.entries.remove(index);
            rebuild_asset_index(&mut ctx.assets.catalog);
            ctx.assets.mesh_by_uuid.remove(&entry.id.value());
            ctx.assets.texture_by_uuid.remove(&entry.id.value());
            // The deleted row (and any instance that referenced it as a parent) is now stale.
            ctx.assets.invalidate_material_caches();
            let file_deleted = if entry.path.is_empty() {
                false
            } else {
                // Drop the co-located durable sidecar too (unless it's an embedded sub-asset,
                // whose `.smeta` is the shared model's — not ours to delete).
                if entry.container.value() == 0 {
                    let _ =
                        std::fs::remove_file(ctx.assets.root.join(format!("{}.smeta", entry.path)));
                }
                std::fs::remove_file(ctx.assets.root.join(&entry.path)).is_ok()
            };
            // The thumbnail cache is content-addressed and shared across assets/projects, so
            // a delete leaves its PNG for the eviction sweep — another asset may share it.
            ctx.scene_edit.scene_version += 1;
            Ok(DeleteAssetResult {
                id: WireUuid(entry.id.value()),
                name: entry.name.clone(),
                cleared,
                file_deleted,
            })
        },
    );

    reg.register::<AssignAssetParams, AssignAssetResult>(
        "assign-asset",
        "assign-asset {entity, slot:mesh|albedo|metallic-roughness, id|name}",
        |ctx, params| {
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let entity = resolve_entity(ctx, &params.entity)?;
            let selector = selector_string(&params.asset);
            let clearing = selector == "0"
                || selector.is_empty()
                || params.asset.as_u64() == Some(0)
                || params.asset.as_i64() == Some(0);
            let (assign_id, assign_name) = if clearing {
                (Uuid(0), String::new())
            } else if let Some(builtin) =
                BuiltinMesh::from_reserved_id(Uuid(selector_id(&params.asset)))
            {
                // A built-in primitive is referenced by its reserved id, not a catalog row —
                // its display name comes from the enum, not `catalog.find`.
                (builtin.reserved_id(), builtin.display_name().to_owned())
            } else {
                let id = resolve_asset(ctx, &params.asset)?;
                let name = ctx
                    .assets
                    .catalog
                    .find(id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                (id, name)
            };
            let scene = ctx.scene_edit.active_scene();
            match params.slot {
                AssetSlotDto::Mesh => {
                    if !scene.has_component::<Mesh>(entity) {
                        let _ = scene.add_component(entity, Mesh::default());
                    }
                    let _ = scene.with_component_mut::<Mesh, _>(entity, |m| m.mesh = assign_id);
                }
                // Texture slots write a per-object override on the entity's material slot 0.
                // The packed ORM means `metallic-roughness` and `occlusion` share `ormTexture`.
                AssetSlotDto::Albedo => {
                    set_slot0_texture_override(scene, entity, "albedoTexture", assign_id);
                }
                AssetSlotDto::MetallicRoughness => {
                    set_slot0_texture_override(scene, entity, "ormTexture", assign_id);
                }
                AssetSlotDto::Normal => {
                    set_slot0_texture_override(scene, entity, "normalTexture", assign_id);
                }
                AssetSlotDto::Occlusion => {
                    set_slot0_texture_override(scene, entity, "ormTexture", assign_id);
                }
                AssetSlotDto::Emissive => {
                    set_slot0_texture_override(scene, entity, "emissiveTexture", assign_id);
                }
                AssetSlotDto::Height => {
                    set_slot0_texture_override(scene, entity, "heightTexture", assign_id);
                }
            }
            ctx.scene_edit.scene_version += 1;
            Ok(AssignAssetResult {
                id: WireUuid(assign_id.value()),
                name: assign_name,
                slot: params.slot,
            })
        },
    );

    reg.register::<MaterialCreateParams, MaterialCreateResult>(
        "material-create",
        "material-create {name} [from-entity]",
        |ctx, params| {
            let asset = default_material_asset();
            let name = if params.name.is_empty() {
                "Material".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &asset, &name, "")
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialAssignParams, MaterialAssignResult>(
        "material-assign",
        "material-assign {entity, material:id|name}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let selector = selector_string(&params.material);
            let clearing =
                selector == "0" || selector.is_empty() || params.material.as_u64() == Some(0);
            let mat_id = if clearing {
                Uuid(0)
            } else {
                resolve_asset(ctx, &params.material)?
            };
            let scene = ctx.scene_edit.active_scene();
            // Assign to every mesh-bearing entity in the model's forest — the renderer reads
            // the material off the entity that carries the mesh, which on a multi-node model is
            // a child of the resolved container, not the container itself. A leaf selection
            // resolves to just itself; a non-mesh selection still takes the component directly.
            let mut targets = scene.model_mesh_entities(entity);
            if targets.is_empty() {
                targets.push(entity);
            }
            for target in targets {
                ensure_material_slot(scene, target);
                let _ = scene.with_component_mut::<MaterialSet, _>(target, |set| {
                    if set.slots.is_empty() {
                        set.slots.push(MaterialSlot::default());
                    }
                    for slot in &mut set.slots {
                        slot.material = mat_id;
                    }
                });
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialAssignResult {
                material: WireUuid(mat_id.value()),
            })
        },
    );

    reg.register::<MaterialImportParams, MaterialImportResultDto>(
        "material-import",
        "material-import {path} [name]",
        |ctx, params| {
            let assets = &mut *ctx.assets;
            let path = params.path.clone();
            let name = params.name.clone();
            let mut result = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = Some(import_material_folder(assets, gpu, &path, &name));
            });
            let imported = result
                .ok_or_else(|| Error::command("upload seam unavailable"))?
                .map_err(|e| Error::command(e.to_string()))?;
            if let Some(attribution) = params.attribution {
                ctx.assets
                    .catalog
                    .set_attribution(imported.material, attribution_from_dto(attribution));
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialImportResultDto {
                id: WireUuid(imported.material.value()),
                roles: imported.roles,
            })
        },
    );

    reg.register::<EmptyParams, MaterialListResult>(
        "material-list",
        "material-list",
        |ctx, _params| {
            let materials = ctx
                .assets
                .catalog
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| MaterialRefDto {
                    id: WireUuid(e.id.value()),
                    name: e.name.clone(),
                    folder: e.folder.clone(),
                })
                .collect();
            Ok(MaterialListResult { materials })
        },
    );

    reg.register::<MaterialGetParams, MaterialGetResult>(
        "material-get",
        "material-get {id|name}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let m = load_catalog_material_asset(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            let graph = load_catalog_material_asset_raw(ctx.assets, id)
                .ok()
                .filter(|raw| raw.graph.is_object())
                .map_or_else(|| json!({}), |raw| raw.graph);
            Ok(MaterialGetResult {
                id: WireUuid(id.value()),
                blend: m.blend.clone(),
                unlit: m.unlit,
                base_color: vec4(m.base_color),
                metallic: m.metallic,
                roughness: m.roughness,
                emissive: vec3(m.emissive),
                emissive_strength: m.emissive_strength,
                albedo_texture: WireUuid(m.albedo_texture.value()),
                orm_texture: WireUuid(m.orm_texture.value()),
                normal_texture: WireUuid(m.normal_texture.value()),
                emissive_texture: WireUuid(m.emissive_texture.value()),
                height_texture: WireUuid(m.height_texture.value()),
                graph,
            })
        },
    );

    reg.register::<MaterialSchemaParams, MaterialSchemaResult>(
        "material-schema",
        "material-schema {id|name} — the material's exposed override parameters",
        |ctx, params| {
            // Validate the material resolves; the exposed set is the fixed übershader's for
            // now (the same list a `MaterialSet` slot's overrides validate against).
            resolve_asset(ctx, &params.material)?;
            let params = pbr_exposed_parameters()
                .into_iter()
                .map(|p| ExposedParamDto {
                    name: p.name.to_owned(),
                    kind: p.kind.as_wire().to_owned(),
                    default: p.default,
                })
                .collect();
            Ok(MaterialSchemaResult { params })
        },
    );

    reg.register::<MaterialUpdateParams, MaterialUpdateResult>(
        "material-update",
        "material-update {id} [baseColor metallic roughness emissive emissiveStrength]",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m =
                load_material_asset(ctx.assets, id).map_err(|e| Error::command(e.to_string()))?;
            if let Some(base) = params.base_color {
                m.base_color = from_vec4(base);
            }
            if let Some(metallic) = params.metallic {
                m.metallic = metallic;
            }
            if let Some(roughness) = params.roughness {
                m.roughness = roughness;
            }
            if let Some(emissive) = params.emissive {
                m.emissive = from_vec3(emissive);
            }
            if let Some(strength) = params.emissive_strength {
                m.emissive_strength = strength;
            }
            if let Some(normal_strength) = params.normal_strength {
                m.normal_strength = normal_strength;
            }
            if let Some(tex) = params.albedo_texture {
                m.albedo_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.orm_texture {
                m.orm_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.normal_texture {
                m.normal_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.emissive_texture {
                m.emissive_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.height_texture {
                m.height_texture = Uuid(tex.0);
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialUpdateResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<PreviewRenderParams, PreviewRenderResult>(
        "preview-render",
        "preview-render {material} [size]",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let loaded = load_catalog_material_asset(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            let size = params.size.unwrap_or(256);
            // A non-foldable graph (procedural nodes) renders through a codegen'd preview
            // shader; a foldable graph already folded into `loaded`, so the default studio
            // preview shows it. Resolved here because the closure below borrows `assets`.
            let codegen_spv = preview_codegen_spv(ctx.assets, id);
            let assets = &mut *ctx.assets;
            let mut png = None;
            ctx.renderer.with_thumbnail_gpu(&mut |gpu| {
                let sm = assets.resolve_material_asset(gpu, &loaded);
                png = Some((|| {
                    let tex = gpu.render_material_preview(&sm, size, codegen_spv.as_deref())?;
                    gpu.encode_texture_thumbnail_png(&tex, size, PngTransfer::Clamp)
                })());
            });
            let png = png
                .ok_or_else(|| Error::command("thumbnail seam unavailable"))?
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(PreviewRenderResult {
                png: base64_encode(&png.bytes),
            })
        },
    );

    reg.register::<MaterialSetGraphParams, MaterialSetGraphResult>(
        "material-set-graph",
        "material-set-graph {material, graph}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m =
                load_material_asset(ctx.assets, id).map_err(|e| Error::command(e.to_string()))?;
            m.graph = params.graph.clone();
            let mut folded = m.clone();
            let foldable = lower_graph_to_params(&m.graph, &mut folded);
            if foldable {
                m = folded;
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            if !foldable {
                let _ = ctx.assets.compile_material_mesh_shader(&m.graph, id);
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetGraphResult {
                id: WireUuid(id.value()),
                foldable,
            })
        },
    );

    reg.register::<MaterialCreateInstanceParams, MaterialCreateResult>(
        "material-create-instance",
        "material-create-instance {parent} [name]",
        |ctx, params| {
            let parent = resolve_asset(ctx, &params.parent)?;
            let mut child = default_material_asset();
            child.parent = parent;
            let name = if params.name.is_empty() {
                "Instance".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &child, &name, "")
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialSetOverrideParams, MaterialSetOverrideResult>(
        "material-set-override",
        "material-set-override {material, field, value}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            // The override key must be one of the material's exposed parameters, and the
            // value must be well-typed for its kind — the schema is the single gate.
            let param = exposed_parameter(&params.field).ok_or_else(|| {
                Error::command(format!("unknown material parameter '{}'", params.field))
            })?;
            if !param.kind.accepts(&params.value) {
                return Err(Error::command(format!(
                    "material parameter '{}' expects a {} value",
                    params.field,
                    param.kind.as_wire()
                )));
            }
            let mut m = load_material_asset_raw(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            if !m.overrides.is_object() {
                m.overrides = json!({});
            }
            if let Some(map) = m.overrides.as_object_mut() {
                map.insert(params.field.clone(), params.value.clone());
            }
            update_material_asset(ctx.assets, id, &m).map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetOverrideResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<MaterialCompileParams, MaterialCompileResult>(
        "material-compile-graph",
        "material-compile-graph {material}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let raw = load_material_asset_raw(ctx.assets, id)
                .map_err(|e| Error::command(e.to_string()))?;
            if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                return Err(Error::command("material has no node graph to compile"));
            }
            ctx.assets
                .compile_material_graph(&raw.graph, id)
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(MaterialCompileResult {
                id: WireUuid(id.value()),
                ok: true,
            })
        },
    );

    reg.register::<EmptyParams, MaterialCookResult>(
        "material-cook",
        "material-cook",
        |ctx, _params| {
            let material_ids: Vec<Uuid> = ctx
                .assets
                .catalog
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| e.id)
                .collect();
            let mut compiled = 0u32;
            let mut failed = 0u32;
            for id in material_ids {
                let Ok(raw) = load_material_asset_raw(ctx.assets, id) else {
                    continue;
                };
                if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                    continue;
                }
                let mut probe = raw.clone();
                if lower_graph_to_params(&raw.graph, &mut probe) {
                    continue;
                }
                if ctx
                    .assets
                    .compile_material_mesh_shader(&raw.graph, id)
                    .is_ok()
                {
                    compiled += 1;
                } else {
                    failed += 1;
                }
            }
            // The recompiled `_mesh.spv` artifacts change what `codegen_shader_for` resolves.
            ctx.assets.invalidate_material_caches();
            Ok(MaterialCookResult { compiled, failed })
        },
    );

    reg.register::<ExportAppParams, ExportAppResult>(
        "export-app",
        "export-app {outputDir, app} — cook the project into a standalone app folder",
        |ctx, params| export_app(ctx, &params),
    );

    reg.register::<PathParams, PathResult>("save-scene", "save-scene {path}", |ctx, params| {
        if params.path.is_empty() {
            return Err(Error::command("missing 'path'"));
        }
        let editor = &mut *ctx.scene_edit;
        editor
            .scene
            .write_scene(&editor.registry, &params.path)
            .map_err(|e| Error::command(e.to_string()))?;
        editor.scene_path = params.path.clone();
        Ok(PathResult { path: params.path })
    });

    reg.register::<PathParams, PathResult>("load-scene", "load-scene {path}", |ctx, params| {
        if ctx.scene_edit.play_state != PlayState::Edit {
            return Err(Error::command("stop play first"));
        }
        if params.path.is_empty() {
            return Err(Error::command("missing 'path'"));
        }
        {
            let editor = &mut *ctx.scene_edit;
            editor
                .scene
                .read_scene(&editor.registry, &params.path)
                .map_err(|e| Error::command(e.to_string()))?;
        }
        ctx.scene_edit.scene_path = params.path.clone();
        ctx.scene_edit.scene_version += 1;
        ctx.scene_edit.set_selection(Entity::NULL);
        Ok(PathResult { path: params.path })
    });

    reg.register::<OptionalPathParams, ProjectInfoDto>(
        "save-project",
        "save-project {path} — assets catalog + scene in one file",
        |ctx, params| {
            let mut path = params.path.clone().unwrap_or_default();
            let mut project = current_project_info(ctx);
            if path.is_empty() {
                path = project.path.clone();
            }
            if path.is_empty() {
                return Err(Error::command("no active project path"));
            }
            if !project.loaded {
                let fs_path = Path::new(&path);
                project.loaded = true;
                project.path = path.clone();
                let parent = fs_path.parent();
                project.root = match parent {
                    Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
                    _ => ".".to_owned(),
                };
                let dir_name = parent
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                project.name = if valid_project_name(&dir_name) {
                    dir_name
                } else {
                    "project".to_owned()
                };
                project.display_name = default_display_name(&project.name);
            }
            let sidecar = saffron_assets::ProjectSidecar {
                editor_camera: ctx.scene_edit.camera.to_json(),
                debug_overlays: saffron_sceneedit::debug_overlays_to_json(
                    &ctx.scene_edit.debug_overlays,
                ),
                stores: ctx.scene_edit.stores.clone(),
            };
            let host = RendererProjectHost {
                renderer: ctx.renderer,
            };
            ctx.assets
                .save_project(
                    &host,
                    &ctx.scene_edit.registry,
                    &mut ctx.scene_edit.scene,
                    &project,
                    &path,
                    &sidecar,
                )
                .map_err(|e| Error::command(e.to_string()))?;
            project.path = path;
            apply_project_info(ctx, &project);
            Ok(project_dto(&project))
        },
    );

    reg.register::<OptionalPathParams, ProjectStatusDto>(
        "load-project",
        "load-project {path} — assets catalog + scene",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            let path = params
                .path
                .clone()
                .unwrap_or_else(|| "project.json".to_owned());
            load_project_into(ctx, &path);
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<EmptyParams, ProjectStatusDto>(
        "reload-project",
        "reload-project — close and re-open the active project",
        |ctx, _params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("stop play first"));
            }
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            require_project_loaded(ctx)?;
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Reload);
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );

    reg.register::<EmptyParams, ProjectStoresDto>(
        "get-stores",
        "get-stores — the project's enabled asset-store connectors",
        |ctx, _params| Ok(stores_dto_from_value(&ctx.scene_edit.stores)),
    );

    reg.register::<ProjectStoresDto, ProjectStoresDto>(
        "set-stores",
        "set-stores {enabled} — set the project's enabled asset-store connectors",
        |ctx, params| {
            require_project_loaded(ctx)?;
            ctx.scene_edit.stores =
                serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
            Ok(params)
        },
    );

    reg.register::<ScreenshotParams, ScreenshotResult>(
        "screenshot",
        "screenshot {target:viewport|window, path}",
        |ctx, params| {
            let target = params.target.unwrap_or(ScreenshotTargetDto::Viewport);
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            match target {
                ScreenshotTargetDto::Viewport => {
                    ctx.renderer
                        .capture_viewport(Path::new(&params.path))
                        .map_err(Error::Command)?;
                    Ok(ScreenshotResult {
                        target,
                        path: params.path,
                        pending: false,
                    })
                }
                ScreenshotTargetDto::Window => {
                    ctx.renderer
                        .request_window_capture(Path::new(&params.path))
                        .map_err(Error::Command)?;
                    Ok(ScreenshotResult {
                        target,
                        path: params.path,
                        pending: true,
                    })
                }
            }
        },
    );

    reg.register::<ThumbnailParams, ThumbnailResult>(
        "get-thumbnail",
        "get-thumbnail {asset:id|name, size=128} — base64 PNG preview",
        |ctx, params| thumbnail_result(ctx, &params, 128),
    );

    reg.register::<ThumbnailParams, ThumbnailResult>(
        "view-asset",
        "view-asset {asset:id|name, size=512} — larger base64 PNG preview",
        |ctx, params| thumbnail_result(ctx, &params, 512),
    );

    reg.register::<ThumbnailCacheParams, ThumbnailCacheResult>(
        "thumbnail-cache",
        "thumbnail-cache {action: stats|clear} — inspect or empty the app-level cache",
        |ctx, params| {
            if params.action == "clear" {
                let removed = ctx.assets.clear_thumbnail_cache_dir();
                return Ok(ThumbnailCacheResult {
                    entries: i32::try_from(removed.entries).unwrap_or(i32::MAX),
                    bytes: i64::try_from(removed.bytes).unwrap_or(i64::MAX),
                });
            }
            if params.action == "stats" || params.action.is_empty() {
                let stats = ctx.assets.thumbnail_cache_stats();
                return Ok(ThumbnailCacheResult {
                    entries: i32::try_from(stats.entries).unwrap_or(i32::MAX),
                    bytes: i64::try_from(stats.bytes).unwrap_or(i64::MAX),
                });
            }
            Err(Error::command(format!(
                "unknown action '{}' (stats|clear)",
                params.action
            )))
        },
    );

    reg.register::<EmptyParams, QuitResult>("quit", "close the running app", |ctx, _params| {
        ctx.window.request_close();
        Ok(QuitResult { quitting: true })
    });
}

/// The codegen `_preview.spv` path for `preview-render`, or `None` for the default studio
/// preview. A material whose raw graph is a non-empty object that *does not* fold into the
/// flat params (a procedural node) gets a freshly compiled preview shader; a foldable or
/// graph-less material renders through the cached default pipeline. A compile failure
/// degrades to `None` (the default preview still renders the folded params), never an error.
fn preview_codegen_spv(assets: &AssetServer, id: Uuid) -> Option<PathBuf> {
    let raw = load_material_asset_raw(assets, id).ok()?;
    let non_empty_graph = raw.graph.as_object().is_some_and(|obj| !obj.is_empty());
    if !non_empty_graph {
        return None;
    }
    let mut probe = raw.clone();
    if lower_graph_to_params(&raw.graph, &mut probe) {
        return None;
    }
    assets.compile_material_preview_shader(&raw.graph, id).ok()
}

/// Ensures the entity carries a [`MaterialSet`] with at least one slot before a slot write.
fn ensure_material_slot(scene: &mut Scene, entity: Entity) {
    if scene.has_component::<MaterialSet>(entity) {
        let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
            if set.slots.is_empty() {
                set.slots.push(MaterialSlot::default());
            }
        });
    } else {
        let _ = scene.add_component(
            entity,
            MaterialSet {
                slots: vec![MaterialSlot::default()],
            },
        );
    }
}

/// Sets (or clears, when `tex_id == 0`) one texture override on the entity's material slot 0,
/// attaching a default `MaterialSet` slot first. The `assign-asset` texture path — a
/// per-object override layered over the slot's referenced `.smat`.
fn set_slot0_texture_override(scene: &mut Scene, entity: Entity, key: &str, tex_id: Uuid) {
    ensure_material_slot(scene, entity);
    let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
        let Some(slot) = set.slots.first_mut() else {
            return;
        };
        if let Some(map) = slot.overrides.as_object_mut() {
            if tex_id.value() == 0 {
                map.remove(key);
            } else {
                map.insert(key.to_owned(), json!(tex_id.value().to_string()));
            }
        }
    });
}

/// The shared `load-project` body: seed the loader inbox with an `Open` request and set the
/// `Loading` phase, then return the current identity as an immediate ack. The non-blocking loader
/// (`ProjectLoader::advance`, driven from the host each frame) runs the read/parse/scan off-thread
/// and installs on the main thread, so the control drain never blocks.
fn load_project_into(ctx: &mut EngineContext<'_>, path: &str) {
    ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(path.to_owned()));
    ctx.scene_edit.project_phase = ProjectPhase::Loading;
}

/// The `enter-asset-preview` body: build an isolated preview scene, commit it, furnish it
/// (floor / key light / procedural sky / framed fly-cam), and route the renderer + active
/// view to it.
fn enter_asset_preview(
    ctx: &mut EngineContext<'_>,
    params: GetAssetModelParams,
) -> Result<AssetPreviewResultWrap> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        return Err(Error::command("stop play first"));
    }
    // A native built-in primitive has no catalog row or model container — preview it on its
    // own geometry rather than resolving + instantiating a model.
    if let Some(builtin) = BuiltinMesh::from_reserved_id(Uuid(selector_id(&params.asset))) {
        return enter_builtin_preview(ctx, builtin);
    }
    let id = resolve_asset(ctx, &params.asset)?;
    let entry = ctx
        .assets
        .catalog
        .find(id)
        .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?;
    let entry_type = entry.asset_type;
    let entry_role = entry.role;
    let entry_hdr = entry.hdr;
    // A standalone texture previews on the isolated sphere, not as a model: an HDRI lights + backs a
    // three-ball environment rig; every other role is its map on one lit sphere (role picks the slot).
    if entry_type == AssetType::Texture {
        if entry_role == TextureRole::Hdri || entry_hdr {
            return enter_hdri_preview(ctx, id);
        }
        return enter_texture_preview(ctx, id, entry_role);
    }
    // A material (`.smat`) previews as itself on the studio sphere — the same subject the
    // material-graph editor's live pane drives.
    if entry_type == AssetType::Material {
        return enter_material_preview(ctx, id);
    }
    let container_id = if entry_type == AssetType::Model {
        id
    } else {
        entry.container
    };
    if container_id.value() == 0 {
        return Err(Error::command(format!(
            "asset {} is not part of a model container",
            id.value()
        )));
    }
    let model = ctx
        .assets
        .load_model_asset(container_id)
        .ok_or_else(|| Error::command(format!("model {} is not loadable", container_id.value())))?;
    let meta = model.meta.clone();

    // Build the preview scene locally so a failed swap stays on the prior model; commit only
    // once the model spawned a renderable mesh. Instantiation references mesh ids by uuid —
    // the GPU upload happens lazily at render — so no upload seam is needed here.
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = ctx
        .assets
        .instantiate_model(&mut preview, container_id, &meta.name)
        .map_err(|e| Error::command(e.to_string()))?;

    // A model is renderable when any entity in its forest carries a mesh — the meshes of a
    // multi-node forest ride child nodes, so probing only the resolved root rejects them.
    if !preview.model_has_renderable(root) {
        return Err(Error::command(format!(
            "model '{}' has no renderable mesh — re-import the asset",
            meta.name
        )));
    }
    let rig_entity = preview.model_rig_entity(root);
    // The animation authority (SkinnedMesh- or AnimationPlayer-bearing entity) the clip drives.
    let animatable = preview.animatable_descendant(root);

    // Open-from-clip: that clip becomes the active clip; the model opens paused at rest.
    if entry_type == AssetType::Animation && preview.has_component::<AnimationPlayer>(animatable) {
        let _ = preview.with_component_mut::<AnimationPlayer, _>(animatable, |player| {
            player.clip = id;
            player.time = 0.0;
            player.playing = false;
            player.preview_in_edit = false;
        });
    }

    let root_uuid = preview
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    let mut bone_by_node = Vec::new();
    let mut bones = Vec::new();
    if let Some(rig_entity) = rig_entity {
        let bone_uuids = preview
            .with_component::<SkinnedMesh, _>(rig_entity, |skin| skin.bones.clone())
            .unwrap_or_default();
        let joint_nodes: Vec<i32> = meta
            .skin
            .get("joints")
            .and_then(Value::as_array)
            .map(|joints| {
                joints
                    .iter()
                    .filter_map(|j| j.as_i64().map(|v| v as i32))
                    .collect()
            })
            .unwrap_or_default();
        let node_count = meta.nodes.as_array().map_or(0, Vec::len);
        bone_by_node = vec![Uuid(0); node_count];
        let joint_count = joint_nodes.len().min(bone_uuids.len());
        for k in 0..joint_count {
            let node_idx = joint_nodes[k];
            let uuid = bone_uuids[k];
            if node_idx >= 0 && (node_idx as usize) < node_count && uuid.value() != 0 {
                bone_by_node[node_idx as usize] = uuid;
                bones.push(saffron_protocol::BoneEntityDto {
                    index: node_idx,
                    entity: WireUuid(uuid.value()),
                });
            }
        }
    }

    // Commit: stash camera + selection + overlay only on a fresh enter (a swap keeps the
    // authored stash). A fresh enter makes the preview the active view.
    if !ctx.scene_edit.previewing() {
        ctx.scene_edit.saved_camera = ctx.scene_edit.camera;
        ctx.scene_edit.saved_selection = ctx.scene_edit.selected;
        ctx.scene_edit.saved_overlay = ctx.scene_edit.skeleton_overlay;
        ctx.scene_edit.saved_exposure = ctx.renderer.exposure_ev();
        ctx.scene_edit.preview_active_view = true;
        let (w, h) = (
            ctx.renderer.viewport_width(),
            ctx.renderer.viewport_height(),
        );
        let _ = ctx
            .renderer
            .set_view_desired_size(ViewId::AssetPreview, w, h);
        ctx.renderer.set_active_view(ViewId::AssetPreview);
    }
    ctx.scene_edit.preview_scene = Some(preview);
    ctx.scene_edit.preview_asset = container_id;
    ctx.scene_edit.preview_root_entity = root;
    ctx.scene_edit.preview_bone_by_node = bone_by_node;
    ctx.scene_edit.preview_floor_entity = Entity::NULL;
    ctx.scene_edit.skeleton_overlay.show = true;
    ctx.scene_edit.skeleton_overlay.highlight_joint = -1;
    let framing = furnish_preview_scene(ctx, root, PreviewEnv::Procedural);
    ctx.scene_edit.set_selection(root);
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.animation_version += 1;
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones,
            target: vec3(framing.target),
            distance: framing.distance,
        },
    ))
}

/// Builds an isolated preview scene for a native built-in primitive (no catalog row, no
/// model container): a single entity carrying the reserved-id mesh + a default material,
/// framed on the shared asset-preview view. Mirrors the commit tail of
/// [`enter_asset_preview`] for a container-less, rig-less subject.
fn enter_builtin_preview(
    ctx: &mut EngineContext<'_>,
    builtin: BuiltinMesh,
) -> Result<AssetPreviewResultWrap> {
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = preview.create_entity(builtin.display_name());
    let _ = preview.add_component(
        root,
        Mesh {
            mesh: builtin.reserved_id(),
        },
    );
    let _ = preview.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot::default()],
        },
    );
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        builtin.reserved_id(),
        PreviewEnv::Procedural,
    ))
}

/// The `enter-asset-preview` branch for a standalone texture: shade a preview sphere with an
/// ephemeral single-slot material carrying the texture in the slot its role feeds (albedo lit,
/// normal bumped, roughness/metallic/AO/ORM through the packed slot, height parallaxed, emissive
/// glowing). The material is seeded into `material_by_uuid` under the reserved
/// [`PREVIEW_MATERIAL_ID`] — no catalog row, rebuilt on each enter — so the scene resolver shades
/// the sphere with it. HDRI is filtered out upstream (it is the environment preview).
fn enter_texture_preview(
    ctx: &mut EngineContext<'_>,
    tid: Uuid,
    role: TextureRole,
) -> Result<AssetPreviewResultWrap> {
    let material = preview_material_for_texture(role, tid);
    ctx.assets.material_by_uuid.insert(
        PREVIEW_MATERIAL_ID.value(),
        Some(std::sync::Arc::new(material)),
    );

    let name = ctx
        .assets
        .catalog
        .find(tid)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "Texture".to_owned());
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = preview.create_entity(&name);
    // The dense sphere so a height map's vertex displacement reads as a true silhouette.
    let _ = preview.add_component(
        root,
        Mesh {
            mesh: PREVIEW_DISPLACE_SPHERE_MESH_ID,
        },
    );
    let _ = preview.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot {
                material: PREVIEW_MATERIAL_ID,
                ..MaterialSlot::default()
            }],
        },
    );
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        tid,
        PreviewEnv::Procedural,
    ))
}

/// The `enter-asset-preview` branch for an HDRI (an `.hdr`/`.exr` texture): the imported equirect
/// becomes both the visible backdrop and the IBL source (`SkyMode::Texture`), and three PBR balls —
/// chrome, diffuse grey, colored satin — read the environment's reflections / irradiance / color
/// response (the AmbientCG-style env rig). The balls differ only by per-slot overrides on the
/// default material, so no ephemeral material assets are needed. The center ball parents the other
/// two so the framing bounds cover all three.
fn enter_hdri_preview(
    ctx: &mut EngineContext<'_>,
    hdri_id: Uuid,
) -> Result<AssetPreviewResultWrap> {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    // (x offset, name, material overrides): chrome mirror / diffuse grey / colored satin.
    let balls = [
        (
            -2.3_f32,
            "Chrome",
            serde_json::json!({ "metallic": 1.0, "roughness": 0.04, "baseColor": [1.0, 1.0, 1.0, 1.0] }),
        ),
        (
            0.0_f32,
            "Diffuse",
            serde_json::json!({ "metallic": 0.0, "roughness": 1.0, "baseColor": [0.5, 0.5, 0.5, 1.0] }),
        ),
        (
            2.3_f32,
            "Satin",
            serde_json::json!({ "metallic": 0.0, "roughness": 0.4, "baseColor": [0.85, 0.5, 0.35, 1.0] }),
        ),
    ];
    let mut entities = Vec::with_capacity(balls.len());
    for (x, name, overrides) in &balls {
        let e = preview.create_entity(*name);
        let _ = preview.add_component(
            e,
            Mesh {
                mesh: BUILTIN_SPHERE_MESH_ID,
            },
        );
        let _ = preview.add_component(
            e,
            MaterialSet {
                slots: vec![MaterialSlot {
                    material: Uuid(0),
                    overrides: overrides.clone(),
                }],
            },
        );
        let _ = preview.with_component_mut::<Transform, _>(e, |t| {
            t.translation = GVec3::new(*x, 0.0, 0.0);
        });
        entities.push(e);
    }
    // The center ball is the framing root; the outer two parent to it so the AABB spans all three.
    let root = entities[1];
    for &e in &[entities[0], entities[2]] {
        let _ = preview.set_parent(e, Some(root), true);
    }
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        hdri_id,
        PreviewEnv::Hdri(hdri_id),
    ))
}

/// The `enter-asset-preview` branch for a material (`.smat`): a built-in sphere carrying that
/// material **by id**, in the shared procedural studio. Referencing by id (not a copy) is the join
/// point — a later `material-set-graph` / `material-update` mutates the `.smat` in the asset cache
/// and the sphere re-renders next frame, so the material-graph editor's live pane and a standalone
/// material "View" tab are one host path.
fn enter_material_preview(
    ctx: &mut EngineContext<'_>,
    mid: Uuid,
) -> Result<AssetPreviewResultWrap> {
    let name = ctx
        .assets
        .catalog
        .find(mid)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "Material".to_owned());
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = preview.create_entity(&name);
    // The dense sphere so a displacement-enabled `.smat` shows a true displaced silhouette.
    let _ = preview.add_component(
        root,
        Mesh {
            mesh: PREVIEW_DISPLACE_SPHERE_MESH_ID,
        },
    );
    let _ = preview.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot {
                material: mid,
                ..MaterialSlot::default()
            }],
        },
    );
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        mid,
        PreviewEnv::Procedural,
    ))
}

/// The ephemeral single-slot material the texture preview shades the sphere with: the texture in
/// the slot its role feeds, neutral factors elsewhere (neutral albedo = mid-grey), so the ball
/// reads the map the way a surface uses it. `Albedo`/`Opacity`/`Gloss`/`Unknown` fall back to the
/// base-color slot (show the map as a plain surface texture); HDRI never reaches here.
fn preview_material_for_texture(role: TextureRole, tid: Uuid) -> MaterialAsset {
    use saffron_geometry::glam::{Vec3 as GVec3, Vec4 as GVec4};
    let grey = |v: f32| GVec4::new(v, v, v, 1.0);
    let mut m = default_material_asset();
    m.metallic = 0.0;
    m.roughness = 0.6;
    match role {
        TextureRole::Normal => {
            m.normal_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Roughness => {
            m.orm_texture = tid;
            m.roughness = 1.0;
            m.base_color = grey(0.55);
        }
        TextureRole::Metallic => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 0.35;
            m.base_color = grey(0.8);
        }
        TextureRole::Ao => {
            m.orm_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Orm => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 1.0;
            m.base_color = grey(0.6);
        }
        TextureRole::Height => {
            // Real vertex-shader displacement on the dense preview sphere — a true deformed
            // silhouette (not parallax). `height_scale` is the world-space amplitude on the unit
            // sphere; keep it modest so the surface bulges without turning inside-out.
            m.height_texture = tid;
            m.height_scale = 0.08;
            m.displacement = true;
            m.base_color = grey(0.6);
        }
        TextureRole::Emissive => {
            m.emissive_texture = tid;
            m.emissive = GVec3::ONE;
            m.emissive_strength = 2.0;
            m.base_color = grey(0.02);
        }
        _ => {
            m.albedo_texture = tid;
            m.base_color = GVec4::ONE;
        }
    }
    m
}

/// Commits a container-less preview subject (a built-in primitive or a texture sphere): stashes
/// the authored view on a fresh enter, installs `preview` as the active preview scene, furnishes
/// it (floor / key light / procedural sky / framed cam), and returns the framing. Shared by
/// [`enter_builtin_preview`] and [`enter_texture_preview`] — the rig-less commit tail of
/// [`enter_asset_preview`].
fn commit_preview_subject(
    ctx: &mut EngineContext<'_>,
    preview: Scene,
    root: Entity,
    preview_asset: Uuid,
    env: PreviewEnv,
) -> AssetPreviewResultWrap {
    let root_uuid = preview
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    if !ctx.scene_edit.previewing() {
        ctx.scene_edit.saved_camera = ctx.scene_edit.camera;
        ctx.scene_edit.saved_selection = ctx.scene_edit.selected;
        ctx.scene_edit.saved_overlay = ctx.scene_edit.skeleton_overlay;
        ctx.scene_edit.saved_exposure = ctx.renderer.exposure_ev();
        ctx.scene_edit.preview_active_view = true;
        let (w, h) = (
            ctx.renderer.viewport_width(),
            ctx.renderer.viewport_height(),
        );
        let _ = ctx
            .renderer
            .set_view_desired_size(ViewId::AssetPreview, w, h);
        ctx.renderer.set_active_view(ViewId::AssetPreview);
    }
    ctx.scene_edit.preview_scene = Some(preview);
    ctx.scene_edit.preview_asset = preview_asset;
    ctx.scene_edit.preview_root_entity = root;
    ctx.scene_edit.preview_bone_by_node = Vec::new();
    ctx.scene_edit.preview_floor_entity = Entity::NULL;
    ctx.scene_edit.skeleton_overlay.show = false;
    ctx.scene_edit.skeleton_overlay.highlight_joint = -1;
    let framing = furnish_preview_scene(ctx, root, env);
    ctx.scene_edit.set_selection(root);
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.animation_version += 1;
    AssetPreviewResultWrap(saffron_protocol::AssetPreviewResult {
        root_entity: WireUuid(root_uuid),
        bones: Vec::new(),
        target: vec3(framing.target),
        distance: framing.distance,
    })
}

/// A newtype around [`AssetPreviewResult`](saffron_protocol::AssetPreviewResult) so the
/// `enter-asset-preview` handler can use a free fn (the closure form does not infer the
/// generic). It serializes transparently to the wire DTO.
#[derive(serde::Serialize)]
#[serde(transparent)]
pub struct AssetPreviewResultWrap(saffron_protocol::AssetPreviewResult);

/// The preview framing pivot + orbit distance.
struct PreviewFraming {
    target: saffron_geometry::glam::Vec3,
    distance: f32,
}

/// The previewed model's world-space bounding sphere from its mesh's rest-pose AABB.
pub(crate) struct PreviewBounds {
    center: saffron_geometry::glam::Vec3,
    radius: f32,
    min_y: f32,
}

/// The lighting/backdrop a preview subject is furnished with.
#[derive(Clone, Copy)]
enum PreviewEnv {
    /// A studio key light over the procedural sky (a model / a lone texture sphere).
    Procedural,
    /// An imported HDRI as both the visible backdrop and the IBL source (the environment rig).
    Hdri(Uuid),
}

/// Make the preview look like a preview: floor + key light + procedural sky, or the HDRI
/// environment; framed fly-cam. Returns the orbit pivot + distance. Operates on the committed
/// preview scene.
fn furnish_preview_scene(
    ctx: &mut EngineContext<'_>,
    root: Entity,
    env: PreviewEnv,
) -> PreviewFraming {
    use saffron_geometry::glam::Vec3 as GVec3;

    let bounds = compute_preview_bounds(ctx, root);
    // The HDRI environment is its own backdrop — no floor slab under the rig.
    if ctx.scene_edit.preview_show_floor && matches!(env, PreviewEnv::Procedural) {
        let floor = spawn_preview_floor(ctx, &bounds);
        ctx.scene_edit.preview_floor_entity = floor;
    }

    let preview = ctx
        .scene_edit
        .preview_scene
        .as_mut()
        .expect("preview scene present");
    match env {
        PreviewEnv::Procedural => {
            let light = preview.create_entity("PreviewLight");
            let _ = preview.add_component(
                light,
                DirectionalLight {
                    direction: GVec3::new(-0.4, -1.0, -0.5).normalize(),
                    color: GVec3::ONE,
                    intensity: 3.0,
                    ambient: 0.25,
                },
            );
            preview.environment.sky_mode = SkyMode::Procedural;
            preview.environment.use_sky_for_ambient = true;
            preview.environment.ambient_intensity = 0.3;
        }
        PreviewEnv::Hdri(id) => {
            // The HDRI both lights (IBL prefilter of the equirect) and backs the scene — no
            // directional key light, so the balls read the environment's own illumination.
            preview.environment.sky_mode = SkyMode::Texture;
            preview.environment.sky_texture = id;
            preview.environment.sky_intensity = 1.0;
            preview.environment.use_sky_for_ambient = true;
            preview.environment.ambient_intensity = 1.0;
        }
    }

    ctx.scene_edit.camera = frame_preview_camera(ctx.scene_edit.camera, &bounds);
    let fovy = ctx.scene_edit.camera.fov.to_radians();
    PreviewFraming {
        target: bounds.center,
        distance: bounds.radius / (fovy * 0.5).tan() * 1.3,
    }
}

/// The previewed model's world-space bounding sphere.
pub(crate) fn compute_preview_bounds(ctx: &mut EngineContext<'_>, root: Entity) -> PreviewBounds {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut out = PreviewBounds {
        center: GVec3::ZERO,
        radius: 1.0,
        min_y: 0.0,
    };
    // The whole forest's world AABB — every mesh-bearing node, skinned through the joint
    // palette — not a single resolved entity's box.
    let preview = ctx
        .scene_edit
        .preview_scene
        .as_mut()
        .expect("preview scene present");
    if !preview.valid(root) {
        return out;
    }
    let assets = &mut *ctx.assets;
    let mut bounds = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        bounds = model_render_aabb(gpu, preview, assets, root);
    });

    let Some((lo, hi)) = bounds else {
        // No resolvable mesh: fall back to the model root's position.
        out.center = preview.world_translation(root);
        out.min_y = out.center.y - 1.0;
        return out;
    };
    out.center = (lo + hi) * 0.5;
    out.radius = (hi - lo).length() * 0.5;
    out.min_y = lo.y;
    if out.radius <= 0.0001 {
        out.radius = 1.0;
    }
    out
}

/// A thin floor slab centered under the model's feet.
pub(crate) fn spawn_preview_floor(ctx: &mut EngineContext<'_>, bounds: &PreviewBounds) -> Entity {
    use saffron_geometry::glam::Vec3 as GVec3;

    let assets = &mut *ctx.assets;
    let mut ensured = false;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        ensured = assets.ensure_preview_floor_mesh(gpu);
    });
    if !ensured {
        return Entity::NULL;
    }
    let preview = ctx
        .scene_edit
        .preview_scene
        .as_mut()
        .expect("preview scene present");
    let floor = preview.create_entity("PreviewFloor");
    let _ = preview.add_component(
        floor,
        Mesh {
            mesh: saffron_assets::PREVIEW_FLOOR_MESH_ID,
        },
    );
    let _ = preview.add_component(
        floor,
        MaterialSet {
            slots: vec![MaterialSlot {
                overrides: json!({
                    "baseColor": [0.32, 0.33, 0.35, 1.0],
                    "roughness": 0.92,
                    "metallic": 0.0,
                }),
                ..MaterialSlot::default()
            }],
        },
    );
    let span = (bounds.radius * 8.0).max(0.5);
    let thickness = (bounds.radius * 0.08).max(0.02);
    let _ = preview.with_component_mut::<Transform, _>(floor, |t| {
        t.translation = GVec3::new(
            bounds.center.x,
            bounds.min_y - thickness * 0.5,
            bounds.center.z,
        );
        t.scale = GVec3::new(span, thickness, span);
    });
    floor
}

/// Aim a fly-cam at the model: a 3/4 view fit to its bounding sphere. Starts from the
/// current camera so the user's fov/near/far survive.
fn frame_preview_camera(mut cam: SceneEditCamera, bounds: &PreviewBounds) -> SceneEditCamera {
    use saffron_geometry::glam::Vec3 as GVec3;

    let fovy = cam.fov.to_radians();
    let distance = bounds.radius / (fovy * 0.5).tan() * 1.3;
    let eye = bounds.center + GVec3::new(1.0, 0.7, 1.0).normalize() * distance;
    let forward = (bounds.center - eye).normalize();
    cam.position = eye;
    cam.pitch = forward.y.clamp(-1.0, 1.0).asin().to_degrees();
    cam.yaw = forward.x.atan2(-forward.z).to_degrees();
    cam.far_plane = cam.far_plane.max(distance + bounds.radius * 4.0);
    cam.near_plane = (distance * 0.01).clamp(1e-4, 0.1);
    cam
}

/// Builds the [`PlayStateResult`] from the editor state.
fn play_state_result(ctx: &EngineContext<'_>) -> PlayStateResult {
    let editor = &ctx.scene_edit;
    PlayStateResult {
        state: editor.play_state.name().to_owned(),
        play_version: i32::try_from(editor.play_version).unwrap_or(i32::MAX),
        scene_version: i32::try_from(editor.scene_version).unwrap_or(i32::MAX),
        has_primary_camera: editor.had_primary_camera,
        animation_version: i32::try_from(editor.animation_version).unwrap_or(i32::MAX),
        preview_asset: WireUuid(editor.preview_asset.value()),
    }
}

/// Converts a glam `Vec3` into the wire `Vec3`.
fn vec3(v: saffron_geometry::glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Converts a glam `Vec4` into the wire `Vec4`.
fn vec4(v: saffron_geometry::glam::Vec4) -> Vec4 {
    Vec4 {
        x: v.x,
        y: v.y,
        z: v.z,
        w: v.w,
    }
}

/// Converts a wire `Vec3` into a glam vector.
fn from_vec3(v: Vec3) -> saffron_geometry::glam::Vec3 {
    saffron_geometry::glam::Vec3::new(v.x, v.y, v.z)
}

/// Converts a wire `Vec4` into a glam vector.
fn from_vec4(v: Vec4) -> saffron_geometry::glam::Vec4 {
    saffron_geometry::glam::Vec4::new(v.x, v.y, v.z, v.w)
}

#[cfg(test)]
mod tests {
    use saffron_scene::{AssetEntry, AssetType, MaterialSet, Mesh};
    use saffron_sceneedit::ProjectPhase;
    use serde_json::json;

    use super::preview_codegen_spv;

    use crate::registry::{CommandRegistry, EngineContext, register_builtin_commands};
    use crate::selector::entity_uuid;
    use crate::test_support::{StubRenderer, with_stub};

    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    /// Roots the test's asset server at a unique scratch dir so the material file-writing
    /// commands never collide across parallel tests.
    fn scratch_root(ctx: &mut EngineContext<'_>, tag: &str) {
        let dir = std::env::temp_dir().join(format!(
            "saffron-control-asset-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ctx.assets.set_asset_root(dir.join("assets"));
    }

    /// Seeds a mesh catalog row, returning its decimal-string id.
    fn seed_mesh(ctx: &mut EngineContext<'_>, name: &str) -> u64 {
        let id = saffron_core::Uuid::new();
        ctx.assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: format!("models/{}.smesh", id.value()),
            ..AssetEntry::default()
        });
        id.value()
    }

    /// `list-assets` and `scan-assets` round-trip on an empty (just-loaded) project.
    #[test]
    fn list_and_scan_on_empty_project() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "empty");
            ctx.scene_edit.project_phase = ProjectPhase::Ready;

            let list = reg.dispatch(ctx, &json!({ "cmd": "list-assets" }));
            assert_eq!(list["ok"], json!(true));
            assert_eq!(list["result"]["assets"], json!([]));
            assert_eq!(list["result"]["folders"], json!([]));

            let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
            assert_eq!(scan["ok"], json!(true));
            assert_eq!(scan["result"]["added"], json!(0));
            assert_eq!(scan["result"]["removed"], json!(0));
        });
    }

    /// `scan-assets` (and the other project-gated commands) refuse without a loaded project.
    #[test]
    fn scan_assets_requires_a_project() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scan = reg.dispatch(ctx, &json!({ "cmd": "scan-assets" }));
            assert_eq!(scan["ok"], json!(false));
            assert_eq!(scan["error"], json!("no project loaded"));
        });
    }

    /// `assign-asset` resolves an `AssetSelector` by id and by name, assigns the mesh slot,
    /// and returns a decimal-string id matching the catalog row.
    #[test]
    fn assign_asset_resolves_id_and_name() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let mesh_id = seed_mesh(ctx, "cube");
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            // By numeric id.
            let by_id = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": mesh_id.to_string() } }),
            );
            assert_eq!(by_id["ok"], json!(true));
            assert_eq!(by_id["result"]["id"], json!(mesh_id.to_string()));
            assert_eq!(by_id["result"]["slot"], json!("mesh"));
            assert_eq!(by_id["result"]["name"], json!("cube"));
            assert_eq!(
                ctx.scene_edit
                    .active_scene()
                    .component::<Mesh>(entity)
                    .unwrap()
                    .mesh
                    .value(),
                mesh_id
            );

            // By name selector.
            let by_name = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "cube" } }),
            );
            assert_eq!(by_name["result"]["id"], json!(mesh_id.to_string()));

            // The null sentinel (id 0) clears the slot rather than resolving an asset.
            let clear = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "mesh", "asset": "0" } }),
            );
            assert_eq!(clear["result"]["id"], json!("0"));
            assert_eq!(
                ctx.scene_edit
                    .active_scene()
                    .component::<Mesh>(entity)
                    .unwrap()
                    .mesh
                    .value(),
                0
            );
        });
    }

    /// `assign-asset` on a texture slot attaches a `Material` and writes the texture id.
    #[test]
    fn assign_asset_albedo_overrides_slot_zero() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let tex_id = {
                let id = saffron_core::Uuid::new();
                ctx.assets.catalog.put(AssetEntry {
                    id,
                    name: "albedo".to_owned(),
                    asset_type: AssetType::Texture,
                    ..AssetEntry::default()
                });
                id.value()
            };
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "assign-asset", "params": { "entity": entity_uuid, "slot": "albedo", "asset": tex_id.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["slot"], json!("albedo"));
            // A MaterialSet with a default slot is attached, and the albedo texture lands as
            // a per-object override (a decimal-string uuid) on slot 0.
            let albedo = ctx
                .scene_edit
                .active_scene()
                .with_component::<MaterialSet, _>(entity, |set| {
                    set.slots
                        .first()
                        .and_then(|s| s.overrides.as_object())
                        .and_then(|o| o.get("albedoTexture"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .ok()
                .flatten();
            assert_eq!(albedo.as_deref(), Some(tex_id.to_string().as_str()));
        });
    }

    /// `set-active-view` maps `scene` / `assetPreview` and errors on an unknown view.
    #[test]
    fn set_active_view_maps_and_errors() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scene = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "scene" } }),
            );
            assert_eq!(scene["ok"], json!(true));
            assert_eq!(scene["result"]["view"], json!("scene"));

            let preview = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "assetPreview" } }),
            );
            assert_eq!(preview["result"]["view"], json!("assetPreview"));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-active-view", "params": { "view": "nope" } }),
            );
            assert_eq!(bad["ok"], json!(false));
            assert_eq!(
                bad["error"],
                json!("unknown view 'nope' (expected 'scene' or 'assetPreview')")
            );
        });
    }

    /// `rename-asset` renames the catalog row and returns the new `{id, name}`.
    #[test]
    fn rename_asset_round_trips() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let id = seed_mesh(ctx, "old-name");
            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "rename-asset", "params": { "asset": id.to_string(), "name": "new-name" } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["id"], json!(id.to_string()));
            assert_eq!(reply["result"]["name"], json!("new-name"));
            assert_eq!(
                ctx.assets
                    .catalog
                    .find(saffron_core::Uuid(id))
                    .unwrap()
                    .name,
                "new-name"
            );
        });
    }

    /// `rename-asset` persists the new name to a durable `<path>.smeta` sidecar, so it survives a
    /// cold scan without a project save. Asserts the on-disk sidecar (not a rescan): `with_stub`
    /// reuses one `AssetServer`, so `preserve_name_folder` would mask a regression — the true
    /// cold-scan proof lives in the assets-crate unit test.
    #[test]
    fn rename_asset_writes_a_durable_smeta() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "renamesmeta");
            // A texture row + its directory so the sidecar writer's parent exists.
            let tex = saffron_core::Uuid::new();
            let rel = format!("textures/{}.png", tex.value());
            std::fs::create_dir_all(ctx.assets.root.join("textures")).unwrap();
            ctx.assets.catalog.put(AssetEntry {
                id: tex,
                name: "old".to_owned(),
                asset_type: AssetType::Texture,
                path: rel.clone(),
                ..AssetEntry::default()
            });

            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "rename-asset", "params": { "asset": tex.value().to_string(), "name": "Brick Wall" } }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["name"], json!("Brick Wall"));

            let smeta = ctx.assets.root.join(format!("{rel}.smeta"));
            assert!(smeta.exists(), "rename writes the sidecar beside the file");
            let doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&smeta).unwrap()).unwrap();
            assert_eq!(doc["name"], json!("Brick Wall"));
        });
    }

    /// `create-asset-folder` adds a folder and `list-assets` reflects it; an invalid path
    /// errors.
    #[test]
    fn create_asset_folder_and_list() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let made = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-asset-folder", "params": { "folder": "props/crates" } }),
            );
            assert_eq!(made["ok"], json!(true));
            assert_eq!(made["result"]["folders"], json!(["props/crates"]));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-asset-folder", "params": { "folder": "/leading" } }),
            );
            assert_eq!(bad["ok"], json!(false));
        });
    }

    /// `material-create` then `material-get` round-trips the `.smat`; `material-set-graph`
    /// stores an opaque graph that `material-get` reads back verbatim.
    #[test]
    fn material_set_graph_keeps_graph_opaque() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "material-graph");
            let create = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-create", "params": { "name": "Mat" } }),
            );
            assert_eq!(create["ok"], json!(true));
            let id = create["result"]["id"].as_str().unwrap().to_owned();

            // A graph object with a codegen-only shape (no fold) is stored verbatim.
            let graph = json!({ "nodes": [{ "id": 1, "type": "noise" }], "edges": [] });
            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-set-graph", "params": { "material": id, "graph": graph } }),
            );
            assert_eq!(set["ok"], json!(true), "set-graph: {set:?}");
            assert_eq!(set["result"]["id"], json!(id));

            let get = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-get", "params": { "material": id } }),
            );
            assert_eq!(get["ok"], json!(true), "get: {get:?}");
            assert_eq!(get["result"]["graph"], graph, "graph round-trips opaque");
        });
    }

    /// `preview_codegen_spv` decides the preview pipeline: a graph-less material and a
    /// foldable graph both use the default studio preview (`None`); a non-foldable
    /// (procedural) graph compiles the `_preview.spv` when `slangc` is present and the path
    /// exists on disk (else degrades to `None` — never an error).
    #[test]
    fn preview_codegen_spv_picks_the_pipeline_per_graph() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "preview-codegen");

            // A freshly created material has no graph → the default preview.
            let create = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-create", "params": { "name": "Mat" } }),
            );
            let id_str = create["result"]["id"].as_str().unwrap().to_owned();
            let id = saffron_core::Uuid(id_str.parse::<u64>().unwrap());
            assert!(
                preview_codegen_spv(ctx.assets, id).is_none(),
                "a graph-less material uses the default preview"
            );

            // A foldable graph (a single constant into baseColor) folds into params → still
            // the default preview, no codegen.
            let foldable = json!({
                "nodes": [
                    { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                    { "id": "out", "type": "materialOutput" }
                ],
                "edges": [ { "from": ["c1", "out"], "to": ["out", "baseColor"] } ]
            });
            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-set-graph", "params": { "material": id_str, "graph": foldable } }),
            );
            assert_eq!(set["result"]["foldable"], json!(true), "set: {set:?}");
            assert!(
                preview_codegen_spv(ctx.assets, id).is_none(),
                "a foldable graph uses the default preview"
            );

            // A non-foldable graph (a procedural multiply against a texture slot) needs the
            // codegen preview shader.
            let procedural = json!({
                "nodes": [
                    { "id": "c1", "type": "constant", "props": { "value": [0.5, 0.25, 1.0, 1.0] } },
                    { "id": "tx", "type": "textureSlot", "props": { "slot": "normal" } },
                    { "id": "mul", "type": "multiply" },
                    { "id": "out", "type": "materialOutput" }
                ],
                "edges": [
                    { "from": ["c1", "out"], "to": ["mul", "a"] },
                    { "from": ["tx", "out"], "to": ["mul", "b"] },
                    { "from": ["mul", "out"], "to": ["out", "baseColor"] }
                ]
            });
            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "material-set-graph", "params": { "material": id_str, "graph": procedural } }),
            );
            assert_eq!(set["result"]["foldable"], json!(false), "set: {set:?}");
            // The codegen path runs; with slangc present it yields an on-disk `_preview.spv`,
            // else it degrades to None. Whatever it returns, a Some path must exist.
            if let Some(spv) = preview_codegen_spv(ctx.assets, id) {
                assert!(
                    spv.exists(),
                    "the compiled _preview.spv is on disk: {spv:?}"
                );
                assert!(
                    spv.to_string_lossy().ends_with("_preview.spv"),
                    "the codegen artifact is the preview variant: {spv:?}"
                );
            } else {
                eprintln!("slangc not runnable: codegen preview degraded to the default");
            }
        });
    }

    /// `thumbnail-cache stats` reports a clean cache; an unknown action errors.
    #[test]
    fn thumbnail_cache_stats_and_unknown_action() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            scratch_root(ctx, "thumb-cache");
            let stats = reg.dispatch(
                ctx,
                &json!({ "cmd": "thumbnail-cache", "params": { "action": "stats" } }),
            );
            assert_eq!(stats["ok"], json!(true));
            assert_eq!(stats["result"]["entries"], json!(0));

            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "thumbnail-cache", "params": { "action": "nope" } }),
            );
            assert_eq!(bad["ok"], json!(false));
            assert_eq!(bad["error"], json!("unknown action 'nope' (stats|clear)"));
        });
    }

    /// `get-project` reports the editor's project identity; it is loaded after a field set.
    #[test]
    fn get_project_reports_identity() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            ctx.scene_edit.project_phase = ProjectPhase::Ready;
            ctx.scene_edit.project_name = "demo".to_owned();
            ctx.scene_edit.project_display_name = "Demo".to_owned();
            let reply = reg.dispatch(ctx, &json!({ "cmd": "get-project" }));
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["loaded"], json!(true));
            assert_eq!(reply["result"]["name"], json!("demo"));
            assert_eq!(reply["result"]["displayName"], json!("Demo"));
        });
    }

    /// The asset domain registers in the frozen manifest order (`get-project` … `quit`),
    /// contiguously at the tail of the registry — the order `help` + the contract test
    /// iterate.
    #[test]
    fn asset_commands_register_in_manifest_order() {
        const FROZEN: &[&str] = &[
            "get-project",
            "project-status",
            "cancel-load",
            "new-project",
            "create-script",
            "open-project",
            "import-model",
            "instantiate-model",
            "asset-placement",
            "import-texture",
            "list-assets",
            "scan-assets",
            "extract-subasset",
            "clear-extraction",
            "reimport-model",
            "model-info",
            "asset-references",
            "get-asset-model",
            "enter-asset-preview",
            "exit-asset-preview",
            "set-active-view",
            "clean-assets",
            "delete-unused",
            "rename-asset",
            "create-asset-folder",
            "rename-asset-folder",
            "delete-asset-folder",
            "move-asset",
            "asset-usages",
            "probe-asset",
            "delete-asset",
            "assign-asset",
            "material-create",
            "material-assign",
            "material-import",
            "material-list",
            "material-get",
            "material-schema",
            "material-update",
            "preview-render",
            "material-set-graph",
            "material-create-instance",
            "material-set-override",
            "material-compile-graph",
            "material-cook",
            "export-app",
            "save-scene",
            "load-scene",
            "save-project",
            "load-project",
            "reload-project",
            "get-stores",
            "set-stores",
            "screenshot",
            "get-thumbnail",
            "view-asset",
            "thumbnail-cache",
            "quit",
        ];
        let reg = registry();
        let names: Vec<&str> = reg.rows().iter().map(|c| c.name).collect();
        let start = names
            .iter()
            .position(|&n| n == "get-project")
            .expect("get-project is registered");
        assert_eq!(
            &names[start..start + FROZEN.len()],
            FROZEN,
            "the asset domain registers contiguously in the frozen manifest order"
        );
        // `quit` is the last command in the registry.
        assert_eq!(names.last(), Some(&"quit"));
    }

    /// `asset-usages` reports a mesh slot that references the queried asset, with the entity
    /// id as a decimal string.
    #[test]
    fn asset_usages_reports_mesh_slot() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let mesh_id = seed_mesh(ctx, "cube");
            let entity = ctx.scene_edit.active_scene().create_entity("box");
            let _ = ctx.scene_edit.active_scene().add_component(
                entity,
                Mesh {
                    mesh: saffron_core::Uuid(mesh_id),
                },
            );
            let entity_uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            let reply = reg.dispatch(
                ctx,
                &json!({ "cmd": "asset-usages", "params": { "asset": mesh_id.to_string() } }),
            );
            assert_eq!(reply["ok"], json!(true));
            let usages = reply["result"]["usages"].as_array().unwrap();
            assert_eq!(usages.len(), 1);
            assert_eq!(usages[0]["slot"], json!("mesh"));
            assert_eq!(usages[0]["entity"], json!(entity_uuid));
        });
    }
}
