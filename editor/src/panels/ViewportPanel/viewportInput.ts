/// Pointer travel (CSS px) below which a press-release is treated as a click
/// (ray-pick) rather than a gizmo drag.
export const DRAG_THRESHOLD_PX = 3;

/// Throttle for streamed fly-cam input while pointer lock is held, in milliseconds.
/// Look deltas accumulate between sends, so nothing is lost to the throttle.
export const FLY_STREAM_MS = 16;

/// Throttle for streamed gizmo pointer phases (hover/drag), in milliseconds.
export const GIZMO_STREAM_MS = 16;

/// Normalized [0,1] viewport coordinate, (0,0) = top-left.
export interface Uv {
  u: number;
  v: number;
}

export function clientPointToUv(el: HTMLElement, clientX: number, clientY: number): Uv {
  const rect = el.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) {
    return { u: 0, v: 0 };
  }
  const u = (clientX - rect.left) / rect.width;
  const v = (clientY - rect.top) / rect.height;
  return {
    u: Math.min(1, Math.max(0, u)),
    v: Math.min(1, Math.max(0, v)),
  };
}

/// Map a pointer event to {u,v} in [0,1] using the panel's own client rect.
export function eventToUv(el: HTMLElement, event: PointerEvent): Uv {
  return clientPointToUv(el, event.clientX, event.clientY);
}

export function scriptKeyFromEvent(event: KeyboardEvent): string | null {
  if (event.metaKey) {
    return null;
  }
  if (event.key === " ") {
    return "space";
  }
  if (event.key === "Shift") {
    return "shift";
  }
  if (event.key === "Control") {
    return "control";
  }
  if (event.key === "Alt") {
    return "alt";
  }
  return event.key.toLowerCase();
}

export function targetOwnsTextInput(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) {
    return false;
  }
  return target.closest("input, textarea, select, [contenteditable='true']") !== null;
}
