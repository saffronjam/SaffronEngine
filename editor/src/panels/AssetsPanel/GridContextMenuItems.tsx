import { Eye, Folder, Info, Pen, Pencil, Plus, Trash } from "lucide-react";
import { ContextMenuItem, ContextMenuSeparator } from "@/components/ui/context-menu";
import { useEditorStore } from "../../state/store";
import type { AssetEntry } from "../../protocol";
import { assetCountLabel } from "./catalog";

/// The grid tile under the last contextmenu event, resolved from the tile DOM attributes before
/// Radix opens the shared menu; null is the empty area.
export type GridMenuTarget =
  | { kind: "asset"; id: string }
  | { kind: "folder"; path: string }
  | null;

const DESTRUCTIVE_ITEM_CLASS =
  "bg-destructive/10 text-destructive focus:bg-destructive focus:text-destructive-foreground";

/// The items of the grid's one shared context menu, mounted by Radix at open time (closed content
/// is unmounted), so each open reads the target ref and the live selection fresh. Batch actions win
/// over the tile under the pointer; an unresolvable target falls through to the empty-area actions.
export function GridContextMenuItems({
  targetRef,
  visibleAssets,
  renamingFolderGridPath,
  nativeDialogOpen,
  onViewAsset,
  onInstantiate,
  onRenameAsset,
  onShowDetails,
  onDeleteAsset,
  onDeleteAssets,
  onRenameFolder,
  onDeleteFolder,
  onNewFolder,
  onImport,
}: {
  targetRef: React.RefObject<GridMenuTarget>;
  visibleAssets: AssetEntry[];
  /// The folder mid-rename on its grid tile, which redirects the menu to the empty-area actions.
  renamingFolderGridPath: string | null;
  nativeDialogOpen: boolean;
  onViewAsset(asset: AssetEntry): void;
  onInstantiate(modelId: string): void;
  onRenameAsset(assetId: string): void;
  onShowDetails(assetId: string): void;
  onDeleteAsset(asset: AssetEntry): void;
  onDeleteAssets(assets: AssetEntry[]): void;
  onRenameFolder(folder: string): void;
  onDeleteFolder(folder: string): void;
  onNewFolder(): void;
  onImport(): void;
}) {
  const selectedAssetIds = useEditorStore((s) => s.selectedAssetIds);
  const target = targetRef.current;
  const batchAssets = visibleAssets.filter((asset) => selectedAssetIds.has(asset.id));
  if (batchAssets.length > 0) {
    return (
      <>
        <ContextMenuItem
          onSelect={() => {
            for (const asset of batchAssets) {
              onViewAsset(asset);
            }
          }}
        >
          <Eye />
          View ({assetCountLabel(batchAssets.length)})
        </ContextMenuItem>
        {batchAssets.some((asset) => asset.type === "model") ? (
          <ContextMenuItem
            onSelect={() => {
              for (const asset of batchAssets) {
                if (asset.type === "model") {
                  onInstantiate(asset.id);
                }
              }
            }}
          >
            <Plus />
            Add to scene
          </ContextMenuItem>
        ) : null}
        <ContextMenuSeparator />
        {batchAssets.length === 1 ? (
          <ContextMenuItem onSelect={() => onRenameAsset(batchAssets[0].id)}>
            <Pencil />
            Rename
          </ContextMenuItem>
        ) : null}
        <ContextMenuItem
          variant="destructive"
          className={DESTRUCTIVE_ITEM_CLASS}
          onSelect={() => onDeleteAssets(batchAssets)}
        >
          <Trash />
          Delete ({assetCountLabel(batchAssets.length)})
        </ContextMenuItem>
      </>
    );
  }
  const asset =
    target?.kind === "asset" ? visibleAssets.find((entry) => entry.id === target.id) : undefined;
  if (asset) {
    return (
      <>
        <ContextMenuItem onSelect={() => onViewAsset(asset)}>
          <Eye />
          View
        </ContextMenuItem>
        {asset.type === "model" ? (
          <ContextMenuItem onSelect={() => onInstantiate(asset.id)}>
            <Plus />
            Add to scene
          </ContextMenuItem>
        ) : null}
        <ContextMenuItem onSelect={() => onShowDetails(asset.id)}>
          <Info />
          Details
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem onSelect={() => onRenameAsset(asset.id)}>
          <Pencil />
          Rename
        </ContextMenuItem>
        <ContextMenuItem
          variant="destructive"
          className={DESTRUCTIVE_ITEM_CLASS}
          onSelect={() => onDeleteAsset(asset)}
        >
          <Trash />
          Delete
        </ContextMenuItem>
      </>
    );
  }
  if (target?.kind === "folder" && target.path !== renamingFolderGridPath) {
    const folder = target.path;
    return (
      <>
        <ContextMenuItem onSelect={() => onRenameFolder(folder)}>
          <Pen />
          Rename
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem
          variant="destructive"
          className={DESTRUCTIVE_ITEM_CLASS}
          onSelect={() => onDeleteFolder(folder)}
        >
          <Trash />
          Delete
        </ContextMenuItem>
      </>
    );
  }
  return (
    <>
      <ContextMenuItem onSelect={onNewFolder}>
        <Folder />
        New Folder
      </ContextMenuItem>
      <ContextMenuItem onSelect={onImport} disabled={nativeDialogOpen}>
        <Plus />
        Import
      </ContextMenuItem>
    </>
  );
}
