/// One asset catalog tile: a lazy thumbnail (mesh = its 3D render, texture = its
/// image, else a lucide type icon), an in-place rename input, and an HTML5 drag
/// SOURCE carrying `application/x-sa-asset`. Double-click opens the View modal; the
/// Delete key on a focused tile starts the same delete flow as the grid's shared
/// context menu (the panel owns that menu — a tile renders no Radix root of its own).
import { memo, useEffect, useRef, useState } from "react";
import {
  Box,
  Clapperboard,
  File,
  Image as ImageIcon,
  Loader2,
  Map as MapIcon,
  Sprout,
  TreePine,
} from "lucide-react";
import { client } from "../control/client";
import { getCachedThumbnailUrl, getThumbnailUrl, useEditorStore } from "../state/store";
import { matchesBinding } from "../lib/keybindings";
import type { AssetEntry } from "../protocol";
import { cn } from "@/lib/utils";
import { useOutsideCommit } from "../lib/useOutsideCommit";
import { errorText, notifyError } from "../lib/flash";
import { logRender } from "../lib/renderLog";
import { Input } from "@/components/ui/input";

/// The DnD payload written to `application/x-sa-asset` (distinct from an OS file
/// drop). `type` lets a drop target type-gate the accept.
export const ASSET_DND_MIME = "application/x-sa-asset";

export interface AssetDragPayload {
  id?: string;
  ids?: string[];
  type?: AssetEntry["type"];
}

export function catalogDragEffectAllowed(
  assetIds: readonly string[],
): DataTransfer["effectAllowed"] {
  return assetIds.length > 0 ? "copyMove" : "move";
}

/// Try to read an asset drag payload off a DataTransfer; null if it is not one.
export function readAssetPayload(dt: DataTransfer): AssetDragPayload | null {
  const raw = dt.getData(ASSET_DND_MIME);
  if (!raw) {
    return null;
  }
  try {
    const parsed = JSON.parse(raw) as Partial<AssetDragPayload>;
    if (Array.isArray(parsed.ids) && parsed.ids.every((id) => typeof id === "string")) {
      return { ids: parsed.ids };
    }
    if (typeof parsed.id === "string") {
      return { id: parsed.id, type: parsed.type };
    }
  } catch {
    // Malformed payload; treat as not-an-asset.
  }
  return null;
}

/// The asset ids carried by a drag payload, preferring the multi-select list.
export function assetIdsFromPayload(payload: AssetDragPayload | null): string[] {
  if (!payload) {
    return [];
  }
  if (payload.ids && payload.ids.length > 0) {
    return payload.ids;
  }
  return payload.id ? [payload.id] : [];
}

export function firstModelAssetId(
  ids: readonly string[],
  assets: readonly AssetEntry[],
): string | null {
  return ids.find((id) => assets.find((asset) => asset.id === id)?.type === "model") ?? null;
}

let transparentDragImage: HTMLImageElement | null = null;

export function hideNativeDragImage(dt: DataTransfer): void {
  if (!transparentDragImage) {
    transparentDragImage = document.createElement("img");
    transparentDragImage.src =
      "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";
    transparentDragImage.style.position = "fixed";
    transparentDragImage.style.left = "-1000px";
    transparentDragImage.style.top = "-1000px";
    transparentDragImage.style.width = "1px";
    transparentDragImage.style.height = "1px";
  }
  if (!transparentDragImage.isConnected) {
    document.body.appendChild(transparentDragImage);
  }
  dt.setDragImage(transparentDragImage, 0, 0);
}

export function AssetDragPreviewTile({ entry }: { entry: AssetEntry }) {
  const url = getCachedThumbnailUrl(entry.id, THUMBNAIL_FETCH_SIZE);

  return (
    <div className="flex w-[86px] flex-col gap-1 rounded-md border border-ring bg-card/95 p-1 shadow-lg ring-1 ring-ring">
      <div className="relative flex aspect-square w-full items-center justify-center overflow-hidden rounded-sm bg-muted">
        {url ? (
          <img src={url} alt={entry.name} className="size-full object-contain" draggable={false} />
        ) : (
          <TypeIcon type={entry.type} />
        )}
        {entry.type === "animation" && entry.duration ? (
          <span className="absolute bottom-1 right-1 rounded bg-background/80 px-1 py-0.5 text-[9px] font-medium tabular-nums text-muted-foreground">
            {formatDurationBadge(entry.duration)}
          </span>
        ) : null}
      </div>
      <div className="line-clamp-2 min-h-[2.5em] break-words rounded-sm px-0.5 text-center text-[13px] leading-tight text-foreground">
        {entry.name}
      </div>
    </div>
  );
}

/// The DnD payload for dragging catalog FOLDERS (their full paths) — distinct from
/// the asset payload so drop targets can route a folder move vs an asset move. A
/// drag may carry both mimes at once (a mixed folder + asset multi-selection).
export const FOLDER_DND_MIME = "application/x-sa-folder";

/// The folder paths carried by a drag payload (empty if it is not a folder drag).
export function readFolderPayload(dt: DataTransfer): string[] {
  const raw = dt.getData(FOLDER_DND_MIME);
  if (!raw) {
    return [];
  }
  try {
    const parsed = JSON.parse(raw) as { path?: string; paths?: string[] };
    if (Array.isArray(parsed.paths)) {
      return parsed.paths.filter((p) => typeof p === "string" && p.length > 0);
    }
    if (typeof parsed.path === "string" && parsed.path.length > 0) {
      return [parsed.path];
    }
  } catch {
    // Malformed payload; treat as not-a-folder.
  }
  return [];
}

/// True when the drag in flight carries an asset or folder payload.
export function isCatalogDrag(dt: DataTransfer): boolean {
  return dt.types.includes(ASSET_DND_MIME) || dt.types.includes(FOLDER_DND_MIME);
}

export function supportsRenderedThumbnail(type: AssetEntry["type"]): boolean {
  return (
    type === "mesh" ||
    type === "texture" ||
    type === "material" ||
    type === "model" ||
    type === "plant" ||
    type === "biome" ||
    type === "vegetation-map"
  );
}

function TypeIcon({ type }: { type: AssetEntry["type"] }) {
  const className = "size-7 text-muted-foreground";
  if (type === "mesh") {
    return <Box className={className} />;
  }
  if (type === "texture") {
    return <ImageIcon className={className} />;
  }
  if (type === "animation") {
    return <Clapperboard className={className} />;
  }
  if (type === "plant") {
    return <Sprout className={className} />;
  }
  if (type === "biome") {
    return <TreePine className={className} />;
  }
  if (type === "vegetation-map") {
    return <MapIcon className={className} />;
  }
  return <File className={className} />;
}

function formatDurationBadge(sec: number): string {
  return `${(Number.isFinite(sec) && sec > 0 ? sec : 0).toFixed(1)}s`;
}

/// A tile's thumbnail fetch state: `loading` while the get-thumbnail promise is
/// outstanding, `ready` once a blob URL resolves, `none` on a reject (unsupported
/// type / failed render) — `none` falls back to the bare type icon. `ready` from the
/// start when the shared cache already has the blob, so a cache hit never flashes the
/// spinner.
type ThumbStatus = "loading" | "ready" | "none";

/// The loading affordance shown in the thumbnail square: a spinner over a dimmed
/// type icon, theme tokens only.
function ThumbnailLoading({ type }: { type: AssetEntry["type"] }) {
  return (
    <div className="relative flex size-full items-center justify-center">
      <span className="opacity-30">
        <TypeIcon type={type} />
      </span>
      <Loader2 className="absolute size-4 animate-spin text-muted-foreground" />
    </div>
  );
}

export interface AssetTileProps {
  entry: AssetEntry;
  /// True while the panel has this tile in inline-rename mode (entered from the
  /// grid's shared context menu).
  renaming?: boolean;
  onView(entry: AssetEntry): void;
  onDelete(entry: AssetEntry): void;
  onSelect(entry: AssetEntry, event: React.MouseEvent): void;
  /// Write the drag payload (the panel owns selection, so it assembles the asset +
  /// folder ids the drag carries).
  onBeginDrag(entry: AssetEntry, event: React.DragEvent): void;
  onEndDrag(): void;
  /// The rename input settled (commit or cancel); the panel clears its rename state.
  onRenameEnd(): void;
}

/// Render the grid thumbnail at 128 px and display it at the 72-px tile size.
const THUMBNAIL_FETCH_SIZE = 128;

/// memo'd so a parent (grid) render skips tiles whose props are unchanged; the
/// callers keep every callback prop referentially stable for that reason.
export const AssetTile = memo(function AssetTile({
  entry,
  renaming = false,
  onView,
  onDelete,
  onSelect,
  onBeginDrag,
  onEndDrag,
  onRenameEnd,
}: AssetTileProps) {
  logRender("AssetTile");
  // Membership-only subscription: the tile re-renders when ITS selected bit flips,
  // never on unrelated selection changes.
  const selected = useEditorStore((s) => s.selectedAssetIds.has(entry.id));
  // Persist the hover highlight while this tile's grid context menu is open.
  const menuActive = useEditorStore(
    (s) => s.assetMenuTarget?.kind === "asset" && s.assetMenuTarget.key === entry.id,
  );
  const dragging = useEditorStore((s) => s.catalogDrag?.assetIds.includes(entry.id) ?? false);
  const [url, setUrl] = useState<string | null>(() =>
    getCachedThumbnailUrl(entry.id, THUMBNAIL_FETCH_SIZE),
  );
  const [status, setStatus] = useState<ThumbStatus>(() =>
    !supportsRenderedThumbnail(entry.type)
      ? "none"
      : getCachedThumbnailUrl(entry.id, THUMBNAIL_FETCH_SIZE)
        ? "ready"
        : "loading",
  );
  const [draft, setDraft] = useState(entry.name);
  // The just-committed name, shown optimistically until the catalog refresh reflects it — so a
  // commit never flashes the stale `entry.name` for a frame between exiting rename mode and the
  // poll landing the new name.
  const [pendingName, setPendingName] = useState<string | null>(null);
  const refreshAssets = useEditorStore((s) => s.refreshAssets);

  // Lazy thumbnail: fetch on mount / id change. The shared cache dedupes across
  // tiles; `loading` shows a spinner, a rejection settles to `none` (the type icon),
  // and a warm cache starts `ready` so re-opening a folder never flashes the spinner.
  useEffect(() => {
    if (!supportsRenderedThumbnail(entry.type)) {
      setUrl(null);
      setStatus("none");
      return;
    }
    let cancelled = false;
    const cached = getCachedThumbnailUrl(entry.id, THUMBNAIL_FETCH_SIZE);
    setUrl(cached);
    setStatus(cached ? "ready" : "loading");
    void getThumbnailUrl(entry.id, THUMBNAIL_FETCH_SIZE)
      .then((resolved) => {
        if (!cancelled) {
          setUrl(resolved);
          setStatus("ready");
        }
      })
      .catch(() => {
        if (!cancelled) {
          setStatus("none"); // a missing thumbnail is not an error toast; the icon is the result
        }
      });
    return () => {
      cancelled = true;
    };
  }, [entry.id, entry.type]);

  // Keep the rename draft in sync when the catalog name changes externally
  // (e.g. an `sa rename-asset` reflected by the poll), but not while renaming.
  useEffect(() => {
    if (!renaming) {
      setDraft(entry.name);
    }
  }, [entry.name, renaming]);

  // Drop the optimistic name once the catalog catches up (rename sets the name verbatim, so the
  // two always converge); the error path clears it in the catch below.
  useEffect(() => {
    if (pendingName !== null && entry.name === pendingName) {
      setPendingName(null);
    }
  }, [entry.name, pendingName]);

  const commitRename = (): void => {
    onRenameEnd();
    const next = draft.trim();
    if (next.length === 0 || next === entry.name) {
      setDraft(entry.name);
      return;
    }
    setPendingName(next); // show the new name this frame; don't flash the stale entry.name
    void client
      .renameAsset(entry.id, next)
      .then(() => refreshAssets())
      .catch((err) => {
        setPendingName(null);
        setDraft(entry.name);
        notifyError(errorText(err));
      });
  };

  return (
    <div className="relative w-[86px]">
      <div
        data-asset-tile-id={entry.id}
        data-asset-item="true"
        // Focusable so a click lands keyboard focus on the tile and the Delete
        // key can run the same delete flow as the context menu item.
        tabIndex={0}
        // Renaming disables the drag so a tile-drag never starts while typing.
        draggable={!renaming}
        onDragStart={(event) => onBeginDrag(entry, event)}
        onDragEnd={onEndDrag}
        onClick={(event) => onSelect(entry, event)}
        onDoubleClick={() => onView(entry)}
        onKeyDown={(event) => {
          if (
            !renaming &&
            matchesBinding(event, "assets.delete", useEditorStore.getState().keyBindings)
          ) {
            event.preventDefault();
            onDelete(entry);
          }
        }}
        className={cn(
          "group flex w-[86px] cursor-grab flex-col gap-1 rounded-md border border-transparent p-1",
          "transition-[opacity,color,border-color,background-color] duration-150 active:cursor-grabbing",
          // Hover affordance only when not already selected — a selected tile keeps its
          // appearance on hover instead of stacking a second highlight on top. An open
          // context menu holds that same hover look so the target tile stays marked.
          !selected && "hover:border-ring hover:bg-accent/40",
          !selected && menuActive && "border-ring bg-accent/40",
          selected && "border-ring bg-accent/60 ring-1 ring-ring",
          dragging && "opacity-45",
        )}
      >
        <div className="relative flex aspect-square w-full items-center justify-center overflow-hidden rounded-sm bg-muted">
          {status === "ready" && url ? (
            <img
              src={url}
              alt={entry.name}
              className="size-full object-contain"
              draggable={false}
            />
          ) : status === "loading" ? (
            <ThumbnailLoading type={entry.type} />
          ) : (
            <TypeIcon type={entry.type} />
          )}
          {entry.type === "animation" && entry.duration ? (
            <span className="absolute bottom-1 right-1 rounded bg-background/80 px-1 py-0.5 text-[9px] font-medium tabular-nums text-muted-foreground">
              {formatDurationBadge(entry.duration)}
            </span>
          ) : null}
        </div>
        {renaming ? (
          <RenameInput
            value={draft}
            onChange={setDraft}
            onCommit={commitRename}
            onCancel={() => {
              onRenameEnd();
              setDraft(entry.name);
            }}
          />
        ) : (
          <button
            type="button"
            className="line-clamp-2 min-h-[2.5em] break-words rounded-sm px-0.5 text-center text-[13px] leading-tight text-foreground"
          >
            {pendingName ?? entry.name}
          </button>
        )}
      </div>
    </div>
  );
});

interface RenameInputProps {
  value: string;
  onChange(value: string): void;
  onCommit(): void;
  onCancel(): void;
}

function RenameInput({ value, onChange, onCommit, onCancel }: RenameInputProps) {
  const ref = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  useOutsideCommit(ref, onCommit);
  return (
    <Input
      ref={ref}
      value={value}
      // Stop pointer/drag propagation so the tile drag never starts mid-edit.
      onPointerDown={(event) => event.stopPropagation()}
      onChange={(event) => onChange(event.currentTarget.value)}
      onBlur={onCommit}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          onCommit();
        } else if (event.key === "Escape") {
          event.preventDefault();
          onCancel();
        }
      }}
      className="h-6 rounded-sm px-1 py-0 text-center font-mono text-[13px]"
    />
  );
}
