import { client, isBusyLoading } from "../../../control/client";
import { errorText } from "../../../lib/flash";
import { loadExpanded } from "../persistence";
import { invalidateThumbnails } from "../thumbnails";
import { SCENE_TAB } from "./tabs";
import type { GetEditorState, ProjectSlice, SetEditorState } from "../types";

export function createProjectSlice(set: SetEditorState, get: GetEditorState): ProjectSlice {
  return {
    project: null,
    engineStatus: { running: false, phase: "idle" },
    projectLoad: {
      phase: "idle",
      stage: "",
      done: 0,
      total: 0,
      label: "",
      currentItem: "",
      version: 0,
      finalizing: false,
    },
    projectLoadCancelling: false,
    sessionCrash: null,

    setProject: (project) =>
      set({
        project,
        expandedIds: project?.path ? loadExpanded(project.path) : new Set<string>(),
      }),
    // Identity-stable: a same-snapshot patch from the poll returns {} so no subscriber re-renders.
    setProjectLoad: (patch) =>
      set((s) => {
        const next = { ...s.projectLoad, ...patch };
        const unchanged =
          next.phase === s.projectLoad.phase &&
          next.stage === s.projectLoad.stage &&
          next.done === s.projectLoad.done &&
          next.total === s.projectLoad.total &&
          next.label === s.projectLoad.label &&
          next.currentItem === s.projectLoad.currentItem &&
          next.error === s.projectLoad.error &&
          next.version === s.projectLoad.version &&
          next.request === s.projectLoad.request &&
          next.finalizing === s.projectLoad.finalizing;
        return unchanged ? {} : { projectLoad: next };
      }),
    startProjectLoad: async (request) => {
      // Flip to loading and stash the request before the first poll, so the modal shows the loading
      // view immediately. Progress and completion belong to useProjectLoadPoll, never awaited here.
      get().setProjectLoadCancelling(false);
      get().setProjectLoad({
        phase: "loading",
        stage: "",
        done: 0,
        total: 0,
        label: "",
        currentItem: "",
        error: undefined,
        request,
        finalizing: false,
      });
      try {
        // With no live session the pick is the session's boot intent: the shell spawns the host
        // with the project in its environment and the host loads it itself — no wire call. With a
        // session running (a menu-initiated switch/reload) the lifecycle commands drive the load.
        const { running } = await client.sessionStatus().catch(() => ({ running: false }));
        if (!running) {
          if (request.kind === "open") {
            await client.sessionStart({ path: request.path ?? "" });
          } else if (request.kind === "new") {
            await client.sessionStart({
              create: { name: request.name ?? "", displayName: request.displayName ?? "" },
            });
          } else {
            await client.sessionStart({});
          }
          // The child exists now, so the crash watchdog can engage and the attach probe takes
          // over (`attaching → ready`). Before this the phase stays `idle`: no session, no probe.
          get().setPhase("attaching");
        } else if (request.kind === "open") {
          await client.openProject(request.path ?? "");
        } else if (request.kind === "new") {
          await client.newProject(request.name ?? "", request.displayName ?? "");
        } else {
          await client.reloadProject();
        }
      } catch (err) {
        // A busy-loading rejection means a load is already in flight and the poll drives it.
        if (!isBusyLoading(err)) {
          get().setProjectLoad({ phase: "error", error: errorText(err) });
        }
      }
    },
    setProjectLoadCancelling: (projectLoadCancelling) => set({ projectLoadCancelling }),
    resetSceneState: () => {
      invalidateThumbnails();
      set({
        entities: [],
        selectedId: null,
        expandedIds: new Set<string>(),
        componentsBySelected: null,
        assets: [],
        assetFolders: [],
        selectedAssetIds: new Set<string>(),
        selectedFolderPaths: new Set<string>(),
        assetSelectionAnchor: null,
        assetMarqueeActive: false,
        assetMenuTarget: null,
        viewTabs: [SCENE_TAB],
        activeViewTabId: "scene",
        previousActiveTabId: null,
        tabHistory: { entries: [SCENE_TAB], cursor: 0 },
        environment: null,
        // Force the reconcile poll's version diff to fire on the next tick.
        sceneVersion: -1,
        selectionVersion: -1,
        focusComponent: null,
        // Every captured prior value is stale against the loaded scene.
        historyByTab: {},
        catalogDrag: null,
      });
    },
    setSessionCrash: (sessionCrash) => set({ sessionCrash }),
    setEngineStatus: (patch) => set((s) => ({ engineStatus: { ...s.engineStatus, ...patch } })),
    setPhase: (phase, error) =>
      set((s) => ({
        engineStatus: {
          ...s.engineStatus,
          phase,
          error: phase === "error" ? error : undefined,
          running:
            phase === "ready" || phase === "error" ? s.engineStatus.running : phase !== "idle",
        },
      })),
  };
}
