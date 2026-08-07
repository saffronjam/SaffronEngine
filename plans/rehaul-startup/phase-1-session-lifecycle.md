# Phase 1 — Shell-owned host sessions

**Status:** COMPLETED

The shell stops spawning the host at launch and instead owns a **project session**: spawn on
demand with the project as boot intent, stop on close, and surface an exit/crash as an event the
frontend can route on. After this phase the startup UI is unchanged (the modal still runs), but it
runs against a session the frontend started — the projectless idle host no longer exists.

## Current state

- `editor/shell/src/main.rs` auto-starts the engine before the event loop via
  `engine::auto_start(&state)`, skipped only by `SAFFRON_SHELL_NO_ENGINE`.
- `editor/shell/src/engine.rs`: `spawn_engine(socket_path)` launches `SAFFRON_ANIMA_BIN` (default
  `engine/target/debug/saffron-host`) with `SAFFRON_CONTROL_SOCK`; `start_engine` wires the
  watchdog phase events; `child_alive` / `teardown` manage the child.
- The host brings a project up from env through `bootstrap_project_from_env`
  (`engine/crates/control/src/commands_asset/project.rs`): `SAFFRON_PROJECT` opens, or creates
  when the value is a `valid_project_name` with no existing `project.json` — but the create branch
  hardcodes `display_name: String::new()`. `SAFFRON_SCRATCH_PROJECT` and the cwd `project.json`
  fallback ride the same inbox.
- With no env set the host waits in `ProjectPhase::Unloaded` for the picker — the state this phase
  eliminates from the editor flow.

## Work

1. **Delete the auto-start.** Remove the `engine::auto_start` call and `SAFFRON_SHELL_NO_ENGINE`
   (a shell with no engine is now the default launch state; delete the flag's uses in scripts and
   docs in the same change). `engine::auto_start` itself is deleted; `start_engine` becomes the
   session entry.
2. **Session IPC.** New shell commands in `editor/shell/src/commands.rs`:
   - `session_start { path?: string, create?: { name, displayName } }` — refuses if a session is
     live; builds the child env (`SAFFRON_PROJECT` = path or slug; for `create`, also the new
     `SAFFRON_PROJECT_DISPLAY_NAME`); fresh per-session control socket path; spawns and arms the
     watchdog.
   - `session_stop` — graceful stop (existing `teardown` semantics), idempotent.
   - `session_status` — `{ running, path?, startedAt? }` for late-mounting UI.
3. **Exit/crash event.** The watchdog emits a `session-exited { code, expected }` JS event when
   the child exits: `expected: true` after a `session_stop`, `false` otherwise. Include the last
   few captured host log lines so the launcher's error card has something concrete to show.
4. **Display name for env create.** `bootstrap_project_from_env` reads
   `SAFFRON_PROJECT_DISPLAY_NAME` into the `NewProjectSpec` create branch (empty stays the
   `default_display_name` derivation). Document both vars where `SAFFRON_PROJECT` is documented.
5. **Frontend session state.** A small store slice: `appPhase: "launcher" | "booting" | "editor"`,
   driven by `session_start`/`session-exited` and the existing `useProjectLoadPoll` reaching
   `ready`. The control client reconnects per session (new socket each start). In this phase the
   existing modal simply gains "start session on pick" instead of "load-project on the idle host"
   — visual changes wait for Phase 2.

## Acceptance

- Launching the shell creates no `saffron-host` process; `pgrep saffron-host` is empty until a
  project is picked.
- Start → stop → start cycles leak nothing: the child exits, the socket path is unlinked, a new
  session gets a new socket, and the control client reconnects.
- `kill -9` on the host mid-session produces `session-exited { expected: false }` and the frontend
  leaves the editor view instead of freezing.
- `just e2e` and headless `SAFFRON_PROJECT` boots are untouched (the env contract only gained an
  optional variable).
- Milestone gate: `just engine` + `just prepare-for-commit` clean; shell `commands.rs` unit tests
  cover `session_start` arg validation and the double-start refusal.
