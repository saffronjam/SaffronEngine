import { client } from "../../control/client";

/// Client-side thumbnail cache, kept at module scope rather than in Zustand: it holds blob URLs
/// that must survive re-renders without provoking store churn, and one fetch is shared by every
/// tile/picker/viewer asking for the same asset. A cached URL serves any request no larger than
/// the size it was fetched at.
interface ThumbnailCacheEntry {
  url: string;
  size: number;
}

const thumbnailCache = new Map<string, ThumbnailCacheEntry>();
/// In-flight fetches keyed by assetId, so N tiles mounting at once issue one `get-thumbnail`.
const thumbnailInflight = new Map<string, Promise<string>>();

export function base64ToBlob(b64: string, mime = "image/png"): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return new Blob([bytes], { type: mime });
}

export function getCachedThumbnailUrl(assetId: string, size: number): string | null {
  const cached = thumbnailCache.get(assetId);
  return cached && cached.size >= size ? cached.url : null;
}

/// Resolve a blob URL for an asset's thumbnail at (at least) `size` px. A `pending` reply means the
/// engine is generating it on a worker thread, so re-request with backoff until it lands. Rejects
/// on an engine error so the caller can fall back to a type icon.
export async function getThumbnailUrl(assetId: string, size: number): Promise<string> {
  const cached = getCachedThumbnailUrl(assetId, size);
  if (cached) {
    return cached;
  }
  const inflight = thumbnailInflight.get(assetId);
  if (inflight) {
    return inflight;
  }
  const promise = (async (): Promise<string> => {
    let delayMs = 60;
    for (;;) {
      const thumb = await client.getThumbnail(assetId, size);
      if (!thumb.pending) {
        const url = URL.createObjectURL(base64ToBlob(thumb.base64));
        const prev = thumbnailCache.get(assetId);
        if (prev) {
          URL.revokeObjectURL(prev.url);
        }
        thumbnailCache.set(assetId, { url, size });
        return url;
      }
      await new Promise((resolve) => setTimeout(resolve, delayMs));
      delayMs = Math.min(delayMs * 2, 1000);
    }
  })();
  thumbnailInflight.set(assetId, promise);
  try {
    return await promise;
  } finally {
    thumbnailInflight.delete(assetId);
  }
}

/// Resolve a material's studio-lit preview as a base64 PNG, bypassing the blob-URL cache above: a
/// material's look changes on every edit, so the pane needs the freshly keyed render rather than the
/// URL a tile already holds. The engine converges the tile over several frames of its ordinary
/// render loop and replies `pending` until it lands, so re-request with backoff. Rejects on an
/// engine error.
export async function getMaterialPreviewBase64(material: string, size: number): Promise<string> {
  let delayMs = 60;
  for (;;) {
    const preview = await client.getThumbnail(material, size);
    if (!preview.pending) {
      return preview.base64;
    }
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    delayMs = Math.min(delayMs * 2, 1000);
  }
}

/// Revoke every cached blob URL: the catalog changed, so cached images are stale.
export function invalidateThumbnails(): void {
  for (const entry of thumbnailCache.values()) {
    URL.revokeObjectURL(entry.url);
  }
  thumbnailCache.clear();
}
