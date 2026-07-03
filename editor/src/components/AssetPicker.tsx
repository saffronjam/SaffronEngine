/// A thumbnail combo for a `Uuid` component field. It lists `(none)` + every catalog
/// asset of `assetType`; selecting one calls `onChange(id)`, `(none)` calls
/// `onChange("0")`. It is ALSO an HTML5 drop TARGET accepting `application/x-sa-asset`,
/// but only when the dragged asset's type matches `assetType`.
///
/// The caller (the inspector's fieldRenderer) owns the write: mesh/albedo go through
/// `assignAsset`, everything else (sky texture, other Uuid fields) through
/// `setComponentField`. The picker is field-agnostic — it only emits `onChange`.
///
/// Lives in the side docks (inspector / environment); the popover anchors there.
import { useEffect, useState } from "react";
import { Box, Check, ChevronsUpDown, File, Image as ImageIcon, Loader2 } from "lucide-react";
import { getCachedThumbnailUrl, getThumbnailUrl, useEditorStore } from "../state/store";
import { ASSET_DND_MIME, assetIdsFromPayload, readAssetPayload } from "./AssetTile";
import type { AssetEntry } from "../protocol";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

const NONE_UUID = "0";

/// The catalog `type` an `AssetKind` field picks from. (Mesh fields show meshes;
/// albedo/sky/texture fields show textures; modelId shows `.smodel` containers; a
/// clip slot shows animation assets.)
export type PickerAssetKind = "mesh" | "texture" | "material" | "model" | "animation";

/// A small thumbnail swatch fetched at 64 px and shown at the given CSS size; falls
/// back to a lucide type icon while loading or on failure. Seeds from the shared
/// cache so a warm-cache mount paints the image on the first frame (the popover
/// remounts its rows on every open).
function AssetSwatch({ asset, size }: { asset: AssetEntry; size: number }) {
  const [url, setUrl] = useState<string | null>(() => getCachedThumbnailUrl(asset.id, 64));
  const [status, setStatus] = useState<"loading" | "ready" | "none">(() =>
    getCachedThumbnailUrl(asset.id, 64) ? "ready" : "loading",
  );
  useEffect(() => {
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
  }, [asset.id]);

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
  const Icon = asset.type === "mesh" ? Box : asset.type === "texture" ? ImageIcon : File;
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

  // Warm the thumbnail cache while the popover is closed, so the first open
  // paints images instead of fallback icons (the shared cache dedupes, so this
  // costs nothing once fetched).
  useEffect(() => {
    for (const asset of assets) {
      if (asset.type === assetType) {
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
              {selected ? <AssetSwatch asset={selected} size={16} /> : null}
              <span className="truncate">{selected ? selected.name : "(none)"}</span>
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
            {options.map((asset) => (
              <PickerRow
                key={asset.id}
                label={asset.name}
                swatch={<AssetSwatch asset={asset} size={16} />}
                active={asset.id === value}
                onSelect={() => pick(asset.id)}
              />
            ))}
            {options.length === 0 ? (
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
