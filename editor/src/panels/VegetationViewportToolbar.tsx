/// The Vegetation mode's floating viewport toolbar: the tool row with shortcut
/// tooltips plus the brush HUD, which reads out exactly the dimensions the active tool
/// consumes. It renders while the vegetation dock panel is open — the mode's scope — and
/// composites over the transparent viewport hole like any editor DOM. While the world plays only
/// the tools that author nothing stay live; the rest disable and say why.
import { COMMANDS_BY_ID, bindingFor, formatBinding } from "../lib/keybindings";
import { findPanelLeaf } from "../state/dockLayout";
import { useEditorStore } from "../state/store";
import {
  VEGETATION_TOOLS,
  isBrushTool,
  isRuntimeTool,
  isStrokeTool,
  usesFalloff,
  usesTargetDensity,
} from "./vegetationTools";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function VegetationViewportToolbar() {
  const active = useEditorStore((s) => findPanelLeaf(s.dockLayouts.scene, "vegetation") !== null);
  const tool = useEditorStore((s) => s.vegetationTool);
  const brush = useEditorStore((s) => s.vegetationBrush);
  const selectedPlants = useEditorStore((s) => s.vegetationSelectedPlants);
  const shapePoints = useEditorStore((s) => s.vegetationShapePoints.length);
  const setTool = useEditorStore((s) => s.setVegetationTool);
  const keyBindings = useEditorStore((s) => s.keyBindings);
  const editing = useEditorStore((s) => s.playState === "edit");
  if (!active) {
    return null;
  }
  const onlyPlant = selectedPlants.size === 1 ? [...selectedPlants][0]! : null;

  return (
    <div className="pointer-events-none absolute inset-x-0 top-2 flex justify-center">
      <div className="pointer-events-auto flex items-center gap-1 rounded-md border border-border bg-background/95 p-1 shadow-sm">
        {VEGETATION_TOOLS.map(({ tool: id, label, icon: Icon, command }) => {
          const disabled = !editing && !isRuntimeTool(id);
          return (
            <Tooltip key={id}>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant={tool === id ? "default" : "ghost"}
                  aria-pressed={tool === id}
                  aria-label={label}
                  className="h-6 w-6"
                  disabled={disabled}
                  onClick={() => setTool(id)}
                >
                  <Icon className="h-3.5 w-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom">
                {disabled
                  ? `${label} authors the map — stop play to use it`
                  : `${label} (${formatBinding(COMMANDS_BY_ID[command], bindingFor(command, keyBindings))})`}
              </TooltipContent>
            </Tooltip>
          );
        })}
        {isBrushTool(tool) && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] tabular-nums text-muted-foreground">
            r {brush.radius.toFixed(1)} m{usesFalloff(tool) && ` · f ${brush.falloff.toFixed(2)}`}
            {isStrokeTool(tool) && ` · s ${brush.spacing.toFixed(1)} m`}
          </span>
        )}
        {usesTargetDensity(tool) && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] tabular-nums text-muted-foreground">
            d {brush.density.toFixed(2)}
          </span>
        )}
        {tool === "spline" && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] tabular-nums text-muted-foreground">
            {shapePoints} pt · Enter commits
          </span>
        )}
        {selectedPlants.size > 0 && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] text-muted-foreground">
            {onlyPlant !== null ? `plant …${onlyPlant.slice(-8)}` : `${selectedPlants.size} plants`}
          </span>
        )}
      </div>
    </div>
  );
}
