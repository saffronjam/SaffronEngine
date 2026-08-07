/// A synchronous event bus the dock `Layout` pings whenever a PanelGroup's layout settles, so the
/// `ViewportPanel` can fire an exact resize-end commit for the native surface. Deliberately outside
/// the store: a layout change is a transient signal, not editor state. The ViewportPanel's
/// ResizeObserver already catches geometry changes during a drag; this bus covers the rest,
/// including tab switches that disturb the surface without changing the measured rect.
export interface LayoutSettledEvent {
  force?: boolean;
}

type LayoutListener = (event: LayoutSettledEvent) => void;

const listeners = new Set<LayoutListener>();

/// Subscribe to layout-settled notifications. Returns an unsubscribe fn.
export function onLayoutSettled(listener: LayoutListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/// Notify every subscriber that a dock layout just settled.
export function emitLayoutSettled(event: LayoutSettledEvent = {}): void {
  for (const listener of listeners) {
    listener(event);
  }
}
