//! The 16 animation-domain control commands: playback state (get/play/set-playing/seek/
//! set-loop/stop-preview), clip listing, the skeleton overlay (get/set/highlight + joint
//! pick), the viewport debug overlays (get/set), the asset-preview options (show-floor), and
//! foot-IK (get/set).
//!
//! `get/set-debug-overlays` sit in this block per the frozen manifest order (between
//! `set-skeleton-overlay` and `set-skeleton-highlight`), interleaved with the
//! skeleton-overlay group. `set-asset-preview-options` likewise sits here per the
//! manifest order (between `pick-skeleton-joint` and `get-foot-ik`); it reuses the
//! preview-floor helpers from `commands_asset`.
//!
//! The handlers drive the per-rig [`AnimationPlayer`] and the overlay render state — a thin
//! command surface over the animation-player runtime. A model's player / foot-IK / state may
//! live on the container root or a rig descendant while the selection is any entity in the
//! forest (e.g. a `Morph` mesh child), so every transport command resolves to the model's
//! single authority via [`Scene::model_player`] first — never a leaf that would spawn a rival
//! player.

use saffron_geometry::{AnimClip, AnimPath, AnimTarget, AnimTrack};
use saffron_protocol::{
    AnimationChannelDto, AnimationClipDto, AnimationStateParams, AnimationStateResult,
    AssetPreviewOptionsResult, AssetSelector, ClipBindingsResult, DebugOverlaysParams,
    DebugOverlaysResult, EmptyParams, EntitySelector, FootIkResult, GetFootIkParams,
    GetMorphWeightsParams, ListClipBindingsParams, ListClipsParams, ListClipsResult,
    MorphWeightsResult, PickSkeletonJointParams, PickSkeletonJointResult, PlayAnimationParams,
    SeekAnimationParams, SetAnimationLoopParams, SetAnimationPlayingParams,
    SetAssetPreviewOptionsParams, SetFootIkParams, SetMorphWeightsParams,
    SetSkeletonHighlightParams, SetSkeletonOverlayParams, SkeletonOverlayResult, Uuid,
};
use saffron_scene::{
    AnimationPlayer, AssetType, Entity, FootIk, MorphComponent, MorphWeightOverride, Name,
    Relationship, Wrap,
};
use saffron_sceneedit::{DebugOverlayOptions, SkeletonOverlayOptions, viewport_project};

use crate::error::{Error, Result};
use crate::registry::{CommandRegistry, EngineContext};
use crate::selector::resolve_entity;

/// The wire spelling of a clip wrap mode.
fn wrap_name(wrap: Wrap) -> &'static str {
    match wrap {
        Wrap::Once => "once",
        Wrap::Loop => "loop",
        Wrap::PingPong => "pingpong",
    }
}

/// A clip wrap mode parsed from its wire spelling; anything but `once`/`pingpong` is `loop`.
fn wrap_from_name(name: &str) -> Wrap {
    match name {
        "once" => Wrap::Once,
        "pingpong" => Wrap::PingPong,
        _ => Wrap::Loop,
    }
}

/// The uuid a uuid-or-name selector names (an unsigned number, or a whole-string parse), and
/// the name string (empty when the selector is not a string), shared by the clip and
/// container resolvers.
fn asset_selector_parts(selector: &AssetSelector) -> (u64, String) {
    (
        selector.id().unwrap_or(0),
        selector.name().unwrap_or_default().to_owned(),
    )
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to an animation catalog
/// entry id.
fn resolve_clip(ctx: &EngineContext<'_>, selector: &AssetSelector) -> Result<saffron_core::Uuid> {
    let (by_id, name) = asset_selector_parts(selector);
    for entry in &ctx.assets.catalog.entries {
        if entry.asset_type == AssetType::Animation && (entry.id.0 == by_id || entry.name == name) {
            return Ok(entry.id);
        }
    }
    Err(Error::command(format!("no animation clip '{name}'")))
}

/// Resolves an [`AssetSelector`](saffron_protocol::AssetSelector) to its owning `.smodel`
/// container id (the model's own id for a model, the container for a sub-asset, `0` for a
/// standalone).
fn resolve_container(
    ctx: &EngineContext<'_>,
    selector: &AssetSelector,
) -> Result<saffron_core::Uuid> {
    let (by_id, name) = asset_selector_parts(selector);
    for entry in &ctx.assets.catalog.entries {
        if entry.id.0 == by_id || entry.name == name {
            if entry.asset_type == AssetType::Model {
                return Ok(entry.id);
            }
            return Ok(entry.container);
        }
    }
    Err(Error::command(format!("no asset '{name}'")))
}

/// Resolves a selector to its rig descendant and ensures it carries an [`AnimationPlayer`],
/// attaching a default one if absent.
fn player_entity(ctx: &mut EngineContext<'_>, selector: &EntitySelector) -> Result<Entity> {
    let entity = resolve_entity(ctx, selector)?;
    let scene = ctx.scene_edit.active_scene();
    // Resolve to the model's single animation authority (up to the model root, then down to
    // its player), so selecting a leaf — e.g. a `Morph` mesh child whose player lives on the
    // container — never forges a rival player on the leaf.
    let target = scene.model_player(entity);
    if !scene.has_component::<AnimationPlayer>(target) {
        let _ = scene.add_component(target, AnimationPlayer::default());
    }
    Ok(target)
}

/// The animation state reply for a rig's player: the clip + its catalog name/duration, the
/// playhead, and the bumping `animation_version`.
fn state_of(
    ctx: &mut EngineContext<'_>,
    target: Entity,
    player: &AnimationPlayer,
) -> AnimationStateResult {
    let (clip_name, duration) = ctx
        .assets
        .catalog
        .find(player.clip)
        .map(|entry| (entry.name.clone(), entry.duration))
        .unwrap_or_default();
    let animation_version = i32::try_from(ctx.scene_edit.animation_version).unwrap_or(i32::MAX);
    let (morph_weights, _) = morph_weights_of(ctx.scene_edit.active_scene(), target);
    AnimationStateResult {
        clip: Uuid(player.clip.0),
        clip_name,
        duration,
        time: player.time,
        playing: player.playing,
        wrap: wrap_name(player.wrap).to_owned(),
        speed: player.speed,
        animation_version,
        morph_weights,
    }
}

/// The wire `kind` string for a track: a morph-weights track keys on its path, every other
/// track on its target then path.
fn channel_kind(track: &AnimTrack) -> &'static str {
    match track.path {
        AnimPath::Weights => "morph-weights",
        AnimPath::Translation | AnimPath::Rotation | AnimPath::Scale => match track.target {
            AnimTarget::Bone => "bone",
            AnimTarget::Node => match track.path {
                AnimPath::Translation => "node-translation",
                AnimPath::Rotation => "node-rotation",
                AnimPath::Scale => "node-scale",
                AnimPath::Weights => "morph-weights",
            },
        },
    }
}

/// Value components per keyframe: `morph_count` for a weights channel, `4` for a rotation
/// quaternion, `3` for translation/scale — so `values.len() == times.len() * width`.
fn channel_width(track: &AnimTrack) -> i32 {
    match track.path {
        AnimPath::Weights => i32::try_from(track.morph_count).unwrap_or(i32::MAX),
        AnimPath::Rotation => 4,
        AnimPath::Translation | AnimPath::Scale => 3,
    }
}

/// Maps a clip's tracks to wire channels, deriving each channel's display label through
/// `label_of` (the raw `target_name` for `list-clips`; the live-forest resolution for
/// `list-clip-bindings`).
pub(crate) fn channels_of(
    clip: &AnimClip,
    label_of: impl Fn(&AnimTrack) -> String,
) -> Vec<AnimationChannelDto> {
    clip.tracks
        .iter()
        .map(|track| AnimationChannelDto {
            kind: channel_kind(track).to_owned(),
            label: label_of(track),
            target_name: track.target_name.clone(),
            times: track.times.clone(),
            width: channel_width(track),
            values: track.values.clone(),
        })
        .collect()
}

/// Finds a descendant of `root` (inclusive) whose `Name` matches `name`, first-match pre-order
/// — the control-side twin of the runtime's scoped node-binding walk (decision #20), so a
/// channel label resolves against the live forest without the global `find_entity_by_uuid`.
fn find_named_in_forest(scene: &saffron_scene::Scene, root: Entity, name: &str) -> Option<Entity> {
    if scene
        .with_component::<Name, _>(root, |n| n.name == name)
        .unwrap_or(false)
    {
        return Some(root);
    }
    let children = scene
        .with_component::<Relationship, _>(root, |r| r.children.clone())
        .unwrap_or_default();
    for child in children {
        if scene.valid(child)
            && let Some(found) = find_named_in_forest(scene, child, name)
        {
            return Some(found);
        }
    }
    None
}

/// The live morph weights of `target` (the runtime [`MorphWeightOverride`] if a preview is
/// live, else the durable [`MorphComponent`]) and the durable target names. Empty vectors when
/// the entity carries no morph mesh.
fn morph_weights_of(scene: &saffron_scene::Scene, target: Entity) -> (Vec<f32>, Vec<String>) {
    let weights = scene
        .with_component::<MorphWeightOverride, _>(target, |o| o.weights.clone())
        .or_else(|_| scene.with_component::<MorphComponent, _>(target, |c| c.weights.clone()))
        .unwrap_or_default();
    let names = scene
        .with_component::<MorphComponent, _>(target, |c| c.names.clone())
        .unwrap_or_default();
    (weights, names)
}

/// The morph-bearing entity of `entity`'s model: the entity itself if it carries a
/// [`MorphComponent`], else the first such entity in its forest ([`Scene::model_morph_entity`]) —
/// the morph mesh rides a child node while the selection resolves to the container. Falls back
/// to `entity` when none, so a non-morph target degrades to the caller's no-op.
fn morph_entity(ctx: &mut EngineContext<'_>, selector: &EntitySelector) -> Result<Entity> {
    let entity = resolve_entity(ctx, selector)?;
    let scene = ctx.scene_edit.active_scene();
    Ok(scene.model_morph_entity(entity).unwrap_or(entity))
}

/// Reads a rig's player and returns its [`AnimationStateResult`].
fn state_for(ctx: &mut EngineContext<'_>, target: Entity) -> Result<AnimationStateResult> {
    let player = ctx
        .scene_edit
        .active_scene()
        .component::<AnimationPlayer>(target)
        .map_err(|_| Error::command("entity has no animation player"))?;
    Ok(state_of(ctx, target, &player))
}

/// The skeleton-overlay reply built from the live options.
fn skeleton_overlay_state(opts: &SkeletonOverlayOptions) -> SkeletonOverlayResult {
    SkeletonOverlayResult {
        show: opts.show,
        axes: opts.axes,
        joint_size: opts.joint_size,
        highlight_joint: opts.highlight_joint,
    }
}

/// The debug-overlays reply built from the live options.
fn debug_overlays_state(opts: &DebugOverlayOptions) -> DebugOverlaysResult {
    DebugOverlaysResult {
        bounds: opts.bounds,
        scene_aabb: opts.scene_aabb,
        light_volumes: opts.light_volumes,
        grid: opts.grid,
        colliders: opts.colliders,
    }
}

/// Resolves a selector to its rig descendant and ensures it carries a [`FootIk`], attaching
/// a default one if absent.
fn foot_ik_entity(ctx: &mut EngineContext<'_>, selector: &EntitySelector) -> Result<Entity> {
    let entity = resolve_entity(ctx, selector)?;
    let scene = ctx.scene_edit.active_scene();
    // Foot IK needs a skeleton; reject a model with no rig in its forest rather than attaching a
    // FootIk to a bare container that the runtime would never read.
    if scene.model_rig_entity(entity).is_none() {
        return Err(Error::command("entity has no rig to attach foot IK to"));
    }
    let target = scene.model_player(entity);
    if !scene.has_component::<FootIk>(target) {
        let _ = scene.add_component(target, FootIk::default());
    }
    Ok(target)
}

/// The foot-IK reply built from the component.
fn foot_ik_result(scene: &saffron_scene::Scene, target: Entity) -> FootIkResult {
    scene
        .with_component::<FootIk, _>(target, |ik| FootIkResult {
            enabled: ik.enabled,
            ground_height: ik.ground_height,
            chains: i32::try_from(ik.chains.len()).unwrap_or(i32::MAX),
        })
        .unwrap_or(FootIkResult {
            enabled: false,
            ground_height: 0.0,
            chains: 0,
        })
}

/// Registers the 13 animation-domain commands onto `reg`.
pub fn register_animation_commands(reg: &mut CommandRegistry) {
    reg.register::<AnimationStateParams, AnimationStateResult>(
        "get-animation-state",
        "get-animation-state {entity} — the rig's playhead, clip, wrap, and speed",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let target = ctx.scene_edit.active_scene().model_player(entity);
            state_for(ctx, target)
        },
    );

    reg.register::<ListClipsParams, ListClipsResult>(
        "list-clips",
        "list-clips {asset?} — animation clips: a model container's own, or the full catalog",
        |ctx, params| {
            let mut container = saffron_core::Uuid(0);
            if let Some(asset) = &params.asset {
                container = resolve_container(ctx, asset)?;
                if container.0 == 0 {
                    return Err(Error::command("asset is not part of a model container"));
                }
            }
            // Collect the catalog rows first (immutable borrow), then load each clip's full
            // track data (mutable borrow) to build real per-channel keyframe strips. No live
            // forest here, so labels are the raw glTF target names.
            let rows: Vec<(saffron_core::Uuid, String, f32)> = ctx
                .assets
                .catalog
                .entries
                .iter()
                .filter(|entry| entry.asset_type == AssetType::Animation)
                .filter(|entry| container.0 == 0 || entry.container.0 == container.0)
                .map(|entry| (entry.id, entry.name.clone(), entry.duration))
                .collect();
            let clips = rows
                .into_iter()
                .map(|(id, name, duration)| {
                    let channels = ctx
                        .assets
                        .load_anim_clip(id)
                        .map(|clip| channels_of(&clip, |track| track.target_name.clone()))
                        .unwrap_or_default();
                    AnimationClipDto {
                        id: Uuid(id.0),
                        name,
                        duration,
                        channels,
                    }
                })
                .collect();
            Ok(ListClipsResult { clips })
        },
    );

    reg.register::<PlayAnimationParams, AnimationStateResult>(
        "play-animation",
        "play-animation {entity, clip, speed=1, loop=true, blend=0} — play a clip (previews in Edit too)",
        |ctx, params| {
            let clip = resolve_clip(ctx, &params.clip)?;
            let target = player_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            let _ = scene.with_component_mut::<AnimationPlayer, _>(target, |p| {
                let blend = params.blend.unwrap_or(0.0);
                if blend > 0.0 && p.clip.0 != 0 && p.clip.0 != clip.0 {
                    p.prev_clip = p.clip; // cross-fade / inertialize from the current clip
                    p.transition = 0.0;
                    p.transition_duration = blend;
                }
                p.clip = clip;
                p.time = 0.0;
                p.speed = params.speed.unwrap_or(1.0);
                p.wrap = if params.r#loop.unwrap_or(true) {
                    Wrap::Loop
                } else {
                    Wrap::Once
                };
                // `paused` loads the clip at frame 0 without advancing; the pose still previews.
                p.playing = !params.paused.unwrap_or(false);
                p.preview_in_edit = true;
            });
            ctx.scene_edit.animation_version += 1;
            state_for(ctx, target)
        },
    );

    reg.register::<SetAnimationPlayingParams, AnimationStateResult>(
        "set-animation-playing",
        "set-animation-playing {entity, playing} — resume (true) or pause (false) without moving the playhead",
        |ctx, params| {
            let target = player_entity(ctx, &params.entity)?;
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<AnimationPlayer, _>(target, |p| {
                    p.playing = params.playing;
                    p.preview_in_edit = true; // keep driving the Edit preview (paused shows the pose)
                });
            ctx.scene_edit.animation_version += 1;
            state_for(ctx, target)
        },
    );

    reg.register::<SeekAnimationParams, AnimationStateResult>(
        "seek-animation",
        "seek-animation {entity, time, seekBlend=0} — set the playhead (previews in Edit); seekBlend eases the pose",
        |ctx, params| {
            let target = player_entity(ctx, &params.entity)?;
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<AnimationPlayer, _>(target, |p| {
                    p.time = params.time;
                    // A self-transition inertializes the pose toward the seeked time.
                    let seek_blend = params.seek_blend.unwrap_or(0.0);
                    if seek_blend > 0.0 && p.clip.0 != 0 {
                        p.prev_clip = p.clip;
                        p.transition = 0.0;
                        p.transition_duration = seek_blend;
                    }
                    p.preview_in_edit = true;
                });
            ctx.scene_edit.animation_version += 1;
            state_for(ctx, target)
        },
    );

    reg.register::<SetAnimationLoopParams, AnimationStateResult>(
        "set-animation-loop",
        "set-animation-loop {entity, wrap} — once | loop | pingpong",
        |ctx, params| {
            let target = player_entity(ctx, &params.entity)?;
            let wrap = wrap_from_name(&params.wrap);
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<AnimationPlayer, _>(target, |p| p.wrap = wrap);
            ctx.scene_edit.animation_version += 1;
            state_for(ctx, target)
        },
    );

    reg.register::<AnimationStateParams, AnimationStateResult>(
        "stop-preview",
        "stop-preview {entity} — clear the Edit preview and stop (revert to rest)",
        |ctx, params| {
            let target = player_entity(ctx, &params.entity)?;
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<AnimationPlayer, _>(target, |p| {
                    p.preview_in_edit = false;
                    p.playing = false;
                });
            ctx.scene_edit.animation_version += 1;
            state_for(ctx, target)
        },
    );

    reg.register::<EmptyParams, SkeletonOverlayResult>(
        "get-skeleton-overlay",
        "get-skeleton-overlay — the line-skeleton overlay toggle, axes, and joint size",
        |ctx, _params| Ok(skeleton_overlay_state(&ctx.scene_edit.skeleton_overlay)),
    );

    reg.register::<SetSkeletonOverlayParams, SkeletonOverlayResult>(
        "set-skeleton-overlay",
        "set-skeleton-overlay {show?, axes?, jointSize?} — the selected rig's line-skeleton viewport overlay",
        |ctx, params| {
            let opts = &mut ctx.scene_edit.skeleton_overlay;
            if let Some(show) = params.show {
                opts.show = show;
            }
            if let Some(axes) = params.axes {
                opts.axes = axes;
            }
            if let Some(joint_size) = params.joint_size {
                opts.joint_size = joint_size.max(0.5);
            }
            Ok(skeleton_overlay_state(opts))
        },
    );

    reg.register::<EmptyParams, DebugOverlaysResult>(
        "get-debug-overlays",
        "get-debug-overlays — the viewport debug-overlay toggles",
        |ctx, _params| Ok(debug_overlays_state(&ctx.scene_edit.debug_overlays)),
    );

    reg.register::<DebugOverlaysParams, DebugOverlaysResult>(
        "set-debug-overlays",
        "set-debug-overlays {bounds?, sceneAabb?, lightVolumes?, grid?, colliders?} — toggle viewport debug overlays",
        |ctx, params| {
            let opts = &mut ctx.scene_edit.debug_overlays;
            if let Some(bounds) = params.bounds {
                opts.bounds = bounds;
            }
            if let Some(scene_aabb) = params.scene_aabb {
                opts.scene_aabb = scene_aabb;
            }
            if let Some(light_volumes) = params.light_volumes {
                opts.light_volumes = light_volumes;
            }
            if let Some(grid) = params.grid {
                opts.grid = grid;
            }
            if let Some(colliders) = params.colliders {
                opts.colliders = colliders;
            }
            Ok(debug_overlays_state(opts))
        },
    );

    reg.register::<SetSkeletonHighlightParams, SkeletonOverlayResult>(
        "set-skeleton-highlight",
        "set-skeleton-highlight {joint} — tint a previewed model's joint by its get-asset-model node index",
        |ctx, params| {
            let highlight = if params.joint < 0 { -1 } else { params.joint };
            ctx.scene_edit.skeleton_overlay.highlight_joint = highlight;
            Ok(skeleton_overlay_state(&ctx.scene_edit.skeleton_overlay))
        },
    );

    reg.register::<PickSkeletonJointParams, PickSkeletonJointResult>(
        "pick-skeleton-joint",
        "pick-skeleton-joint {u, v, radiusPx=8} — the previewed model's nearest joint to a viewport click",
        |ctx, params| {
            if !ctx.scene_edit.previewing() || ctx.scene_edit.preview_bone_by_node.is_empty() {
                return Ok(PickSkeletonJointResult {
                    found: false,
                    node_index: -1,
                });
            }
            let width = ctx.renderer.viewport_width();
            let height = ctx.renderer.viewport_height();
            if width == 0 || height == 0 {
                return Ok(PickSkeletonJointResult {
                    found: false,
                    node_index: -1,
                });
            }
            let cam = ctx.scene_edit.render_camera_view();
            let bones = ctx.scene_edit.preview_bone_by_node.clone();
            let scene = ctx.scene_edit.active_scene();
            scene.update_world_transforms(); // pick against the same world positions the overlay draws
            let click = saffron_geometry::glam::Vec2::new(
                params.u * width as f32,
                params.v * height as f32,
            );
            let radius_px = params.radius_px.unwrap_or(8.0);
            let mut best_node = -1i32;
            let mut best_dist_sq = radius_px * radius_px;
            for (node, id) in bones.iter().enumerate() {
                if id.0 == 0 {
                    continue;
                }
                let Some(bone) = scene.find_entity_by_uuid(*id) else {
                    continue;
                };
                if !scene.valid(bone) {
                    continue;
                }
                let projection = viewport_project(&cam, width, height, scene.world_translation(bone));
                if !projection.visible {
                    continue;
                }
                let d = projection.pixel - click;
                let dist_sq = d.dot(d);
                if dist_sq <= best_dist_sq {
                    best_dist_sq = dist_sq;
                    best_node = i32::try_from(node).unwrap_or(i32::MAX);
                }
            }
            Ok(PickSkeletonJointResult {
                found: best_node >= 0,
                node_index: best_node,
            })
        },
    );

    reg.register::<SetAssetPreviewOptionsParams, AssetPreviewOptionsResult>(
        "set-asset-preview-options",
        "set-asset-preview-options {floor?} — preview-scene settings (show floor)",
        |ctx, params| {
            if !ctx.scene_edit.previewing() {
                return Err(Error::command("not in an asset preview"));
            }
            if let Some(floor) = params.floor
                && floor != ctx.scene_edit.preview_show_floor
            {
                ctx.scene_edit.preview_show_floor = floor;
                if floor {
                    let root = ctx.scene_edit.preview_root_entity;
                    let assets = &mut *ctx.assets;
                    let scene = ctx.scene_edit.preview_scene.as_mut().expect("previewing");
                    let mut entity = Entity::NULL;
                    ctx.renderer.with_gpu_uploader(&mut |gpu| {
                        let bounds =
                            crate::commands_asset::compute_preview_bounds(scene, assets, gpu, root);
                        entity =
                            crate::commands_asset::spawn_preview_floor(scene, assets, gpu, &bounds);
                    });
                    ctx.scene_edit.preview_floor_entity = entity;
                } else {
                    let floor_entity = ctx.scene_edit.preview_floor_entity;
                    let scene = ctx.scene_edit.active_scene();
                    if floor_entity != Entity::NULL && scene.valid(floor_entity) {
                        scene.destroy_entity(floor_entity);
                    }
                    ctx.scene_edit.preview_floor_entity = Entity::NULL;
                }
                ctx.scene_edit.scene_version += 1;
            }
            Ok(AssetPreviewOptionsResult {
                floor: ctx.scene_edit.preview_show_floor,
            })
        },
    );

    reg.register::<GetFootIkParams, FootIkResult>(
        "get-foot-ik",
        "get-foot-ik {entity} — the rig's foot-IK enable, ground height, and chain count",
        |ctx, params| {
            let target = foot_ik_entity(ctx, &params.entity)?;
            Ok(foot_ik_result(ctx.scene_edit.active_scene(), target))
        },
    );

    reg.register::<SetFootIkParams, FootIkResult>(
        "set-foot-ik",
        "set-foot-ik {entity, enabled?, groundHeight?} — toggle kinematic foot IK on a rig",
        |ctx, params| {
            let target = foot_ik_entity(ctx, &params.entity)?;
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<FootIk, _>(target, |c| {
                    if let Some(enabled) = params.enabled {
                        c.enabled = enabled;
                    }
                    if let Some(ground_height) = params.ground_height {
                        c.ground_height = ground_height;
                    }
                });
            ctx.scene_edit.animation_version += 1;
            Ok(foot_ik_result(ctx.scene_edit.active_scene(), target))
        },
    );

    reg.register::<SetMorphWeightsParams, MorphWeightsResult>(
        "set-morph-weights",
        "set-morph-weights {entity, weights} — set blend-shape weights (canonical 0..1)",
        |ctx, params| {
            let target = morph_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            let count = scene
                .with_component::<MorphComponent, _>(target, |c| c.weights.len())
                .map_err(|_| Error::command("entity has no morph mesh"))?;
            if params.weights.len() != count {
                return Err(Error::command(format!(
                    "weights length {} does not match the {count} morph targets",
                    params.weights.len()
                )));
            }
            // A live preview owns a runtime `MorphWeightOverride`; update it so the change is
            // visible at once. Otherwise edit the durable component (the saved rest weights).
            if scene.has_component::<MorphWeightOverride>(target) {
                let _ = scene.with_component_mut::<MorphWeightOverride, _>(target, |o| {
                    o.weights = params.weights.clone();
                });
            } else {
                let _ = scene.with_component_mut::<MorphComponent, _>(target, |c| {
                    c.weights = params.weights.clone();
                });
            }
            ctx.scene_edit.animation_version += 1;
            let (weights, names) = morph_weights_of(ctx.scene_edit.active_scene(), target);
            Ok(MorphWeightsResult { weights, names })
        },
    );

    reg.register::<GetMorphWeightsParams, MorphWeightsResult>(
        "get-morph-weights",
        "get-morph-weights {entity} — live blend-shape weights + target names",
        |ctx, params| {
            let target = morph_entity(ctx, &params.entity)?;
            let (weights, names) = morph_weights_of(ctx.scene_edit.active_scene(), target);
            Ok(MorphWeightsResult { weights, names })
        },
    );

    reg.register::<ListClipBindingsParams, ClipBindingsResult>(
        "list-clip-bindings",
        "list-clip-bindings {entity, clip} — a clip's channels resolved against the live forest",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            // The whole model forest is the binding-resolution scope (a leaf selection still
            // resolves node names across the entire forest, not just its own subtree).
            let root = ctx.scene_edit.active_scene().model_root_of(entity);
            let clip_id = resolve_clip(ctx, &params.clip)?;
            let clip = ctx
                .assets
                .load_anim_clip(clip_id)
                .map_err(|_| Error::command("clip not found"))?;
            let scene = ctx.scene_edit.active_scene();
            // Node/bone labels resolve against the live forest (the resolved entity's current
            // name; the raw glTF name on a miss — the broken-binding tell). Morph labels are
            // the raw target name.
            let channels = channels_of(&clip, |track| match track.path {
                AnimPath::Weights => track.target_name.clone(),
                _ => find_named_in_forest(scene, root, &track.target_name)
                    .and_then(|e| scene.with_component::<Name, _>(e, |n| n.name.clone()).ok())
                    .unwrap_or_else(|| track.target_name.clone()),
            });
            Ok(ClipBindingsResult { channels })
        },
    );
}

#[cfg(test)]
mod tests {
    use saffron_scene::{AssetEntry, AssetType};
    use serde_json::json;

    use crate::registry::{CommandRegistry, register_builtin_commands};
    use crate::selector::entity_uuid;
    use crate::test_support::{StubRenderer, with_stub};

    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    /// `set-skeleton-overlay` toggles the overlay flag and `get-skeleton-overlay` reads it
    /// back; the joint size is clamped to >= 0.5.
    #[test]
    fn skeleton_overlay_round_trips() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-skeleton-overlay", "params": { "show": true, "jointSize": 0.1 } }),
            );
            assert_eq!(set["ok"], json!(true));
            assert_eq!(set["result"]["show"], json!(true));
            assert_eq!(set["result"]["jointSize"], json!(0.5)); // clamped up

            let get = reg.dispatch(ctx, &json!({ "cmd": "get-skeleton-overlay" }));
            assert_eq!(get["result"]["show"], json!(true));
        });
    }

    /// `set-foot-ik` rejects a rig-less entity (foot IK needs a skeleton) and round-trips its
    /// enable flag on a rigged entity.
    #[test]
    fn foot_ik_requires_a_rig_and_round_trips() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            // A rig-less entity is rejected — no skeleton to attach foot IK to.
            let plain = ctx.scene_edit.active_scene().create_entity("plain");
            let plain_uuid = entity_uuid(ctx.scene_edit.active_scene(), plain).to_string();
            let rejected = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-foot-ik", "params": { "entity": plain_uuid, "enabled": true } }),
            );
            assert_eq!(rejected["ok"], json!(false));

            // A rigged entity (a SkinnedMesh in its forest) round-trips.
            let rig = ctx.scene_edit.active_scene().create_entity("rig");
            let _ = ctx
                .scene_edit
                .active_scene()
                .add_component(rig, saffron_scene::SkinnedMesh::default());
            let uuid = entity_uuid(ctx.scene_edit.active_scene(), rig).to_string();

            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-foot-ik", "params": { "entity": uuid, "enabled": true, "groundHeight": 1.5 } }),
            );
            assert_eq!(set["ok"], json!(true));
            assert_eq!(set["result"]["enabled"], json!(true));
            assert_eq!(set["result"]["groundHeight"], json!(1.5));
            assert_eq!(set["result"]["chains"], json!(0));

            let get = reg.dispatch(
                ctx,
                &json!({ "cmd": "get-foot-ik", "params": { "entity": uuid } }),
            );
            assert_eq!(get["result"]["enabled"], json!(true));
        });
    }

    /// A `play-animation` over a catalog clip sets the player and `get-animation-state`
    /// reports a coherent `AnimationStateResult` (clip id as a decimal string, wrap name,
    /// speed). Exercises the resolve-clip + player attach + state-of path on a stub rig.
    #[test]
    fn play_animation_reports_coherent_state() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            // Seed a clip into the catalog so resolve_clip + find() both resolve it.
            let clip_id = saffron_core::Uuid(4242);
            ctx.assets.catalog.put(AssetEntry {
                id: clip_id,
                name: "walk".to_owned(),
                asset_type: AssetType::Animation,
                duration: 1.25,
                tracks: 7,
                ..AssetEntry::default()
            });
            let entity = ctx.scene_edit.active_scene().create_entity("rig");
            let uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            let play = reg.dispatch(
                ctx,
                &json!({ "cmd": "play-animation", "params": { "entity": uuid, "clip": "walk", "speed": 2.0, "loop": false } }),
            );
            assert_eq!(play["ok"], json!(true));
            // Clip id emits as a decimal string.
            assert_eq!(play["result"]["clip"], json!("4242"));
            assert_eq!(play["result"]["clipName"], json!("walk"));
            assert_eq!(play["result"]["duration"], json!(1.25));
            assert_eq!(play["result"]["wrap"], json!("once"));
            assert_eq!(play["result"]["speed"], json!(2.0));
            assert_eq!(play["result"]["playing"], json!(true));

            let state = reg.dispatch(
                ctx,
                &json!({ "cmd": "get-animation-state", "params": { "entity": uuid } }),
            );
            assert_eq!(state["result"]["clipName"], json!("walk"));
            assert_eq!(state["result"]["wrap"], json!("once"));
        });
    }

    /// `list-clips` over the full catalog returns only Animation entries, decimal-string ids.
    #[test]
    fn list_clips_filters_to_animation_entries() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            ctx.assets.catalog.put(AssetEntry {
                id: saffron_core::Uuid(10),
                name: "run".to_owned(),
                asset_type: AssetType::Animation,
                duration: 0.5,
                tracks: 3,
                ..AssetEntry::default()
            });
            ctx.assets.catalog.put(AssetEntry {
                id: saffron_core::Uuid(11),
                name: "box".to_owned(),
                asset_type: AssetType::Mesh,
                ..AssetEntry::default()
            });
            let reply = reg.dispatch(ctx, &json!({ "cmd": "list-clips" }));
            assert_eq!(reply["ok"], json!(true));
            let clips = reply["result"]["clips"].as_array().unwrap();
            assert_eq!(clips.len(), 1);
            assert_eq!(clips[0]["id"], json!("10"));
            assert_eq!(clips[0]["name"], json!("run"));
        });
    }

    /// `set-morph-weights` writes a morph mesh's weights, `get-morph-weights` reads them back
    /// (with the durable names), `get-animation-state` reports them, and a length mismatch is
    /// an `Err`.
    #[test]
    fn morph_weight_commands_round_trip_and_reject_length_mismatch() {
        use saffron_scene::{AnimationPlayer, MorphComponent};

        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scene = ctx.scene_edit.active_scene();
            let entity = scene.create_entity("face");
            let _ = scene.add_component(
                entity,
                MorphComponent {
                    weights: vec![0.0, 0.0],
                    names: vec!["smile".to_owned(), "blink".to_owned()],
                },
            );
            // A player so get-animation-state has something to report the weights against.
            let _ = scene.add_component(entity, AnimationPlayer::default());
            let uuid = entity_uuid(ctx.scene_edit.active_scene(), entity).to_string();

            let set = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-morph-weights", "params": { "entity": uuid, "weights": [0.25, 0.75] } }),
            );
            assert_eq!(set["ok"], json!(true));
            assert_eq!(set["result"]["weights"], json!([0.25, 0.75]));
            assert_eq!(set["result"]["names"], json!(["smile", "blink"]));

            let got = reg.dispatch(
                ctx,
                &json!({ "cmd": "get-morph-weights", "params": { "entity": uuid } }),
            );
            assert_eq!(got["result"]["weights"], json!([0.25, 0.75]));

            let state = reg.dispatch(
                ctx,
                &json!({ "cmd": "get-animation-state", "params": { "entity": uuid } }),
            );
            assert_eq!(state["result"]["morphWeights"], json!([0.25, 0.75]));

            // A wrong-length vector is rejected.
            let bad = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-morph-weights", "params": { "entity": uuid, "weights": [0.5] } }),
            );
            assert_eq!(bad["ok"], json!(false));
        });
    }
}
