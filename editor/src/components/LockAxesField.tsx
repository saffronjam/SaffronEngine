/// Per-axis lock toggles for a `bvec3` field. A locked axis is tinted with the gizmo axis accent
/// (X red / Y green / Z blue). Each toggle emits a one-key patch, which the Inspector reassembles so
/// the read-modify-write DTO stays whole; a discrete toggle is one undo entry, with no drag bracket.
import { cn } from "@/lib/utils";

/// Active-axis tints matching the viewport gizmo (X red, Y green, Z blue).
const AXIS_ACTIVE: Record<string, string> = {
  x: "bg-red-950 text-red-300 border-red-800",
  y: "bg-green-950 text-green-300 border-green-800",
  z: "bg-blue-950 text-blue-300 border-blue-800",
};

const AXES = ["x", "y", "z"] as const;

export interface LockAxesFieldProps {
  value: Record<string, boolean>;
  onChange(patch: Record<string, boolean>): void;
}

export function LockAxesField({ value, onChange }: LockAxesFieldProps) {
  return (
    <div className="flex gap-1">
      {AXES.map((axis) => {
        const locked = value?.[axis] === true;
        return (
          <button
            key={axis}
            type="button"
            aria-pressed={locked}
            onClick={() => onChange({ [axis]: !locked })}
            className={cn(
              "flex h-7 flex-1 items-center justify-center rounded-sm border text-[11px] font-semibold transition-colors select-none",
              locked
                ? AXIS_ACTIVE[axis]
                : "border-border bg-background text-muted-foreground hover:bg-accent hover:text-accent-foreground",
            )}
          >
            {axis.toUpperCase()}
          </button>
        );
      })}
    </div>
  );
}
