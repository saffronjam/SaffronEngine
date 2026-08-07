/// The asset View modal: a 512 px preview of one catalog asset, fetched as a base64 PNG. The native
/// viewport surface always paints over the webview, so while the viewer is open `viewportHidden`
/// parks it and the dialog is visible.
import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { client } from "../control/client";
import { base64ToBlob, useEditorStore } from "../state/store";
import type { AssetEntry } from "../protocol";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/// Preview render size (px).
const VIEW_SIZE = 512;

export interface AssetViewerProps {
  /// The asset to preview, or null when the modal is closed.
  entry: AssetEntry | null;
  onClose(): void;
}

export function AssetViewer({ entry, onClose }: AssetViewerProps) {
  const open = entry !== null;
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          onClose();
        }
      }}
    >
      <DialogContent className="sm:max-w-[560px]">
        <DialogHeader>
          <DialogTitle className="truncate font-mono text-sm">{entry?.name ?? "Asset"}</DialogTitle>
        </DialogHeader>
        {entry ? <AssetPreview entry={entry} /> : null}
      </DialogContent>
    </Dialog>
  );
}

export function AssetPreview({ entry, className }: { entry: AssetEntry; className?: string }) {
  const [url, setUrl] = useState<string | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "none">("loading");
  const setViewportHidden = useEditorStore((s) => s.setViewportHidden);

  // Park the native viewport off-screen while a webview preview occupies the main
  // content area; restore it on unmount.
  useEffect(() => {
    setViewportHidden(true);
    return () => {
      setViewportHidden(false);
    };
  }, [setViewportHidden]);

  // Fetch the 512 preview when the asset changes; revoke the previous URL and the
  // final URL on close/unmount (no blob leak — this URL is NOT the shared cache).
  useEffect(() => {
    let cancelled = false;
    let created: string | null = null;
    setStatus("loading");
    setUrl(null);
    // A cold cache-miss replies `pending` while the engine worker generates it; retry with backoff.
    void (async () => {
      let delayMs = 60;
      try {
        for (;;) {
          const thumb = await client.viewAsset(entry.id, VIEW_SIZE);
          if (cancelled) {
            return;
          }
          if (!thumb.pending) {
            created = URL.createObjectURL(base64ToBlob(thumb.base64));
            setUrl(created);
            setStatus("ready");
            return;
          }
          await new Promise((resolve) => setTimeout(resolve, delayMs));
          delayMs = Math.min(delayMs * 2, 1000);
        }
      } catch {
        if (!cancelled) {
          setStatus("none"); // no preview for this asset; not an error toast
        }
      }
    })();
    return () => {
      cancelled = true;
      if (created) {
        URL.revokeObjectURL(created);
      }
      setUrl(null);
    };
  }, [entry]);

  return (
    <div
      className={cn(
        "flex aspect-square w-full items-center justify-center overflow-hidden rounded-md border border-border bg-muted",
        className,
      )}
    >
      {status === "ready" && url ? (
        <img src={url} alt={entry.name} className="size-full object-contain" draggable={false} />
      ) : status === "loading" ? (
        <Loader2 className="size-6 animate-spin text-muted-foreground" />
      ) : (
        <span className="text-xs italic text-muted-foreground">Preview unavailable</span>
      )}
    </div>
  );
}
