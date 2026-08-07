//! Command-keyed text formatting for `sa` replies.

use serde_json::Value;

/// Renders the `text`-mode line(s) for a command. Returning a `Vec<String>` rather than writing to
/// stdout keeps every formatter a pure function of `&Value`; the one side-effecting arm
/// (`profiler.capture-stop`, which writes an inline trace to a temp file) does its I/O here.
pub(crate) fn format_text(cmd: &str, result: &Value) -> Vec<String> {
    match cmd {
        "help" if result.get("commands").is_some() => format_help(result),
        "ping" => vec![format!(
            "pong  engine={}  version={}  pid={}",
            field_str(result, "engine"),
            field_str(result, "version"),
            field_i64(result, "pid"),
        )],
        "list-entities" if result.get("entities").is_some() => format_list_entities(result),
        "list-components" if result.get("components").is_some() => {
            field_array(result, "components")
                .iter()
                .map(|c| format!("  {}", c.as_str().unwrap_or("")))
                .collect()
        }
        "list-assets" if result.get("assets").is_some() => format_list_assets(result),
        "get-asset-model" => format_asset_model(result),
        "project-status" | "cancel-load" | "new-project" | "open-project" | "load-project"
        | "reload-project" => vec![format_project_status(result)],
        "list-clips" if result.get("clips").is_some() => field_array(result, "clips")
            .iter()
            .map(|c| format_clip(c, "", 32))
            .collect(),
        "render-stats" => vec![format_render_stats(result)],
        "profiler.set-mode" => vec![format!(
            "mode={}  timestamps={}  pipeline-stats={}{}",
            field_str(result, "mode"),
            yes_no(field_bool(result, "timestampsSupported")),
            yes_no(field_bool(result, "pipelineStatsSupported")),
            software_gpu_suffix(result),
        )],
        "pass-timings" => format_pass_timings(result),
        "profiler.capture-start" => vec![format!(
            "armed capture id={}  (stop with: sa profiler.capture-stop)",
            field_u64(result, "captureId"),
        )],
        "profiler.capture-stop" => format_capture_stop(result),
        "frame-history" => vec![format_frame_history(result)],
        "get-perf-config" | "set-perf-config" => vec![format_perf_config(result)],
        "vegetation-promote" | "vegetation-demote" | "vegetation-fell" => {
            vec![format_promotion(result)]
        }
        "vegetation-plant-vitals" => vec![format_plant_vitals(result)],
        "vegetation-telemetry" => format_vegetation_telemetry(result),
        "plant-elements" => format_plant_elements(result),
        "plant-create" | "plant-graph" | "plant-graph-set" | "plant-growth" => {
            format_plant_growth(result)
        }
        "vegetation-drain-events" => format_vegetation_events(result),
        "vegetation-nav-contributions" => format_vegetation_nav(result),
        "vegetation-advance-ecology" => vec![format_ecology_report(result)],
        "vegetation-ecology-status" => format_ecology_status(result),
        "vegetation-ecology-clock" => vec![format_ecology_clock(result)],
        "drain-alarms" => format_drain_alarms(result),
        "list-active-alarms" => format_active_alarms(result),
        "play" | "pause" | "stop" | "step" | "get-play-state" => vec![format!(
            "state={}  playVersion={}  sceneVersion={}  camera={}",
            field_str(result, "state"),
            field_i64(result, "playVersion"),
            field_i64(result, "sceneVersion"),
            if field_bool(result, "hasPrimaryCamera") {
                "ok"
            } else {
                "missing"
            },
        )],
        "physics-state" => vec![format!(
            "physics={}  bodies={}  dynamic={}",
            if field_bool(result, "active") {
                "active"
            } else {
                "inactive"
            },
            field_i64(result, "bodyCount"),
            field_i64(result, "dynamicCount"),
        )],
        "fit-collider" => vec![format_fit_collider(result)],
        "raycast" | "shapecast" => vec![format_raycast(result)],
        "enable-ragdoll" | "set-ragdoll" | "get-ragdoll" => vec![format!(
            "ragdoll={}  active={}  bodyWeight={:.2}  bones={}",
            if field_bool(result, "present") {
                "present"
            } else {
                "none"
            },
            yes_no(field_bool(result, "active")),
            field_f64(result, "bodyWeight"),
            field_i64(result, "bones"),
        )],
        "move-character" => {
            let p = result.get("position");
            vec![format!(
                "position=({:.3}, {:.3}, {:.3})  onGround={}",
                vec_component(p, "x"),
                vec_component(p, "y"),
                vec_component(p, "z"),
                yes_no(field_bool(result, "onGround")),
            )]
        }
        "set-kinematic-bones" => vec![format!(
            "kinematic-bones={}  entity={}  bones={}",
            if field_bool(result, "enabled") {
                "on"
            } else {
                "off"
            },
            field_str(result, "entity"),
            field_i64(result, "boneCount"),
        )],
        "drain-contacts" => format_drain_contacts(result),
        "viewport-native-info" => vec![format!(
            "{}  {}  {}x{}  sock={}",
            field_str(result, "status"),
            field_str(result, "transport"),
            field_u64(result, "width"),
            field_u64(result, "height"),
            field_str(result, "controlSocket"),
        )],
        "set-active-view" => vec![format!("view={}", field_str(result, "view"))],
        "get-selection" => vec![format_selection(result)],
        "add-entity" | "copy-entity" => vec![format!(
            "{}  id={}",
            field_str(result, "name"),
            field_str(result, "id"),
        )],
        "get-gizmo" | "set-gizmo" => vec![format!(
            "op={}  space={}",
            field_str(result, "op"),
            field_str(result, "space"),
        )],
        "gizmo-pointer" => vec![format!(
            "hovered={}  dragging={}",
            result
                .get("hovered")
                .and_then(Value::as_str)
                .unwrap_or("none"),
            yes_no(field_bool(result, "dragging")),
        )],
        "pick" => vec![if field_bool(result, "hit") {
            format!(
                "{}  {}  id={}",
                field_str(result, "kind"),
                field_str(result, "name"),
                field_str(result, "id"),
            )
        } else {
            "no hit".to_owned()
        }],
        "pick-skeleton-joint" => vec![if field_bool(result, "found") {
            format!(
                "joint node={}",
                result
                    .get("nodeIndex")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1),
            )
        } else {
            "no joint".to_owned()
        }],
        "get-camera" | "set-camera" => {
            let p = result.get("position");
            vec![format!(
                "pos=({:.2}, {:.2}, {:.2})  yaw={:.1}  pitch={:.1}  fov={:.1}",
                vec_component(p, "x"),
                vec_component(p, "y"),
                vec_component(p, "z"),
                field_f64(result, "yaw"),
                field_f64(result, "pitch"),
                field_f64(result, "fov"),
            )]
        }
        "get-thumbnail" | "view-asset" => {
            let b64_len = field_str(result, "base64").len();
            vec![format!(
                "{} {}x{}  ~{} bytes (base64 {} chars)",
                field_str(result, "format"),
                field_u64(result, "width"),
                field_u64(result, "height"),
                (b64_len / 4) * 3,
                b64_len,
            )]
        }
        _ => vec![pretty(result)],
    }
}

/// The `help` two-column table: each command name left-padded to 22 columns, then its summary, both
/// indented two spaces.
pub(crate) fn format_help(result: &Value) -> Vec<String> {
    field_array(result, "commands")
        .iter()
        .map(|entry| {
            format!(
                "  {:<22}  {}",
                field_str(entry, "name"),
                field_str(entry, "help"),
            )
        })
        .collect()
}

/// The `list-entities` table: id, name, parent id, each in a 24-wide column.
pub(crate) fn format_list_entities(result: &Value) -> Vec<String> {
    field_array(result, "entities")
        .iter()
        .map(|e| {
            format!(
                "  {:<24}  {:<24}  {}",
                field_str(e, "id"),
                field_str(e, "name"),
                field_str(e, "parentId"),
            )
        })
        .collect()
}

/// The `list-assets` table: type (8 wide), name (32 wide), id.
pub(crate) fn format_list_assets(result: &Value) -> Vec<String> {
    field_array(result, "assets")
        .iter()
        .map(|a| {
            format!(
                "  {:<8}  {:<32}  {}",
                field_str(a, "type"),
                field_str(a, "name"),
                field_str(a, "id"),
            )
        })
        .collect()
}

/// The `get-asset-model` report: header, capability counts, the indented bone tree, then the clip
/// list. The bone indent walks each bone's `parent` chain with a 256-iteration cycle guard so a
/// malformed/cyclic parent index cannot hang.
pub(crate) fn format_asset_model(result: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "model {}  (mesh {})",
        field_str(result, "name"),
        field_str(result, "mesh"),
    )];
    let caps = result.get("capabilities");
    lines.push(format!(
        "  meshes={}  materials={}  nodes={}  rig={}  bones={}  clips={}",
        nested_i64(caps, "meshCount"),
        nested_i64(caps, "materialCount"),
        nested_i64(caps, "nodeCount"),
        yes_no(nested_bool(caps, "hasRig")),
        nested_i64(caps, "boneCount"),
        nested_i64(caps, "clipCount"),
    ));
    let bones = field_array(result, "bones");
    for bone in &bones {
        let mut depth = 0_usize;
        let mut parent = bone.get("parent").and_then(Value::as_i64).unwrap_or(-1);
        let mut guard = 0;
        while parent >= 0 && (parent as usize) < bones.len() && guard < 256 {
            depth += 1;
            parent = bones[parent as usize]
                .get("parent")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            guard += 1;
        }
        lines.push(format!(
            "  {:indent$}{}{}",
            "",
            field_str(bone, "name"),
            if field_bool(bone, "joint") {
                "  [joint]"
            } else {
                ""
            },
            indent = depth * 2,
        ));
    }
    for clip in field_array(result, "clips") {
        lines.push(format_clip(&clip, "clip ", 28));
    }
    lines
}

/// One clip line shared by `list-clips` (no prefix, name width 32) and `get-asset-model` (a `clip `
/// prefix, name width 28): `[prefix]name  duration.s  id`.
pub(crate) fn format_clip(clip: &Value, prefix: &str, name_width: usize) -> String {
    format!(
        "  {prefix}{:<name_width$}  {:>8.3}s  {}",
        field_str(clip, "name"),
        field_f64(clip, "duration"),
        field_str(clip, "id"),
    )
}

/// The `render-stats` one-liner; the device-local memory reads `used/budget` in MiB.
pub(crate) fn format_render_stats(result: &Value) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!(
        "cpu={:.2}ms  gpu={:.2}ms  wait={:.2}ms  fps={:.0}  draws={}  tris={}  binds={}  pso+={}  vram={:.0}/{:.0}MiB{}",
        field_f64(result, "cpuFrameMs"),
        field_f64(result, "gpuFrameMs"),
        field_f64(result, "cpuWaitMs"),
        field_f64(result, "fps"),
        field_i64(result, "drawCalls"),
        field_i64(result, "triangles"),
        field_i64(result, "descriptorBinds"),
        field_i64(result, "pipelinesCreated"),
        field_u64(result, "vramUsageBytes") as f64 / MIB,
        field_u64(result, "vramBudgetBytes") as f64 / MIB,
        software_gpu_suffix(result),
    )
}

/// The `pass-timings` table: an optional software-gpu note, one line per pass, then the span total.
pub(crate) fn format_pass_timings(result: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if field_bool(result, "softwareGpu") {
        lines.push("[software-gpu: timings are CPU rasterization time, not hardware]".to_owned());
    }
    for pass in field_array(result, "passes") {
        lines.push(format!(
            "  {:<28}  {:>8.3} ms",
            field_str(&pass, "name"),
            field_f64(&pass, "gpuMs"),
        ));
    }
    lines.push(format!(
        "  {:<28}  {:>8.3} ms",
        "total (span)",
        field_f64(result, "gpuTotalMs"),
    ));
    lines
}

/// The `profiler.capture-stop` report: frame/span counts and a trace path. When the reply carries no
/// `path` but an inline `chromeTrace` string, that trace is written to
/// `<temp_dir>/saffron-profile.json` and that path is printed.
pub(crate) fn format_capture_stop(result: &Value) -> Vec<String> {
    if !field_bool(result, "ready") {
        return vec!["no capture ready (arm one with: sa profiler.capture-start)".to_owned()];
    }
    let capture = result.get("capture");
    let meta = capture.and_then(|c| c.get("metadata"));
    let span_count = capture
        .and_then(|c| c.get("spans"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut lines = vec![format!(
        "captured {} frame(s), {} spans  [{}{}]",
        field_u64(result, "frameCount"),
        span_count,
        if nested_bool(meta, "correlated") {
            "correlated"
        } else {
            "uncorrelated"
        },
        if nested_bool(meta, "softwareGpu") {
            ", software-gpu"
        } else {
            ""
        },
    )];
    let mut path = field_str(result, "path").to_owned();
    if path.is_empty() {
        let trace = field_str(result, "chromeTrace");
        if !trace.is_empty() {
            let target = std::env::temp_dir().join("saffron-profile.json");
            if std::fs::write(&target, trace).is_ok() {
                path = target.to_string_lossy().into_owned();
            }
        }
    }
    if !path.is_empty() {
        lines.push(format!(
            "trace: {path}  (open in chrome://tracing or ui.perfetto.dev)"
        ));
    }
    lines
}

/// The `frame-history` percentiles line.
pub(crate) fn format_frame_history(result: &Value) -> String {
    format!(
        "p50={:.2}  p95={:.2}  p99={:.2}  p99.9={:.2}  max={:.2}  stddev={:.2}  budget={:.2}ms  stutters={}  n={}",
        field_f64(result, "p50Ms"),
        field_f64(result, "p95Ms"),
        field_f64(result, "p99Ms"),
        field_f64(result, "p999Ms"),
        field_f64(result, "maxMs"),
        field_f64(result, "stddevMs"),
        field_f64(result, "budgetMs"),
        field_i64(result, "stutterCount"),
        field_i64(result, "sampleCount"),
    )
}

/// The `get-perf-config`/`set-perf-config` line; the vram fractions print as percentages.
pub(crate) fn format_perf_config(result: &Value) -> String {
    format!(
        "targetFps={:.0}  budget={:.2}ms  green<{:.2}×budget  amber<{:.1}×median  frozen={:.0}ms  vram warn/crit={:.0}%/{:.0}%",
        field_f64(result, "targetFps"),
        field_f64(result, "budgetMs"),
        field_f64(result, "greenBudgetFrac"),
        field_f64(result, "amberMedianMul"),
        field_f64(result, "frozenMs"),
        field_f64(result, "vramWarnFrac") * 100.0,
        field_f64(result, "vramCritFrac") * 100.0,
    )
}

/// The `drain-alarms` table: one line per event, then a summary footer.
pub(crate) fn format_drain_alarms(result: &Value) -> Vec<String> {
    let events = field_array(result, "events");
    let mut lines: Vec<String> = events
        .iter()
        .map(|e| {
            format!(
                "  #{:<5}  {:<8} {:<9} {:<13}  {:>8.2} / {:<8.2}  x{}",
                field_i64(e, "seq"),
                field_str(e, "state"),
                field_str(e, "severity"),
                field_str(e, "metric"),
                field_f64(e, "value"),
                field_f64(e, "threshold"),
                field_i64(e, "count"),
            )
        })
        .collect();
    lines.push(format!(
        "  high={}  oldest={}  overflowed={}  ({} events)",
        field_i64(result, "highWaterSeq"),
        field_i64(result, "oldestSeq"),
        yes_no(field_bool(result, "overflowed")),
        events.len(),
    ));
    lines
}

/// The `list-active-alarms` table: a "no active alarms" line when empty, else one line per alarm
/// with an optional `pass=` suffix.
pub(crate) fn format_active_alarms(result: &Value) -> Vec<String> {
    let alarms = field_array(result, "alarms");
    if alarms.is_empty() {
        return vec!["no active alarms".to_owned()];
    }
    alarms
        .iter()
        .map(|a| {
            let pass = field_str(a, "pass");
            format!(
                "  {:<9} {:<13}  {:>8.2} / {:<8.2}  x{}{}{}",
                field_str(a, "severity"),
                field_str(a, "metric"),
                field_f64(a, "value"),
                field_f64(a, "threshold"),
                field_i64(a, "count"),
                if pass.is_empty() { "" } else { "  pass=" },
                pass,
            )
        })
        .collect()
}

/// The `fit-collider` line: the fitted shape, entity, half-extents, and offset.
pub(crate) fn format_fit_collider(result: &Value) -> String {
    let he = result.get("halfExtents");
    let off = result.get("offset");
    format!(
        "fitted {}  entity={}  halfExtents=({:.3}, {:.3}, {:.3})  offset=({:.3}, {:.3}, {:.3})",
        field_str(result, "shape"),
        field_str(result, "entity"),
        vec_component(he, "x"),
        vec_component(he, "y"),
        vec_component(he, "z"),
        vec_component(off, "x"),
        vec_component(off, "y"),
        vec_component(off, "z"),
    )
}

/// A tagged world-hit target's display form: `entity=<uuid>`, `plant=<hex>`, or `unowned`.
/// The `vegetation-telemetry` lines: the last synchronization's stage times, the running average,
/// then the work counters and what is resident.
pub(crate) fn format_vegetation_telemetry(result: &Value) -> Vec<String> {
    let stages = |value: &Value| {
        format!(
            "residency={:.2}ms  promotion={:.2}ms  collision={:.2}ms  nav={:.2}ms  ecology={:.2}ms  total={:.2}ms",
            field_u64(value, "residencyUs") as f64 / 1000.0,
            field_u64(value, "promotionUs") as f64 / 1000.0,
            field_u64(value, "collisionUs") as f64 / 1000.0,
            field_u64(value, "navigationUs") as f64 / 1000.0,
            field_u64(value, "ecologyUs") as f64 / 1000.0,
            field_u64(value, "totalUs") as f64 / 1000.0,
        )
    };
    let null = Value::Null;
    let work = result.get("work").unwrap_or(&null);
    vec![
        format!("  last     {}", stages(result.get("last").unwrap_or(&null))),
        format!(
            "  average  {}",
            stages(result.get("average").unwrap_or(&null))
        ),
        format!(
            "  syncs={}  queries={} (hits {}, cells {}, nodes {}, rows {})  mutations={} ({} bytes)",
            field_str(work, "synchronizations"),
            field_str(work, "queries"),
            field_str(work, "queryHits"),
            field_str(work, "queryGenerationsVisited"),
            field_str(work, "queryNodesVisited"),
            field_str(work, "queryRowsTested"),
            field_str(work, "mutations"),
            field_str(work, "mutationBytes"),
        ),
        format!(
            "  snapshots={} ({} bytes)  ecologyTicks={}",
            field_str(work, "snapshots"),
            field_str(work, "snapshotBytes"),
            field_str(work, "ecologyTicks"),
        ),
        format!(
            "  bodies={}  navContributions={}  promoted={}",
            field_str(result, "collisionBodies"),
            field_str(result, "navigationContributions"),
            field_str(result, "promoted"),
        ),
        {
            let queue = result.get("cookQueue").unwrap_or(&Value::Null);
            format!(
                "  cook live={}  submitted={}  completed={}  cancelled={}  superseded={}  failed={}",
                field_str(queue, "live"),
                field_str(queue, "submitted"),
                field_str(queue, "completed"),
                field_str(queue, "cancelled"),
                field_str(queue, "superseded"),
                field_str(queue, "failed"),
            )
        },
    ]
}

/// A Q15.16 bit pattern as metres.
pub(crate) fn q16(bits: i64) -> f64 {
    bits as f64 / 65_536.0
}

/// The `plant-elements` lines: one per addressable axis, then one per placed element. The
/// identities printed here are what a manual edit targets.
pub(crate) fn format_plant_elements(result: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for axis in field_array(result, "axes") {
        let base = field_array(&axis, "baseBits");
        lines.push(format!(
            "  axis {:>34}  {:<8} base=({:.2}, {:.2}, {:.2})  r={:.3}  points={}",
            field_str(&axis, "id"),
            field_str(&axis, "element"),
            q16(base.first().and_then(Value::as_i64).unwrap_or(0)),
            q16(base.get(1).and_then(Value::as_i64).unwrap_or(0)),
            q16(base.get(2).and_then(Value::as_i64).unwrap_or(0)),
            q16(field_i64(&axis, "baseRadiusBits")),
            field_u64(&axis, "points"),
        ));
    }
    for element in field_array(result, "elements") {
        let position = field_array(&element, "positionBits");
        lines.push(format!(
            "  elem {:>34}  {:<8} at=({:.2}, {:.2}, {:.2})  size={:.3}  slot={}  axis={}",
            field_str(&element, "id"),
            field_str(&element, "element"),
            q16(position.first().and_then(Value::as_i64).unwrap_or(0)),
            q16(position.get(1).and_then(Value::as_i64).unwrap_or(0)),
            q16(position.get(2).and_then(Value::as_i64).unwrap_or(0)),
            q16(field_i64(&element, "sizeBits")),
            field_u64(&element, "materialSlot"),
            field_str(&element, "axis"),
        ));
    }
    lines
}

/// The growth summary line, plus one line per manual edit that found nothing to change.
pub(crate) fn format_plant_growth(result: &Value) -> Vec<String> {
    let growth = result.get("growth").unwrap_or(result);
    let mut lines = vec![format!(
        "axes={}  frames={}  shells={}  elements={}  verts={}  tris={}  parts={}  height={:.2}m  edits={}",
        field_u64(growth, "axes"),
        field_u64(growth, "frames"),
        field_u64(growth, "shells"),
        field_u64(growth, "elements"),
        field_u64(growth, "vertices"),
        field_u64(growth, "triangles"),
        field_u64(growth, "parts"),
        q16(field_i64(growth, "heightBits")),
        field_u64(growth, "appliedEdits"),
    )];
    for orphan in field_array(growth, "orphans") {
        lines.push(format!(
            "  orphan {:>34}  {}  {}",
            field_str(&orphan, "target"),
            field_str(orphan.get("action").unwrap_or(&Value::Null), "kind"),
            field_str(&orphan, "reason"),
        ));
    }
    lines
}

/// The `vegetation-nav-contributions` lines: one per contributing cell, the dirty regions, then
/// the totals.
pub(crate) fn format_vegetation_nav(result: &Value) -> Vec<String> {
    let cells = field_array(result, "cells");
    let mut lines: Vec<String> = cells
        .iter()
        .map(|cell| {
            let contributions = field_array(cell, "contributions");
            let obstacles = contributions
                .iter()
                .filter(|row| field_str(row, "kind") != "cost")
                .count();
            format!(
                "  cell {:<28}  {} contribution(s), {obstacles} obstacle(s)",
                world_cell_label(cell.get("cell").unwrap_or(&Value::Null)),
                contributions.len(),
            )
        })
        .collect();
    let dirty = field_array(result, "dirtyRegions");
    lines.push(format!(
        "  dirty={}  contributions={}  obstacles={} (dynamic {})  drained={}",
        dirty.len(),
        field_str(result, "contributions"),
        field_str(result, "obstacles"),
        field_str(result, "dynamicObstacles"),
        yes_no(field_bool(result, "drained")),
    ));
    lines
}

/// The `vegetation-drain-events` lines: one per committed transition, then the cursor summary.
pub(crate) fn format_vegetation_events(result: &Value) -> Vec<String> {
    let events = field_array(result, "events");
    let mut lines: Vec<String> = events
        .iter()
        .map(|event| {
            let plant = event
                .get("plant")
                .and_then(Value::as_str)
                .map_or_else(|| "cell-wide".to_owned(), |plant| format!("plant={plant}"));
            format!(
                "  #{:<5}  {:<18}  {}",
                field_str(event, "seq"),
                event
                    .get("transition")
                    .and_then(|transition| transition.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                plant,
            )
        })
        .collect();
    lines.push(format!(
        "  high={}  oldest={}  overflowed={}  ({} events)",
        field_str(result, "highWaterSeq"),
        field_str(result, "oldestSeq"),
        yes_no(field_bool(result, "overflowed")),
        events.len(),
    ));
    lines
}

/// The `vegetation-advance-ecology` line: what the call ran, what it still owes, and the checkpoint
/// the committed state now hashes to.
pub(crate) fn format_ecology_report(result: &Value) -> String {
    format!(
        "worldTick={}  ticksRun={} on {} workers  ticksOwed={} (+{} awaiting residency)  regions={} ({} caught up, {} awaiting residency)  checkpoint={}",
        field_str(result, "worldTick"),
        field_str(result, "ticksRun"),
        field_u64(result, "workers"),
        field_str(result, "ticksOwed"),
        field_str(result, "ticksAwaitingResidency"),
        field_i64(result, "regions"),
        field_i64(result, "regionsCaughtUp"),
        field_i64(result, "regionsAwaitingResidency"),
        short_hex(field_str(result, "checkpoint")),
    )
}

/// The `vegetation-ecology-clock` line: whether biology is advancing, how fast, and what it owes.
pub(crate) fn format_ecology_clock(result: &Value) -> String {
    format!(
        "clock={}  tick={}ms (+{}ms pending)  budget={} ticks/sync  workers={}  water={}  warmth={}  owed={}",
        if field_bool(result, "running") {
            "running"
        } else {
            "stopped"
        },
        field_u64(result, "tickMilliseconds"),
        field_str(result, "pendingMilliseconds"),
        field_u64(result, "maxTicksPerSync"),
        field_u64(result, "workers"),
        field_u64(result, "water"),
        field_u64(result, "warmth"),
        field_str(result, "ticksOwed"),
    )
}

/// The `vegetation-ecology-status` report: the world clock, one line per dependency region with its
/// tick and residency, then one line per cell's boundary summary.
pub(crate) fn format_ecology_status(result: &Value) -> Vec<String> {
    let regions = field_array(result, "regions");
    let cells = field_array(result, "cells");
    let mut lines = vec![
        format!(
            "worldTick={}  ruleSet={}  regionRadius={} cells  regions={}  checkpoint={}",
            field_str(result, "worldTick"),
            field_i64(result, "simulationVersion"),
            field_i64(result, "regionRadiusCells"),
            regions.len(),
            short_hex(field_str(result, "checkpoint")),
        ),
        format!(
            "  {}",
            format_ecology_clock(result.get("clock").unwrap_or(&Value::Null))
        ),
    ];
    for region in &regions {
        let members = field_array(region, "cells");
        let state = if !field_bool(region, "resident") {
            "awaiting residency"
        } else if field_bool(region, "caughtUp") {
            "caught up"
        } else {
            "behind"
        };
        lines.push(format!(
            "  region {:<28}  tick={:<8}  {state}",
            match members.first() {
                Some(cell) if members.len() == 1 => world_cell_label(cell),
                Some(cell) => format!("{} +{}", world_cell_label(cell), members.len() - 1),
                None => "empty".to_owned(),
            },
            field_str(region, "tick"),
        ));
    }
    for cell in &cells {
        lines.push(format!(
            "  cell   {:<28}  tick={:<8}  plants={:<5}  canopy={:<6} health={:<6} moisture={:<6} fuel={}",
            world_cell_label(cell.get("cell").unwrap_or(&Value::Null)),
            field_str(cell, "tick"),
            field_i64(cell, "plants"),
            field_i64(cell, "canopy"),
            field_i64(cell, "health"),
            field_i64(cell, "moisture"),
            field_i64(cell, "fuel"),
        ));
    }
    lines
}

/// A world cell as `x,y,z L<level>`. The coordinates cross as strings so the whole `i64` range
/// survives JavaScript, so they print as read.
pub(crate) fn world_cell_label(cell: &Value) -> String {
    let coordinates = field_array(cell, "coordinates");
    let axis = |index: usize| {
        coordinates
            .get(index)
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned()
    };
    format!(
        "{},{},{} L{}",
        axis(0),
        axis(1),
        axis(2),
        field_i64(cell, "level"),
    )
}

/// The leading 12 characters of a hex identity, which is enough to compare two by eye.
pub(crate) fn short_hex(value: &str) -> &str {
    &value[..value.len().min(12)]
}

/// The `vegetation-promote`/`vegetation-demote` line: the plant and the state its transition
/// leaves it in until the next synchronization point commits.
pub(crate) fn format_promotion(result: &Value) -> String {
    let state = result.get("state");
    let label = state
        .and_then(|state| state.get("state"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let entity = state
        .and_then(|state| state.get("entity"))
        .and_then(Value::as_str)
        .map(|entity| format!("  entity={entity}"))
        .unwrap_or_default();
    format!(
        "plant={}  state={label}{entity}",
        field_str(result, "plant")
    )
}

/// The `vegetation-plant-vitals` line: the live biology of one promoted plant's entity view.
pub(crate) fn format_plant_vitals(result: &Value) -> String {
    format!(
        "plant={}  entity={}  lifecycle={}  health={}  moisture={}  fuel={}  tick={}",
        field_str(result, "plant"),
        field_str(result, "entity"),
        field_str(result, "lifecycle"),
        field_u64(result, "health"),
        field_u64(result, "moisture"),
        field_u64(result, "fuel"),
        field_str(result, "ecologyTick"),
    )
}

pub(crate) fn format_hit_target(value: Option<&Value>) -> String {
    let Some(target) = value else {
        return "unowned".to_owned();
    };
    match target.get("kind").and_then(Value::as_str) {
        Some("scene-entity") => format!("entity={}", field_str(target, "id")),
        Some("vegetation") => format!("plant={}", field_str(target, "plant")),
        _ => "unowned".to_owned(),
    }
}

/// The `raycast`/`shapecast` line: the hit detail or `no hit`.
pub(crate) fn format_raycast(result: &Value) -> String {
    if !field_bool(result, "hit") {
        return "no hit".to_owned();
    }
    let p = result.get("point");
    let n = result.get("normal");
    format!(
        "hit {}  point=({:.3}, {:.3}, {:.3})  normal=({:.2}, {:.2}, {:.2})  dist={:.3}",
        format_hit_target(result.get("target")),
        vec_component(p, "x"),
        vec_component(p, "y"),
        vec_component(p, "z"),
        vec_component(n, "x"),
        vec_component(n, "y"),
        vec_component(n, "z"),
        field_f64(result, "distance"),
    )
}

/// The `drain-contacts` table: one line per event, then a summary footer.
pub(crate) fn format_drain_contacts(result: &Value) -> Vec<String> {
    let events = field_array(result, "events");
    let mut lines: Vec<String> = events
        .iter()
        .map(|e| {
            format!(
                "  #{:<5}  {:<6} {:<6}  {} <-> {}",
                field_i64(e, "seq"),
                field_str(e, "kind"),
                if field_bool(e, "sensor") {
                    "sensor"
                } else {
                    "solid"
                },
                format_hit_target(e.get("targetA")),
                format_hit_target(e.get("targetB")),
            )
        })
        .collect();
    lines.push(format!(
        "  high={}  oldest={}  overflowed={}  ({} events)",
        field_i64(result, "highWaterSeq"),
        field_i64(result, "oldestSeq"),
        yes_no(field_bool(result, "overflowed")),
        events.len(),
    ));
    lines
}

/// The `project-status` line: the project name, phase, boot stage, `n/m` when determinate, and the
/// human label — or the error on a failed load.
pub(crate) fn format_project_status(result: &Value) -> String {
    let name = field_str(result, "name");
    let phase = field_str(result, "phase");
    let error = field_str(result, "error");
    if !error.is_empty() {
        return format!("{name}  {phase}  error: {error}");
    }
    let stage = field_str(result, "stage");
    let label = field_str(result, "label");
    let (done, total) = (field_i64(result, "done"), field_i64(result, "total"));
    let progress = if total > 0 {
        format!("  {done}/{total}")
    } else {
        String::new()
    };
    format!("{name}  {phase}  [{stage}]{progress}  {label}")
}

/// The `get-selection` line: the selected entity name, or "no selection", with the selection/scene
/// versions.
pub(crate) fn format_selection(result: &Value) -> String {
    let sel_version = field_u64(result, "selectionVersion");
    let scene_version = field_u64(result, "sceneVersion");
    match result.get("entity") {
        Some(entity) if entity.is_object() => format!(
            "selected: {}  (sel v{sel_version}, scene v{scene_version})",
            field_str(entity, "name"),
        ),
        _ => format!("no selection  (sel v{sel_version}, scene v{scene_version})"),
    }
}

/// Pretty-prints a `Value` with UTF-8 left unescaped (`serde_json`'s default) so non-ASCII renders
/// literally.
pub(crate) fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `"yes"`/`"no"` for a bool flag.
pub(crate) fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

/// The `  [software-gpu]` suffix appended when the `softwareGpu` flag is set, else empty.
pub(crate) fn software_gpu_suffix(result: &Value) -> &'static str {
    if field_bool(result, "softwareGpu") {
        "  [software-gpu]"
    } else {
        ""
    }
}

/// Reads a string field, defaulting to `""`.
pub(crate) fn field_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Reads an integer field, defaulting to `0`.
pub(crate) fn field_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Reads an unsigned field, defaulting to `0`.
pub(crate) fn field_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Reads a float field, defaulting to `0.0`.
pub(crate) fn field_f64(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Reads a boolean field, defaulting to `false`.
pub(crate) fn field_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Reads an array field, defaulting to an empty slice (cloned into an owned `Vec` so the caller can
/// index it for the bone-tree walk).
pub(crate) fn field_array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Reads an integer field from an optional nested object, defaulting to `0`.
pub(crate) fn nested_i64(parent: Option<&Value>, key: &str) -> i64 {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Reads a boolean field from an optional nested object.
pub(crate) fn nested_bool(parent: Option<&Value>, key: &str) -> bool {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads one component (`x`/`y`/`z`) of an optional vector object, defaulting to `0.0`.
pub(crate) fn vec_component(parent: Option<&Value>, key: &str) -> f64 {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}
