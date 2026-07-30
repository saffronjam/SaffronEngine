use saffron_assets::{
    asset_bytes, asset_type_name, build_dependency_graph, clear_extraction, extract_sub_asset,
    reimport_model,
};
use saffron_core::Uuid;
use saffron_protocol::{
    AssetCapabilitiesDto, AssetModelResult, AssetRef, AssetReferencesParams, AssetReferencesResult,
    ClearExtractionParams, EmptyParams, ExtractSubAssetParams, GetAssetModelParams,
    ModelInfoParams, ModelInfoResult, ModelSubAssetDto, PlayStateResult, ReimportModelParams,
    ReimportModelResult, ScanAssetsResult, SetActiveViewParams, SetActiveViewResult,
    Uuid as WireUuid,
};
use saffron_rendering::ViewId;
use saffron_scene::{AssetEntry, AssetType};

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;

/// Registers the catalog inspection, sub-asset, and asset-preview commands.
pub(crate) fn register_catalog(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, ScanAssetsResult>("scan-assets", "scan-assets", |ctx, _params| {
        require_project_loaded(ctx)?;
        ctx.renderer.wait_gpu_idle();
        ctx.assets.clear_asset_caches();
        let delta = ctx.assets.scan_assets().map_err(Error::command)?;
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
                    .map_err(Error::command)?;
            let name = ctx
                .assets
                .catalog()
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
            clear_extraction(ctx.assets, model_id, sub_id).map_err(Error::command)?;
            let name = ctx
                .assets
                .catalog()
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
            let delta = reimport_model(ctx.assets, id).map_err(Error::command)?;
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
            if ctx.assets.catalog().find(id).map(|e| e.asset_type) != Some(AssetType::Model) {
                return Err(Error::command(format!(
                    "asset {} is not a model",
                    id.value()
                )));
            }
            let path = ctx
                .assets
                .catalog()
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
                .catalog()
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
}
