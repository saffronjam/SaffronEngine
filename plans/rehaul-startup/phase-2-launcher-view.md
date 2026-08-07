# Phase 2 — The launcher view

**Status:** COMPLETED

A first-class **launcher view** replaces the startup dialog: a full-bleed animated dot-grid
background with a centered picker card, shown whenever no session is live. Session boot progress
and crash reports render as card variants over the same background, and reaching the loaded scene
crossfades to the live viewport. `ProjectStartupModal` and every hack that existed to float a
dialog over a viewport are deleted in the same change.

## Current state

- `editor/src/app/ProjectStartupModal.tsx` is a locked-open `Dialog` hosting recents, the
  open-by-path flow, the two-field create form, and the load-progress view. It carries
  `MODAL_EXIT_MS` (wait out the dialog fade so the scene doesn't pop in behind it) and parks the
  viewport off-screen while open because the reparented viewport paints over the webview.
- `editor/src/app/useProjectLoadPoll.ts` polls `project-status` and drives
  `useEditorStore.projectLoad` (`BootStage` label, determinate `done/total`, error) — reusable
  as-is for the boot card.
- `App.tsx` mounts the modal beside the full editor UI; there is no view-level split.

## Design

- **Two views, one toplevel.** `App.tsx` branches on `appPhase` (Phase 1): `launcher`/`booting`
  render `<Launcher/>` — an opaque, full-viewport React view (no engine frames underneath, so no
  parking, no exit timers); `editor` renders the existing panel layout. "Close project" in the
  project menu calls `session_stop` and returns to the launcher.
- **Dot grid.** One `<canvas>` component (`editor/src/launcher/DotGrid.tsx`): a regular grid of
  ~2–4k dots, each displaced by a slowly drifting curl-noise field (time-scaled simplex, bundled
  implementation — no external fetch), subtle per-dot size/brightness modulation, capped at
  30 fps, static under `prefers-reduced-motion`, unmounted the moment the editor view mounts. Calm
  is the spec: displacement amplitude a few dot spacings, field drift on the order of tens of
  seconds per screen.
- **Cards over the grid.** One centered card with three variants:
  - **Picker** — recents list, open-by-path (native dialog), create form (moved as-is from the
    modal in this phase; the one-input redesign is Phase 3).
  - **Booting** — project name + the `BootStage` progress from `useProjectLoadPoll`; a cancel
    button that `session_stop`s.
  - **Crashed** — the `session-exited` payload (exit code + last log lines), with "Back" and
    "Retry".
- **Crossfade.** The viewport stays hidden (existing `setViewportHidden`) until `projectLoad`
  reaches `ready` and the first published frame is presented; then the launcher fades out over it
  (a plain opacity transition on the launcher root — the viewport is already live beneath).

## Work

1. `editor/src/launcher/` — `Launcher.tsx` (view + card state machine), `DotGrid.tsx`,
   `PickerCard.tsx`, `BootCard.tsx`, `CrashCard.tsx`; `App.tsx` view branch.
2. Route the picker actions through Phase 1's `session_start`; recents still come from
   `list_recent_projects`.
3. Delete `ProjectStartupModal.tsx`, `MODAL_EXIT_MS`, the viewport-parking effect, and the
   modal-open store plumbing (`setProjectModalOpen` and friends) — the project menu's "Open…"
   path that reopened the modal now stops the session instead (fully rerouted in Phase 4).
4. Crossfade + first-frame gating against the existing viewport reveal seam.

## Acceptance

- Cold start paints an interactive launcher with the grid animating; no host process exists.
- Picking a project keeps the background steady while the boot card streams `BootStage` progress,
  then crossfades into the loaded viewport with no pop-in.
- Killing the host lands on the crash card with the exit code visible; Retry boots the same
  project.
- `grep -rn "ProjectStartupModal\|MODAL_EXIT_MS" editor/src` returns nothing.
- `cd editor && bun run check`, `just typecheck`, `just lint` clean; the dot grid idles below a
  few percent CPU at 30 fps and stops entirely in the editor view.
