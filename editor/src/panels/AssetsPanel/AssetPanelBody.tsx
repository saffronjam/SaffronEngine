import { memo, useCallback, useRef } from "react";
import type {
  DragEvent as ReactDragEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import { AssetTile } from "../../components/AssetTile";
import {
  assetIdsFromPayload,
  isCatalogDrag,
  readAssetPayload,
  readFolderPayload,
} from "../../components/AssetTile";
import { logRender } from "../../lib/renderLog";
import { useEditorStore } from "../../state/store";
import type { AssetEntry } from "../../protocol";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import { parentFolderPath, sortedFolderItems } from "./catalog";
import { FolderTile, NewFolderTile, ParentFolderTile } from "./FolderTiles";

/// A folder rename initiated from the grid or the tree; the origin picks which surface renders the
/// inline input so the two never both mount.
export interface FolderActionTarget {
  path: string;
  origin: "grid" | "tree";
}

interface RectLike {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/// Drag-local marquee state, kept in a ref rather than React state: the box position is written
/// straight to the DOM and the hit test runs against rects cached at drag start, so a pointer move
/// renders nothing by itself — only an actual change in the hit set reaches the store.
interface MarqueeDrag {
  startX: number;
  startY: number;
  currentX: number;
  currentY: number;
  panelLeft: number;
  panelTop: number;
  viewport: HTMLElement | null;
  scrollTop: number;
  tiles: { kind: "asset" | "folder"; key: string; rect: RectLike }[];
  lastHits: string;
  raf: number;
}

function marqueeRect(marquee: MarqueeDrag): RectLike {
  return {
    left: Math.min(marquee.startX, marquee.currentX),
    top: Math.min(marquee.startY, marquee.currentY),
    right: Math.max(marquee.startX, marquee.currentX),
    bottom: Math.max(marquee.startY, marquee.currentY),
  };
}

function rectsIntersect(a: RectLike, b: RectLike): boolean {
  return a.left <= b.right && a.right >= b.left && a.top <= b.bottom && a.bottom >= b.top;
}

/// memo'd with every callback prop stable (the panel hoists them through useCallback), so panel
/// renders that change nothing for the grid skip it.
export const AssetPanelBody = memo(function AssetPanelBody({
  assets,
  folders,
  currentFolder,
  searchText,
  dropActive,
  creatingFolder,
  creatingFolderName,
  renamingFolder,
  renamingAsset,
  folderError,
  assetDropTarget,
  onOpenFolder,
  onView,
  onSelectAsset,
  onSelectFolder,
  onBeginDrag,
  onEndDrag,
  onDeleteAsset,
  onDeleteFolder,
  onMoveAssets,
  onMoveFolders,
  onCommitNewFolder,
  onChangeNewFolderName,
  onCancelNewFolder,
  onCommitRenameFolder,
  onCancelRenameFolder,
  onAssetDropTarget,
  onClearFolderError,
  onRenameEnd,
}: {
  assets: AssetEntry[];
  folders: string[];
  currentFolder: string | null;
  searchText: string;
  dropActive: boolean;
  creatingFolder: boolean;
  creatingFolderName: string;
  renamingFolder: FolderActionTarget | null;
  renamingAsset: string | null;
  folderError: string | null;
  assetDropTarget: string | null;
  onOpenFolder(folder: string | null): void;
  onView(asset: AssetEntry): void;
  onSelectAsset(asset: AssetEntry, event: ReactMouseEvent): void;
  onSelectFolder(folder: string, event: ReactMouseEvent): void;
  onBeginDrag(kind: "asset" | "folder", key: string, event: ReactDragEvent): void;
  onEndDrag(): void;
  onDeleteAsset(asset: AssetEntry): void;
  onDeleteFolder(folder: string): void;
  onMoveAssets(assetIds: string[], folder: string | null): void;
  onMoveFolders(paths: string[], parent: string | null): void;
  onCommitNewFolder(name: string): void;
  onChangeNewFolderName(name: string): void;
  onCancelNewFolder(): void;
  onCommitRenameFolder(folder: string, name: string): void;
  onCancelRenameFolder(): void;
  onAssetDropTarget(folder: string | null): void;
  onClearFolderError(): void;
  onRenameEnd(): void;
}) {
  logRender("AssetPanelBody");
  const folderItems = sortedFolderItems(folders, currentFolder, creatingFolder, searchText);
  const parentFolder = currentFolder !== null ? parentFolderPath(currentFolder) : null;
  const hasParentTile = currentFolder !== null;
  const searching = searchText.length > 0;
  const blank =
    !hasParentTile && !creatingFolder && folderItems.length === 0 && assets.length === 0;
  const panelRef = useRef<HTMLDivElement | null>(null);
  const boxRef = useRef<HTMLDivElement | null>(null);
  const marqueeRef = useRef<MarqueeDrag | null>(null);
  const setAssetSelection = useEditorStore((s) => s.setAssetSelection);
  const setAssetMarqueeActive = useEditorStore((s) => s.setAssetMarqueeActive);
  const marqueeActive = useEditorStore((s) => s.assetMarqueeActive);

  // Per-kind bindings of the panel's beginDrag, made once so every tile of a kind shares one
  // identity.
  const beginAssetDrag = useCallback(
    (entry: AssetEntry, event: ReactDragEvent): void => onBeginDrag("asset", entry.id, event),
    [onBeginDrag],
  );
  const beginFolderDrag = useCallback(
    (path: string, event: ReactDragEvent): void => onBeginDrag("folder", path, event),
    [onBeginDrag],
  );

  // Snapshot every tile's client rect in one pass, at drag start and again if the grid scrolls
  // mid-drag, so the per-frame hit test never reads layout.
  const snapshotMarqueeTiles = (drag: MarqueeDrag): void => {
    const panel = panelRef.current;
    if (!panel) {
      return;
    }
    drag.scrollTop = drag.viewport?.scrollTop ?? 0;
    drag.tiles = [];
    for (const el of panel.querySelectorAll<HTMLElement>("[data-asset-tile-id]")) {
      drag.tiles.push({
        kind: "asset",
        key: el.dataset.assetTileId ?? "",
        rect: el.getBoundingClientRect(),
      });
    }
    for (const el of panel.querySelectorAll<HTMLElement>("[data-asset-folder-path]")) {
      drag.tiles.push({
        kind: "folder",
        key: el.dataset.assetFolderPath ?? "",
        rect: el.getBoundingClientRect(),
      });
    }
  };

  // The per-frame marquee step: position the box via direct style writes (no re-render) and
  // hit-test against the cached rects, propagating the selection only when the hit set changed.
  // Runs at most once per animation frame regardless of the pointer's event rate.
  const applyMarquee = (): void => {
    const drag = marqueeRef.current;
    if (!drag) {
      return;
    }
    drag.raf = 0;
    if (drag.viewport && drag.viewport.scrollTop !== drag.scrollTop) {
      snapshotMarqueeTiles(drag);
    }
    const rect = marqueeRect(drag);
    const box = boxRef.current;
    if (box) {
      box.style.left = `${rect.left - drag.panelLeft}px`;
      box.style.top = `${rect.top - drag.panelTop}px`;
      box.style.width = `${rect.right - rect.left}px`;
      box.style.height = `${rect.bottom - rect.top}px`;
    }
    const assetIds: string[] = [];
    const folderPaths: string[] = [];
    for (const tile of drag.tiles) {
      if (rectsIntersect(rect, tile.rect)) {
        (tile.kind === "asset" ? assetIds : folderPaths).push(tile.key);
      }
    }
    const hits = `${assetIds.join("\n")}\0${folderPaths.join("\n")}`;
    if (hits !== drag.lastHits) {
      drag.lastHits = hits;
      setAssetSelection(assetIds, folderPaths);
    }
  };

  const startMarquee = (event: ReactPointerEvent<HTMLDivElement>): void => {
    if (event.button !== 0) {
      return;
    }
    const target = event.target;
    // Portaled overlays (context menus) bubble here through the React tree but are not DOM
    // descendants; capturing their pointer would swallow the item's click.
    if (!(target instanceof Element) || !event.currentTarget.contains(target)) {
      return;
    }
    if (target.closest("[data-asset-item='true'], [data-asset-folder='true'], button, input")) {
      return;
    }
    event.preventDefault();
    // preventDefault suppresses the click's default focus, so an empty-grid press would leave the
    // panel unfocused and Ctrl+F (handled on the panel root) dead.
    event.currentTarget.closest<HTMLElement>("[data-asset-panel]")?.focus({ preventScroll: true });
    event.currentTarget.setPointerCapture(event.pointerId);
    const panelRect = event.currentTarget.getBoundingClientRect();
    const drag: MarqueeDrag = {
      startX: event.clientX,
      startY: event.clientY,
      currentX: event.clientX,
      currentY: event.clientY,
      panelLeft: panelRect.left,
      panelTop: panelRect.top,
      viewport: event.currentTarget.querySelector("[data-radix-scroll-area-viewport]"),
      scrollTop: 0,
      tiles: [],
      lastHits: "\0",
      raf: 0,
    };
    snapshotMarqueeTiles(drag);
    marqueeRef.current = drag;
    setAssetMarqueeActive(true);
    setAssetSelection([], []);
  };

  const moveMarquee = (event: ReactPointerEvent<HTMLDivElement>): void => {
    const drag = marqueeRef.current;
    if (!drag) {
      return;
    }
    drag.currentX = event.clientX;
    drag.currentY = event.clientY;
    if (drag.raf === 0) {
      drag.raf = requestAnimationFrame(applyMarquee);
    }
  };

  const endMarquee = (event: ReactPointerEvent<HTMLDivElement>): void => {
    const drag = marqueeRef.current;
    if (!drag) {
      return;
    }
    if (drag.raf !== 0) {
      cancelAnimationFrame(drag.raf);
      drag.raf = 0;
    }
    // Flush the final position so a fast flick-release still selects what the pointer covered.
    applyMarquee();
    marqueeRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setAssetMarqueeActive(false);
  };

  return (
    <div
      ref={panelRef}
      className={cn(
        "relative h-full min-h-0",
        dropActive && "rounded-sm ring-2 ring-inset ring-ring",
      )}
      onPointerDown={startMarquee}
      onPointerMove={moveMarquee}
      onPointerUp={endMarquee}
      onPointerCancel={endMarquee}
      onDragOver={(event) => {
        if (isCatalogDrag(event.dataTransfer)) {
          event.preventDefault();
          event.dataTransfer.dropEffect = "move";
          const target = event.target;
          if (!(target instanceof Element) || !target.closest("[data-asset-folder='true']")) {
            onAssetDropTarget(null);
          }
        }
      }}
      onDragLeave={(event) => {
        // dragleave bubbles on every interior element boundary and the webview leaves
        // relatedTarget null, so a containment check would clear the drop target on each
        // micro-move and flicker the folder highlight. dragover keeps the target current while
        // inside, so clear only once the pointer is genuinely outside the panel bounds.
        const rect = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX >= rect.left &&
          event.clientX < rect.right &&
          event.clientY >= rect.top &&
          event.clientY < rect.bottom
        ) {
          return;
        }
        onAssetDropTarget(null);
      }}
      onDrop={(event) => {
        const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
        const folderPaths = readFolderPayload(event.dataTransfer);
        if (ids.length === 0 && folderPaths.length === 0) {
          return;
        }
        event.preventDefault();
        onAssetDropTarget(null);
        if (folderPaths.length > 0) {
          onMoveFolders(folderPaths, currentFolder);
        }
        if (ids.length > 0) {
          onMoveAssets(ids, currentFolder);
        }
      }}
    >
      <ScrollArea className="h-full">
        <div className="min-h-full p-2">
          {blank ? (
            <p className="px-1 py-3 text-center text-xs italic text-muted-foreground">
              {searching
                ? "No assets match your search."
                : "No assets yet. Import or drag-and-drop a model or texture."}
            </p>
          ) : (
            <div
              className="grid gap-2"
              style={{ gridTemplateColumns: "repeat(auto-fill, minmax(86px, 1fr))" }}
            >
              {hasParentTile ? (
                <ParentFolderTile
                  target={parentFolder}
                  onOpen={onOpenFolder}
                  onMoveAssets={onMoveAssets}
                  onMoveFolders={onMoveFolders}
                />
              ) : null}
              {folderItems.map((item) =>
                item.kind === "new" ? (
                  <NewFolderTile
                    key="new-folder"
                    value={creatingFolderName}
                    invalid={folderError !== null}
                    onChange={(name) => {
                      onChangeNewFolderName(name);
                      if (folderError !== null) {
                        onClearFolderError();
                      }
                    }}
                    onCommit={onCommitNewFolder}
                    onCancel={onCancelNewFolder}
                  />
                ) : (
                  <FolderTile
                    key={item.path}
                    path={item.path}
                    name={item.label}
                    editing={renamingFolder?.origin === "grid" && renamingFolder.path === item.path}
                    invalid={folderError !== null && renamingFolder?.path === item.path}
                    dragActive={assetDropTarget === item.path}
                    onOpen={onOpenFolder}
                    onSelect={onSelectFolder}
                    onBeginDrag={beginFolderDrag}
                    onEndDrag={onEndDrag}
                    onAssetDropTarget={onAssetDropTarget}
                    onMoveAssets={onMoveAssets}
                    onMoveFolders={onMoveFolders}
                    onDelete={onDeleteFolder}
                    onCommitRename={onCommitRenameFolder}
                    onChangeRename={onClearFolderError}
                    onCancelRename={onCancelRenameFolder}
                  />
                ),
              )}
              {assets.map((asset) => (
                <AssetTile
                  key={asset.id}
                  entry={asset}
                  renaming={renamingAsset === asset.id}
                  onView={onView}
                  onDelete={onDeleteAsset}
                  onSelect={onSelectAsset}
                  onBeginDrag={beginAssetDrag}
                  onEndDrag={onEndDrag}
                  onRenameEnd={onRenameEnd}
                />
              ))}
            </div>
          )}
        </div>
      </ScrollArea>
      {marqueeActive ? (
        // Seed the box at the press point with zero size: without an explicit rect it would lay
        // out at its unpositioned static offset (below the full-height ScrollArea), which perturbs
        // layout and flashes a spurious scrollbar on press-and-hold. applyMarquee takes over the
        // position on the first move.
        <div
          ref={boxRef}
          className="pointer-events-none absolute border border-ring bg-ring/15"
          style={{
            left: (marqueeRef.current?.startX ?? 0) - (marqueeRef.current?.panelLeft ?? 0),
            top: (marqueeRef.current?.startY ?? 0) - (marqueeRef.current?.panelTop ?? 0),
            width: 0,
            height: 0,
          }}
        />
      ) : null}
    </div>
  );
});
