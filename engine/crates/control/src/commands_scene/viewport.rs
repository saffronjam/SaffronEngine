use saffron_geometry::glam::Vec2;
use saffron_protocol::{
    EditorCamera, EmptyParams, FlyInputParams, FlyInputResult, GizmoOpDto, GizmoPointerParams,
    GizmoPointerPhase, GizmoPointerResult, GizmoSpaceDto, GizmoState, ScriptInputParams,
    ScriptInputResult, SetCameraParams, SetGizmoParams,
};
use saffron_scene::{Entity, Transform};
use saffron_sceneedit::{
    GizmoOp, GizmoSpace, NativeGizmoHandle, OrbitState, PlayState, SceneEditCamera,
    SceneEditContext,
};

use super::*;
use crate::error::Error;
use crate::registry::CommandRegistry;

/// The editor fly-camera as its wire DTO.
pub(crate) fn camera_dto(camera: &SceneEditCamera) -> EditorCamera {
    EditorCamera {
        position: from_glam3(camera.position),
        yaw: camera.yaw,
        pitch: camera.pitch,
        fov: camera.fov,
        near: camera.near_plane,
        far: camera.far_plane,
        move_speed: camera.move_speed,
        look_speed: camera.look_speed,
    }
}

/// Maps the backend-neutral [`GizmoOp`] to its wire spelling.
pub(crate) fn gizmo_op_dto(op: GizmoOp) -> GizmoOpDto {
    match op {
        GizmoOp::Rotate => GizmoOpDto::Rotate,
        GizmoOp::Scale => GizmoOpDto::Scale,
        GizmoOp::Translate => GizmoOpDto::Translate,
    }
}

/// Maps a wire op spelling to the backend-neutral [`GizmoOp`].
pub(crate) fn gizmo_op_from_dto(op: GizmoOpDto) -> GizmoOp {
    match op {
        GizmoOpDto::Rotate => GizmoOp::Rotate,
        GizmoOpDto::Scale => GizmoOp::Scale,
        GizmoOpDto::Translate => GizmoOp::Translate,
    }
}

/// Maps the backend-neutral [`GizmoSpace`] to its wire spelling.
pub(crate) fn gizmo_space_dto(space: GizmoSpace) -> GizmoSpaceDto {
    match space {
        GizmoSpace::Local => GizmoSpaceDto::Local,
        GizmoSpace::World => GizmoSpaceDto::World,
    }
}

/// Maps a wire space spelling to the backend-neutral [`GizmoSpace`].
pub(crate) fn gizmo_space_from_dto(space: GizmoSpaceDto) -> GizmoSpace {
    match space {
        GizmoSpaceDto::Local => GizmoSpace::Local,
        GizmoSpaceDto::World => GizmoSpace::World,
    }
}

/// The overlay handle's wire name.
pub(crate) fn native_gizmo_handle_name(handle: NativeGizmoHandle) -> &'static str {
    match handle {
        NativeGizmoHandle::X => "x",
        NativeGizmoHandle::Y => "y",
        NativeGizmoHandle::Z => "z",
        NativeGizmoHandle::Xy => "xy",
        NativeGizmoHandle::Yz => "yz",
        NativeGizmoHandle::Xz => "xz",
        NativeGizmoHandle::Screen => "screen",
        NativeGizmoHandle::Uniform => "uniform",
        NativeGizmoHandle::None => "none",
    }
}

/// The gizmo op/space/preserve-children as its wire DTO.
pub(crate) fn gizmo_state_dto(editor: &SceneEditContext) -> GizmoState {
    GizmoState {
        op: gizmo_op_dto(editor.gizmo_op),
        space: gizmo_space_dto(editor.gizmo_space),
        preserve_children: editor.preserve_children,
    }
}

/// Registers the editor camera, the gizmo, and the input commands.
pub(crate) fn register_viewport(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, EditorCamera>(
        "get-camera",
        "get-camera — the editor fly-camera state",
        |ctx, _params| Ok(camera_dto(&ctx.scene_edit.camera)),
    );

    reg.register::<SetCameraParams, EditorCamera>(
        "set-camera",
        "set-camera {position?, yaw?, pitch?, fov?, near?, far?, moveSpeed?, lookSpeed?, \
         pivot?, distance?} — pivot+distance eases the preview orbit along the arc; else a \
         free-eye set that snaps to position",
        |ctx, params| {
            let c = &mut ctx.scene_edit.camera;
            if let Some(f) = params.fov {
                c.fov = f;
            }
            if let Some(n) = params.near {
                c.near_plane = n;
            }
            if let Some(f) = params.far {
                c.far_plane = f;
            }
            if let Some(m) = params.move_speed {
                c.move_speed = m;
            }
            if let Some(l) = params.look_speed {
                c.look_speed = l;
            }
            match (params.pivot, params.distance) {
                (Some(pivot), Some(distance)) => {
                    // Orbit drive (the preview drag): ease pivot / distance / angles toward the
                    // sample; the per-frame update sweeps the eye along the arc.
                    let pivot = to_glam3(pivot);
                    if let Some(y) = params.yaw {
                        c.target_yaw = y;
                    }
                    if let Some(p) = params.pitch {
                        c.target_pitch = p.clamp(-89.0, 89.0);
                    }
                    if let Some(orbit) = c.orbit.as_mut() {
                        orbit.target_pivot = pivot;
                        orbit.target_distance = distance;
                    } else {
                        // Not framed into orbit yet: snap into it at the sample.
                        if let Some(y) = params.yaw {
                            c.yaw = y;
                        }
                        if let Some(p) = params.pitch {
                            c.pitch = p.clamp(-89.0, 89.0);
                        }
                        c.orbit = Some(OrbitState {
                            pivot,
                            distance,
                            target_pivot: pivot,
                            target_distance: distance,
                        });
                        c.position = pivot - c.forward() * distance;
                    }
                }
                _ => {
                    // Free-eye set (scripting, absolute framing restore): leave orbit mode and
                    // snap to the pose so a scripted set lands at once.
                    c.orbit = None;
                    if let Some(p) = params.position {
                        c.position = to_glam3(p);
                    }
                    if let Some(y) = params.yaw {
                        c.yaw = y;
                    }
                    if let Some(p) = params.pitch {
                        c.pitch = p;
                    }
                    c.sync_target();
                }
            }
            Ok(camera_dto(c))
        },
    );

    reg.register::<EmptyParams, GizmoState>(
        "get-gizmo",
        "get-gizmo — the gizmo op + space",
        |ctx, _params| Ok(gizmo_state_dto(ctx.scene_edit)),
    );

    reg.register::<SetGizmoParams, GizmoState>(
        "set-gizmo",
        "set-gizmo {op?:translate|rotate|scale, space?:world|local, preserveChildren?:0|1}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("gizmo is hidden during play"));
            }
            if let Some(op) = params.op {
                ctx.scene_edit.gizmo_op = gizmo_op_from_dto(op);
            }
            if let Some(space) = params.space {
                ctx.scene_edit.gizmo_space = gizmo_space_from_dto(space);
            }
            if let Some(preserve) = params.preserve_children {
                ctx.scene_edit.preserve_children = preserve;
            }
            Ok(gizmo_state_dto(ctx.scene_edit))
        },
    );

    reg.register::<GizmoPointerParams, GizmoPointerResult>(
        "gizmo-pointer",
        "gizmo-pointer {phase:hover|begin|drag|end, x, y} — drive the overlay gizmo \
         (x,y are NDC [-1,1])",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("gizmo is hidden during play"));
            }
            // Keep mode/space in sync with the backend-neutral gizmo state (the single source).
            ctx.scene_edit.sync_native_gizmo();
            let cam = ctx.scene_edit.camera.view();
            let width = ctx.renderer.viewport_width();
            let height = ctx.renderer.viewport_height();
            // NDC [-1,1] (top-left = -1,-1) → viewport pixels, matching the SDL pointer path.
            let x = params.x.unwrap_or(0.0);
            let y = params.y.unwrap_or(0.0);
            let mouse = Vec2::new(
                (x * 0.5 + 0.5) * width as f32,
                (y * 0.5 + 0.5) * height as f32,
            );

            let phase = params.phase.unwrap_or(GizmoPointerPhase::Hover);
            match phase {
                GizmoPointerPhase::Hover => {
                    ctx.scene_edit.native_gizmo.hovered =
                        ctx.scene_edit.hit_native_gizmo(&cam, width, height, mouse);
                }
                GizmoPointerPhase::Begin => {
                    let hovered = ctx.scene_edit.hit_native_gizmo(&cam, width, height, mouse);
                    ctx.scene_edit.native_gizmo.hovered = hovered;
                    let selected = ctx.scene_edit.selected;
                    if hovered != NativeGizmoHandle::None
                        && selected != Entity::NULL
                        && ctx
                            .scene_edit
                            .active_scene()
                            .has_component::<Transform>(selected)
                    {
                        ctx.scene_edit.native_gizmo.active = hovered;
                        ctx.scene_edit.native_gizmo.dragging = true;
                        ctx.scene_edit.native_gizmo.start_mouse = mouse;
                        ctx.scene_edit.native_gizmo.drag_target = mouse;
                        ctx.scene_edit.native_gizmo.drag_smoothed = mouse;
                        ctx.scene_edit.native_gizmo.drag_pending = false;
                        ctx.scene_edit.native_gizmo.target = selected;
                        ctx.scene_edit.snapshot_native_gizmo_start(selected);
                    }
                }
                GizmoPointerPhase::Drag => {
                    // Record the sample only; step_native_gizmo_drag smooths toward it every
                    // rendered frame, so ~60Hz pointer samples don't staircase on screen.
                    ctx.scene_edit.native_gizmo.drag_target = mouse;
                    ctx.scene_edit.native_gizmo.drag_pending = true;
                }
                GizmoPointerPhase::End => {
                    // Land exactly on the release position regardless of smoothing lag.
                    if ctx.scene_edit.native_gizmo.dragging {
                        ctx.scene_edit
                            .apply_native_gizmo_drag(&cam, width, height, mouse);
                    }
                    ctx.scene_edit.native_gizmo.dragging = false;
                    ctx.scene_edit.native_gizmo.drag_pending = false;
                    ctx.scene_edit.native_gizmo.active = NativeGizmoHandle::None;
                    ctx.scene_edit.native_gizmo.target = Entity::NULL;
                }
            }

            let handle = if ctx.scene_edit.native_gizmo.dragging {
                ctx.scene_edit.native_gizmo.active
            } else {
                ctx.scene_edit.native_gizmo.hovered
            };
            Ok(GizmoPointerResult {
                hovered: native_gizmo_handle_name(handle).to_owned(),
                dragging: ctx.scene_edit.native_gizmo.dragging,
            })
        },
    );

    reg.register::<FlyInputParams, FlyInputResult>(
        "fly-input",
        "fly-input {active, lookDx, lookDy, forward, back, left, right, up, down} — stream \
         editor fly-cam input (look deltas in pixels accumulate until the next frame)",
        |ctx, params| {
            let fly = &mut ctx.scene_edit.fly_input;
            fly.active = params.active.unwrap_or(false);
            fly.look_delta +=
                Vec2::new(params.look_dx.unwrap_or(0.0), params.look_dy.unwrap_or(0.0));
            fly.forward = params.forward.unwrap_or(false);
            fly.back = params.back.unwrap_or(false);
            fly.left = params.left.unwrap_or(false);
            fly.right = params.right.unwrap_or(false);
            fly.up = params.up.unwrap_or(false);
            fly.down = params.down.unwrap_or(false);
            if !fly.active {
                fly.look_delta = Vec2::ZERO;
            }
            Ok(FlyInputResult { active: fly.active })
        },
    );

    reg.register::<ScriptInputParams, ScriptInputResult>(
        "script-input",
        "script-input {keys, mouseButtons?, mouseX?, mouseY?, scroll?} — forward gameplay \
         input to Lua",
        |ctx, params| {
            let input = &mut ctx.scene_edit.script_input;
            input.held.clear();
            for key in &params.keys {
                let normalized = normalize_script_key(key);
                if !normalized.is_empty() {
                    input.held.insert(normalized);
                }
            }
            if let Some(buttons) = &params.mouse_buttons {
                input.mouse_buttons.clear();
                for button in buttons {
                    let normalized = normalize_script_key(button);
                    if !normalized.is_empty() {
                        input.mouse_buttons.insert(normalized);
                    }
                }
            }
            if let Some(x) = params.mouse_x {
                input.mouse_x = x;
            }
            if let Some(y) = params.mouse_y {
                input.mouse_y = y;
            }
            if let Some(scroll) = params.scroll {
                input.scroll = scroll;
            }
            let mut keys: Vec<String> = input.held.iter().cloned().collect();
            keys.sort();
            Ok(ScriptInputResult { keys })
        },
    );
}
