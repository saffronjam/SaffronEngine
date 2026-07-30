/// A thumbnail combo for a `Uuid` component field: `(none)` plus every catalog asset of `assetType`.
/// Also an HTML5 drop target, but only for a dragged asset whose type matches. The picker is
/// field-agnostic — it only emits `onChange`, and the caller owns the write.
import { useEffect, useState } from "react";
import {
  Box,
  Check,
  ChevronsUpDown,
  Circle,
  File,
  Image as ImageIcon,
  Loader2,
  Map as MapIcon,
  Square,
  Sprout,
  TreePine,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { getCachedThumbnailUrl, getThumbnailUrl, useEditorStore } from "../state/store";
import {
  ASSET_DND_MIME,
  assetIdsFromPayload,
  readAssetPayload,
  supportsRenderedThumbnail,
} from "./AssetTile";
import type { AssetEntry } from "../protocol";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

const NONE_UUID = "0";

/// The catalog `type` an `AssetKind` field picks from. (Mesh fields show meshes;
/// albedo/sky/texture fields show textures; modelId shows `.smodel` containers; a
/// clip slot shows animation assets.)
export type PickerAssetKind =
  | "mesh"
  | "texture"
  | "material"
  | "model"
  | "animation"
  | "vegetation-map";

/// The native built-in primitive meshes, mirroring the engine's `BuiltinMesh` reserved
/// ids (`saffron-assets`). They are choosable in the mesh picker but are never catalog
/// rows, so they carry their id + label here rather than coming from `assets`.
const BUILTIN_MESHES: { id: string; label: string; icon: LucideIcon }[] = [
  { id: "3", label: "Cube", icon: Box },
  { id: "4", label: "Plane", icon: Square },
  { id: "5", label: "Sphere", icon: Circle },
];

/// The built-in primitive for a mesh id, or `undefined` for a catalog / none value. The
/// single `< 1024` decoder on the editor side (mirrors `BuiltinMesh::from_reserved_id`).
function builtinMesh(value: string): (typeof BUILTIN_MESHES)[number] | undefined {
  return BUILTIN_MESHES.find((b) => b.id === value);
}

/// A small badge marking a value as engine-native rather than a project asset.
function BuiltinChip() {
  return (
    <span className="flex-none rounded-sm bg-muted px-1 py-px text-[9px] font-medium uppercase tracking-wide text-muted-foreground">
      Built-in
    </span>
  );
}

/// A subtle section header inside the picker popover.
function PickerGroupLabel({ children }: { children: React.ReactNode }) {
  return (
    <span className="px-1.5 pt-1 text-[9px] uppercase tracking-wide text-muted-foreground">
      {children}
    </span>
  );
}

/// A small thumbnail swatch fetched at 64 px and shown at the given CSS size; falls
/// back to a lucide type icon while loading or on failure. Seeds from the shared
/// cache so a warm-cache mount paints the image on the first frame (the popover
/// remounts its rows on every open).
function AssetSwatch({ asset, size }: { asset: AssetEntry; size: number }) {
  const supportsThumbnail = supportsRenderedThumbnail(asset.type);
  const [url, setUrl] = useState<string | null>(() =>
    supportsThumbnail ? getCachedThumbnailUrl(asset.id, 64) : null,
  );
  const [status, setStatus] = useState<"loading" | "ready" | "none">(() =>
    !supportsThumbnail ? "none" : getCachedThumbnailUrl(asset.id, 64) ? "ready" : "loading",
  );
  useEffect(() => {
    if (!supportsThumbnail) {
      setUrl(null);
      setStatus("none");
      return;
    }
    let cancelled = false;
    const cached = getCachedThumbnailUrl(asset.id, 64);
    setUrl(cached);
    setStatus(cached ? "ready" : "loading");
    void getThumbnailUrl(asset.id, 64)
      .then((resolved) => {
        if (!cancelled) {
          setUrl(resolved);
          setStatus("ready");
        }
      })
      .catch(() => {
        if (!cancelled) {
          setStatus("none");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [asset.id, supportsThumbnail]);

  const style = { width: size, height: size } as const;
  if (status === "ready" && url) {
    return (
      <img
        src={url}
        alt={asset.name}
        style={style}
        className="flex-none rounded-sm object-contain"
        draggable={false}
      />
    );
  }
  const Icon =
    asset.type === "mesh"
      ? Box
      : asset.type === "texture"
        ? ImageIcon
        : asset.type === "plant"
          ? Sprout
          : asset.type === "biome"
            ? TreePine
            : asset.type === "vegetation-map"
              ? MapIcon
              : File;
  return (
    <span
      style={style}
      className="flex flex-none items-center justify-center rounded-sm bg-muted text-muted-foreground"
    >
      {status === "loading" ? (
        <Loader2 className="size-3 animate-spin" />
      ) : (
        <Icon className="size-3" />
      )}
    </span>
  );
}

export interface AssetPickerProps {
  /// Current value (a Uuid string; "0"/"" = none).
  value: string;
  /// Which catalog type this field references.
  assetType: PickerAssetKind;
  onChange(assetId: string): void;
}

export function AssetPicker({ value, assetType, onChange }: AssetPickerProps) {
  const assets = useEditorStore((s) => s.assets);
  const [open, setOpen] = useState(false);
  const [dropActive, setDropActive] = useState(false);

  const options = assets.filter((a) => a.type === assetType);
  const isNone = value === NONE_UUID || value === "";
  // The mesh picker offers the native built-in primitives (cube/plane/sphere) above the
  // catalog rows; they are reserved-id meshes, never catalog assets.
  const showBuiltins = assetType === "mesh";
  const selectedBuiltin = showBuiltins ? builtinMesh(value) : undefined;

  // Warm the thumbnail cache while the popover is closed, so the first open
  // paints images instead of fallback icons (the shared cache dedupes, so this
  // costs nothing once fetched).
  useEffect(() => {
    for (const asset of assets) {
      if (asset.type === assetType && supportsRenderedThumbnail(asset.type)) {
        void getThumbnailUrl(asset.id, 64).catch(() => {});
      }
    }
  }, [assets, assetType]);
  const selected = isNone ? null : (options.find((a) => a.id === value) ?? null);

  const pick = (id: string): void => {
    onChange(id);
    setOpen(false);
  };

  // Drop TARGET: accept an asset tile only when its type matches this field;
  // ignore OS file drops here.
  const onDrop = (event: React.DragEvent<HTMLDivElement>): void => {
    event.preventDefault();
    setDropActive(false);
    // The catalog drag carries asset ids (single or multi-select) without a type, so
    // resolve them against the catalog and assign the first one matching this field.
    const ids = assetIdsFromPayload(readAssetPayload(event.dataTransfer));
    const match = ids
      .map((id) => assets.find((a) => a.id === id))
      .find((a) => a?.type === assetType);
    if (match) {
      onChange(match.id);
    }
  };
  const onDragOver = (event: React.DragEvent<HTMLDivElement>): void => {
    if (event.dataTransfer.types.includes(ASSET_DND_MIME)) {
      // Allow the drop and signal a copy.
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
      setDropActive(true);
    }
  };

  return (
    <div
      onDragOver={onDragOver}
      onDragLeave={() => setDropActive(false)}
      onDrop={onDrop}
      className={cn("rounded-sm", dropActive && "ring-2 ring-ring")}
    >
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-7 w-full justify-between gap-1.5 px-1.5 font-mono text-[11px]"
          >
            <span className="flex min-w-0 items-center gap-1.5">
              {selectedBuiltin ? (
                <>
                  <selectedBuiltin.icon className="size-4 flex-none text-muted-foreground" />
                  <span className="truncate">{selectedBuiltin.label}</span>
                  <BuiltinChip />
                </>
              ) : selected ? (
                <>
                  <AssetSwatch asset={selected} size={16} />
                  <span className="truncate">{selected.name}</span>
                </>
              ) : (
                <span className="truncate">(none)</span>
              )}
            </span>
            <ChevronsUpDown className="size-3 flex-none opacity-50" />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-(--radix-popover-trigger-width) p-1">
          {/* Native max-height + overflow scroll: a Radix ScrollArea's `h-full` viewport
              can't resolve against a max-height-only (auto-height) parent, so it would grow
              to full content height and overshoot the cap instead of scrolling. */}
          <div className="flex max-h-56 flex-col gap-0.5 overflow-y-auto">
            <PickerRow label="(none)" active={isNone} onSelect={() => pick(NONE_UUID)} />
            {showBuiltins ? (
              <>
                <PickerGroupLabel>Built-in</PickerGroupLabel>
                {BUILTIN_MESHES.map((b) => (
                  <PickerRow
                    key={b.id}
                    label={b.label}
                    swatch={<b.icon className="size-4 flex-none text-muted-foreground" />}
                    active={b.id === value}
                    onSelect={() => pick(b.id)}
                  />
                ))}
                {options.length > 0 ? <PickerGroupLabel>Assets</PickerGroupLabel> : null}
              </>
            ) : null}
            {options.map((asset) => (
              <PickerRow
                key={asset.id}
                label={asset.name}
                swatch={<AssetSwatch asset={asset} size={16} />}
                active={asset.id === value}
                onSelect={() => pick(asset.id)}
              />
            ))}
            {options.length === 0 && !showBuiltins ? (
              <span className="px-2 py-1 text-[11px] italic text-muted-foreground">
                No {assetType} assets
              </span>
            ) : null}
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

interface PickerRowProps {
  label: string;
  swatch?: React.ReactNode;
  active: boolean;
  onSelect(): void;
}

function PickerRow({ label, swatch, active, onSelect }: PickerRowProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "flex w-full items-center gap-1.5 rounded-sm px-1.5 py-1 text-left font-mono text-[11px]",
        "hover:bg-accent hover:text-accent-foreground",
        active && "bg-accent/60",
      )}
    >
      {swatch ?? <span className="size-4 flex-none" />}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {active ? <Check className="size-3 flex-none text-foreground" /> : null}
    </button>
  );
}
