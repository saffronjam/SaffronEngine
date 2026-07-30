import { create } from "zustand";
import { client } from "../../control/client";
import { emitLayoutSettled } from "../../app/layoutBus";
import { persistDockLayouts } from "./persistence";
import { createAssetSlice } from "./slices/assets";
import { createDockSlice } from "./slices/dock";
import { createHistorySlice, entityCreationEdit } from "./slices/history";
import { createProjectSlice } from "./slices/project";
import { createSceneSlice } from "./slices/scene";
import { createStorefrontSlice } from "./slices/storefront";
import { createTabSlice } from "./slices/tabs";
import { createTelemetrySlice } from "./slices/telemetry";
import { createUiSlice } from "./slices/ui";
import { createVegetationSlice } from "./slices/vegetation";
import type { EditorState } from "./types";

export { isPanelOpen } from "./slices/dock";

export const useEditorStore = create<EditorState>((set, get) => ({
  ...createSceneSlice(set, get),
  ...createAssetSlice(set, get),
  ...createTabSlice(set, get),
  ...createDockSlice(set),
  ...createTelemetrySlice(set),
  ...createProjectSlice(set, get),
  ...createUiSlice(set),
  ...createVegetationSlice(set),
  ...createStorefrontSlice(set),
  ...createHistorySlice(set, get),
}));

// The one viewport re-glue path for every dock mutation: a `dockLayouts` identity change forces a
// layout-settled on the next frame, so no open/close/move/drop/reset/load call site can forget to
// re-commit the subsurface bounds. Over-emitting is harmless — the inactive island's host sits at
// 0×0 and computeBounds skips degenerate rects.
let lastDockLayouts = useEditorStore.getState().dockLayouts;
useEditorStore.subscribe((s) => {
  if (s.dockLayouts !== lastDockLayouts) {
    lastDockLayouts = s.dockLayouts;
    requestAnimationFrame(() => emitLayoutSettled({ force: true }));
  }
});

// Debounced dock persistence, coalescing rapid drags.
let lastPersistedDock = useEditorStore.getState().dockLayouts;
let lastPersistedLoc = useEditorStore.getState().lastLocation;
let dockPersistTimer: ReturnType<typeof setTimeout> | undefined;
useEditorStore.subscribe((s) => {
  if (s.dockLayouts === lastPersistedDock && s.lastLocation === lastPersistedLoc) {
    return;
  }
  lastPersistedDock = s.dockLayouts;
  lastPersistedLoc = s.lastLocation;
  clearTimeout(dockPersistTimer);
  dockPersistTimer = setTimeout(() => {
    const state = useEditorStore.getState();
    persistDockLayouts(state.project?.path, state.dockLayouts, state.lastLocation);
  }, 300);
});

/// Record an entity-creation edit onto the scene history.
export function recordEntityCreation(entityId: string, label: string): void {
  useEditorStore.getState().pushEdit(entityCreationEdit(entityId, label), "scene");
}

/// Hydrate the keybinding overrides from appdata/settings.json once at app start. A missing or
/// unreadable file leaves the registry defaults active.
export async function loadEditorSettings(): Promise<void> {
  try {
    const settings = await client.loadEditorSettings();
    useEditorStore.getState().hydrateKeyBindings(settings.keyBindings ?? {});
  } catch {
    // Defaults stay active.
  }
}

/// Run a native file-dialog thunk under the app-side dialog lock: a no-op returning null if one is
/// already open, otherwise `nativeDialogOpen` is held for the dialog's lifetime so the controls that
/// spawn dialogs grey out. The lock covers only the dialog, not the engine work that follows it.
export async function withNativeDialog<T>(fn: () => Promise<T>): Promise<T | null> {
  const store = useEditorStore.getState();
  if (store.nativeDialogOpen) {
    return null;
  }
  store.setNativeDialogOpen(true);
  try {
    return await fn();
  } finally {
    useEditorStore.getState().setNativeDialogOpen(false);
  }
}
