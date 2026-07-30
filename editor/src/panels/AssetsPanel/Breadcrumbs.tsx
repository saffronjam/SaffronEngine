import { Fragment, useState } from "react";
import {
  assetIdsFromPayload,
  isCatalogDrag,
  readAssetPayload,
  readFolderPayload,
} from "../../components/AssetTile";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/// The clickable path: Root plus one segment per folder level, each navigating to its prefix and
/// accepting a catalog drop (the Root crumb is the move-to-root affordance).
export function Breadcrumbs({
  currentFolder,
  onNavigate,
  onMoveAssets,
  onMoveFolders,
}: {
  currentFolder: string | null;
  onNavigate(folder: string | null): void;
  onMoveAssets(assetIds: string[], folder: string | null): void;
  onMoveFolders(paths: string[], parent: string | null): void;
}) {
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const segments = currentFolder ? currentFolder.split("/") : [];
  const crumbs: { key: string; label: string; target: string | null }[] = [
    { key: "", label: "Root", target: null },
    ...segments.map((segment, i) => {
      const prefix = segments.slice(0, i + 1).join("/");
      return { key: prefix, label: segment, target: prefix };
    }),
  ];
  return (
    <div className="ml-1 flex min-w-0 items-center gap-0.5 overflow-hidden text-xs text-muted-foreground">
      {crumbs.map((crumb, i) => (
        <Fragment key={crumb.key}>
          {i > 0 ? <span className="flex-none">/</span> : null}
          <Button
            type="button"
            size="xs"
            variant="ghost"
            className={cn(
              "max-w-40 truncate px-1 font-mono",
              i === crumbs.length - 1 && i > 0 && "text-foreground",
              dropTarget === crumb.key && "bg-accent/60 ring-1 ring-ring",
            )}
            onClick={() => onNavigate(crumb.target)}
            onDragEnter={(event) => {
              if (isCatalogDrag(event.dataTransfer)) {
                setDropTarget(crumb.key);
              }
            }}
            onDragOver={(event) => {
              if (isCatalogDrag(event.dataTransfer)) {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
                setDropTarget(crumb.key);
              }
            }}
            onDragLeave={() => {
              setDropTarget((current) => (current === crumb.key ? null : current));
            }}
            onDrop={(event) => {
              const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
              const folderPaths = readFolderPayload(event.dataTransfer);
              if (ids.length === 0 && folderPaths.length === 0) {
                return;
              }
              event.preventDefault();
              setDropTarget(null);
              if (folderPaths.length > 0) {
                onMoveFolders(folderPaths, crumb.target);
              }
              if (ids.length > 0) {
                onMoveAssets(ids, crumb.target);
              }
            }}
          >
            {crumb.label}
          </Button>
        </Fragment>
      ))}
    </div>
  );
}
