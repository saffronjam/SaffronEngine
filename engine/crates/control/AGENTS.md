# saffron-control — the control plane

The synchronous `AF_UNIX` command server, the fn-pointer command registry, the `EngineContext`
borrow seam, and the wire dispatch from request DTOs (`saffron-protocol`) to engine subsystems. A
non-blocking, single-threaded socket drained once per frame from the host's main loop: no async
runtime, no worker thread. One request is one newline-delimited compact-JSON line; the reply echoes
the request `id` and carries `ok` plus exactly one of `result` / `error`.

The wire contract is DTO-first and generated — read `schemas/control/AGENTS.md` before adding or
changing a command payload. Vegetation is the largest single domain here: 47 of the manifest's
commands, plus the job machinery behind them.

## Layout

| Area | Files |
|---|---|
| Transport + dispatch | `server.rs`, `registry.rs`, `context.rs`, `selector.rs`, `error.rs` |
| Command registration | `commands_scene.rs`, `commands_asset.rs`, `commands_render.rs`, `commands_animation.rs`, `commands_physics.rs`, `commands_vegetation.rs`, `commands_vegetation_runtime.rs` |
| Vegetation jobs | `vegetation_jobs.rs`, `vegetation_cook_jobs.rs`, `owned_worker.rs` |
| Vegetation DTO conversion | `botanical_dto.rs`, `vegetation_cook_dto.rs`, `vegetation_layer_dto.rs`, `vegetation_mutation_dto.rs` |
| Project + tests | `project_loader.rs`, `test_support.rs` |

## Where the vegetation commands live

They are spread across **four** registration files, which is the least obvious thing in this crate.
Adding a command to the wrong one is easy and the split is by concern, not by name:

| File | Count | Concern |
|---|---:|---|
| `commands_asset.rs` | 14 | Authoring and catalog: `import-vegetation-asset`, all seven `plant-*`, `vegetation-asset-summary`, the two `*-points` interchange commands, the three `vegetation-map-*` commands |
| `commands_vegetation.rs` | 14 | Evaluation and cooking: `vegetation-cook`, `vegetation-start-evaluation`, `vegetation-compile-biome`, `vegetation-manifest`, `vegetation-explain-point`, the cancel/status pairs |
| `commands_vegetation_runtime.rs` | 18 | Runtime and persistent state: `vegetation-mutate`, `vegetation-promote`/`-demote`/`-fell`, the ecology commands, the `vegetation-state-*` and `vegetation-runtime-*` families |
| `commands_render.rs` | 1 | `vegetation-render-stats` — it reports renderer counters, so it registers with the render commands |

## Rules that are easy to break

- **Registration order is the manifest order.** `help` lists commands in registration order
  (`registry.rs`'s `help_lists_commands_in_registration_order`), and the live `help` output is
  compared against the generated manifest by `tools/check-control-schema/check.ts`. A new command
  inserted mid-table in `saffron-protocol`'s `command.rs` must be registered at the matching
  position, or `just schema` fails on an ordering diff rather than on anything that looks like the
  real mistake.
- **A long-running operation is a start/status/cancel triad, never a blocking call.** The socket is
  drained once per frame from the main loop, so a handler that blocks stalls the whole engine.
  `vegetation_jobs.rs` and `vegetation_cook_jobs.rs` own the job state; a handler starts work and
  returns a job id.
- **A cancelled or superseded job must not publish.** Publication is guarded by the generation
  token, so a job whose generation changed while it ran drops its result rather than overwriting
  newer state.
- **Wire shapes that are silent when wrong.** Rust enums serialize as `{"Available": {...}}` with
  capital variant keys, not camelCase. Mutation kinds are kebab-case. Optional fields are **omitted,
  never null**. Guids cross as 32-hex strings, and `PlantId` is a lowercase 32-hex string — never a
  number.
- **`vegetation-runtime-inspect`'s `persistent` field is a list**, in canonical cell order. A plant
  that moved has entries in two cells, so reading only the first silently hides one of them.
- **Every mutating command needs an inverse the editor can record.** Undo is reconstructed in the
  editor from paired control calls (`editor/AGENTS.md`), so a command with no expressible inverse is
  a command the user cannot undo.
- **A feature that adds engine state worth driving or inspecting gets a command here.** That is the
  repo-wide "Keep current" rule: one registration, so the running editor stays scriptable from `sa`.
