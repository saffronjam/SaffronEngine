# Phase 1 — Project-load state machine + command gate

**Status:** COMPLETED

Part of the `plans/loading-screen/` feature (non-blocking project load + loading screen). This is the
first, self-contained phase: it lands the state machine and the dispatch-level gate that later phases
rely on, but load is still synchronous at the end of this phase, so the gate is correct-but-dormant.

## Goal

Replace the boolean `SceneEditContext.project_loaded` with a `ProjectPhase` state machine
`{ Unloaded, Loading, Ready, Failed }`, add a single dispatch-level gate in `CommandRegistry::dispatch`
that rejects non-allow-listed commands while `Loading` with a typed `Error::Busy` (wire
`code: "busy-loading"`), add a machine-readable `code` to every `ok:false` envelope, and teach the
editor control client to drop a busy reply silently on background poll lanes.

Because the control server is single-threaded and drained once per frame, no second command can arrive
mid-synchronous-load in Phase 1 — the gate never actually fires yet. Phase 2 gives `Loading` real
duration (the load moves off-thread) and the gate becomes load-bearing. This phase must build clean and
pass its own tests in isolation.

## NO-LEGACY checklist for this phase

- Zero `project_loaded` references survive tree-wide (grep returns nothing). The bool is fully replaced
  by `ProjectPhase` + the `project_ready()` convenience, and every reader is migrated in the same change.
- The `ok:false` envelope gains a `code` on **every** error path (gate + `Err` arm + unknown-command),
  not a second parallel error shape.

## Engine crate: `saffron-sceneedit`

The phase lives in `saffron-sceneedit` because that crate has no protocol dependency and already owns
the authoritative editor state that both the dispatch gate and the status command read through
`EngineContext.scene_edit`.

**File `engine/crates/sceneedit/src/context.rs`.**

1. Add the phase enum near the top of the crate. Prefer a small new `project.rs` module in the crate
   re-exported from `lib.rs` (keeps `context.rs` from growing a second concern); a `pub enum` at the top
   of `context.rs` is acceptable if the module split is noise. Either way it must be reachable as
   `saffron_sceneedit::ProjectPhase`:

   ```rust
   /// The project bring-up lifecycle. The single authoritative phase the dispatch gate and the
   /// `project-status` command read.
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
   pub enum ProjectPhase {
       /// No project is open.
       #[default]
       Unloaded,
       /// A load is in flight (doc parse, install, residency, warmup). Set by the loader.
       Loading,
       /// A project is fully open and resident.
       Ready,
       /// The last load failed; the progress snapshot (Phase 2) carries the message.
       Failed,
   }
   ```

2. Replace the field `pub project_loaded: bool` on `SceneEditContext` with
   `pub project_phase: ProjectPhase`. Remove the corresponding `project_loaded: false` initializer in
   the `SceneEditContext` constructor / `Default` impl — `ProjectPhase::default()` yields `Unloaded`, so
   no explicit initializer is needed (or set `project_phase: ProjectPhase::default()` for clarity).

3. Add the convenience read used by the guard sites (roughly twenty callers across `saffron-control`):

   ```rust
   impl SceneEditContext {
       /// True only when a project is fully open and resident. The guard the per-handler
       /// `require_project_loaded` and inline project checks read.
       #[must_use]
       pub fn project_ready(&self) -> bool {
           self.project_phase == ProjectPhase::Ready
       }
   }
   ```

   > Phase 1 lands **only** the phase enum + `project_ready()`. Phase 2 adds the `ProjectLoadProgress`
   > snapshot field, the `ProjectLoadRequest` inbox, the cancel flag, and the `BootStage` enum here — do
   > not add them now.

4. Re-export `ProjectPhase` from `engine/crates/sceneedit/src/lib.rs` so `saffron-control` and
   `saffron-host` reach it as `saffron_sceneedit::ProjectPhase`.

## Engine crate: `saffron-control`

### Typed error + wire code

**File `engine/crates/control/src/error.rs`.**

1. Add a `Busy` variant to the crate `Error` enum, placed after `Params`:

   ```rust
   /// A command was rejected because a project load is in flight; the editor drops and retries.
   #[error("engine busy loading project")]
   Busy,
   ```

2. Add a `code()` method on `Error` — the single source of truth for the kebab-case wire code carried
   alongside the human message. Cover **every** existing variant (match exhaustively so a future variant
   forces a code):

   ```rust
   impl Error {
       /// The machine-readable code that crosses the wire alongside the human message.
       #[must_use]
       pub fn code(&self) -> &'static str {
           match self {
               Error::Socket(_) => "socket",
               Error::PathTooLong(_) => "path-too-long",
               Error::Command(_) => "command",
               Error::Params(_) => "params",
               Error::Busy => "busy-loading",
           }
       }
   }
   ```

   > Confirm the exact existing variant set when implementing (the names above mirror the current
   > `error.rs`); the rule is one arm per variant, no `_ =>` catch-all, so adding a variant later is a
   > compile error until it gets a code.

### Dispatch gate + allow-list

**File `engine/crates/control/src/registry.rs`.**

3. In `CommandRegistry::dispatch`, after the `help` special-case and the unknown-command early return,
   and **before** params parsing / `row.run`, insert the `Loading` gate:

   ```rust
   if ctx.scene_edit.project_phase == saffron_sceneedit::ProjectPhase::Loading
       && !is_loading_safe_command(command)
   {
       let error = Error::Busy;
       return json!({ "id": id, "ok": false, "error": error.to_string(), "code": error.code() });
   }
   ```

4. Change the existing `Err(error)` arm of the handler-run match to emit `code` uniformly for **all**
   errors (NO-LEGACY: one error shape, not two):

   ```rust
   Err(error) => json!({ "id": id, "ok": false, "error": error.to_string(), "code": error.code() }),
   ```

5. Give the unknown-command early return the same shape for uniformity — add `"code": "command"` to its
   `ok:false` envelope so every failure envelope carries a `code`.

6. Add the allow-list as a free function sibling to the existing `is_read_only_command`, with a
   doc-comment mirroring that precedent. Phase-1 set only — Phase 2 adds `cancel-load`, Phase 3 adds
   `project-status`:

   ```rust
   /// Commands that stay serviceable while a project load is in flight. Deliberately tiny: during a
   /// load the scene/catalog are mid-swap, so even reads (`inspect`, `list-entities`) could observe a
   /// torn state — they get the busy error and the editor retries them once `Ready`.
   #[must_use]
   pub fn is_loading_safe_command(name: &str) -> bool {
       matches!(name, "ping" | "help" | "get-project" | "quit")
   }
   ```

Do **not** fold this gate into `require_project_loaded`. They are orthogonal axes:

- `Loading` is **global and mechanical** — the whole engine is mid-swap; almost everything is rejected.
- `Unloaded` is **per-command and semantic** — many commands run fine with no project open (`ping`,
  render toggles, camera) and are only blocked by the handlers that genuinely need a project.

They must stay separate functions with separate call sites.

### Migrate every `project_loaded` reader (NO-LEGACY — no bool survives)

**File `engine/crates/control/src/commands_asset.rs`.**

7. `require_project_loaded` — change its body to guard on the phase:
   `if !ctx.scene_edit.project_ready() { return Err(Error::command("no project loaded")); }`.

8. `apply_project_info` (and/or `apply_loaded_project` where the "project is now open" flag is set) —
   where it previously set `project_loaded = true`, set
   `ctx.scene_edit.project_phase = ProjectPhase::Ready`.

9. `current_project_info` — the `ProjectInfoDto.loaded` field is derived from
   `ctx.scene_edit.project_ready()`. Phase 1 keeps the DTO wire shape unchanged (`loaded: bool`); only
   the source of the boolean moves from the field to the derived read.

10. `load_project_into`, the `open-project` handler, and the `new-project` handler — wrap the synchronous
    body so it sets `project_phase = Loading` at entry, and `Ready` (via `apply_project_info` /
    `apply_loaded_project`) on success or `Failed` on the error map. In Phase 1 this is atomic within the
    single-threaded handler (the phase is `Loading` only for the duration of the synchronous body, which
    nothing else observes). Phase 2 hollows these handlers out to seed the loader instead.

11. Any inline project check in this file (e.g. the `create-script` inline check) — swap
    `ctx.scene_edit.project_loaded` for `ctx.scene_edit.project_ready()`.

**File `engine/crates/control/src/commands_scene.rs`.**

12. Swap the inline `project_loaded` check(s) here for `ctx.scene_edit.project_ready()`.

**Sweep.** After the above, `grep -rn project_loaded engine/` must return nothing. Any remaining reader
(in any crate) is migrated the same way in this change.

## Wire contract

**File `schemas/control/envelope.schema.json`.**

Add an optional `code` property alongside the existing `error` field, documented as present on every
`ok:false` reply:

```json
"code": {
  "type": "string",
  "description": "machine-readable error code (present on ok:false)"
}
```

Keep it optional (not `required`) — `ok:true` replies do not carry it. If the schema uses
`additionalProperties: false`, adding the property is mandatory or the contract test rejects the new
field.

Regenerate the committed protocol artifacts so `tools/check-control-schema` stays green:
`cargo run -p xtask -- gen-protocol` (from inside the `saffron-build` toolbox). The OpenRPC / manifest
are DTO-driven and unchanged here, but run the gate to confirm the envelope edit is accepted.

## Editor control client

**Files `editor/src/control/client.ts` and `editor/src-tauri/src/lib.rs`.**

The Tauri bridge turns an `ok:false` reply into a rejected promise (the control passthrough in
`src-tauri/src/lib.rs`). Thread the new `code` through the rejection so the typed TS layer can match it:

1. Where the bridge reply is unwrapped, build a typed error carrier instead of a bare `Error`:

   ```ts
   class ControlError extends Error {
     code?: string;
   }
   ```

   Populate `code` from the envelope `code` field on the `ok:false` path.

2. Add a helper next to the client:

   ```ts
   export function isBusyLoading(err: unknown): boolean {
     return (err as ControlError | undefined)?.code === "busy-loading";
   }
   ```

3. In the **background poll lanes** in `editor/src/state/store.ts` (the reconcile / metrics lanes — the
   fire-every-tick ones, not user-triggered actions), each existing `catch` on a control call drops
   silently when `isBusyLoading(err)` (no `notifyError`). User-triggered actions keep surfacing
   non-busy failures per the editor error rule.

> Phase 1 lands only the `code` passthrough + the `isBusyLoading` drop on background lanes. The full
> loading-view wiring (`useProjectLoadPoll`, the modal split, the `projectLoad` store slice) is Phase 4.

## `sa` CLI + docs

- The phase already surfaces through the existing `sa get-project` (its `loaded` field, now derived from
  `project_ready()`). `sa` reaches any registered command via `external_subcommand`, so no new `sa`
  command is needed in Phase 1. `project-status` / `cancel-load` arrive in Phase 3.
- Docs: defer the concept page to Phase 3, which adds the pollable status surface worth documenting.
  Phase 1 is internal plumbing.

## Ordering / dependencies

Depends on nothing. Prerequisite for Phase 2 (which sets `Loading` for a real duration and makes the
gate fire) and Phase 3 (which adds `cancel-load` / `project-status` to the allow-list and over the wire).
The gate is dormant until Phase 2.

## Verification

Run the milestone gate and confirm each item:

1. **Build + lint clean.** `just engine` then `just prepare-for-commit` — clippy `-D warnings` passes.
   `grep -rn project_loaded engine/ editor/` returns nothing (the bool is gone tree-wide).

2. **Unit test in the `registry.rs` tests module.** Construct an `EngineContext` (or the smallest test
   harness the module already uses) with `ctx.scene_edit.project_phase = ProjectPhase::Loading`, then:
   - `dispatch` of a non-allow-listed command (e.g. `add-entity`) returns an envelope with
     `ok == false` and `code == "busy-loading"`.
   - `dispatch` of `ping` and `get-project` returns `ok == true` (allow-listed).
   - A `Params`/`Command` error still returns its own `code` (`"params"` / `"command"`), proving the
     `Err` arm emits `code` uniformly.

3. **Contract test.** `tools/check-control-schema` passes — the envelope `code` property is accepted and
   validates against a real `ok:false` reply.

4. **Headless smoke.** `just run-engine-headless 3` boots clean, and `sa get-project` still reports the
   bootstrapped project as `loaded: true` (confirming the `project_ready()` derivation is correct on the
   synchronous path).

## Cross-cutting reminders

- **Milestone gate at this phase boundary:** `just engine` + `just prepare-for-commit`; regenerate
  protocol (the envelope changed); run `just e2e` if the suite exercises error envelopes.
- **Do not commit.** Leave changes unstaged and report; the user stages and commits (git is read-only by
  default).
