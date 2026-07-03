# Phase 4 — Editor loading screen (ProjectStartupModal → progress view)

**Status:** NOT STARTED

## Goal

Turn `editor/src/app/ProjectStartupModal.tsx` from a picker that **awaits** the open/new promise and
closes the instant it resolves into a two-mode surface:

- The **picker view** kicks off a *non-blocking* load (seed the request, return immediately) and hands
  off to
- the **loading view** — a progress bar + a single engine-supplied stage line — driven by polling the
  Phase 3 `project-status` command. On the DTO reaching `Ready` it runs the "project loaded"
  side-effects and closes; on `Failed` it shows an inline error with **Retry** / **Back to picker**;
  while `Loading` it is non-dismissable and offers **Cancel** (which calls `cancel-load`).

The same non-blocking seam replaces the duplicate blocking open/reload path in
`editor/src/app/ProjectMenu.tsx`. It stays a **distinct axis** from the engine-process
`LoadingOverlay` (see the relation section). NO-LEGACY: the inline `complete()` /
`handleProjectLoaded` close flow and the `ProjectMenu` `loadProjectPath` / `reloadProject` bodies are
**replaced** by one shared `startProjectLoad` thunk + the `useProjectLoadPoll` hook — never kept
alongside.

## Dependencies

- **Phase 3** — the `project-status` / `cancel-load` commands and the regenerated `@saffron/protocol`
  types (`ProjectStatusDto`, `ProjectPhaseDto`, `BootStageDto`). `openProject`/`newProject`/
  `reloadProject` now return the initial `ProjectStatusDto` snapshot (a kick-off ack), not
  `ProjectInfoDto`.
- **Phase 1** — the `isBusyLoading(err)` helper and the `code`-carrying `ControlError` in
  `editor/src/control/client.ts`.

This is the terminal phase. Do not start it until Phase 3's protocol regen is committed, or
`@saffron/protocol` will not carry the new DTO/command types.

## Cross-cutting rules (per the blueprint)

- **NO-LEGACY grep gate for this phase:** after the work, `grep -rn "complete(" editor/src/app/ProjectStartupModal.tsx`,
  `grep -rn "handleProjectLoaded" editor/src/`, and `grep -rn "loadProjectPath\|await client.openProject\|await client.reloadProject" editor/src/app/ProjectMenu.tsx`
  all return nothing — there is exactly one bring-up seam (`startProjectLoad`) and one completion path
  (the poll hook).
- Keep engine-process lifecycle (`engineStatus.phase` / `LoadingOverlay`) and project load
  (`projectLoad` / the modal loading view) as **distinct axes**. Do not merge them.
- Milestone gate: `cd editor && bun run check` (regenerates `@saffron/protocol` + tsc) and
  `bun run lint` must be clean before this phase is done.

---

## Grounded starting points (file : symbol)

| What | File | Symbol |
|------|------|--------|
| Modal, picker JSX, `complete()`/`busy`/`status` | `editor/src/app/ProjectStartupModal.tsx` | `ProjectStartupModal`, `complete`, `createProject`, `openProjectPath`, `validProjectName`, `viewportHidden` effect |
| Modal mount + `handleProjectLoaded` + picker-open sync | `editor/src/app/App.tsx` | `syncProject` (`phase === "ready"` effect), `handleProjectLoaded`, `<ProjectStartupModal … />` |
| Duplicate blocking open/reload | `editor/src/app/ProjectMenu.tsx` | `loadProjectPath`, `reloadProject` |
| Engine-process overlay (keep distinct) | `editor/src/app/LoadingOverlay.tsx` | `LoadingOverlay` (error `<pre>` + `size="sm" variant="outline"` Retry/Restart) |
| Store: engine axis + project + scene reset | `editor/src/state/store.ts` | `EnginePhase`, `engineStatus`, `setProject`, `resetSceneState`, `setProjectModalOpen` |
| Poll lanes prior art | `editor/src/state/store.ts` | `startReconcile`, `engineMayBeAlive` (watchdog lane), `readyForSync` (fast lane), `fastInFlight` guard |
| Control wrappers | `editor/src/control/client.ts` | `call<C>()`, `getProject`, `newProject`, `openProject`, `reloadProject` |
| Recents helper | `editor/src/lib/recentProjects.ts` | `rememberProject` |
| shadcn primitives present (Progress is NOT) | `editor/src/components/ui/` | `button.tsx`, `dialog.tsx`, `input.tsx`, `label.tsx` — no `progress.tsx` |

---

## Step 1 — Store slice `projectLoad` (`editor/src/state/store.ts`)

Add a slice **distinct** from `engineStatus` / `EnginePhase` (`store.ts` `EnginePhase` at the
`type EnginePhase = …` line). Project load is orthogonal: a reload happens while the engine stays
`ready`, and a bootstrap (env/scratch) load happens before `phase === "ready"`.

Types (place near the other UI-state interfaces):

```ts
export type ProjectLoadPhase = "idle" | "loading" | "ready" | "error";

export interface ProjectLoadRequest {
  kind: "open" | "new" | "reload";
  path?: string;        // open
  name?: string;        // new (validated slug)
  displayName?: string; // new
}

export interface ProjectLoadState {
  phase: ProjectLoadPhase;
  stage: string;        // BootStageDto value, e.g. "assets" / "scene" / "skybox"
  done: number;
  total: number;        // 0 == indeterminate → spinner, not a filled bar
  label: string;        // engine-supplied human line ("Loading assets 12/40")
  currentItem: string;
  error?: string;
  version: number;      // monotonic dedup key from the DTO
  request?: ProjectLoadRequest; // stashed so Retry can replay
}
```

State field + initial value (in the store's initial object, beside `engineStatus`):

```ts
projectLoad: {
  phase: "idle", stage: "", done: 0, total: 0, label: "",
  currentItem: "", version: 0,
} as ProjectLoadState,
```

Actions on the store interface + implementation:

- `setProjectLoad(patch: Partial<ProjectLoadState>): void` — **identity-stable** like `setGizmo`
  (`store.ts` `setGizmo`): merge, and `return {}` (no re-render) when nothing changed. Dedup on
  `version`: if `patch.version !== undefined && patch.version === s.projectLoad.version` and the patch
  carries no phase/error change, `return {}`. This keeps the ~10 Hz poll from re-rendering subscribers
  every tick when the snapshot is unchanged.
- `startProjectLoad(request: ProjectLoadRequest): Promise<void>` — a thunk (a plain method that closes
  over `get`/`set`, mirroring the existing thunk actions in the store):
  1. `set({ projectLoad: { ...idle-ish, phase: "loading", version: <current>, request } })` — set
     `phase: "loading"` and stash `request` immediately so the combined Dialog `open` (Step 4) shows the
     loading view without waiting for the first poll.
  2. Fire the engine kick and **return without awaiting completion**:
     `open` → `client.openProject(request.path!)`,
     `new` → `client.newProject(request.name!, request.displayName ?? "")`,
     `reload` → `client.reloadProject()`.
     These now return the initial `ProjectStatusDto` snapshot (Phase 3); optionally seed `projectLoad`
     from it, but the poll is the source of truth. On a rejected kick that is **not** busy-loading
     (`isBusyLoading` false), `setProjectLoad({ phase: "error", error: errorText(err) })`. Completion and
     stage progress are driven by `useProjectLoadPoll`, never by awaiting the kick.

Keep `setProject`, `resetSceneState`, `setProjectModalOpen` exactly as they are (`store.ts`
`setProject`, `resetSceneState`, `setProjectModalOpen`) — the completion path in Step 3 calls them.

## Step 2 — Client wrappers (`editor/src/control/client.ts`)

`openProject` / `newProject` / `reloadProject` (`client.ts` `openProject`, `newProject`,
`reloadProject`) become **kick-off** calls. Their result type changes from `ProjectInfo` to
`ProjectStatus` (the regenerated alias for `ProjectStatusDto`) per Phase 3 — this is the NO-LEGACY
rebuild, not an additive overload. Update the signatures and let tsc flush every stale caller; the
loader thunk + poll consume the new shape.

Add two wrappers next to `getProject` (`client.ts` `getProject`), using the same `call<C>()` seam:

```ts
projectStatus(): Promise<ProjectStatus> {
  return call("project-status");
},
cancelLoad(): Promise<ProjectStatus> {
  return call("cancel-load");
},
```

Types (`ProjectStatus`, `ProjectPhase`, `BootStage` aliases) come from the regenerated
`@saffron/protocol` re-exported through `client.ts` — **never** hand-edit `editor/src/protocol/sa-types.ts`.

## Step 3 — Poll hook `editor/src/app/useProjectLoadPoll.ts` (new)

Model it on the **always-live watchdog lane** (`store.ts` `engineMayBeAlive`), **not** the
`readyForSync()` fast lane (`store.ts` `readyForSync`): a load runs while the viewport may not be
`ready`, and env/scratch bootstrap loads land before `phase === "ready"`. `project-status` is
allow-listed during `Loading` (Phase 3), so the poll is always answered.

A `useEffect` that starts a self-rescheduling `setTimeout` at ~10 Hz (a `PROJECT_POLL_MS = 100`
constant), active while `engineMayBeAlive()`:

- One-in-flight guard (a `inFlight` ref, mirroring `fastInFlight`). Each tick, if not in flight and the
  engine may be alive, `await client.projectStatus()`.
- **Dedup on `version`**: keep a `lastVersion` ref; skip the state write when the DTO `version` matches
  and the phase is unchanged (`setProjectLoad`'s dedup also guards this, but the ref avoids the call).
- Map `dto.phase` (`ProjectPhaseDto`, kebab) → `ProjectLoadPhase`: `Loading → "loading"`,
  `Failed → "error"`; `Ready`/`Unloaded` drive the terminal handling below rather than a plain phase
  write. Write `setProjectLoad({ phase, stage: dto.stage, done: dto.done, total: dto.total, label:
  dto.label, currentItem: dto.currentItem, error: dto.error || undefined, version: dto.version })`.
- **On `Ready`** — run the completion side-effects that used to live in `complete()`
  (`ProjectStartupModal.tsx` `complete`):
  1. `const project = await client.getProject();` (fresh `ProjectInfo` — the status DTO carries
     name/path but the editor state wants the full project info).
  2. `store.setProject(project);`
  3. `store.resetSceneState();`
  4. `await rememberProject(project);` (`editor/src/lib/recentProjects.ts` `rememberProject`).
  5. `store.setProjectModalOpen(false);`
  6. `store.setProjectLoad({ phase: "idle" });`
  Guard this so it fires once per completed load (e.g. only when the previous local phase was
  `"loading"`), not on every subsequent `Ready` poll.
- **On `Failed`** — `setProjectLoad({ phase: "error", error: dto.error })` and stay open (the loading
  view shows Retry / Back).
- **On any interleaved `isBusyLoading(err)`** (Phase 1 helper) — ignore silently; the next tick
  recovers (same discipline as the metrics/watchdog `catch` blocks in `store.ts`).
- On teardown clear the timer and set the stop flag (mirror `startReconcile`'s cleanup).

**Mount once** in `App.tsx` (call `useProjectLoadPoll()` in the top-level component body, alongside the
existing effects) so it covers modal-, menu-, AND bootstrap-initiated loads — not inside the modal
(the modal unmounts when closed, and bootstrap loads never open it).

## Step 4 — Modal transform (`editor/src/app/ProjectStartupModal.tsx`)

Keep the single `Dialog` shell (preserving the `viewportHidden` effect and the `DialogContent`
wiring), and swap the **body** on `projectLoad.phase`. Subscribe to narrow primitives so a progress
tick never re-renders the picker:

```ts
const loadPhase   = useEditorStore((s) => s.projectLoad.phase);
const loadStage   = useEditorStore((s) => s.projectLoad.stage);
const loadLabel   = useEditorStore((s) => s.projectLoad.label);
const loadDone    = useEditorStore((s) => s.projectLoad.done);
const loadTotal   = useEditorStore((s) => s.projectLoad.total);
const loadError   = useEditorStore((s) => s.projectLoad.error);
const startProjectLoad = useEditorStore((s) => s.startProjectLoad);
```

### 4a — Combined `open` + `viewportHidden`

The Dialog is open when the picker is up **or** a load is in flight (so a bootstrap load — which never
opens the picker — still shows the loading view):

```ts
const dialogOpen = modalOpen || loadPhase === "loading" || loadPhase === "error";
```

Drive `<Dialog open={dialogOpen}>`, and key the `viewportHidden` effect on `dialogOpen` (not
`modalOpen`) so the viewport stays parked across picker → loading → close, and only unparks on true
close (`ProjectStartupModal.tsx` `viewportHidden` effect):

```ts
useEffect(() => {
  setViewportHidden(dialogOpen);
  return () => setViewportHidden(false);
}, [dialogOpen, setViewportHidden]);
```

### 4b — Picker view `<ProjectPickerView>`

Extract the current picker JSX (recent list + create/open sections + footer) into a
`ProjectPickerView` subcomponent (same file). Its handlers call the shared thunk instead of the local
async bodies:

- Recent entry / Open Folder / Open project.json → `startProjectLoad({ kind: "open", path })`.
- Create Project → `startProjectLoad({ kind: "new", name, displayName: displayName.trim() })`.

Delete the local `openProjectPath` / `createProject` async bodies and the `complete()` function
(NO-LEGACY — completion now lives only in the poll hook). Drop the local `busy` state **for the load
path** — "is a load running" is now `loadPhase === "loading"`. Keep the local `status` state **only**
for pre-flight validation (`nameError`, "Enter a valid project name" from `validProjectName`); disable
inputs while `loadPhase === "loading"`.

### 4c — Loading view `<ProjectLoadingView>`

A centered column: a shadcn `Progress` bar + one status line + a Cancel button.

- Bar value: `total > 0 ? Math.round((done / total) * 100) : undefined`. When `total === 0` the stage is
  indeterminate → show a `Loader2` spinner (`lucide-react`, as in `LoadingOverlay.tsx`) instead of / above
  a determinate bar.
- Status text: `label` verbatim (the engine supplies "Loading assets 12/40", "Scanning assets",
  "Loading scene", etc.). Optionally a dimmer `currentItem` second line.
- **Add `editor/src/components/ui/progress.tsx`** — the standard shadcn Progress primitive over
  `@radix-ui/react-progress` (none exists in `editor/src/components/ui/`). Add the dependency to
  `editor/package.json`, run `bun install`. Semantic tokens only: `bg-muted` track, `bg-primary` fill,
  `text-foreground` / `text-muted-foreground` for the lines — match the tokens used in
  `LoadingOverlay.tsx`.
- **Cancel button** — `size="sm" variant="outline"` calling `client.cancelLoad()`. The poll then
  reports `Unloaded` → the hook resolves it to `idle`; if the picker was the origin (`modalOpen`
  true) it returns to `<ProjectPickerView>`, and if it was a bootstrap/menu load it closes.

### 4d — Error view

On `loadPhase === "error"`, mirror `LoadingOverlay.tsx`'s destructive affordance: a destructive-colored
title, the error string in a `<pre>` block (`text-destructive` title, `bg-card` + `font-mono text-[11px]`
`<pre>`), and two `size="sm" variant="outline"` buttons:

- **Retry** → `startProjectLoad(projectLoad.request)` (replay the stashed request; guard for
  `request` present).
- **Back to picker** → `setProjectLoad({ phase: "idle" })`, revealing `<ProjectPickerView>` (only
  meaningful when `modalOpen`; when it was a bootstrap load, "Back" should route to opening the picker
  via `setProjectModalOpen(true)` so the user can pick another project).

### 4e — Non-dismissable while loading

While `loadPhase === "loading"` the Dialog must be **non-dismissable** regardless of the existing
`dismissable = project !== null` logic (`ProjectStartupModal.tsx`): pass `showCloseButton={false}` and
no-op `onOpenChange` (Escape / outside-click do nothing). The only exit is Cancel. When `phase ===
"error"`, keep it non-dismissable too — exit is Retry / Back. When back in the picker
(`modalOpen && loadPhase === "idle"`) restore the existing `dismissable`/close-on-`onOpenChange`
behavior verbatim.

## Step 5 — Fold the duplicate `ProjectMenu` path (`editor/src/app/ProjectMenu.tsx`)

Replace the bodies of `loadProjectPath` (`ProjectMenu.tsx` `loadProjectPath`) and `reloadProject`
(`ProjectMenu.tsx` `reloadProject`) — which currently do their own blocking
open-then-`setProject`-then-`resetSceneState`-then-`notify` — with the shared thunk:

- `loadProjectPath(path)` → `startProjectLoad({ kind: "open", path })` (keep the native-dialog
  selection wrapper `openProject` that calls it).
- `reloadProject()` → `startProjectLoad({ kind: "reload" })`.

The modal opens via the combined `open` condition (Step 4a) as soon as `projectLoad.phase ===
"loading"`, so the menu shows the same loading view. Remove the menu's local success/fail `notify`
toasts **for the load itself** (progress + inline error now cover it). Keep `notifyError` only for
pre-flight failures (e.g. a bad native-dialog selection). Do not touch `saveProject` / `openInVsCode` /
recents refresh.

## Step 6 — App.tsx cleanup (`editor/src/app/App.tsx`)

- Delete `handleProjectLoaded` (`App.tsx` `handleProjectLoaded`) — the poll hook owns completion. The
  `<ProjectStartupModal>` no longer takes an `onProjectLoaded` prop; drop it from the JSX and the
  component's props type.
- Add `useProjectLoadPoll()` to the top-level component body (Step 3).
- Leave `syncProject` (`App.tsx` `syncProject`) intact: it still decides *initial* picker visibility
  via `setProjectModalOpen(!project.loaded && !info.envProject && !info.scratchProject)`. Env/scratch
  projects still bypass the picker; their bootstrap load now shows the loading view through the
  combined Dialog `open` + the always-live poll, then closes on `Ready` — no regression to the bypass.

## Relation to `LoadingOverlay` — keep DISTINCT (decision)

**Do not merge.** They answer different questions on independent axes:

- `LoadingOverlay.tsx` is the engine-**process** overlay: phases `starting` / `attaching` / `ready` /
  `error` from `engineStatus`, driving `client.startEngine` / `quitEngine`. Its header explicitly
  requires it to be an **inline absolutely-positioned sibling** inside the viewport panel (NOT a Radix
  Dialog/portal), because the native window maps only after attach.
- The project loader answers "is the project loaded into the *running* renderer?" — a different axis
  that lives inside the modal `Dialog` (which already parks the viewport). `LoadingOverlay` gates
  first: while the engine is not `ready` there is no viewport and no project load possible; the project
  loading view only ever shows once the engine is up.

They **share visual vocabulary only** (the `Loader2` spinner, the destructive `<pre>` error block, the
`size="sm" variant="outline"` buttons) — factor a tiny presentational helper if it reduces
duplication, but keep the two components and their state axes separate.

## Component breakdown (this file's deliverable, at a glance)

| Piece | Location | Role |
|-------|----------|------|
| `projectLoad` slice + `startProjectLoad` thunk | `editor/src/state/store.ts` | single non-blocking bring-up seam + progress state |
| `projectStatus` / `cancelLoad` wrappers | `editor/src/control/client.ts` | poll + abort |
| `useProjectLoadPoll` | `editor/src/app/useProjectLoadPoll.ts` (new) | ~10 Hz poll, completion side-effects, mounted once in App |
| `progress.tsx` | `editor/src/components/ui/progress.tsx` (new) | shadcn Radix Progress primitive |
| `ProjectPickerView` / `ProjectLoadingView` (+ error view) | `editor/src/app/ProjectStartupModal.tsx` | body swap on `projectLoad.phase` |
| menu fold | `editor/src/app/ProjectMenu.tsx` | reuse `startProjectLoad` |
| App cleanup | `editor/src/app/App.tsx` | drop `handleProjectLoaded`, mount poll |

## Verification

- `cd editor && bun run check` (regenerates `@saffron/protocol` via `xtask gen-protocol`, then tsc) and
  `bun run lint` (oxlint) are clean.
- NO-LEGACY grep gate (Step "Cross-cutting rules") returns nothing.
- `just run` a **thick** project: the picker → progress bar advances through the boot stages → closes
  on completion; the viewport stays parked (`viewportHidden`) across picker → loading → close.
  **Cancel** returns to the picker (or closes for a bootstrap load). A deliberately broken
  `project.json` shows the inline error + Retry / Back.
- Menu **Open Recent** / **Reload** now show the same loading view (not a frozen blocking call) — the
  webview never freezes because the load runs off the host main loop (Phase 2) and the editor only
  polls.
- Env/scratch bootstrap: launch with `SAFFRON_PROJECT` set to a thick project — the loading view shows
  and then closes on `Ready` without the picker ever appearing.
- `just e2e` stays green (it drives the control plane directly; this phase's changes are editor-only,
  but re-run to confirm no protocol/type regression from the Phase 3 result-type change).
- Per the editor debugging rule: if a picker → loading → close transition is unclear at runtime, add
  temporary `[vp-dbg]` logging via a Tauri command (webview `console.log` does not reach the
  `tauri dev` terminal), restart `just run` (HMR does not apply store-shape changes), reproduce from
  the real log, then remove it.
