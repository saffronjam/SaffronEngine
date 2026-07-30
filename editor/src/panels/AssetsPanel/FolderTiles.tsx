import { memo, useEffect, useRef, useState } from "react";
import type { DragEvent as ReactDragEvent, MouseEvent as ReactMouseEvent } from "react";
import { Folder } from "lucide-react";
import {
  assetIdsFromPayload,
  isCatalogDrag,
  readAssetPayload,
  readFolderPayload,
} from "../../components/AssetTile";
import { logRender } from "../../lib/renderLog";
import { matchesBinding } from "../../lib/keybindings";
import { useOutsideCommit } from "../../lib/useOutsideCommit";
import { useEditorStore } from "../../state/store";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

/// Commit on Enter, blur, or an outside pointer press, exactly once per edit — the settle latch
/// stops the blur that follows an Enter from committing a second time.
function useNameCommit(
  inputRef: React.RefObject<HTMLInputElement | null>,
  onCommit: () => void,
): { commit: () => void; settledRef: React.RefObject<boolean> } {
  const settledRef = useRef(false);

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    return () => cancelAnimationFrame(frame);
  }, [inputRef]);

  const commit = (): void => {
    if (settledRef.current) {
      return;
    }
    settledRef.current = true;
    onCommit();
    window.setTimeout(() => {
      settledRef.current = false;
    }, 100);
  };

  useOutsideCommit(inputRef, commit);
  return { commit, settledRef };
}

export function NewFolderTile({
  value,
  invalid = false,
  onChange,
  onCommit,
  onCancel,
}: {
  value: string;
  invalid?: boolean;
  onChange(name: string): void;
  onCommit(name: string): void;
  onCancel(): void;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const { commit, settledRef } = useNameCommit(inputRef, () => onCommit(value));

  return (
    <div
      className="flex w-[86px] flex-col gap-1 rounded-md border border-ring bg-background p-1"
      data-asset-folder="true"
    >
      <div className="flex aspect-square w-full items-center justify-center">
        <Folder className="size-16 fill-current stroke-current text-yellow-500" />
      </div>
      <Input
        ref={inputRef}
        value={value}
        aria-invalid={invalid}
        className={cn(
          "h-6 rounded-sm px-1 py-0 text-center font-mono text-[13px]",
          invalid && "border-destructive ring-1 ring-destructive",
        )}
        onChange={(event) => onChange(event.currentTarget.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            commit();
          } else if (event.key === "Escape") {
            event.preventDefault();
            settledRef.current = true;
            onCancel();
          }
        }}
      />
    </div>
  );
}

export function FolderNameInput({
  initial,
  invalid = false,
  onChange,
  onCommit,
  onCancel,
}: {
  initial: string;
  invalid?: boolean;
  onChange?(): void;
  onCommit(name: string): void;
  onCancel(): void;
}) {
  const [value, setValue] = useState(initial);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const { commit, settledRef } = useNameCommit(inputRef, () => onCommit(value));

  return (
    <Input
      ref={inputRef}
      value={value}
      aria-invalid={invalid}
      className={cn(
        "h-5 rounded-sm px-1 py-0 text-center font-mono text-[11px]",
        invalid && "border-destructive ring-1 ring-destructive",
      )}
      onClick={(event) => event.stopPropagation()}
      onDoubleClick={(event) => event.stopPropagation()}
      onChange={(event) => {
        setValue(event.currentTarget.value);
        onChange?.();
      }}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          commit();
        } else if (event.key === "Escape") {
          event.preventDefault();
          settledRef.current = true;
          onCancel();
        }
      }}
    />
  );
}

export const ParentFolderTile = memo(function ParentFolderTile({
  target,
  onOpen,
  onMoveAssets,
  onMoveFolders,
}: {
  target: string | null;
  onOpen(folder: string | null): void;
  onMoveAssets(assetIds: string[], folder: string | null): void;
  onMoveFolders(paths: string[], parent: string | null): void;
}) {
  logRender("ParentFolderTile");
  const [dragActive, setDragActive] = useState(false);
  return (
    <div className="relative w-[86px]">
      <button
        type="button"
        data-asset-parent-folder="true"
        className={cn(
          "flex w-[86px] flex-col gap-1 rounded-md border border-transparent p-1 text-left transition-colors hover:border-ring hover:bg-accent/40",
          dragActive && "border-ring bg-accent/60 ring-1 ring-ring",
        )}
        onDoubleClick={() => onOpen(target)}
        onDragEnter={(event) => {
          if (isCatalogDrag(event.dataTransfer)) {
            setDragActive(true);
          }
        }}
        onDragOver={(event) => {
          if (isCatalogDrag(event.dataTransfer)) {
            event.preventDefault();
            event.dataTransfer.dropEffect = "move";
            setDragActive(true);
          }
        }}
        onDragLeave={() => setDragActive(false)}
        onDrop={(event) => {
          const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
          const folderPaths = readFolderPayload(event.dataTransfer);
          if (ids.length === 0 && folderPaths.length === 0) {
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          setDragActive(false);
          if (folderPaths.length > 0) {
            onMoveFolders(folderPaths, target);
          }
          if (ids.length > 0) {
            onMoveAssets(ids, target);
          }
        }}
      >
        <div className="flex aspect-square w-full items-center justify-center">
          <Folder className="size-16 fill-current stroke-current text-yellow-500" />
        </div>
        <span className="min-h-[2.5em] truncate px-0.5 text-center font-mono text-[13px] leading-tight text-foreground">
          ../
        </span>
      </button>
    </div>
  );
});

/// memo'd like AssetTile: the body passes one function identity per callback to every folder tile,
/// which binds its own `path` at call time.
export const FolderTile = memo(function FolderTile({
  path,
  name,
  editing = false,
  invalid = false,
  dragActive = false,
  onOpen,
  onSelect,
  onBeginDrag,
  onEndDrag,
  onAssetDropTarget,
  onMoveAssets,
  onMoveFolders,
  onDelete,
  onCommitRename,
  onChangeRename,
  onCancelRename,
}: {
  path: string;
  name: string;
  editing?: boolean;
  invalid?: boolean;
  dragActive?: boolean;
  onOpen(path: string): void;
  onSelect?(path: string, event: ReactMouseEvent): void;
  onBeginDrag(path: string, event: ReactDragEvent): void;
  onEndDrag(): void;
  /// Hover highlight for a catalog drag: enter/over report this tile's path, drop/drag-end clear.
  onAssetDropTarget(path: string | null): void;
  onMoveAssets(assetIds: string[], path: string): void;
  onMoveFolders?(paths: string[], path: string): void;
  onDelete?(path: string): void;
  onCommitRename?(path: string, name: string): void;
  onChangeRename?(): void;
  onCancelRename?(): void;
}) {
  logRender("FolderTile");
  const selected = useEditorStore((s) => s.selectedFolderPaths.has(path));
  const menuActive = useEditorStore(
    (s) => s.assetMenuTarget?.kind === "folder" && s.assetMenuTarget.key === path,
  );

  return (
    <div className="relative w-[86px]">
      <button
        type="button"
        data-asset-folder="true"
        data-asset-folder-path={path}
        className={cn(
          "flex w-[86px] flex-col gap-1 rounded-md border border-transparent p-1 text-left transition-colors",
          // Hover affordance only when not already highlighted, so a selected folder keeps its
          // appearance instead of stacking a second highlight. An open context menu holds that
          // same hover look so the target folder stays marked.
          !selected && !dragActive && "hover:border-ring hover:bg-accent/40",
          !selected && !dragActive && menuActive && "border-ring bg-accent/40",
          selected && "border-ring bg-accent/60 ring-1 ring-ring",
          dragActive && "border-ring bg-accent/60 ring-1 ring-ring",
        )}
        draggable={!editing}
        onClick={(event) => onSelect?.(path, event)}
        onDoubleClick={() => onOpen(path)}
        onKeyDown={(event) => {
          if (
            !editing &&
            onDelete &&
            matchesBinding(event, "assets.delete", useEditorStore.getState().keyBindings)
          ) {
            event.preventDefault();
            onDelete(path);
          }
        }}
        onDragStart={(event) => onBeginDrag(path, event)}
        onDragEnter={(event) => {
          if (isCatalogDrag(event.dataTransfer)) {
            onAssetDropTarget(path);
          }
        }}
        onDragOver={(event) => {
          if (isCatalogDrag(event.dataTransfer)) {
            event.preventDefault();
            event.dataTransfer.dropEffect = "move";
            onAssetDropTarget(path);
          }
        }}
        onDrop={(event) => {
          const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
          const folderPaths = readFolderPayload(event.dataTransfer);
          if (ids.length === 0 && folderPaths.length === 0) {
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          onAssetDropTarget(null);
          if (folderPaths.length > 0) {
            onMoveFolders?.(folderPaths, path);
          }
          if (ids.length > 0) {
            onMoveAssets(ids, path);
          }
        }}
        onDragEnd={() => {
          onEndDrag();
          onAssetDropTarget(null);
        }}
      >
        <div className="flex aspect-square w-full items-center justify-center">
          <Folder className="size-16 fill-current stroke-current text-yellow-500" />
        </div>
        {editing && onCommitRename && onCancelRename ? (
          <FolderNameInput
            initial={name}
            invalid={invalid}
            onChange={onChangeRename}
            onCommit={(next) => onCommitRename(path, next)}
            onCancel={onCancelRename}
          />
        ) : (
          <span className="line-clamp-2 min-h-[2.5em] break-words px-0.5 text-center text-[13px] leading-tight text-foreground">
            {name}
          </span>
        )}
      </button>
    </div>
  );
});
