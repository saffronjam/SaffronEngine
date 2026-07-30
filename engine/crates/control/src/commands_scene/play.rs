use saffron_protocol::{
    DeselectResult, DrainScriptErrorsParams, DrainScriptErrorsResult, DrainScriptLogsParams,
    DrainScriptLogsResult, EmptyParams, PlayStateResult, ScriptErrorDto, ScriptLogDto,
    ScriptStatusResult, SelectionResult, SetScriptOverrideParams, SetScriptOverrideResult,
    StepParams, Uuid as WireUuid,
};
use saffron_scene::{Entity, Script};
use saffron_sceneedit::{PlayState, SceneEditContext};
use serde_json::{Map, Value};

use crate::error::Error;
use crate::registry::CommandRegistry;
use crate::selector::{entity_ref_dto, resolve_entity};

/// The uniform play-state reply.
pub(crate) fn play_state_result_dto(editor: &SceneEditContext) -> PlayStateResult {
    PlayStateResult {
        state: editor.play_state.name().to_owned(),
        play_version: editor.play_version as i32,
        scene_version: editor.scene_version as i32,
        has_primary_camera: editor.had_primary_camera,
        animation_version: editor.animation_version as i32,
        preview_asset: WireUuid(editor.preview_asset.value()),
    }
}

/// Lowercases a script-input key/button.
pub(crate) fn normalize_script_key(key: &str) -> String {
    key.to_ascii_lowercase()
}

/// Registers selection read-back, the play machine, and the scripting surface.
pub(crate) fn register_play_and_scripts(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, SelectionResult>(
        "get-selection",
        "get-selection — the current editor selection + scene/selection version stamps",
        |ctx, _params| {
            let sel = ctx.scene_edit.selected;
            let entity = if sel != Entity::NULL && ctx.scene_edit.active_scene().valid(sel) {
                let scene = ctx.scene_edit.active_scene();
                Some(entity_ref_dto(scene, sel))
            } else {
                None
            };
            Ok(SelectionResult {
                selection_version: ctx.scene_edit.selection_version as i32,
                scene_version: ctx.scene_edit.scene_version as i32,
                entity,
                play_state: ctx.scene_edit.play_state.name().to_owned(),
                play_version: ctx.scene_edit.play_version as i32,
                animation_version: ctx.scene_edit.animation_version as i32,
            })
        },
    );

    reg.register::<EmptyParams, DeselectResult>(
        "deselect",
        "deselect — clear the editor selection",
        |ctx, _params| {
            ctx.scene_edit.set_selection(Entity::NULL);
            Ok(DeselectResult {
                selection_version: ctx.scene_edit.selection_version as i32,
            })
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "play",
        "play — enter play mode (Edit) or resume (Paused)",
        |ctx, _params| {
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            if ctx.scene_edit.play_state == PlayState::Paused {
                ctx.scene_edit.resume_play().map_err(Error::command)?;
            } else {
                ctx.scene_edit.enter_play().map_err(Error::command)?;
            }
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "pause",
        "pause — freeze the running scene (Playing only)",
        |ctx, _params| {
            ctx.scene_edit.pause_play().map_err(Error::command)?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<StepParams, PlayStateResult>(
        "step",
        "step {frames=1} — advance fixed ticks (Paused only)",
        |ctx, params| {
            ctx.scene_edit
                .step_play(params.frames.unwrap_or(1))
                .map_err(Error::command)?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "stop",
        "stop — discard the play scene and restore the authored one",
        |ctx, _params| {
            ctx.scene_edit.stop_play().map_err(Error::command)?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "get-play-state",
        "get-play-state — the current play state + version",
        |ctx, _params| Ok(play_state_result_dto(ctx.scene_edit)),
    );

    reg.register::<EmptyParams, ScriptStatusResult>(
        "get-script-status",
        "get-script-status — play state, live script instances, error high-water",
        |ctx, _params| {
            Ok(ScriptStatusResult {
                state: ctx.scene_edit.play_state.name().to_owned(),
                instances: ctx.scene_edit.script_instance_count,
                error_high_water: ctx.scene_edit.script_error_seq,
            })
        },
    );

    reg.register::<SetScriptOverrideParams, SetScriptOverrideResult>(
        "set-script-override",
        "set-script-override {entity, slot, name, value} — write one per-instance script \
         field override (a null value clears it)",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            if !scene.has_component::<Script>(entity) {
                return Err(Error::command("entity has no Script component"));
            }
            let slot_count = scene
                .with_component::<Script, _>(entity, |c| c.scripts.len())
                .unwrap_or(0);
            if params.slot < 0 || params.slot as usize >= slot_count {
                return Err(Error::command(format!(
                    "slot {} out of range ({} slot(s))",
                    params.slot, slot_count
                )));
            }
            let (script_path, overrides) = scene
                .with_component_mut::<Script, _>(entity, |component| {
                    let slot = &mut component.scripts[params.slot as usize];
                    if !slot.overrides.is_object() {
                        slot.overrides = Value::Object(Map::new());
                    }
                    if params.value.is_null() {
                        if let Some(map) = slot.overrides.as_object_mut() {
                            map.remove(&params.name);
                        }
                    } else {
                        slot.overrides[&params.name] = params.value.clone();
                    }
                    (slot.script_path.clone(), slot.overrides.clone())
                })
                .map_err(Error::command)?;
            ctx.scene_edit.scene_version += 1;
            Ok(SetScriptOverrideResult {
                script_path,
                overrides,
            })
        },
    );

    reg.register::<DrainScriptErrorsParams, DrainScriptErrorsResult>(
        "drain-script-errors",
        "drain-script-errors {since} — script errors with seq > since (non-blocking)",
        |ctx, params| {
            let since = params.since.unwrap_or(0);
            let high_water_seq = ctx.scene_edit.script_error_seq;
            let oldest_seq = ctx.scene_edit.script_errors.first().map_or(0, |e| e.seq);
            // The ring drops its oldest entries; a cursor older than what survives means the
            // caller missed events.
            let overflowed = oldest_seq > 0 && since + 1 < oldest_seq;
            let events = ctx
                .scene_edit
                .script_errors
                .iter()
                .filter(|e| e.seq > since)
                .map(|e| ScriptErrorDto {
                    seq: e.seq,
                    entity: WireUuid(e.entity_uuid),
                    script: e.script.clone(),
                    message: e.message.clone(),
                    tick: e.tick,
                })
                .collect();
            Ok(DrainScriptErrorsResult {
                events,
                high_water_seq,
                oldest_seq,
                overflowed,
            })
        },
    );

    reg.register::<DrainScriptLogsParams, DrainScriptLogsResult>(
        "drain-script-logs",
        "drain-script-logs {since} — sa.log lines with seq > since (non-blocking)",
        |ctx, params| {
            let since = params.since.unwrap_or(0);
            let high_water_seq = ctx.scene_edit.script_log_seq;
            let oldest_seq = ctx.scene_edit.script_logs.first().map_or(0, |e| e.seq);
            let overflowed = oldest_seq > 0 && since + 1 < oldest_seq;
            let events = ctx
                .scene_edit
                .script_logs
                .iter()
                .filter(|e| e.seq > since)
                .map(|e| ScriptLogDto {
                    seq: e.seq,
                    entity: WireUuid(e.entity_uuid),
                    message: e.message.clone(),
                    epoch_ms: e.epoch_ms,
                    tick: e.tick,
                })
                .collect();
            Ok(DrainScriptLogsResult {
                events,
                high_water_seq,
                oldest_seq,
                overflowed,
            })
        },
    );
}
