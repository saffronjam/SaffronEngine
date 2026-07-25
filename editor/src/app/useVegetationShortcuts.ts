/// Vegetation mode keyboard shortcuts: digit keys select the viewport tool and the
/// bracket keys grow/shrink the brush. The scope is the open vegetation dock panel —
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
import type { VegetationMutationDto, VegetationMutationRecordDto } from "../protocol";
import { isTextEntryFocused } from "./useGizmoShortcuts";

/// A fresh 32-hex vegetation GUID (transaction/idempotency keys).
function freshGuid(): string {
  return crypto.randomUUID().replaceAll("-", "");
}

/// The editor's stable mutation authority id.
const EDITOR_AUTHORITY = "0000000000000000000000000000e017";

/// Tombstones the selected plant as one undoable edit: the runtime row supplies the
/// Regrow preimage (lifecycle/phenotype/tick), so undo restores the same stable
/// identity through the reducer — never by rewriting cooked bytes.
async function deleteSelectedPlant(plant: string): Promise<void> {
  const store = useEditorStore.getState();
  const inspected = await client.vegetationRuntimeInspect(plant);
  const resident = inspected.resident;
  if (!resident) {
    store.setVegetationSelectedPlant(null);
    return;
  }
  const record = (mutation: VegetationMutationDto): VegetationMutationRecordDto => ({
    header: {
      cell: resident.cell,
      transaction: freshGuid(),
      authority: EDITOR_AUTHORITY,
      logicalTick: String(Date.now()),
      idempotencyKey: freshGuid(),
    },
    mutation,
  });
  const tombstone = () => client.vegetationMutate([record({ kind: "tombstone", plant })]);
  const regrow = () =>
    client.vegetationMutate([
      record({
        kind: "regrow",
        plant,
        lifecycle: resident.lifecycle,
        phenotype: resident.phenotype,
        ecologyTick: String(BigInt(resident.ecologyTick) + 1n),
      }),
    ]);
  await tombstone();
  store.setVegetationSelectedPlant(null);
  store.pushEdit({
    label: "Delete plant",
    undo: regrow,
    redo: tombstone,
  });
}

export function useVegetationShortcuts(): void {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (isTextEntryFocused()) {
        return;
      }
      const store = useEditorStore.getState();
      if (store.settingsOpen || store.playState !== "edit") {
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
      if (
        store.vegetationSelectedPlant !== null &&
        matchesBinding(event, "vegetation.delete", overrides)
      ) {
        event.preventDefault();
        void deleteSelectedPlant(store.vegetationSelectedPlant).catch((err: unknown) =>
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
