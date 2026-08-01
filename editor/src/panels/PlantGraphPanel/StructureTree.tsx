/// The grown structure as a tree, and the manual-edit layer over the element it selects. Axes nest
/// under the axis they grew from; a placed element hangs under the axis carrying its frame. Picking
/// a row is the semantic selection every manual edit addresses — an edit names *that* element's
/// identity, which is derived from ancestry and so survives a parameter change.
import { memo, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Scissors, Trash2 } from "lucide-react";
import type {
  BotanicalAxisDto,
  BotanicalEditActionDto,
  BotanicalManualEditDto,
  BotanicalPlacementDto,
  PlantGraftSourceDto,
} from "../../protocol";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Q16, UNIT } from "./botanicalSchema";

/// One node of the derived structure tree.
interface StructureNode {
  id: string;
  label: string;
  detail: string;
  depth: number;
  children: StructureNode[];
}

function axisNode(axis: BotanicalAxisDto, depth: number): StructureNode {
  return {
    id: axis.id,
    label: axis.element,
    detail: `r ${(axis.baseRadiusBits / Q16).toFixed(3)} m · ${axis.points} pts`,
    depth,
    children: [],
  };
}

/// Builds the axis hierarchy from parent links and hangs each placement under the axis the report
/// names as carrying its frame. An axis whose parent is absent from the report roots the tree.
export function buildStructure(
  axes: BotanicalAxisDto[],
  placements: BotanicalPlacementDto[],
): StructureNode[] {
  const byId = new Map<string, StructureNode>();
  for (const axis of axes) {
    byId.set(axis.id, axisNode(axis, 0));
  }
  const roots: StructureNode[] = [];
  for (const axis of axes) {
    const node = byId.get(axis.id)!;
    const parent = axis.parent === undefined ? undefined : byId.get(axis.parent);
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  for (const placement of placements) {
    const node: StructureNode = {
      id: placement.id,
      label: placement.element,
      detail: `size ${(placement.sizeBits / Q16).toFixed(3)} m · slot ${placement.materialSlot}`,
      depth: 0,
      children: [],
    };
    const parent = byId.get(placement.axis);
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  const stamp = (nodes: StructureNode[], depth: number): void => {
    for (const node of nodes) {
      node.depth = depth;
      stamp(node.children, depth + 1);
    }
  };
  stamp(roots, 0);
  return roots;
}

const StructureRow = memo(function StructureRow({
  node,
  selected,
  expanded,
  edited,
  onSelect,
  onToggle,
}: {
  node: StructureNode;
  selected: boolean;
  expanded: boolean;
  edited: boolean;
  onSelect: (id: string) => void;
  onToggle: (id: string) => void;
}) {
  return (
    <div className="flex items-center gap-1" style={{ paddingLeft: `${node.depth * 10}px` }}>
      {node.children.length > 0 ? (
        <button
          type="button"
          aria-label={expanded ? "Collapse" : "Expand"}
          className="text-muted-foreground"
          onClick={() => onToggle(node.id)}
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        </button>
      ) : (
        <span className="w-3" />
      )}
      <button
        type="button"
        aria-pressed={selected}
        className={`flex min-w-0 flex-1 items-baseline justify-between gap-2 rounded px-1 text-left text-[11px] ${
          selected ? "bg-primary text-primary-foreground" : "hover:bg-muted"
        }`}
        onClick={() => onSelect(node.id)}
      >
        <span className="truncate">
          {edited && <span className="mr-1 text-amber-500">•</span>}
          {node.label}
        </span>
        <span className="shrink-0 font-mono tabular-nums opacity-70">{node.detail}</span>
      </button>
    </div>
  );
});

export function StructureTree({
  axes,
  placements,
  edits,
  grafts,
  selected,
  onSelect,
  onEdit,
  onClearEdits,
  busy,
}: {
  axes: BotanicalAxisDto[];
  placements: BotanicalPlacementDto[];
  edits: BotanicalManualEditDto[];
  grafts: PlantGraftSourceDto[];
  selected: string | null;
  onSelect: (id: string | null) => void;
  onEdit: (label: string, target: string, action: BotanicalEditActionDto) => void;
  onClearEdits: (target: string) => void;
  busy: boolean;
}) {
  const roots = useMemo(() => buildStructure(axes, placements), [axes, placements]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const editedIds = useMemo(() => new Set(edits.map((edit) => edit.target)), [edits]);
  const selectedEdits = edits.filter((edit) => edit.target === selected);

  const rows: StructureNode[] = [];
  const walk = (nodes: StructureNode[]): void => {
    for (const node of nodes) {
      rows.push(node);
      if (!collapsed.has(node.id)) {
        walk(node.children);
      }
    }
  };
  walk(roots);

  const toggle = (id: string): void =>
    setCollapsed((set) => {
      const next = new Set(set);
      if (!next.delete(id)) {
        next.add(id);
      }
      return next;
    });

  const transform = edits.find(
    (edit) => edit.target === selected && edit.action.kind === "transform",
  )?.action;
  const offset = transform?.kind === "transform" ? transform.offsetBits : [0, 0, 0];
  const scaleBits = transform?.kind === "transform" ? transform.scaleBits : Q16;
  const roll = transform?.kind === "transform" ? transform.roll : 0;
  const trim = edits.find(
    (edit) => edit.target === selected && edit.action.kind === "trim",
  )?.action;
  const trimAt = trim?.kind === "trim" ? trim.at : Math.round(UNIT / 2);
  const graft = edits.find(
    (edit) => edit.target === selected && edit.action.kind === "graft",
  )?.action;
  const graftSource = graft?.kind === "graft" ? graft.source : null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="min-h-0 flex-1 overflow-auto p-1">
        {rows.length === 0 ? (
          <p className="p-2 text-[11px] text-muted-foreground">
            This graph grows no structure yet.
          </p>
        ) : (
          rows.map((node) => (
            <StructureRow
              key={node.id}
              node={node}
              selected={selected === node.id}
              expanded={!collapsed.has(node.id)}
              edited={editedIds.has(node.id)}
              onSelect={(id) => onSelect(selected === id ? null : id)}
              onToggle={toggle}
            />
          ))
        )}
      </div>
      {selected !== null && (
        <div className="flex shrink-0 flex-col gap-1 border-t border-border p-2">
          <div className="flex items-center justify-between gap-2">
            <Label className="truncate font-mono text-[10px] text-muted-foreground">
              {selected.slice(0, 16)}…
            </Label>
            <span className="flex items-center gap-1">
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    size="icon"
                    variant="ghost"
                    aria-label="Trim here"
                    className="h-5 w-5"
                    disabled={busy}
                    onClick={() => onEdit("Trim element", selected, { kind: "trim", at: trimAt })}
                  >
                    <Scissors className="h-3 w-3" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">Cut the axis and drop what sits above</TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    size="icon"
                    variant="ghost"
                    aria-label="Remove element"
                    className="h-5 w-5"
                    disabled={busy}
                    onClick={() => onEdit("Remove element", selected, { kind: "remove" })}
                  >
                    <Trash2 className="h-3 w-3" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent side="top">
                  Delete this element and everything it carries
                </TooltipContent>
              </Tooltip>
              <Button
                size="sm"
                variant="ghost"
                className="h-5 text-[10px]"
                disabled={busy || selectedEdits.length === 0}
                onClick={() => onClearEdits(selected)}
              >
                Clear
              </Button>
            </span>
          </div>
          <div className="flex items-center gap-2">
            <Label className="w-16 shrink-0 text-[11px] text-muted-foreground">Offset (m)</Label>
            <div className="flex min-w-0 flex-1 gap-1">
              {[0, 1, 2].map((axis) => (
                <Input
                  key={axis}
                  type="number"
                  step={0.05}
                  className="h-6 min-w-0 flex-1 font-mono text-[11px]"
                  value={Number((offset[axis]! / Q16).toFixed(3))}
                  disabled={busy}
                  onChange={(event) => {
                    const next: [number, number, number] = [offset[0]!, offset[1]!, offset[2]!];
                    next[axis] = Math.round(Number(event.target.value) * Q16);
                    if (Number.isFinite(next[axis])) {
                      onEdit("Move element", selected, {
                        kind: "transform",
                        offsetBits: next,
                        roll,
                        scaleBits,
                      });
                    }
                  }}
                />
              ))}
            </div>
          </div>
          <div className="flex items-center gap-2">
            <Label className="w-16 shrink-0 text-[11px] text-muted-foreground">Scale</Label>
            <Input
              type="number"
              step={0.05}
              min={0.01}
              className="h-6 min-w-0 flex-1 font-mono text-[11px]"
              value={Number((scaleBits / Q16).toFixed(3))}
              disabled={busy}
              onChange={(event) => {
                const next = Math.max(1, Math.round(Number(event.target.value) * Q16));
                if (Number.isFinite(next)) {
                  onEdit("Scale element", selected, {
                    kind: "transform",
                    offsetBits: [offset[0]!, offset[1]!, offset[2]!],
                    roll,
                    scaleBits: next,
                  });
                }
              }}
            />
            <Label className="w-8 shrink-0 text-[11px] text-muted-foreground">Roll</Label>
            <Input
              type="number"
              step={0.01}
              min={0}
              max={1}
              className="h-6 min-w-0 flex-1 font-mono text-[11px]"
              value={Number((roll / UNIT).toFixed(3))}
              disabled={busy}
              onChange={(event) => {
                const next = Math.min(
                  UNIT,
                  Math.max(0, Math.round(Number(event.target.value) * UNIT)),
                );
                if (Number.isFinite(next)) {
                  onEdit("Turn element", selected, {
                    kind: "transform",
                    offsetBits: [offset[0]!, offset[1]!, offset[2]!],
                    roll: next,
                    scaleBits,
                  });
                }
              }}
            />
          </div>
          {grafts.length > 0 && (
            <div className="flex items-center gap-2">
              <Label className="w-16 shrink-0 text-[11px] text-muted-foreground">Graft</Label>
              <Select
                value={graftSource ?? ""}
                onValueChange={(source) =>
                  onEdit("Graft element", selected, {
                    kind: "graft",
                    source,
                    selector: { kind: "whole" },
                  })
                }
              >
                <SelectTrigger size="sm" className="h-6 min-w-0 flex-1 text-[11px]">
                  <SelectValue placeholder="Hero mesh…" />
                </SelectTrigger>
                <SelectContent>
                  {grafts.map((row) => (
                    <SelectItem key={row.id} value={row.id} className="font-mono text-[11px]">
                      {row.id.slice(0, 12)}…
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
