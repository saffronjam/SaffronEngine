import { client } from "../../../control/client";
import { COMMANDS_BY_ID, isCommandId } from "../../../lib/keybindings";
import {
  loadDevMode,
  loadHideBones,
  loadShowSubrows,
  persistDevMode,
  persistHideBones,
  persistShowSubrows,
} from "../persistence";
import type { SetEditorState, UiSlice } from "../types";

/// Fire-and-forget settings write; the in-memory state stays applied on rejection, since the global
/// shortcut hook has no panel to flash.
function persistKeyBindings(keyBindings: Record<string, string>): void {
  void client.saveEditorSettings({ keyBindings }).catch((err: unknown) => {
    console.error("save editor settings rejected:", err);
  });
}

export function createUiSlice(set: SetEditorState): UiSlice {
  return {
    viewportHidden: false,
    nativeDialogOpen: false,
    projectModalOpen: false,
    exportModalOpen: false,
    showComponentSubrows: loadShowSubrows(),
    hideBones: loadHideBones(),
    keyBindings: {},
    settingsOpen: false,
    devMode: loadDevMode(),

    setViewportHidden: (viewportHidden) => set({ viewportHidden }),
    setNativeDialogOpen: (nativeDialogOpen) => set({ nativeDialogOpen }),
    setProjectModalOpen: (projectModalOpen) => set({ projectModalOpen }),
    setExportModalOpen: (exportModalOpen) => set({ exportModalOpen }),
    toggleComponentSubrows: () =>
      set((s) => {
        const showComponentSubrows = !s.showComponentSubrows;
        persistShowSubrows(showComponentSubrows);
        return { showComponentSubrows };
      }),
    toggleHideBones: () =>
      set((s) => {
        const hideBones = !s.hideBones;
        persistHideBones(hideBones);
        return { hideBones };
      }),
    setKeyBinding: (id, value) =>
      set((s) => {
        const keyBindings = { ...s.keyBindings };
        if (value === COMMANDS_BY_ID[id].default) {
          delete keyBindings[id];
        } else {
          keyBindings[id] = value;
        }
        persistKeyBindings(keyBindings);
        return { keyBindings };
      }),
    resetKeyBinding: (id) =>
      set((s) => {
        if (!(id in s.keyBindings)) {
          return {};
        }
        const keyBindings = { ...s.keyBindings };
        delete keyBindings[id];
        persistKeyBindings(keyBindings);
        return { keyBindings };
      }),
    resetAllKeyBindings: () => {
      persistKeyBindings({});
      set({ keyBindings: {} });
    },
    hydrateKeyBindings: (overrides) =>
      set({
        keyBindings: Object.fromEntries(
          Object.entries(overrides).filter(([id]) => isCommandId(id)),
        ),
      }),
    setSettingsOpen: (settingsOpen) => set({ settingsOpen }),
    setDevMode: (devMode) => {
      persistDevMode(devMode);
      set({ devMode });
    },
  };
}
