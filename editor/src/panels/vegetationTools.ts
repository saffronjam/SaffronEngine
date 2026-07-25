/// The Vegetation mode's tool vocabulary shared by the dock panel's palette, the
/// viewport toolbar, and the shortcut hook — one definition of icon, label, and
/// keybinding command per tool.
import {
  Anchor,
  Ban,
  Brush,
  CircleDot,
  Eraser,
  Lasso,
  Layers,
  MousePointer2,
  PaintBucket,
  Pin,
  Repeat2,
  Spline,
  TreePine,
} from "lucide-react";
import type { CommandId } from "../lib/keybindings";
import type { VegetationTool } from "../state/store";

export interface VegetationToolDef {
  tool: VegetationTool;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  command: CommandId;
}

export const VEGETATION_TOOLS: VegetationToolDef[] = [
  { tool: "select", label: "Select", icon: MousePointer2, command: "vegetation.tool.select" },
  { tool: "lasso", label: "Lasso", icon: Lasso, command: "vegetation.tool.lasso" },
  { tool: "paint", label: "Paint", icon: Brush, command: "vegetation.tool.paint" },
  { tool: "erase", label: "Erase", icon: Eraser, command: "vegetation.tool.erase" },
  { tool: "density", label: "Density", icon: Layers, command: "vegetation.tool.density" },
  { tool: "reapply", label: "Reapply", icon: Repeat2, command: "vegetation.tool.reapply" },
  { tool: "single", label: "Single", icon: CircleDot, command: "vegetation.tool.single" },
  { tool: "fill", label: "Fill", icon: PaintBucket, command: "vegetation.tool.fill" },
  { tool: "spline", label: "Spline", icon: Spline, command: "vegetation.tool.spline" },
  { tool: "volume", label: "Volume", icon: TreePine, command: "vegetation.tool.volume" },
  { tool: "exclude", label: "Exclude", icon: Ban, command: "vegetation.tool.exclude" },
  { tool: "pin", label: "Pin", icon: Pin, command: "vegetation.tool.pin" },
  { tool: "promote", label: "Promote", icon: Anchor, command: "vegetation.tool.promote" },
];

/// Tools that read the brush parameters (the HUD shows radius/falloff for these).
export function isBrushTool(tool: VegetationTool): boolean {
  return tool !== "select" && tool !== "lasso" && tool !== "pin";
}
