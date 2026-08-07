import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { humanizeFieldName } from "@/lib/humanize";
import { renderField, resolveHint } from "../../components/fieldRenderer";
import { MATERIAL_PARAMS, vectorAxes } from "./registry";

/// One card per `MaterialSet` slot: the `.smat` it binds to, plus the sparse override rows the
/// object adds on top. Overrides are opt-in, so a parameter appears only once added from the
/// menu — everything absent inherits the referenced material silently.
export function MaterialSetSlots({
  slots,
  onSlotFieldChange,
  onSlotFieldDragStart,
  onSlotFieldDragEnd,
  setSlotOverride,
  clearSlotOverride,
  onEditMaterial,
}: {
  slots: Record<string, unknown>[];
  onSlotFieldChange(slotIndex: number, field: string, next: unknown): void;
  onSlotFieldDragStart(slotIndex: number, field: string): void;
  onSlotFieldDragEnd(slotIndex: number, field: string): void;
  setSlotOverride(slotIndex: number, field: string, value: unknown): void;
  clearSlotOverride(slotIndex: number, field: string): void;
  onEditMaterial(material: string): void;
}) {
  return (
    <>
      {slots.map((slot, slotIndex) => {
        const overrides = (slot.overrides as Record<string, unknown> | undefined) ?? {};
        return (
          <div key={slotIndex} className="rounded border border-border/60">
            <div className="border-b border-border/60 bg-muted/30 px-2 py-1 text-[11px] font-medium text-muted-foreground">
              Slot {slotIndex}
            </div>
            <div className="flex flex-col gap-1.5 px-2 py-1.5">
              {/* The referenced .smat material this slot binds to. */}
              <div className="grid grid-cols-[78px_1fr_20px] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Material
                </Label>
                <div className="min-w-0">
                  {renderField(
                    "MaterialSlot",
                    "material",
                    slot.material ?? "0",
                    (next) => onSlotFieldChange(slotIndex, "material", next),
                    { onDragStart: () => {}, onDragEnd: () => {} },
                  )}
                </div>
                {typeof slot.material === "string" && slot.material !== "0" ? (
                  <Button
                    type="button"
                    size="xs"
                    variant="ghost"
                    className="h-5 w-5 p-0 text-muted-foreground"
                    aria-label="Edit material"
                    onClick={() => onEditMaterial(slot.material as string)}
                  >
                    ✎
                  </Button>
                ) : (
                  <span />
                )}
              </div>
              {/* Only the parameters this object overrides — a sparse list. Each shows its
                  widget layered over the referenced material's value; ✕ removes the override
                  (reverting to the material's value). Everything not listed inherits. */}
              {MATERIAL_PARAMS.filter((p) => p.field in overrides).map(
                ({ field, default: def }) => {
                  const hint = resolveHint("Material", field, def);
                  const axes = vectorAxes(hint.kind);
                  const raw = overrides[field];
                  const widgetValue =
                    axes && Array.isArray(raw)
                      ? Object.fromEntries(axes.map((a, i) => [a, (raw as number[])[i] ?? 0]))
                      : raw;
                  const onChange = (next: unknown): void => {
                    const stored =
                      axes && next && typeof next === "object" && !Array.isArray(next)
                        ? axes.map((a) => (next as Record<string, number>)[a] ?? 0)
                        : next;
                    setSlotOverride(slotIndex, field, stored);
                  };
                  return (
                    <div
                      key={field}
                      className="grid grid-cols-[78px_1fr_20px] items-center gap-1.5"
                    >
                      <Label className="truncate text-[11px] font-normal text-foreground">
                        {humanizeFieldName(field)}
                      </Label>
                      <div className="min-w-0">
                        {renderField("Material", field, widgetValue, onChange, {
                          onDragStart: () => onSlotFieldDragStart(slotIndex, "overrides"),
                          onDragEnd: () => onSlotFieldDragEnd(slotIndex, "overrides"),
                        })}
                      </div>
                      <Button
                        type="button"
                        size="xs"
                        variant="ghost"
                        className="h-5 w-5 p-0 text-muted-foreground"
                        aria-label={`Remove ${humanizeFieldName(field)} override`}
                        onClick={() => clearSlotOverride(slotIndex, field)}
                      >
                        ✕
                      </Button>
                    </div>
                  );
                },
              )}
              {/* Opt-in: overrides are sparse, so a parameter only appears once added here. */}
              {MATERIAL_PARAMS.some((p) => !(p.field in overrides)) ? (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button type="button" size="sm" variant="outline" className="self-center">
                      + Override
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-64 overflow-y-auto">
                    {MATERIAL_PARAMS.filter((p) => !(p.field in overrides)).map(
                      ({ field, default: def }) => (
                        <DropdownMenuItem
                          key={field}
                          onSelect={() => setSlotOverride(slotIndex, field, def)}
                        >
                          {humanizeFieldName(field)}
                        </DropdownMenuItem>
                      ),
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              ) : null}
            </div>
          </div>
        );
      })}
    </>
  );
}
