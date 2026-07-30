//! The asset/project-domain control commands: project lifecycle, model + texture import and
//! instantiation, the catalog, sub-asset extraction, model info + references, the asset preview,
//! asset/folder management, the material system, scene save/load, screenshots, and thumbnails.
//!
//! The highest-coupling domain: a handler holds `&mut` to the [`AssetServer`][saffron_assets::AssetServer],
//! the [`SceneEditContext`][saffron_sceneedit::SceneEditContext], and the renderer at once through
//! the disjoint [`EngineContext`][crate::registry::EngineContext] fields. The importers, the
//! node-graph to Slang codegen, and the preview render live in `saffron-assets`; these handlers are
//! thin orchestration.

use crate::registry::CommandRegistry;

mod catalog;
mod commands_catalog;
mod commands_import;
mod commands_interchange;
mod commands_manage;
mod commands_map;
mod commands_material;
mod commands_plant;
mod commands_project;
mod export;
mod map_dto;
mod plant_dto;
mod preview;
mod preview_scene;
mod project;
#[cfg(test)]
mod tests;

pub(crate) use catalog::*;
pub(crate) use commands_interchange::*;
pub(crate) use commands_plant::*;
pub(crate) use export::*;
pub(crate) use map_dto::*;
pub(crate) use plant_dto::*;
pub(crate) use preview::*;
pub(crate) use preview_scene::*;
pub use preview_scene::{PreviewSubject, build_preview_scene_for_thumbnail};
pub(crate) use project::*;

/// Registers the asset/project-domain commands in the frozen manifest order
/// (`get-project` … `quit`).
pub fn register_asset_commands(reg: &mut CommandRegistry) {
    commands_project::register_project_lifecycle(reg);
    commands_import::register_import(reg);
    commands_map::register_vegetation_map(reg);
    commands_catalog::register_catalog(reg);
    commands_manage::register_asset_management(reg);
    commands_material::register_material(reg);
    commands_project::register_project_io(reg);
}
