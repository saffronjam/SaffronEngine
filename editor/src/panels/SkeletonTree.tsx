/// The asset editor's bone hierarchy, shown only for a rigged model. Selecting a bone drives the
/// preview overlay's highlight channel, never scene selection, so the selection-keyed animation
/// state the timeline reads stays alive; a viewport joint click flows back the same way. Bone names
/// render verbatim — they are the durable clip-binding keys.
import { useEffect, useMemo, useRef, useState } from "react";
import { Bone, ChevronDown, ChevronRight } from "lucide-react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import type { BoneDto } from "../protocol";

/// Indent caps at this depth so a deep humanoid chain never forces horizontal scroll.
const MAX_INDENT_DEPTH = 10;
const INDENT_PX = 12;

interface SkeletonTreeProps {
  bones: BoneDto[];
  /// The get-asset-model node index currently highlighted (-1 = none). Local view state in the workspace.
  selectedIndex: number;
  onSelect: (joint: number) => void;
}

export function SkeletonTree({ bones, selectedIndex, onSelect }: SkeletonTreeProps) {
  // children-by-parent (keyed by node index) + roots, rebuilt only when the bone list changes.
  const { childrenOf, roots } = useMemo(() => {
    const present = new Set(bones.map((b) => b.index));
    const byParent = new Map<number, BoneDto[]>();
    const rootList: BoneDto[] = [];
    for (const bone of bones) {
      if (bone.parent >= 0 && present.has(bone.parent)) {
        const list = byParent.get(bone.parent);
        if (list) {
          list.push(bone);
        } else {
          byParent.set(bone.parent, [bone]);
        }
      } else {
        rootList.push(bone);
      }
    }
    return { childrenOf: byParent, roots: rootList };
  }, [bones]);

  const [collapsed, setCollapsed] = useState<ReadonlySet<number>>(() => new Set());

  // Scroll the selected row into view when the selection moves (e.g. a viewport joint pick selected a
  // bone that's off-screen in a long list). No-ops if the row isn't rendered (a collapsed ancestor).
  const selectedRowRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    selectedRowRef.current?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  if (bones.length === 0) {
    return (
      <div className="flex h-full items-center justify-center bg-background p-3 text-center text-xs italic text-muted-foreground">
        No skeleton in this asset.
      </div>
    );
  }

  const toggle = (index: number): void => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  };

  const renderRows = (bone: BoneDto, depth: number): React.ReactNode => {
    const kids = childrenOf.get(bone.index) ?? [];
    const hasKids = kids.length > 0;
    const isCollapsed = collapsed.has(bone.index);
    const selected = selectedIndex === bone.index;
    return (
      <div key={bone.index}>
        <div
          ref={selected ? selectedRowRef : null}
          role="button"
          tabIndex={0}
          className={cn(
            "flex w-full cursor-default items-center gap-1 py-0.5 pr-2 text-xs",
            selected ? "bg-accent text-accent-foreground" : "hover:bg-accent/40",
            bone.joint ? "text-foreground" : "text-muted-foreground",
          )}
          style={{ paddingLeft: `${Math.min(depth, MAX_INDENT_DEPTH) * INDENT_PX + 8}px` }}
          onClick={() => onSelect(bone.index)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onSelect(bone.index);
            }
          }}
        >
          {hasKids ? (
            <span
              className="flex size-4 shrink-0 items-center justify-center text-muted-foreground"
              onClick={(e) => {
                e.stopPropagation();
                toggle(bone.index);
              }}
            >
              {isCollapsed ? (
                <ChevronRight className="size-3" />
              ) : (
                <ChevronDown className="size-3" />
              )}
            </span>
          ) : (
            <span className="size-4 shrink-0" />
          )}
          {bone.joint && <Bone className="size-3 shrink-0 opacity-70" />}
          <span className="truncate">{bone.name || `node ${bone.index}`}</span>
        </div>
        {hasKids && !isCollapsed && kids.map((kid) => renderRows(kid, depth + 1))}
      </div>
    );
  };

  return (
    <div className="flex h-full flex-col bg-background">
      <ScrollArea className="min-h-0 flex-1">
        <div className="py-1">{roots.map((bone) => renderRows(bone, 0))}</div>
      </ScrollArea>
    </div>
  );
}
