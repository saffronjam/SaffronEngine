/// Vegetation mode keyboard shortcuts: digit keys select the viewport tool, the
/// bracket keys grow/shrink the brush, and Enter/Escape close or drop the Spline
/// tool's in-progress control polyline. The scope is the open vegetation dock panel —
/// with the panel closed the digits pass through untouched, so the mode never steals
/// keys from ordinary editing. Bindings resolve through lib/keybindings, so rebinds
/// in settings take effect immediately.
import { useEffect } from "react";
import { client } from "../control/client";
import { errorText, notifyError } from "../lib/flash";
import { matchesBinding } from "../lib/keybindings";
import { findPanelLeaf } from "../state/dockLayout";
import { useEditorStore } from "../state/store";
import { VEGETATION_TOOLS, isBrushTool } from "../panels/vegetationTools";
import { splineCommit } from "../panels/ViewportPanel/viewportVegetation";
import { applyVegetationGesture, pushVegetationGesture } from "../panels/vegetationGesture";
import { mutationRecord } from "../panels/vegetationPlanting";
import type { VegetationMutationRecordDto } from "../protocol";
import { isTextEntryFocused } from "./useGizmoShortcuts";

/// Tombstones every selected plant as ONE undoable edit. Undo restores each plant's exact
/// persistent delta through the reducer — never by rewriting cooked bytes.
async function deleteSelectedPlants(plants: readonly string[]): Promise<void> {
  const store = useEditorStore.getState();
  const tombstones: VegetationMutationRecordDto[] = [];
  for (const plant of plants) {
    const resident = (await client.vegetationRuntimeInspect(plant)).resident;
    if (!resident) {
      continue;
    }
    tombstones.push(mutationRecord(resident.cell, { kind: "tombstone", plant }));
  }
  if (tombstones.length === 0) {
    store.setVegetationSelectedPlants([]);
    return;
  }
  const inverse = await applyVegetationGesture(tombstones);
  store.setVegetationSelectedPlants([]);
  pushVegetationGesture(
    tombstones.length === 1 ? "Delete plant" : `Delete ${tombstones.length} plants`,
    inverse,
  );
}

export function useVegetationShortcuts(): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (isTextEntryFocused()) {
        return;
      }
      const store = useEditorStore.getState();
      if (store.settingsOpen) {
        return;
      }
      // The scope: the vegetation panel is open in the scene dock.
      if (findPanelLeaf(store.dockLayouts.scene, "vegetation") === null) {
        return;
      }
      const overrides = store.keyBindings;
      for (const def of VEGETATION_TOOLS) {
        if (matchesBinding(event, def.command, overrides)) {
          event.preventDefault();
          store.setVegetationTool(def.tool);
          return;
        }
      }
      // Everything below authors, so it stands down while the world plays.
      if (store.playState !== "edit") {
        return;
      }
      if (matchesBinding(event, "vegetation.shapeCommit", overrides)) {
        event.preventDefault();
        void splineCommit();
        return;
      }
      if (matchesBinding(event, "vegetation.shapeCancel", overrides)) {
        event.preventDefault();
        store.clearVegetationShapePoints();
        return;
      }
      if (
        store.vegetationSelectedPlants.size > 0 &&
        matchesBinding(event, "vegetation.delete", overrides)
      ) {
        event.preventDefault();
        void deleteSelectedPlants([...store.vegetationSelectedPlants]).catch((err: unknown) =>
          notifyError(errorText(err)),
        );
        return;
      }
      if (isBrushTool(store.vegetationTool)) {
        if (matchesBinding(event, "vegetation.brushGrow", overrides)) {
          event.preventDefault();
          store.setVegetationBrush({
            radius: Math.min(64, store.vegetationBrush.radius * 1.25),
          });
          return;
        }
        if (matchesBinding(event, "vegetation.brushShrink", overrides)) {
          event.preventDefault();
          store.setVegetationBrush({
            radius: Math.max(0.5, store.vegetationBrush.radius / 1.25),
          });
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, []);
}
