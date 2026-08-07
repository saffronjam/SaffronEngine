import type { Client } from "../../control/client";
import { routeAlarmToasts } from "../../lib/alarmToasts";
import { resetScriptErrorToasts, routeScriptErrorToasts } from "../../lib/scriptErrorToasts";
import { appendFrameSamples } from "../../lib/frameSeries";
import { isPanelOpen, useEditorStore } from "./editorStore";
import { invalidateThumbnails } from "./thumbnails";
import type { PlayState } from "./types";

/// Frames requested per metrics poll — the engine's full ring, so no frames are missed between
/// polls (the client dedups the overlap by frame index).
const FRAME_HISTORY_SAMPLES = 1000;
/// The cheap state lane, targeting ~20 Hz.
const FAST_RECONCILE_INTERVAL_MS = 50;
const WATCHDOG_INTERVAL_MS = 1000;
/// The metrics lane wakes on this base tick and fetches once `metricsRefreshMs` has elapsed, so a
/// rate or pause change applies within one tick instead of waiting out a long interval.
const METRICS_BASE_TICK_MS = 100;

interface HeavyRefreshRequest {
  selectedId: string | null;
  sceneChanged: boolean;
  selectionChanged: boolean;
  sceneVersion: number;
  previousSceneVersion: number;
}

/// Start the focus-gated reconcile loops. Cheap interactive state runs frequently; heavier
/// scene/entity/inspect refreshes only run when versions change and never block the next tick.
export function startReconcile(client: Client): () => void {
  let stopped = false;
  let fastTimer: ReturnType<typeof setTimeout> | null = null;
  let watchdogTimer: ReturnType<typeof setInterval> | null = null;
  let metricsTimer: ReturnType<typeof setTimeout> | null = null;
  let fastInFlight = false;
  let refreshInFlight = false;
  let watchdogInFlight = false;
  let metricsInFlight = false;
  let lastMetricsFetchAt = 0;
  // Last-Event-ID cursors: they only advance, so a missed poll just catches up.
  let alarmSince = 0;
  let scriptErrorSince = 0;
  let lastPlayStateForScripts: PlayState = "edit";
  // Contact and script-log cursors reset on each fresh play session (the engine rings restart).
  let contactsSince = 0;
  let lastPlayStateForPhysics: PlayState = "edit";
  let scriptLogsSince = 0;
  let lastPlayStateForScriptLogs: PlayState = "edit";
  let pendingRefresh: HeavyRefreshRequest | null = null;

  let knownSceneVersion = -1;
  let knownSelectionVersion = -1;
  let knownSelectedId: string | null = null;
  let knownPlayVersion = -1;
  let knownAnimationVersion = -1;
  let animationInFlight = false;
  // A false→true transition of the scene-view gate forces a heavy refetch: a tick that ran while it
  // was false fetched the hierarchy, dropped it, yet still advanced the version.
  let prevSceneEntitiesLive = true;

  let lastTickAt = 0;
  let emaIntervalMs = 0;

  const schedule = (): void => {
    if (stopped) {
      return;
    }
    fastTimer = setTimeout(tick, FAST_RECONCILE_INTERVAL_MS);
  };

  const readyForSync = (): boolean => {
    const store = useEditorStore.getState();
    return store.engineStatus.phase === "ready" && document.hasFocus();
  };

  /// The crash watchdog covers the starting/attaching window too — the gap between socket-up and
  /// the viewport attach — so a child that binds its socket then exits before attaching is caught.
  const engineMayBeAlive = (): boolean => {
    const phase = useEditorStore.getState().engineStatus.phase;
    return phase !== "idle" && phase !== "error" && document.hasFocus();
  };

  const refreshHeavyState = (request: HeavyRefreshRequest): void => {
    const { selectedId, sceneChanged, selectionChanged, sceneVersion, previousSceneVersion } =
      request;
    if (refreshInFlight || (!sceneChanged && !selectionChanged)) {
      if (refreshInFlight) {
        pendingRefresh = request;
      }
      return;
    }
    refreshInFlight = true;

    void (async () => {
      try {
        if (sceneChanged) {
          if (sceneVersion < previousSceneVersion) {
            // A backwards version step is a different scene loaded under us, so the captured prior
            // values are stale; drop the scene history (graph tabs survive).
            invalidateThumbnails();
            useEditorStore.getState().clearSceneHistory();
          }
          void useEditorStore.getState().refreshAssets();
          client
            .getEnvironment()
            .then((env) => {
              const s = useEditorStore.getState();
              if (!stopped && !s.dragActive && s.sceneEntitiesLive) {
                s.setEnvironment(env);
              }
            })
            .catch(() => {});

          const list = await client.listEntities();
          if (stopped) {
            return;
          }
          // Only write the Scene hierarchy when the scene view is the confirmed active view — never
          // the preview scene's entities, and never mid-switch, so the asset→scene switch shows no
          // stale preview-entity frame.
          const live = useEditorStore.getState();
          if (!live.dragActive && live.sceneEntitiesLive) {
            live.setEntities(list.entities);
          }
        }

        if (selectedId === null) {
          const s = useEditorStore.getState();
          if (!s.dragActive && s.sceneEntitiesLive) {
            s.setComponentsBySelected(null);
          }
          return;
        }

        const inspected = await client.inspect(selectedId);
        if (stopped) {
          return;
        }
        // Freeze the inspector on the authored selection while the preview view is active, so it
        // never flashes the previewed entity's components when the scene tab reappears.
        const s = useEditorStore.getState();
        if (!s.dragActive && s.sceneEntitiesLive) {
          s.setComponentsBySelected(inspected);
        }
      } catch {
        // Engine briefly busy; the next version change re-runs this.
      } finally {
        refreshInFlight = false;
        if (pendingRefresh !== null && !stopped) {
          const next = pendingRefresh;
          pendingRefresh = null;
          refreshHeavyState(next);
        }
      }
    })();
  };

  /// Refresh the selected rig's animation state + clips. `get-animation-state` rejects when the
  /// entity is not an animation player — the "not rigged / nothing playing yet" case, so the slice
  /// clears silently rather than raising a toast.
  const refreshAnimation = (selectedId: string | null): void => {
    if (animationInFlight) {
      return;
    }
    if (selectedId === null) {
      useEditorStore.getState().setAnimationState(null, []);
      return;
    }
    animationInFlight = true;
    void (async () => {
      try {
        const [state, clips] = await Promise.all([
          client.getAnimationState(selectedId).catch(() => null),
          client.listClips().catch(() => ({ clips: [] })),
        ]);
        if (stopped) {
          return;
        }
        useEditorStore.getState().setAnimationState(state, clips.clips);
      } catch {
        // The next animationVersion bump retries.
      } finally {
        animationInFlight = false;
      }
    })();
  };

  /// The perf-telemetry lane, decoupled from the cheap state tick. Alarms drain every tick so the
  /// badge stays live with the panel closed; the heavier reads run only while their panel is open.
  const pollMetrics = async (): Promise<void> => {
    if (stopped || metricsInFlight || !readyForSync()) {
      return;
    }
    metricsInFlight = true;
    try {
      const drained = await client.drainAlarms(alarmSince);
      if (stopped) {
        return;
      }
      if (drained.events.length > 0) {
        useEditorStore.getState().appendAlarmEvents(drained.events);
        routeAlarmToasts(drained.events, performance.now());
      }
      alarmSince = Math.max(alarmSince, drained.highWaterSeq);

      // Contained script errors pause play engine-side; surface the traceback here. Drained while
      // play is active — a pause keeps the state visible until stop.
      const playState = useEditorStore.getState().playState;
      if (playState !== "edit") {
        if (lastPlayStateForScripts === "edit") {
          resetScriptErrorToasts();
        }
        const scriptErrors = await client.drainScriptErrors(scriptErrorSince);
        if (stopped) {
          return;
        }
        if (scriptErrors.events.length > 0) {
          routeScriptErrorToasts(scriptErrors.events);
        }
        scriptErrorSince = Math.max(scriptErrorSince, scriptErrors.highWaterSeq);
      }
      lastPlayStateForScripts = playState;

      const active = await client.listActiveAlarms();
      if (stopped) {
        return;
      }
      useEditorStore.getState().setActiveAlarms(active.alarms);

      // Seeded once; the target-FPS dropdown refreshes it on write.
      if (useEditorStore.getState().perfConfig === null) {
        const config = await client.getPerfConfig();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setPerfConfig(config);
      }

      // Seeded once; the Resolution control refreshes it on write.
      if (useEditorStore.getState().upscale === null) {
        const up = await client.getUpscale();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setUpscale(up.upscale);
      }

      if (isPanelOpen(useEditorStore.getState(), "stats")) {
        const history = await client.frameHistory(FRAME_HISTORY_SAMPLES);
        if (stopped) {
          return;
        }
        appendFrameSamples(history.samples);
        useEditorStore.getState().setFrameHistory(history);
        const stats = useEditorStore.getState().renderStats;
        if (stats && stats.profilerMode !== "off") {
          const passes = await client.passTimings();
          if (stopped) {
            return;
          }
          useEditorStore.getState().setPassTimings(passes);
        }
      }

      // Polled only while the Render panel is open, so an external `sa set-debug-overlays` reflects.
      if (isPanelOpen(useEditorStore.getState(), "render")) {
        const overlays = await client.getDebugOverlays();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setDebugOverlays(overlays);
      }

      // The physics world exists only in play, so a closed panel or Edit adds zero round-trips.
      const physicsPlayState = useEditorStore.getState().playState;
      if (isPanelOpen(useEditorStore.getState(), "physics") && physicsPlayState !== "edit") {
        if (lastPlayStateForPhysics === "edit") {
          contactsSince = 0;
          useEditorStore.getState().clearContacts();
        }
        const ps = await client.physicsState();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setPhysicsState(ps);
        const bodies = await client.physicsBodies();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setPhysicsBodies(bodies.bodies);
        const contactsDrained = await client.drainContacts(contactsSince);
        if (stopped) {
          return;
        }
        useEditorStore
          .getState()
          .appendContactEvents(contactsDrained.events, contactsDrained.overflowed);
        contactsSince = Math.max(contactsSince, contactsDrained.highWaterSeq);
      }
      lastPlayStateForPhysics = physicsPlayState;

      // Scripts run only in play; the buffer resets on each fresh play and is retained after Stop.
      const scriptLogsPlayState = useEditorStore.getState().playState;
      if (isPanelOpen(useEditorStore.getState(), "scriptLogs") && scriptLogsPlayState !== "edit") {
        if (lastPlayStateForScriptLogs === "edit") {
          scriptLogsSince = 0;
          useEditorStore.getState().clearScriptLogs();
        }
        const logsDrained = await client.drainScriptLogs(scriptLogsSince);
        if (stopped) {
          return;
        }
        useEditorStore.getState().appendScriptLogs(logsDrained.events, logsDrained.overflowed);
        scriptLogsSince = Math.max(scriptLogsSince, logsDrained.highWaterSeq);
      }
      lastPlayStateForScriptLogs = scriptLogsPlayState;
    } catch {
      // Engine briefly busy; the next tick recovers.
    } finally {
      metricsInFlight = false;
    }
  };

  const watchdog = (): void => {
    if (stopped || watchdogInFlight || !engineMayBeAlive()) {
      return;
    }
    watchdogInFlight = true;
    void client
      .sessionStatus()
      .then(({ running }) => {
        if (!running && !stopped) {
          useEditorStore.getState().setPhase("error", "Engine process exited.");
        }
      })
      .catch(() => {
        if (!stopped) {
          useEditorStore.getState().setPhase("error", "Engine process exited.");
        }
      })
      .finally(() => {
        watchdogInFlight = false;
      });
  };

  async function tick(): Promise<void> {
    if (stopped || fastInFlight) {
      if (!stopped) {
        schedule();
      }
      return;
    }
    fastInFlight = true;
    try {
      if (!readyForSync()) {
        lastTickAt = 0;
        return;
      }

      const now = performance.now();
      if (lastTickAt !== 0) {
        const interval = now - lastTickAt;
        emaIntervalMs = emaIntervalMs === 0 ? interval : emaIntervalMs * 0.8 + interval * 0.2;
        if (emaIntervalMs > 0) {
          useEditorStore.getState().setPollRateHz(1000 / emaIntervalMs);
        }
      }
      lastTickAt = now;

      const [selection, stats, gizmo] = await Promise.all([
        client.getSelection(),
        client.renderStats(),
        client.getGizmo(),
      ]);
      if (stopped) {
        return;
      }

      const live = useEditorStore.getState();
      if (live.dragActive || live.engineStatus.phase !== "ready") {
        return;
      }

      // A project load stamps the versions to -1 to demand a fresh heavy refetch. Honour it by
      // dropping the cached diff, so the refetch still happens if an earlier tick already recorded
      // the engine's post-load version before the load completion cleared the store.
      if (live.sceneVersion === -1) {
        knownSceneVersion = -1;
      }
      if (live.selectionVersion === -1) {
        knownSelectionVersion = -1;
        knownSelectedId = null;
      }
      if (live.sceneEntitiesLive && !prevSceneEntitiesLive) {
        knownSceneVersion = -1;
        knownSelectionVersion = -1;
        knownSelectedId = null;
      }
      prevSceneEntitiesLive = live.sceneEntitiesLive;

      live.setRenderStats(stats);
      // Reflect the engine's gizmo and play state so an external `sa set-gizmo` / `sa play` shows
      // up here; Topbar clicks write optimistically and the poll confirms.
      live.setGizmo(gizmo);
      if (selection.playVersion !== knownPlayVersion) {
        live.setPlayState(selection.playState as PlayState);
      }

      const nextSelectedId = selection.entity ? selection.entity.id : null;
      const previousSceneVersion = knownSceneVersion;
      const sceneChanged = selection.sceneVersion !== knownSceneVersion;
      const selectionChanged =
        selection.selectionVersion !== knownSelectionVersion || nextSelectedId !== knownSelectedId;

      live.setSelectionVersion(selection.selectionVersion);
      live.setSceneVersion(selection.sceneVersion);
      // The version stamps advance regardless so the next post-switch poll picks up the authored
      // selection, but the selection itself only lands while the scene view is confirmed active.
      if (selectionChanged && live.sceneEntitiesLive) {
        live.setSelectedId(nextSelectedId);
      }

      refreshHeavyState({
        selectedId: nextSelectedId,
        sceneChanged,
        selectionChanged,
        sceneVersion: selection.sceneVersion,
        previousSceneVersion,
      });

      if (selection.animationVersion !== knownAnimationVersion || selectionChanged) {
        refreshAnimation(nextSelectedId);
      }

      knownSceneVersion = selection.sceneVersion;
      knownSelectionVersion = selection.selectionVersion;
      knownSelectedId = nextSelectedId;
      knownPlayVersion = selection.playVersion;
      knownAnimationVersion = selection.animationVersion;
    } catch {
      // Engine briefly busy; the next tick recovers.
    } finally {
      fastInFlight = false;
      schedule();
    }
  }

  const metricsTick = (): void => {
    if (stopped) {
      return;
    }
    const state = useEditorStore.getState();
    const now = performance.now();
    if (!state.metricsPaused && now - lastMetricsFetchAt >= state.metricsRefreshMs) {
      lastMetricsFetchAt = now;
      void pollMetrics().finally(() => {
        if (!stopped) {
          metricsTimer = setTimeout(metricsTick, METRICS_BASE_TICK_MS);
        }
      });
      return;
    }
    metricsTimer = setTimeout(metricsTick, METRICS_BASE_TICK_MS);
  };

  schedule();
  watchdogTimer = setInterval(watchdog, WATCHDOG_INTERVAL_MS);
  metricsTimer = setTimeout(metricsTick, METRICS_BASE_TICK_MS);

  return () => {
    stopped = true;
    if (fastTimer !== null) {
      clearTimeout(fastTimer);
      fastTimer = null;
    }
    if (watchdogTimer !== null) {
      clearInterval(watchdogTimer);
      watchdogTimer = null;
    }
    if (metricsTimer !== null) {
      clearTimeout(metricsTimer);
      metricsTimer = null;
    }
  };
}
