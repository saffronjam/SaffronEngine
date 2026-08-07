/// The per-panel definition table: title, closability, render policy, and body component.
/// One row per `DockPanelId` across both islands. `DockPanelsHost` reads it to decide what to
/// mount and when, and the strips read `title`. The panel→default-leaf fallback the
/// `openPanel` resolver uses lives in the pure model (`DEFAULT_LEAF`), which the store imports
/// directly — keeping the store decoupled from these React components.
import type { LucideIcon } from "lucide-react";
import type { ComponentType } from "react";
import type { AssetEditorDockPanelId, DockPanelId, SceneDockPanelId } from "../../state/dockLayout";
import {
  AssetClipsPanel,
  AssetMaterialPanel,
  AssetPreviewPanel,
  AssetSkeletonPanel,
  AssetTimelinePanel,
  VegetationSummaryPanel,
} from "../../panels/assetEditorPanels";
import { BiomeGraphPanel } from "../../panels/BiomeGraphPanel";
import { InspectorPanel } from "../../panels/InspectorPanel";
import { EnvironmentPanel } from "../../panels/EnvironmentPanel";
import { WindDebugPanel } from "../../panels/WindDebugPanel";
import { RenderPanel } from "../../panels/RenderPanel";
import { PostProcessPanel } from "../../panels/PostProcessPanel";
import { RenderStatsPanel } from "../../panels/RenderStatsPanel";
import { ProfilerPanel } from "../../panels/ProfilerPanel";
import { PhysicsPanel } from "../../panels/PhysicsPanel";
import { ScriptLogsPanel } from "../../panels/ScriptLogsPanel";
import { MaterialEditorPanel } from "../../panels/MaterialEditorPanel";
import { PlantAtlasPanel } from "../../panels/PlantAtlasPanel";
import { PlantHierarchyPanel } from "../../panels/PlantHierarchyPanel";
import { PlantProxiesPanel } from "../../panels/PlantProxiesPanel";
import { PlantSeasonPanel } from "../../panels/PlantSeasonPanel";
import { PlantWindPanel } from "../../panels/PlantWindPanel";
import { VegetationPanel } from "../../panels/VegetationPanel";
import { VegetationTelemetryPanel } from "../../panels/VegetationTelemetryPanel";
import { EcologyTimelinePanel } from "../../panels/EcologyTimelinePanel";
import { PlantGraphPanel } from "../../panels/PlantGraphPanel";
import { TimelinePanel } from "../../panels/TimelinePanel";
import { HierarchyPanel } from "../../panels/HierarchyPanel";
import { AssetsPanel } from "../../panels/AssetsPanel";
import { ViewportPanel } from "../../panels/ViewportPanel";

/// The Tools-menu group a closable Scene panel belongs to (editor panels vs diagnostics).
export type PanelGroup = "editing" | "diagnostics";

export interface DockPanelDef {
  id: DockPanelId;
  title: string;
  icon?: LucideIcon;
  closable: boolean;
  /// Tools-menu grouping for the closable Scene panels (omitted for the rest).
  group?: PanelGroup;
  /// `always`: stay mounted (hidden) when not the active tab — for panels with expensive
  /// live state (Material's GPU preview, Assets' thumbnails). `onlyWhenVisible`: unmount
  /// when hidden, leaving an empty attached host div.
  renderer: "always" | "onlyWhenVisible";
  component: ComponentType;
}

/// The Scene island's panels.
export const SCENE_PANEL_REGISTRY: Record<SceneDockPanelId, DockPanelDef> = {
  inspector: {
    id: "inspector",
    title: "Inspector",
    closable: false,
    renderer: "onlyWhenVisible",
    component: InspectorPanel,
  },
  environment: {
    id: "environment",
    title: "Environment",
    closable: false,
    renderer: "onlyWhenVisible",
    component: EnvironmentPanel,
  },
  windDebug: {
    id: "windDebug",
    title: "Wind Debug",
    closable: true,
    renderer: "onlyWhenVisible",
    component: WindDebugPanel,
  },
  render: {
    id: "render",
    title: "Render",
    closable: false,
    renderer: "onlyWhenVisible",
    component: RenderPanel,
  },
  postProcess: {
    id: "postProcess",
    title: "Post",
    closable: false,
    renderer: "onlyWhenVisible",
    component: PostProcessPanel,
  },
  stats: {
    id: "stats",
    title: "Stats",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: RenderStatsPanel,
  },
  profiler: {
    id: "profiler",
    title: "Profiler",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: ProfilerPanel,
  },
  physics: {
    id: "physics",
    title: "Physics",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: PhysicsPanel,
  },
  scriptLogs: {
    id: "scriptLogs",
    title: "Script Logs",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: ScriptLogsPanel,
  },
  material: {
    id: "material",
    title: "Material",
    closable: true,
    group: "editing",
    renderer: "always",
    component: MaterialEditorPanel,
  },
  vegetation: {
    id: "vegetation",
    title: "Vegetation",
    closable: true,
    group: "editing",
    renderer: "onlyWhenVisible",
    component: VegetationPanel,
  },
  vegetationTelemetry: {
    id: "vegetationTelemetry",
    title: "Vegetation Telemetry",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: VegetationTelemetryPanel,
  },
  ecologyTimeline: {
    id: "ecologyTimeline",
    title: "Ecology Timeline",
    closable: true,
    group: "diagnostics",
    renderer: "onlyWhenVisible",
    component: EcologyTimelinePanel,
  },
  timeline: {
    id: "timeline",
    title: "Timeline",
    closable: true,
    group: "editing",
    renderer: "onlyWhenVisible",
    component: TimelinePanel,
  },
  hierarchy: {
    id: "hierarchy",
    title: "Hierarchy",
    closable: false,
    renderer: "onlyWhenVisible",
    component: HierarchyPanel,
  },
  assets: {
    id: "assets",
    title: "Assets",
    closable: true,
    group: "editing",
    renderer: "always",
    component: AssetsPanel,
  },
  viewport: {
    id: "viewport",
    title: "Viewport",
    closable: false,
    renderer: "always",
    component: ViewportPanel,
  },
};

/// The asset-editor island's panels. `preview` is the locked live-subsurface host (like the
/// Scene viewport); the other three carry the unmount-when-hidden policy of their Scene
/// cousins. Capability gating (rig → skeleton; clips → clips + assetTimeline) opens/closes
/// them per the previewed model, so a row being registered does not mean it is always open.
export const ASSET_EDITOR_PANEL_REGISTRY: Record<AssetEditorDockPanelId, DockPanelDef> = {
  skeleton: {
    id: "skeleton",
    title: "Skeleton",
    closable: true,
    renderer: "onlyWhenVisible",
    component: AssetSkeletonPanel,
  },
  preview: {
    id: "preview",
    title: "Preview",
    closable: false,
    renderer: "always",
    component: AssetPreviewPanel,
  },
  clips: {
    id: "clips",
    title: "Clips",
    closable: true,
    renderer: "onlyWhenVisible",
    component: AssetClipsPanel,
  },
  assetTimeline: {
    id: "assetTimeline",
    title: "Timeline",
    closable: true,
    renderer: "onlyWhenVisible",
    component: AssetTimelinePanel,
  },
  materialEdit: {
    id: "materialEdit",
    title: "Material",
    closable: true,
    // Live GPU preview inside — keep it mounted (hidden) when the tab isn't active, like the
    // Scene dock's Material panel.
    renderer: "always",
    component: AssetMaterialPanel,
  },
  assetStats: {
    id: "assetStats",
    title: "Stats",
    closable: true,
    renderer: "onlyWhenVisible",
    component: RenderStatsPanel,
  },
  vegSummary: {
    id: "vegSummary",
    title: "Vegetation",
    closable: true,
    renderer: "onlyWhenVisible",
    component: VegetationSummaryPanel,
  },
  plantGraph: {
    id: "plantGraph",
    title: "Plant Graph",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantGraphPanel,
  },
  plantWind: {
    id: "plantWind",
    title: "Wind Preview",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantWindPanel,
  },
  plantAtlas: {
    id: "plantAtlas",
    title: "Atlas",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantAtlasPanel,
  },
  plantHierarchy: {
    id: "plantHierarchy",
    title: "Hierarchy",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantHierarchyPanel,
  },
  plantSeason: {
    id: "plantSeason",
    title: "Season",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantSeasonPanel,
  },
  plantProxies: {
    id: "plantProxies",
    title: "Proxies",
    closable: true,
    renderer: "onlyWhenVisible",
    component: PlantProxiesPanel,
  },
  biomeGraph: {
    id: "biomeGraph",
    title: "Biome Graph",
    closable: true,
    renderer: "onlyWhenVisible",
    component: BiomeGraphPanel,
  },
};

const REGISTRY: Record<DockPanelId, DockPanelDef> = {
  ...SCENE_PANEL_REGISTRY,
  ...ASSET_EDITOR_PANEL_REGISTRY,
};

/// The definition for a panel id (every id is registered across the two islands).
export function panelDef(id: DockPanelId): DockPanelDef | undefined {
  return REGISTRY[id];
}

/// The strip title for a panel id, falling back to the id when unregistered.
export function panelTitle(id: DockPanelId): string {
  return REGISTRY[id]?.title ?? id;
}
