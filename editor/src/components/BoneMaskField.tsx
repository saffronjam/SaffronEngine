/// The joint-subset selector for KinematicBones.driven. The wire value is an i32[] of indices into
/// SkinnedMesh.bones, where an EMPTY array means EVERY joint (not "none"). So the control is an
/// "All joints" toggle plus, when a subset is chosen, a checklist popover. Unchecking the last joint
/// normalizes back to the empty (= all) array. Each toggle is a discrete edit (one undo entry).
import { useState } from "react";
import { Check } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import type { Joint } from "./BoneSelect";

export interface BoneMaskFieldProps {
  value: number[];
  joints: readonly Joint[];
  onChange(next: number[]): void;
}

export function BoneMaskField({ value, joints, onChange }: BoneMaskFieldProps) {
  const [open, setOpen] = useState(false);
  const all = value.length === 0;
  const selected = new Set(value);
  const total = joints.length;

  const setAll = (on: boolean): void => {
    onChange(on ? [] : joints.map((j) => j.index));
  };
  const toggle = (index: number): void => {
    const next = new Set(selected);
    if (next.has(index)) {
      next.delete(index);
    } else {
      next.add(index);
    }
    onChange([...next].sort((a, b) => a - b));
  };

  return (
    <div className="flex flex-col gap-1.5">
      <div className="grid grid-cols-[78px_1fr] items-center gap-1.5">
        <Label className="truncate text-[11px] font-normal text-muted-foreground">All joints</Label>
        <Switch checked={all} onCheckedChange={setAll} />
      </div>
      {all ? (
        <span className="px-0.5 text-[10px] text-muted-foreground">
          Every joint ({total}) gets a kinematic body.
        </span>
      ) : (
        <>
          <Popover open={open} onOpenChange={setOpen}>
            <PopoverTrigger asChild>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="h-7 w-full justify-between px-1.5 font-mono text-[11px]"
              >
                <span className="truncate">
                  {selected.size} of {total} joints
                </span>
              </Button>
            </PopoverTrigger>
            <PopoverContent align="start" className="w-(--radix-popover-trigger-width) p-1">
              {/* Native max-height + overflow scroll: a Radix ScrollArea's `h-full` viewport
                  can't resolve against a max-height-only (auto-height) parent, so it would grow
                  to full content height and overshoot the cap instead of scrolling. */}
              <div className="flex max-h-56 flex-col gap-0.5 overflow-y-auto">
                {joints.map((j) => (
                  <button
                    key={j.id}
                    type="button"
                    onClick={() => toggle(j.index)}
                    className={cn(
                      "flex w-full items-center gap-1.5 rounded-sm px-1.5 py-1 text-left font-mono text-[11px]",
                      "hover:bg-accent hover:text-accent-foreground",
                      selected.has(j.index) && "bg-accent/60",
                    )}
                  >
                    <span className="flex size-3.5 flex-none items-center justify-center">
                      {selected.has(j.index) ? <Check className="size-3 text-foreground" /> : null}
                    </span>
                    <span className="min-w-0 flex-1 truncate">{j.name}</span>
                  </button>
                ))}
                {total === 0 ? (
                  <span className="px-2 py-1 text-[11px] italic text-muted-foreground">
                    No joints
                  </span>
                ) : null}
              </div>
            </PopoverContent>
          </Popover>
          <span className="px-0.5 text-[10px] text-muted-foreground">
            Unchecking every joint reverts to all.
          </span>
        </>
      )}
    </div>
  );
}
