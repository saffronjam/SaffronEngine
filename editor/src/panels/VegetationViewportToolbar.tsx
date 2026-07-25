/// The Vegetation mode's floating viewport toolbar: the tool row with shortcut
/// tooltips plus the brush HUD (radius/falloff/spacing readout) when a brush tool is
/// active. It renders only while the vegetation dock panel is open — the mode's
/// scope — and composites over the transparent viewport hole like any editor DOM.
import { COMMANDS_BY_ID, bindingFor, formatBinding } from "../lib/keybindings";
import { findPanelLeaf } from "../state/dockLayout";
import { useEditorStore } from "../state/store";
import { VEGETATION_TOOLS, isBrushTool } from "./vegetationTools";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function VegetationViewportToolbar() {
  const active = useEditorStore((s) => findPanelLeaf(s.dockLayouts.scene, "vegetation") !== null);
  const tool = useEditorStore((s) => s.vegetationTool);
  const brush = useEditorStore((s) => s.vegetationBrush);
  const selectedPlant = useEditorStore((s) => s.vegetationSelectedPlant);
  const setTool = useEditorStore((s) => s.setVegetationTool);
  const keyBindings = useEditorStore((s) => s.keyBindings);
  const editing = useEditorStore((s) => s.playState === "edit");
  if (!active || !editing) {
    return null;
  }

  return (
    <div className="pointer-events-none absolute inset-x-0 top-2 flex justify-center">
      <div className="pointer-events-auto flex items-center gap-1 rounded-md border border-border bg-background/95 p-1 shadow-sm">
        {VEGETATION_TOOLS.map(({ tool: id, label, icon: Icon, command }) => (
          <Tooltip key={id}>
            <TooltipTrigger asChild>
              <Button
                size="icon"
                variant={tool === id ? "default" : "ghost"}
                aria-pressed={tool === id}
                aria-label={label}
                className="h-6 w-6"
                onClick={() => setTool(id)}
              >
                <Icon className="h-3.5 w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              {label} ({formatBinding(COMMANDS_BY_ID[command], bindingFor(command, keyBindings))})
            </TooltipContent>
          </Tooltip>
        ))}
        {isBrushTool(tool) && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] tabular-nums text-muted-foreground">
            r {brush.radius.toFixed(1)} m · f {brush.falloff.toFixed(2)} · s{" "}
            {brush.spacing.toFixed(1)} m
          </span>
        )}
        {selectedPlant !== null && (
          <span className="ml-1 border-l border-border pl-2 font-mono text-[10px] text-muted-foreground">
            plant …{selectedPlant.slice(-8)}
          </span>
        )}
      </div>
    </div>
  );
}
