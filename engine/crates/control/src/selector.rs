//! The shared entity selector + small wire conversions every domain handler reuses.
//!
//! The scene/asset/animation/physics phases call [`resolve_entity`] and
//! [`entity_ref_dto`] rather than re-deriving the id-or-name lookup. A numeric
//! selector (or a fully-numeric string) is a UUID, parsed whole-string, resolved
//! against the **active** scene so it finds runtime entities during play; a
//! non-numeric string falls back to a [`Name`] scan.

use saffron_geometry::glam;
use saffron_protocol::{EntityRef, EntitySelector, Uuid, Vec3};
use saffron_scene::{Entity, IdComponent, Name, Scene};

use crate::error::Error;
use crate::registry::EngineContext;

/// Converts a glam vector into the wire `Vec3`.
#[must_use]
pub fn to_vec3(v: glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Converts a wire `Vec3` into a glam vector.
#[must_use]
pub fn from_vec3(v: Vec3) -> glam::Vec3 {
    glam::Vec3::new(v.x, v.y, v.z)
}

/// The entity's `IdComponent` uuid, or `0` when it carries none.
#[must_use]
pub fn entity_uuid(scene: &Scene, entity: Entity) -> u64 {
    scene
        .component::<IdComponent>(entity)
        .map(|id| id.id.0)
        .unwrap_or(0)
}

/// Resolves an [`EntitySelector`] to a live [`Entity`] in the active scene.
///
/// UUID first (stable across reloads; a fully-numeric string counts as a UUID, parsed
/// whole-string), then a [`Name`] scan.
///
/// # Errors
///
/// [`Error::Command`] when the selector resolves to no entity.
pub fn resolve_entity(
    ctx: &mut EngineContext<'_>,
    selector: &EntitySelector,
) -> Result<Entity, Error> {
    let scene = ctx.scene_edit.active_scene();

    if let Some(wanted) = selector.id()
        && let Some(found) = scene.find_entity_by_uuid(saffron_core::Uuid(wanted))
    {
        return Ok(found);
    }

    if let Some(name) = selector.name() {
        let mut found: Option<Entity> = None;
        scene.for_each::<&Name, _>(|entity, component| {
            if found.is_none() && component.name == name {
                found = Some(entity);
            }
        });
        if let Some(found) = found {
            return Ok(found);
        }
    }

    let selector = serde_json::to_string(selector).unwrap_or_else(|_| "<selector>".to_owned());
    Err(Error::command(format!("entity not found: {selector}")))
}

/// Re-fits an entity's `Collider` to its mesh AABB through the asset reader: a thin
/// wrapper that binds the [`MeshCook`](saffron_physics::MeshCook) seam to
/// [`AssetServer::load_mesh_cpu_asset`] and forwards to the shared
/// [`saffron_physics::fit_collider_to_mesh`]. Returns `false` when there is no
/// collider, no resolvable mesh, or a degenerate shape.
///
/// [`AssetServer`]: saffron_assets::AssetServer
/// [`AssetServer::load_mesh_cpu_asset`]: saffron_assets::AssetServer::load_mesh_cpu_asset
pub fn fit_collider(ctx: &mut EngineContext<'_>, entity: Entity) -> bool {
    let assets = &mut *ctx.assets;
    let mut cook = |id: saffron_core::Uuid| {
        assets
            .load_mesh_cpu_asset(id)
            .map_err(|error| error.to_string())
    };
    saffron_physics::fit_collider_to_mesh(ctx.scene_edit.active_scene(), entity, &mut cook)
}

/// Builds the `{ id: decimal-string, name }` reply for a resolved entity. A missing
/// `Name` reads as empty, a missing id as `0`.
#[must_use]
pub fn entity_ref_dto(scene: &Scene, entity: Entity) -> EntityRef {
    let name = scene
        .with_component::<Name, _>(entity, |n| n.name.clone())
        .unwrap_or_default();
    EntityRef {
        id: Uuid(entity_uuid(scene, entity)),
        name,
    }
}
