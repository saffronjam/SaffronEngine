import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  DragEvent as ReactDragEvent,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
} from "react";
import { getCurrentWebview, open } from "../../shell";
import { ArrowLeft, ArrowRight, Folder, FolderPlus, Plus, X } from "lucide-react";
import { client } from "../../control/client";
import { invalidateThumbnails, useEditorStore, withNativeDialog } from "../../state/store";
import type { AssetGridItem, AssetSortMode } from "../../state/store";
import {
  ASSET_DND_MIME,
  FOLDER_DND_MIME,
  catalogDragEffectAllowed,
  hideNativeDragImage,
} from "../../components/AssetTile";
import { AssetFolderTree, folderLabel } from "../AssetFolderTree";
import { logRender } from "../../lib/renderLog";
import { AssetDetailsDialog } from "../../components/AssetDetailsDialog";
import { errorText, notify, notifyError } from "../../lib/flash";
import type { AssetEntry, AssetUsageDto } from "../../protocol";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from "@/components/ui/resizable";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { AnimaSearchbar } from "../../components/anima/AnimaSearchbar";
import { emptySearchState } from "../../components/anima/chipSearch";
import type { SearchState } from "../../components/anima/chipSearch";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { ASSET_SEARCH_CHIPS, ASSET_SORT_OPTIONS } from "./assetSearch";
import {
  IMAGE_EXTS,
  MODEL_EXTS,
  NATIVE_VEGETATION_EXTS,
  assetCountLabel,
  deleteFolderBody,
  importPath,
  isFolderDescendant,
  isInsidePanel,
  nearestHistoryIndex,
  sortAssets,
  sortedFolderItems,
} from "./catalog";
import type { FolderHistory } from "./catalog";
import { AssetPanelBody } from "./AssetPanelBody";
import { Breadcrumbs } from "./Breadcrumbs";
import { GridContextMenuItems } from "./GridContextMenuItems";
import { useFolderEditing } from "./useFolderEditing";
import type { GridMenuTarget } from "./GridContextMenuItems";

interface PendingAssetDelete {
  assets: AssetEntry[];
  usages: AssetUsageDto[];
}

/// Usage lines shown in the delete dialog before collapsing into "And X additional".
const MAX_USAGE_LINES = 5;

/// The Assets panel: a folder tree sidebar plus a responsive tile grid over `store.assets`,
/// navigated through a back/forward history and clickable breadcrumbs, with an Import button, an
/// OS file-drop target, and the delete/details dialogs.
export function AssetsPanel() {
  logRender("AssetsPanel");
  const assets = useEditorStore((s) => s.assets);
  const folders = useEditorStore((s) => s.assetFolders);
  const refreshAssets = useEditorStore((s) => s.refreshAssets);
  const instantiateModel = useEditorStore((s) => s.instantiateModel);
  const nativeDialogOpen = useEditorStore((s) => s.nativeDialogOpen);
  const openImageViewerTab = useEditorStore((s) => s.openImageViewerTab);
  const openAssetEditorForAsset = useEditorStore((s) => s.openAssetEditorForAsset);
  const closeViewTab = useEditorStore((s) => s.closeViewTab);
  const setAssetsPanelHovered = useEditorStore((s) => s.setAssetsPanelHovered);
  const setAssetsFolderNav = useEditorStore((s) => s.setAssetsFolderNav);
  const assetSort = useEditorStore((s) => s.assetSort);
  const setAssetSort = useEditorStore((s) => s.setAssetSort);
  const [history, setHistory] = useState<FolderHistory>({ stack: [null], index: 0 });
  // The Ctrl+F find bar: an overlay revealed by the shortcut, filtering the current folder's tiles.
  // `searchMounted` keeps it alive through the exit animation.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchMounted, setSearchMounted] = useState(false);
  const [search, setSearch] = useState<SearchState>(emptySearchState());
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const panelRootRef = useRef<HTMLDivElement | null>(null);
  const menuTargetRef = useRef<GridMenuTarget>(null);
  const setAssetMenuTarget = useEditorStore((s) => s.setAssetMenuTarget);
  const [pendingAssetDelete, setPendingAssetDelete] = useState<PendingAssetDelete | null>(null);
  const [pendingFolderDelete, setPendingFolderDelete] = useState<string | null>(null);
  const [detailsAssetId, setDetailsAssetId] = useState<string | null>(null);
  // Grid selection lives in the store so tiles subscribe to their own membership; this panel never
  // reads it at render time (handlers use getState()), so a selection delta re-renders tiles only.
  const selectAssetGridItem = useEditorStore((s) => s.selectAssetGridItem);
  const pruneAssetSelection = useEditorStore((s) => s.pruneAssetSelection);
  const removeFromAssetSelection = useEditorStore((s) => s.removeFromAssetSelection);
  const rewriteSelectedFolderPaths = useEditorStore((s) => s.rewriteSelectedFolderPaths);
  const [dropActive, setDropActive] = useState(false);
  const [assetDropTarget, setAssetDropTarget] = useState<string | null>(null);
  const currentFolder = history.stack[history.index] ?? null;
  const searchText = search.freeText.trim().toLowerCase();
  const typeFilter = search.chips.find((chip) => chip.keyword === "type")?.value ?? null;
  // Embedded sub-assets (a `.smodel`'s mesh/material/texture rows, or a material import's maps) are
  // hidden from the top level so a container imports as ONE tile. A material import is a
  // *self-container* (`container === id`), so its parent row stays visible while its embedded maps
  // (pointing at a different container id) stay hidden. `folderAssets` drives selection pruning;
  // `visibleAssets` applies the sort and find-bar filter for what renders.
  const folderAssets = useMemo(
    () =>
      assets.filter(
        (asset) =>
          (asset.folder ?? "") === (currentFolder ?? "") &&
          (!asset.container || asset.container === asset.id),
      ),
    [assets, currentFolder],
  );
  const visibleAssets = useMemo(() => {
    let list = folderAssets;
    if (typeFilter) {
      list = list.filter((asset) => asset.type === typeFilter);
    }
    if (searchText) {
      list = list.filter((asset) => asset.name.toLowerCase().includes(searchText));
    }
    return sortAssets(list, assetSort);
  }, [folderAssets, typeFilter, searchText, assetSort]);

  // The grid's selection order: folder tiles then asset tiles, matching the body's render order, so
  // a shift-range can span folders and assets.
  const gridOrder = useMemo<AssetGridItem[]>(() => {
    const folderKeys = sortedFolderItems(folders, currentFolder, false, searchText).flatMap(
      (item) => (item.kind === "folder" ? [{ kind: "folder" as const, key: item.path }] : []),
    );
    return [
      ...folderKeys,
      ...visibleAssets.map((asset) => ({ kind: "asset" as const, key: asset.id })),
    ];
  }, [folders, currentFolder, visibleAssets, searchText]);

  const navigateTo = useCallback((folder: string | null): void => {
    setHistory((current) => {
      if (current.stack[current.index] === folder) {
        return current;
      }
      const stack = [...current.stack.slice(0, current.index + 1), folder];
      return { stack, index: stack.length - 1 };
    });
  }, []);

  const {
    creatingFolder,
    creatingFolderName,
    setCreatingFolderName,
    renamingFolder,
    renamingAsset,
    setRenamingAsset,
    folderError,
    startCreateFolder,
    startRenameFolder,
    endRenameAsset,
    commitNewFolderName,
    cancelNewFolder,
    commitRenameFolderName,
    cancelRenameFolder,
    clearFolderError,
    moveAssetsToFolder,
    moveFoldersTo,
  } = useFolderEditing({
    folders,
    refreshAssets,
    navigateTo,
    setHistory,
    rewriteSelectedFolderPaths,
  });

  const onNewFolder = useCallback((): void => {
    startCreateFolder(currentFolder, "grid");
  }, [currentFolder, startCreateFolder]);

  const goBack = useCallback((): void => {
    setHistory((current) => {
      const index = nearestHistoryIndex(current, folders, -1);
      return index < 0 ? current : { ...current, index };
    });
  }, [folders]);

  const goForward = useCallback((): void => {
    setHistory((current) => {
      const index = nearestHistoryIndex(current, folders, 1);
      return index < 0 ? current : { ...current, index };
    });
  }, [folders]);

  const canGoBack = nearestHistoryIndex(history, folders, -1) >= 0;
  const canGoForward = nearestHistoryIndex(history, folders, 1) >= 0;

  // Register this panel's folder back/forward so the central mouse dispatcher can drive it while
  // the pointer is over the panel.
  useEffect(() => {
    setAssetsFolderNav({ back: goBack, forward: goForward });
    return () => setAssetsFolderNav(null);
  }, [goBack, goForward, setAssetsFolderNav]);

  // Clear the hover flag on unmount so a stale `true` never misroutes the side buttons away from
  // tab navigation.
  useEffect(() => () => setAssetsPanelHovered(false), [setAssetsPanelHovered]);

  useEffect(() => {
    if (currentFolder && !folders.includes(currentFolder)) {
      navigateTo(null);
    }
  }, [currentFolder, folders, navigateTo]);

  useEffect(() => {
    if (searchOpen) {
      setSearchMounted(true);
      requestAnimationFrame(() => searchInputRef.current?.focus());
    }
  }, [searchOpen]);

  // Return focus to the panel root so Ctrl+F still routes here after the bar closes; the focused
  // search input unmounts, which would otherwise drop focus to the body.
  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setSearch(emptySearchState());
    panelRootRef.current?.focus({ preventScroll: true });
  }, []);

  const onPanelKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>): void => {
      if ((event.ctrlKey || event.metaKey) && (event.key === "f" || event.key === "F")) {
        event.preventDefault();
        if (searchOpen) {
          closeSearch();
        } else {
          setSearchOpen(true);
          requestAnimationFrame(() => searchInputRef.current?.focus());
        }
        return;
      }
      if (event.key === "Escape" && searchOpen) {
        event.preventDefault();
        closeSearch();
      }
    },
    [searchOpen, closeSearch],
  );

  // Prune the selection (and a rename-in-progress) when assets leave the visible grid or folders
  // cease to exist; the store action bails out identity-stable, so the StrictMode double-run is a
  // no-op.
  useEffect(() => {
    pruneAssetSelection(folderAssets, folders);
    setRenamingAsset((current) =>
      current !== null && !folderAssets.some((asset) => asset.id === current) ? null : current,
    );
  }, [folderAssets, folders, pruneAssetSelection, setRenamingAsset]);

  const importMany = useCallback(
    async (paths: string[]): Promise<void> => {
      for (const path of paths) {
        await importPath(path, currentFolder);
      }
      await refreshAssets();
    },
    [currentFolder, refreshAssets],
  );

  // OS file-drop arrives on the webview drag-drop channel, distinct from the HTML5 tile DnD. Only
  // import when the drop lands inside this panel's rect, so a model dropped on the viewport does
  // not trigger a catalog import here.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "enter" || payload.type === "over") {
          setDropActive(isInsidePanel(payload.position));
        } else if (payload.type === "leave") {
          setDropActive(false);
        } else if (payload.type === "drop") {
          setDropActive(false);
          if (payload.paths.length > 0 && isInsidePanel(payload.position)) {
            void importMany(payload.paths);
          }
        }
      })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [importMany]);

  const onImportClick = useCallback(async (): Promise<void> => {
    const selection = await withNativeDialog(() =>
      open({
        multiple: true,
        filters: [
          {
            name: "Project assets",
            extensions: [...MODEL_EXTS, ...IMAGE_EXTS, ...NATIVE_VEGETATION_EXTS],
          },
          { name: "Models", extensions: MODEL_EXTS },
          { name: "Images", extensions: IMAGE_EXTS },
          { name: "Vegetation", extensions: NATIVE_VEGETATION_EXTS },
        ],
      }),
    );
    if (!selection) {
      return;
    }
    await importMany(Array.isArray(selection) ? selection : [selection]);
  }, [importMany]);

  // Stable void-returning shapes of the async handlers above, so the memo'd body and tiles see one
  // identity across renders.
  const moveAssets = useCallback(
    (assetIds: string[], folder: string | null): void => void moveAssetsToFolder(assetIds, folder),
    [moveAssetsToFolder],
  );

  const moveFolders = useCallback(
    (paths: string[], parent: string | null): void => void moveFoldersTo(paths, parent),
    [moveFoldersTo],
  );

  // The one selection entry point for grid tiles, folders and assets alike; the store action holds
  // the plain/toggle/shift-range semantics and the anchor.
  const selectGridItem = useCallback(
    (kind: "asset" | "folder", key: string, event: ReactMouseEvent): void =>
      selectAssetGridItem(
        kind,
        key,
        { shift: event.shiftKey, toggle: event.ctrlKey || event.metaKey },
        gridOrder,
      ),
    [selectAssetGridItem, gridOrder],
  );

  const selectAsset = useCallback(
    (asset: AssetEntry, event: ReactMouseEvent): void => selectGridItem("asset", asset.id, event),
    [selectGridItem],
  );

  const selectFolder = useCallback(
    (folder: string, event: ReactMouseEvent): void => selectGridItem("folder", folder, event),
    [selectGridItem],
  );

  // Assemble the drag payload from the live selection, read at event time so the callback identity
  // never tracks it. A dragged tile outside the selection carries just itself without changing the
  // selection; click owns selection.
  const beginDrag = useCallback(
    (kind: "asset" | "folder", key: string, event: ReactDragEvent): void => {
      const state = useEditorStore.getState();
      const sel = kind === "asset" ? state.selectedAssetIds : state.selectedFolderPaths;
      let assetIds = [...state.selectedAssetIds];
      let folderPaths = [...state.selectedFolderPaths];
      if (!sel.has(key)) {
        assetIds = kind === "asset" ? [key] : [];
        folderPaths = kind === "folder" ? [key] : [];
      }
      const visibleIds = new Set(
        state.assets
          .filter((asset) => (asset.folder ?? "") === (currentFolder ?? ""))
          .map((asset) => asset.id),
      );
      const folderSet = new Set(state.assetFolders);
      assetIds = assetIds.filter((id) => visibleIds.has(id));
      folderPaths = folderPaths.filter((path) => folderSet.has(path));
      if (assetIds.length > 0) {
        event.dataTransfer.setData(ASSET_DND_MIME, JSON.stringify({ ids: assetIds }));
      }
      if (folderPaths.length > 0) {
        event.dataTransfer.setData(FOLDER_DND_MIME, JSON.stringify({ paths: folderPaths }));
      }
      state.setCatalogDrag({ assetIds, folderPaths });
      event.dataTransfer.effectAllowed = catalogDragEffectAllowed(assetIds);
      // `CatalogDragGhost` in App.tsx is the one drag preview for every asset type, so always
      // suppress the browser's native ghost (which CEF OSR would not render anyway).
      if (assetIds.length > 0) {
        hideNativeDragImage(event.dataTransfer);
      }
    },
    [currentFolder],
  );

  const endDrag = useCallback((): void => {
    useEditorStore.getState().setCatalogDrag(null);
  }, []);

  const requestDeleteAsset = useCallback(async (asset: AssetEntry): Promise<void> => {
    setPendingFolderDelete(null);
    let usages: AssetUsageDto[];
    try {
      usages = (await client.assetUsages(asset.id)).usages;
    } catch (err) {
      notify(`Could not look up usages: ${errorText(err)}`);
      return;
    }
    setPendingAssetDelete({ assets: [asset], usages });
  }, []);

  const deleteAsset = useCallback(
    (asset: AssetEntry): void => void requestDeleteAsset(asset),
    [requestDeleteAsset],
  );

  const onInstantiate = useCallback(
    (modelId: string): void => {
      void instantiateModel(modelId)
        .then(() => notify("Added to scene"))
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [instantiateModel],
  );

  const requestDeleteAssets = useCallback(async (targetAssets: AssetEntry[]): Promise<void> => {
    if (targetAssets.length === 0) {
      return;
    }
    setPendingFolderDelete(null);
    const usages: AssetUsageDto[] = [];
    let failed = false;
    await targetAssets.reduce(
      (prev, asset) =>
        prev.then(async () => {
          if (failed) {
            return;
          }
          try {
            usages.push(...(await client.assetUsages(asset.id)).usages);
          } catch (err) {
            failed = true;
            notify(`Could not look up usages for ${asset.name}: ${errorText(err)}`);
          }
        }),
      Promise.resolve(),
    );
    if (failed) {
      return;
    }
    setPendingAssetDelete({ assets: targetAssets, usages });
  }, []);

  // Every previewable asset opens the 3D asset editor: an HDRI as a lit environment, any other
  // texture role as its map on the studio sphere, a material as itself (the graph editor stays the
  // place to *edit* it). Only non-previewable files fall back to the flat image view.
  const routeView = useCallback(
    (asset: AssetEntry) => {
      const ridesAssetEditor =
        asset.type === "model" ||
        asset.type === "mesh" ||
        asset.type === "animation" ||
        asset.type === "texture" ||
        asset.type === "material" ||
        asset.type === "plant" ||
        asset.type === "biome" ||
        asset.type === "vegetation-map";
      if (ridesAssetEditor) {
        openAssetEditorForAsset(asset.id, asset.name);
      } else {
        openImageViewerTab(asset);
      }
    },
    [openImageViewerTab, openAssetEditorForAsset],
  );

  const confirmDeleteAssets = useCallback(
    async (targetAssets: AssetEntry[]): Promise<void> => {
      setPendingAssetDelete(null);
      const deletedIds = new Set<string>();
      await targetAssets.reduce(
        (prev, asset) =>
          prev.then(async () => {
            try {
              await client.deleteAsset(asset.id);
              deletedIds.add(asset.id);
              closeViewTab(`imageViewer:${asset.id}`);
              // The asset-editor tab is keyed by the owning model container, so close both the
              // asset's own key and its container's.
              closeViewTab(`assetEditor:${asset.id}`);
              if (asset.container && asset.container !== "0") {
                closeViewTab(`assetEditor:${asset.container}`);
              }
            } catch (err) {
              notify(`Could not delete ${asset.name}: ${errorText(err)}`);
            }
          }),
        Promise.resolve(),
      );
      if (deletedIds.size > 0) {
        invalidateThumbnails();
        removeFromAssetSelection(deletedIds);
        await refreshAssets();
      }
    },
    [closeViewTab, refreshAssets, removeFromAssetSelection],
  );

  const requestDeleteFolder = useCallback((folder: string): void => {
    setPendingAssetDelete(null);
    setPendingFolderDelete(folder);
  }, []);

  const confirmDeleteFolder = useCallback(
    async (folder: string): Promise<void> => {
      setPendingFolderDelete(null);
      try {
        await client.deleteAssetFolder(folder);
      } catch (err) {
        notify(`Could not delete ${folderLabel(folder)}: ${errorText(err)}`);
        return;
      }
      if (
        currentFolder === folder ||
        (currentFolder && isFolderDescendant(currentFolder, folder))
      ) {
        navigateTo(null);
      }
      await refreshAssets();
    },
    [currentFolder, navigateTo, refreshAssets],
  );

  const cancelDelete = useCallback((): void => {
    setPendingAssetDelete(null);
    setPendingFolderDelete(null);
  }, []);

  // The single delete-confirmation modal, fed by whichever request is pending.
  const pendingDelete = pendingAssetDelete
    ? (() => {
        const count = pendingAssetDelete.assets.length;
        return {
          title:
            count === 1
              ? `Delete ${pendingAssetDelete.assets[0].name}?`
              : `Delete ${assetCountLabel(count)}?`,
          body:
            pendingAssetDelete.usages.length > 0
              ? `Clears ${pendingAssetDelete.usages.length} usage${pendingAssetDelete.usages.length === 1 ? "" : "s"}:`
              : count === 1
                ? "Removes the catalog entry and imported file."
                : `Removes ${assetCountLabel(count)} from the catalog and deletes their imported files.`,
          usages: pendingAssetDelete.usages,
          confirm: () => void confirmDeleteAssets(pendingAssetDelete.assets),
        };
      })()
    : pendingFolderDelete !== null
      ? {
          title: `Delete ${folderLabel(pendingFolderDelete)}?`,
          body: deleteFolderBody(pendingFolderDelete),
          usages: [],
          confirm: () => void confirmDeleteFolder(pendingFolderDelete),
        }
      : null;
  const shownUsages = pendingDelete?.usages.slice(0, MAX_USAGE_LINES) ?? [];
  const extraUsages = (pendingDelete?.usages.length ?? 0) - shownUsages.length;

  return (
    <div
      ref={panelRootRef}
      className="flex h-full min-h-0 flex-col outline-none"
      data-asset-panel="true"
      tabIndex={0}
      onKeyDown={onPanelKeyDown}
      onPointerEnter={() => setAssetsPanelHovered(true)}
      onPointerLeave={() => setAssetsPanelHovered(false)}
    >
      <div className="flex h-10 flex-none items-center gap-1 border-b border-border px-3">
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          className="flex-none"
          disabled={!canGoBack}
          onClick={goBack}
          aria-label="Back"
        >
          <ArrowLeft />
        </Button>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          className="flex-none"
          disabled={!canGoForward}
          onClick={goForward}
          aria-label="Forward"
        >
          <ArrowRight />
        </Button>
        <Breadcrumbs
          currentFolder={currentFolder}
          onNavigate={navigateTo}
          onMoveAssets={moveAssets}
          onMoveFolders={moveFolders}
        />
        <div className="ml-auto flex flex-none items-center gap-2">
          <Button type="button" size="sm" variant="ghost" className="gap-1" onClick={onNewFolder}>
            <FolderPlus />
            New Folder
          </Button>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                type="button"
                size="sm"
                variant="ghost"
                className="gap-1"
                onClick={() => void onImportClick()}
                disabled={nativeDialogOpen}
              >
                <Plus />
                Import
              </Button>
            </TooltipTrigger>
            <TooltipContent>Import a model or texture</TooltipContent>
          </Tooltip>
          <Select value={assetSort} onValueChange={(value) => setAssetSort(value as AssetSortMode)}>
            <SelectTrigger size="sm" className="w-[8.5rem]" aria-label="Sort assets">
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end">
              {ASSET_SORT_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      <ResizablePanelGroup orientation="horizontal" className="min-h-0 flex-1">
        <ResizablePanel
          defaultSize={220}
          minSize={140}
          maxSize={480}
          groupResizeBehavior="preserve-pixel-size"
          className="min-w-0"
        >
          <ContextMenu modal={false}>
            <ContextMenuTrigger asChild>
              <div className="h-full min-h-0">
                <AssetFolderTree
                  folders={folders}
                  currentFolder={currentFolder}
                  renamingFolder={renamingFolder?.origin === "tree" ? renamingFolder.path : null}
                  renameInvalid={folderError !== null}
                  creatingIn={
                    creatingFolder?.origin === "tree" ? { parent: creatingFolder.parent } : null
                  }
                  createInvalid={folderError !== null}
                  onNavigate={navigateTo}
                  onMoveAssets={moveAssets}
                  onMoveFolders={moveFolders}
                  onNewFolder={(parent) => startCreateFolder(parent, "tree")}
                  onStartRename={(folder) => startRenameFolder(folder, "tree")}
                  onChangeRename={clearFolderError}
                  onCommitRename={commitRenameFolderName}
                  onCancelRename={cancelRenameFolder}
                  onChangeCreate={clearFolderError}
                  onCommitCreate={commitNewFolderName}
                  onCancelCreate={cancelNewFolder}
                  onDelete={requestDeleteFolder}
                />
              </div>
            </ContextMenuTrigger>
            {/* No focus restore on close: it lands after the inline name input takes focus (the
                menu unmounts post-exit-animation) and would blur-cancel it. */}
            <ContextMenuContent
              className="min-w-40"
              onCloseAutoFocus={(event) => event.preventDefault()}
            >
              <ContextMenuItem onSelect={() => startCreateFolder(null, "tree")}>
                <Folder />
                New Folder
              </ContextMenuItem>
              <ContextMenuItem onSelect={() => void onImportClick()} disabled={nativeDialogOpen}>
                <Plus />
                Import
              </ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel className="relative min-w-0 overflow-hidden">
          <ContextMenu
            modal={false}
            // Mirror the resolved target into the store while the menu is open so the right-clicked
            // tile keeps a highlight; `onContextMenu` below has already populated the ref by the
            // time this fires with open=true.
            onOpenChange={(menuOpen) => {
              const target = menuOpen ? menuTargetRef.current : null;
              setAssetMenuTarget(
                target
                  ? { kind: target.kind, key: target.kind === "asset" ? target.id : target.path }
                  : null,
              );
            }}
          >
            <ContextMenuTrigger asChild>
              <div
                className="h-full min-h-0"
                // Resolve the tile under a right-click into the ref before Radix opens the one
                // shared menu; the menu items read it at open time.
                onContextMenu={(event) => {
                  const target = event.target instanceof Element ? event.target : null;
                  const assetId =
                    target?.closest<HTMLElement>("[data-asset-tile-id]")?.dataset.assetTileId;
                  const folderPath = target?.closest<HTMLElement>("[data-asset-folder-path]")
                    ?.dataset.assetFolderPath;
                  menuTargetRef.current = assetId
                    ? { kind: "asset", id: assetId }
                    : folderPath
                      ? { kind: "folder", path: folderPath }
                      : null;
                  // Right-clicking a tile that is not already selected replaces the selection with
                  // just it, so the menu's action targets the clicked tile. Right-clicking within
                  // an existing multi-selection leaves it intact so a batch action still applies.
                  const state = useEditorStore.getState();
                  if (assetId && !state.selectedAssetIds.has(assetId)) {
                    selectAssetGridItem(
                      "asset",
                      assetId,
                      { shift: false, toggle: false },
                      gridOrder,
                    );
                  } else if (folderPath && !state.selectedFolderPaths.has(folderPath)) {
                    selectAssetGridItem(
                      "folder",
                      folderPath,
                      { shift: false, toggle: false },
                      gridOrder,
                    );
                  }
                }}
              >
                <AssetPanelBody
                  assets={visibleAssets}
                  folders={folders}
                  currentFolder={currentFolder}
                  searchText={searchText}
                  dropActive={dropActive}
                  creatingFolder={creatingFolder?.origin === "grid"}
                  creatingFolderName={creatingFolderName}
                  renamingFolder={renamingFolder}
                  renamingAsset={renamingAsset}
                  folderError={folderError}
                  assetDropTarget={assetDropTarget}
                  onOpenFolder={navigateTo}
                  onView={routeView}
                  onSelectAsset={selectAsset}
                  onSelectFolder={selectFolder}
                  onBeginDrag={beginDrag}
                  onEndDrag={endDrag}
                  onDeleteAsset={deleteAsset}
                  onDeleteFolder={requestDeleteFolder}
                  onMoveAssets={moveAssets}
                  onMoveFolders={moveFolders}
                  onCommitNewFolder={commitNewFolderName}
                  onChangeNewFolderName={setCreatingFolderName}
                  onCancelNewFolder={cancelNewFolder}
                  onCommitRenameFolder={commitRenameFolderName}
                  onCancelRenameFolder={cancelRenameFolder}
                  onAssetDropTarget={setAssetDropTarget}
                  onClearFolderError={clearFolderError}
                  onRenameEnd={endRenameAsset}
                />
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent
              className="min-w-40"
              onCloseAutoFocus={(event) => event.preventDefault()}
            >
              <GridContextMenuItems
                targetRef={menuTargetRef}
                visibleAssets={visibleAssets}
                renamingFolderGridPath={
                  renamingFolder?.origin === "grid" ? renamingFolder.path : null
                }
                nativeDialogOpen={nativeDialogOpen}
                onViewAsset={routeView}
                onInstantiate={onInstantiate}
                onRenameAsset={setRenamingAsset}
                onShowDetails={setDetailsAssetId}
                onDeleteAsset={deleteAsset}
                onDeleteAssets={(targets) => void requestDeleteAssets(targets)}
                onRenameFolder={(folder) => startRenameFolder(folder, "grid")}
                onDeleteFolder={requestDeleteFolder}
                onNewFolder={onNewFolder}
                onImport={() => void onImportClick()}
              />
            </ContextMenuContent>
          </ContextMenu>
          {/* Find bar: a top-right overlay revealed by Ctrl+F, dismissed by Esc or its X. The
              closed state stays mounted until its exit animation ends; the animationend guard
              ignores child animations that bubble up. */}
          {searchMounted ? (
            <div
              data-state={searchOpen ? "open" : "closed"}
              onAnimationEnd={(event) => {
                if (event.target === event.currentTarget && !searchOpen) {
                  setSearchMounted(false);
                }
              }}
              className="absolute top-1.5 right-1.5 z-20 flex w-72 max-w-[calc(100%-0.75rem)] items-center gap-1 rounded-md border border-border bg-card p-1 shadow-lg duration-150 ease-out data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:slide-out-to-top-1 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:slide-in-from-top-1 data-[state=open]:zoom-in-95"
            >
              <AnimaSearchbar
                value={search}
                onChange={setSearch}
                chips={ASSET_SEARCH_CHIPS}
                placeholder="Find assets"
                debounceMs={120}
                inputRef={searchInputRef}
                showClear={false}
                className="flex-1"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-6 w-6 shrink-0"
                onClick={closeSearch}
                aria-label="Close find"
              >
                <X className="size-4" />
              </Button>
            </div>
          ) : null}
        </ResizablePanel>
      </ResizablePanelGroup>
      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(dialogOpen) => {
          if (!dialogOpen) {
            cancelDelete();
          }
        }}
      >
        <DialogContent showCloseButton={false} className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{pendingDelete?.title}</DialogTitle>
            <DialogDescription>{pendingDelete?.body}</DialogDescription>
          </DialogHeader>
          {shownUsages.length > 0 ? (
            <div className="text-sm text-muted-foreground">
              <ul className="list-disc space-y-1 pl-5">
                {shownUsages.map((usage) => (
                  <li key={`${usage.entity ?? ""}-${usage.slot}`}>
                    <span className="flex items-center gap-1.5">
                      {usage.entityName ? (
                        <span className="truncate">{usage.entityName}</span>
                      ) : null}
                      <Badge variant="secondary">{usage.slot}</Badge>
                    </span>
                  </li>
                ))}
              </ul>
              {extraUsages > 0 ? (
                <p className="mt-2">
                  And {extraUsages} additional usage{extraUsages === 1 ? "" : "s"}
                </p>
              ) : null}
            </div>
          ) : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={cancelDelete}>
              Cancel
            </Button>
            <Button type="button" variant="destructive" onClick={() => pendingDelete?.confirm()}>
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <AssetDetailsDialog assetId={detailsAssetId} onClose={() => setDetailsAssetId(null)} />
    </div>
  );
}
