use saffron_assets::{BuiltinMesh, model_render_aabb};
use saffron_geometry::glam::Mat4;
use saffron_protocol::{
    AddComponentResult, AddEntityParams, AddEntityPreset, ComponentList, ComponentParams,
    CreateEntityParams, DestroyEntityResult, EmptyParams, EntityList, EntityListEntry,
    EntityParams, EntityRef, InspectResult, RemoveComponentResult, RenameEntityParams,
    SetComponentFieldParams, SetComponentFieldResult, SetComponentOrderParams,
    SetComponentOrderResult, SetComponentParams, SetComponentResult, SetLightParams,
    SetParentParams, SetTransformParams, Uuid as WireUuid,
};
use saffron_scene::{
    Bone, Camera, ComponentTraits, DirectionalLight, Entity, IdComponent, MaterialSet,
    MaterialSlot, Mesh, Name, PointLight, PreviewGhost, Relationship, SpotLight, Transform,
};
use serde_json::{Map, Value, json};

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::{entity_ref_dto, entity_uuid, fit_collider, resolve_entity};
use saffron_geometry::glam::Vec3 as GlamVec3;

/// Whether a parent selector means "the scene root" — absent, `0`, `"0"`, or empty: a
/// detach never resolves entity 0.
pub(crate) fn is_root_selector(selector: &saffron_protocol::EntitySelector) -> bool {
    selector.id() == Some(0) || selector.name().is_some_and(str::is_empty)
}

/// Registers the entity, component, and transform commands.
pub(crate) fn register_entities(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, EntityList>(
        "list-entities",
        "list all entities",
        |ctx, _params| {
            let scene = ctx.scene_edit.active_scene();
            // Gather the id+name during the scan (which borrows the scene), then read the
            // parent/bone flags after, so the per-entity reads don't overlap the for_each.
            let mut rows: Vec<(Entity, u64, String)> = Vec::new();
            scene.for_each::<(&IdComponent, &Name), _>(|entity, (id, name)| {
                rows.push((entity, id.id.value(), name.name.clone()));
            });
            // Asset-placement preview ghosts render in the viewport but are not authored
            // entities, so they never appear in the outliner.
            rows.retain(|&(entity, _, _)| !scene.has_component::<PreviewGhost>(entity));
            let mut entities = Vec::with_capacity(rows.len());
            for (entity, id, name) in rows {
                let mut entry = EntityListEntry {
                    id: WireUuid(id),
                    name,
                    parent_id: None,
                    bone: None,
                };
                // Omit parentId for roots (and bone for non-joints) so the optional fields
                // stay genuinely optional.
                if let Ok(parent) =
                    scene.with_component::<Relationship, _>(entity, |r| r.parent.value())
                    && parent != 0
                {
                    entry.parent_id = Some(WireUuid(parent));
                }
                if scene.has_component::<Bone>(entity) {
                    entry.bone = Some(true);
                }
                entities.push(entry);
            }
            Ok(EntityList { entities })
        },
    );

    reg.register::<EmptyParams, ComponentList>(
        "list-components",
        "list registered component types",
        |ctx, _params| {
            let components = ctx
                .scene_edit
                .registry
                .rows()
                .iter()
                .map(|t| t.name.to_owned())
                .collect();
            Ok(ComponentList { components })
        },
    );

    reg.register::<CreateEntityParams, EntityRef>(
        "create-entity",
        "create-entity {name}",
        |ctx, params| {
            let entity = ctx.scene_edit.active_scene().create_entity(params.name);
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, DestroyEntityResult>(
        "destroy-entity",
        "destroy-entity {entity}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            let id = entity_uuid(scene, entity);
            // destroyEntity takes the whole subtree, so clear the selection when it sits
            // anywhere under the doomed root (walk the selection's ancestry).
            let selected = ctx.scene_edit.selected;
            let mut cursor =
                if selected != Entity::NULL && ctx.scene_edit.active_scene().valid(selected) {
                    Some(selected)
                } else {
                    None
                };
            while let Some(node) = cursor {
                if node == entity {
                    ctx.scene_edit.set_selection(Entity::NULL);
                    break;
                }
                cursor = ctx
                    .scene_edit
                    .active_scene()
                    .with_component::<Relationship, _>(node, |r| r.parent_handle)
                    .ok()
                    .flatten();
            }
            ctx.scene_edit.active_scene().destroy_entity(entity);
            ctx.scene_edit.scene_version += 1;
            Ok(DestroyEntityResult {
                destroyed: WireUuid(id),
            })
        },
    );

    reg.register::<SetParentParams, EntityRef>(
        "set-parent",
        "set-parent {entity, parent?} — reparent (absent/0 parent detaches to root)",
        |ctx, params| {
            let child = resolve_entity(ctx, &params.entity)?;
            let mut new_parent = None;
            if let Some(parent) = &params.parent
                && !is_root_selector(parent)
            {
                new_parent = Some(resolve_entity(ctx, parent)?);
            }
            // set_parent carries the self/cycle guards and the world-preserving rebase
            // (keep_world); the selection stays intact (only sceneVersion bumps).
            ctx.scene_edit
                .active_scene()
                .set_parent(child, new_parent, true)
                .map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, child))
        },
    );

    reg.register::<ComponentParams, AddComponentResult>(
        "add-component",
        "add-component {entity, component}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if (row.has)(ctx.scene_edit.active_scene(), entity) {
                return Err(Error::command(format!(
                    "entity already has '{}'",
                    params.component
                )));
            }
            (row.add_default)(ctx.scene_edit.active_scene(), entity).map_err(Error::command)?;
            // Auto-fit a Collider's shape to the entity mesh AABB on add (the locked
            // decision). The registry add hook can't see the asset/renderer handles, so it
            // runs here.
            if row.name == "Collider" {
                let _ = fit_collider(ctx, entity);
            } else if row.name == "KinematicBones" {
                // Auto-fit per-bone capsules through the shared physics helper.
                let _ = saffron_physics::fit_bone_capsules(ctx.scene_edit.active_scene(), entity);
            }
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry.append_component_order(scene, entity, row.name);
            ctx.scene_edit.scene_version += 1;
            Ok(AddComponentResult {
                added: row.name.to_owned(),
            })
        },
    );

    reg.register::<ComponentParams, RemoveComponentResult>(
        "remove-component",
        "remove-component {entity, component}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if !row.removable {
                return Err(Error::command(format!(
                    "component '{}' is not removable",
                    row.name
                )));
            }
            (row.remove)(ctx.scene_edit.active_scene(), entity);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry.remove_component_order(scene, entity, row.name);
            ctx.scene_edit.scene_version += 1;
            Ok(RemoveComponentResult {
                removed: row.name.to_owned(),
            })
        },
    );

    reg.register::<SetComponentOrderParams, SetComponentOrderResult>(
        "set-component-order",
        "set-component-order {entity, components}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry
                .set_component_order(scene, entity, params.components)
                .map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let components = registry.component_order(scene, entity);
            Ok(SetComponentOrderResult { components })
        },
    );

    // Applies a component's serialized form. Routing through the registry's deserialize
    // keeps the wire shape identical to scene files.
    reg.register::<SetComponentParams, SetComponentResult>(
        "set-component",
        "set-component {entity, component, json}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            let had_component = (row.has)(ctx.scene_edit.active_scene(), entity);
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &params.json)
                .map_err(Error::command)?;
            if !had_component {
                let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
                registry.append_component_order(scene, entity, row.name);
            }
            // A raw Relationship write changes the durable parent uuid; relink so the caches
            // follow (a cyclic parent is cut back to root with a warning).
            if row.name == "Relationship" {
                ctx.scene_edit.active_scene().relink_hierarchy();
            }
            ctx.scene_edit.scene_version += 1;
            Ok(SetComponentResult {
                set: row.name.to_owned(),
            })
        },
    );

    // Routes through the Transform row's deserialize so the wire shape matches scene files
    // exactly: {translation:{x,y,z}, rotation:{x,y,z} Euler radians, scale:{x,y,z}}.
    reg.register::<SetTransformParams, EntityRef>(
        "set-transform",
        "set-transform {entity, translation?, rotation?, scale?, smooth?:0|1}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name("Transform")
                .ok_or_else(|| Error::command("Transform component is not registered"))?;
            if !(row.has)(ctx.scene_edit.active_scene(), entity) {
                return Err(Error::command("entity has no Transform"));
            }
            // With preserve-children, freeze each direct child's world pose so the write
            // below can rebase their locals (the children visually stay put).
            let mut child_worlds: Vec<(Entity, Mat4)> = Vec::new();
            if ctx.scene_edit.preserve_children
                && ctx
                    .scene_edit
                    .active_scene()
                    .has_component::<Relationship>(entity)
            {
                let children = ctx
                    .scene_edit
                    .active_scene()
                    .with_component::<Relationship, _>(entity, |r| r.children.clone())
                    .unwrap_or_default();
                for child in children {
                    if ctx
                        .scene_edit
                        .active_scene()
                        .has_component::<Transform>(child)
                    {
                        let world = ctx.scene_edit.active_scene().compose_world_matrix(child);
                        child_worlds.push((child, world));
                    }
                }
            }
            // Smooth edits become per-frame animation targets (step_edit_smoothing) instead
            // of writes — except under preserve-children, where every write must rebase the
            // subtree, so the edit applies exact.
            if params.smooth.unwrap_or(false) && child_worlds.is_empty() {
                let target = ctx.scene_edit.transform_smooth_entry_for(entity);
                if let Some(t) = &params.translation {
                    target.translation = Some(to_glam3(*t));
                }
                if let Some(r) = &params.rotation {
                    target.rotation = Some(to_glam3(*r));
                }
                if let Some(s) = &params.scale {
                    target.scale = Some(to_glam3(*s));
                }
                ctx.scene_edit.scene_version += 1;
                let scene = ctx.scene_edit.active_scene();
                return Ok(entity_ref_dto(scene, entity));
            }
            ctx.scene_edit.cancel_transform_smoothing(entity);
            // Merge provided fields over the current transform so unspecified fields (e.g.
            // scale) are preserved rather than reset to defaults.
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), entity);
            if let Some(t) = &params.translation {
                body["translation"] = vec3_json(t);
            }
            if let Some(r) = &params.rotation {
                body["rotation"] = vec3_json(r);
            }
            if let Some(s) = &params.scale {
                body["scale"] = vec3_json(s);
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &body)
                .map_err(Error::command)?;
            if !child_worlds.is_empty() {
                let inv_world = ctx
                    .scene_edit
                    .active_scene()
                    .compose_world_matrix(entity)
                    .inverse();
                for (child, world) in child_worlds {
                    ctx.scene_edit
                        .active_scene()
                        .set_local_from_matrix(child, inv_world * world);
                }
            }
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    // Sets the directional light (the given entity, else the first one), merging provided
    // fields (direction/color as {x,y,z}) over its current value.
    reg.register::<SetLightParams, EntityRef>(
        "set-light",
        "set-light {entity?, direction?, color?, intensity?, ambient?}",
        |ctx, params| {
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name("DirectionalLight")
                .ok_or_else(|| Error::command("DirectionalLight component is not registered"))?;
            let target = if let Some(selector) = &params.entity {
                resolve_entity(ctx, selector)?
            } else {
                let mut found = Entity::NULL;
                ctx.scene_edit
                    .active_scene()
                    .for_each::<&DirectionalLight, _>(|entity, _| {
                        if found == Entity::NULL {
                            found = entity;
                        }
                    });
                found
            };
            if target == Entity::NULL || !(row.has)(ctx.scene_edit.active_scene(), target) {
                return Err(Error::command("no DirectionalLight to set"));
            }
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), target);
            if let Some(d) = &params.direction {
                body["direction"] = vec3_json(d);
            }
            if let Some(c) = &params.color {
                body["color"] = vec3_json(c);
            }
            if let Some(i) = params.intensity {
                body["intensity"] = json!(i);
            }
            if let Some(a) = params.ambient {
                body["ambient"] = json!(a);
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), target, &body)
                .map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, target))
        },
    );
}

/// Registers inspect, focus, and world-transform.
pub(crate) fn register_inspection(reg: &mut CommandRegistry) {
    reg.register::<EntityParams, InspectResult>(
        "inspect",
        "inspect {entity} — dump all its components as json",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let mut components = Map::new();
            // The registry rows are `Copy`; snapshot them so the per-row active-scene borrow
            // does not overlap the registry borrow.
            let rows: Vec<ComponentTraits> = ctx.scene_edit.registry.rows().to_vec();
            for row in rows {
                let scene = ctx.scene_edit.active_scene();
                if (row.has)(scene, entity) {
                    components.insert(row.name.to_owned(), (row.serialize)(scene, entity));
                }
            }
            let scene = ctx.scene_edit.active_scene();
            let r = entity_ref_dto(scene, entity);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let component_order = registry.component_order(scene, entity);
            Ok(InspectResult {
                id: r.id,
                name: r.name,
                components: Value::Object(components),
                component_order,
            })
        },
    );

    reg.register::<EntityParams, EntityRef>(
        "focus",
        "focus {entity} — aim the editor camera at it",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if !ctx
                .scene_edit
                .active_scene()
                .has_component::<Transform>(entity)
            {
                return Err(Error::command("entity has no Transform"));
            }
            let fovy = ctx.scene_edit.camera.fov.to_radians();
            let forward = ctx.scene_edit.camera.forward();
            // Frame the whole model: union the forest's mesh AABB and pull the camera back to
            // fit it, rather than aiming at the container pivot at a fixed distance (which
            // mis-frames large or off-pivot models).
            let scene = ctx.scene_edit.active_scene();
            let assets = &mut *ctx.assets;
            let mut bounds = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                bounds = model_render_aabb(gpu, scene, assets, entity);
            });
            let (target, distance) = match bounds {
                Some((lo, hi)) => {
                    let center = (lo + hi) * 0.5;
                    let radius = (hi - lo).length() * 0.5;
                    (center, (radius / (fovy * 0.5).tan() * 1.3).max(0.5))
                }
                None => (ctx.scene_edit.active_scene().world_translation(entity), 5.0),
            };
            ctx.scene_edit.camera.position = target - forward * distance;
            // Framing an entity jumps the eye — snap the target so it does not ease back.
            ctx.scene_edit.camera.sync_target();
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, saffron_protocol::WorldTransformResult>(
        "get-world-transform",
        "get-world-transform {entity} — the entity's composed world translation + scale",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let world = ctx.scene_edit.active_scene().world_matrix(entity);
            let t = world.w_axis.truncate();
            let s = GlamVec3::new(
                world.x_axis.truncate().length(),
                world.y_axis.truncate().length(),
                world.z_axis.truncate().length(),
            );
            Ok(saffron_protocol::WorldTransformResult {
                translation: from_glam3(t),
                scale: from_glam3(s),
            })
        },
    );
}

/// Registers entity authoring: presets, copy, rename, and component-field edits.
pub(crate) fn register_entity_authoring(reg: &mut CommandRegistry) {
    reg.register::<AddEntityParams, EntityRef>(
        "add-entity",
        "add-entity {preset=empty|cube|plane|sphere|point-light|spot-light|directional-light|camera|reflection-probe|fog-volume}",
        |ctx, params| {
            let preset = params.preset.unwrap_or(AddEntityPreset::Empty);
            let entity = match preset {
                AddEntityPreset::Empty => ctx.scene_edit.active_scene().create_entity("Entity"),
                AddEntityPreset::Cube | AddEntityPreset::Plane | AddEntityPreset::Sphere => {
                    // Built-in primitives are native geometry: a reserved-id mesh (seeded on
                    // demand, never a catalog asset) plus a default material slot. No project
                    // required.
                    let (builtin, name) = match preset {
                        AddEntityPreset::Plane => (BuiltinMesh::Plane, "Plane"),
                        AddEntityPreset::Sphere => (BuiltinMesh::Sphere, "Sphere"),
                        _ => (BuiltinMesh::Cube, "Cube"),
                    };
                    let scene = ctx.scene_edit.active_scene();
                    let e = scene.create_entity(name);
                    let _ = scene.add_component(
                        e,
                        Mesh {
                            mesh: builtin.reserved_id(),
                        },
                    );
                    let _ = scene.add_component(
                        e,
                        MaterialSet {
                            slots: vec![MaterialSlot::default()],
                        },
                    );
                    e
                }
                AddEntityPreset::PointLight => {
                    let e = ctx.scene_edit.active_scene().create_entity("Point Light");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, PointLight::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::SpotLight => {
                    let e = ctx.scene_edit.active_scene().create_entity("Spot Light");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, SpotLight::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 4.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::DirectionalLight => {
                    let e = ctx
                        .scene_edit
                        .active_scene()
                        .create_entity("Directional Light");
                    let _ = ctx
                        .scene_edit
                        .active_scene()
                        .add_component(e, DirectionalLight::default());
                    e
                }
                AddEntityPreset::Camera => {
                    let e = ctx.scene_edit.active_scene().create_entity("Camera");
                    let _ = ctx
                        .scene_edit
                        .active_scene()
                        .add_component(e, Camera::default());
                    e
                }
                AddEntityPreset::ReflectionProbe => {
                    let e = ctx
                        .scene_edit
                        .active_scene()
                        .create_entity("Reflection Probe");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, saffron_scene::ReflectionProbe::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::FogVolume => {
                    let e = ctx.scene_edit.active_scene().create_entity("Fog Volume");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, saffron_scene::FogVolume::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
            };
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(entity);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, EntityRef>(
        "copy-entity",
        "copy-entity {entity} — deep-duplicate it (selects the copy)",
        |ctx, params| {
            let src = resolve_entity(ctx, &params.entity)?;
            let src_name = ctx
                .scene_edit
                .active_scene()
                .with_component::<Name, _>(src, |n| n.name.clone())
                .unwrap_or_default();
            let copy_name = format!("{src_name} (copy)");
            let fresh = ctx
                .scene_edit
                .active_scene()
                .create_entity(copy_name.clone());
            // deserialize add-defaults each missing component and applies fromJson, so we do
            // not call addDefault (which would double-emplace Name/Transform that
            // create_entity already added). Copying the Name component overwrites the
            // "(copy)" suffix, so restore it afterwards.
            let rows: Vec<ComponentTraits> = ctx.scene_edit.registry.rows().to_vec();
            for row in rows {
                let scene = ctx.scene_edit.active_scene();
                if (row.has)(scene, src) {
                    let body = (row.serialize)(scene, src);
                    let _ = (row.deserialize)(scene, fresh, &body);
                }
            }
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<Name, _>(fresh, |n| n.name = copy_name);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let src_order = registry.component_order(scene, src);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let _ = registry.set_component_order(scene, fresh, src_order);
            // The round-trip duplicated the source's parent uuid (the copy joins the source's
            // parent as a sibling); relink so the copy lands in its parent's children cache.
            ctx.scene_edit.active_scene().relink_hierarchy();
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(fresh);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, fresh))
        },
    );

    reg.register::<RenameEntityParams, EntityRef>(
        "rename-entity",
        "rename-entity {entity, name} — set its Name component",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if params.name.is_empty() {
                return Err(Error::command("usage: rename-entity {entity, name}"));
            }
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<Name, _>(entity, |n| n.name = params.name.clone());
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<SetComponentFieldParams, SetComponentFieldResult>(
        "set-component-field",
        "set-component-field {entity, component, field, value} — merge one field (value may \
         be a uuid string, number, bool, or json object)",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if params.component.is_empty() || params.field.is_empty() {
                return Err(Error::command(
                    "usage: set-component-field {entity, component, field, value}",
                ));
            }
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if !(row.has)(ctx.scene_edit.active_scene(), entity) {
                (row.add_default)(ctx.scene_edit.active_scene(), entity).map_err(Error::command)?;
            }
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), entity);
            // The CLI passes every value as a string; a fully-numeric one becomes a u64 so
            // numeric/id fields land as numbers, while non-numeric strings pass through.
            let mut value = params.value.clone();
            if let Some(s) = value.as_str()
                && let Ok(n) = s.parse::<u64>()
            {
                value = json!(n);
            }
            if let Some(index) = params.index {
                // Address one element of an array field: an object value merges its keys into
                // body[field][index] (a partial edit), any other value replaces the element.
                let array = body.get_mut(&params.field).and_then(Value::as_array_mut);
                let out_of_range = array
                    .as_ref()
                    .is_none_or(|a| index < 0 || index as usize >= a.len());
                if out_of_range {
                    return Err(Error::command(format!(
                        "'{}.{}' has no array index {}",
                        params.component, params.field, index
                    )));
                }
                let element = &mut body[&params.field][index as usize];
                if let Value::Object(map) = value {
                    for (key, sub) in map {
                        element[&key] = sub;
                    }
                } else {
                    *element = value;
                }
            } else {
                body[&params.field] = value;
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &body)
                .map_err(Error::command)?;
            // A raw Relationship write changes the durable parent uuid; relink so the caches
            // follow (a cyclic parent is cut back to root with a warning).
            if row.name == "Relationship" {
                ctx.scene_edit.active_scene().relink_hierarchy();
            }
            ctx.scene_edit.scene_version += 1;
            Ok(SetComponentFieldResult {
                set: row.name.to_owned(),
                field: params.field,
            })
        },
    );
}
