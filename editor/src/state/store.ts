/// Central editor state (Zustand) plus the focus-gated reconcile poll that keeps
/// the slices in sync with the running engine. The data panels read the slices
/// this fills.
import { create } from "zustand";
import { client, isBusyLoading, type Client } from "../control/client";
import type { ProjectInfo } from "../control/client";
import { COMMANDS_BY_ID, isCommandId, type CommandId } from "../lib/keybindings";
import { routeAlarmToasts } from "../lib/alarmToasts";
import { resetScriptErrorToasts, routeScriptErrorToasts } from "../lib/scriptErrorToasts";
import { appendFrameSamples } from "../lib/frameSeries";
import { errorText, notifyError } from "../lib/flash";
import {
  appendEdit,
  emptyHistory,
  takeRedo,
  takeUndo,
  type TabHistory,
  type UndoableEdit,
} from "../lib/undo";
import { emitLayoutSettled } from "../app/layoutBus";
import { getDockDrag } from "../components/dock/dockDrag";
import {
  DEFAULT_LEAF,
  type DockLayout,
  type DockNodeId,
  type DockPanelId,
  type DockSpaceKind,
  type DropTarget,
  allOpenPanels,
  defaultDockLayout,
  defaultDockLayouts,
  findPanelLeaf,
  hasNode,
  hasRequiredPanels,
  isPanelOpenIn,
  knownPanelIds,
  movePanel as movePanelInLayout,
  normalize,
  openPanelResolve,
  panelKind,
  pruneLastLocation,
  removePanel,
  reorderTab as reorderTabInLayout,
  resetLayoutPreservingOpen,
  setBranchSizes as setBranchSizesInLayout,
  setLeafActiveTab,
  validate as validateLayout,
} from "./dockLayout";
import type {
  ActiveAlarmDto,
  AlarmEventDto,
  AnimationClipDto,
  AnimationStateResult,
  AssetEntry,
  DebugOverlaysResult,
  EntityListEntry,
  Environment,
  FrameHistoryDto,
  GizmoState,
  InspectResult,
  PerfConfigDto,
  UpscaleDto,
  ProfileCaptureDto,
  RenderPassTimingsDto,
  RenderStats,
  PhysicsStateResult,
  PhysicsBodyDto,
  ContactEventDto,
  ScriptLogDto,
} from "../protocol";
import type { StoreKind, StoreResult } from "../storefront/types";

/// Cap on the retained alarm-log entries (the dashboard shows the most recent).
const ALARM_LOG_LIMIT = 200;
/// Cap on the retained physics contact/trigger feed (newest-N, like the alarm log).
const CONTACT_LOG_LIMIT = 200;
/// Cap on the retained sa.log feed; the engine ring is bounded too, the editor keeps a deeper window.
const SCRIPT_LOG_LIMIT = 2000;
/// Frames requested from the engine per metrics poll — its full ring, so no frames are
/// missed between polls (the client dedups the overlap by frame index).
const FRAME_HISTORY_SAMPLES = 1000;
const METRICS_RANGE_STORAGE_KEY = "saffron.metricsRangeSec";
const METRICS_BUCKET_STORAGE_KEY = "saffron.metricsBucketMs";
const METRICS_REFRESH_STORAGE_KEY = "saffron.metricsRefreshMs";
const CAPTURE_WINDOW_STORAGE_KEY = "saffron.captureWindowFrames";
const CAPTURE_STATS_STORAGE_KEY = "saffron.captureIncludeStats";

export type EnginePhase = "idle" | "starting" | "attaching" | "ready" | "error";

/// Project-load phase — a DISTINCT axis from `EnginePhase` (a reload runs while the engine stays
/// `ready`; an env/scratch bootstrap load lands before `phase === "ready"`). Drives the startup
/// modal's loading view, not the engine-process `LoadingOverlay`.
export type ProjectLoadPhase = "idle" | "loading" | "ready" | "error";

/// What a `startProjectLoad` kicks off. Stashed in `ProjectLoadState.request` so a failed load's
/// Retry can replay it.
export interface ProjectLoadRequest {
  kind: "open" | "new" | "reload";
  /// `open`: the project path / name.
  path?: string;
  /// `new`: the validated project slug.
  name?: string;
  /// `new`: optional human display name.
  displayName?: string;
}

/// The live project-load progress mirror, fed by the `project-status` poll. `total === 0` means
/// the current stage is indeterminate (spinner, not a filled bar). `version` is the DTO's monotonic
/// stamp — the poll + `setProjectLoad` dedup on it.
export interface ProjectLoadState {
  phase: ProjectLoadPhase;
  stage: string;
  done: number;
  total: number;
  label: string;
  currentItem: string;
  error?: string;
  version: number;
  request?: ProjectLoadRequest;
  /// The load reached 100% and is settling: the bar sits full while the status text fades out,
  /// just before the modal dismisses. Set by the poll's completion hold; drives the fade in the
  /// loading view.
  finalizing?: boolean;
}

/// The profiler capture lifecycle, mirrored from the engine's recorder. On-demand: a
/// capture is armed by a button, never polled on the metrics lane.
export type CaptureState = "idle" | "arming" | "recording" | "ready";
export type ViewTab =
  | { id: "scene"; kind: "scene"; title: "Scene"; closable: false }
  | { id: "flamegraph"; kind: "flamegraph"; title: "Flame graph"; closable: true }
  | { id: "store"; kind: "store"; title: "Store"; closable: true }
  | { id: string; kind: "materialGraph"; materialId: string; title: string; closable: true }
  // The asset editor is keyed by the resolved model (the container uuid), not the clicked asset, so a
  // model, its mesh, and any of its clips open or focus the SAME tab — and the engine's one-previewScene
  // constraint can never be violated by two tabs of one model.
  | { id: string; kind: "assetEditor"; assetId: string; title: string; closable: true }
  // The image viewer: a passive texture/image preview (distinct from the asset editor's 3D preview).
  | {
      id: string;
      kind: "imageViewer";
      assetId: string;
      title: string;
      assetType: AssetEntry["type"];
      closable: true;
    };

/// The Scene tab — always present, never closable, the ultimate navigation fallback.
const SCENE_TAB: ViewTab = { id: "scene", kind: "scene", title: "Scene", closable: false };

/// Cross-tab navigation history with browser semantics: each activation snapshots the
/// landed-on tab, navigating truncates the forward tail, and back/forward reopen a tab
/// that was closed from its stored snapshot. Distinct from `historyByTab` (per-tab
/// undo/redo of edits).
interface TabNavHistory {
  entries: ViewTab[];
  cursor: number;
}

/// Cap on retained navigation entries; older ones drop off the front.
const TAB_HISTORY_CAP = 100;

/// The Assets panel's folder back/forward, exposed to the central mouse dispatcher.
export interface AssetsFolderNav {
  back(): void;
  forward(): void;
}

/// Fold a tab activation into a state patch: set the active tab, remember the tab we
/// left as `previousActiveTabId` (for the close-to-last-active behaviour), and push the
/// landed-on tab onto the navigation history (truncating any forward tail). Re-activating
/// the already-active tab records nothing.
function recordActivation(
  s: EditorState,
  patch: Partial<EditorState>,
  activeViewTabId: string,
): Partial<EditorState> {
  const previousActiveTabId =
    activeViewTabId === s.activeViewTabId ? s.previousActiveTabId : s.activeViewTabId;
  const viewTabs = patch.viewTabs ?? s.viewTabs;
  const tab = viewTabs.find((t) => t.id === activeViewTabId);
  let tabHistory = s.tabHistory;
  if (tab && s.tabHistory.entries[s.tabHistory.cursor]?.id !== activeViewTabId) {
    const entries = [...s.tabHistory.entries.slice(0, s.tabHistory.cursor + 1), tab].slice(
      -TAB_HISTORY_CAP,
    );
    tabHistory = { entries, cursor: entries.length - 1 };
  }
  return { ...patch, activeViewTabId, previousActiveTabId, tabHistory };
}

export interface EngineStatus {
  running: boolean;
  phase: EnginePhase;
  error?: string;
}

/// The editor's play-mode state, mirrored from the engine.
export type PlayState = "edit" | "playing" | "paused";

/// The Assets-grid shift anchor: the last clicked tile, folder or asset.
export type AssetSelectionAnchor = { kind: "asset" | "folder"; key: string } | null;

/// How the Assets grid orders its asset tiles. Name uses a natural (numeric) locale compare;
/// created uses each asset's `createdAt` file timestamp. Folders stay alphabetical regardless.
export type AssetSortMode = "name-asc" | "name-desc" | "created-desc" | "created-asc";

export interface CatalogDragPayload {
  assetIds: string[];
  folderPaths: string[];
}

/// One Assets-grid tile in the body's render order (folders sorted first, then
/// assets) — the coordinate space for shift-range selection.
export interface AssetGridItem {
  kind: "asset" | "folder";
  key: string;
}

export interface EditorState {
  entities: EntityListEntry[];
  selectedId: string | null;
  selectedMaterialId: string | null; // the material asset open in the Material editor panel
  /// Hierarchy rows whose children are shown. Plain UI state, deliberately outside
  /// the sceneVersion/selectionVersion keying so a scene mutation never collapses
  /// the tree; setEntities prunes ids that vanished from the scene.
  expandedIds: Set<string>;
  sceneVersion: number;
  selectionVersion: number;
  componentsBySelected: InspectResult | null;
  assets: AssetEntry[];
  assetFolders: string[];
  /// Assets-grid selection (UI-only, like devMode/viewTabs). Tiles subscribe to
  /// their own membership (`(s) => s.selectedAssetIds.has(id)`), so a selection
  /// delta re-renders only the flipped tiles, never the grid.
  selectedAssetIds: Set<string>;
  selectedFolderPaths: Set<string>;
  /// The shift-range anchor: the last clicked grid tile, folder or asset.
  assetSelectionAnchor: AssetSelectionAnchor;
  /// True while the Assets-grid marquee is sweeping; gates the details overlay so
  /// it opens once on release instead of flickering per crossed tile.
  assetMarqueeActive: boolean;
  /// The asset/folder tile whose grid context menu is open, so that tile keeps a
  /// highlight while the menu is up (cleared on close). Null when no menu is open.
  assetMenuTarget: AssetSelectionAnchor;
  /// True while the pointer is over the Assets panel. Arbitrates the mouse side
  /// buttons: when set, back/forward drive the Assets folder history; otherwise they
  /// drive cross-tab navigation.
  assetsPanelHovered: boolean;
  /// The Assets panel's folder back/forward, registered while it is mounted so the
  /// central mouse dispatcher can drive it without owning the panel's local history.
  assetsFolderNav: AssetsFolderNav | null;
  viewTabs: ViewTab[];
  /// The main-tab id under the pointer, or null. Drives the "close hovered tab" mouse
  /// command; set by the titlebar tab strip on enter/leave.
  hoveredTabId: string | null;
  activeViewTabId: string;
  /// The tab that was active immediately before `activeViewTabId`. Closing the active
  /// tab returns here when it is still open; otherwise it falls back to the left
  /// neighbour. Not an MRU walk — only the single previous tab is consulted.
  previousActiveTabId: string | null;
  /// Browser-style back/forward across tabs, driven by the mouse side buttons; reopens
  /// a tab that was closed since it was last visited.
  tabHistory: TabNavHistory;
  /// One dock-layout tree PER main-tab kind (Scene, asset editor). The two kinds carry
  /// disjoint `DockPanelId` spaces, so a panel can never resolve into the other island's
  /// tree — the structural no-cross-main-tab guarantee. Region emptiness (a leaf's
  /// `tabs.length`) drives the Scene-region mount/unmount.
  dockLayouts: Record<DockSpaceKind, DockLayout>;
  /// Unreal-style re-open memory: the leaf each panel last lived in, so `openPanel`
  /// returns it home. Written by `closePanel` and `movePanel`.
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>;
  environment: Environment | null;
  renderStats: RenderStats | null;
  /// Live physics world stats + contact feed (Physics panel), filled by the open+playing poll.
  physicsState: PhysicsStateResult | null;
  physicsBodies: PhysicsBodyDto[];
  contactLog: ContactEventDto[];
  contactsOverflowed: boolean;
  /// sa.log output (Script Logs panel), filled by the open+playing poll; cleared on each fresh play.
  scriptLogs: ScriptLogDto[];
  scriptLogsOverflowed: boolean;
  /// Viewport debug-overlay toggles (set-debug-overlays); filled by the render-panel-gated poll.
  debugOverlays: DebugOverlaysResult | null;
  /// The editor's target-fps selection: `"default"` tracks the display refresh (the vsync-locked
  /// rAF cadence), or a fixed Hz. Drives the engine's `target_fps` (which paces the render loop).
  /// UI-only state — the resolved Hz lives in `perfConfig.targetFps`.
  targetFpsMode: "default" | number;
  /// The TAAU upscale surface (fixed ratio, dynamic-resolution toggle + budget, live extents),
  /// seeded on connect. `null` until the first `get-upscale`.
  upscale: UpscaleDto | null;
  /// Performance-telemetry slices, filled by the gated metrics poll only while the
  /// Stats tab is open (history/passes) or always (alarms, for the badge).
  perfConfig: PerfConfigDto | null;
  frameHistory: FrameHistoryDto | null;
  passTimings: RenderPassTimingsDto | null;
  activeAlarms: ActiveAlarmDto[];
  /// Append-only FIRING/RESOLVED log (bounded), newest last; drives the dashboard log.
  alarmLog: AlarmEventDto[];
  /// The frame-time graph's time RANGE in seconds — how far back to display (bounded by the
  /// client history ring). A persisted view preference.
  metricsRangeSec: number;
  /// The graph's WINDOW (bucket / group-by) interval in ms — samples within it are averaged
  /// into one plotted point. The smoothness knob (larger = smoother). Persisted.
  metricsBucketMs: number;
  /// How often the metrics lane fetches (ms); persisted, default 1000 (1/s). The badge,
  /// graph, numbers, and per-pass all refresh at this rate.
  metricsRefreshMs: number;
  /// Pause the metrics lane (freeze the dashboard). Session-only; alarms catch up on resume.
  metricsPaused: boolean;
  /// Profiler capture, request-scoped and kept OUT of the metrics lane. The last
  /// completed capture, its lifecycle state, and the in-flight frame progress.
  captureState: CaptureState;
  captureProgress: { current: number; total: number };
  capture: ProfileCaptureDto | null;
  /// Window length to capture (frames); persisted preference, default 1 (single-frame snapshot).
  captureWindowFrames: number;
  /// Request pipeline-statistics (overdraw / cull / vertex-reuse) in the capture; persisted,
  /// default off (the heaviest mode). The engine drops it gracefully when unsupported.
  captureIncludeStats: boolean;
  /// The pass name highlighted across the Profiler sub-views (cross-highlight).
  selectedPass: string | null;
  project: ProjectInfo | null;
  /// Client-side reconcile-poll rate (Hz), an EMA over the actual tick interval.
  /// This is the WEBVIEW poll cadence, NOT the engine frame rate (the engine's own
  /// fps/frameMs/gpuMs ride on `render-stats`; the native viewport paints
  /// independently of this poll).
  pollRateHz: number;
  /// Webview repaint cadence measured with requestAnimationFrame.
  uiFrameRateHz: number;
  uiFrameMs: number;
  engineStatus: EngineStatus;
  /// Project-load progress (a distinct axis from `engineStatus`); drives the startup modal's
  /// loading view. Fed by the `project-status` poll (`useProjectLoadPoll`).
  projectLoad: ProjectLoadState;
  dragActive: boolean;
  /// The current asset-browser drag payload, populated at dragstart so hover targets
  /// can inspect it without relying on DataTransfer.getData during dragover.
  catalogDrag: CatalogDragPayload | null;
  /// True only when the engine is confirmed rendering the authored scene view, so the reconcile poll may
  /// write `entities` (the Scene hierarchy source). Held false while an asset-editor tab is active and
  /// through a view switch until set-active-view resolves, so the preview scene's entities never reach
  /// the Scene hierarchy and the asset→scene switch shows no one-frame stale list. App.tsx drives it.
  sceneEntitiesLive: boolean;
  gizmo: GizmoState;
  /// Editor play mode, mirrored from the engine via the reconcile poll
  /// (get-selection carries playState/playVersion; the poll dedups on the version).
  /// The gizmo is hidden and save/load are locked while not "edit"; panels stay live
  /// (writes discard on stop).
  playState: PlayState;
  /// The selected rig's animation player state (playhead, clip, wrap, speed) or null
  /// when the selection is not an animation player. Read-only mirror of the engine,
  /// refreshed by the reconcile poll's animationVersion gate; drives the TimelinePanel.
  animationState: AnimationStateResult | null;
  /// The animation clips available to the selected rig (the project catalog). Fetched
  /// alongside `animationState` when the selection changes.
  animationClips: AnimationClipDto[];
  /// When true, the native viewport window is parked off-screen so an overlay
  /// (a modal dialog) can paint over the viewport rect — the reparented X11
  /// child always paints on top otherwise. The ViewportPanel reads this and skips
  /// gluing the native window to its div until it clears.
  viewportHidden: boolean;
  /// True while a native OS file dialog (Tauri `open`/`save`) is showing. These
  /// dialogs are not window-modal under the reparented-viewport setup, so the
  /// webview stays live; this flag is the app-side lock that stops a second dialog
  /// from being opened and greys the controls that would open one.
  nativeDialogOpen: boolean;
  /// True while the project picker (`ProjectStartupModal`) is showing. Forced open at
  /// startup until a project loads; the project menu's "New Project" reopens it (then
  /// dismissable, since a project is already loaded).
  projectModalOpen: boolean;
  /// The user hit Cancel on the loading screen. The engine aborts an in-flight load, but a fast
  /// load can already have reached `ready` (and the completion hold started) by the time the click
  /// lands — so the poll checks this flag and refuses to complete a cancelled load, returning to
  /// the picker instead. Reset when a fresh load starts.
  projectLoadCancelling: boolean;
  /// Whether the "Export App" dialog is open (project-menu action).
  exportModalOpen: boolean;
  /// Show the SELECTED entity's components as read-only leaf subrows in the
  /// hierarchy (sourced from componentsBySelected — never an extra inspect).
  /// A persisted view preference, default off so the outliner stays clean.
  showComponentSubrows: boolean;
  /// Hide skeleton joints (rows flagged `bone` by list-entities) in the outliner;
  /// their non-bone descendants re-anchor to the nearest visible ancestor. A
  /// persisted view preference like the subrows toggle.
  hideBones: boolean;
  /// How the Assets grid sorts asset tiles; a persisted view preference (default name A–Z).
  assetSort: AssetSortMode;
  /// Asset Store browse state (the `store` ViewTab). `storeSelected`/`storeSearchText`/
  /// `storeKind` persist to localStorage so reopening returns you to the same store + query;
  /// the rest is in-memory (a live backend search session dies when the bridge restarts).
  storeSelected: string | null;
  storeSearchText: string;
  storeKind: StoreKind | null;
  /// The active backend search-session id (null = no search yet / after a restart).
  storeSession: string | null;
  storeResults: StoreResult[];
  storeExhausted: boolean;
  storeScrollTop: number;
  /// The session id `storeResults` were loaded for; a grid remount whose results already match
  /// the active session restores them (and the scroll) instead of refetching.
  storeResultsSession: string | null;
  /// One-shot "jump the Inspector to this component" signal set by a subrow click;
  /// the Inspector consumes and clears it. Never gated on the poll's versions.
  focusComponent: string | null;
  /// Keybinding OVERRIDES only (command id → key-string); commands absent here use
  /// their registry default. Hydrated from appdata/settings.json at startup and
  /// persisted back (deltas only) on every change.
  keyBindings: Record<string, string>;
  /// True while the Editor Settings modal is open; gates the global shortcut hook
  /// (the dialog holds focus on non-text elements, so the text-entry guard alone
  /// would let shortcuts fire underneath it).
  settingsOpen: boolean;
  /// Developer mode: a persisted, app-wide flag that gates experimental/diagnostic
  /// UI flows (e.g. the project menu's Reload Project). Toggled by the hidden gesture
  /// in the titlebar; read by any panel that wants a dev-only affordance.
  devMode: boolean;
  /// Per-main-tab undo/redo history, keyed by `ViewTab.id` (the scene tab owns the
  /// scene-edit history; each material-graph tab owns its own). Read-only tabs never
  /// get an entry. Editor-only — the engine is unaware undo exists.
  historyByTab: Record<string, TabHistory>;
  /// True while an undo/redo replay's inverse command is in flight; suppresses
  /// re-recording (a replay must not record itself) and re-entrancy.
  historyReplaying: boolean;

  setEntities(entities: EntityListEntry[]): void;
  /// Optimistically rename one entity's hierarchy row between polls (inline rename).
  /// The reconcile poll's sceneVersion bump re-fetches the authoritative list.
  applyOptimisticEntityName(id: string, name: string): void;
  setSelectedId(selectedId: string | null): void;
  setSelectedMaterialId(id: string | null): void;
  selectEntity(id: string): void;
  toggleExpanded(id: string): void;
  setExpanded(id: string, expanded: boolean): void;
  /// Engine-authoritative reparent (null detaches to root): optimistically relink the
  /// moved entity's parentId in place — selection untouched — and hold dragActive over
  /// the round trip so the poll cannot clobber the relink. A rejection falls back to
  /// the next authoritative list (set-parent bumps sceneVersion on success only).
  setParent(id: string, parentId: string | null): Promise<void>;
  setSceneVersion(sceneVersion: number): void;
  setSelectionVersion(selectionVersion: number): void;
  setComponentsBySelected(components: InspectResult | null): void;
  /// Overlay one component's full DTO onto the live inspect result between polls
  /// (optimistic write). The reconcile poll is gated off mid-drag, so this is the
  /// UI's truth until the next poll; a new selectionVersion drops it (poll re-inspects).
  applyOptimisticComponent(component: string, dto: object): void;
  setAssetList(assets: AssetEntry[], folders: string[]): void;
  /// Re-fetch the catalog from the engine into `assets`. Called by the reconcile
  /// poll on a sceneVersion change and eagerly after an import/rename.
  refreshAssets(): Promise<void>;
  /// Instantiate a model asset into the scene (the decoupled "import once, instance many"
  /// path), then refresh the catalog. Returns the new root entity id, or null on failure.
  instantiateModel(modelId: string, name?: string): Promise<string | null>;
  /// Extract an embedded sub-asset to a standalone file (keeping its id), then refresh.
  extractSubAsset(modelId: string, subAssetId: string): Promise<void>;
  /// Revert an extracted sub-asset to the embedded chunk, then refresh.
  clearExtraction(modelId: string, subAssetId: string): Promise<void>;
  /// Rescan assets/ and reconcile the catalog from disk, then refresh.
  scanAssets(): Promise<void>;
  /// Re-bake a model from its source (skip if unchanged), then refresh.
  reimportModel(modelId: string): Promise<void>;
  /// Click selection for Assets-grid tiles: plain replaces, toggle (ctrl/meta)
  /// flips membership, shift unions the anchor→key range along `gridOrder` (an
  /// absent anchor falls through to the plain/toggle path).
  selectAssetGridItem(
    kind: "asset" | "folder",
    key: string,
    modifiers: { shift: boolean; toggle: boolean },
    gridOrder: AssetGridItem[],
  ): void;
  /// Replace the grid selection outright (marquee sweep, drag-collapse, clear);
  /// the anchor becomes the last asset, else the last folder, else null.
  setAssetSelection(assetIds: string[], folderPaths: string[]): void;
  /// Drop selected assets that left the visible grid and selected folders that no
  /// longer exist (plus a dangling anchor); identity-stable when nothing changed.
  pruneAssetSelection(visibleAssets: AssetEntry[], folders: readonly string[]): void;
  /// Drop just-deleted assets from the selection eagerly, before the refresh.
  removeFromAssetSelection(assetIds: ReadonlySet<string>): void;
  /// Rewrite selected folder paths through a folder move (prefix rename).
  rewriteSelectedFolderPaths(rewrite: (path: string) => string): void;
  setAssetMarqueeActive(assetMarqueeActive: boolean): void;
  setAssetMenuTarget(target: AssetSelectionAnchor): void;
  setAssetsPanelHovered(assetsPanelHovered: boolean): void;
  setAssetsFolderNav(assetsFolderNav: AssetsFolderNav | null): void;
  setHoveredTabId(hoveredTabId: string | null): void;
  /// Open (or focus) the image/texture viewer for a texture asset.
  openImageViewerTab(asset: AssetEntry): void;
  /// Open (or focus) the Flame graph main tab.
  openFlameTab(): void;
  /// Open (or focus) the asset Store main tab.
  openStoreTab(): void;
  /// Open (or focus) the node-graph editor for a material as a main tab.
  openMaterialGraphTab(materialId: string): void;
  /// Open (or focus) the asset editor for a model, keyed by its resolved container uuid. The caller
  /// resolves an asset (model, mesh, or clip) to its model id before opening.
  openAssetEditorTab(assetId: string, title: string): void;
  /// Route an asset (a model, mesh, or clip) to the asset editor: resolve it via get-asset-model (all
  /// share the owning .smodel container), then open/focus that model's tab. On a resolution failure the
  /// tab opens keyed by the asset so the workspace surfaces a load-failure state (not a dead toast).
  openAssetEditorForAsset(assetId: string, fallbackName: string): void;
  closeViewTab(id: string): void;
  setActiveViewTab(id: string): void;
  moveViewTab(id: string, index: number): void;
  /// Step through the cross-tab navigation history (mouse back/forward): -1 is back, +1
  /// is forward. A target tab that was closed is reopened from its snapshot; a no-op past
  /// either end.
  navigateTabHistory(step: -1 | 1): void;
  /// Focus-or-open a panel: activate it if already open, else resolve a leaf
  /// (last-location ⇒ default ⇒ first non-locked ⇒ a fresh leaf) and insert it.
  openPanel(id: DockPanelId): void;
  /// Close a panel; the active tab falls back to the index−1 neighbor and the leaf is
  /// remembered in `lastLocation`.
  closePanel(id: DockPanelId): void;
  /// Make an already-open panel its leaf's active tab (no-op when closed).
  activatePanel(id: DockPanelId): void;
  /// Move a panel to a drop target (tab-merge or split), then normalize.
  movePanel(id: DockPanelId, target: DropTarget): void;
  /// Reorder a tab within its leaf; `index` is in the without-moving-tab space.
  reorderTab(leafId: DockNodeId, id: DockPanelId, index: number): void;
  /// Persist a branch's child sizes (the rrp `onLayoutChanged` round-trip).
  setBranchSizes(branchId: DockNodeId, sizes: Record<string, number>): void;
  /// Reset one island's dock tree to its default factory.
  resetDockLayout(): void;
  /// Load both dock trees + last-location memory from the per-project key (validated;
  /// per-kind fallback to the default factory). No-op without a loaded project.
  hydrateDockLayouts(): void;
  setEnvironment(environment: Environment | null): void;
  /// Set the selected rig's animation state and available clips together (poll-driven).
  setAnimationState(state: AnimationStateResult | null, clips: AnimationClipDto[]): void;
  setRenderStats(renderStats: RenderStats | null): void;
  setPhysicsState(physicsState: PhysicsStateResult | null): void;
  setPhysicsBodies(physicsBodies: PhysicsBodyDto[]): void;
  appendContactEvents(events: ContactEventDto[], overflowed: boolean): void;
  clearContacts(): void;
  appendScriptLogs(events: ScriptLogDto[], overflowed: boolean): void;
  clearScriptLogs(): void;
  setDebugOverlays(debugOverlays: DebugOverlaysResult | null): void;
  setPerfConfig(perfConfig: PerfConfigDto | null): void;
  setUpscale(upscale: UpscaleDto | null): void;
  setTargetFpsMode(mode: "default" | number): void;
  setFrameHistory(frameHistory: FrameHistoryDto | null): void;
  setPassTimings(passTimings: RenderPassTimingsDto | null): void;
  setActiveAlarms(activeAlarms: ActiveAlarmDto[]): void;
  /// Append drained alarm events to the bounded log (newest last).
  appendAlarmEvents(events: AlarmEventDto[]): void;
  /// Set + persist the frame-time graph's time range (seconds).
  setMetricsRangeSec(metricsRangeSec: number): void;
  /// Set + persist the graph's bucket (group-by) interval (ms).
  setMetricsBucketMs(metricsBucketMs: number): void;
  /// Set + persist the metrics fetch interval (ms).
  setMetricsRefreshMs(metricsRefreshMs: number): void;
  /// Pause/resume the metrics lane.
  setMetricsPaused(metricsPaused: boolean): void;
  /// Profiler capture setters (request-scoped; no metrics-lane coupling).
  setCaptureState(captureState: CaptureState): void;
  setCaptureProgress(current: number, total: number): void;
  setCapture(capture: ProfileCaptureDto | null): void;
  /// Set + persist the capture window length (frames).
  setCaptureWindowFrames(captureWindowFrames: number): void;
  setCaptureIncludeStats(captureIncludeStats: boolean): void;
  setSelectedPass(selectedPass: string | null): void;
  setProject(project: ProjectInfo | null): void;
  /// Merge a project-load progress patch, identity-stable (like `setGizmo`): no re-render when the
  /// patch changes nothing, and dedup on the DTO `version`. The `project-status` poll calls this.
  setProjectLoad(patch: Partial<ProjectLoadState>): void;
  /// Kick off a non-blocking project load: stash the request, flip to `loading`, fire the engine
  /// kick without awaiting completion. Progress + completion come from `useProjectLoadPoll`.
  startProjectLoad(request: ProjectLoadRequest): Promise<void>;
  setPollRateHz(pollRateHz: number): void;
  setUiFrameStats(frameRateHz: number, frameMs: number): void;
  /// Hard scene reset after a project/scene load: clear entities + selection +
  /// the live inspect result + assets + environment, invalidate cached thumbnails,
  /// and let the next reconcile tick re-fetch everything against the new scene
  /// (the engine cleared its own selection on load, so we mirror that). Idempotent.
  resetSceneState(): void;
  setEngineStatus(patch: Partial<EngineStatus>): void;
  setPhase(phase: EnginePhase, error?: string): void;
  setDragActive(dragActive: boolean): void;
  setCatalogDrag(catalogDrag: CatalogDragPayload | null): void;
  setSceneEntitiesLive(live: boolean): void;
  setGizmo(patch: Partial<GizmoState>): void;
  /// Optimistic play-state write (the reconcile poll repairs it from the engine).
  setPlayState(playState: PlayState): void;
  setViewportHidden(viewportHidden: boolean): void;
  setNativeDialogOpen(nativeDialogOpen: boolean): void;
  setProjectModalOpen(projectModalOpen: boolean): void;
  setProjectLoadCancelling(cancelling: boolean): void;
  setExportModalOpen(exportModalOpen: boolean): void;
  toggleComponentSubrows(): void;
  toggleHideBones(): void;
  setAssetSort(assetSort: AssetSortMode): void;
  /// Set the Asset Store's selected store and persist it. The caller re-runs the search.
  setStoreSelected(storeSelected: string | null): void;
  /// Set + persist the Asset Store's committed query (free text + optional kind filter).
  setStoreQuery(query: { text: string; kind: StoreKind | null }): void;
  /// Set the active backend search-session id (in-memory).
  setStoreSession(storeSession: string | null): void;
  /// Replace the browse results, stamping the session they belong to (a fresh search's first batch).
  setStoreResults(storeResults: StoreResult[], session: string): void;
  /// Append the next batch of browse results for `session` (infinite scroll).
  appendStoreResults(results: StoreResult[], session: string): void;
  setStoreExhausted(storeExhausted: boolean): void;
  setStoreScrollTop(storeScrollTop: number): void;
  /// Clear the live browse session + results (nothing selectable, or the selected store was removed).
  resetStoreBrowse(): void;
  setFocusComponent(focusComponent: string | null): void;
  /// Set one binding override and persist. A value equal to the registry default
  /// removes the override instead, keeping settings.json delta-minimal.
  setKeyBinding(id: CommandId, value: string): void;
  /// Remove one override (back to the default) and persist.
  resetKeyBinding(id: CommandId): void;
  /// Remove every override and persist.
  resetAllKeyBindings(): void;
  /// Load-time hydration: replace the override map (unknown command ids dropped),
  /// without writing back.
  hydrateKeyBindings(overrides: Record<string, string>): void;
  setSettingsOpen(settingsOpen: boolean): void;
  /// Set developer mode and persist it (localStorage, like the view-preference toggles).
  setDevMode(devMode: boolean): void;
  /// Record an edit onto a tab's history (default: the active tab). No-ops on a
  /// read-only tab and while a replay is in flight.
  pushEdit(edit: UndoableEdit, tabId?: string): void;
  /// Replay the next undo on a tab's history (default: the active tab).
  undo(tabId?: string): Promise<void>;
  /// Replay the next redo on a tab's history (default: the active tab).
  redo(tabId?: string): Promise<void>;
  /// Open a gesture transaction: capture `prior` now and push exactly one entry at
  /// `commit` (the drag/scrub end), so a burst of ticks becomes one undo entry.
  beginEdit<T>(opts: { prior: T; selectionId?: string }): {
    commit(final: T, build: (prior: T, final: T) => UndoableEdit): void;
  };
  /// Drop one tab's history (on tab close).
  clearTabHistory(tabId: string): void;
  /// Drop the scene history and every orphaned non-scene history (on scene replace).
  clearSceneHistory(): void;
}

export const useEditorStore = create<EditorState>((set) => ({
  entities: [],
  selectedId: null,
  selectedMaterialId: null,
  expandedIds: new Set<string>(),
  sceneVersion: -1,
  selectionVersion: -1,
  componentsBySelected: null,
  assets: [],
  assetFolders: [],
  selectedAssetIds: new Set<string>(),
  selectedFolderPaths: new Set<string>(),
  assetSelectionAnchor: null,
  assetMarqueeActive: false,
  assetMenuTarget: null,
  assetsPanelHovered: false,
  assetsFolderNav: null,
  viewTabs: [SCENE_TAB],
  hoveredTabId: null,
  activeViewTabId: "scene",
  previousActiveTabId: null,
  tabHistory: { entries: [SCENE_TAB], cursor: 0 },
  dockLayouts: defaultDockLayouts(),
  lastLocation: {},
  environment: null,
  renderStats: null,
  physicsState: null,
  physicsBodies: [],
  contactLog: [],
  contactsOverflowed: false,
  scriptLogs: [],
  scriptLogsOverflowed: false,
  debugOverlays: null,
  perfConfig: null,
  upscale: null,
  targetFpsMode: "default",
  frameHistory: null,
  passTimings: null,
  activeAlarms: [],
  alarmLog: [],
  metricsRangeSec: loadMetricsRangeSec(),
  metricsBucketMs: loadMetricsBucketMs(),
  metricsRefreshMs: loadMetricsRefreshMs(),
  metricsPaused: false,
  captureState: "idle",
  captureProgress: { current: 0, total: 0 },
  capture: null,
  captureWindowFrames: loadCaptureWindowFrames(),
  captureIncludeStats: loadCaptureIncludeStats(),
  selectedPass: null,
  project: null,
  pollRateHz: 0,
  uiFrameRateHz: 0,
  uiFrameMs: 0,
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
  dragActive: false,
  catalogDrag: null,
  // True at startup: the scene view is the active view from the first frame, so authored entities load
  // immediately. App.tsx holds it false while an asset tab is active / during a view switch.
  sceneEntitiesLive: true,
  gizmo: { op: "translate", space: "world", preserveChildren: false },
  playState: "edit",
  animationState: null,
  animationClips: [],
  viewportHidden: false,
  nativeDialogOpen: false,
  projectModalOpen: false,
  projectLoadCancelling: false,
  exportModalOpen: false,
  showComponentSubrows: loadShowSubrows(),
  hideBones: loadHideBones(),
  assetSort: loadAssetSort(),
  storeSelected: loadStoreSelected(),
  storeSearchText: loadStoreSearchText(),
  storeKind: loadStoreKind(),
  storeSession: null,
  storeResults: [],
  storeExhausted: false,
  storeScrollTop: 0,
  storeResultsSession: null,
  focusComponent: null,
  keyBindings: {},
  settingsOpen: false,
  devMode: loadDevMode(),
  historyByTab: {},
  historyReplaying: false,

  setEntities: (entities) =>
    set((s) => {
      // Prune expand-state for ids that left the scene; survivors keep their state
      // so a poll refresh never collapses the tree.
      const present = new Set(entities.map((e) => e.id));
      let expandedIds = s.expandedIds;
      if ([...expandedIds].some((id) => !present.has(id))) {
        expandedIds = new Set([...expandedIds].filter((id) => present.has(id)));
        persistExpanded(expandedIds);
      }
      return { entities, expandedIds };
    }),
  applyOptimisticEntityName: (id, name) =>
    set((s) => ({
      entities: s.entities.map((e) => (e.id === id ? { ...e, name } : e)),
    })),
  setSelectedId: (selectedId) => set({ selectedId }),
  setSelectedMaterialId: (selectedMaterialId) => set({ selectedMaterialId }),
  // Optimistic local selection: set immediately on a hierarchy click so the row
  // highlights without waiting a poll interval; the reconcile poll confirms via
  // selectionVersion (engine is authoritative if a newer version arrives).
  selectEntity: (id) => set({ selectedId: id }),
  toggleExpanded: (id) =>
    set((s) => {
      const expandedIds = new Set(s.expandedIds);
      if (!expandedIds.delete(id)) {
        expandedIds.add(id);
      }
      persistExpanded(expandedIds);
      return { expandedIds };
    }),
  setExpanded: (id, expanded) =>
    set((s) => {
      if (s.expandedIds.has(id) === expanded) {
        return {};
      }
      const expandedIds = new Set(s.expandedIds);
      if (expanded) {
        expandedIds.add(id);
      } else {
        expandedIds.delete(id);
      }
      persistExpanded(expandedIds);
      return { expandedIds };
    }),
  setParent: async (id, parentId) => {
    const previous = useEditorStore.getState().entities.find((e) => e.id === id)?.parentId;
    useEditorStore.getState().setDragActive(true);
    set((s) => ({
      entities: s.entities.map((e) =>
        e.id === id ? { ...e, parentId: parentId ?? undefined } : e,
      ),
    }));
    try {
      await client.setParent(id, parentId);
      useEditorStore.getState().pushEdit(
        {
          label: "Reparent",
          selectionId: id,
          undo: () => client.setParent(id, previous ?? null),
          redo: () => client.setParent(id, parentId),
        },
        "scene",
      );
    } catch (err) {
      // A rejected reparent never bumps sceneVersion, so the poll will not restore
      // the row — roll the optimistic relink back by hand.
      set((s) => ({
        entities: s.entities.map((e) => (e.id === id ? { ...e, parentId: previous } : e)),
      }));
      throw err;
    } finally {
      useEditorStore.getState().setDragActive(false);
    }
  },
  setSceneVersion: (sceneVersion) => set({ sceneVersion }),
  setSelectionVersion: (selectionVersion) => set({ selectionVersion }),
  setComponentsBySelected: (componentsBySelected) => set({ componentsBySelected }),
  applyOptimisticComponent: (component, dto) =>
    set((s) => {
      if (!s.componentsBySelected) {
        return {};
      }
      return {
        componentsBySelected: {
          ...s.componentsBySelected,
          components: {
            ...s.componentsBySelected.components,
            [component]: dto,
          },
        },
      };
    }),
  setAssetList: (assets, assetFolders) =>
    set((s) => ({
      assets,
      assetFolders,
      viewTabs: s.viewTabs.map((tab) => {
        if (tab.kind === "assetEditor") {
          const model = assets.find((entry) => entry.id === tab.assetId);
          return model ? { ...tab, title: model.name } : tab;
        }
        if (tab.kind !== "imageViewer") {
          return tab;
        }
        const asset = assets.find((entry) => entry.id === tab.assetId);
        return asset ? { ...tab, title: asset.name, assetType: asset.type } : tab;
      }),
    })),
  refreshAssets: async () => {
    try {
      const list = await client.listAssets();
      useEditorStore.getState().setAssetList(list.assets, list.folders);
    } catch {
      // Engine may be briefly busy; the next reconcile tick recovers.
    }
  },
  // These actions let a rejected control call propagate, so the calling panel surfaces it
  // through notifyError; they refresh the catalog on success.
  instantiateModel: async (modelId, name) => {
    const entity = await client.instantiateModel(modelId, name);
    if (entity.id) {
      recordEntityCreation(entity.id, "Add to scene");
    }
    return entity.id ?? null;
  },
  extractSubAsset: async (modelId, subAssetId) => {
    await client.extractSubAsset(modelId, subAssetId);
    await useEditorStore.getState().refreshAssets();
  },
  clearExtraction: async (modelId, subAssetId) => {
    await client.clearExtraction(modelId, subAssetId);
    await useEditorStore.getState().refreshAssets();
  },
  scanAssets: async () => {
    await client.scanAssets();
    await useEditorStore.getState().refreshAssets();
  },
  reimportModel: async (modelId) => {
    await client.reimportModel(modelId);
    await useEditorStore.getState().refreshAssets();
  },
  selectAssetGridItem: (kind, key, modifiers, gridOrder) =>
    set((s) => {
      const index = gridOrder.findIndex((item) => item.kind === kind && item.key === key);
      if (index < 0) {
        return {};
      }
      const anchor = s.assetSelectionAnchor;
      if (modifiers.shift && anchor) {
        const anchorIndex = gridOrder.findIndex(
          (item) => item.kind === anchor.kind && item.key === anchor.key,
        );
        if (anchorIndex >= 0) {
          const range = gridOrder.slice(
            Math.min(anchorIndex, index),
            Math.max(anchorIndex, index) + 1,
          );
          const selectedAssetIds = new Set(s.selectedAssetIds);
          const selectedFolderPaths = new Set(s.selectedFolderPaths);
          for (const item of range) {
            (item.kind === "asset" ? selectedAssetIds : selectedFolderPaths).add(item.key);
          }
          return { selectedAssetIds, selectedFolderPaths, assetSelectionAnchor: { kind, key } };
        }
      }
      if (modifiers.toggle) {
        const next = new Set(kind === "asset" ? s.selectedAssetIds : s.selectedFolderPaths);
        if (!next.delete(key)) {
          next.add(key);
        }
        return {
          ...(kind === "asset" ? { selectedAssetIds: next } : { selectedFolderPaths: next }),
          assetSelectionAnchor: { kind, key },
        };
      }
      return {
        selectedAssetIds: kind === "asset" ? new Set([key]) : new Set<string>(),
        selectedFolderPaths: kind === "folder" ? new Set([key]) : new Set<string>(),
        assetSelectionAnchor: { kind, key },
      };
    }),
  setAssetSelection: (assetIds, folderPaths) =>
    set((s) => {
      if (
        assetIds.length === 0 &&
        folderPaths.length === 0 &&
        s.selectedAssetIds.size === 0 &&
        s.selectedFolderPaths.size === 0 &&
        s.assetSelectionAnchor === null
      ) {
        return {};
      }
      const lastAsset = assetIds.at(-1);
      const lastFolder = folderPaths.at(-1);
      return {
        selectedAssetIds: new Set(assetIds),
        selectedFolderPaths: new Set(folderPaths),
        assetSelectionAnchor: lastAsset
          ? { kind: "asset", key: lastAsset }
          : lastFolder
            ? { kind: "folder", key: lastFolder }
            : null,
      };
    }),
  pruneAssetSelection: (visibleAssets, folders) =>
    set((s) => {
      const visibleIds = new Set(visibleAssets.map((asset) => asset.id));
      const folderSet = new Set(folders);
      const patch: Partial<EditorState> = {};
      const assetIds = [...s.selectedAssetIds].filter((id) => visibleIds.has(id));
      if (assetIds.length !== s.selectedAssetIds.size) {
        patch.selectedAssetIds = new Set(assetIds);
      }
      const folderPaths = [...s.selectedFolderPaths].filter((path) => folderSet.has(path));
      if (folderPaths.length !== s.selectedFolderPaths.size) {
        patch.selectedFolderPaths = new Set(folderPaths);
      }
      const anchor = s.assetSelectionAnchor;
      if (
        anchor &&
        (anchor.kind === "asset" ? !visibleIds.has(anchor.key) : !folderSet.has(anchor.key))
      ) {
        patch.assetSelectionAnchor = null;
      }
      return patch;
    }),
  removeFromAssetSelection: (assetIds) =>
    set((s) => {
      const next = new Set([...s.selectedAssetIds].filter((id) => !assetIds.has(id)));
      return next.size === s.selectedAssetIds.size ? {} : { selectedAssetIds: next };
    }),
  rewriteSelectedFolderPaths: (rewrite) =>
    set((s) => {
      let changed = false;
      const next = new Set<string>();
      for (const path of s.selectedFolderPaths) {
        const to = rewrite(path);
        changed ||= to !== path;
        next.add(to);
      }
      return changed ? { selectedFolderPaths: next } : {};
    }),
  setAssetMarqueeActive: (assetMarqueeActive) => set({ assetMarqueeActive }),
  setAssetMenuTarget: (assetMenuTarget) => set({ assetMenuTarget }),
  setAssetsPanelHovered: (assetsPanelHovered) => set({ assetsPanelHovered }),
  setAssetsFolderNav: (assetsFolderNav) => set({ assetsFolderNav }),
  setHoveredTabId: (hoveredTabId) => set({ hoveredTabId }),
  openImageViewerTab: (asset) =>
    set((s) => {
      const id = `imageViewer:${asset.id}`;
      const tab: ViewTab = {
        id,
        kind: "imageViewer",
        assetId: asset.id,
        title: asset.name,
        assetType: asset.type,
        closable: true,
      };
      const existing = s.viewTabs.some((t) => t.id === id);
      return recordActivation(s, { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] }, id);
    }),
  openFlameTab: () =>
    set((s) => {
      const tab: ViewTab = {
        id: "flamegraph",
        kind: "flamegraph",
        title: "Flame graph",
        closable: true,
      };
      const existing = s.viewTabs.some((t) => t.id === "flamegraph");
      return recordActivation(
        s,
        { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] },
        "flamegraph",
      );
    }),
  openStoreTab: () =>
    set((s) => {
      const tab: ViewTab = { id: "store", kind: "store", title: "Store", closable: true };
      const existing = s.viewTabs.some((t) => t.id === "store");
      return recordActivation(
        s,
        { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] },
        "store",
      );
    }),
  openMaterialGraphTab: (materialId) =>
    set((s) => {
      const id = `materialGraph:${materialId}`;
      const tab: ViewTab = {
        id,
        kind: "materialGraph",
        materialId,
        title: "Material graph",
        closable: true,
      };
      const existing = s.viewTabs.some((t) => t.id === id);
      return recordActivation(s, { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] }, id);
    }),
  openAssetEditorTab: (assetId, title) =>
    set((s) => {
      const id = `assetEditor:${assetId}`;
      const tab: ViewTab = { id, kind: "assetEditor", assetId, title, closable: true };
      const existing = s.viewTabs.some((t) => t.id === id);
      return recordActivation(s, { viewTabs: existing ? s.viewTabs : [...s.viewTabs, tab] }, id);
    }),
  openAssetEditorForAsset: (assetId, fallbackName) => {
    void (async () => {
      try {
        const model = await client.getAssetModel(assetId);
        useEditorStore.getState().openAssetEditorTab(model.mesh, model.name);
      } catch {
        // Resolution failed (e.g. not part of a model container): key the tab by the asset; the
        // workspace's enter then surfaces a load-failure state.
        useEditorStore.getState().openAssetEditorTab(assetId, fallbackName);
      }
    })();
  },
  closeViewTab: (id) =>
    set((s) => {
      if (id === "scene") {
        return {};
      }
      const index = s.viewTabs.findIndex((tab) => tab.id === id);
      const viewTabs = s.viewTabs.filter((tab) => tab.id !== id);
      const patch: Partial<EditorState> = { viewTabs };
      if (id in s.historyByTab) {
        const historyByTab = { ...s.historyByTab };
        delete historyByTab[id];
        patch.historyByTab = historyByTab;
      }
      if (s.activeViewTabId !== id) {
        // A background tab closed: the active tab is unchanged. Forget it as the
        // remembered previous tab if that is what it was.
        return {
          ...patch,
          previousActiveTabId: s.previousActiveTabId === id ? null : s.previousActiveTabId,
        };
      }
      // The active tab closed: land on the last active tab if it is still open,
      // otherwise the tab to the left (the historical fallback), else Scene.
      const previous = s.previousActiveTabId;
      const target =
        previous && previous !== id && viewTabs.some((tab) => tab.id === previous)
          ? previous
          : (viewTabs[Math.max(0, index - 1)]?.id ?? "scene");
      return recordActivation(s, patch, target);
    }),
  setActiveViewTab: (id) =>
    set((s) => (s.viewTabs.some((tab) => tab.id === id) ? recordActivation(s, {}, id) : {})),
  navigateTabHistory: (step) =>
    set((s) => {
      const cursor = s.tabHistory.cursor + step;
      if (cursor < 0 || cursor >= s.tabHistory.entries.length) {
        return {};
      }
      const entry = s.tabHistory.entries[cursor];
      const reopened = !s.viewTabs.some((tab) => tab.id === entry.id);
      return {
        activeViewTabId: entry.id,
        viewTabs: reopened ? [...s.viewTabs, entry] : s.viewTabs,
        tabHistory: { ...s.tabHistory, cursor },
        previousActiveTabId: s.activeViewTabId,
      };
    }),
  moveViewTab: (id, index) =>
    set((s) => {
      if (id === "scene") {
        return {};
      }
      const moving = s.viewTabs.find((tab) => tab.id === id);
      if (!moving) {
        return {};
      }
      const without = s.viewTabs.filter((tab) => tab.id !== id);
      const nextIndex = Math.min(Math.max(1, index), without.length);
      return {
        viewTabs: [...without.slice(0, nextIndex), moving, ...without.slice(nextIndex)],
      };
    }),
  openPanel: (id) =>
    set((s) => {
      const kind = panelKind(id);
      const resolved = openPanelResolve(s.dockLayouts[kind], id, {
        defaultLeafId: DEFAULT_LEAF[id],
        lastLeafId: s.lastLocation[id],
      });
      return { dockLayouts: { ...s.dockLayouts, [kind]: resolved.layout } };
    }),
  closePanel: (id) =>
    set((s) => {
      const kind = panelKind(id);
      const leafId = findPanelLeaf(s.dockLayouts[kind], id);
      if (leafId === null) {
        return {};
      }
      const layout = normalize(removePanel(s.dockLayouts[kind], id));
      return {
        dockLayouts: { ...s.dockLayouts, [kind]: layout },
        lastLocation: { ...s.lastLocation, [id]: leafId },
      };
    }),
  activatePanel: (id) =>
    set((s) => {
      const kind = panelKind(id);
      const layout = setLeafActiveTab(s.dockLayouts[kind], id);
      return layout === s.dockLayouts[kind]
        ? {}
        : { dockLayouts: { ...s.dockLayouts, [kind]: layout } };
    }),
  movePanel: (id, target) =>
    set((s) => {
      const kind = panelKind(id);
      const layout = movePanelInLayout(s.dockLayouts[kind], id, target);
      const leafId = findPanelLeaf(layout, id);
      return {
        dockLayouts: { ...s.dockLayouts, [kind]: layout },
        lastLocation: leafId ? { ...s.lastLocation, [id]: leafId } : s.lastLocation,
      };
    }),
  reorderTab: (leafId, id, index) =>
    set((s) => {
      const kind = panelKind(id);
      return {
        dockLayouts: {
          ...s.dockLayouts,
          [kind]: reorderTabInLayout(s.dockLayouts[kind], leafId, id, index),
        },
      };
    }),
  setBranchSizes: (branchId, sizes) =>
    set((s) => {
      // A torn drag renders an effective (panel-subtracted) layout whose collapse remounts an rrp
      // group and fires `onLayoutChanged` with transient sizes — ignore those so they never reach
      // the real tree; real resizes resume the moment the drag clears.
      if (getDockDrag() !== null) {
        return {};
      }
      const kind: DockSpaceKind = hasNode(s.dockLayouts.scene, branchId) ? "scene" : "assetEditor";
      return {
        dockLayouts: {
          ...s.dockLayouts,
          [kind]: setBranchSizesInLayout(s.dockLayouts[kind], branchId, sizes),
        },
      };
    }),
  resetDockLayout: () =>
    set((s) => ({
      dockLayouts: {
        scene: resetLayoutPreservingOpen("scene", allOpenPanels(s.dockLayouts.scene)),
        assetEditor: resetLayoutPreservingOpen(
          "assetEditor",
          allOpenPanels(s.dockLayouts.assetEditor),
        ),
      },
      lastLocation: {},
    })),
  hydrateDockLayouts: () =>
    set((s) => {
      const loaded = loadDockLayouts(s.project?.path);
      if (!loaded) {
        return {};
      }
      // Validate, then reject a tree that lacks a structural panel for the default.
      // No layout migration.
      const accept = (raw: DockLayout | undefined, kind: DockSpaceKind): DockLayout => {
        const validated = raw ? validateLayout(raw, knownPanelIds(kind)) : null;
        return validated && hasRequiredPanels(validated, kind)
          ? validated
          : defaultDockLayout(kind);
      };
      const scene = accept(loaded.layouts?.scene, "scene");
      const assetEditor = accept(loaded.layouts?.assetEditor, "assetEditor");
      const lastLocation = {
        ...pruneLastLocation(scene, loaded.lastLocation),
        ...pruneLastLocation(assetEditor, loaded.lastLocation),
      };
      return { dockLayouts: { scene, assetEditor }, lastLocation };
    }),
  setEnvironment: (environment) => set({ environment }),
  setDebugOverlays: (debugOverlays) => set({ debugOverlays }),
  setAnimationState: (animationState, animationClips) => set({ animationState, animationClips }),
  setRenderStats: (renderStats) => set({ renderStats }),
  setPhysicsState: (physicsState) => set({ physicsState }),
  setPhysicsBodies: (physicsBodies) => set({ physicsBodies }),
  appendContactEvents: (events, overflowed) =>
    set((s) => {
      if (events.length === 0) {
        return overflowed === s.contactsOverflowed ? {} : { contactsOverflowed: overflowed };
      }
      // Newest-first, bounded ring (the engine drains oldest→newest, so prepend reversed).
      const contactLog = [...events].reverse().concat(s.contactLog).slice(0, CONTACT_LOG_LIMIT);
      return { contactLog, contactsOverflowed: overflowed };
    }),
  clearContacts: () => set({ contactLog: [], contactsOverflowed: false }),
  appendScriptLogs: (events, overflowed) =>
    set((s) => {
      const nextOverflowed = s.scriptLogsOverflowed || overflowed; // sticky: a dropped line stays dropped
      if (events.length === 0) {
        return nextOverflowed === s.scriptLogsOverflowed
          ? {}
          : { scriptLogsOverflowed: nextOverflowed };
      }
      // Chronological (the engine drains oldest→newest by seq); newest at the end, keep the last N.
      const scriptLogs = s.scriptLogs.concat(events).slice(-SCRIPT_LOG_LIMIT);
      return { scriptLogs, scriptLogsOverflowed: nextOverflowed };
    }),
  clearScriptLogs: () => set({ scriptLogs: [], scriptLogsOverflowed: false }),
  setPerfConfig: (perfConfig) => set({ perfConfig }),
  setUpscale: (upscale) => set({ upscale }),
  setTargetFpsMode: (targetFpsMode) => set({ targetFpsMode }),
  setFrameHistory: (frameHistory) => set({ frameHistory }),
  setPassTimings: (passTimings) => set({ passTimings }),
  setActiveAlarms: (activeAlarms) => set({ activeAlarms }),
  appendAlarmEvents: (events) =>
    set((s) => {
      if (events.length === 0) {
        return {};
      }
      const alarmLog = [...s.alarmLog, ...events];
      return { alarmLog: alarmLog.slice(Math.max(0, alarmLog.length - ALARM_LOG_LIMIT)) };
    }),
  setMetricsRangeSec: (metricsRangeSec) =>
    set(() => {
      try {
        localStorage.setItem(METRICS_RANGE_STORAGE_KEY, String(metricsRangeSec));
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { metricsRangeSec };
    }),
  setMetricsBucketMs: (metricsBucketMs) =>
    set(() => {
      try {
        localStorage.setItem(METRICS_BUCKET_STORAGE_KEY, String(metricsBucketMs));
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { metricsBucketMs };
    }),
  setMetricsRefreshMs: (metricsRefreshMs) =>
    set(() => {
      try {
        localStorage.setItem(METRICS_REFRESH_STORAGE_KEY, String(metricsRefreshMs));
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { metricsRefreshMs };
    }),
  setMetricsPaused: (metricsPaused) => set({ metricsPaused }),
  setCaptureState: (captureState) => set({ captureState }),
  setCaptureProgress: (current, total) => set({ captureProgress: { current, total } }),
  setCapture: (capture) => set({ capture }),
  setCaptureWindowFrames: (captureWindowFrames) =>
    set(() => {
      try {
        localStorage.setItem(CAPTURE_WINDOW_STORAGE_KEY, String(captureWindowFrames));
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { captureWindowFrames };
    }),
  setCaptureIncludeStats: (captureIncludeStats) =>
    set(() => {
      try {
        localStorage.setItem(CAPTURE_STATS_STORAGE_KEY, captureIncludeStats ? "1" : "0");
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { captureIncludeStats };
    }),
  setSelectedPass: (selectedPass) => set({ selectedPass }),
  // Rehydrate the persisted expand-state when a project (path) becomes current.
  setProject: (project) =>
    set({
      project,
      expandedIds: project?.path ? loadExpanded(project.path) : new Set<string>(),
    }),
  // Identity-stable (like setGizmo): a same-snapshot patch from the ~10 Hz poll returns {} so no
  // subscriber re-renders. Dedups on version + every progress field.
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
    // Flip to loading + stash the request immediately so the combined modal shows the loading view
    // before the first poll. Progress + completion are owned by useProjectLoadPoll, never awaited here.
    useEditorStore.getState().setProjectLoadCancelling(false);
    useEditorStore.getState().setProjectLoad({
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
      if (request.kind === "open") {
        await client.openProject(request.path ?? "");
      } else if (request.kind === "new") {
        await client.newProject(request.name ?? "", request.displayName ?? "");
      } else {
        await client.reloadProject();
      }
    } catch (err) {
      // A busy-loading rejection means a load is already in flight — the poll drives it; ignore.
      // Any other kick failure is terminal for this attempt.
      if (!isBusyLoading(err)) {
        useEditorStore.getState().setProjectLoad({ phase: "error", error: errorText(err) });
      }
    }
  },
  setPollRateHz: (pollRateHz) => set({ pollRateHz }),
  setUiFrameStats: (uiFrameRateHz, uiFrameMs) => set({ uiFrameRateHz, uiFrameMs }),
  resetSceneState: () => {
    // The catalog changed under us, so every cached thumbnail blob URL is stale.
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
      // Force the reconcile poll's version diff to fire on the next tick so the
      // hierarchy/inspector/assets/env all re-fetch against the loaded scene.
      sceneVersion: -1,
      selectionVersion: -1,
      // One-shot UI intent; never meaningful across a scene swap. The subrows
      // toggle survives — it is a view preference, not scene state.
      focusComponent: null,
      // Every captured prior value is stale against the loaded scene, and the rebuilt
      // viewTabs drop every non-scene tab; clear all per-tab history.
      historyByTab: {},
      catalogDrag: null,
    });
  },
  setEngineStatus: (patch) => set((s) => ({ engineStatus: { ...s.engineStatus, ...patch } })),
  setPhase: (phase, error) =>
    set((s) => ({
      engineStatus: {
        ...s.engineStatus,
        phase,
        error: phase === "error" ? error : undefined,
        running: phase === "ready" || phase === "error" ? s.engineStatus.running : phase !== "idle",
      },
    })),
  setDragActive: (dragActive) => set({ dragActive }),
  setCatalogDrag: (catalogDrag) => set({ catalogDrag }),
  setSceneEntitiesLive: (sceneEntitiesLive) => set({ sceneEntitiesLive }),
  // Keeps the object identity when the patch changes nothing, so the reconcile
  // poll confirming an unchanged gizmo doesn't re-render every subscriber.
  setGizmo: (patch) =>
    set((s) => {
      const gizmo = { ...s.gizmo, ...patch };
      return gizmo.op === s.gizmo.op &&
        gizmo.space === s.gizmo.space &&
        gizmo.preserveChildren === s.gizmo.preserveChildren
        ? {}
        : { gizmo };
    }),
  setPlayState: (playState) => set({ playState }),
  setViewportHidden: (viewportHidden) => set({ viewportHidden }),
  setNativeDialogOpen: (nativeDialogOpen) => set({ nativeDialogOpen }),
  setProjectModalOpen: (projectModalOpen) => set({ projectModalOpen }),
  setProjectLoadCancelling: (projectLoadCancelling) => set({ projectLoadCancelling }),
  setExportModalOpen: (exportModalOpen) => set({ exportModalOpen }),
  toggleComponentSubrows: () =>
    set((s) => {
      const showComponentSubrows = !s.showComponentSubrows;
      try {
        localStorage.setItem(SUBROWS_STORAGE_KEY, showComponentSubrows ? "1" : "0");
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { showComponentSubrows };
    }),
  toggleHideBones: () =>
    set((s) => {
      const hideBones = !s.hideBones;
      try {
        localStorage.setItem(HIDE_BONES_STORAGE_KEY, hideBones ? "1" : "0");
      } catch {
        // Storage unavailable; the preference is then session-only.
      }
      return { hideBones };
    }),
  setAssetSort: (assetSort) => {
    try {
      localStorage.setItem(ASSET_SORT_STORAGE_KEY, assetSort);
    } catch {
      // Storage unavailable; the preference is then session-only.
    }
    set({ assetSort });
  },
  setStoreSelected: (storeSelected) => {
    try {
      if (storeSelected === null) localStorage.removeItem(STORE_SELECTED_STORAGE_KEY);
      else localStorage.setItem(STORE_SELECTED_STORAGE_KEY, storeSelected);
    } catch {
      // Storage unavailable; the selection is then session-only.
    }
    set({ storeSelected });
  },
  setStoreQuery: ({ text, kind }) => {
    try {
      localStorage.setItem(STORE_SEARCH_TEXT_STORAGE_KEY, text);
      if (kind === null) localStorage.removeItem(STORE_KIND_STORAGE_KEY);
      else localStorage.setItem(STORE_KIND_STORAGE_KEY, kind);
    } catch {
      // Storage unavailable; the query is then session-only.
    }
    set({ storeSearchText: text, storeKind: kind });
  },
  setStoreSession: (storeSession) => set({ storeSession }),
  setStoreResults: (storeResults, session) => set({ storeResults, storeResultsSession: session }),
  appendStoreResults: (results, session) =>
    set((s) => ({ storeResults: [...s.storeResults, ...results], storeResultsSession: session })),
  setStoreExhausted: (storeExhausted) => set({ storeExhausted }),
  setStoreScrollTop: (storeScrollTop) => set({ storeScrollTop }),
  resetStoreBrowse: () =>
    set({
      storeSession: null,
      storeResults: [],
      storeResultsSession: null,
      storeExhausted: false,
      storeScrollTop: 0,
    }),
  setFocusComponent: (focusComponent) => set({ focusComponent }),
  setKeyBinding: (id, value) =>
    set((s) => {
      const keyBindings = { ...s.keyBindings };
      if (value === COMMANDS_BY_ID[id].default) {
        delete keyBindings[id];
      } else {
        keyBindings[id] = value;
      }
      persistKeyBindings(keyBindings);
      return { keyBindings };
    }),
  resetKeyBinding: (id) =>
    set((s) => {
      if (!(id in s.keyBindings)) {
        return {};
      }
      const keyBindings = { ...s.keyBindings };
      delete keyBindings[id];
      persistKeyBindings(keyBindings);
      return { keyBindings };
    }),
  resetAllKeyBindings: () => {
    persistKeyBindings({});
    set({ keyBindings: {} });
  },
  hydrateKeyBindings: (overrides) =>
    set({
      keyBindings: Object.fromEntries(Object.entries(overrides).filter(([id]) => isCommandId(id))),
    }),
  setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
  setDevMode: (devMode) =>
    set(() => {
      try {
        localStorage.setItem(DEV_MODE_STORAGE_KEY, devMode ? "1" : "0");
      } catch {
        // Storage unavailable; the flag is then session-only.
      }
      return { devMode };
    }),
  pushEdit: (edit, tabId) =>
    set((s) => {
      if (s.historyReplaying) {
        return {};
      }
      const id = tabId ?? s.activeViewTabId;
      if (!isHistoryTab(s.viewTabs, id)) {
        return {};
      }
      // The scene tab edits a throwaway duplicate during play; those edits are discarded
      // on Stop, so they never enter the authored-scene history (which is preserved, not
      // cleared, across a play session).
      if (id === "scene" && s.playState !== "edit") {
        return {};
      }
      const history = s.historyByTab[id] ?? emptyHistory();
      return { historyByTab: { ...s.historyByTab, [id]: appendEdit(history, edit) } };
    }),
  undo: (tabId) => replayHistory("undo", tabId),
  redo: (tabId) => replayHistory("redo", tabId),
  beginEdit: ({ prior, selectionId }) => ({
    commit: (final, build) => {
      const edit = build(prior, final);
      if (selectionId !== undefined && edit.selectionId === undefined) {
        edit.selectionId = selectionId;
      }
      useEditorStore.getState().pushEdit(edit);
    },
  }),
  clearTabHistory: (tabId) =>
    set((s) => {
      if (!(tabId in s.historyByTab)) {
        return {};
      }
      const historyByTab = { ...s.historyByTab };
      delete historyByTab[tabId];
      return { historyByTab };
    }),
  clearSceneHistory: () => set((s) => ({ historyByTab: sceneHistoryCleared(s) })),
}));

/// True when a panel is open in its island's tree (in any leaf, active or not). The
/// metrics poll keys on *open*, not *active*, so a hidden-but-mounted Stats panel keeps
/// polling. Use as a selector: `useEditorStore((s) => isPanelOpen(s, "stats"))`.
export function isPanelOpen(state: EditorState, id: DockPanelId): boolean {
  return isPanelOpenIn(state.dockLayouts[panelKind(id)], id);
}

// The one viewport re-glue path for every dock mutation: a `dockLayouts` identity change
// fires a forced layout-settled on the next frame, so no open/close/move/drop/reset/load
// call site can forget to re-commit the subsurface bounds. Over-emitting is harmless —
// the inactive island's host sits at 0×0 and computeBounds skips degenerate rects.
let lastDockLayouts = useEditorStore.getState().dockLayouts;
useEditorStore.subscribe((s) => {
  if (s.dockLayouts !== lastDockLayouts) {
    lastDockLayouts = s.dockLayouts;
    requestAnimationFrame(() => emitLayoutSettled({ force: true }));
  }
});

// Debounced dock persistence: any change to the trees or last-location memory writes the
// per-project key ~300 ms later (coalescing rapid drags). No-op without a loaded project.
let lastPersistedDock = useEditorStore.getState().dockLayouts;
let lastPersistedLoc = useEditorStore.getState().lastLocation;
let dockPersistTimer: ReturnType<typeof setTimeout> | undefined;
useEditorStore.subscribe((s) => {
  if (s.dockLayouts === lastPersistedDock && s.lastLocation === lastPersistedLoc) {
    return;
  }
  lastPersistedDock = s.dockLayouts;
  lastPersistedLoc = s.lastLocation;
  clearTimeout(dockPersistTimer);
  dockPersistTimer = setTimeout(() => {
    const state = useEditorStore.getState();
    persistDockLayouts(state.project?.path, state.dockLayouts, state.lastLocation);
  }, 300);
});

/// The tab kinds whose edits feed an undo history — the editable surfaces. A viewer
/// kind (`flamegraph`/`imageViewer`) or any future kind defaults out, so only an
/// editable tab ever records; the asset editor is in by design.
const HISTORY_TAB_KINDS = new Set<ViewTab["kind"]>(["scene", "materialGraph", "assetEditor"]);

/// True when a tab id resolves to a live, editable tab that records history.
function isHistoryTab(viewTabs: ViewTab[], id: string): boolean {
  const tab = viewTabs.find((t) => t.id === id);
  return tab !== undefined && HISTORY_TAB_KINDS.has(tab.kind);
}

/// The historyByTab with the scene history and every orphaned non-scene history dropped,
/// keeping live non-scene (e.g. material-graph) tabs. Shared by the scene-swap
/// invalidation and the play-enter transition.
function sceneHistoryCleared(s: EditorState): Record<string, TabHistory> {
  const live = new Set(s.viewTabs.map((tab) => tab.id));
  const next: Record<string, TabHistory> = {};
  for (const [id, history] of Object.entries(s.historyByTab)) {
    if (id !== "scene" && live.has(id)) {
      next[id] = history;
    }
  }
  return next;
}

/// Resolve and replay the next undo/redo on a tab's history. Holds `historyReplaying`
/// over the in-flight inverse so no edit site records the replay, moves the entry
/// between stacks regardless of success (a half-applied replay is repaired by the
/// reconcile poll), and surfaces a rejection through the toast path.
async function replayHistory(direction: "undo" | "redo", tabId?: string): Promise<void> {
  const store = useEditorStore.getState();
  const id = tabId ?? store.activeViewTabId;
  // Undo/redo edits the authored scene; while playing, the scene tab targets the
  // throwaway play duplicate, so it is paused until Stop (the history is untouched).
  if (id === "scene" && store.playState !== "edit") {
    return;
  }
  const history = store.historyByTab[id];
  if (history === undefined) {
    return;
  }
  const taken = direction === "undo" ? takeUndo(history) : takeRedo(history);
  if (taken === null) {
    return;
  }
  useEditorStore.setState({ historyReplaying: true });
  try {
    await (direction === "undo" ? taken.edit.undo() : taken.edit.redo());
  } catch (err) {
    notifyError(errorText(err));
    console.error(`${direction} rejected:`, err);
  } finally {
    useEditorStore.setState((s) => ({
      historyByTab: { ...s.historyByTab, [id]: taken.next },
      historyReplaying: false,
    }));
    restoreSelectionContext(id, taken.edit);
  }
}

/// After a scene-tab replay, land selection back on the entity the edit touched so
/// the gizmo follows it. Only the scene tab carries entity selectionIds; other tabs'
/// ids are not entities, so they are left untouched.
function restoreSelectionContext(tabId: string, edit: UndoableEdit): void {
  if (tabId !== "scene" || edit.selectionId === undefined) {
    return;
  }
  useEditorStore.getState().setSelectedId(edit.selectionId);
  void client.selectEntity(edit.selectionId).catch(() => {});
}

/// Record an entity-creation edit (undo-only: undo destroys the entity; redo is
/// dropped, since re-creation would mint a new id). Shared by every creation site.
export function recordEntityCreation(entityId: string, label: string): void {
  useEditorStore.getState().pushEdit(
    {
      label,
      redoable: false,
      selectionId: entityId,
      undo: () => client.destroyEntity(entityId),
      redo: () => Promise.resolve(),
    },
    "scene",
  );
}

/// Fire-and-forget settings write; the in-memory state stays applied on rejection
/// (the global shortcut hook has no panel to flash, so the failure goes to the console).
function persistKeyBindings(keyBindings: Record<string, string>): void {
  void client.saveEditorSettings({ keyBindings }).catch((err: unknown) => {
    console.error("save editor settings rejected:", err);
  });
}

/// Hydrate the keybinding overrides from appdata/settings.json (called once at app
/// start). A missing or unreadable file leaves the registry defaults active.
export async function loadEditorSettings(): Promise<void> {
  try {
    const settings = await client.loadEditorSettings();
    useEditorStore.getState().hydrateKeyBindings(settings.keyBindings ?? {});
  } catch {
    // Defaults stay active.
  }
}

/// Run a native file-dialog thunk (`open`/`save`) under the app-side dialog lock:
/// no-op if one is already open (the re-entry guard that stops stacked pickers),
/// otherwise hold `nativeDialogOpen` for the dialog's lifetime so the controls that
/// spawn dialogs grey out. The lock covers only the dialog, not the engine work that
/// follows it. Returns the thunk's result, or null when skipped by the guard.
export async function withNativeDialog<T>(fn: () => Promise<T>): Promise<T | null> {
  const store = useEditorStore.getState();
  if (store.nativeDialogOpen) {
    return null;
  }
  store.setNativeDialogOpen(true);
  try {
    return await fn();
  } finally {
    useEditorStore.getState().setNativeDialogOpen(false);
  }
}

/// One outliner node: an entity plus its resolved children. The tree is built
/// client-side from the flat `entities` slice; the engine ships only `parentId`.
export interface TreeNode {
  entity: EntityListEntry;
  children: TreeNode[];
}

/// Group the flat entity list into a forest by parentId. Absent/`"0"` parentId,
/// an unknown parent id, or a self-reference all land the entry at the root
/// (defensive: the engine roots dangling parents on load, and the client must
/// never loop). Sibling order preserves the engine's array order (unordered v1).
export function buildTree(entities: EntityListEntry[]): TreeNode[] {
  const nodes = new Map<string, TreeNode>();
  for (const entity of entities) {
    nodes.set(entity.id, { entity, children: [] });
  }
  const roots: TreeNode[] = [];
  for (const entity of entities) {
    const node = nodes.get(entity.id)!;
    const parentId = entity.parentId;
    const parent =
      parentId && parentId !== "0" && parentId !== entity.id ? nodes.get(parentId) : undefined;
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return roots;
}

/// Expand-state persistence, keyed by project path (plain UI state, like the
/// thumbnail cache kept out of Zustand): survives reloads, never the poll's concern.
const EXPANDED_STORAGE_PREFIX = "saffron.expandedIds:";

function persistExpanded(ids: Set<string>): void {
  const path = useEditorStore.getState().project?.path;
  if (!path) {
    return;
  }
  try {
    localStorage.setItem(EXPANDED_STORAGE_PREFIX + path, JSON.stringify([...ids]));
  } catch {
    // Storage may be unavailable (private mode); expansion is then session-only.
  }
}

function loadExpanded(path: string): Set<string> {
  try {
    const raw = localStorage.getItem(EXPANDED_STORAGE_PREFIX + path);
    if (raw) {
      return new Set(JSON.parse(raw) as string[]);
    }
  } catch {
    // Fall through to empty.
  }
  return new Set<string>();
}

/// Dock-layout persistence: both island trees + last-location memory under one per-project
/// key, written debounced on any dock mutation, validated on load. No-op without a project
/// (session-only before one loads).
const DOCK_LAYOUT_STORAGE_PREFIX = "saffron.layout.dock:";

interface PersistedDockLayouts {
  version: 1;
  layouts: Record<DockSpaceKind, DockLayout>;
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>;
}

export function persistDockLayouts(
  path: string | undefined,
  layouts: Record<DockSpaceKind, DockLayout>,
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>,
): void {
  if (!path) {
    return;
  }
  try {
    const payload: PersistedDockLayouts = { version: 1, layouts, lastLocation };
    localStorage.setItem(DOCK_LAYOUT_STORAGE_PREFIX + path, JSON.stringify(payload));
  } catch {
    // Storage may be unavailable (private mode); the layout is then session-only.
  }
}

function loadDockLayouts(path: string | undefined): PersistedDockLayouts | null {
  if (!path) {
    return null;
  }
  try {
    const raw = localStorage.getItem(DOCK_LAYOUT_STORAGE_PREFIX + path);
    if (!raw) {
      return null;
    }
    const parsed = JSON.parse(raw) as PersistedDockLayouts;
    if (parsed?.version !== 1 || !parsed.layouts) {
      return null;
    }
    return parsed;
  } catch {
    return null;
  }
}

/// The component-subrows toggle is a view preference (one key, unlike the
/// per-project expand-state); default off keeps the outliner clean.
const SUBROWS_STORAGE_KEY = "saffron.showComponentSubrows";

function loadShowSubrows(): boolean {
  try {
    return localStorage.getItem(SUBROWS_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

const HIDE_BONES_STORAGE_KEY = "saffron.hideBones";

function loadHideBones(): boolean {
  try {
    // Default hidden: skeleton bones belong in the asset/rig editor, not the scene
    // outliner (the UE5/Godot convention). An explicit "0" opts back into showing them.
    const raw = localStorage.getItem(HIDE_BONES_STORAGE_KEY);
    return raw === null ? true : raw === "1";
  } catch {
    return true;
  }
}

const ASSET_SORT_STORAGE_KEY = "saffron.assetSort";
const ASSET_SORT_MODES: readonly AssetSortMode[] = [
  "name-asc",
  "name-desc",
  "created-desc",
  "created-asc",
];

function loadAssetSort(): AssetSortMode {
  try {
    const raw = localStorage.getItem(ASSET_SORT_STORAGE_KEY);
    return ASSET_SORT_MODES.includes(raw as AssetSortMode) ? (raw as AssetSortMode) : "name-asc";
  } catch {
    return "name-asc";
  }
}

/// The Asset Store's selected store + last query persist app-wide (not per-project) so reopening
/// the Store returns you to where you left off; the store id is reconciled against the project's
/// enabled set on load.
const STORE_SELECTED_STORAGE_KEY = "saffron.store.selected";
const STORE_SEARCH_TEXT_STORAGE_KEY = "saffron.store.searchText";
const STORE_KIND_STORAGE_KEY = "saffron.store.kind";
const STORE_KINDS: readonly StoreKind[] = ["model", "hdri", "material", "texture"];

function loadStoreSelected(): string | null {
  try {
    return localStorage.getItem(STORE_SELECTED_STORAGE_KEY);
  } catch {
    return null;
  }
}

function loadStoreSearchText(): string {
  try {
    return localStorage.getItem(STORE_SEARCH_TEXT_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function loadStoreKind(): StoreKind | null {
  try {
    const raw = localStorage.getItem(STORE_KIND_STORAGE_KEY);
    return STORE_KINDS.includes(raw as StoreKind) ? (raw as StoreKind) : null;
  } catch {
    return null;
  }
}

/// Developer mode persists app-wide (one key, not per-project), default off.
/// `VITE_SAFFRON_DEV_MODE=1` (set by `make run-debug`) forces it on for the session
/// without touching the persisted flag.
const DEV_MODE_STORAGE_KEY = "saffron.devMode";

function loadDevMode(): boolean {
  if (import.meta.env.VITE_SAFFRON_DEV_MODE === "1") {
    return true;
  }
  try {
    return localStorage.getItem(DEV_MODE_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

/// The frame-time graph time range (seconds); default 30 s.
function loadMetricsRangeSec(): number {
  try {
    const raw = localStorage.getItem(METRICS_RANGE_STORAGE_KEY);
    if (raw !== null) {
      const value = Number(raw);
      if (Number.isFinite(value) && value > 0) {
        return value;
      }
    }
  } catch {
    // Fall through to the default.
  }
  return 30;
}

/// The graph bucket (group-by) interval (ms); default 250. Clamped to a sane [10, 5000].
function loadMetricsBucketMs(): number {
  try {
    const raw = localStorage.getItem(METRICS_BUCKET_STORAGE_KEY);
    if (raw !== null) {
      const value = Number(raw);
      if (Number.isFinite(value) && value >= 10 && value <= 5000) {
        return value;
      }
    }
  } catch {
    // Fall through to the default.
  }
  return 250;
}

/// The metrics fetch interval (ms); default 1000 (1/s).
function loadMetricsRefreshMs(): number {
  try {
    const raw = localStorage.getItem(METRICS_REFRESH_STORAGE_KEY);
    if (raw !== null) {
      const value = Number(raw);
      if (Number.isFinite(value) && value >= 100) {
        return value;
      }
    }
  } catch {
    // Fall through to the default.
  }
  return 1000;
}

/// The capture window length (frames); default 1 (single-frame snapshot). Presets are
/// 1/8/64/256, clamped to the engine's [1, 256] capture cap.
function loadCaptureWindowFrames(): number {
  try {
    const raw = localStorage.getItem(CAPTURE_WINDOW_STORAGE_KEY);
    if (raw !== null) {
      const value = Number(raw);
      if (Number.isFinite(value) && value >= 1 && value <= 256) {
        return Math.floor(value);
      }
    }
  } catch {
    // Fall through to the default.
  }
  return 1;
}

/// Whether captures request pipeline statistics; default off (the heaviest mode).
function loadCaptureIncludeStats(): boolean {
  try {
    return localStorage.getItem(CAPTURE_STATS_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

/// The outliner's bone filter: drop rows flagged `bone` and re-anchor every surviving
/// entity whose ancestry passes through bones to its nearest visible ancestor (walks
/// are bounded so corrupt data cannot loop). Pure — used before buildTree.
export function reanchorPastBones(entities: EntityListEntry[]): EntityListEntry[] {
  const byId = new Map(entities.map((e) => [e.id, e]));
  return entities
    .filter((e) => !e.bone)
    .map((e) => {
      let parent = e.parentId ? byId.get(e.parentId) : undefined;
      for (let steps = 0; parent?.bone && steps <= entities.length; steps++) {
        parent = parent.parentId ? byId.get(parent.parentId) : undefined;
      }
      const parentId = parent?.id;
      return parentId === e.parentId ? e : { ...e, parentId };
    });
}

const FAST_RECONCILE_INTERVAL_MS = 50; // cheap state lane, target ~20 Hz
const WATCHDOG_INTERVAL_MS = 1000;
// The metrics lane wakes on this base tick and fetches when `metricsRefreshMs` has elapsed
// (and not paused), so a rate or pause change applies within one tick instead of waiting
// out a long interval.
const METRICS_BASE_TICK_MS = 100;

/// Start the focus-gated reconcile loops. Cheap interactive state runs frequently;
/// heavier scene/entity/inspect refreshes only run when versions change and never
/// block the next cheap tick.
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
  // Alarm cursor (Last-Event-ID): only advances, so a missed poll just catches up.
  let alarmSince = 0;
  // Script-error cursor, same protocol; the toast dedup resets per play session.
  let scriptErrorSince = 0;
  let lastPlayStateForScripts: PlayState = "edit";
  // Physics contact cursor; reset on each fresh play session (the engine ring restarts).
  let contactsSince = 0;
  let lastPlayStateForPhysics: PlayState = "edit";
  let scriptLogsSince = 0;
  let lastPlayStateForScriptLogs: PlayState = "edit";
  let pendingRefresh: {
    selectedId: string | null;
    sceneChanged: boolean;
    selectionChanged: boolean;
    sceneVersion: number;
    previousSceneVersion: number;
  } | null = null;

  let knownSceneVersion = -1;
  let knownSelectionVersion = -1;
  let knownSelectedId: string | null = null;
  let knownPlayVersion = -1;
  let knownAnimationVersion = -1;
  let animationInFlight = false;
  // Tracks the scene-view "live" gate so a false→true transition (a view switch resolving) forces
  // a heavy refetch: a tick that ran while it was false fetched the hierarchy but dropped it, yet
  // still advanced the version — so without this the entities never re-fetch. Init true (store
  // default) so a steady session never spuriously re-fetches.
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

  /// The crash watchdog runs once the engine is live (socket bound) and stops once
  /// it has died or never started. This includes the starting/attaching window —
  /// the gap between socket-up and the viewport attach — so a child that binds its
  /// socket and then exits before attaching is still caught (the ViewportPanel's
  /// phase flip to ready never fires in that case).
  const engineMayBeAlive = (): boolean => {
    const phase = useEditorStore.getState().engineStatus.phase;
    return phase !== "idle" && phase !== "error" && document.hasFocus();
  };

  const refreshHeavyState = (request: {
    selectedId: string | null;
    sceneChanged: boolean;
    selectionChanged: boolean;
    sceneVersion: number;
    previousSceneVersion: number;
  }): void => {
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
            invalidateThumbnails();
            // A backwards version step is a different scene loaded under us; the captured
            // prior values are stale, so drop the scene history (graph tabs survive).
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
          // Only write the Scene hierarchy when the scene view is the confirmed active view — never the
          // preview scene's entities (asset tab active), and never mid-switch (sceneEntitiesLive false
          // until set-active-view resolves), so the asset→scene switch shows no stale preview-entity frame.
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
        // Freeze the inspector on the authored selection while the preview view is active / mid-switch,
        // so it never flashes the previewed entity's components when the scene tab reappears.
        const s = useEditorStore.getState();
        if (!s.dragActive && s.sceneEntitiesLive) {
          s.setComponentsBySelected(inspected);
        }
      } catch {
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

  /// Refresh the selected rig's animation state + clips for the TimelinePanel. Driven by
  /// the animationVersion gate (a play/seek/pause bumped the player) or a selection change.
  /// `get-animation-state` rejects when the entity is not an animation player — that is the
  /// "not rigged / nothing playing yet" case, so clear the slice silently (no toast).
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
      } finally {
        animationInFlight = false;
      }
    })();
  };

  /// The perf-telemetry lane (decoupled from the cheap state tick). Alarms are drained
  /// every tick so the badge stays live with the panel closed; the heavier frame-history
  /// and per-pass reads only run while the Stats dashboard is open.
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

      // Contained script errors pause play engine-side; surface the traceback here.
      // Drained while play is active (a pause keeps the state visible until stop).
      const playState = useEditorStore.getState().playState;
      if (playState !== "edit") {
        if (lastPlayStateForScripts === "edit") {
          resetScriptErrorToasts(); // fresh session: the engine cleared its ring
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

      // Fetch the shared config once; the target-FPS dropdown refreshes it on write.
      if (useEditorStore.getState().perfConfig === null) {
        const config = await client.getPerfConfig();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setPerfConfig(config);
      }

      // Seed the TAAU upscale surface (ratio / dynamic / extents); the Resolution control
      // refreshes it on write.
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

      // The Render panel owns the debug-overlay toggles; poll them only while it is open so
      // an external `sa set-debug-overlays` reflects in the panel.
      if (isPanelOpen(useEditorStore.getState(), "render")) {
        const overlays = await client.getDebugOverlays();
        if (stopped) {
          return;
        }
        useEditorStore.getState().setDebugOverlays(overlays);
      }

      // The Physics panel: live world stats + the contact/trigger feed. Polled ONLY while the
      // panel is open AND play is active (the world exists only in play) — a closed panel or
      // Edit adds zero round-trips. The contact cursor + feed reset on each fresh play session.
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

      // The Script Logs panel: sa.log output. Polled ONLY while the panel is open AND play is active
      // (scripts run only in play); the cursor + buffer reset on each fresh play, retained after Stop.
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
      .engineAlive()
      .then((alive) => {
        if (!alive && !stopped) {
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

      // A project load / scene swap stamps sceneVersion (and selectionVersion) to -1 via
      // resetSceneState to demand a fresh heavy refetch. Honor it by dropping our cached version
      // diff, so the hierarchy/inspector/assets/env re-fetch even if an earlier tick already
      // recorded the engine's post-load version before `complete()` cleared the store — the
      // load-completion race the closure-cached diff would otherwise lose.
      if (live.sceneVersion === -1) {
        knownSceneVersion = -1;
      }
      if (live.selectionVersion === -1) {
        knownSelectionVersion = -1;
        knownSelectedId = null;
      }
      // The scene view just became live again (a view switch resolved — e.g. the startup modal
      // closed and unparked the viewport). A tick during the not-live window fetched the hierarchy
      // but dropped it (the `sceneEntitiesLive` gate below), so force a refetch now.
      if (live.sceneEntitiesLive && !prevSceneEntitiesLive) {
        knownSceneVersion = -1;
        knownSelectionVersion = -1;
        knownSelectedId = null;
      }
      prevSceneEntitiesLive = live.sceneEntitiesLive;

      live.setRenderStats(stats);
      // Reflect the engine's gizmo state (so an external `sa set-gizmo` shows up
      // in the Topbar); Topbar clicks set this optimistically, the poll confirms.
      live.setGizmo(gizmo);

      // Mirror play state from the engine on a version change (a shell `sa play`
      // shows up here too); Topbar clicks set it optimistically, the poll confirms.
      // The stop-driven sceneVersion bump rides the refreshHeavyState path below,
      // snapping the tree/inspector back to the authored scene.
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
      // Only mirror the selection into the Scene dock when the scene view is the confirmed active view —
      // never the preview scene's selection (asset tab active / mid-switch), so the inspector doesn't
      // flash the previewed entity on the asset→scene switch. The version stamps above still advance so
      // the next post-switch poll picks up the authored selection.
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

      // Animation gate, parallel to the playVersion gate: refetch the selected rig's
      // playhead/clips when the engine bumps animationVersion (a play/seek/pause, here
      // or via the `sa` CLI) or when the selection changes to a different entity. The
      // returned `time` drives the TimelinePanel playhead (canvas, never React state).
      if (selection.animationVersion !== knownAnimationVersion || selectionChanged) {
        refreshAnimation(nextSelectedId);
      }

      knownSceneVersion = selection.sceneVersion;
      knownSelectionVersion = selection.selectionVersion;
      knownSelectedId = nextSelectedId;
      knownPlayVersion = selection.playVersion;
      knownAnimationVersion = selection.animationVersion;
    } catch {
    } finally {
      fastInFlight = false;
      schedule();
    }
  }

  // The metrics lane: a base tick that fetches only when the configured interval has
  // elapsed and the lane is not paused — so the refresh-rate and pause controls take
  // effect within one base tick.
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

/// Client-side thumbnail cache: assetId → { blob URL, the px size it was fetched
/// at }. Kept at module scope (NOT in Zustand) because it holds blob URLs that
/// must survive re-renders without provoking store churn, and a single fetch must
/// be shared by all tiles/pickers/viewers asking for the same asset.
///
/// A get-thumbnail call is a GPU→CPU readback + PNG encode (off the present path),
/// so we fetch lazily on first view, dedupe concurrent requests, and re-use a
/// cached URL whenever the cached image is at least as large as the requested
/// size (a 128 grid tile and a 16-from-64 combo swatch both read the 64/128 blob).
interface ThumbnailCacheEntry {
  url: string;
  size: number;
}

const thumbnailCache = new Map<string, ThumbnailCacheEntry>();
/// In-flight fetches keyed by assetId, so N tiles mounting at once issue ONE
/// get-thumbnail per asset (the rest await the same promise).
const thumbnailInflight = new Map<string, Promise<string>>();

export function base64ToBlob(b64: string, mime = "image/png"): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return new Blob([bytes], { type: mime });
}

export function getCachedThumbnailUrl(assetId: string, size: number): string | null {
  const cached = thumbnailCache.get(assetId);
  return cached && cached.size >= size ? cached.url : null;
}

/// Resolve a blob URL for an asset's thumbnail at (at least) `size` px. Cache hit
/// (same or larger cached size) returns immediately; a miss fetches once over the
/// socket, decodes the base64 PNG, and stores the object URL. A `pending` reply means
/// the engine is generating it on a worker thread (cold cache) — re-request with
/// backoff until it lands (a pure disk-cache hit). Rejects (engine error) to let the
/// caller fall back to a type icon. The in-flight map keeps one retry loop per asset.
export async function getThumbnailUrl(assetId: string, size: number): Promise<string> {
  const cached = getCachedThumbnailUrl(assetId, size);
  if (cached) {
    return cached;
  }
  const inflight = thumbnailInflight.get(assetId);
  if (inflight) {
    return inflight;
  }
  const promise = (async (): Promise<string> => {
    let delayMs = 60;
    for (;;) {
      const thumb = await client.getThumbnail(assetId, size);
      if (!thumb.pending) {
        const url = URL.createObjectURL(base64ToBlob(thumb.base64));
        const prev = thumbnailCache.get(assetId);
        if (prev) {
          URL.revokeObjectURL(prev.url);
        }
        thumbnailCache.set(assetId, { url, size });
        return url;
      }
      await new Promise((resolve) => setTimeout(resolve, delayMs));
      delayMs = Math.min(delayMs * 2, 1000); // exponential backoff, capped at 1s
    }
  })();
  thumbnailInflight.set(assetId, promise);
  try {
    return await promise;
  } finally {
    thumbnailInflight.delete(assetId);
  }
}

/// Revoke every cached blob URL and clear the cache. Called on a scene/project
/// load (the catalog changed, so cached images are stale) — see the reconcile
/// poll's scene-reset branch.
export function invalidateThumbnails(): void {
  for (const entry of thumbnailCache.values()) {
    URL.revokeObjectURL(entry.url);
  }
  thumbnailCache.clear();
}
