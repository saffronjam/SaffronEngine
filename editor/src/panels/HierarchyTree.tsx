/// The outliner tree: the entity forest built client-side from the flat store slice. Rows select,
/// rename inline, delete, and reparent by drag; expand state lives outside the version-gated poll so
/// a scene mutation never collapses the tree.
///
/// Every row is a `memo`'d component subscribing to its OWN selected/expanded bits, and the whole
/// tree shares ONE context menu resolved from `data-entity-id`, so a selection change re-renders two
/// rows rather than the tree. Every drag affordance is in-flow sidebar DOM — the viewport surface
/// paints over anything floating.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useEditorStore, buildTree, reanchorPastBones, type TreeNode } from "../state/store";
import { ASSET_DND_MIME, assetIdsFromPayload, readAssetPayload } from "../components/AssetTile";
import { errorText, notify, notifyError } from "../lib/flash";
import { matchesBinding } from "../lib/keybindings";
import { orderedComponentNames } from "../lib/componentOrder";
import type { EntityListEntry } from "../protocol";
import { logRender } from "../lib/renderLog";
import { Input } from "@/components/ui/input";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { cn } from "@/lib/utils";

export interface TreeActions {
  onSelect(entity: EntityListEntry): void;
  onFocus(id: string): void;
  onCopy(id: string): void;
  onDelete(id: string): void;
  /// Reparent via the store's optimistic setParent; null detaches to root. The
  /// caller wraps rejections in its flash.
  onReparent(id: string, parentId: string | null): void;
  renamingId: string | null;
  onRenameStart(id: string): void;
  onRenameCommit(id: string, next: string): void;
  onRenameCancel(): void;
}

const DND_MIME = "application/x-saffron-entity";
const MAX_INDENT_DEPTH = 10;
const INDENT_PX = 12;

/// True when `candidateId` is `rootId` or sits anywhere in its subtree, walking the
/// candidate's ancestry through `parentId` (bounded so corrupt data cannot loop).
export function isInSubtree(
  entities: EntityListEntry[],
  rootId: string,
  candidateId: string,
): boolean {
  const parentOf = new Map(entities.map((e) => [e.id, e.parentId]));
  let cursor: string | undefined = candidateId;
  for (let steps = 0; cursor && cursor !== "0" && steps <= entities.length; steps++) {
    if (cursor === rootId) {
      return true;
    }
    cursor = parentOf.get(cursor);
  }
  return false;
}

/// The dragged entity's subtree (itself + every descendant), so per-row drop
/// validity is a Set lookup instead of an ancestry walk per row.
export function subtreeIds(entities: EntityListEntry[], rootId: string): Set<string> {
  const childrenOf = new Map<string, string[]>();
  for (const e of entities) {
    const parent = e.parentId ?? "0";
    const list = childrenOf.get(parent);
    if (list) {
      list.push(e.id);
    } else {
      childrenOf.set(parent, [e.id]);
    }
  }
  const ids = new Set<string>([rootId]);
  const stack = [rootId];
  for (let cursor = stack.pop(); cursor !== undefined; cursor = stack.pop()) {
    for (const child of childrenOf.get(cursor) ?? []) {
      if (!ids.has(child)) {
        ids.add(child);
        stack.push(child);
      }
    }
  }
  return ids;
}

export function HierarchyTree({ actions }: { actions: TreeActions }) {
  logRender("HierarchyTree");
  const entities = useEditorStore((s) => s.entities);
  const selectedId = useEditorStore((s) => s.selectedId);
  const setExpanded = useEditorStore((s) => s.setExpanded);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const hideBones = useEditorStore((s) => s.hideBones);
  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);
  // The entity under the last right-click, resolved before Radix opens the one
  // shared menu; the menu items read it at open time.
  const menuTargetRef = useRef<string | null>(null);

  // The bone filter only shapes the RENDERED tree; drag validity and reparenting
  // keep working against the unfiltered entities, so a drop can never corrupt the
  // real ancestry through a hidden joint.
  const roots = useMemo(
    () => buildTree(hideBones ? reanchorPastBones(entities) : entities),
    [entities, hideBones],
  );

  // Reveal an externally selected row (viewport pick, `sa select`): expand every
  // ancestor so the selection is never hidden inside a collapsed branch.
  const lastRevealed = useRef<string | null>(null);
  useEffect(() => {
    if (!selectedId || selectedId === lastRevealed.current) {
      return;
    }
    lastRevealed.current = selectedId;
    const parentOf = new Map(entities.map((e) => [e.id, e.parentId]));
    let cursor = parentOf.get(selectedId);
    for (let steps = 0; cursor && cursor !== "0" && steps <= entities.length; steps++) {
      setExpanded(cursor, true);
      cursor = parentOf.get(cursor);
    }
  }, [selectedId, entities, setExpanded]);

  const draggingSubtree = useMemo(
    () => (draggingId === null ? null : subtreeIds(entities, draggingId)),
    [draggingId, entities],
  );

  // Stable handlers (drop reads the live drag id from a ref), so the drag context
  // changes identity only while a drag is in flight — never on selection.
  const draggingIdRef = useRef<string | null>(null);
  draggingIdRef.current = draggingId;
  const begin = useCallback(
    (id: string): void => {
      setDraggingId(id);
      setDragActive(true);
    },
    [setDragActive],
  );
  const end = useCallback((): void => {
    setDraggingId(null);
    setDropTargetId(null);
    setDragActive(false);
  }, [setDragActive]);
  const drop = useCallback(
    (targetId: string | null): void => {
      const id = draggingIdRef.current;
      if (id) {
        actions.onReparent(id, targetId);
      }
      setDraggingId(null);
      setDropTargetId(null);
    },
    [actions],
  );

  const drag = useMemo<DragContext>(
    () => ({
      draggingId,
      dropTargetId,
      draggingSubtree,
      begin,
      end,
      setDropTarget: setDropTargetId,
      drop,
    }),
    [draggingId, dropTargetId, draggingSubtree, begin, end, drop],
  );

  const draggingEntity = draggingId ? entities.find((e) => e.id === draggingId) : undefined;

  return (
    <ContextMenu modal={false}>
      <ContextMenuTrigger
        asChild
        // Resolve the row under a right-click into the ref; an empty-area click
        // suppresses the menu (preventDefault makes Radix skip opening), so blank
        // space stays menu-less.
        onContextMenu={(event) => {
          const el =
            event.target instanceof Element ? event.target.closest("[data-entity-id]") : null;
          if (el) {
            menuTargetRef.current = el.getAttribute("data-entity-id");
          } else {
            menuTargetRef.current = null;
            event.preventDefault();
          }
        }}
      >
        <div
          className="py-1"
          role="tree"
          aria-label="Scene entities"
          // Dropping a model asset from the catalog instantiates it into the scene. Asset drags carry
          // `application/x-sa-asset`; entity-reparent drags (`application/x-saffron-entity`) are ignored
          // here and handled by the per-row / unparent drop zones.
          onDragOver={(e) => {
            if (e.dataTransfer.types.includes(ASSET_DND_MIME)) {
              e.preventDefault();
            }
          }}
          onDrop={(e) => {
            const ids = assetIdsFromPayload(readAssetPayload(e.dataTransfer));
            if (ids.length === 0) {
              return;
            }
            e.preventDefault();
            const state = useEditorStore.getState();
            const models = ids.filter(
              (id) => state.assets.find((asset) => asset.id === id)?.type === "model",
            );
            for (const id of models) {
              void state
                .instantiateModel(id)
                .then(() => notify("Added to scene"))
                .catch((err: unknown) => notifyError(errorText(err)));
            }
          }}
        >
          {entities.length === 0 ? (
            <div className="p-2.5 text-center italic text-muted-foreground">No entities</div>
          ) : (
            roots.map((node) => (
              <TreeRow key={node.entity.id} node={node} depth={0} actions={actions} drag={drag} />
            ))
          )}
          {draggingEntity?.parentId ? (
            <div
              className={cn(
                "mx-2 mt-1 rounded-md border border-dashed border-border px-2.5 py-1 text-center text-xs text-muted-foreground",
                dropTargetId === "" && "border-primary text-primary",
              )}
              onDragOver={(e) => {
                e.preventDefault();
                drag.setDropTarget("");
              }}
              onDragLeave={() => drag.setDropTarget(null)}
              onDrop={(e) => {
                e.preventDefault();
                drag.drop(null);
              }}
            >
              Drop here to unparent
            </div>
          ) : null}
        </div>
      </ContextMenuTrigger>
      {/* No focus restore on close: it lands after the inline rename input takes
          focus and would blur-cancel the edit. */}
      <ContextMenuContent className="min-w-36" onCloseAutoFocus={(event) => event.preventDefault()}>
        <RowContextMenuItems targetRef={menuTargetRef} actions={actions} />
      </ContextMenuContent>
    </ContextMenu>
  );
}

/// The items of the tree's one shared context menu, mounted by Radix at open time
/// (closed content is unmounted), so each open reads the target ref and recomputes
/// the valid "Parent to…" set fresh — never per row per render.
function RowContextMenuItems({
  targetRef,
  actions,
}: {
  targetRef: React.RefObject<string | null>;
  actions: TreeActions;
}) {
  const entities = useEditorStore((s) => s.entities);
  const id = targetRef.current;
  const entity = id ? entities.find((e) => e.id === id) : undefined;
  if (!entity) {
    return null;
  }
  // Valid "Parent to…" targets: anything outside this row's own subtree.
  const parentTargets = entities.filter(
    (e) => e.id !== entity.id && !isInSubtree(entities, entity.id, e.id),
  );
  return (
    <>
      <ContextMenuItem onSelect={() => actions.onFocus(entity.id)}>Focus</ContextMenuItem>
      <ContextMenuItem onSelect={() => actions.onRenameStart(entity.id)}>Rename</ContextMenuItem>
      <ContextMenuItem onSelect={() => actions.onCopy(entity.id)}>Copy</ContextMenuItem>
      <ContextMenuSeparator />
      <ContextMenuSub>
        <ContextMenuSubTrigger>Parent to…</ContextMenuSubTrigger>
        <ContextMenuSubContent className="max-h-64 min-w-36 overflow-y-auto">
          {parentTargets.length === 0 ? (
            <ContextMenuItem disabled>No valid parents</ContextMenuItem>
          ) : (
            parentTargets.map((target) => (
              <ContextMenuItem
                key={target.id}
                onSelect={() => actions.onReparent(entity.id, target.id)}
              >
                {target.name}
              </ContextMenuItem>
            ))
          )}
        </ContextMenuSubContent>
      </ContextMenuSub>
      {entity.parentId ? (
        <ContextMenuItem onSelect={() => actions.onReparent(entity.id, null)}>
          Unparent
        </ContextMenuItem>
      ) : null}
      <ContextMenuSeparator />
      <ContextMenuItem variant="destructive" onSelect={() => actions.onDelete(entity.id)}>
        Delete
      </ContextMenuItem>
    </>
  );
}

interface DragContext {
  draggingId: string | null;
  dropTargetId: string | null;
  /// The dragged entity's subtree (self + descendants), or null when idle.
  draggingSubtree: Set<string> | null;
  begin(id: string): void;
  end(): void;
  setDropTarget(id: string | null): void;
  drop(targetId: string | null): void;
}

const TreeRow = memo(function TreeRow({
  node,
  depth,
  actions,
  drag,
}: {
  node: TreeNode;
  depth: number;
  actions: TreeActions;
  drag: DragContext;
}) {
  logRender("TreeRow");
  const entity = node.entity;
  const selected = useEditorStore((s) => s.selectedId === entity.id);
  const expanded = useEditorStore((s) => s.expandedIds.has(entity.id));
  const toggleExpanded = useEditorStore((s) => s.toggleExpanded);
  // Read-only component leaves for the SELECTED row only, sourced from the inspect
  // result the poll keeps fresh. The count drives the twisty; the list lives in
  // ComponentSubrows so an inspect-poll update re-renders that node, not the row.
  // For every other row the gate short-circuits before the component walk.
  const subrowCount = useEditorStore((s) =>
    s.showComponentSubrows &&
    s.selectedId === entity.id &&
    s.componentsBySelected &&
    s.componentsBySelected.id === entity.id
      ? orderedComponentNames(
          s.componentsBySelected.components as Record<string, unknown>,
          s.componentsBySelected.componentOrder,
        ).length
      : 0,
  );
  const hasChildren = node.children.length > 0;
  const expandable = hasChildren || subrowCount > 0;
  const renaming = entity.id === actions.renamingId;

  // A row is a valid drop target unless it is the dragged row or inside its subtree.
  const validDropTarget = drag.draggingSubtree !== null && !drag.draggingSubtree.has(entity.id);

  return (
    <>
      {renaming ? (
        <div data-entity-id={entity.id} style={{ paddingLeft: depth * 14 }}>
          <RenameRow
            initial={entity.name}
            onCommit={(next) => actions.onRenameCommit(entity.id, next)}
            onCancel={actions.onRenameCancel}
          />
        </div>
      ) : (
        <div
          data-entity-id={entity.id}
          role="treeitem"
          aria-selected={selected}
          aria-expanded={expandable ? expanded : undefined}
          draggable
          tabIndex={0}
          className={cn(
            "flex w-full cursor-default items-center gap-1 py-0.5 pr-2 text-left text-xs",
            selected ? "bg-accent text-accent-foreground" : "text-foreground hover:bg-accent/40",
            drag.dropTargetId === entity.id && validDropTarget && "ring-1 ring-primary",
          )}
          style={{ paddingLeft: Math.min(depth, MAX_INDENT_DEPTH) * INDENT_PX + 8 }}
          onClick={() => actions.onSelect(entity)}
          onMouseDown={() => {
            if (actions.renamingId && actions.renamingId !== entity.id) {
              actions.onRenameCancel();
            }
          }}
          onDoubleClick={() => actions.onRenameStart(entity.id)}
          // The configured delete key on the focused row (clicking focuses it)
          // runs the same delete as the context menu item.
          onKeyDown={(e) => {
            if (matchesBinding(e, "hierarchy.delete", useEditorStore.getState().keyBindings)) {
              e.preventDefault();
              actions.onDelete(entity.id);
            }
          }}
          onDragStart={(e) => {
            e.dataTransfer.setData(DND_MIME, entity.id);
            e.dataTransfer.effectAllowed = "move";
            drag.begin(entity.id);
          }}
          onDragEnd={drag.end}
          onDragOver={(e) => {
            if (validDropTarget) {
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              drag.setDropTarget(entity.id);
            }
          }}
          onDragLeave={() => {
            if (drag.dropTargetId === entity.id) {
              drag.setDropTarget(null);
            }
          }}
          onDrop={(e) => {
            if (validDropTarget) {
              e.preventDefault();
              drag.drop(entity.id);
            }
          }}
        >
          {expandable ? (
            <button
              type="button"
              aria-label={expanded ? "Collapse" : "Expand"}
              className="flex size-4 flex-none items-center justify-center text-muted-foreground hover:text-foreground"
              onClick={(e) => {
                e.stopPropagation();
                toggleExpanded(entity.id);
              }}
            >
              {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
            </button>
          ) : (
            <span className="size-4 flex-none" />
          )}
          <span className="truncate">{entity.name}</span>
        </div>
      )}
      {hasChildren && expanded
        ? node.children.map((child) => (
            <TreeRow
              key={child.entity.id}
              node={child}
              depth={depth + 1}
              actions={actions}
              drag={drag}
            />
          ))
        : null}
      {expanded && subrowCount > 0 ? <ComponentSubrows entity={entity} depth={depth + 1} /> : null}
    </>
  );
});

/// The selected row's read-only component leaves. The only node subscribing to
/// `componentsBySelected`, so the ~6 Hz inspect poll re-renders this — not the tree.
function ComponentSubrows({ entity, depth }: { entity: EntityListEntry; depth: number }) {
  logRender("ComponentSubrows");
  const components = useEditorStore((s) => s.componentsBySelected);
  const names =
    components && components.id === entity.id
      ? orderedComponentNames(
          components.components as Record<string, unknown>,
          components.componentOrder,
        )
      : [];
  return (
    <>
      {names.map((name) => (
        <SubRow key={`${entity.id}:${name}`} entity={entity} name={name} depth={depth} />
      ))}
    </>
  );
}

/// A read-only component leaf under the selected entity: pure navigation, never an
/// edit surface and never a fetch. Clicking keeps the entity selected (local set
/// only — no control command), flips the bottom tab to the Inspector, and fires the
/// one-shot focus signal that scrolls the matching section into view.
function SubRow({ entity, name, depth }: { entity: EntityListEntry; name: string; depth: number }) {
  const selectEntity = useEditorStore((s) => s.selectEntity);
  const openPanel = useEditorStore((s) => s.openPanel);
  const setFocusComponent = useEditorStore((s) => s.setFocusComponent);
  return (
    <button
      type="button"
      role="treeitem"
      className="flex w-full items-center gap-1 py-0.5 pr-2 text-left text-xs italic text-muted-foreground hover:bg-accent/40"
      style={{ paddingLeft: Math.min(depth, MAX_INDENT_DEPTH) * INDENT_PX + 24 }}
      onClick={() => {
        selectEntity(entity.id);
        openPanel("inspector");
        setFocusComponent(name);
      }}
    >
      {name}
    </button>
  );
}

/// Inline rename input rendered in place of a row. Autofocuses and selects all,
/// commits on Enter or blur, cancels on Escape. Enter and Escape unmount the input,
/// which fires a native blur; a `settled` ref ensures the blur does not commit a second
/// time after Enter and does not commit at all after Escape.
export function RenameRow({
  initial,
  onCommit,
  onCancel,
}: {
  initial: string;
  onCommit(next: string): void;
  onCancel(): void;
}) {
  const [value, setValue] = useState(initial);
  const settled = useRef(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    return () => cancelAnimationFrame(frame);
  }, []);

  return (
    <Input
      ref={inputRef}
      value={value}
      className="h-6 px-2 py-1 text-xs"
      onChange={(e) => setValue(e.target.value)}
      onFocus={(e) => e.currentTarget.select()}
      onBlur={() => {
        if (settled.current) {
          return;
        }
        settled.current = true;
        onCommit(value);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (settled.current) {
            return;
          }
          settled.current = true;
          onCommit(value);
        } else if (e.key === "Escape") {
          e.preventDefault();
          settled.current = true;
          onCancel();
        }
      }}
    />
  );
}
