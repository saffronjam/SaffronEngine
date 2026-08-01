use saffron_assets::{
    AssetServer, BUILTIN_SPHERE_MESH_ID, BuiltinMesh, ContainerMetadata, MaterialAsset,
    PREVIEW_MATERIAL_ID, PlantRecookOptions, PlantRecookOutcome, default_material_asset,
    load_plant_family_asset, portable_vegetation_platform_profile, recook_plant_family,
    request_thumbnail, vegetation_cook_versions,
};
use saffron_core::{HeightMode, Uuid};
use saffron_protocol::{
    AnimationClipDto, BoneDto, GetAssetModelParams, ThumbnailFormatDto, ThumbnailParams,
    ThumbnailResult, Uuid as WireUuid,
};
use saffron_scene::{
    AnimationPlayer, AssetType, Entity, IdComponent, MaterialSet, MaterialSlot, Mesh, Scene,
    SkinnedMesh, TextureRole, Transform,
};
use saffron_sceneedit::{PlayState, ProjectLoadRequest, ProjectPhase};
use saffron_spatial::UnitInterval;
use saffron_vegetation::PlantCompileLimits;
use serde_json::{Value, json};

use super::*;
use crate::error::{Error, Result};
use crate::registry::EngineContext;

/// Resolves `{asset, size?}` to a base64-PNG thumbnail reply. [`request_thumbnail`] classifies the
/// asset and either returns a cache hit or enqueues a main-graph render (reply `pending`); the host
/// drains the queue in `on_update`. Shared by `get-thumbnail` (128) + `view-asset` (512).
pub(crate) fn thumbnail_result(
    ctx: &mut EngineContext<'_>,
    params: &ThumbnailParams,
    default_size: u32,
) -> Result<ThumbnailResult> {
    let id = resolve_asset(ctx, &params.asset)?;
    let size = u32::try_from(params.size.unwrap_or(default_size as i32)).unwrap_or(default_size);
    let reply = request_thumbnail(&mut *ctx.assets, id, size).map_err(Error::command)?;
    if reply.pending {
        return Ok(ThumbnailResult {
            id: WireUuid(id.value()),
            format: ThumbnailFormatDto::Png,
            width: 0,
            height: 0,
            base64: String::new(),
            pending: true,
        });
    }
    Ok(ThumbnailResult {
        id: WireUuid(id.value()),
        format: ThumbnailFormatDto::Png,
        width: i32::try_from(reply.width).unwrap_or(0),
        height: i32::try_from(reply.height).unwrap_or(0),
        base64: base64_encode(&reply.png),
        pending: false,
    })
}

/// Drops the asset preview and restores the authored edit state. A no-op when no preview
/// is alive.
pub(crate) fn leave_asset_preview(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() {
        return;
    }
    let was_active = ctx.preview_active_view;
    ctx.preview_scene = None;
    ctx.preview_asset = Uuid(0);
    ctx.preview_root_entity = Entity::NULL;
    ctx.preview_floor_entity = Entity::NULL;
    ctx.preview_bone_by_node.clear();
    ctx.preview_active_view = false;
    if was_active {
        ctx.camera = ctx.saved_camera;
        ctx.skeleton_overlay = ctx.saved_overlay;
        let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection)
        {
            ctx.saved_selection
        } else {
            Entity::NULL
        };
        ctx.saved_selection = Entity::NULL;
        ctx.set_selection(restore);
    }
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Parks the preview orbit and restores the authored fly-cam/overlay/selection so the scene
/// view shows the authored scene. A no-op unless the preview is the active view.
pub(crate) fn deactivate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || !ctx.preview_active_view {
        return;
    }
    ctx.parked_preview_camera = ctx.camera;
    ctx.camera = ctx.saved_camera;
    ctx.skeleton_overlay = ctx.saved_overlay;
    let restore = if ctx.saved_selection != Entity::NULL && ctx.scene.valid(ctx.saved_selection) {
        ctx.saved_selection
    } else {
        Entity::NULL
    };
    ctx.set_selection(restore);
    ctx.preview_active_view = false;
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// Re-stashes the authored view and restores the parked preview orbit + overlay + selected
/// root. A no-op unless a preview scene is alive but not currently active.
pub(crate) fn activate_preview_view(ctx: &mut saffron_sceneedit::SceneEditContext) {
    if ctx.preview_scene.is_none() || ctx.preview_active_view {
        return;
    }
    ctx.saved_camera = ctx.camera;
    ctx.saved_selection = ctx.selected;
    ctx.saved_overlay = ctx.skeleton_overlay;
    ctx.camera = ctx.parked_preview_camera;
    ctx.skeleton_overlay.show = true;
    ctx.skeleton_overlay.highlight_joint = -1;
    ctx.preview_active_view = true;
    let root = ctx.preview_root_entity;
    ctx.set_selection(root);
    ctx.scene_version += 1;
    ctx.animation_version += 1;
}

/// The flat parent-indexed bone tree for a skinned container's nodes (the
/// `get-asset-model` rig walk): the joints plus their ancestor chains bounded at the
/// skeleton root, with node indices preserved.
pub(crate) fn build_bone_tree(meta: &ContainerMetadata) -> Vec<BoneDto> {
    let node_count = meta.nodes.as_array().map_or(0, Vec::len);
    let nodes = meta.nodes.as_array();
    let parents: Vec<i32> = (0..node_count)
        .map(|i| {
            nodes
                .and_then(|n| n[i].get("parent"))
                .and_then(Value::as_i64)
                .map_or(-1, |v| v as i32)
        })
        .collect();
    let mut is_joint = vec![false; node_count];
    let skeleton_root = meta
        .skin
        .get("skeletonRoot")
        .and_then(Value::as_i64)
        .map_or(-1, |v| v as i32);
    if let Some(joints) = meta.skin.get("joints").and_then(Value::as_array) {
        for joint in joints {
            if let Some(index) = joint.as_i64()
                && index >= 0
                && (index as usize) < node_count
            {
                is_joint[index as usize] = true;
            }
        }
    }
    let mut in_rig = vec![false; node_count];
    if skeleton_root >= 0 && (skeleton_root as usize) < node_count {
        in_rig[skeleton_root as usize] = true;
    }
    for start in is_joint
        .iter()
        .enumerate()
        .filter_map(|(i, &joint)| joint.then_some(i as i32))
    {
        let mut node = start;
        while node >= 0 && (node as usize) < node_count && !in_rig[node as usize] {
            in_rig[node as usize] = true;
            if node == skeleton_root {
                break;
            }
            node = parents[node as usize];
        }
    }
    let mut bones = Vec::new();
    for i in 0..node_count {
        if !in_rig[i] {
            continue;
        }
        let parent = parents[i];
        let parent = if parent >= 0 && (parent as usize) < node_count && in_rig[parent as usize] {
            parent
        } else {
            -1
        };
        bones.push(BoneDto {
            index: i as i32,
            name: nodes
                .and_then(|n| n[i].get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent,
            joint: is_joint[i],
        });
    }
    bones
}

/// The animation sub-asset clips a container carries (the `get-asset-model` clip walk), each
/// with its real per-channel keyframe data loaded from the clip. No live forest here, so
/// channel labels are the raw glTF target names.
pub(crate) fn container_clips(
    assets: &mut saffron_assets::AssetServer,
    meta: &ContainerMetadata,
) -> Vec<AnimationClipDto> {
    let subs: Vec<(u64, String, f32)> = meta
        .sub_assets
        .iter()
        .filter(|sub| sub.asset_type == AssetType::Animation)
        .map(|sub| (sub.sub_id.value(), sub.name.clone(), sub.duration))
        .collect();
    subs.into_iter()
        .map(|(sub_id, name, duration)| {
            let channels = assets
                .load_anim_clip(saffron_core::Uuid(sub_id))
                .map(|clip| {
                    crate::commands_animation::channels_of(&clip, |track| track.target_name.clone())
                })
                .unwrap_or_default();
            AnimationClipDto {
                id: WireUuid(sub_id),
                name,
                duration,
                channels,
            }
        })
        .collect()
}

/// Grows one native family's graph and reports what it produced.
/// The growth budget a preview request names, or the cook budget when it names none.
pub(crate) fn preview_budget(
    axes: Option<u32>,
    elements: Option<u32>,
) -> saffron_vegetation::BotanicalBudget {
    saffron_vegetation::BotanicalBudget {
        axes: axes.map(|value| value as usize),
        elements: elements.map(|value| value as usize),
        cancellation: None,
    }
}

/// One authored appearance on the wire.
pub(crate) fn phenotype_dto(
    phenotype: &saffron_vegetation::PlantPhenotype,
) -> saffron_protocol::PlantPhenotypeDto {
    saffron_protocol::PlantPhenotypeDto {
        id: phenotype.id,
        role: phenotype_role_dto(phenotype.role),
        variation: phenotype.variation,
        season_window: phenotype
            .response
            .season_window
            .map(|(start, end)| [u32::from(start), u32::from(end)]),
        health_band: phenotype.response.health_band.map(unit_band_dto),
        moisture_band: phenotype.response.moisture_band.map(unit_band_dto),
        ramp_mille: u32::from(phenotype.response.ramp_mille),
        material_remap: phenotype
            .material_remap
            .iter()
            .map(|(from, to)| [*from, *to])
            .collect(),
        // Part identities are u128 and stay strings end to end; narrowing one through a JSON
        // number would corrupt it silently.
        active_parts: phenotype
            .active_parts
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

/// One phenotype role on the wire.
pub(crate) fn phenotype_role_dto(
    role: saffron_vegetation::PhenotypeRole,
) -> saffron_protocol::PhenotypeRoleDto {
    use saffron_vegetation::PhenotypeRole;
    match role {
        PhenotypeRole::Healthy => saffron_protocol::PhenotypeRoleDto::Healthy,
        PhenotypeRole::Harvested => saffron_protocol::PhenotypeRoleDto::Harvested,
        PhenotypeRole::Damaged => saffron_protocol::PhenotypeRoleDto::Damaged,
        PhenotypeRole::Burned => saffron_protocol::PhenotypeRoleDto::Burned,
        PhenotypeRole::Dead => saffron_protocol::PhenotypeRoleDto::Dead,
        PhenotypeRole::Flowering => saffron_protocol::PhenotypeRoleDto::Flowering,
        PhenotypeRole::Fruiting => saffron_protocol::PhenotypeRoleDto::Fruiting,
        PhenotypeRole::Senescent => saffron_protocol::PhenotypeRoleDto::Senescent,
        PhenotypeRole::Wet => saffron_protocol::PhenotypeRoleDto::Wet,
    }
}

/// A unit band as per-mille endpoints.
fn unit_band_dto(band: (UnitInterval, UnitInterval)) -> [u32; 2] {
    let mille = |unit: UnitInterval| u32::from(unit.bits()) * 1000 / u32::from(u16::MAX);
    [mille(band.0), mille(band.1)]
}

/// Per-mille endpoints back to a unit band, rejecting an inverted or out-of-range one.
fn unit_band_from_dto(band: [u32; 2]) -> Result<(UnitInterval, UnitInterval)> {
    let unit = |value: u32| {
        (value <= 1000)
            .then(|| UnitInterval::from_bits((value * u32::from(u16::MAX) / 1000) as u16))
            .ok_or_else(|| Error::command("a phenotype band is per-mille (0..=1000)"))
    };
    let (low, high) = (unit(band[0])?, unit(band[1])?);
    if high <= low {
        return Err(Error::command(
            "a phenotype band's high must exceed its low",
        ));
    }
    Ok((low, high))
}

pub(crate) fn phenotype_from_dto(
    dto: &saffron_protocol::PlantPhenotypeDto,
) -> Result<saffron_vegetation::PlantPhenotype> {
    use saffron_vegetation::PhenotypeRole;
    let season_window = dto
        .season_window
        .map(|[start, end]| {
            let narrow = |value: u32| {
                u16::try_from(value)
                    .ok()
                    .filter(|value| *value < 1000)
                    .ok_or_else(|| {
                        Error::command("seasonWindow is per-mille of the year (0..1000)")
                    })
            };
            Result::Ok((narrow(start)?, narrow(end)?))
        })
        .transpose()?;
    let health_band = dto.health_band.map(unit_band_from_dto).transpose()?;
    let moisture_band = dto.moisture_band.map(unit_band_from_dto).transpose()?;
    let ramp_mille = u16::try_from(dto.ramp_mille)
        .ok()
        .filter(|value| *value <= 500)
        .ok_or_else(|| Error::command("rampMille is per-mille of a band (0..=500)"))?;
    Ok(saffron_vegetation::PlantPhenotype {
        id: dto.id,
        role: match dto.role {
            saffron_protocol::PhenotypeRoleDto::Healthy => PhenotypeRole::Healthy,
            saffron_protocol::PhenotypeRoleDto::Harvested => PhenotypeRole::Harvested,
            saffron_protocol::PhenotypeRoleDto::Damaged => PhenotypeRole::Damaged,
            saffron_protocol::PhenotypeRoleDto::Burned => PhenotypeRole::Burned,
            saffron_protocol::PhenotypeRoleDto::Dead => PhenotypeRole::Dead,
            saffron_protocol::PhenotypeRoleDto::Flowering => PhenotypeRole::Flowering,
            saffron_protocol::PhenotypeRoleDto::Fruiting => PhenotypeRole::Fruiting,
            saffron_protocol::PhenotypeRoleDto::Senescent => PhenotypeRole::Senescent,
            saffron_protocol::PhenotypeRoleDto::Wet => PhenotypeRole::Wet,
        },
        response: saffron_vegetation::PhenotypeResponse {
            season_window,
            health_band,
            moisture_band,
            ramp_mille,
        },
        variation: dto.variation,
        material_remap: dto
            .material_remap
            .iter()
            .map(|[from, to]| (*from, *to))
            .collect(),
        active_parts: dto
            .active_parts
            .iter()
            .map(|part| {
                part.parse::<u128>()
                    .map_err(|_| Error::command("activeParts entries are decimal part identities"))
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

pub(crate) fn plant_growth_dto(
    assets: &saffron_assets::AssetServer,
    family: &saffron_vegetation::PlantFamilyAsset,
    variation: u32,
    budget: &saffron_vegetation::BotanicalBudget,
) -> Result<saffron_protocol::BotanicalGrowthDto> {
    let saffron_vegetation::PlantFamilySource::Native { graph, .. } = &family.source else {
        return Err(Error::command(
            "plant family has an imported source, not a botanical graph",
        ));
    };
    let individual = graph
        .variations
        .get(variation as usize)
        .ok_or_else(|| Error::command(format!("plant family declares no variation {variation}")))?;
    let modules = saffron_assets::PlantModules::for_family(assets, family);
    let growth = saffron_vegetation::grow(graph, variation as usize, &modules, budget)
        .map_err(Error::command)?;
    let assembly = &growth.assembly;
    let geometry = saffron_vegetation::normalize_botanical_geometry(
        saffron_vegetation::native_plant_source_id(family.id),
        assembly,
        &std::collections::BTreeMap::new(),
    )
    .map_err(Error::command)?;
    let structure =
        saffron_vegetation::derive_family_structure(assembly).map_err(Error::command)?;
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    Ok(saffron_protocol::BotanicalGrowthDto {
        truncated: growth.truncated,
        graph: graph.identity().to_string(),
        variation,
        seed: individual.seed.to_string(),
        age: individual.age.bits(),
        variations: count(graph.variations.len()),
        axes: count(assembly.axes.len()),
        frames: count(assembly.frames.len()),
        shells: count(assembly.shells.len()),
        elements: count(assembly.elements.len()),
        vertices: count(geometry.meshes.iter().map(|mesh| mesh.vertices.len()).sum()),
        triangles: count(
            geometry
                .meshes
                .iter()
                .map(|mesh| mesh.indices.len() / 3)
                .sum(),
        ),
        parts: count(structure.parts.len()),
        spines: count(structure.spines.len()),
        height_bits: structure.dimensions.height.bits(),
        grafts: count(assembly.grafts.len()),
        applied_edits: growth.diagnostics.applied,
        orphans: growth
            .diagnostics
            .orphans
            .iter()
            .map(crate::botanical_dto::orphan_dto)
            .collect(),
    })
}

/// Ensures the entity carries a [`MaterialSet`] with at least one slot before a slot write.
pub(crate) fn ensure_material_slot(scene: &mut Scene, entity: Entity) {
    if scene.has_component::<MaterialSet>(entity) {
        let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
            if set.slots.is_empty() {
                set.slots.push(MaterialSlot::default());
            }
        });
    } else {
        let _ = scene.add_component(
            entity,
            MaterialSet {
                slots: vec![MaterialSlot::default()],
            },
        );
    }
}

/// Sets (or clears, when `tex_id == 0`) one texture override on the entity's material slot 0,
/// attaching a default `MaterialSet` slot first. The `assign-asset` texture path — a
/// per-object override layered over the slot's referenced `.smat`.
pub(crate) fn set_slot0_texture_override(
    scene: &mut Scene,
    entity: Entity,
    key: &str,
    tex_id: Uuid,
) {
    ensure_material_slot(scene, entity);
    let _ = scene.with_component_mut::<MaterialSet, _>(entity, |set| {
        let Some(slot) = set.slots.first_mut() else {
            return;
        };
        if let Some(map) = slot.overrides.as_object_mut() {
            if tex_id.value() == 0 {
                map.remove(key);
            } else {
                map.insert(key.to_owned(), json!(tex_id.value().to_string()));
            }
        }
    });
}

/// The shared `load-project` body: seed the loader inbox with an `Open` request and set the
/// `Loading` phase, then return the current identity as an immediate ack. The non-blocking loader
/// (`ProjectLoader::advance`, driven from the host each frame) runs the read/parse/scan off-thread
/// and installs on the main thread, so the control drain never blocks.
pub(crate) fn load_project_into(ctx: &mut EngineContext<'_>, path: &str) {
    ctx.vegetation_cook_jobs.shutdown();
    ctx.scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(path.to_owned()));
    ctx.scene_edit.project_phase = ProjectPhase::Loading;
}

/// The `enter-asset-preview` body: build an isolated preview scene, commit it, furnish it
/// (floor / key light / procedural sky / framed fly-cam), and route the renderer + active
/// view to it.
pub(crate) fn enter_asset_preview(
    ctx: &mut EngineContext<'_>,
    params: GetAssetModelParams,
) -> Result<AssetPreviewResultWrap> {
    require_project_loaded(ctx)?;
    if ctx.scene_edit.play_state != PlayState::Edit {
        return Err(Error::command("stop play first"));
    }
    // A native built-in primitive has no catalog row or model container — preview it on its
    // own geometry rather than resolving + instantiating a model.
    if let Some(builtin) = BuiltinMesh::from_reserved_id(Uuid(selector_id(&params.asset))) {
        return enter_builtin_preview(ctx, builtin);
    }
    let id = resolve_asset(ctx, &params.asset)?;
    let entry = ctx
        .assets
        .catalog()
        .find(id)
        .ok_or_else(|| Error::command(format!("no asset '{}'", id.value())))?;
    let entry_type = entry.asset_type;
    let entry_role = entry.role;
    let entry_hdr = entry.hdr;
    // A standalone texture previews on the isolated sphere, not as a model: an HDRI lights + backs a
    // three-ball environment rig; every other role is its map on one lit sphere (role picks the slot).
    if entry_type == AssetType::Texture {
        if entry_role == TextureRole::Hdri || entry_hdr {
            return enter_hdri_preview(ctx, id);
        }
        return enter_texture_preview(ctx, id, entry_role);
    }
    // A material (`.smat`) previews as itself on the studio sphere — the same subject the
    // material-graph editor's live pane drives.
    if entry_type == AssetType::Material {
        return enter_material_preview(ctx, id);
    }
    // A plant family previews as its compiled renderable form on the studio floor.
    if entry_type == AssetType::Plant {
        return enter_plant_preview(ctx, id);
    }
    let container_id = if entry_type == AssetType::Model {
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
    let model = ctx
        .assets
        .load_model_asset(container_id)
        .ok_or_else(|| Error::command(format!("model {} is not loadable", container_id.value())))?;
    let meta = model.meta.clone();

    // Build the preview scene locally so a failed swap stays on the prior model; commit only
    // once the model spawned a renderable mesh. Instantiation references mesh ids by uuid —
    // the GPU upload happens lazily at render — so no upload seam is needed here.
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = ctx
        .assets
        .instantiate_model(&mut preview, container_id, &meta.name)
        .map_err(Error::command)?;

    // A model is renderable when any entity in its forest carries a mesh — the meshes of a
    // multi-node forest ride child nodes, so probing only the resolved root rejects them.
    if !preview.model_has_renderable(root) {
        return Err(Error::command(format!(
            "model '{}' has no renderable mesh — re-import the asset",
            meta.name
        )));
    }
    let rig_entity = preview.model_rig_entity(root);
    // The animation authority (SkinnedMesh- or AnimationPlayer-bearing entity) the clip drives.
    let animatable = preview.animatable_descendant(root);

    // Open-from-clip: that clip becomes the active clip; the model opens paused at rest.
    if entry_type == AssetType::Animation && preview.has_component::<AnimationPlayer>(animatable) {
        let _ = preview.with_component_mut::<AnimationPlayer, _>(animatable, |player| {
            player.clip = id;
            player.time = 0.0;
            player.playing = false;
            player.preview_in_edit = false;
        });
    }

    let root_uuid = preview
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    let mut bone_by_node = Vec::new();
    let mut bones = Vec::new();
    if let Some(rig_entity) = rig_entity {
        let bone_uuids = preview
            .with_component::<SkinnedMesh, _>(rig_entity, |skin| skin.bones.clone())
            .unwrap_or_default();
        let joint_nodes: Vec<i32> = meta
            .skin
            .get("joints")
            .and_then(Value::as_array)
            .map(|joints| {
                joints
                    .iter()
                    .filter_map(|j| j.as_i64().map(|v| v as i32))
                    .collect()
            })
            .unwrap_or_default();
        let node_count = meta.nodes.as_array().map_or(0, Vec::len);
        bone_by_node = vec![Uuid(0); node_count];
        let joint_count = joint_nodes.len().min(bone_uuids.len());
        for k in 0..joint_count {
            let node_idx = joint_nodes[k];
            let uuid = bone_uuids[k];
            if node_idx >= 0 && (node_idx as usize) < node_count && uuid.value() != 0 {
                bone_by_node[node_idx as usize] = uuid;
                bones.push(saffron_protocol::BoneEntityDto {
                    index: node_idx,
                    entity: WireUuid(uuid.value()),
                });
            }
        }
    }

    // Furnish the instantiated model scene through the shared builder (floor / key light /
    // procedural sky / framed cam), then install it as the active preview (a rig keeps the bone
    // overlay on). A fresh enter makes the preview the active view; a swap keeps the authored stash.
    // A model is floor-standing geometry — default the floor on (the toggle still removes it).
    ctx.scene_edit.preview_show_floor = true;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut furnish = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        furnish = Some(furnish_preview_scene(&mut preview, assets, gpu, root, spec));
    });
    let furnish = furnish.expect("furnish ran");
    let (_root_uuid, framing) = install_preview_scene(
        ctx,
        preview,
        root,
        container_id,
        furnish,
        bone_by_node,
        true,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones,
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// Builds an isolated preview scene for a native built-in primitive (no catalog row, no
/// model container): a single entity carrying the reserved-id mesh + a default material,
/// framed on the shared asset-preview view. Mirrors the commit tail of
/// [`enter_asset_preview`] for a container-less, rig-less subject.
pub(crate) fn enter_builtin_preview(
    ctx: &mut EngineContext<'_>,
    builtin: BuiltinMesh,
) -> Result<AssetPreviewResultWrap> {
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let root = preview.create_entity(builtin.display_name());
    let _ = preview.add_component(
        root,
        Mesh {
            mesh: builtin.reserved_id(),
        },
    );
    let _ = preview.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot::default()],
        },
    );
    // A built-in primitive is floor-standing geometry — default the floor on.
    ctx.scene_edit.preview_show_floor = true;
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        builtin.reserved_id(),
        PreviewEnv::Procedural,
    ))
}

/// Compiles a plant family through its retained recipe (content-addressed — an
/// unchanged family republishes the identical artifact), registers its renderable
/// form under the family id, and spawns the floor-standing entity carrying the
/// family mesh with the family's material slots. Shared by the interactive plant
/// preview and the thumbnail subject build.
pub(crate) fn plant_preview_root(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    scene: &mut Scene,
    family: Uuid,
) -> Result<(Entity, saffron_assets::PlantFamilyRender)> {
    let plant = load_plant_family_asset(assets, family).map_err(Error::command)?;
    let name = plant.name.clone();
    let options = PlantRecookOptions {
        limits: PlantCompileLimits::default(),
        versions: vegetation_cook_versions(),
        platform: portable_vegetation_platform_profile(None),
    };
    let published = match recook_plant_family(assets, &plant, &options).map_err(Error::from)? {
        PlantRecookOutcome::Published(published) => published,
        PlantRecookOutcome::Rejected(_) => {
            return Err(Error::command(
                "plant family does not validate — run plant-validate for diagnostics",
            ));
        }
    };
    let render = assets
        .load_plant_family(gpu, family, published.publication.content_hash)
        .ok_or_else(|| Error::command("plant family artifact did not load a renderable form"))?;
    let root = scene.create_entity(&name);
    let _ = scene.add_component(root, Mesh { mesh: family });
    let _ = scene.add_component(
        root,
        MaterialSet {
            slots: render
                .materials
                .iter()
                .map(|material| MaterialSlot {
                    material: *material,
                    ..MaterialSlot::default()
                })
                .collect(),
        },
    );
    Ok((root, render))
}

/// The `enter-asset-preview` branch for a plant family: [`plant_preview_root`] on an
/// isolated scene, committed with the floor on; the result carries the authored
/// combination domain the variation/phenotype scrub selects from.
pub(crate) fn enter_plant_preview(
    ctx: &mut EngineContext<'_>,
    id: Uuid,
) -> Result<AssetPreviewResultWrap> {
    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    let assets = &mut *ctx.assets;
    let mut built = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        built = Some(plant_preview_root(assets, gpu, &mut preview, id));
    });
    let (root, render) = built.expect("build ran")?;
    ctx.scene_edit.preview_show_floor = true;
    let mut result = commit_preview_subject(ctx, preview, root, id, PreviewEnv::Procedural);
    result.0.plant_combinations = render
        .combinations
        .iter()
        .map(
            |(variation, phenotype)| saffron_protocol::PlantCombinationDto {
                variation: *variation,
                phenotype: *phenotype,
            },
        )
        .collect();
    Ok(result)
}

/// The `enter-asset-preview` branch for a standalone texture: shade a preview sphere with an
/// ephemeral single-slot material carrying the texture in the slot its role feeds. The material is
/// seeded into `material_by_uuid` under the reserved [`PREVIEW_MATERIAL_ID`], with no catalog row,
/// and rebuilt on each enter.
pub(crate) fn enter_texture_preview(
    ctx: &mut EngineContext<'_>,
    tid: Uuid,
    role: TextureRole,
) -> Result<AssetPreviewResultWrap> {
    let catalog = ctx.scene_edit.scene.catalog.clone();
    // A texture map previews on a floating surface sphere — default the floor off (non-model).
    ctx.scene_edit.preview_show_floor = false;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut build = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        build = Some(build_preview_scene(
            assets,
            gpu,
            catalog.clone(),
            PreviewSubject::TextureRole { tid, role },
            PREVIEW_MATERIAL_ID,
            spec,
        ));
    });
    let build = build.expect("build ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        build.scene,
        build.root,
        tid,
        build.furnish,
        Vec::new(),
        false,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones: Vec::new(),
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// The `enter-asset-preview` branch for an HDRI: the imported equirect becomes both the backdrop and
/// the IBL source, and three PBR balls — chrome, diffuse grey, colored satin — read its reflections,
/// irradiance, and color response. The balls differ only by per-slot overrides on the default
/// material, and the centre ball parents the other two so the framing bounds cover all three.
pub(crate) fn enter_hdri_preview(
    ctx: &mut EngineContext<'_>,
    hdri_id: Uuid,
) -> Result<AssetPreviewResultWrap> {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut preview = Scene::new();
    preview.catalog = ctx.scene_edit.scene.catalog.clone();
    // (x offset, name, material overrides): chrome mirror / diffuse grey / colored satin.
    let balls = [
        (
            -2.3_f32,
            "Chrome",
            serde_json::json!({ "metallic": 1.0, "roughness": 0.04, "baseColor": [1.0, 1.0, 1.0, 1.0] }),
        ),
        (
            0.0_f32,
            "Diffuse",
            serde_json::json!({ "metallic": 0.0, "roughness": 1.0, "baseColor": [0.5, 0.5, 0.5, 1.0] }),
        ),
        (
            2.3_f32,
            "Satin",
            serde_json::json!({ "metallic": 0.0, "roughness": 0.4, "baseColor": [0.85, 0.5, 0.35, 1.0] }),
        ),
    ];
    let mut entities = Vec::with_capacity(balls.len());
    for (x, name, overrides) in &balls {
        let e = preview.create_entity(*name);
        let _ = preview.add_component(
            e,
            Mesh {
                mesh: BUILTIN_SPHERE_MESH_ID,
            },
        );
        let _ = preview.add_component(
            e,
            MaterialSet {
                slots: vec![MaterialSlot {
                    material: Uuid(0),
                    overrides: overrides.clone(),
                }],
            },
        );
        let _ = preview.with_component_mut::<Transform, _>(e, |t| {
            t.translation = GVec3::new(*x, 0.0, 0.0);
        });
        entities.push(e);
    }
    // The center ball is the framing root; the outer two parent to it so the AABB spans all three.
    let root = entities[1];
    for &e in &[entities[0], entities[2]] {
        let _ = preview.set_parent(e, Some(root), true);
    }
    // The HDRI env rig floats in its own equirect — no floor.
    ctx.scene_edit.preview_show_floor = false;
    Ok(commit_preview_subject(
        ctx,
        preview,
        root,
        hdri_id,
        PreviewEnv::Hdri(hdri_id),
    ))
}

/// The `enter-asset-preview` branch for a material (`.smat`): a built-in sphere carrying that
/// material **by id**, in the shared procedural studio. Referencing by id (not a copy) is the join
/// point — a later `material-set-graph` / `material-update` mutates the `.smat` in the asset cache
/// and the sphere re-renders next frame, so the material-graph editor's live pane and a standalone
/// material "View" tab are one host path.
pub(crate) fn enter_material_preview(
    ctx: &mut EngineContext<'_>,
    mid: Uuid,
) -> Result<AssetPreviewResultWrap> {
    let catalog = ctx.scene_edit.scene.catalog.clone();
    // A material previews on a floating surface sphere, not floor-standing geometry — default the
    // floor off (the toggle still lets the user add one).
    ctx.scene_edit.preview_show_floor = false;
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env: PreviewEnv::Procedural,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut build = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        build = Some(build_preview_scene(
            assets,
            gpu,
            catalog.clone(),
            PreviewSubject::Material(mid),
            PREVIEW_MATERIAL_ID,
            spec,
        ));
    });
    let build = build.expect("build ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        build.scene,
        build.root,
        mid,
        build.furnish,
        Vec::new(),
        false,
    );
    Ok(AssetPreviewResultWrap(
        saffron_protocol::AssetPreviewResult {
            root_entity: WireUuid(root_uuid),
            bones: Vec::new(),
            target: vec3(framing.target),
            distance: framing.distance,
            plant_combinations: Vec::new(),
        },
    ))
}

/// The ephemeral single-slot material the texture preview shades the sphere with: the texture in
/// the slot its role feeds, neutral factors elsewhere (neutral albedo = mid-grey), so the ball
/// reads the map the way a surface uses it. `Albedo`/`Opacity`/`Gloss`/`Unknown` fall back to the
/// base-color slot (show the map as a plain surface texture); HDRI never reaches here.
pub(crate) fn preview_material_for_texture(role: TextureRole, tid: Uuid) -> MaterialAsset {
    use saffron_geometry::glam::{Vec3 as GVec3, Vec4 as GVec4};
    let grey = |v: f32| GVec4::new(v, v, v, 1.0);
    let mut m = default_material_asset();
    m.metallic = 0.0;
    m.roughness = 0.6;
    match role {
        TextureRole::Normal => {
            m.normal_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Roughness => {
            m.orm_texture = tid;
            m.roughness = 1.0;
            m.base_color = grey(0.55);
        }
        TextureRole::Metallic => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 0.35;
            m.base_color = grey(0.8);
        }
        TextureRole::Ao => {
            m.orm_texture = tid;
            m.base_color = grey(0.6);
        }
        TextureRole::Orm => {
            m.orm_texture = tid;
            m.metallic = 1.0;
            m.roughness = 1.0;
            m.base_color = grey(0.6);
        }
        TextureRole::Height => {
            // A bare height texture previews as parallax-occlusion mapping on the ordinary sphere —
            // never auto-routed to real displacement (that is a deliberate authored `.smat` choice
            // carrying a tessellation + BLAS cost). A Displacement-authored material bulges the same
            // sphere through the real tessellating path, so preview matches scene either way.
            m.height_texture = tid;
            m.height_scale = 0.05;
            m.height_mode = HeightMode::Parallax;
            m.base_color = grey(0.6);
        }
        TextureRole::Emissive => {
            m.emissive_texture = tid;
            m.emissive = GVec3::ONE;
            m.emissive_strength = 2.0;
            m.base_color = grey(0.02);
        }
        _ => {
            m.albedo_texture = tid;
            m.base_color = GVec4::ONE;
        }
    }
    m
}
