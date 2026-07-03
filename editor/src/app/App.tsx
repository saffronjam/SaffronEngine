/// Top-level editor shell. Wires the Tauri lifecycle events to the store, starts
/// the reconcile poll + the global W/E/R gizmo shortcuts, and composes the chrome
/// above the Scene dock `Layout` and a status bar below.
/// Each main tab that owns a dockspace is its own island: the Scene tree (Hierarchy,
/// the tabbed Inspector/Environment/Render group, Assets, the locked Viewport, plus the
/// right/bottom docks) lives in `Layout`; the asset editor is the second island
/// (`AssetEditorWorkspace`). The embedded viewport's LoadingOverlay is a sibling inside
/// ViewportPanel, never a panel the native window paints over. Both islands stay mounted
/// while the other main tab is active (display:none), so layouts, scroll positions, and the
/// viewport survive tab navigation; each remounts on the per-project key.
import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { client } from "../control/client";
import { loadEditorSettings, startReconcile, useEditorStore } from "../state/store";
import type { AssetEntry } from "../protocol";
import { Topbar } from "../panels/Topbar";
import { Layout } from "./Layout";
import { WindowTitlebar } from "./WindowTitlebar";
import { useGizmoShortcuts } from "./useGizmoShortcuts";
import { useUndoRedoShortcuts } from "./useUndoRedoShortcuts";
import { useMouseBindings } from "./useMouseBindings";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ProjectStartupModal } from "./ProjectStartupModal";
import { SettingsModal } from "./SettingsModal";
import { ExportModal } from "./ExportModal";
import type { ProjectInfo, ViewId } from "../control/client";
import { AssetPreview } from "../components/AssetViewer";
import { CaptureFlame } from "../components/CaptureFlame";
import { MaterialGraphEditor } from "../panels/MaterialGraphEditor";
import { AssetEditorWorkspace } from "../panels/AssetEditorWorkspace";
import { StoreWorkspace } from "../storefront/StoreWorkspace";
import { DockPanelsHost } from "../components/dock/DockPanelsHost";
import { DockDropOverlay } from "../components/dock/DockDropOverlay";
import { AssetDragPreviewTile, firstModelAssetId } from "../components/AssetTile";
import { emitLayoutSettled } from "./layoutBus";
import { logRender } from "../lib/renderLog";
import { Toaster } from "@/components/ui/sonner";
import { cn } from "@/lib/utils";

type EnginePhaseEvent = "starting" | "attaching";

let didRevealWindow = false;
let revealWindowPromise: Promise<void> | null = null;

function revealEditorWindow(): Promise<void> {
  if (revealWindowPromise === null) {
    revealWindowPromise = getCurrentWindow()
      .show()
      .then(() => {
        didRevealWindow = true;
      });
  }
  return revealWindowPromise;
}

export function App() {
  logRender("App");
  const setPhase = useEditorStore((s) => s.setPhase);
  const setProject = useEditorStore((s) => s.setProject);
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const activeViewTabId = useEditorStore((s) => s.activeViewTabId);
  const projectPath = useEditorStore((s) => s.project?.path);
  const activeKind = useEditorStore(
    (s) => s.viewTabs.find((candidate) => candidate.id === s.activeViewTabId)?.kind ?? "scene",
  );
  const activeImage = useEditorStore((s) => {
    const tab = s.viewTabs.find((candidate) => candidate.id === s.activeViewTabId);
    return tab?.kind === "imageViewer"
      ? (s.assets.find((asset) => asset.id === tab.assetId) ?? null)
      : null;
  });
  const activeGraphMaterialId = useEditorStore((s) => {
    const tab = s.viewTabs.find((candidate) => candidate.id === s.activeViewTabId);
    return tab?.kind === "materialGraph" ? tab.materialId : null;
  });
  const activeAssetEditorId = useEditorStore((s) => {
    const tab = s.viewTabs.find((candidate) => candidate.id === s.activeViewTabId);
    return tab?.kind === "assetEditor" ? tab.assetId : null;
  });
  // The Store tab stays mounted (hidden when inactive) while its tab exists, so the search
  // query and results survive switching to another tab and back.
  const storeTabExists = useEditorStore((s) => s.viewTabs.some((tab) => tab.kind === "store"));
  // Keep one asset editor mounted across tab switches (like the scene dock) so returning is instant: it
  // suspends/resumes the engine preview on `active` rather than remounting + re-entering. We keep the
  // most-recently-active asset tab mounted, sticky until its tab closes; switching to a different asset
  // tab remounts via the key (a real model swap).
  const [mountedAssetId, setMountedAssetId] = useState<string | null>(null);
  const mountedAssetTabExists = useEditorStore(
    (s) =>
      mountedAssetId !== null &&
      s.viewTabs.some((tab) => tab.kind === "assetEditor" && tab.assetId === mountedAssetId),
  );
  useEffect(() => {
    // One source of truth for which asset editor is mounted. An ACTIVE asset tab is always the mounted
    // one — and this branch takes precedence so that closing the active asset tab while another asset tab
    // becomes active swaps the preview to it (remount via the key) rather than unmounting. Only when no
    // asset tab is active AND the kept (sticky) asset's tab has since closed do we unmount + exit the
    // preview; otherwise the most-recently-active asset stays mounted (hidden) so returning is instant.
    if (activeKind === "assetEditor" && activeAssetEditorId !== null) {
      setMountedAssetId(activeAssetEditorId);
    } else if (mountedAssetId !== null && !mountedAssetTabExists) {
      setMountedAssetId(null);
    }
  }, [activeKind, activeAssetEditorId, mountedAssetId, mountedAssetTabExists]);
  const [revealed, setRevealed] = useState(didRevealWindow);
  const projectModalOpen = useEditorStore((s) => s.projectModalOpen);
  const setProjectModalOpen = useEditorStore((s) => s.setProjectModalOpen);
  const sceneTabActive = activeViewTabId === "scene";
  // viewportHidden is the MODAL-only global hide (the startup / asset-image modals set it via the store).
  // Per-view park is derived from the active tab: each view's own surface is parked unless its tab is the
  // active pane (or a modal covers the region). activeRenderView tells the engine which scene+camera to
  // render into which target.
  const viewportHidden = useEditorStore((s) => s.viewportHidden);
  const activeRenderView: ViewId = activeKind === "assetEditor" ? "assetPreview" : "scene";
  const sceneParked = viewportHidden || !sceneTabActive;
  const assetParked = viewportHidden || activeKind !== "assetEditor";

  // W/E/R → translate/rotate/scale, gated off while a text field is focused.
  useGizmoShortcuts();
  // Ctrl+Z / Ctrl+Shift+Z (+ Ctrl+Y) → undo/redo on the active tab's history.
  useUndoRedoShortcuts();
  // Mouse-button commands (tab back/forward, close hovered tab) via the keybinding registry.
  useMouseBindings();

  useEffect(() => {
    if (didRevealWindow) {
      return;
    }
    let cancelled = false;
    void revealEditorWindow().finally(() => {
      if (!cancelled) {
        requestAnimationFrame(() => setRevealed(true));
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Subscribe to the Rust-emitted lifecycle events. StrictMode double-mounts in
  // dev, so the cleanup must unlisten idempotently.
  useEffect(() => {
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];

    const register = async (): Promise<void> => {
      const offPhase = await listen<EnginePhaseEvent>("engine-phase", (event) => {
        setPhase(event.payload);
      });
      const offError = await listen<string>("viewport-error", (event) => {
        setPhase("error", event.payload);
      });
      if (disposed) {
        offPhase();
        offError();
        return;
      }
      unlisteners.push(offPhase, offError);
    };

    void register();

    return () => {
      disposed = true;
      for (const off of unlisteners) {
        off();
      }
    };
  }, [setPhase]);

  // Start the focus-gated reconcile poll once; it self-gates on phase === 'ready'.
  useEffect(() => {
    const stop = startReconcile(client);
    return stop;
  }, []);

  // Report viewport visibility so the host idles a hidden/unfocused window (the engine suppresses
  // rendering when occluded). Fire-and-forget on focus/blur + tab visibility; gated on readiness.
  //
  // A native HTML5 drag (dragging an asset onto the viewport) fires a window `blur` and clears
  // `document.hasFocus()`, which would otherwise report `unfocused` and pace the engine down to
  // UNFOCUSED_FPS_CAP (6 fps) — making the drag preview crawl. But dragging an asset *into* the
  // viewport is active use, not a backgrounded window, so we hold `focused` for the whole drag and
  // re-evaluate the real state when it ends.
  useEffect(() => {
    if (phase !== "ready") {
      return;
    }
    let dragActive = false;
    const send = (): void => {
      const state = dragActive
        ? "focused"
        : document.hidden
          ? "occluded"
          : document.hasFocus()
            ? "focused"
            : "unfocused";
      void client.setViewportPowerState(state).catch(() => {
        // Transient (engine briefly busy); the next focus/visibility event re-sends.
      });
    };
    const onDragStart = (): void => {
      dragActive = true;
      send();
    };
    const onDragEnd = (): void => {
      dragActive = false;
      send();
    };
    send();
    window.addEventListener("focus", send);
    window.addEventListener("blur", send);
    document.addEventListener("visibilitychange", send);
    // `dragstart` fires before the drag grab's blur, so the flag is set in time; `dragend`/`drop`
    // both clear it (dragend on the source, drop on the target) and restore the real state.
    document.addEventListener("dragstart", onDragStart);
    document.addEventListener("dragend", onDragEnd);
    document.addEventListener("drop", onDragEnd);
    return () => {
      window.removeEventListener("focus", send);
      window.removeEventListener("blur", send);
      document.removeEventListener("visibilitychange", send);
      document.removeEventListener("dragstart", onDragStart);
      document.removeEventListener("dragend", onDragEnd);
      document.removeEventListener("drop", onDragEnd);
    };
  }, [phase]);

  // Hydrate the keybinding overrides from appdata/settings.json once at startup
  // (editor-wide state, independent of the engine phase).
  useEffect(() => {
    void loadEditorSettings();
  }, []);

  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    let sampleStart = last;
    let frames = 0;
    let averageMs = 0;

    const tick = (now: number): void => {
      const delta = now - last;
      last = now;
      frames += 1;
      averageMs = averageMs === 0 ? delta : averageMs * 0.9 + delta * 0.1;

      if (now - sampleStart >= 500) {
        const hz = (frames * 1000) / (now - sampleStart);
        useEditorStore.getState().setUiFrameStats(hz, averageMs);
        sampleStart = now;
        frames = 0;
      }

      raf = requestAnimationFrame(tick);
    };

    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  useEffect(() => {
    if (phase !== "ready") {
      return;
    }
    let cancelled = false;
    const syncProject = async (): Promise<void> => {
      try {
        const [info, project] = await Promise.all([client.appDataInfo(), client.getProject()]);
        if (cancelled) {
          return;
        }
        setProject(project.loaded ? project : null);
        setProjectModalOpen(!project.loaded && !info.envProject && !info.scratchProject);
      } catch {
        if (!cancelled) {
          setProjectModalOpen(false);
        }
      }
    };
    void syncProject();
    return () => {
      cancelled = true;
    };
  }, [phase, setProject, setProjectModalOpen]);

  const handleProjectLoaded = (project: ProjectInfo): void => {
    setProject(project);
    setProjectModalOpen(false);
  };

  // Push the full per-view state to the engine, gated on the control socket being up (phase === 'ready')
  // — the startup push must not fire before the engine answers (the calls would silently fail and never
  // re-run), and it re-pushes on any later park/active-view change. Per view: park its surface unless its
  // tab is the active pane (a parked surface detaches; its ring keeps the last frame, so unparking re-shows
  // it instantly — preserve-last), and tell the engine which view to render + address (routes
  // activeScene/camera + the per-view target; a scene↔asset change resets that view's temporal state).
  // setActiveView is sequenced through sceneEntitiesLive so the reconcile poll never writes the
  // still-preview entity list into the Scene hierarchy while the engine switches views (set false now,
  // restored when the switch resolves). Force a layout-settled so the shown view commits its pane bounds
  // immediately (no debounce delay).
  useEffect(() => {
    if (phase !== "ready") {
      return;
    }
    void client.setViewportParked("scene", sceneParked).catch(() => {});
    void client.setViewportParked("assetPreview", assetParked).catch(() => {});
    useEditorStore.getState().setSceneEntitiesLive(false);
    void client
      .setActiveView(activeRenderView)
      .catch(() => {})
      .finally(() => useEditorStore.getState().setSceneEntitiesLive(activeRenderView === "scene"));
    requestAnimationFrame(() => emitLayoutSettled({ force: true }));
  }, [phase, sceneParked, assetParked, activeRenderView]);

  // Startup bounds commit, decoupled from the push above: the host div has a 0-size rect until the
  // window is shown, so a commit fired on phase-ready alone (the merged push, and the hook's mount
  // commit) is skipped — leaving the scene surface AND the shared backdrop without bounds, so the
  // viewport stays blank/see-through until an asset round-trip re-commits. Once BOTH the engine is
  // reachable and the window is revealed, force a layout-settle so the visible view commits its pane
  // bounds (and the backdrop its window size) with a real rect — the same path the round-trip uses.
  useEffect(() => {
    if (phase === "ready" && revealed) {
      requestAnimationFrame(() => emitLayoutSettled({ force: true }));
    }
  }, [phase, revealed]);

  return (
    <TooltipProvider delayDuration={300}>
      <div
        className="flex h-full min-w-[900px] flex-col overflow-hidden transition-opacity duration-300 ease-out"
        style={{ opacity: revealed ? 1 : 0 }}
      >
        <WindowTitlebar />
        {/* The dock is hidden, never unmounted, while an asset tab is active: its
            in-memory layout state survives, and the ViewportPanel's host rect goes
            0x0 (computeBounds skips degenerate rects) while viewportHidden parks
            the subsurface. The key remounts the dock once per project so the
            persisted per-project layout applies. */}
        <div className={cn("flex min-h-0 min-w-0 flex-1 flex-col", !sceneTabActive && "hidden")}>
          <Topbar />
          <Layout key={projectPath ?? ""} />
        </div>
        {/* The Scene panels render here, once, portaled into the per-panel host divs the
            dock leaves claim — so a panel's React tree survives moves between docks and
            main-tab switches. Mounted unconditionally; the host divs live inside the dock
            above, hidden with it when a non-scene tab is active. */}
        <DockPanelsHost space="scene" />
        {/* The torn-drag ghost + drop highlight, above every panel (pointer-events: none). */}
        <DockDropOverlay />
        <CatalogDragGhost />
        {activeKind === "imageViewer" && <ImageViewerWorkspace asset={activeImage} />}
        {activeKind === "flamegraph" && <FlameGraphWorkspace />}
        {/* Kept mounted (hidden when inactive) so the search query + results persist across
            tab switches, like the scene dock and asset editor. */}
        {storeTabExists && (
          <div
            className={cn(
              "flex min-h-0 min-w-0 flex-1 flex-col",
              activeKind !== "store" && "hidden",
            )}
          >
            <StoreWorkspace active={activeKind === "store"} />
          </div>
        )}
        {activeKind === "materialGraph" && (
          <MaterialGraphWorkspace materialId={activeGraphMaterialId} />
        )}
        {/* Kept mounted across tab switches (hidden when inactive) so returning suspends/resumes the
            preview instead of re-spawning it. key={assetId} so a model A -> model B switch still remounts
            (cleanup exits A, mount enters B). */}
        {mountedAssetId !== null && (
          <div
            className={cn(
              "flex min-h-0 min-w-0 flex-1 flex-col",
              activeKind !== "assetEditor" && "hidden",
            )}
          >
            <AssetEditorWorkspace
              key={mountedAssetId}
              assetId={mountedAssetId}
              active={activeKind === "assetEditor" && activeAssetEditorId === mountedAssetId}
            />
          </div>
        )}
        <ProjectStartupModal open={projectModalOpen} onProjectLoaded={handleProjectLoaded} />
        <SettingsModal />
        <ExportModal />
        <Toaster />
        <StatusFooter />
      </div>
    </TooltipProvider>
  );
}

function CatalogDragGhost() {
  const catalogDrag = useEditorStore((s) => s.catalogDrag);
  const asset = useEditorStore((s) => {
    if (!s.catalogDrag) {
      return null;
    }
    const id = firstModelAssetId(s.catalogDrag.assetIds, s.assets);
    return id ? (s.assets.find((entry) => entry.id === id) ?? null) : null;
  });
  const [pointer, setPointer] = useState<{
    x: number;
    y: number;
    overViewport: boolean;
  } | null>(null);

  useEffect(() => {
    if (!catalogDrag || !asset) {
      setPointer(null);
      return;
    }

    const update = (event: DragEvent): void => {
      const hit = document.elementFromPoint(event.clientX, event.clientY);
      const overViewport =
        hit instanceof Element && hit.closest("[data-viewport-drop-target='true']") !== null;
      setPointer({ x: event.clientX, y: event.clientY, overViewport });
    };
    const clear = (): void => setPointer(null);

    window.addEventListener("dragover", update);
    window.addEventListener("dragenter", update);
    window.addEventListener("drop", clear);
    window.addEventListener("dragend", clear);
    return () => {
      window.removeEventListener("dragover", update);
      window.removeEventListener("dragenter", update);
      window.removeEventListener("drop", clear);
      window.removeEventListener("dragend", clear);
    };
  }, [asset, catalogDrag]);

  if (!asset || !pointer || pointer.overViewport) {
    return null;
  }

  return (
    <div
      className="pointer-events-none fixed z-[110]"
      style={{ left: pointer.x + 12, top: pointer.y + 12 }}
    >
      <AssetDragPreviewTile entry={asset} />
    </div>
  );
}

/// Hidden dev-mode gesture: five quick clicks on the fps counter, each within this gap of
/// the last, toggle developer mode.
const DEV_GESTURE_CLICKS = 5;
const DEV_GESTURE_WINDOW_MS = 600;

/// Status chip flagging that developer mode is on; the X exits dev mode.
function DevModeChip({ onExit }: { onExit: () => void }) {
  return (
    <span className="flex h-4 flex-none select-none items-center gap-1 rounded-full bg-orange-500/15 pl-2 pr-1 text-[10px] font-medium uppercase tracking-wide text-orange-400">
      Dev mode
      <button
        type="button"
        aria-label="Exit developer mode"
        className="flex size-3.5 items-center justify-center rounded-full hover:bg-orange-500/25"
        onClick={onExit}
      >
        <X className="size-3" />
      </button>
    </span>
  );
}

/// The status strip below the dock. A leaf so the fps meter's twice-a-second store write
/// re-renders this one line, not the entire shell above it. Five quick clicks on the fps
/// counter toggle developer mode (the hidden gesture), and the DEV MODE chip shows here
/// while it is on.
function StatusFooter() {
  const phase = useEditorStore((s) => s.engineStatus.phase);
  const uiFrameRateHz = useEditorStore((s) => s.uiFrameRateHz);
  const devMode = useEditorStore((s) => s.devMode);
  const setDevMode = useEditorStore((s) => s.setDevMode);
  const clicks = useRef(0);
  const lastClickAt = useRef(0);
  const onCounterClick = (): void => {
    const now = Date.now();
    clicks.current = now - lastClickAt.current > DEV_GESTURE_WINDOW_MS ? 1 : clicks.current + 1;
    lastClickAt.current = now;
    if (clicks.current >= DEV_GESTURE_CLICKS) {
      clicks.current = 0;
      setDevMode(!devMode);
    }
  };
  return (
    <footer className="flex h-[22px] flex-none items-center justify-end gap-2 border-t border-border bg-card px-3">
      {devMode ? <DevModeChip onExit={() => setDevMode(false)} /> : null}
      <button
        type="button"
        onClick={onCounterClick}
        aria-label="Engine status and UI frame rate"
        className="cursor-default select-none font-mono text-[10px] uppercase tracking-wide text-muted-foreground"
      >
        {phase} · UI {uiFrameRateHz > 0 ? uiFrameRateHz.toFixed(0) : "--"} fps
      </button>
    </footer>
  );
}

function ImageViewerWorkspace({ asset }: { asset: AssetEntry | null }) {
  if (!asset) {
    return (
      <main className="flex min-h-0 flex-1 items-center justify-center bg-background text-xs italic text-muted-foreground">
        Asset not found
      </main>
    );
  }
  return (
    <main className="flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-background p-6">
      <AssetPreview entry={asset} className="h-full max-h-full w-auto max-w-full" />
    </main>
  );
}

/// The Flame graph main tab: a large view of the last profiler capture's flame chart.
function FlameGraphWorkspace() {
  const capture = useEditorStore((s) => s.capture);
  if (capture === null) {
    return (
      <main className="flex min-h-0 flex-1 items-center justify-center bg-background text-xs italic text-muted-foreground">
        Capture a frame in the Profiler to populate the flame graph.
      </main>
    );
  }
  return (
    <main className="min-h-0 flex-1 overflow-hidden bg-background p-3">
      <CaptureFlame />
    </main>
  );
}

/// The Material graph main tab: the node-graph editor for one material, filling the work area.
function MaterialGraphWorkspace({ materialId }: { materialId: string | null }) {
  if (materialId === null) {
    return (
      <main className="flex min-h-0 flex-1 items-center justify-center bg-background text-xs italic text-muted-foreground">
        Material not found
      </main>
    );
  }
  return (
    <main className="min-h-0 flex-1 overflow-hidden bg-background">
      <MaterialGraphEditor materialId={materialId} />
    </main>
  );
}
