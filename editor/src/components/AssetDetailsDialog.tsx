/// The asset Details modal: on-disk metadata for one asset (filename, location, type,
/// size, mesh vertex/triangle counts, modified time), fetched via `probe-asset`. Opened
/// from the grid context menu's Details item.
import { useEffect, useRef, useState } from "react";
import { client } from "../control/client";
import type { AssetMetadataDto } from "../protocol";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

function formatDate(unixSeconds: number): string {
  if (!unixSeconds) {
    return "—";
  }
  return new Date(unixSeconds * 1000).toLocaleString();
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</span>
      <span className="break-words font-mono text-xs text-foreground">{value}</span>
    </div>
  );
}

export function AssetDetailsDialog({
  assetId,
  onClose,
}: {
  assetId: string | null;
  onClose(): void;
}) {
  const [metadata, setMetadata] = useState<AssetMetadataDto | null>(null);
  const loadedIdRef = useRef<string | null>(null);

  // Probe on-disk metadata for the target asset; ignore a stale response if the target
  // changes mid-flight.
  useEffect(() => {
    // On close (assetId → null) keep the last content mounted so the modal fades out at a
    // stable size instead of blinking to "Loading…" during the exit animation.
    if (!assetId) {
      return;
    }
    // Only blank to "Loading…" when opening a DIFFERENT asset; reopening the same one keeps
    // its content and refreshes silently.
    if (assetId !== loadedIdRef.current) {
      setMetadata(null);
    }
    loadedIdRef.current = assetId;
    let active = true;
    void client
      .probeAsset(assetId)
      .then((meta) => {
        if (active) {
          setMetadata(meta);
        }
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [assetId]);

  return (
    <Dialog
      open={assetId !== null}
      onOpenChange={(open) => {
        if (!open) {
          onClose();
        }
      }}
    >
      <DialogContent aria-describedby={undefined} className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Details</DialogTitle>
        </DialogHeader>
        {metadata ? (
          <div className="flex flex-col gap-2.5">
            <Row label="Filename" value={metadata.name} />
            <Row label="Location" value={metadata.folder ?? "Root"} />
            <Row label="Type" value={metadata.type} />
            <Row label="Size" value={formatBytes(metadata.sizeBytes)} />
            {metadata.vertexCount !== undefined ? (
              <Row label="Vertices" value={metadata.vertexCount.toLocaleString()} />
            ) : null}
            {metadata.triangleCount !== undefined ? (
              <Row label="Triangles" value={metadata.triangleCount.toLocaleString()} />
            ) : null}
            <Row label="Created" value={formatDate(metadata.createdAt)} />
          </div>
        ) : (
          <p className="text-xs italic text-muted-foreground">Loading…</p>
        )}
      </DialogContent>
    </Dialog>
  );
}
