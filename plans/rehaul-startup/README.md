# Rehaul startup — launcher-first editor, session-per-project host

**Status:** IN PROGRESS (phase 1 implemented)

The editor becomes **launcher-first**: starting the app shows an instantly interactive launcher view
(a calm animated dot-grid background with a centered project picker card) rendered by CEF/React
alone — no engine process exists yet. Picking a project **starts a host session**: the shell spawns
`saffron-host` with the project as its boot intent, the launcher shows the engine's own boot
progress over the same background, and the UI crossfades to the live viewport when the scene is up.
Closing a project ends the session and returns to the launcher; a host crash lands back on the
launcher with an error card instead of a dead app. Project handling is simplified in the same
stroke: one **display name** input with a derived on-disk slug and a live "will be created at …"
preview, full **project deletion**, and **hide from recents**.

## Why

- **The shell spawns the host projectless at launch.** `editor/shell/src/main.rs` calls
  `engine::auto_start` before the event loop (opt-out only via `SAFFRON_SHELL_NO_ENGINE`), so a full
  Vulkan renderer sits idle behind the picker doing nothing (`editor/shell/src/engine.rs`,
  `spawn_engine`/`auto_start`).
- **The picker is a dialog floating over a dead viewport.** `ProjectStartupModal.tsx` is a
  locked-open shadcn `Dialog` whose surroundings exist only to fight that layering: a
  `MODAL_EXIT_MS` timer so the scene doesn't pop in behind the fading modal, and off-screen
  viewport parking because the reparented viewport paints over the webview. A launcher *view*
  (not an overlay) deletes the entire problem class.
- **Session-per-project needs no new engine machinery.** The host already boots a project from the
  environment through the exact loader the wire commands use: `bootstrap_project_from_env`
  (`engine/crates/control/src/commands_asset/project.rs`) seeds `ProjectLoadRequest::Open`/`New`
  into the same inbox `ProjectLoader` drains (`engine/crates/control/src/project_loader.rs`), and
  `SAFFRON_PROJECT` already means open-or-create. The only engine gap is a display name for the
  create branch.
- **The registry is already shell-owned, but anemic.** Recents persist in editor appdata
  (`editor/shell/src/settings.rs`, `recent-projects.json`; `list_recent_projects` in
  `commands.rs`) — the right home, since it must exist before any engine runs. It has no hidden
  flag, no delete, and no view of projects created outside the editor.
- **Creation UX asks the user to type the slug.** The modal has two inputs — "Project name" with
  placeholder `a-name-like-this` (validated by the `validProjectName` mirror of
  `valid_project_name`, `engine/crates/assets/src/project.rs`) plus a separate display name. The
  slug is a derived storage detail, not something a person should author.
- **A host crash is a dead editor.** The shell watchdog emits phase events at spawn but nothing
  routes an exited child back to a usable state.

## Design stance

- **The launcher is the app; the engine is a project session.** One toplevel, two views
  (launcher / editor) — never separate windows. The Wayland/AppKit viewport presenter only engages
  while a session publishes frames; the launcher is plain opaque CEF.
- **Session lifecycle lives in the shell.** The frontend asks the shell to start/stop a session;
  the boot intent crosses as environment (`SAFFRON_PROJECT`, plus a display name for creation) into
  the same loader inbox the wire uses. One bring-up path in the engine, one lifecycle owner in the
  shell.
- **Per-layer single path.** The engine *keeps* its project lifecycle commands
  (`load-project`/`new-project`/`open-project`/`reload-project`): the control plane stays
  scriptable (`sa`, the e2e harness cycles projects on one headless host). The *editor* uses
  exactly one flow — session restart — and stops calling those commands entirely. Each layer has
  one way to do it.
- **The registry is editor state.** Known projects, recents order, and the hidden flag live in
  editor appdata. `project.json` is never written to express a machine-local preference.
- **Delete is a shell filesystem operation** with an explicit destructive confirmation, allowed
  only for projects under the userdata root; anything outside it offers "remove from list" only.
- **The dot grid is pure frontend.** One canvas, a few thousand dots displaced by a slowly
  drifting noise field (the swaying-in-wind look), capped at 30 fps, static under
  `prefers-reduced-motion`, unmounted in the editor view. No engine, no external assets.

## Goal

- Cold start to an interactive launcher in well under a second after first CEF paint, with no
  `saffron-host` process on the machine.
- Pick → boot progress over the animated background → crossfade to the live viewport; close →
  launcher; crash → launcher with the exit reason.
- One name field when creating; the slug and its on-disk path are derived and previewed live.
- Projects can be deleted (full on-disk cleanup) and hidden from recents without deletion.
- `ProjectStartupModal.tsx`, its exit-timer and viewport-parking hacks, and the editor's in-place
  project-switch calls are gone; the editor never drives `load-project`/`new-project`/
  `reload-project` over the wire.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-session-lifecycle.md` | Shell-owned host sessions: no spawn at boot; `session_start`/`session_stop` IPC with a project boot intent; exit/crash events; display-name env for the create branch; frontend app-phase state. | — |
| 2 | `phase-2-launcher-view.md` | The launcher view replacing `ProjectStartupModal`: dot-grid canvas, centered picker card, boot-progress and crash cards over the same background, crossfade to the viewport; the dialog and its layering hacks deleted. | 1 |
| 3 | `phase-3-project-management.md` | The project registry grows hidden + known-projects merge; one-input creation with derived slug + live path preview; delete with full cleanup; hide from recents. | 2 |
| 4 | `phase-4-editor-cutover.md` | The editor's project menu routes New/Open/Recent/Reload through session restarts; the frontend's wire-lifecycle calls are deleted; docs updated. | 1–3 |
