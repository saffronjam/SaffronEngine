// Route a provider image URL through the bridge's shared resource cache — the `saffron-img://`
// custom scheme served by the shell. The bytes are fetched once (throttled), kept on disk across
// restarts, and served locally, so a screenful of thumbnails never stampedes a provider CDN (which
// was surfacing as broken-image tiles). Non-remote sources (data:/blob:, already-wrapped) pass
// through untouched.
export function cachedImage(url: string): string {
  if (
    !url ||
    url.startsWith("data:") ||
    url.startsWith("blob:") ||
    url.startsWith("saffron-img:")
  ) {
    return url;
  }
  return `saffron-img://fetch/?u=${encodeURIComponent(url)}`;
}
