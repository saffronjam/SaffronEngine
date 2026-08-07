/// The Hierarchy panel: the tree outliner over the scene entities, refreshed by the reconcile poll
/// on a sceneVersion change — this component never fetches. Every drag affordance stays in sidebar
/// DOM, because the viewport surface paints over anything floating.
import { useCallback, useMemo, useState } from "react";
import { Bone, ListTree } from "lucide-react";
import { client } from "../control/client";
import { recordEntityCreation, useEditorStore } from "../state/store";
import { CreateMenu } from "../app/CreateMenu";
import { errorText, notifyError } from "../lib/flash";
import type { EntityListEntry } from "../protocol";
import { HierarchyTree, type TreeActions } from "./HierarchyTree";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { logRender } from "../lib/renderLog";

export function HierarchyPanel() {
  logRender("HierarchyPanel");
  const selectEntity = useEditorStore((s) => s.selectEntity);
  const setSelectedId = useEditorStore((s) => s.setSelectedId);
  const setParent = useEditorStore((s) => s.setParent);
  const applyOptimisticEntityName = useEditorStore((s) => s.applyOptimisticEntityName);
  const showComponentSubrows = useEditorStore((s) => s.showComponentSubrows);
  const toggleComponentSubrows = useEditorStore((s) => s.toggleComponentSubrows);
  const hideBones = useEditorStore((s) => s.hideBones);
  const toggleHideBones = useEditorStore((s) => s.toggleHideBones);
  const [renamingId, setRenamingId] = useState<string | null>(null);

  // Left-click a row: optimistic local select, then tell the engine. The poll
  // confirms via selectionVersion.
  const onSelect = useCallback(
    (entity: EntityListEntry): void => {
      selectEntity(entity.id);
      void client.selectEntity(entity.id).catch((err: unknown) => notifyError(errorText(err)));
    },
    [selectEntity],
  );
  // Aim the editor camera at the entity.
  const onFocus = useCallback((id: string): void => {
    void client.focus(id).catch((err: unknown) => notifyError(errorText(err)));
  }, []);
  // Copy duplicates the entity; the engine selects the dup, so mirror it locally
  // and let the sceneVersion bump refresh the list.
  const onCopy = useCallback(
    (id: string): void => {
      void client
        .copyEntity(id)
        .then((ref) => {
          selectEntity(ref.id);
          recordEntityCreation(ref.id, "Duplicate entity");
        })
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [selectEntity],
  );
  // Delete removes the entity and its subtree; clear selection if it was selected.
  const onDelete = useCallback(
    (id: string): void => {
      if (useEditorStore.getState().selectedId === id) {
        setSelectedId(null);
      }
      void client.destroyEntity(id).catch((err: unknown) => notifyError(errorText(err)));
    },
    [setSelectedId],
  );
  // Reparent (drag-drop or the context menu); the store action relinks
  // optimistically and rolls back on rejection — surface the error here.
  const onReparent = useCallback(
    (id: string, parentId: string | null): void => {
      void setParent(id, parentId).catch((err: unknown) => notifyError(errorText(err)));
    },
    [setParent],
  );
  const onRenameStart = useCallback((id: string): void => setRenamingId(id), []);
  // Inline rename: optimistically update the row name and commit via rename-entity;
  // the sceneVersion bump re-fetches the authoritative list. A rejection reverts to
  // the next poll's value and surfaces via the error toast.
  const onRenameCommit = useCallback(
    (id: string, next: string): void => {
      setRenamingId(null);
      const trimmed = next.trim();
      if (trimmed === "") {
        return;
      }
      const prior = useEditorStore.getState().entities.find((e) => e.id === id)?.name;
      applyOptimisticEntityName(id, trimmed);
      void client
        .renameEntity(id, trimmed)
        .then(() => {
          if (prior !== undefined && prior !== trimmed) {
            useEditorStore.getState().pushEdit(
              {
                label: "Rename",
                selectionId: id,
                undo: () => client.renameEntity(id, prior),
                redo: () => client.renameEntity(id, trimmed),
              },
              "scene",
            );
          }
        })
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [applyOptimisticEntityName],
  );
  const onRenameCancel = useCallback((): void => setRenamingId(null), []);

  // Stable across renders except when renamingId changes (rename start/commit, rare),
  // so a selection change never re-renders the tree through this prop.
  const actions = useMemo<TreeActions>(
    () => ({
      onSelect,
      onFocus,
      onCopy,
      onDelete,
      onReparent,
      renamingId,
      onRenameStart,
      onRenameCommit,
      onRenameCancel,
    }),
    [
      onSelect,
      onFocus,
      onCopy,
      onDelete,
      onReparent,
      renamingId,
      onRenameStart,
      onRenameCommit,
      onRenameCancel,
    ],
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-10 flex-none items-center justify-end border-b border-border px-3">
        <div className="flex items-center gap-1">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                aria-pressed={hideBones}
                className={cn(hideBones ? "bg-accent text-foreground" : "text-muted-foreground")}
                onClick={toggleHideBones}
              >
                <Bone />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Hide skeleton bones in the tree</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                aria-pressed={showComponentSubrows}
                className={cn(
                  showComponentSubrows ? "bg-accent text-foreground" : "text-muted-foreground",
                )}
                onClick={toggleComponentSubrows}
              >
                <ListTree />
              </Button>
            </TooltipTrigger>
            <TooltipContent>Show the selected entity's components as tree rows</TooltipContent>
          </Tooltip>
          <CreateMenu />
        </div>
      </div>
      <ScrollArea
        className="min-h-0 flex-1"
        onClick={(e) => {
          // A click in the empty space below/around the rows clears the selection,
          // mirroring the Escape shortcut. Row clicks land inside a treeitem.
          if ((e.target as Element).closest('[role="treeitem"]')) {
            return;
          }
          setSelectedId(null);
          void client.deselect().catch((err: unknown) => notifyError(errorText(err)));
        }}
      >
        <HierarchyTree actions={actions} />
      </ScrollArea>
    </div>
  );
}
