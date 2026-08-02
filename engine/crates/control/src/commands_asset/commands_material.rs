use saffron_assets::{
    default_material_asset, exposed_parameter, import_material_folder, load_catalog_material_asset,
    load_catalog_material_asset_raw, lower_graph_to_params, pbr_exposed_parameters,
    save_material_asset, update_material_asset,
};
use saffron_core::{HeightMode, Uuid};
use saffron_protocol::{
    EmptyParams, ExposedParamDto, MaterialAssignParams, MaterialAssignResult,
    MaterialCompileParams, MaterialCompileResult, MaterialCookResult, MaterialCreateInstanceParams,
    MaterialCreateParams, MaterialCreateResult, MaterialGetParams, MaterialGetResult,
    MaterialImportParams, MaterialImportResultDto, MaterialListResult, MaterialRefDto,
    MaterialSchemaParams, MaterialSchemaResult, MaterialSetGraphParams, MaterialSetGraphResult,
    MaterialSetOverrideParams, MaterialSetOverrideResult, MaterialUpdateParams,
    MaterialUpdateResult, Uuid as WireUuid,
};
use saffron_scene::{AssetType, MaterialSet, MaterialSlot};
use serde_json::json;

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::resolve_entity;

/// Registers the `material-*` commands.
pub(crate) fn register_material(reg: &mut CommandRegistry) {
    reg.register::<MaterialCreateParams, MaterialCreateResult>(
        "material-create",
        "material-create {name} [from-entity]",
        |ctx, params| {
            let asset = default_material_asset();
            let name = if params.name.is_empty() {
                "Material".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &asset, &name, "").map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialAssignParams, MaterialAssignResult>(
        "material-assign",
        "material-assign {entity, material:id|name}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let selector = selector_string(&params.material);
            let clearing =
                selector == "0" || selector.is_empty() || params.material.id() == Some(0);
            let mat_id = if clearing {
                Uuid(0)
            } else {
                resolve_asset(ctx, &params.material)?
            };
            let scene = ctx.scene_edit.active_scene();
            // Assign to every mesh-bearing entity in the model's forest — the renderer reads
            // the material off the entity that carries the mesh, which on a multi-node model is
            // a child of the resolved container, not the container itself. A leaf selection
            // resolves to just itself; a non-mesh selection still takes the component directly.
            let mut targets = scene.model_mesh_entities(entity);
            if targets.is_empty() {
                targets.push(entity);
            }
            for target in targets {
                ensure_material_slot(scene, target);
                let _ = scene.with_component_mut::<MaterialSet, _>(target, |set| {
                    if set.slots.is_empty() {
                        set.slots.push(MaterialSlot::default());
                    }
                    for slot in &mut set.slots {
                        slot.material = mat_id;
                    }
                });
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialAssignResult {
                material: WireUuid(mat_id.value()),
            })
        },
    );

    reg.register::<MaterialImportParams, MaterialImportResultDto>(
        "material-import",
        "material-import {path} [name]",
        |ctx, params| {
            // Baking the material container is pure disk — no GPU uploader needed; the maps
            // load lazily from the container when the material is first rendered / previewed.
            let imported = import_material_folder(&mut *ctx.assets, &params.path, &params.name)
                .map_err(Error::command)?;
            if let Some(attribution) = params.attribution {
                let _ = ctx
                    .assets
                    .set_asset_attribution(imported.material, attribution_from_dto(attribution));
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialImportResultDto {
                id: WireUuid(imported.material.value()),
                roles: imported.roles,
            })
        },
    );

    reg.register::<EmptyParams, MaterialListResult>(
        "material-list",
        "material-list",
        |ctx, _params| {
            let materials = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| MaterialRefDto {
                    id: WireUuid(e.id.value()),
                    name: e.name.clone(),
                    folder: e.folder.clone(),
                })
                .collect();
            Ok(MaterialListResult { materials })
        },
    );

    reg.register::<MaterialGetParams, MaterialGetResult>(
        "material-get",
        "material-get {id|name}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let m = load_catalog_material_asset(ctx.assets, id).map_err(Error::command)?;
            let graph = load_catalog_material_asset_raw(ctx.assets, id)
                .ok()
                .filter(|raw| raw.graph.is_object())
                .map_or_else(|| json!({}), |raw| raw.graph);
            Ok(MaterialGetResult {
                id: WireUuid(id.value()),
                surface: material_surface_dto(&m.surface),
                blend: m.blend.clone(),
                unlit: m.unlit,
                base_color: vec4(m.base_color),
                metallic: m.metallic,
                roughness: m.roughness,
                emissive: vec3(m.emissive),
                emissive_strength: m.emissive_strength,
                height_scale: m.height_scale,
                height_mode: m.height_mode.as_wire().to_owned(),
                albedo_texture: WireUuid(m.albedo_texture.value()),
                orm_texture: WireUuid(m.orm_texture.value()),
                normal_texture: WireUuid(m.normal_texture.value()),
                emissive_texture: WireUuid(m.emissive_texture.value()),
                height_texture: WireUuid(m.height_texture.value()),
                vector_displacement_texture: WireUuid(m.vector_displacement_texture.value()),
                graph,
            })
        },
    );

    reg.register::<MaterialSchemaParams, MaterialSchemaResult>(
        "material-schema",
        "material-schema {id|name} — the material's exposed override parameters",
        |ctx, params| {
            // Validate the material resolves; the exposed set is the fixed übershader's for
            // now (the same list a `MaterialSet` slot's overrides validate against).
            resolve_asset(ctx, &params.material)?;
            let params = pbr_exposed_parameters()
                .into_iter()
                .map(|p| ExposedParamDto {
                    name: p.name.to_owned(),
                    kind: p.kind.as_wire().to_owned(),
                    default: p.default,
                })
                .collect();
            Ok(MaterialSchemaResult { params })
        },
    );

    reg.register::<MaterialUpdateParams, MaterialUpdateResult>(
        "material-update",
        "material-update {id} [baseColor metallic roughness emissive emissiveStrength]",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m = load_catalog_material_asset(ctx.assets, id).map_err(Error::command)?;
            if let Some(surface) = params.surface {
                m.surface = material_surface_from_dto(surface)?;
            }
            if let Some(base) = params.base_color {
                m.base_color = from_vec4(base);
            }
            if let Some(metallic) = params.metallic {
                m.metallic = metallic;
            }
            if let Some(roughness) = params.roughness {
                m.roughness = roughness;
            }
            if let Some(emissive) = params.emissive {
                m.emissive = from_vec3(emissive);
            }
            if let Some(strength) = params.emissive_strength {
                m.emissive_strength = strength;
            }
            if let Some(normal_strength) = params.normal_strength {
                m.normal_strength = normal_strength;
            }
            if let Some(height_scale) = params.height_scale {
                m.height_scale = height_scale;
            }
            if let Some(height_mode) = &params.height_mode {
                m.height_mode = HeightMode::from_wire(height_mode);
            }
            if let Some(tex) = params.albedo_texture {
                m.albedo_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.orm_texture {
                m.orm_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.normal_texture {
                m.normal_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.emissive_texture {
                m.emissive_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.height_texture {
                m.height_texture = Uuid(tex.0);
            }
            if let Some(tex) = params.vector_displacement_texture {
                m.vector_displacement_texture = Uuid(tex.0);
            }
            update_material_asset(ctx.assets, id, &m).map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialUpdateResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<MaterialSetGraphParams, MaterialSetGraphResult>(
        "material-set-graph",
        "material-set-graph {material, graph}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let mut m = load_catalog_material_asset(ctx.assets, id).map_err(Error::command)?;
            m.graph = params.graph.clone();
            let mut folded = m.clone();
            let foldable = lower_graph_to_params(&m.graph, &mut folded);
            if foldable {
                m = folded;
            }
            update_material_asset(ctx.assets, id, &m).map_err(Error::command)?;
            if !foldable {
                ctx.assets
                    .compile_material_mesh_shader(&m.graph, id)
                    .map_err(Error::command)?;
            }
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetGraphResult {
                id: WireUuid(id.value()),
                foldable,
            })
        },
    );

    reg.register::<MaterialCreateInstanceParams, MaterialCreateResult>(
        "material-create-instance",
        "material-create-instance {parent} [name]",
        |ctx, params| {
            let parent = resolve_asset(ctx, &params.parent)?;
            let mut child = default_material_asset();
            child.parent = parent;
            let name = if params.name.is_empty() {
                "Instance".to_owned()
            } else {
                params.name.clone()
            };
            let id = save_material_asset(ctx.assets, &child, &name, "").map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialCreateResult {
                id: WireUuid(id.value()),
                name,
            })
        },
    );

    reg.register::<MaterialSetOverrideParams, MaterialSetOverrideResult>(
        "material-set-override",
        "material-set-override {material, field, value}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            // The override key must be one of the material's exposed parameters, and the
            // value must be well-typed for its kind — the schema is the single gate.
            let param = exposed_parameter(&params.field).ok_or_else(|| {
                Error::command(format!("unknown material parameter '{}'", params.field))
            })?;
            if !param.kind.accepts(&params.value) {
                return Err(Error::command(format!(
                    "material parameter '{}' expects a {} value",
                    params.field,
                    param.kind.as_wire()
                )));
            }
            let mut m = load_catalog_material_asset_raw(ctx.assets, id).map_err(Error::command)?;
            if !m.overrides.is_object() {
                m.overrides = json!({});
            }
            if let Some(map) = m.overrides.as_object_mut() {
                map.insert(params.field.clone(), params.value.clone());
            }
            update_material_asset(ctx.assets, id, &m).map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(MaterialSetOverrideResult {
                id: WireUuid(id.value()),
            })
        },
    );

    reg.register::<MaterialCompileParams, MaterialCompileResult>(
        "material-compile-graph",
        "material-compile-graph {material}",
        |ctx, params| {
            let id = resolve_asset(ctx, &params.material)?;
            let raw = load_catalog_material_asset_raw(ctx.assets, id).map_err(Error::command)?;
            if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                return Err(Error::command("material has no node graph to compile"));
            }
            ctx.assets
                .compile_material_graph(&raw.graph, id)
                .map_err(Error::command)?;
            let _ = ctx.assets.asset_edited(id);
            Ok(MaterialCompileResult {
                id: WireUuid(id.value()),
                ok: true,
            })
        },
    );

    reg.register::<EmptyParams, MaterialCookResult>(
        "material-cook",
        "material-cook",
        |ctx, _params| {
            let material_ids: Vec<Uuid> = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|e| e.asset_type == AssetType::Material)
                .map(|e| e.id)
                .collect();
            let mut compiled = 0u32;
            let mut failed = 0u32;
            for id in material_ids {
                let Ok(raw) = load_catalog_material_asset_raw(ctx.assets, id) else {
                    continue;
                };
                if !raw.graph.is_object() || raw.graph.as_object().is_none_or(|g| g.is_empty()) {
                    continue;
                }
                let mut probe = raw.clone();
                if lower_graph_to_params(&raw.graph, &mut probe) {
                    continue;
                }
                if ctx
                    .assets
                    .compile_material_mesh_shader(&raw.graph, id)
                    .is_ok()
                {
                    let _ = ctx.assets.asset_edited(id);
                    compiled += 1;
                } else {
                    failed += 1;
                }
            }
            Ok(MaterialCookResult { compiled, failed })
        },
    );
}
