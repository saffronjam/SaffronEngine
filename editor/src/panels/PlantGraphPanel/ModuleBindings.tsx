/// The family's module calls: which `.splant` preset each call site grows, at which variation and
/// scale. A call site and its binding are one thing here because the engine validates them against
/// each other — adding one adds both, removing one removes both.
import { useState } from "react";
import { Trash2 } from "lucide-react";
import { AssetPicker } from "../../components/AssetPicker";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Q16 } from "./botanicalSchema";
import {
  withModuleBinding,
  withModuleCall,
  withoutModuleCall,
  type PlantDocument,
} from "./document";

const NONE_UUID = "0";

export function ModuleBindings({
  document,
  onApply,
  onScrub,
  busy,
}: {
  document: PlantDocument;
  onApply: (label: string, next: PlantDocument) => void;
  onScrub: (label: string, next: PlantDocument) => void;
  busy: boolean;
}) {
  const [adding, setAdding] = useState(NONE_UUID);

  return (
    <div className="flex flex-col gap-1">
      <Label className="text-[11px] text-muted-foreground">Module calls</Label>
      {document.modules.map((module) => (
        <div key={module.callGuid} className="flex items-center gap-1">
          <div className="min-w-0 flex-1">
            <AssetPicker
              value={module.plant}
              assetType="plant"
              onChange={(plant) =>
                onApply("Rebind module", withModuleBinding(document, module.callGuid, { plant }))
              }
            />
          </div>
          <Input
            type="number"
            min={0}
            className="h-6 w-14 font-mono text-[11px]"
            value={module.variation}
            disabled={busy}
            aria-label="Module variation"
            onChange={(event) => {
              const variation = Math.max(0, Math.round(Number(event.target.value)));
              if (Number.isFinite(variation)) {
                onScrub(
                  "Set module variation",
                  withModuleBinding(document, module.callGuid, { variation }),
                );
              }
            }}
          />
          <Input
            type="number"
            min={0.01}
            step={0.05}
            className="h-6 w-16 font-mono text-[11px]"
            value={Number((module.scaleBits / Q16).toFixed(3))}
            disabled={busy}
            aria-label="Module scale"
            onChange={(event) => {
              // The engine refuses a zero or negative placement scale, so the field cannot reach one.
              const scaleBits = Math.max(1, Math.round(Number(event.target.value) * Q16));
              if (Number.isFinite(scaleBits)) {
                onScrub(
                  "Set module scale",
                  withModuleBinding(document, module.callGuid, { scaleBits }),
                );
              }
            }}
          />
          <Button
            size="icon"
            variant="ghost"
            className="h-6 w-6"
            aria-label="Remove module call"
            disabled={busy}
            onClick={() =>
              onApply("Remove module call", withoutModuleCall(document, module.callGuid))
            }
          >
            <Trash2 className="h-3 w-3" />
          </Button>
        </div>
      ))}
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1">
          <AssetPicker
            value={adding}
            assetType="plant"
            onChange={(plant) => {
              setAdding(NONE_UUID);
              if (plant !== NONE_UUID && plant !== "") {
                onApply("Add module call", withModuleCall(document, plant));
              }
            }}
          />
        </span>
        <span className="text-[10px] text-muted-foreground">adds a call site</span>
      </div>
    </div>
  );
}
