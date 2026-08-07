import { useEffect, useRef } from "react";
import { client } from "../control/client";
import { rememberProject } from "../lib/recentProjects";
import { useEditorStore } from "../state/store";

/// The project-load poll cadence (~10 Hz). `project-status` is a tiny allow-listed query, so this is
/// cheap even during a load.
const PROJECT_POLL_MS = 100;

/// How long to hold the completed loading view (bar at 100%, "Ready") before dismissing it, so the
/// fill visibly finishes instead of blinking away. Comfortably exceeds the bar's 200 ms fill
/// transition, leaving a short beat to read the completion.
const PROJECT_LOAD_FINALIZE_MS = 500;

const delay = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/// The ~10 Hz `project-status` poll that drives the startup modal's loading view + owns load
/// completion. Mounted ONCE in `App.tsx` so it covers modal-, menu-, AND bootstrap-initiated loads
/// (the modal unmounts when closed; bootstrap/env loads never open it).
///
/// Modeled on the store's always-live watchdog lane (not the `readyForSync` fast lane): a load runs
/// while the viewport may not be `ready` yet — an env/scratch bootstrap load lands before
/// `phase === "ready"`, and a reload runs while the engine stays `ready`. `project-status` is
/// allow-listed during `Loading`, so the poll is always answered.
export function useProjectLoadPoll(): void {
  const inFlight = useRef(false);
  const lastVersion = useRef(-1);
  // Guards the one-shot Ready completion so it fires once per load, not on every subsequent Ready
  // poll (the engine keeps reporting `ready` until the next load).
  const completing = useRef(false);
  // The status version the user cancelled at. The engine keeps reporting that same `ready` version
  // (it does not unload), so without this the `project === null` completion clause would re-adopt
  // the cancelled load on the next tick. A fresh load bumps the version, clearing the suppression.
  const cancelledVersion = useRef(-1);

  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const engineMayBeAlive = (): boolean => {
      const phase = useEditorStore.getState().engineStatus.phase;
      return phase !== "idle" && phase !== "error";
    };

    const wantsCancel = (): boolean => useEditorStore.getState().projectLoadCancelling;

    /// The load-completion side-effects: pull the full project
    /// info, reset the scene, remember the recent, and close the modal.
    const complete = async (): Promise<void> => {
      const project = await client.getProject();
      const store = useEditorStore.getState();
      store.setProject(project);
      store.resetSceneState();
      await rememberProject(project);
      store.setLauncherOpen(false);
      store.setProjectLoad({ phase: "idle", version: lastVersion.current, finalizing: false });
    };

    /// The user cancelled: drop the (possibly already-`ready`) load without adopting it. Return to
    /// the picker when there is no project yet (the startup case), else the launcher dissolves back
    /// to the editor (a menu-initiated reload/switch — the prior project stays). `cancelledVersion`
    /// blocks the poll from re-completing this same `ready` version on the next tick.
    const abortToPicker = (version: number): void => {
      const store = useEditorStore.getState();
      lastVersion.current = version;
      cancelledVersion.current = version;
      store.setProjectLoadCancelling(false);
      store.setProjectLoad({ phase: "idle", version, finalizing: false });
      store.setLauncherOpen(store.project === null);
    };

    const tick = async (): Promise<void> => {
      if (!stopped && !inFlight.current && engineMayBeAlive()) {
        inFlight.current = true;
        try {
          const dto = await client.projectStatus();
          if (!stopped) {
            const store = useEditorStore.getState();
            const localPhase = store.projectLoad.phase;
            if (dto.phase === "ready") {
              // Complete when a load was in flight locally, OR when the engine reports a loaded
              // project the editor has not recorded yet (a very fast bootstrap load can finish
              // between App's initial getProject and the first poll). Once `complete()` sets the
              // project, both conditions go false, so it fires exactly once per load.
              const needsComplete =
                (localPhase === "loading" || store.project === null) &&
                dto.version !== cancelledVersion.current;
              if (needsComplete && !completing.current) {
                completing.current = true;
                lastVersion.current = dto.version;
                try {
                  // When a loading view is up, snap the bar to 100% and let it visibly fill + fade
                  // before dismissing — the completion reads as finished, not a blink. A silent
                  // fast bootstrap (no modal shown) skips the hold; nothing is on screen to animate.
                  if (localPhase === "loading" && !wantsCancel()) {
                    const total = dto.total > 0 ? dto.total : 1;
                    // Fill the bar to 100% and mark finalizing: the loading view keeps the last
                    // status text but fades it out (no "Ready" swap), then the modal dismisses.
                    useEditorStore.getState().setProjectLoad({
                      phase: "loading",
                      done: total,
                      total,
                      currentItem: "",
                      error: undefined,
                      version: dto.version,
                      finalizing: true,
                    });
                    await delay(PROJECT_LOAD_FINALIZE_MS);
                  }
                  // A cancel may have landed before or during the hold (a fast load can hit `ready`
                  // before the click). Honor it: abort instead of adopting the project.
                  if (stopped) {
                    // effect torn down mid-hold; drop it
                  } else if (wantsCancel()) {
                    abortToPicker(dto.version);
                  } else {
                    await complete();
                  }
                } finally {
                  completing.current = false;
                }
              }
            } else if (dto.version !== lastVersion.current) {
              lastVersion.current = dto.version;
              if (dto.phase === "failed") {
                useEditorStore.getState().setProjectLoad({
                  phase: "error",
                  stage: dto.stage,
                  error: dto.error || "Project load failed.",
                  version: dto.version,
                });
              } else if (dto.phase === "loading") {
                useEditorStore.getState().setProjectLoad({
                  phase: "loading",
                  stage: dto.stage,
                  done: dto.done,
                  total: dto.total,
                  label: dto.label,
                  currentItem: dto.currentItem,
                  error: undefined,
                  version: dto.version,
                  finalizing: false,
                });
              } else if (localPhase === "loading" || localPhase === "error") {
                // Unloaded after a cancel / back-out: settle to idle and return to the picker if
                // there is no project yet (the startup case).
                const store2 = useEditorStore.getState();
                store2.setProjectLoadCancelling(false);
                store2.setProjectLoad({ phase: "idle", version: dto.version });
                if (store2.project === null) {
                  store2.setLauncherOpen(true);
                }
              }
            }
          }
        } catch {
          // Transient engine-busy / socket hiccup; the next tick recovers.
        } finally {
          inFlight.current = false;
        }
      }
      if (!stopped) {
        timer = setTimeout(() => void tick(), PROJECT_POLL_MS);
      }
    };

    timer = setTimeout(() => void tick(), PROJECT_POLL_MS);
    return () => {
      stopped = true;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }, []);
}
