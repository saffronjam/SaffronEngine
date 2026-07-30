import { GradingWheel } from "../../components/GradingWheel";
import type { Triplet } from "./grading";

/// A row of Lift/Gamma/Gain trackballs over one CDL block (global or a tonal range). Lift patches
/// `offset` (neutral 0), Gamma `power`, Gain `slope` (both neutral 1) — the canonical Resolve mapping.
export function CdlWheels({
  cdl,
  onPatch,
  onDragStart,
  onDragEnd,
}: {
  cdl: { slope: Triplet; offset: Triplet; power: Triplet };
  onPatch(patch: { slope?: Triplet; offset?: Triplet; power?: Triplet }): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  return (
    <div className="flex items-start justify-around gap-1 py-1">
      <GradingWheel
        label="Lift"
        value={cdl.offset}
        neutral={0}
        chromaRange={0.15}
        masterMin={-0.25}
        masterMax={0.25}
        onChange={(rgb) => onPatch({ offset: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <GradingWheel
        label="Gamma"
        value={cdl.power}
        neutral={1}
        chromaRange={0.3}
        masterMin={0.25}
        masterMax={2}
        onChange={(rgb) => onPatch({ power: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <GradingWheel
        label="Gain"
        value={cdl.slope}
        neutral={1}
        chromaRange={0.3}
        masterMin={0}
        masterMax={2}
        onChange={(rgb) => onPatch({ slope: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </div>
  );
}
