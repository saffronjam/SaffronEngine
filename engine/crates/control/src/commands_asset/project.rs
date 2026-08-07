use std::path::Path;

use saffron_assets::{ProjectHost, ProjectInfo, valid_project_name};
use saffron_protocol::{BootStageDto, ProjectInfoDto, ProjectPhaseDto, ProjectStatusDto};
use saffron_sceneedit::{
    BootStage, NewProjectSpec, ProjectLoadRequest, ProjectPhase, SceneEditContext,
};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::registry::{ControlRenderer, EngineContext};

/// The current project's identity, read from the editor.
pub(crate) fn current_project_info(ctx: &EngineContext<'_>) -> ProjectInfo {
    ProjectInfo {
        loaded: ctx.scene_edit.project_ready(),
        root: ctx.scene_edit.project_root.clone(),
        path: ctx.scene_edit.project_path.clone(),
        name: ctx.scene_edit.project_name.clone(),
        display_name: ctx.scene_edit.project_display_name.clone(),
    }
}

/// Writes a [`ProjectInfo`] back onto the editor, also resetting the scene path.
pub(crate) fn apply_project_info(ctx: &mut EngineContext<'_>, project: &ProjectInfo) {
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

/// Brings the host's project up from the editor-set environment by seeding the loader inbox, the
/// same non-blocking path the lifecycle commands use. `SAFFRON_PROJECT` names a project to open or
/// create (a created project takes its display name from `SAFFRON_PROJECT_DISPLAY_NAME` when set),
/// else `SAFFRON_SCRATCH_PROJECT` makes a deterministic per-shell scratch project, else a
/// `project.json` in the working directory is opened. With none set the phase stays `Unloaded` and
/// the host waits for the editor's project picker.
pub fn bootstrap_project_from_env(scene_edit: &mut SceneEditContext) {
    let request = if let Some(selected) = std::env::var_os("SAFFRON_PROJECT")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
    {
        let create_new =
            valid_project_name(&selected) && !saffron_assets::project_json_path(&selected).exists();
        if create_new {
            let display_name = std::env::var("SAFFRON_PROJECT_DISPLAY_NAME").unwrap_or_default();
            ProjectLoadRequest::New(NewProjectSpec {
                name: selected,
                display_name,
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
pub(crate) fn project_dto(project: &ProjectInfo) -> ProjectInfoDto {
    ProjectInfoDto {
        loaded: project.loaded,
        root: project.root.clone(),
        path: project.path.clone(),
        name: project.name.clone(),
        display_name: project.display_name.clone(),
    }
}

/// The wire DTO for the live project-load phase + progress.
pub(crate) fn project_status_dto(ctx: &EngineContext<'_>) -> ProjectStatusDto {
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
pub(crate) fn boot_stage_dto(stage: BootStage) -> BootStageDto {
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
pub(crate) fn require_project_loaded(ctx: &EngineContext<'_>) -> Result<()> {
    if ctx.scene_edit.project_ready() {
        Ok(())
    } else {
        Err(Error::command("no project loaded"))
    }
}

/// The [`ProjectHost`] adapter over the renderer seam, for the project lifecycle calls that need
/// the GPU-idle and render-settings serde the renderer owns. It wraps `&mut dyn ControlRenderer`, so
/// it borrows only that field of the [`EngineContext`], disjoint from `assets` and `scene_edit`.
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
