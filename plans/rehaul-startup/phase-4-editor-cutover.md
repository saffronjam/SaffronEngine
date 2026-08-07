# Phase 4 — Editor cutover: one flow, menu rerouted, docs

**Status:** NOT STARTED

The editor's remaining in-place project switching is cut over to session restarts and its
wire-lifecycle calls are deleted. After this phase the editor has exactly one project flow —
shell-owned sessions — while the engine's lifecycle commands remain for the control plane's other
callers (`sa`, the e2e harness), which cycle projects on a single headless host by design.

## Current state

- `editor/src/app/ProjectMenu.tsx` drives in-place switches on the live host:
  `startProjectLoad({ kind: "open" | "new" | "reload" })` and recents rows through
  `loadProjectPath`, all landing in `editor/src/state/store/slices/project.ts`, which calls the
  wire commands (`load-project`, `new-project`, `reload-project`) via the control client.
- The engine side (`ProjectLoader`, the `ProjectPhase::Loading` dispatch gate in
  `engine/crates/control/src/registry.rs`, the lifecycle commands in
  `commands_asset/commands_project.rs`) serves three callers: editor, `sa`, e2e
  (`tests/e2e/harness.ts` creates/loads projects per test on one host).
- Editor undo/redo is per-tab, reconstructed from inverse control calls (`editor/AGENTS.md`); an
  in-place project swap must carefully invalidate it, a session restart resets it for free.

## Design

- **Per-layer single path, stated and enforced.** The editor never sends a project lifecycle
  command; it stops and starts sessions. The engine keeps the commands because the control plane
  staying scriptable is a repo rule — a headless host driven by `sa` or the e2e harness has no
  shell to restart it. These are different layers, each with exactly one way to change projects.
- **Menu semantics.** *New Project…* and *Open…* stop the session and land on the launcher with
  the respective card focused. Recents rows restart the session directly with that path. *Reload
  project* (revert to saved) becomes a session restart with the current path — same boot path,
  honest cost, no special case. *Save* / *Save As* are untouched (in-session concerns).
- **Frontend deletions.** The `open`/`new`/`reload` kinds of `startProjectLoad` and the control
  client's `loadProject`/`newProject`/`openProject`/`reloadProject` wrappers are deleted;
  `useProjectLoadPoll` survives as the session boot progress feed. Any store state that existed
  to sequence an in-place swap (mid-swap selection/undo/tab resets) is deleted — session restart
  supersedes it.

## Work

1. Reroute `ProjectMenu.tsx`: New/Open → `session_stop` + launcher card focus; recents →
   `session_start { path }`; Reload → restart with the current project path (confirm-if-dirty
   stays in front of it).
2. Shrink `slices/project.ts` to session boot state; delete the wire-lifecycle client wrappers and
   the in-place swap bookkeeping they fed.
3. Sweep the frontend for the deleted flows: command palette / shortcuts / any panel that invoked
   open-reload paths routes through the same two session calls.
4. **Docs (keep-current):** update the ui-and-editor explanations that describe the startup modal
   and project lifecycle (launcher view, session-per-project, crash recovery), the control-plane
   page's note on who calls the lifecycle commands, and the env-var documentation
   (`SAFFRON_PROJECT_DISPLAY_NAME`); hub `_index.md` rows in the same change.
5. Verify the untouched callers: `just e2e` green (harness still cycles projects over the wire),
   `sa load-project` against a headless host still works, `just schema` unchanged (no wire shape
   was modified).

## Acceptance

- `grep -rn "loadProject\|reloadProject\|newProject\|openProject" editor/src` matches only
  generated protocol types; the editor sends no project lifecycle command.
- Every menu path (New/Open/Recent/Reload/Close) works through sessions, including with unsaved
  changes (dirty confirm precedes the restart).
- Undo history and per-tab state are empty after any project change, with no bespoke reset code
  left doing it.
- `just check` green end to end; planset READMEs flipped to COMPLETED where earned.
