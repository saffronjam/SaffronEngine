import type { CommandId } from "../../lib/keybindings";
import type { TabHistory, UndoableEdit } from "../../lib/undo";
import type { ProjectInfo } from "../../control/client";
import type { DockLayout, DockNodeId, DockPanelId, DockSpaceKind, DropTarget } from "../dockLayout";
import type {
  ActiveAlarmDto,
  AlarmEventDto,
  AnimationClipDto,
  AnimationStateResult,
  AssetEntry,
  ContactEventDto,
  DebugOverlaysResult,
  EntityListEntry,
  Environment,
  FieldChannelDto,
  FrameHistoryDto,
  GizmoState,
  InspectResult,
  PerfConfigDto,
  PhysicsBodyDto,
  PhysicsStateResult,
  ProfileCaptureDto,
  RenderPassTimingsDto,
  RenderStats,
  ScriptLogDto,
  UpscaleDto,
  VegetationLayerOperatorDto,
  WorldBoundsDto,
} from "../../protocol";
import type { StoreKind, StoreResult } from "../../storefront/types";

/// Host/renderer process lifecycle.
export type EnginePhase = "idle" | "starting" | "attaching" | "ready" | "error";

/// Project-load lifecycle — a distinct axis from `EnginePhase`: a reload runs while the engine
/// stays `ready`, and a bootstrap load lands before it is.
export type ProjectLoadPhase = "idle" | "loading" | "ready" | "error";

/// What a `startProjectLoad` kicks off, stashed so a failed load's Retry can replay it.
export interface ProjectLoadRequest {
  kind: "open" | "new" | "reload";
  path?: string;
  name?: string;
  displayName?: string;
}

/// Live project-load progress, fed by the `project-status` poll. `total === 0` means the current
/// stage is indeterminate; `version` is the DTO's monotonic stamp the poll dedups on.
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
  /// The load reached 100% and is settling: the bar sits full while the status text fades out.
  finalizing?: boolean;
}

/// The profiler capture lifecycle, mirrored from the engine's recorder.
export type CaptureState = "idle" | "arming" | "recording" | "ready";
export type VegetationAssetType = "plant" | "biome" | "vegetation-map";

export type VegetationTool =
  | "select"
  | "lasso"
  | "paint"
  | "erase"
  | "density"
  | "reapply"
  | "single"
  | "fill"
  | "spline"
  | "volume"
  | "exclude"
  | "pin"
  | "promote";

/// Vegetation brush parameters (metres for radius/spacing; falloff 0..1). `projection` picks how a
/// stroke sample lands on the world: the camera ray's hit, or a straight-down cast above it.
/// `maxSlopeDeg` drops samples whose surface tilts past the limit (90 = no filter). `density` is the
/// 0..1 level the Density and Fill tools drive texels toward, and the Volume/Exclude shapes' weight.
export interface VegetationBrush {
  radius: number;
  falloff: number;
  spacing: number;
  projection: "view" | "down";
  maxSlopeDeg: number;
  density: number;
}

/// Which chunk payload slot an authored layer's tiles live in: ordinary fields, or the signed
/// blocker set the Exclude tool writes.
export type VegetationPaintSlot = "field" | "blocker";

/// The authored layer a vegetation brush stroke targets. `channel` is the layer operator's field
/// channel for paintable layers; null means the layer is selectable but takes no strokes.
export interface VegetationPaintTarget {
  map: string;
  layer: string;
  /// The layer operator's kind, which decides the gestures the layer accepts.
  operator: VegetationLayerOperatorDto["kind"];
  channel: FieldChannelDto | null;
  slot: VegetationPaintSlot;
  chunkLevel: number;
  locked: boolean;
}

/// One picked world position in metres, the unit every vegetation gesture samples in.
export type VegetationPoint = [number, number, number];

export type ViewTab =
  | { id: "scene"; kind: "scene"; title: "Scene"; closable: false }
  | { id: "flamegraph"; kind: "flamegraph"; title: "Flame graph"; closable: true }
  | { id: "store"; kind: "store"; title: "Store"; closable: true }
  | { id: string; kind: "materialGraph"; materialId: string; title: string; closable: true }
  // Keyed by the resolved model (the container uuid), not the clicked asset, so a model, its mesh,
  // and any of its clips share ONE tab — the engine allows only one previewScene.
  | { id: string; kind: "assetEditor"; assetId: string; title: string; closable: true }
  | {
      id: string;
      kind: "imageViewer";
      assetId: string;
      title: string;
      assetType: AssetEntry["type"];
      closable: true;
    };

/// Cross-tab navigation history with browser semantics: navigating truncates the forward tail, and
/// back/forward reopen a closed tab from its stored snapshot. Distinct from `historyByTab`.
export interface TabNavHistory {
  entries: ViewTab[];
  cursor: number;
}

export interface EngineStatus {
  running: boolean;
  phase: EnginePhase;
  error?: string;
}

export type PlayState = "edit" | "playing" | "paused";

/// The Assets-grid shift anchor: the last clicked tile, folder or asset.
export type AssetSelectionAnchor = { kind: "asset" | "folder"; key: string } | null;

/// Name uses a natural (numeric) locale compare; created uses each asset's `createdAt` file
/// timestamp. Folders stay alphabetical regardless.
export type AssetSortMode = "name-asc" | "name-desc" | "created-desc" | "created-asc";

export interface CatalogDragPayload {
  assetIds: string[];
  folderPaths: string[];
}

/// One Assets-grid tile in the body's render order — the coordinate space for shift-range selection.
export interface AssetGridItem {
  kind: "asset" | "folder";
  key: string;
}

/// The Assets panel's folder back/forward, exposed to the central mouse dispatcher.
export interface AssetsFolderNav {
  back(): void;
  forward(): void;
}

/// One outliner node: an entity plus its resolved children. The tree is built client-side from the
/// flat `entities` slice; the engine ships only `parentId`.
export interface TreeNode {
  entity: EntityListEntry;
  children: TreeNode[];
}

/// The authored scene: entity list, selection, outliner expansion, and the inspected components.
export interface SceneSlice {
  entities: EntityListEntry[];
  selectedId: string | null;
  /// The material asset open in the Material editor panel.
  selectedMaterialId: string | null;
  /// Hierarchy rows whose children are shown. Outside the sceneVersion/selectionVersion keying so a
  /// scene mutation never collapses the tree; `setEntities` prunes ids that left the scene.
  expandedIds: Set<string>;
  sceneVersion: number;
  selectionVersion: number;
  componentsBySelected: InspectResult | null;
  /// One-shot "jump the Inspector to this component" signal set by a subrow click.
  focusComponent: string | null;
  environment: Environment | null;
  /// The selected rig's animation player state, or null when the selection is not a player.
  animationState: AnimationStateResult | null;
  animationClips: AnimationClipDto[];
  gizmo: GizmoState;
  /// Mirrored from the engine. The gizmo is hidden and save/load locked while not "edit".
  playState: PlayState;
  dragActive: boolean;
  /// True only when the engine is confirmed rendering the authored scene view, so the reconcile poll
  /// may write `entities`. Held false while an asset-editor tab is active and through a view switch
  /// until set-active-view resolves.
  sceneEntitiesLive: boolean;

  setEntities(entities: EntityListEntry[]): void;
  /// Optimistically rename one hierarchy row between polls; the next sceneVersion bump re-fetches.
  applyOptimisticEntityName(id: string, name: string): void;
  setSelectedId(selectedId: string | null): void;
  setSelectedMaterialId(id: string | null): void;
  selectEntity(id: string): void;
  toggleExpanded(id: string): void;
  setExpanded(id: string, expanded: boolean): void;
  /// Engine-authoritative reparent (null detaches to root): optimistically relink in place and hold
  /// `dragActive` over the round trip so the poll cannot clobber it. A rejection rolls back by hand,
  /// since a rejected reparent never bumps sceneVersion.
  setParent(id: string, parentId: string | null): Promise<void>;
  setSceneVersion(sceneVersion: number): void;
  setSelectionVersion(selectionVersion: number): void;
  setComponentsBySelected(components: InspectResult | null): void;
  /// Overlay one component's full DTO onto the live inspect result between polls. The poll is gated
  /// off mid-drag, so this is the UI's truth until a new selectionVersion drops it.
  applyOptimisticComponent(component: string, dto: object): void;
  setFocusComponent(focusComponent: string | null): void;
  setEnvironment(environment: Environment | null): void;
  setAnimationState(state: AnimationStateResult | null, clips: AnimationClipDto[]): void;
  setGizmo(patch: Partial<GizmoState>): void;
  /// Optimistic play-state write; the reconcile poll repairs it from the engine.
  setPlayState(playState: PlayState): void;
  setDragActive(dragActive: boolean): void;
  setSceneEntitiesLive(live: boolean): void;
}

/// The asset catalog and the Assets-grid selection model.
export interface AssetSlice {
  assets: AssetEntry[];
  assetFolders: string[];
  /// Tiles subscribe to their own membership, so a selection delta re-renders only the flipped
  /// tiles, never the grid.
  selectedAssetIds: Set<string>;
  selectedFolderPaths: Set<string>;
  assetSelectionAnchor: AssetSelectionAnchor;
  /// True while the Assets-grid marquee is sweeping; gates the details overlay so it opens once on
  /// release instead of flickering per crossed tile.
  assetMarqueeActive: boolean;
  /// The tile whose grid context menu is open, so it keeps a highlight while the menu is up.
  assetMenuTarget: AssetSelectionAnchor;
  /// True while the pointer is over the Assets panel: the mouse side buttons then drive the folder
  /// history rather than cross-tab navigation.
  assetsPanelHovered: boolean;
  assetsFolderNav: AssetsFolderNav | null;
  assetSort: AssetSortMode;
  /// The asset-browser drag payload, populated at dragstart so hover targets can inspect it without
  /// `DataTransfer.getData` during dragover.
  catalogDrag: CatalogDragPayload | null;

  setAssetList(assets: AssetEntry[], folders: string[]): void;
  refreshAssets(): Promise<void>;
  /// Instantiate a model asset into the scene; returns the new root entity id, or null on failure.
  instantiateModel(modelId: string, name?: string): Promise<string | null>;
  /// Extract an embedded sub-asset to a standalone file, keeping its id.
  extractSubAsset(modelId: string, subAssetId: string): Promise<void>;
  clearExtraction(modelId: string, subAssetId: string): Promise<void>;
  /// Rescan assets/ and reconcile the catalog from disk.
  scanAssets(): Promise<void>;
  /// Re-bake a model from its source, skipping if unchanged.
  reimportModel(modelId: string): Promise<void>;
  /// Plain replaces, toggle flips membership, shift unions the anchor→key range along `gridOrder`.
  selectAssetGridItem(
    kind: "asset" | "folder",
    key: string,
    modifiers: { shift: boolean; toggle: boolean },
    gridOrder: AssetGridItem[],
  ): void;
  /// Replace the grid selection outright; the anchor becomes the last asset, else the last folder.
  setAssetSelection(assetIds: string[], folderPaths: string[]): void;
  /// Drop selected assets that left the visible grid and folders that no longer exist;
  /// identity-stable when nothing changed.
  pruneAssetSelection(visibleAssets: AssetEntry[], folders: readonly string[]): void;
  /// Drop just-deleted assets from the selection eagerly, before the refresh.
  removeFromAssetSelection(assetIds: ReadonlySet<string>): void;
  /// Rewrite selected folder paths through a folder move (prefix rename).
  rewriteSelectedFolderPaths(rewrite: (path: string) => string): void;
  setAssetMarqueeActive(assetMarqueeActive: boolean): void;
  setAssetMenuTarget(target: AssetSelectionAnchor): void;
  setAssetsPanelHovered(assetsPanelHovered: boolean): void;
  setAssetsFolderNav(assetsFolderNav: AssetsFolderNav | null): void;
  setAssetSort(assetSort: AssetSortMode): void;
  setCatalogDrag(catalogDrag: CatalogDragPayload | null): void;
}

/// The main-tab strip and its cross-tab navigation history.
export interface TabSlice {
  viewTabs: ViewTab[];
  /// The main-tab id under the pointer; drives the "close hovered tab" mouse command.
  hoveredTabId: string | null;
  activeViewTabId: string;
  /// The single tab active immediately before `activeViewTabId` — closing the active tab returns
  /// here when it is still open, else the left neighbour. Not an MRU walk.
  previousActiveTabId: string | null;
  tabHistory: TabNavHistory;

  setHoveredTabId(hoveredTabId: string | null): void;
  openImageViewerTab(asset: AssetEntry): void;
  openFlameTab(): void;
  openStoreTab(): void;
  openMaterialGraphTab(materialId: string): void;
  /// Open (or focus) the asset editor for a model, keyed by its resolved container uuid.
  openAssetEditorTab(assetId: string, title: string): void;
  /// Resolve an asset (model, mesh, or clip) to its owning `.smodel` container, then open that
  /// model's tab. A resolution failure keys the tab by the asset so the workspace can show a
  /// load-failure state.
  openAssetEditorForAsset(assetId: string, fallbackName: string): void;
  closeViewTab(id: string): void;
  setActiveViewTab(id: string): void;
  moveViewTab(id: string, index: number): void;
  /// Step through the cross-tab navigation history: -1 is back, +1 is forward. A closed target tab
  /// is reopened from its snapshot; a no-op past either end.
  navigateTabHistory(step: -1 | 1): void;
}

/// The two dockspace trees and where each panel last lived.
export interface DockSlice {
  /// One dock tree per island. The two kinds carry disjoint `DockPanelId` spaces, so a panel can
  /// never resolve into the other island's tree.
  dockLayouts: Record<DockSpaceKind, DockLayout>;
  /// The leaf each panel last lived in, so `openPanel` returns it home.
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>;

  /// Focus-or-open a panel: activate it if open, else resolve a leaf
  /// (last-location ⇒ default ⇒ first non-locked ⇒ a fresh leaf).
  openPanel(id: DockPanelId): void;
  closePanel(id: DockPanelId): void;
  /// Make an already-open panel its leaf's active tab (no-op when closed).
  activatePanel(id: DockPanelId): void;
  movePanel(id: DockPanelId, target: DropTarget): void;
  /// Reorder a tab within its leaf; `index` is in the without-moving-tab space.
  reorderTab(leafId: DockNodeId, id: DockPanelId, index: number): void;
  setBranchSizes(branchId: DockNodeId, sizes: Record<string, number>): void;
  resetDockLayout(): void;
  /// Load both dock trees + last-location memory from the per-project key, validated, with a
  /// per-kind fallback to the default factory. No-op without a loaded project.
  hydrateDockLayouts(): void;
}

/// Everything the engine reports about itself: render stats, physics, logs, alarms, and captures.
export interface TelemetrySlice {
  renderStats: RenderStats | null;
  physicsState: PhysicsStateResult | null;
  physicsBodies: PhysicsBodyDto[];
  contactLog: ContactEventDto[];
  contactsOverflowed: boolean;
  scriptLogs: ScriptLogDto[];
  scriptLogsOverflowed: boolean;
  debugOverlays: DebugOverlaysResult | null;
  /// `"default"` tracks the display refresh, or a fixed Hz. The resolved Hz lives in
  /// `perfConfig.targetFps`.
  targetFpsMode: "default" | number;
  upscale: UpscaleDto | null;
  perfConfig: PerfConfigDto | null;
  frameHistory: FrameHistoryDto | null;
  passTimings: RenderPassTimingsDto | null;
  activeAlarms: ActiveAlarmDto[];
  /// Append-only FIRING/RESOLVED log, bounded, newest last.
  alarmLog: AlarmEventDto[];
  /// How far back the frame-time graph displays, in seconds.
  metricsRangeSec: number;
  /// Bucket interval in ms — samples within it average into one plotted point (the smoothness knob).
  metricsBucketMs: number;
  metricsRefreshMs: number;
  /// Freeze the metrics lane; alarms catch up on resume.
  metricsPaused: boolean;
  captureState: CaptureState;
  captureProgress: { current: number; total: number };
  capture: ProfileCaptureDto | null;
  captureWindowFrames: number;
  /// Request pipeline statistics (overdraw / cull / vertex-reuse) in the capture — the heaviest mode.
  captureIncludeStats: boolean;
  /// The pass name highlighted across the Profiler sub-views.
  selectedPass: string | null;
  /// Webview reconcile-poll rate (Hz) as an EMA over the tick interval — NOT the engine frame rate.
  pollRateHz: number;
  uiFrameRateHz: number;
  uiFrameMs: number;

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
  appendAlarmEvents(events: AlarmEventDto[]): void;
  setMetricsRangeSec(metricsRangeSec: number): void;
  setMetricsBucketMs(metricsBucketMs: number): void;
  setMetricsRefreshMs(metricsRefreshMs: number): void;
  setMetricsPaused(metricsPaused: boolean): void;
  setCaptureState(captureState: CaptureState): void;
  setCaptureProgress(current: number, total: number): void;
  setCapture(capture: ProfileCaptureDto | null): void;
  setCaptureWindowFrames(captureWindowFrames: number): void;
  setCaptureIncludeStats(captureIncludeStats: boolean): void;
  setSelectedPass(selectedPass: string | null): void;
  setPollRateHz(pollRateHz: number): void;
  setUiFrameStats(frameRateHz: number, frameMs: number): void;
}

/// An unrequested host exit, surfaced by the launcher's crash card until dismissed.
export interface SessionCrash {
  /// The host exit code (`-1` for a signal-terminated child).
  code: number;
  /// The tail of the host log the shell captured before the exit.
  logTail: string[];
  /// The crashed session's project path, for the restart action; `null` when none was loaded.
  projectPath: string | null;
}

/// The two independent lifecycle axes: the host process and project loading.
export interface ProjectSlice {
  project: ProjectInfo | null;
  engineStatus: EngineStatus;
  projectLoad: ProjectLoadState;
  /// The user hit Cancel on the loading screen. A fast load can already have reached `ready` by the
  /// time the click lands, so the poll refuses to complete a cancelled load.
  projectLoadCancelling: boolean;
  /// The last unrequested session exit, or `null`; set from the `session-exited` shell event.
  sessionCrash: SessionCrash | null;

  setProject(project: ProjectInfo | null): void;
  /// Merge a project-load progress patch, identity-stable and deduped on the DTO `version`.
  setProjectLoad(patch: Partial<ProjectLoadState>): void;
  /// Stash the request, flip to `loading`, and fire the engine kick without awaiting completion.
  /// Progress and completion come from `useProjectLoadPoll`.
  startProjectLoad(request: ProjectLoadRequest): Promise<void>;
  setProjectLoadCancelling(cancelling: boolean): void;
  /// Hard scene reset after a project/scene load: clear scene state, invalidate cached thumbnails,
  /// and let the next reconcile tick re-fetch everything. Idempotent.
  resetSceneState(): void;
  setEngineStatus(patch: Partial<EngineStatus>): void;
  setPhase(phase: EnginePhase, error?: string): void;
  setSessionCrash(sessionCrash: SessionCrash | null): void;
}

/// Editor-local chrome: overlays, view preferences, and the keybinding overrides.
export interface UiSlice {
  /// Parks the native viewport surface so an overlay can paint over the viewport rect.
  viewportHidden: boolean;
  /// True while a native OS file dialog is showing. These are not window-modal, so this is the
  /// app-side lock that stops a second dialog from opening.
  nativeDialogOpen: boolean;
  /// The launcher view is explicitly requested (menu "New Project…", load-failure "Back"). The
  /// launcher also shows itself whenever no project is loaded, a load is in flight, or a session
  /// crashed — this flag only forces it over a live project.
  launcherOpen: boolean;
  exportModalOpen: boolean;
  /// Show the selected entity's components as read-only leaf subrows in the hierarchy, sourced from
  /// `componentsBySelected` (never an extra inspect).
  showComponentSubrows: boolean;
  /// Hide skeleton joints in the outliner; their non-bone descendants re-anchor to the nearest
  /// visible ancestor.
  hideBones: boolean;
  /// Keybinding OVERRIDES only (command id → key-string); absent commands use the registry default.
  keyBindings: Record<string, string>;
  /// Gates the global shortcut hook: the settings dialog holds focus on non-text elements, so the
  /// text-entry guard alone would let shortcuts fire underneath it.
  settingsOpen: boolean;
  devMode: boolean;

  setViewportHidden(viewportHidden: boolean): void;
  setNativeDialogOpen(nativeDialogOpen: boolean): void;
  setLauncherOpen(launcherOpen: boolean): void;
  setExportModalOpen(exportModalOpen: boolean): void;
  toggleComponentSubrows(): void;
  toggleHideBones(): void;
  /// Set one binding override and persist. A value equal to the registry default removes the
  /// override instead, keeping settings.json delta-minimal.
  setKeyBinding(id: CommandId, value: string): void;
  resetKeyBinding(id: CommandId): void;
  resetAllKeyBindings(): void;
  /// Load-time hydration: replace the override map, dropping unknown command ids.
  hydrateKeyBindings(overrides: Record<string, string>): void;
  setSettingsOpen(settingsOpen: boolean): void;
  setDevMode(devMode: boolean): void;
}

/// The vegetation authoring mode: tool, brush, species palette, and cook state.
export interface VegetationSlice {
  vegetationTool: VegetationTool;
  vegetationBrush: VegetationBrush;
  /// Plant asset ids selected in the Vegetation palette.
  vegetationSpecies: Set<string>;
  /// Per-species paint weight (0..1; absent means 1).
  vegetationWeights: Record<string, number>;
  /// The viewport-picked macro plants (stable PlantId hex). A pick holds one; the Lasso tool holds
  /// every plant its polygon enclosed.
  vegetationSelectedPlants: ReadonlySet<string>;
  vegetationActiveLayer: VegetationPaintTarget | null;
  vegetationCookJob: string | null;
  /// World bounds of the last committed brush stroke — the region the panel's Estimate preflights.
  vegetationLastStroke: WorldBoundsDto | null;
  /// Control points the Spline tool has picked but not yet committed, base first.
  vegetationShapePoints: VegetationPoint[];

  setVegetationTool(tool: VegetationTool): void;
  setVegetationBrush(patch: Partial<VegetationBrush>): void;
  toggleVegetationSpecies(id: string, additive: boolean): void;
  setVegetationWeight(id: string, weight: number): void;
  setVegetationSelectedPlants(plants: Iterable<string>): void;
  setVegetationActiveLayer(target: VegetationPaintTarget | null): void;
  setVegetationCookJob(job: string | null): void;
  setVegetationLastStroke(bounds: WorldBoundsDto | null): void;
  addVegetationShapePoint(point: VegetationPoint): void;
  clearVegetationShapePoints(): void;
}

/// The Asset Store browse session, persisted across a grid remount.
export interface StorefrontSlice {
  storeSelected: string | null;
  storeSearchText: string;
  storeKind: StoreKind | null;
  /// The active backend search-session id (null after a bridge restart).
  storeSession: string | null;
  storeResults: StoreResult[];
  storeExhausted: boolean;
  storeScrollTop: number;
  /// The session `storeResults` were loaded for; a grid remount whose results still match the active
  /// session restores them instead of refetching.
  storeResultsSession: string | null;

  setStoreSelected(storeSelected: string | null): void;
  setStoreQuery(query: { text: string; kind: StoreKind | null }): void;
  setStoreSession(storeSession: string | null): void;
  /// Replace the browse results, stamping the session they belong to.
  setStoreResults(storeResults: StoreResult[], session: string): void;
  appendStoreResults(results: StoreResult[], session: string): void;
  setStoreExhausted(storeExhausted: boolean): void;
  setStoreScrollTop(storeScrollTop: number): void;
  resetStoreBrowse(): void;
}

/// Editor-only undo/redo, reconstructed from inverse control calls.
export interface HistorySlice {
  /// Per-main-tab undo/redo history keyed by `ViewTab.id`. Editor-only — the engine has no undo.
  historyByTab: Record<string, TabHistory>;
  /// True while a replay's inverse command is in flight; suppresses re-recording and re-entrancy.
  historyReplaying: boolean;

  /// Record an edit onto a tab's history (default: the active tab). No-ops on a read-only tab and
  /// while a replay is in flight.
  pushEdit(edit: UndoableEdit, tabId?: string): void;
  undo(tabId?: string): Promise<void>;
  redo(tabId?: string): Promise<void>;
  /// Open a gesture transaction: capture `prior` now and push exactly one entry at `commit`, so a
  /// burst of drag/scrub ticks becomes one undo entry.
  beginEdit<T>(opts: { prior: T; selectionId?: string }): {
    commit(final: T, build: (prior: T, final: T) => UndoableEdit): void;
  };
  clearTabHistory(tabId: string): void;
  /// Drop the scene history and every orphaned non-scene history (on scene replace).
  clearSceneHistory(): void;
}

export interface EditorState
  extends
    SceneSlice,
    AssetSlice,
    TabSlice,
    DockSlice,
    TelemetrySlice,
    ProjectSlice,
    UiSlice,
    VegetationSlice,
    StorefrontSlice,
    HistorySlice {}

/// The store's `set`, as each slice receives it.
export type SetEditorState = (
  partial: Partial<EditorState> | ((state: EditorState) => Partial<EditorState>),
) => void;

/// The store's `get`, as each slice receives it. Cross-slice calls go through it.
export type GetEditorState = () => EditorState;
