use std::path::Path;

use saffron_assets::{create_project_script, default_display_name, valid_project_name};
use saffron_protocol::{
    CreateScriptParams, CreateScriptResult, EmptyParams, ExportAppParams, ExportAppResult,
    NewProjectParams, OptionalPathParams, PathParams, PathResult, ProjectInfoDto, ProjectStatusDto,
    ProjectStoresDto, QuitResult, ScreenshotParams, ScreenshotResult, ScreenshotTargetDto,
    ThumbnailCacheParams, ThumbnailCacheResult, ThumbnailParams, ThumbnailResult,
};
use saffron_scene::Entity;
use saffron_sceneedit::{NewProjectSpec, PlayState, ProjectLoadRequest, ProjectPhase};

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;

/// Registers the project-lifecycle commands (`get-project` … `open-project`).
pub(crate) fn register_project_lifecycle(reg: &mut CommandRegistry) {
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
            let name = params.name.unwrap_or_default();
            if !valid_project_name(&name) {
                return Err(Error::command(format!("invalid project name '{name}'")));
            }
            ctx.vegetation_cook_jobs.shutdown();
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::New(NewProjectSpec {
                name,
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
                .map_err(Error::command)?;
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
            ctx.vegetation_cook_jobs.shutdown();
            ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(params.path.clone()));
            ctx.scene_edit.project_phase = ProjectPhase::Loading;
            Ok(project_status_dto(ctx))
        },
    );
}

/// Registers export, scene/project I/O, screenshots, thumbnails, and `quit`.
pub(crate) fn register_project_io(reg: &mut CommandRegistry) {
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
            .map_err(Error::command)?;
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
                .map_err(Error::command)?;
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
            // The save barrier: every promoted plant's live transform and velocity reduces
            // through the vegetation reducer first, so the saved state is complete without
            // waiting for a falling object to settle.
            crate::commands_vegetation_runtime::flush_promoted_state(ctx)?;
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
                .map_err(Error::command)?;
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
            ctx.vegetation_cook_jobs.shutdown();
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
