use saffron_assets::{BuiltinMesh, analyze_clean, delete_unused};
use saffron_core::Uuid;
use saffron_protocol::{
    AssetList, AssetMetadataDto, AssetMetadataParams, AssetRef, AssetSlotDto, AssetUsagesParams,
    AssetUsagesResult, AssignAssetParams, AssignAssetResult, CleanAssetsParams, CleanCandidateDto,
    CleanReport, CreateAssetFolderParams, DeleteAssetFolderParams, DeleteAssetParams,
    DeleteAssetResult, DeleteUnusedParams, DeleteUnusedResult, MoveAssetParams,
    RenameAssetFolderParams, RenameAssetParams, Uuid as WireUuid,
};
use saffron_scene::{AssetType, Mesh};
use saffron_sceneedit::PlayState;

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::resolve_entity;

/// Registers the asset and folder management commands.
pub(crate) fn register_asset_management(reg: &mut CommandRegistry) {
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
            let deleted = delete_unused(assets, scene, &ids, confirm).map_err(Error::command)?;
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
            let Some(id) = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .find(|entry| entry.id.value() == by_id || entry.name == selector)
                .map(|entry| entry.id)
            else {
                return Err(Error::command(format!("no asset '{selector}'")));
            };
            let renamed = ctx.assets.rename_asset(id, params.name.clone());
            debug_assert!(renamed, "selected asset remains catalogued");
            // Persist the new name to the durable sidecar so it survives a cold scan without a save.
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "rename-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(
                ctx.assets.catalog().find(id).expect("just renamed"),
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
            if !has_folder(ctx.assets.catalog(), &params.folder) {
                ctx.assets.add_catalog_folder(params.folder.clone());
                ctx.scene_edit.scene_version += 1;
            }
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
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
            if !has_folder(ctx.assets.catalog(), &params.folder) {
                return Err(Error::command(format!(
                    "no asset folder '{}'",
                    params.folder
                )));
            }
            if params.folder == params.name {
                return Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()));
            }
            if is_folder_descendant(&params.name, &params.folder) {
                return Err(Error::command("asset folder cannot be moved inside itself"));
            }
            if has_folder(ctx.assets.catalog(), &params.name) {
                return Err(Error::command(format!(
                    "asset folder '{}' already exists",
                    params.name
                )));
            }
            let folders = ctx
                .assets
                .catalog_folders()
                .iter()
                .map(|folder| {
                    if *folder == params.folder || is_folder_descendant(folder, &params.folder) {
                        replace_folder_prefix(folder, &params.folder, &params.name)
                    } else {
                        folder.clone()
                    }
                })
                .collect();
            ctx.assets.replace_catalog_folders(folders);
            let touched = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| {
                    entry.folder == params.folder
                        || is_folder_descendant(&entry.folder, &params.folder)
                })
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            for id in &touched {
                let folder = ctx
                    .assets
                    .catalog()
                    .find(*id)
                    .map(|entry| replace_folder_prefix(&entry.folder, &params.folder, &params.name))
                    .expect("collected asset remains catalogued");
                let _ = ctx.assets.move_asset_to_folder(*id, folder);
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "rename-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
        },
    );

    reg.register::<DeleteAssetFolderParams, AssetList>(
        "delete-asset-folder",
        "delete-asset-folder {folder}",
        |ctx, params| {
            let mut removed = false;
            let mut folders = Vec::with_capacity(ctx.assets.catalog().folders.len());
            for folder in &ctx.assets.catalog().folders {
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
            ctx.assets.replace_catalog_folders(folders);
            let touched = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| {
                    entry.folder == params.folder
                        || is_folder_descendant(&entry.folder, &params.folder)
                })
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            for id in &touched {
                let _ = ctx.assets.move_asset_to_folder(*id, String::new());
            }
            ctx.scene_edit.scene_version += 1;
            write_asset_sidecars(ctx.assets, &touched, "delete-asset-folder");
            Ok(asset_list_dto(&ctx.assets.root, ctx.assets.catalog()))
        },
    );

    reg.register::<MoveAssetParams, AssetRef>(
        "move-asset",
        "move-asset {asset, folder?}",
        |ctx, params| {
            let index = resolve_asset_index(ctx, &params.asset)?;
            let folder = params.folder.clone().unwrap_or_default();
            if !folder.is_empty() && !has_folder(ctx.assets.catalog(), &folder) {
                return Err(Error::command(format!("no asset folder '{folder}'")));
            }
            let id = ctx.assets.catalog().entries[index].id;
            let moved = ctx.assets.move_asset_to_folder(id, folder);
            debug_assert!(moved, "selected asset remains catalogued");
            ctx.scene_edit.scene_version += 1;
            if let Err(err) = ctx.assets.write_asset_sidecar(id) {
                tracing::warn!(
                    "move-asset: could not write .smeta for {}: {err}",
                    id.value()
                );
            }
            Ok(asset_ref(
                ctx.assets.catalog().find(id).expect("just moved"),
            ))
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
                .catalog()
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
            let entry = ctx.assets.catalog().entries[index].clone();
            saffron_assets::remove_vegetation_map_package(ctx.assets, &entry)
                .map_err(Error::command)?;
            let cleared = clear_asset_usages(&mut ctx.scene_edit.scene, entry.id);
            let removed = ctx.assets.delete_asset_entry(entry.id);
            debug_assert!(removed.is_some(), "selected asset remains catalogued");
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
            let clearing = selector == "0" || selector.is_empty() || params.asset.id() == Some(0);
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
                    .catalog()
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
}
