// The asset detail modal opened by a card's expand button: a large gallery on the left and
// the asset's metadata + import controls on the right, mirroring the provider modal's layout.
import { invoke } from "@tauri-apps/api/core";
import { ExternalLink } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogScopedOverlay,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

import { errorText, notifyError } from "../lib/flash";
import { cachedImage } from "./cachedImage";
import { GalleryViewer } from "./GalleryViewer";
import { ImportControls } from "./ImportControls";
import { useStoreOverlayContainer } from "./storeOverlay";
import type { GalleryImage, StoreResult } from "./types";
import { useGalleryNav } from "./useGallery";

function openExternal(url: string) {
  void invoke("open_external", { url }).catch((err: unknown) => notifyError(errorText(err)));
}

function fmtSize(bytes?: number): string | null {
  if (!bytes) return null;
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

export function AssetDetailModal({
  result,
  images,
  open,
  onOpenChange,
}: {
  result: StoreResult;
  images: GalleryImage[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // The modal navigates independently of the card behind it.
  const nav = useGalleryNav(images.length);
  const container = useStoreOverlayContainer();
  const facts: { label: string; value: string }[] = [];
  if (result.kind) facts.push({ label: "Type", value: result.kind });
  if (result.triCount) facts.push({ label: "Triangles", value: result.triCount.toLocaleString() });
  if (result.resolution) facts.push({ label: "Resolution", value: result.resolution });
  const size = fmtSize(result.fileSize);
  if (size) facts.push({ label: "Size", value: size });

  // Scoped to the Store view's region; hold off until that container exists so we never briefly
  // portal to document.body.
  const visible = open && container != null;

  return (
    <Dialog open={visible} onOpenChange={onOpenChange} modal={false}>
      <DialogScopedOverlay
        open={visible}
        container={container}
        onClose={() => onOpenChange(false)}
      />
      <DialogContent
        container={container}
        onInteractOutside={(e) => e.preventDefault()}
        onPointerDownOutside={(e) => e.preventDefault()}
        className="absolute flex h-[560px] w-auto max-w-[calc(100%-2rem)] gap-0 overflow-hidden p-0 sm:max-w-[940px]"
      >
        <div className="flex w-[560px] shrink-0 flex-col bg-muted">
          <div className="min-h-0 flex-1 p-4">
            <GalleryViewer images={images} nav={nav} alt={result.name} showLabel large />
          </div>
          {images.length > 1 ? (
            <div className="flex shrink-0 gap-1.5 overflow-x-auto border-t border-border p-2">
              {images.map((img, i) => (
                <button
                  key={img.url}
                  type="button"
                  onClick={() => nav.goTo(i)}
                  aria-label={img.label ?? `Image ${i + 1}`}
                  className={cn(
                    "size-12 shrink-0 overflow-hidden rounded border bg-background",
                    i === nav.index ? "border-ring ring-1 ring-ring" : "border-border",
                  )}
                >
                  <img
                    src={cachedImage(img.url)}
                    alt=""
                    className="size-full object-cover"
                    loading="lazy"
                  />
                </button>
              ))}
            </div>
          ) : null}
        </div>

        <div className="flex w-[380px] shrink-0 flex-col border-l border-border p-6">
          <Badge variant="secondary" className="w-fit text-[10px]">
            {result.store.displayName}
          </Badge>
          <DialogTitle className="mt-2 text-base leading-snug">{result.name}</DialogTitle>
          {result.author ? (
            <DialogDescription className="mt-0.5 text-xs">by {result.author}</DialogDescription>
          ) : null}

          <div className="mt-4 flex flex-wrap gap-1.5">
            <Badge variant="outline" className="text-[10px] uppercase">
              {result.license.id}
            </Badge>
            {result.license.requiresAttribution ? (
              <Badge variant="outline" className="text-[10px]">
                Attribution required
              </Badge>
            ) : null}
          </div>

          {facts.length > 0 ? (
            <dl className="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {facts.map((f) => (
                <div key={f.label} className="contents">
                  <dt className="text-muted-foreground">{f.label}</dt>
                  <dd className="text-right text-foreground capitalize">{f.value}</dd>
                </div>
              ))}
            </dl>
          ) : null}

          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mt-3 h-7 w-fit px-0 text-xs text-muted-foreground hover:text-foreground"
            onClick={() => openExternal(result.sourceUrl)}
          >
            <ExternalLink className="size-3.5" />
            Open on {result.store.displayName}
          </Button>

          <div className="mt-auto pt-4">
            <ImportControls result={result} size="default" />
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
