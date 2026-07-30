/// The Assets panel's folder sidebar: a pinned Root row plus the virtual-folder forest built from
/// the flat `assetFolders` path list. Expand state is local, with an ancestor-reveal effect so tile,
/// breadcrumb, and history navigation never lands inside a collapsed branch.
import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronRight, Folder, FolderPlus, Pen, Trash } from "lucide-react";
import {
  assetIdsFromPayload,
  isCatalogDrag,
  readAssetPayload,
  readFolderPayload,
} from "../components/AssetTile";
import { useOutsideCommit } from "../lib/useOutsideCommit";
import { useEditorStore } from "../state/store";
import { matchesBinding } from "../lib/keybindings";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import { logRender } from "../lib/renderLog";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

/// One node of the virtual-folder forest; `path` is the full slash-joined path.
export interface FolderNode {
  path: string;
  name: string;
  children: FolderNode[];
}

/// The last path segment of a folder path.
export function folderLabel(folder: string): string {
  const slash = folder.lastIndexOf("/");
  return slash >= 0 ? folder.slice(slash + 1) : folder;
}

/// Every prefix path of `folder`, shortest first, including `folder` itself.
export function folderAncestorPaths(folder: string): string[] {
  const segments = folder.split("/");
  const paths: string[] = [];
  for (let i = 1; i <= segments.length; i += 1) {
    paths.push(segments.slice(0, i).join("/"));
  }
  return paths;
}

/// Build the sorted folder forest from the flat path list, synthesizing any
/// intermediate node the list does not carry explicitly.
export function buildFolderTree(folders: string[]): FolderNode[] {
  const roots: FolderNode[] = [];
  const byPath = new Map<string, FolderNode>();
  const nodeFor = (path: string): FolderNode => {
    const existing = byPath.get(path);
    if (existing) {
      return existing;
    }
    const node: FolderNode = { path, name: folderLabel(path), children: [] };
    byPath.set(path, node);
    const slash = path.lastIndexOf("/");
    if (slash >= 0) {
      nodeFor(path.slice(0, slash)).children.push(node);
    } else {
      roots.push(node);
    }
    return node;
  };
  for (const folder of folders) {
    nodeFor(folder);
  }
  const sortBranch = (nodes: FolderNode[]): void => {
    nodes.sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }),
    );
    for (const node of nodes) {
      sortBranch(node.children);
    }
  };
  sortBranch(roots);
  return roots;
}

export interface FolderTreeProps {
  folders: string[];
  currentFolder: string | null;
  /// The folder being renamed inline in the TREE (the panel keeps grid-initiated
  /// renames on the grid tile instead).
  renamingFolder: string | null;
  renameInvalid: boolean;
  /// A tree-initiated creation: an inline name row under `parent` (null = root).
  creatingIn: { parent: string | null } | null;
  createInvalid: boolean;
  onNavigate(folder: string | null): void;
  onMoveAssets(assetIds: string[], folder: string | null): void;
  onMoveFolders(paths: string[], parent: string | null): void;
  onNewFolder(parent: string | null): void;
  onStartRename(folder: string): void;
  onChangeRename(): void;
  onCommitRename(folder: string, name: string): void;
  onCancelRename(): void;
  onChangeCreate(): void;
  onCommitCreate(name: string): void;
  onCancelCreate(): void;
  onDelete(folder: string): void;
}

/// One row of the visible (expansion-respecting) DFS order, for arrow-key walks.
interface VisibleRow {
  path: string;
  hasChildren: boolean;
}

function flattenVisible(nodes: FolderNode[], expanded: Set<string>, out: VisibleRow[]): void {
  for (const node of nodes) {
    out.push({ path: node.path, hasChildren: node.children.length > 0 });
    if (node.children.length > 0 && expanded.has(node.path)) {
      flattenVisible(node.children, expanded, out);
    }
  }
}

export function AssetFolderTree(props: FolderTreeProps) {
  logRender("AssetFolderTree");
  const { folders, currentFolder } = props;
  const creatingParent = props.creatingIn?.parent ?? null;
  const roots = useMemo(() => buildFolderTree(folders), [folders]);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  // The hovered asset-drop row, by folder path.
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  // Reveal externally driven navigation (grid tiles, breadcrumbs, back/forward):
  // expand the current folder and every ancestor.
  useEffect(() => {
    if (!currentFolder) {
      return;
    }
    setExpanded((current) => {
      const next = new Set(current);
      for (const path of folderAncestorPaths(currentFolder)) {
        next.add(path);
      }
      return next.size === current.size ? current : next;
    });
  }, [currentFolder]);

  // Reveal an inline creation row: its parent branch must be open to show it.
  useEffect(() => {
    if (!creatingParent) {
      return;
    }
    setExpanded((current) => {
      const next = new Set(current);
      for (const path of folderAncestorPaths(creatingParent)) {
        next.add(path);
      }
      return next.size === current.size ? current : next;
    });
  }, [creatingParent]);

  const toggleExpanded = (path: string): void => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  const tree: TreeContext = {
    ...props,
    expanded,
    toggleExpanded,
    dropTarget,
    setDropTarget,
  };

  // Arrow-key walk over the visible rows: Up/Down move the selection, Right
  // expands (then descends), Left collapses (then ascends, to Root at the top).
  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>): void => {
    if (event.target instanceof HTMLInputElement) {
      return;
    }
    const rows: VisibleRow[] = [];
    flattenVisible(roots, expanded, rows);
    if (rows.length === 0) {
      return;
    }
    const index = rows.findIndex((row) => row.path === currentFolder);
    const row = index >= 0 ? rows[index] : null;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      const next = rows[Math.min(index + 1, rows.length - 1)];
      props.onNavigate(next.path);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      props.onNavigate(index > 0 ? rows[index - 1].path : null);
    } else if (event.key === "ArrowRight" && row) {
      event.preventDefault();
      if (row.hasChildren && !expanded.has(row.path)) {
        toggleExpanded(row.path);
      } else if (row.hasChildren && index + 1 < rows.length) {
        props.onNavigate(rows[index + 1].path);
      }
    } else if (event.key === "ArrowLeft" && row) {
      event.preventDefault();
      if (row.hasChildren && expanded.has(row.path)) {
        toggleExpanded(row.path);
      } else {
        const slash = row.path.lastIndexOf("/");
        props.onNavigate(slash >= 0 ? row.path.slice(0, slash) : null);
      }
    }
  };

  return (
    // The focusable surface is the whole sidebar (it owns the ScrollArea), so a
    // click anywhere in the box — not just on a row — arms the arrow keys.
    <div
      ref={containerRef}
      className="h-full min-h-0 outline-none"
      tabIndex={0}
      onKeyDown={onKeyDown}
      onPointerDown={(event) => {
        if (!(event.target instanceof HTMLInputElement)) {
          containerRef.current?.focus();
        }
      }}
    >
      <ScrollArea className="h-full">
        <div className="p-1" role="tree" aria-label="Asset folders">
          <RootRow tree={tree} />
          {props.creatingIn && props.creatingIn.parent === null ? (
            <NewFolderRow depth={1} tree={tree} />
          ) : null}
          {roots.map((node) => (
            <FolderRow key={node.path} node={node} depth={1} tree={tree} />
          ))}
        </div>
      </ScrollArea>
    </div>
  );
}

/// Drop-highlight identity for the pinned Root row (an empty string is never a valid
/// folder path, so it can't collide with a real node).
const ROOT_ROW_KEY = "";

/// The pinned Root row at the top of the tree: the common parent of every top-level
/// folder (which render indented beneath it). Selects the catalog root on click, is a
/// move-to-root drop target, and offers New Folder — but is virtual, so it cannot be
/// renamed or deleted.
function RootRow({ tree }: { tree: TreeContext }) {
  const selected = tree.currentFolder === null;
  const row = (
    <button
      type="button"
      role="treeitem"
      aria-selected={selected}
      className={rowStateClass(selected, tree.dropTarget === ROOT_ROW_KEY)}
      style={{ paddingLeft: 4 }}
      onClick={() => tree.onNavigate(null)}
      {...dropHandlers(tree, ROOT_ROW_KEY, null)}
    >
      <span className="size-4 flex-none" />
      <Folder className="size-3.5 flex-none opacity-70" />
      <span className="truncate">Root</span>
    </button>
  );
  return (
    <div className="relative" onContextMenu={(event) => event.stopPropagation()}>
      <ContextMenu>
        <ContextMenuTrigger asChild>{row}</ContextMenuTrigger>
        <ContextMenuContent
          className="min-w-36"
          onCloseAutoFocus={(event) => event.preventDefault()}
        >
          <ContextMenuItem onSelect={() => tree.onNewFolder(null)}>
            <FolderPlus />
            New Folder
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
    </div>
  );
}

/// The inline name input for a tree-initiated folder creation, indented like a
/// child row of its parent.
function NewFolderRow({ depth, tree }: { depth: number; tree: TreeContext }) {
  return (
    <div className={rowClass} style={{ paddingLeft: depth * 14 + 4 }}>
      <span className="size-4 flex-none" />
      <Folder className="size-3.5 flex-none opacity-70" />
      <FolderRenameInput
        initial=""
        invalid={tree.createInvalid}
        onChange={tree.onChangeCreate}
        onCommit={tree.onCommitCreate}
        onCancel={tree.onCancelCreate}
      />
    </div>
  );
}

interface TreeContext extends FolderTreeProps {
  expanded: Set<string>;
  toggleExpanded(path: string): void;
  dropTarget: string | null;
  setDropTarget(target: string | null): void;
}

/// Drag-drop handlers making a row an asset/folder-move target; `key` is the row's
/// highlight identity and `folder` the move destination.
function dropHandlers(tree: TreeContext, key: string, folder: string | null) {
  return {
    onDragEnter: (event: React.DragEvent): void => {
      if (isCatalogDrag(event.dataTransfer)) {
        tree.setDropTarget(key);
      }
    },
    onDragOver: (event: React.DragEvent): void => {
      if (isCatalogDrag(event.dataTransfer)) {
        event.preventDefault();
        event.dataTransfer.dropEffect = "move";
        tree.setDropTarget(key);
      }
    },
    onDragLeave: (): void => {
      if (tree.dropTarget === key) {
        tree.setDropTarget(null);
      }
    },
    onDrop: (event: React.DragEvent): void => {
      const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
      const draggedFolders = readFolderPayload(event.dataTransfer);
      if (ids.length === 0 && draggedFolders.length === 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      tree.setDropTarget(null);
      if (draggedFolders.length > 0) {
        tree.onMoveFolders(draggedFolders, folder);
      }
      if (ids.length > 0) {
        tree.onMoveAssets(ids, folder);
      }
    },
  };
}

// outline-none: the tree container owns keyboard interaction; rows never show the
// UA focus ring (selection is the bg highlight).
const rowClass =
  "flex w-full items-center gap-1 truncate rounded-md px-1 py-1 text-left text-xs outline-none";

function rowStateClass(selected: boolean, dropActive: boolean): string {
  return cn(
    rowClass,
    selected ? "bg-primary text-primary-foreground" : "text-foreground hover:bg-accent",
    // A drop-active row shows a ring as the affordance; the translucent fill is gated to
    // unselected rows so it never replaces the selected row's solid bg and matched text,
    // which would wash the label out.
    dropActive && "ring-1 ring-ring",
    dropActive && !selected && "bg-accent/60",
  );
}

function FolderRow({ node, depth, tree }: { node: FolderNode; depth: number; tree: TreeContext }) {
  const selected = tree.currentFolder === node.path;
  const expanded = tree.expanded.has(node.path);
  const hasChildren = node.children.length > 0;
  const renaming = tree.renamingFolder === node.path;
  const rowRef = useRef<HTMLButtonElement | null>(null);

  // Keep the selection visible during arrow-key walks through a scrolled tree.
  useEffect(() => {
    if (selected) {
      rowRef.current?.scrollIntoView({ block: "nearest" });
    }
  }, [selected]);

  const row = renaming ? (
    <div className={rowClass} style={{ paddingLeft: depth * 14 + 4 }}>
      <Folder className="size-3.5 flex-none opacity-70" />
      <FolderRenameInput
        initial={node.name}
        invalid={tree.renameInvalid}
        onChange={tree.onChangeRename}
        onCommit={(name) => tree.onCommitRename(node.path, name)}
        onCancel={tree.onCancelRename}
      />
    </div>
  ) : (
    <button
      ref={rowRef}
      type="button"
      role="treeitem"
      aria-selected={selected}
      aria-expanded={hasChildren ? expanded : undefined}
      className={rowStateClass(selected, tree.dropTarget === node.path)}
      style={{ paddingLeft: depth * 14 + 4 }}
      onClick={() => tree.onNavigate(node.path)}
      // The configured delete key on the focused row runs the same delete as the
      // context menu.
      onKeyDown={(event) => {
        if (matchesBinding(event, "assets.delete", useEditorStore.getState().keyBindings)) {
          event.preventDefault();
          tree.onDelete(node.path);
        }
      }}
      {...dropHandlers(tree, node.path, node.path)}
    >
      {hasChildren ? (
        <span
          role="button"
          aria-label={expanded ? "Collapse" : "Expand"}
          className="flex size-4 flex-none items-center justify-center rounded hover:bg-foreground/10"
          onClick={(event) => {
            event.stopPropagation();
            tree.toggleExpanded(node.path);
          }}
        >
          <ChevronRight className={cn("size-3.5 transition-transform", expanded && "rotate-90")} />
        </span>
      ) : (
        <span className="size-4 flex-none" />
      )}
      <Folder className="size-3.5 flex-none opacity-70" />
      <span className="truncate">{node.name}</span>
    </button>
  );

  return (
    <>
      <div className="relative" onContextMenu={(event) => event.stopPropagation()}>
        <ContextMenu>
          <ContextMenuTrigger asChild>{row}</ContextMenuTrigger>
          {/* No focus restore on close: it lands after the inline input takes focus
              (the menu unmounts post-exit-animation) and would blur-cancel the edit. */}
          <ContextMenuContent
            className="min-w-36"
            onCloseAutoFocus={(event) => event.preventDefault()}
          >
            <ContextMenuItem onSelect={() => tree.onNewFolder(node.path)}>
              <FolderPlus />
              New Folder
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => tree.onStartRename(node.path)}>
              <Pen />
              Rename
            </ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem
              variant="destructive"
              className="bg-destructive/10 text-destructive focus:bg-destructive focus:text-destructive-foreground"
              onSelect={() => tree.onDelete(node.path)}
            >
              <Trash />
              Delete
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      </div>
      {tree.creatingIn?.parent === node.path ? (
        <NewFolderRow depth={depth + 1} tree={tree} />
      ) : null}
      {hasChildren && expanded
        ? node.children.map((child) => (
            <FolderRow key={child.path} node={child} depth={depth + 1} tree={tree} />
          ))
        : null}
    </>
  );
}

/// Inline rename input rendered in place of a folder row: autofocus + select-all,
/// Enter/blur commits once (the `settled` ref absorbs the unmount blur), Escape
/// cancels.
function FolderRenameInput({
  initial,
  invalid,
  onChange,
  onCommit,
  onCancel,
}: {
  initial: string;
  invalid: boolean;
  onChange(): void;
  onCommit(name: string): void;
  onCancel(): void;
}) {
  const [value, setValue] = useState(initial);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const settledRef = useRef(false);

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
    return () => cancelAnimationFrame(frame);
  }, []);

  const commit = (): void => {
    if (settledRef.current) {
      return;
    }
    settledRef.current = true;
    onCommit(value);
    window.setTimeout(() => {
      settledRef.current = false;
    }, 100);
  };

  useOutsideCommit(inputRef, commit);

  return (
    <Input
      ref={inputRef}
      value={value}
      aria-invalid={invalid}
      className={cn(
        "h-5 min-w-0 flex-1 rounded-sm px-1 py-0 font-mono text-[11px]",
        invalid && "border-destructive ring-1 ring-destructive",
      )}
      onClick={(event) => event.stopPropagation()}
      onChange={(event) => {
        setValue(event.currentTarget.value);
        onChange();
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
