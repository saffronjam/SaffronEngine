import type { DockLayout, DockNodeId, DockPanelId, DockSpaceKind } from "../dockLayout";
import type { StoreKind } from "../../storefront/types";
import type { AssetSortMode } from "./types";

const EXPANDED_STORAGE_PREFIX = "saffron.expandedIds:";
const DOCK_LAYOUT_STORAGE_PREFIX = "saffron.layout.dock:";
const SUBROWS_STORAGE_KEY = "saffron.showComponentSubrows";
const HIDE_BONES_STORAGE_KEY = "saffron.hideBones";
const ASSET_SORT_STORAGE_KEY = "saffron.assetSort";
const STORE_SELECTED_STORAGE_KEY = "saffron.store.selected";
const STORE_SEARCH_TEXT_STORAGE_KEY = "saffron.store.searchText";
const STORE_KIND_STORAGE_KEY = "saffron.store.kind";
const DEV_MODE_STORAGE_KEY = "saffron.devMode";
const METRICS_RANGE_STORAGE_KEY = "saffron.metricsRangeSec";
const METRICS_BUCKET_STORAGE_KEY = "saffron.metricsBucketMs";
const METRICS_REFRESH_STORAGE_KEY = "saffron.metricsRefreshMs";
const CAPTURE_WINDOW_STORAGE_KEY = "saffron.captureWindowFrames";
const CAPTURE_STATS_STORAGE_KEY = "saffron.captureIncludeStats";

/// Every read/write goes through these two so a private-mode `localStorage` throw degrades the
/// preference to session-only instead of breaking the action.
function readSetting(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeSetting(key: string, value: string | null): void {
  try {
    if (value === null) {
      localStorage.removeItem(key);
    } else {
      localStorage.setItem(key, value);
    }
  } catch {
    // Session-only from here on.
  }
}

function readNumber(key: string, fallback: number, valid: (value: number) => boolean): number {
  const raw = readSetting(key);
  if (raw === null) {
    return fallback;
  }
  const value = Number(raw);
  return Number.isFinite(value) && valid(value) ? value : fallback;
}

/// Expand-state is per project path, so switching projects never carries a stale outliner shape.
export function persistExpanded(path: string | undefined, ids: Set<string>): void {
  if (path) {
    writeSetting(EXPANDED_STORAGE_PREFIX + path, JSON.stringify([...ids]));
  }
}

export function loadExpanded(path: string): Set<string> {
  try {
    const raw = readSetting(EXPANDED_STORAGE_PREFIX + path);
    return raw ? new Set(JSON.parse(raw) as string[]) : new Set<string>();
  } catch {
    return new Set<string>();
  }
}

interface PersistedDockLayouts {
  version: 2;
  layouts: Record<DockSpaceKind, DockLayout>;
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>;
}

/// Both island trees plus last-location memory under one per-project key. No-op without a project.
export function persistDockLayouts(
  path: string | undefined,
  layouts: Record<DockSpaceKind, DockLayout>,
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>,
): void {
  if (!path) {
    return;
  }
  const payload: PersistedDockLayouts = { version: 2, layouts, lastLocation };
  writeSetting(DOCK_LAYOUT_STORAGE_PREFIX + path, JSON.stringify(payload));
}

export function loadDockLayouts(path: string | undefined): PersistedDockLayouts | null {
  if (!path) {
    return null;
  }
  const raw = readSetting(DOCK_LAYOUT_STORAGE_PREFIX + path);
  if (!raw) {
    return null;
  }
  try {
    const parsed = JSON.parse(raw) as PersistedDockLayouts;
    return parsed?.version === 2 && parsed.layouts ? parsed : null;
  } catch {
    return null;
  }
}

export function loadShowSubrows(): boolean {
  return readSetting(SUBROWS_STORAGE_KEY) === "1";
}

export function persistShowSubrows(value: boolean): void {
  writeSetting(SUBROWS_STORAGE_KEY, value ? "1" : "0");
}

/// Bones default to hidden — they belong in the asset/rig editor, not the scene outliner.
export function loadHideBones(): boolean {
  const raw = readSetting(HIDE_BONES_STORAGE_KEY);
  return raw === null ? true : raw === "1";
}

export function persistHideBones(value: boolean): void {
  writeSetting(HIDE_BONES_STORAGE_KEY, value ? "1" : "0");
}

const ASSET_SORT_MODES: readonly AssetSortMode[] = [
  "name-asc",
  "name-desc",
  "created-desc",
  "created-asc",
];

export function loadAssetSort(): AssetSortMode {
  const raw = readSetting(ASSET_SORT_STORAGE_KEY);
  return ASSET_SORT_MODES.includes(raw as AssetSortMode) ? (raw as AssetSortMode) : "name-asc";
}

export function persistAssetSort(value: AssetSortMode): void {
  writeSetting(ASSET_SORT_STORAGE_KEY, value);
}

const STORE_KINDS: readonly StoreKind[] = ["model", "hdri", "material", "texture"];

/// The Asset Store's store + query persist app-wide, not per project, so reopening the Store
/// returns you to where you left off.
export function loadStoreSelected(): string | null {
  return readSetting(STORE_SELECTED_STORAGE_KEY);
}

export function persistStoreSelected(value: string | null): void {
  writeSetting(STORE_SELECTED_STORAGE_KEY, value);
}

export function loadStoreSearchText(): string {
  return readSetting(STORE_SEARCH_TEXT_STORAGE_KEY) ?? "";
}

export function loadStoreKind(): StoreKind | null {
  const raw = readSetting(STORE_KIND_STORAGE_KEY);
  return STORE_KINDS.includes(raw as StoreKind) ? (raw as StoreKind) : null;
}

export function persistStoreQuery(text: string, kind: StoreKind | null): void {
  writeSetting(STORE_SEARCH_TEXT_STORAGE_KEY, text);
  writeSetting(STORE_KIND_STORAGE_KEY, kind);
}

/// `VITE_SAFFRON_DEV_MODE=1` forces dev mode on for the session without touching the persisted flag.
export function loadDevMode(): boolean {
  return import.meta.env.VITE_SAFFRON_DEV_MODE === "1" || readSetting(DEV_MODE_STORAGE_KEY) === "1";
}

export function persistDevMode(value: boolean): void {
  writeSetting(DEV_MODE_STORAGE_KEY, value ? "1" : "0");
}

export function loadMetricsRangeSec(): number {
  return readNumber(METRICS_RANGE_STORAGE_KEY, 30, (v) => v > 0);
}

export function persistMetricsRangeSec(value: number): void {
  writeSetting(METRICS_RANGE_STORAGE_KEY, String(value));
}

export function loadMetricsBucketMs(): number {
  return readNumber(METRICS_BUCKET_STORAGE_KEY, 250, (v) => v >= 10 && v <= 5000);
}

export function persistMetricsBucketMs(value: number): void {
  writeSetting(METRICS_BUCKET_STORAGE_KEY, String(value));
}

export function loadMetricsRefreshMs(): number {
  return readNumber(METRICS_REFRESH_STORAGE_KEY, 1000, (v) => v >= 100);
}

export function persistMetricsRefreshMs(value: number): void {
  writeSetting(METRICS_REFRESH_STORAGE_KEY, String(value));
}

/// Clamped to the engine's [1, 256] capture cap.
export function loadCaptureWindowFrames(): number {
  return Math.floor(readNumber(CAPTURE_WINDOW_STORAGE_KEY, 1, (v) => v >= 1 && v <= 256));
}

export function persistCaptureWindowFrames(value: number): void {
  writeSetting(CAPTURE_WINDOW_STORAGE_KEY, String(value));
}

export function loadCaptureIncludeStats(): boolean {
  return readSetting(CAPTURE_STATS_STORAGE_KEY) === "1";
}

export function persistCaptureIncludeStats(value: boolean): void {
  writeSetting(CAPTURE_STATS_STORAGE_KEY, value ? "1" : "0");
}
