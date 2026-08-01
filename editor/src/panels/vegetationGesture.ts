/// One vegetation gesture: the engine applies the records and replies with the records that undo
/// them exactly, read from the preimage the batch replaced. Applying an inverse returns the
/// inverse of *that*, so undo and redo are the same call over one toggling list and no preimage
/// is ever reconstructed here.
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import type { VegetationMutationRecordDto } from "../protocol";
import { freshGuid } from "./vegetationPlanting";

/// Applies one gesture, returning the records that reverse it.
export async function applyVegetationGesture(
  records: VegetationMutationRecordDto[],
): Promise<VegetationMutationRecordDto[]> {
  return (await client.vegetationMutate(freshGuid(), records)).inverse;
}

/// Records one applied gesture on the undo stack, replaying through the engine-returned inverse.
export function pushVegetationGesture(label: string, inverse: VegetationMutationRecordDto[]): void {
  let step = inverse;
  const replay = async (): Promise<void> => {
    step = await applyVegetationGesture(step);
  };
  useEditorStore.getState().pushEdit({ label, undo: replay, redo: replay });
}
