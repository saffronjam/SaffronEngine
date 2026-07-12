/// The viewport view modes, shown in the Topbar's View Modes dropdown (a radio group, the
/// per-channel modes tucked under a "Buffer Visualization" submenu, UE5-style). The mode is a
/// transient debug output (not persisted, not undoable); it is driven over the control plane
/// via `client.setViewMode` and read back from `renderStats.viewMode`. Only implemented modes
/// are listed, so the menu never offers a value the engine ignores. Order here is display order.
import {
  Activity,
  Axis3d,
  CloudFog,
  Contrast,
  Flame,
  Flashlight,
  Frame,
  Globe,
  Grid3x3,
  Layers,
  Lightbulb,
  type LucideIcon,
  Magnet,
  Palette,
  Sparkles,
  Sun,
  SunDim,
  Waves,
  Wind,
} from "lucide-react";
import type { RenderStats } from "../protocol";

export type ViewMode = RenderStats["viewMode"];

/// Where a mode sits in the dropdown: a top-level shading mode, a per-channel buffer
/// visualization (the submenu), or a standalone analysis heatmap below the submenu.
export type ViewModeGroup = "shading" | "buffer" | "analysis";

export interface ViewModeDef {
  value: ViewMode;
  label: string;
  icon: LucideIcon;
  group: ViewModeGroup;
}

export const VIEW_MODES: ViewModeDef[] = [
  { value: "lit", label: "Lit", icon: Sun, group: "shading" },
  { value: "unlit", label: "Unlit", icon: SunDim, group: "shading" },
  { value: "wireframe", label: "Wireframe", icon: Grid3x3, group: "shading" },
  { value: "lit-wireframe", label: "Lit Wireframe", icon: Frame, group: "shading" },
  { value: "detail-lighting", label: "Detail Lighting", icon: Lightbulb, group: "shading" },
  { value: "lighting-only", label: "Lighting Only", icon: Flashlight, group: "shading" },
  { value: "reflections", label: "Reflections", icon: Sparkles, group: "shading" },
  { value: "albedo", label: "Albedo", icon: Palette, group: "buffer" },
  { value: "normal", label: "Normal", icon: Axis3d, group: "buffer" },
  { value: "roughness", label: "Roughness", icon: Waves, group: "buffer" },
  { value: "metallic", label: "Metallic", icon: Magnet, group: "buffer" },
  { value: "emissive", label: "Emissive", icon: Flame, group: "buffer" },
  { value: "depth", label: "Depth", icon: Layers, group: "buffer" },
  { value: "ambient-occlusion", label: "Ambient Occlusion", icon: Contrast, group: "buffer" },
  { value: "gi", label: "Global Illumination", icon: Globe, group: "buffer" },
  { value: "motion-vectors", label: "Motion Vectors", icon: Wind, group: "buffer" },
  { value: "light-complexity", label: "Light Complexity", icon: Activity, group: "analysis" },
  { value: "fog", label: "Fog", icon: CloudFog, group: "analysis" },
];

/// The mode metadata keyed by wire value, for the trigger icon + label lookup.
export const VIEW_MODE_BY_VALUE = Object.fromEntries(VIEW_MODES.map((m) => [m.value, m])) as Record<
  ViewMode,
  ViewModeDef
>;
