# Phase 3 — Progress protocol, boot stages, and cancel command

**Status:** NOT STARTED

## Goal

Expose the engine-internal `ProjectLoadProgress` (Phase 2) over the control plane as a pollable,
read-only `project-status` command returning a `ProjectStatusDto` — coarse phase, current boot stage,
`done`/`total`, a human label, current item, error text, and a monotonic `version` — and add a
`cancel-load` command that flips the loader's cancel flag and returns the post-cancel snapshot. Freeze
the two DTO enums plus the struct into the generated protocol artifacts, wire the `sa` CLI formatter,
and document the whole project-load lifecycle. This is the exact surface the editor loading bar polls in
Phase 4.

NO-LEGACY finish carried over from Phase 2: the `open-project` / `new-project` / `load-project` /
`reload-project` handlers change their **result type** from `ProjectInfoDto` to `ProjectStatusDto` (the
initial `Loading` snapshot). There is no dual sync-return + async-poll shape — the real result arrives
only through `project-status` polling.

## The ordered boot-stage list (authoritative)

`ProjectPhase::Loading` is subdivided into ordered `BootStage`s. This is the concrete, ordered list the
`ProjectStatusDto.stage` field carries; the human labels below are what the loader writes into
`ProjectLoadProgress.label` (Phase 2), and the editor renders them verbatim. "Knowable n/m" says whether
a determinate `done`/`total` exists (otherwise `total == 0` ⇒ indeterminate spinner). "Where" says which
thread/subsystem does the work.

| Order | `BootStage` | Human label | n/m knowable? | Where |
|---|---|---|---|---|
| 1 | `Manifest` | "Reading project" | No (indeterminate) | Doc worker — CPU/IO: `read_to_string` + `parse_json` + version gate on `project.json` |
| 2 | `Catalog` | "Scanning assets N/M" | Yes — `done`/`total` over files enumerated by the cold disk scan | Doc worker — CPU/IO: catalog reconcile vs. disk (`scan_assets` / `load_catalog`) |
| 3 | `Scene` | "Loading scene" | No (indeterminate) | Doc worker — CPU: `scene_from_json` into a fresh `Scene` + residency-job set build |
| 4 | `Install` | "Installing project" | No (one frame) | Main thread — one bounded step: `wait_gpu_idle` + `clear_asset_caches` + swap scene/catalog + `enqueue_residency` |
| 5 | `Assets` | "Loading assets N/M" | Yes — `done`/`total` = `resident`/`total` residency jobs | Residency worker (`AssetLoadWorker`) — CPU decode + GPU upload of meshes/textures |
| 6 | `Skybox` | "Baking environment" | No (warmup) | GPU — sky panorama resident + IBL env bake (renderer `warmup_state().env_bake`) |
| 7 | `Accel` | "Building acceleration structures" | No (warmup) | GPU — RT TLAS build / first-frame priming (renderer `warmup_state().accel`) |
| — | `Ready` | "Ready" | — | terminal success (loader flips `ProjectPhase::Ready`) |
| — | `Failed` | (carries `error`) | — | terminal failure (loader sets `ProjectPhase::Failed`) |

This ordering must match the `BootStage` enum landed in Phase 2 (`saffron-sceneedit`, next to
`ProjectPhase`) exactly — the DTO enum below is a 1:1 kebab-case mirror. Determinate stages (`Catalog`,
`Assets`) drive a real progress bar; the rest render as a spinner with the label.

## DTOs — `engine/crates/protocol/src/dto.rs`

Mirror the kebab-enum pattern of `AssetPlacementPhaseDto` (`dto.rs:1386`) and the read-only
`EmptyParams`-in / typed-out shape of `get-project` (`ProjectInfoDto`, `dto.rs:1289`).

```rust
/// Coarse project-load lifecycle (mirrors `saffron_sceneedit::ProjectPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProjectPhaseDto {
    Unloaded,
    Loading,
    Ready,
    Failed,
}

/// The current boot stage within `Loading` (mirrors `saffron_sceneedit::BootStage`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BootStageDto {
    Manifest,
    Catalog,
    Scene,
    Install,
    Assets,
    Skybox,
    Accel,
    Ready,
    Failed,
}

/// Project-load phase + progress. `total == 0` means indeterminate (spinner). `version` is
/// monotonic; the editor dedups on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectStatusDto {
    pub phase: ProjectPhaseDto,
    pub stage: BootStageDto,
    pub done: i32,
    pub total: i32,
    pub label: String,
    pub current_item: String,
    pub error: String,
    pub version: i64,
    pub name: String,
    pub path: String,
}
```

Params: both commands reuse `EmptyParams` (`dto.rs:355`). `cancel-load` returns `ProjectStatusDto` (the
post-cancel snapshot, typically `phase: Unloaded`).

Field-width note: `ProjectLoadProgress` (Phase 2) uses `u32` for `done`/`total` and `u64` for `version`;
the DTO uses `i32`/`i64` to match the existing signed-integer wire convention (schemars/ts-rs emit
`number`). The `project_status_dto` mapper below performs the widening cast.

## Command manifest — `engine/crates/protocol/src/command.rs`

Add two `CommandSpec` rows (`COMMANDS`, `command.rs:36`) inside the **asset domain**, in frozen order,
immediately after the `get-project` row and before `quit`:

```rust
CommandSpec {
    name: "project-status",
    summary: "project-load phase + progress",
    params: "EmptyParams",
    result: "ProjectStatusDto",
},
CommandSpec {
    name: "cancel-load",
    summary: "abort the in-flight project load",
    params: "EmptyParams",
    result: "ProjectStatusDto",
},
```

Then satisfy every compile-time contract test in the same change (search the `#[cfg(test)]` module in
`command.rs`):

- `DTO_TYPE_NAMES`: add `"ProjectStatusDto"`, `"ProjectPhaseDto"`, `"BootStageDto"` so
  `every_command_type_name_resolves_to_a_dto` stays green.
- `COMMAND_FIXTURES`: add `("project-status", "empty")` and `("cancel-load", "empty")` so
  `every_command_has_exactly_one_of_fixture_or_skip` stays green.
- The `asset_domain()` expected command list: add `"project-status"` and `"cancel-load"` in the same
  position, so the domain-partition test stays clean; the domain endpoints remain `get-project` … `quit`.
- Update the module-doc command-count comment (`command.rs:8` neighborhood) — comment only, no behavior.

## Codegen — `engine/crates/protocol/src/codegen.rs`

- `ts_decls()` (`codegen.rs:26`): add `decl_entry!(ProjectPhaseDto)` and `decl_entry!(BootStageDto)` in
  the enum block near the other kebab enums (around `codegen.rs:38`, alongside `GizmoSpaceDto`), and
  `decl_entry!(ProjectStatusDto)` near the project DTOs (next to `decl_entry!(ProjectInfoDto)`,
  `codegen.rs:144`).
- `struct_fragments()` (`codegen.rs:314`): add `frag_entry!(ProjectStatusDto)` next to
  `frag_entry!(ProjectInfoDto)` (`codegen.rs:417`). **Structs only** — the two enums stay inline and get
  no `frag_entry!` (mirroring `AssetPlacementPhaseDto`, which appears in `ts_decls` but not
  `struct_fragments`).
- `schema.rs` needs no change: `ProjectStatusDto` has no selector / free-form `serde_json::Value`
  fields, so the object-shape validators (`schema.rs:718` onward) apply unchanged.

## Registration — `engine/crates/control/src/commands_asset.rs`

In `register_asset_commands` (`commands_asset.rs:1136`), immediately after the `get-project` registration
block (`commands_asset.rs:1137`), in frozen order:

```rust
reg.register::<EmptyParams, ProjectStatusDto>(
    "project-status",
    "project-status — project-load phase + progress",
    |ctx, _params| Ok(project_status_dto(&ctx.scene_edit)),
);
reg.register::<EmptyParams, ProjectStatusDto>(
    "cancel-load",
    "cancel-load — abort the in-flight project load",
    |ctx, _params| {
        ctx.scene_edit.project_cancel = true;
        Ok(project_status_dto(&ctx.scene_edit))
    },
);
```

Add a private mapper alongside the existing `current_project_info` / `apply_project_info` helpers
(`commands_asset.rs:598`–`609`):

```rust
/// Snapshot the authoritative project-load phase + progress into the wire DTO.
fn project_status_dto(sc: &SceneEditContext) -> ProjectStatusDto {
    let p = &sc.project_load;
    ProjectStatusDto {
        phase: match sc.project_phase {
            ProjectPhase::Unloaded => ProjectPhaseDto::Unloaded,
            ProjectPhase::Loading => ProjectPhaseDto::Loading,
            ProjectPhase::Ready => ProjectPhaseDto::Ready,
            ProjectPhase::Failed => ProjectPhaseDto::Failed,
        },
        stage: match p.stage {
            BootStage::Manifest => BootStageDto::Manifest,
            BootStage::Catalog => BootStageDto::Catalog,
            BootStage::Scene => BootStageDto::Scene,
            BootStage::Install => BootStageDto::Install,
            BootStage::Assets => BootStageDto::Assets,
            BootStage::Skybox => BootStageDto::Skybox,
            BootStage::Accel => BootStageDto::Accel,
            BootStage::Ready => BootStageDto::Ready,
            BootStage::Failed => BootStageDto::Failed,
        },
        done: p.done as i32,
        total: p.total as i32,
        label: p.label.clone(),
        current_item: p.current_item.clone(),
        error: p.error.clone(),
        version: p.version as i64,
        name: sc.project_name().to_string(),
        path: sc.project_path().map(|p| p.display().to_string()).unwrap_or_default(),
    }
}
```

`name` / `path` read from the same `SceneEditContext` accessors `current_project_info`
(`commands_asset.rs:598`) already uses — reuse those exact getters rather than reaching into fields, so
there is one source of the project identity. Import `ProjectPhase` / `BootStage` from `saffron_sceneedit`
and the three new DTOs from `saffron_protocol` at the top of the file (the crate already depends on both).

Update the in-file register-order test that asserts the `asset_domain` command sequence
(`commands_asset.rs` test module, register-order assertion) to include `"project-status"` and
`"cancel-load"` in the frozen position between `get-project` and `quit`.

## Allow-list + read-only classification — `engine/crates/control/src/registry.rs`

- Extend `is_loading_safe_command` (added in Phase 1) so it now reads:
  ```rust
  pub fn is_loading_safe_command(name: &str) -> bool {
      matches!(name, "ping" | "help" | "get-project" | "project-status" | "cancel-load" | "quit")
  }
  ```
  These are exactly the commands the editor needs while `ProjectPhase::Loading` — the poll
  (`project-status`), the abort (`cancel-load`), the liveness probes, and quit. Everything else stays
  discarded with `{ok:false, code:"busy-loading"}` (the dispatch gate, `registry.rs:427`).
- Add `"project-status"` to the explicit `matches!` set in `is_read_only_command`
  (`registry.rs:572`) — it is a pure read but does not carry a `get-` / `list-` prefix, so it needs an
  explicit entry to avoid being treated as a mutation. Leave `cancel-load` **out** of the read-only set:
  it mutates load state, so its completion should follow the mutating path (request a redraw), matching
  how other state-changing commands are classified.

## `open` / `new` / `reload` return the initial snapshot (NO-LEGACY finish of Phase 2)

Change the result type of the lifecycle handlers from `ProjectInfoDto` to `ProjectStatusDto`:

- `new-project` (`reg.register::<NewProjectParams, ProjectInfoDto>`, `commands_asset.rs:1143`) →
  `reg.register::<NewProjectParams, ProjectStatusDto>`, returning `project_status_dto(&ctx.scene_edit)`
  after seeding the loader inbox (the Phase-2 kick-off).
- `open-project` (`reg.register::<PathParams, ProjectInfoDto>`, `commands_asset.rs:1194`) →
  `reg.register::<PathParams, ProjectStatusDto>`, same immediate-`Loading`-snapshot return.
- `load-project` / `reload-project` (routed via `load_project_into`, per Phase 2) → same change to
  `ProjectStatusDto`.

`get-project` keeps returning `ProjectInfoDto` (its `loaded` field is derived from `project_ready()`,
Phase 1) — it is the steady-state identity query, distinct from the transient load snapshot. There is no
dual return shape: the poll surface is `project-status`, the identity surface is `get-project`.

Update the corresponding `command.rs` `COMMANDS` rows' `result` for `new-project` / `open-project` /
`load-project` / `reload-project` to `"ProjectStatusDto"`, and update `DTO_TYPE_NAMES` / `COMMAND_FIXTURES`
accordingly (their fixtures are unchanged in shape — params still `EmptyParams`-adjacent; only the result
name changes). Update the editor client wrappers in Phase 4.

## `sa` CLI — `engine/crates/sa/src/main.rs`

`sa project-status` and `sa cancel-load` reach the registry free via `external_subcommand`
(`main.rs:99`) — no new clap subcommand is needed. Add a human one-liner to `format_text`
(`main.rs:331`), next to the `render-stats` (`main.rs:353`) and `physics-state` (`main.rs:382`) arms:

```rust
"project-status" | "cancel-load" => vec![format!(
    "phase={} stage={} {}/{} '{}'{}",
    r["phase"], r["stage"], r["done"], r["total"], r["label"],
    if r["error"].as_str().unwrap_or("").is_empty() {
        String::new()
    } else {
        format!(" error={}", r["error"])
    },
)],
```

Add a `format_text` unit test in the same test module (mirroring the `render-stats` / `physics-state`
tests around `main.rs:1595`–`1782`): a fabricated `project-status` value formats to the expected
`phase=… stage=… N/M '…'` line, and a value with a non-empty `error` appends `error=…`.

## Docs — `docs/content/explanations/tooling-and-control/`

Add one Diátaxis **explanation** page, `project-load-lifecycle.md`, in the tooling-and-control section
(the same hub the other control pages live under, `docs/content/explanations/tooling-and-control/`).
Follow the house page conventions: TOML front matter, `title` a short sentence-case noun phrase equal to
the body `# H1`; lead with the concept and *why* (staying responsive while a thick project loads), not
"file X does Y". Cover:

- The `ProjectPhase` state machine (`Unloaded → Loading → Ready / Failed`) and that it is one
  authoritative axis on `SceneEditContext`, distinct from the engine-process lifecycle.
- The ordered `BootStage` list (the table above), calling out which stages are determinate `n/m`
  (`Catalog`, `Assets`) versus indeterminate spinners, and CPU/IO vs. GPU.
- The non-blocking design in one paragraph: the off-thread `ProjectDocWorker` (CPU/IO) + persistent
  `AssetLoadWorker` (GPU decode+upload), the one-frame main-thread install step, and warmup hold on
  `warmup_state()`.
- The `project-status` (poll) and `cancel-load` commands and the `busy-loading` dispatch gate that
  discards non-allow-listed commands mid-load.

Include a slim `What | File | Symbols` table (symbols, not line numbers), e.g.:

| What | File | Symbols |
|---|---|---|
| Load orchestrator | `engine/crates/host/src/layer.rs` | `ProjectLoader`, `ProjectLoader::advance` |
| Residency worker | `engine/crates/assets/src/asset_load_worker.rs` | `AssetLoadWorker`, `LoadJob` |
| Phase + progress state | `engine/crates/sceneedit/src/context.rs` | `ProjectPhase`, `BootStage`, `ProjectLoadProgress` |
| Status command | `engine/crates/control/src/commands_asset.rs` | `project-status`, `cancel-load`, `project_status_dto` |
| Wire DTO | `engine/crates/protocol/src/dto.rs` | `ProjectStatusDto`, `ProjectPhaseDto`, `BootStageDto` |

Add a ` ```mermaid ` state diagram: `Unloaded → Loading` and, within `Loading`, the stage chain
`Manifest → Catalog → Scene → Install → Assets → Skybox → Accel`, then `→ Ready`, with `Failed` and the
`cancel-load → Unloaded` edges. Run the prose through the `humanizer` pass (plain, direct voice; no
change-journey notes).

Add a row to the section hub `docs/content/explanations/tooling-and-control/_index.md` "Pages" table:

```
| `project-load-lifecycle` | the project-load phase machine + boot stages, non-blocking doc/residency workers, `project-status` / `cancel-load`, the `busy-loading` gate | `engine/crates/host/src/layer.rs`; `engine/crates/control/src/commands_asset.rs` |
```

## Cross-cutting NO-LEGACY / gate notes

- No dual result shape: `open`/`new`/`reload` return only `ProjectStatusDto`; `get-project` stays
  `ProjectInfoDto`. Do not keep a `ProjectInfoDto`-returning variant of the lifecycle commands alongside.
- The DTO enums are a strict 1:1 mirror of the `saffron-sceneedit` truth enums — do not diverge the
  variant sets or ordering.
- Milestone gate at this phase boundary: `just engine` then `just prepare-for-commit`; regenerate the
  protocol (a wire type changed); run `just e2e`. Do not defer to the end. Leave changes unstaged — the
  user stages/commits.

## Ordering / dependencies

Depends on Phase 2 (the `ProjectLoadProgress` snapshot + `BootStage` enum + `project_cancel` flag +
`ProjectPhase::Loading` given real duration). Blocks Phase 4 (the editor consumes `project-status` /
`cancel-load` + the regenerated `@saffron/protocol`).

## Verification

1. `cargo run -p xtask -- gen-protocol` (from the toolbox) regenerates
   `schemas/control/openrpc.generated.json`, `schemas/control/command-manifest.generated.json`, and
   `editor/src/protocol/sa-types.ts` byte-for-byte with the three new types + the changed lifecycle
   result types; commit the regenerated artifacts (never hand-edit `sa-types.ts`).
2. `just engine` + `just prepare-for-commit` clean (clippy `-D warnings`). All `command.rs` contract
   tests (`every_command_type_name_resolves_to_a_dto`, `every_command_has_exactly_one_of_fixture_or_skip`,
   the domain-partition test), the `commands_asset.rs` register-order test, the `codegen.rs` coverage
   tests, and `component_schemas_match_committed_openrpc` (`schema.rs`) pass.
3. `tools/check-control-schema` passes (the live registry matches the committed manifest, including the
   two new commands and the changed result names).
4. Headless behavior proof: `just run-engine-headless` a thick project while a driver polls
   `sa project-status` — it prints an advancing `phase=loading stage=… N/M '…'` sequence through
   `manifest → catalog → scene → install → assets → skybox → accel`, ending `phase=ready stage=ready`.
   `sa cancel-load` mid-load aborts, and the next `sa project-status` shows `phase=unloaded`.
5. `just run-docs` builds; the new `project-load-lifecycle` page and its hub row render, and the mermaid
   diagram displays.
