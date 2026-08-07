use std::path::Path;

use saffron_assets::{
    AssetServer, colorspace_name, model_render_aabb, pick_scene_surface, texture_role_name,
    viewport_ray,
};
use saffron_core::Uuid;
use saffron_geometry::glam::Vec2;
use saffron_protocol::{
    AssetAttributionDto, AssetEntryDto, AssetList, AssetPlacementParams, AssetPlacementResult,
    AssetRef, AssetSelector, AssetUsageDto, PlacementTransformDto, ProjectStoresDto,
    Uuid as WireUuid,
};
use saffron_scene::{
    AssetEntry, AssetType, Attribution, Colorspace, Entity, IdComponent, MaterialSet, Mesh, Name,
    PreviewGhost, Scene, TextureRole, Transform, VegetationField,
};
use saffron_sceneedit::{PlacementPreview, PlayState};

use super::*;
use crate::error::{Error, Result};
use crate::registry::EngineContext;
use crate::selector::{entity_ref_dto, entity_uuid};
use saffron_geometry::glam::Vec3 as MathVec3;

/// Reads an id-or-name selector value as its string form, treating any non-string as empty.
pub(crate) fn selector_string(selector: &AssetSelector) -> String {
    selector.name().unwrap_or_default().to_owned()
}

/// Wraps a folder path as an optional, mapping an empty path to `None`.
pub(crate) fn optional_folder(folder: &str) -> Option<String> {
    if folder.is_empty() {
        None
    } else {
        Some(folder.to_owned())
    }
}

/// The uuid an id-or-name selector resolves to: an unsigned number, a non-negative signed
/// number, or a whole-string decimal parse.
pub(crate) fn selector_id(selector: &AssetSelector) -> u64 {
    selector.id().unwrap_or(0)
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to a catalog entry id,
/// by id or name.
pub(crate) fn resolve_asset(ctx: &EngineContext<'_>, selector: &AssetSelector) -> Result<Uuid> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for entry in &ctx.assets.catalog().entries {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(entry.id);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to its index in the
/// catalog `entries`.
pub(crate) fn resolve_asset_index(
    ctx: &EngineContext<'_>,
    selector: &AssetSelector,
) -> Result<usize> {
    let by_id = selector_id(selector);
    let name = selector_string(selector);
    for (i, entry) in ctx.assets.catalog().entries.iter().enumerate() {
        if entry.id.value() == by_id || entry.name == name {
            return Ok(i);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

pub(crate) fn preview_asset_placement(
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
        .catalog()
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
                .map_err(Error::command)?;
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
pub(crate) fn clear_placement_ghost(ctx: &mut EngineContext<'_>) {
    if let Some(preview) = ctx.scene_edit.placement_preview.take() {
        ctx.scene_edit.scene.destroy_entity(preview.root);
    }
}

pub(crate) fn commit_asset_placement(ctx: &mut EngineContext<'_>) -> Result<AssetPlacementResult> {
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
pub(crate) fn compute_asset_placement(
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
        .map_err(|error| error.to_string())?
        .map(|hit| {
            hit.surface
                .position
                .to_render_relative(saffron_spatial::WorldPosition::origin())
        })
        .transpose()
        .map_err(|error| error.to_string())?
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

pub(crate) fn ground_plane_hit(ray: saffron_geometry::Ray) -> Option<MathVec3> {
    if ray.dir.y.abs() < 0.0001 {
        return None;
    }
    let t = -ray.origin.y / ray.dir.y;
    (t >= 0.0).then_some(ray.origin + ray.dir * t)
}

pub(crate) fn apply_transform(scene: &mut Scene, entity: Entity, transform: Transform) {
    let _ = scene.with_component_mut::<Transform, _>(entity, |t| *t = transform);
}

pub(crate) fn placement_transform_dto(transform: &Transform) -> PlacementTransformDto {
    PlacementTransformDto {
        translation: vec3(transform.translation),
        rotation: vec3(transform.rotation),
        scale: vec3(transform.scale),
    }
}

/// Parses the opaque `stores` sidecar block into the wire DTO, defaulting when absent.
pub(crate) fn stores_dto_from_value(value: &serde_json::Value) -> ProjectStoresDto {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Maps an `import-texture` colorspace hint to a `Colorspace`; `auto`/absent → heuristic.
pub(crate) fn colorspace_from_str(value: Option<&str>) -> Option<Colorspace> {
    match value.map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("srgb") => Some(Colorspace::Srgb),
        Some("linear") => Some(Colorspace::Linear),
        Some("hdr") => Some(Colorspace::Hdr),
        _ => None,
    }
}

/// Converts a wire attribution DTO into the catalog's `Attribution`.
pub(crate) fn attribution_from_dto(dto: AssetAttributionDto) -> Attribution {
    Attribution {
        license_id: dto.license_id,
        requires_attribution: dto.requires_attribution,
        license_url: dto.license_url,
        author: dto.author,
        source_url: dto.source_url,
        store_id: dto.store_id,
    }
}

/// Creation time (seconds since the Unix epoch) of the file backing a catalog entry, preferring
/// the filesystem birth time and falling back to the modified time; `0` if neither is available.
pub(crate) fn asset_created_at(root: &Path, rel_path: &str) -> i64 {
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
pub(crate) fn asset_dto(root: &Path, entry: &AssetEntry) -> AssetEntryDto {
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
pub(crate) fn attribution_to_dto(attribution: &Attribution) -> AssetAttributionDto {
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
pub(crate) fn asset_ref(entry: &AssetEntry) -> AssetRef {
    AssetRef {
        id: WireUuid(entry.id.value()),
        name: entry.name.clone(),
        folder: optional_folder(&entry.folder),
    }
}

/// Rewrites the durable `.smeta` sidecar for each id after a folder mutation touched many
/// rows. Best-effort: an IO failure is logged, not surfaced, so the command still succeeds.
pub(crate) fn write_asset_sidecars(assets: &AssetServer, ids: &[Uuid], label: &str) {
    for &id in ids {
        if let Err(err) = assets.write_asset_sidecar(id) {
            tracing::warn!("{label}: could not write .smeta for {}: {err}", id.value());
        }
    }
}

/// The full catalog as an [`AssetList`].
pub(crate) fn asset_list_dto(root: &Path, catalog: &saffron_scene::AssetCatalog) -> AssetList {
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
pub(crate) fn valid_folder_path(folder: &str) -> bool {
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
pub(crate) fn has_folder(catalog: &saffron_scene::AssetCatalog, folder: &str) -> bool {
    catalog.folders.iter().any(|existing| existing == folder)
}

/// Whether `candidate` is a strict descendant folder of `folder`.
pub(crate) fn is_folder_descendant(candidate: &str, folder: &str) -> bool {
    candidate.len() > folder.len()
        && candidate.starts_with(folder)
        && candidate.as_bytes()[folder.len()] == b'/'
}

/// Re-roots `value` from the `from` folder prefix onto `to`.
pub(crate) fn replace_folder_prefix(value: &str, from: &str, to: &str) -> String {
    if value == from {
        return to.to_owned();
    }
    if is_folder_descendant(value, from) {
        return format!("{to}{}", &value[from.len()..]);
    }
    value.to_owned()
}

/// The entity's `Name`, or empty.
pub(crate) fn entity_name(scene: &Scene, entity: Entity) -> String {
    scene
        .with_component::<Name, _>(entity, |n| n.name.clone())
        .unwrap_or_default()
}

/// The entity's `IdComponent` uuid as an optional wire uuid.
pub(crate) fn entity_id(scene: &Scene, entity: Entity) -> Option<WireUuid> {
    let id = entity_uuid(scene, entity);
    (id != 0).then_some(WireUuid(id))
}

/// One `(entity, slot)` reference, collected during a scene scan and resolved to a usage DTO
/// after — so the scan's `&mut` scene borrow does not overlap the per-entity name/id reads.
pub(crate) type Reference = (Entity, &'static str);

/// Collects every `(entity, slot)` reference to `asset` in the scene (the scan half of
/// [`collect_asset_usages`] / [`clear_asset_usages`]): mesh slots + material albedo /
/// metallic-roughness slots, and the vegetation-map field. The environment sky-texture hit is
/// the boolean second tuple.
pub(crate) fn scan_asset_references(scene: &mut Scene, asset: Uuid) -> (Vec<Reference>, bool) {
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
    scene.for_each::<(&VegetationField,), _>(|entity, (field,)| {
        if field.map.value() == asset.value() {
            refs.push((entity, "vegetationField.map"));
        }
    });
    let sky = scene.environment.sky_texture.value() == asset.value();
    (refs, sky)
}

/// One collected `(entity, slot)` reference as a usage DTO (the name/id read after the scan).
pub(crate) fn usage_dto(scene: &Scene, entity: Entity, slot: &str) -> AssetUsageDto {
    AssetUsageDto {
        entity: entity_id(scene, entity),
        entity_name: Some(entity_name(scene, entity)),
        slot: slot.to_owned(),
    }
}

/// Every place `asset` is referenced in the active scene: `Mesh` slots, `MaterialSet` slot
/// material references, and the environment sky texture.
pub(crate) fn collect_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
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
pub(crate) fn clear_asset_usages(scene: &mut Scene, asset: Uuid) -> Vec<AssetUsageDto> {
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
            "vegetationField.map" => {
                let _ = scene.with_component_mut::<VegetationField, _>(entity, |field| {
                    field.map = Uuid(0);
                    field.enabled = false;
                });
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
