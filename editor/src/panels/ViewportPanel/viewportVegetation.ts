import { client } from "../../control/client";
import { errorText, notifyError } from "../../lib/flash";
import { useEditorStore } from "../../state/store";
import { anchorRecord, mutationRecord } from "../vegetationPlanting";
import { commitStroke, recookRegion, togglePin, type BrushStamp } from "../vegetationPainting";
import type { Uv } from "./viewportInput";

/// Anchor-plants the first selected species at the picked ground position as one
/// undoable edit (undo tombstones the minted identity; redo regrows it).
export async function plantAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const position = picked.position;
    if (!position) {
      notifyError("No ground under the cursor to plant on");
      return;
    }
    const store = useEditorStore.getState();
    const family = [...store.vegetationSpecies][0];
    if (!family) {
      return;
    }
    const { record, plant, cell } = anchorRecord(position, family);
    await client.vegetationMutate([record]);
    let regrowTick = 1n;
    store.pushEdit({
      label: "Plant anchor",
      undo: () => client.vegetationMutate([mutationRecord(cell, { kind: "tombstone", plant })]),
      redo: () => {
        regrowTick += 1n;
        return client.vegetationMutate([
          mutationRecord(cell, {
            kind: "regrow",
            plant,
            lifecycle: "mature",
            phenotype: 0,
            ecologyTick: regrowTick.toString(),
          }),
        ]);
      },
    });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Commits a captured brush stroke as one authored-chunk transaction (with a
/// cell-scoped recook), recorded as one undoable edit.
export async function strokeCommit(stamps: BrushStamp[]): Promise<void> {
  const store = useEditorStore.getState();
  const target = store.vegetationActiveLayer;
  if (!target) {
    return;
  }
  try {
    const pair = await commitStroke(target, stamps);
    if (!pair) {
      return;
    }
    store.pushEdit({
      label: stamps[0]?.sign === -1 ? "Erase vegetation" : "Paint vegetation",
      undo: pair.undo,
      redo: pair.redo,
    });
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Recooks the region a reapply stroke touched — deterministic refresh, no
/// authored mutation and no undo entry.
export async function reapplyCommit(stamps: BrushStamp[]): Promise<void> {
  const target = useEditorStore.getState().vegetationActiveLayer;
  if (!target || stamps.length === 0) {
    return;
  }
  try {
    await recookRegion(target, stamps);
  } catch (err) {
    notifyError(errorText(err));
  }
}

/// Toggles the clicked macro plant's pin row in the active layer as one undoable
/// chunk transaction (a pin survives graph recooks).
export async function pinAt({ u, v }: Uv): Promise<void> {
  try {
    const picked = await client.pick(u, v);
    const plant = picked.plant;
    if (!plant) {
      return;
    }
    const store = useEditorStore.getState();
    const target = store.vegetationActiveLayer;
    if (!target || target.locked) {
      notifyError("Select an unlocked layer to pin into");
      return;
    }
    const inspected = await client.vegetationRuntimeInspect(plant);
    const cell = inspected.resident?.cell;
    if (!cell) {
      return;
    }
    const pair = await togglePin(target, plant, cell);
    if (!pair) {
      return;
    }
    store.pushEdit({
      label: pair.pinned ? "Pin plant" : "Unpin plant",
      undo: pair.undo,
      redo: pair.redo,
    });
  } catch (err) {
    notifyError(errorText(err));
  }
}
