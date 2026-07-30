import { useEditorStore } from "../state/store";
import type { RenderStats } from "../protocol";

/// Fold a patch into the live render-stats snapshot so a write shows before the next stats poll.
/// A no-op until the first snapshot has landed.
export function applyOptimisticRenderStats(patch: Partial<RenderStats>): void {
  const store = useEditorStore.getState();
  const current = store.renderStats;
  if (current) {
    store.setRenderStats({ ...current, ...patch });
  }
}

/// Record one render/post-process settings edit. These settings persist with the project, so their
/// edits belong to the scene tab's history.
export function recordRenderEdit(
  label: string,
  undo: () => Promise<unknown>,
  redo: () => Promise<unknown>,
): void {
  useEditorStore.getState().pushEdit({ label, undo, redo }, "scene");
}
