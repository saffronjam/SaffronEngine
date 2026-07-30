import { client } from "../../../control/client";
import { errorText, notifyError } from "../../../lib/flash";
import {
  appendEdit,
  emptyHistory,
  takeRedo,
  takeUndo,
  type TabHistory,
  type UndoableEdit,
} from "../../../lib/undo";
import type { EditorState, GetEditorState, HistorySlice, SetEditorState, ViewTab } from "../types";

/// The tab kinds whose edits feed an undo history. Viewer kinds default out.
const HISTORY_TAB_KINDS = new Set<ViewTab["kind"]>(["scene", "materialGraph", "assetEditor"]);

/// The inverse of creating `entityId`. Undo-only: re-creation would mint a new id, so redo is
/// dropped.
export function entityCreationEdit(entityId: string, label: string): UndoableEdit {
  return {
    label,
    redoable: false,
    selectionId: entityId,
    undo: () => client.destroyEntity(entityId),
    redo: () => Promise.resolve(),
  };
}

function isHistoryTab(viewTabs: ViewTab[], id: string): boolean {
  const tab = viewTabs.find((t) => t.id === id);
  return tab !== undefined && HISTORY_TAB_KINDS.has(tab.kind);
}

/// The scene history and every orphaned non-scene history dropped, keeping live non-scene tabs.
function sceneHistoryCleared(s: EditorState): Record<string, TabHistory> {
  const live = new Set(s.viewTabs.map((tab) => tab.id));
  const next: Record<string, TabHistory> = {};
  for (const [id, history] of Object.entries(s.historyByTab)) {
    if (id !== "scene" && live.has(id)) {
      next[id] = history;
    }
  }
  return next;
}

/// After a scene-tab replay, land selection back on the entity the edit touched so the gizmo
/// follows it. Only the scene tab carries entity selectionIds.
function restoreSelectionContext(get: GetEditorState, tabId: string, edit: UndoableEdit): void {
  if (tabId !== "scene" || edit.selectionId === undefined) {
    return;
  }
  get().setSelectedId(edit.selectionId);
  void client.selectEntity(edit.selectionId).catch(() => {});
}

/// Resolve and replay the next undo/redo on a tab's history. Holds `historyReplaying` over the
/// in-flight inverse so no edit site records the replay, and moves the entry between stacks
/// regardless of success — a half-applied replay is repaired by the reconcile poll.
async function replayHistory(
  set: SetEditorState,
  get: GetEditorState,
  direction: "undo" | "redo",
  tabId?: string,
): Promise<void> {
  const store = get();
  const id = tabId ?? store.activeViewTabId;
  // While playing, the scene tab targets the throwaway play duplicate, so replay is paused
  // until Stop and the history is left untouched.
  if (id === "scene" && store.playState !== "edit") {
    return;
  }
  const history = store.historyByTab[id];
  if (history === undefined) {
    return;
  }
  const taken = direction === "undo" ? takeUndo(history) : takeRedo(history);
  if (taken === null) {
    return;
  }
  set({ historyReplaying: true });
  try {
    await (direction === "undo" ? taken.edit.undo() : taken.edit.redo());
  } catch (err) {
    notifyError(errorText(err));
    console.error(`${direction} rejected:`, err);
  } finally {
    set((s) => ({
      historyByTab: { ...s.historyByTab, [id]: taken.next },
      historyReplaying: false,
    }));
    restoreSelectionContext(get, id, taken.edit);
  }
}

export function createHistorySlice(set: SetEditorState, get: GetEditorState): HistorySlice {
  return {
    historyByTab: {},
    historyReplaying: false,

    pushEdit: (edit, tabId) =>
      set((s) => {
        if (s.historyReplaying) {
          return {};
        }
        const id = tabId ?? s.activeViewTabId;
        if (!isHistoryTab(s.viewTabs, id)) {
          return {};
        }
        // The scene tab edits a throwaway duplicate during play; those edits are discarded on Stop,
        // so they never enter the authored-scene history.
        if (id === "scene" && s.playState !== "edit") {
          return {};
        }
        const history = s.historyByTab[id] ?? emptyHistory();
        return { historyByTab: { ...s.historyByTab, [id]: appendEdit(history, edit) } };
      }),
    undo: (tabId) => replayHistory(set, get, "undo", tabId),
    redo: (tabId) => replayHistory(set, get, "redo", tabId),
    beginEdit: ({ prior, selectionId }) => ({
      commit: (final, build) => {
        const edit = build(prior, final);
        if (selectionId !== undefined && edit.selectionId === undefined) {
          edit.selectionId = selectionId;
        }
        get().pushEdit(edit);
      },
    }),
    clearTabHistory: (tabId) =>
      set((s) => {
        if (!(tabId in s.historyByTab)) {
          return {};
        }
        const historyByTab = { ...s.historyByTab };
        delete historyByTab[tabId];
        return { historyByTab };
      }),
    clearSceneHistory: () => set((s) => ({ historyByTab: sceneHistoryCleared(s) })),
  };
}
