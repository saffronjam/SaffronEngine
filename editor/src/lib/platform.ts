import type { CSSProperties } from "react";

/// The host platform, from the embedded Chromium's user agent. Drives the small per-OS visual
/// accommodations (macOS native window chrome: traffic lights instead of the custom
/// minimize/maximize/close buttons, no client-side resize strips); everything else is identical
/// across platforms.
export const IS_MACOS = navigator.userAgent.includes("Macintosh");

/// `-webkit-app-region` markers for the frameless titlebar. Chromium reports every `drag`
/// rectangle to the shell (CEF's `OnDraggableRegionsChanged`), which starts a native window drag
/// when a press lands in one — so the titlebar's empty areas move the window while its controls
/// (tabs, buttons) mark themselves `no-drag` to stay clickable.
export const DRAG_REGION = { WebkitAppRegion: "drag" } as unknown as CSSProperties;
export const NO_DRAG_REGION = { WebkitAppRegion: "no-drag" } as unknown as CSSProperties;
