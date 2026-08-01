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

/// Tools whose gesture rasterizes brush stamps into the active layer's authored tiles.
const STROKE_TOOLS = new Set<VegetationTool>(["paint", "erase", "density", "reapply", "exclude"]);

/// Tools that read the brush radius: the stroke tools stamp at it, Spline sweeps its influence at
/// it, and Volume raises its box by it.
export function isBrushTool(tool: VegetationTool): boolean {
  return STROKE_TOOLS.has(tool) || tool === "spline" || tool === "volume";
}

/// Whether a press with `tool` rasterizes stamps into authored tiles. Only these sample along the
/// pointer path, so only they read the spacing, the projection, and the slope limit.
export function isStrokeTool(tool: VegetationTool): boolean {
  return STROKE_TOOLS.has(tool);
}

/// Tools that read the brush falloff: a stroke's stamp edge and a volume's soft boundary. A spline
/// carries one influence radius with no edge profile, so it does not.
export function usesFalloff(tool: VegetationTool): boolean {
  return STROKE_TOOLS.has(tool) || tool === "volume";
}

/// Tools that read the brush's target density: the level a Density stroke drives texels to and the
/// level a Fill lays across a whole tile. An Exclude stroke accumulates like Paint and an analytic
/// volume carries no per-texel level, so neither reads it and neither shows the control.
export function usesTargetDensity(tool: VegetationTool): boolean {
  return tool === "density" || tool === "fill";
}

/// Tools whose gesture authors nothing and so stays live while the world is playing: viewport
/// selection and the promotion of a bulk plant to an entity view.
export function isRuntimeTool(tool: VegetationTool): boolean {
  return tool === "select" || tool === "lasso" || tool === "promote";
}
