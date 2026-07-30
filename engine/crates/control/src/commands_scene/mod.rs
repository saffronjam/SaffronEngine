//! The scene-edit control commands: entity lifecycle, the registry-driven component commands,
//! selection, picking, inspect, focus, world transform, the editor camera, the gizmo, the input
//! surfaces, the play-state machine, environment authoring, and the scripting surface.
//!
//! The most `sceneEdit`-coupled domain: the handlers drive the
//! [`SceneEditContext`][saffron_sceneedit::SceneEditContext] — selection, gizmo state, play
//! machine, active-scene resolution — and read the scene world through its component registry.
//! `set-component`, `add-entity`, and `pick` also touch `assets` and the renderer.

use crate::registry::CommandRegistry;

mod entity;
mod environment;
mod play;
mod spatial;
#[cfg(test)]
mod tests;
mod viewport;

pub(crate) use environment::*;
pub(crate) use play::*;
pub(crate) use spatial::*;

/// Registers the scene-domain commands in the frozen manifest order.
pub fn register_scene_commands(reg: &mut CommandRegistry) {
    entity::register_entities(reg);
    spatial::register_picking(reg);
    entity::register_inspection(reg);
    environment::register_environment(reg);
    play::register_play_and_scripts(reg);
    entity::register_entity_authoring(reg);
    viewport::register_viewport(reg);
}
