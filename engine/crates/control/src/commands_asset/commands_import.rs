use saffron_assets::{
    colorspace_for_role_explicit, import_vegetation_asset, texture_role_from_hint,
};
use saffron_protocol::{
    AssetPlacementParams, AssetPlacementPhaseDto, AssetPlacementResult, EntityRef,
    ImportModelParams, ImportModelResult, ImportTextureParams, ImportTextureResult,
    ImportVegetationAssetParams, ImportVegetationAssetResult, InstantiateModelParams,
    Uuid as WireUuid,
};
use saffron_scene::{AssetType, TextureRole};

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::entity_ref_dto;

/// Registers the import + instantiation commands.
pub(crate) fn register_import(reg: &mut CommandRegistry) {
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
                .map_err(Error::command)?;
            if let Some(attribution) = params.attribution {
                let _ = ctx
                    .assets
                    .set_asset_attribution(bake.model_id, attribution_from_dto(attribution));
            }
            let name = ctx
                .assets
                .catalog()
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
            let entry_type = ctx.assets.catalog().find(id).map(|e| e.asset_type);
            let entry_name = ctx
                .assets
                .catalog()
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
                .map_err(Error::command)?;
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
                .map_err(Error::command)?;
            Ok(ImportTextureResult {
                texture: WireUuid(id.value()),
            })
        },
    );

    reg.register::<saffron_protocol::ImportLutParams, saffron_protocol::ImportLutResult>(
        "import-lut",
        "import-lut {path} — import a creative .cube look as a LUT asset",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            let assets = &mut *ctx.assets;
            let path = params.path.clone();
            let mut result = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = Some(assets.import_cube_lut(gpu, &path));
            });
            let id = result
                .ok_or_else(|| Error::command("upload seam unavailable"))?
                .map_err(Error::command)?;
            Ok(saffron_protocol::ImportLutResult {
                lut: WireUuid(id.value()),
            })
        },
    );

    reg.register::<ImportVegetationAssetParams, ImportVegetationAssetResult>(
        "import-vegetation-asset",
        "import-vegetation-asset {path} [folder] — import an authored .splant, .sbiome, or .svegmap package",
        |ctx, params| {
            if params.path.is_empty() {
                return Err(Error::command("missing 'path'"));
            }
            require_project_loaded(ctx)?;
            let folder = params.folder.unwrap_or_default();
            if !folder.is_empty() && !has_folder(ctx.assets.catalog(), &folder) {
                return Err(Error::command(format!("no asset folder '{folder}'")));
            }
            let imported = import_vegetation_asset(ctx.assets, &params.path, &folder)
                .map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(ImportVegetationAssetResult {
                id: WireUuid(imported.id.value()),
                name: imported.name,
                r#type: asset_type_dto(imported.asset_type),
            })
        },
    );
}
