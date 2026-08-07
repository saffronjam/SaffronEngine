import { client } from "../../../control/client";
import { persistExpanded } from "../persistence";
import type { GetEditorState, SceneSlice, SetEditorState } from "../types";

export function createSceneSlice(set: SetEditorState, get: GetEditorState): SceneSlice {
  return {
    entities: [],
    selectedId: null,
    selectedMaterialId: null,
    expandedIds: new Set<string>(),
    sceneVersion: -1,
    selectionVersion: -1,
    componentsBySelected: null,
    focusComponent: null,
    environment: null,
    animationState: null,
    animationClips: [],
    gizmo: { op: "translate", space: "world", preserveChildren: false },
    playState: "edit",
    dragActive: false,
    // The scene view is the active view from the first frame, so authored entities load immediately;
    // App.tsx holds it false while an asset tab is active and during a view switch.
    sceneEntitiesLive: true,

    setEntities: (entities) =>
      set((s) => {
        // Survivors keep their expand state, so a poll refresh never collapses the tree.
        const present = new Set(entities.map((e) => e.id));
        let expandedIds = s.expandedIds;
        if ([...expandedIds].some((id) => !present.has(id))) {
          expandedIds = new Set([...expandedIds].filter((id) => present.has(id)));
          persistExpanded(s.project?.path, expandedIds);
        }
        return { entities, expandedIds };
      }),
    applyOptimisticEntityName: (id, name) =>
      set((s) => ({
        entities: s.entities.map((e) => (e.id === id ? { ...e, name } : e)),
      })),
    setSelectedId: (selectedId) => set({ selectedId }),
    setSelectedMaterialId: (selectedMaterialId) => set({ selectedMaterialId }),
    // Optimistic: the row highlights without waiting a poll interval; the reconcile poll confirms via
    // selectionVersion, and the engine wins if a newer version arrives.
    selectEntity: (id) => set({ selectedId: id }),
    toggleExpanded: (id) =>
      set((s) => {
        const expandedIds = new Set(s.expandedIds);
        if (!expandedIds.delete(id)) {
          expandedIds.add(id);
        }
        persistExpanded(s.project?.path, expandedIds);
        return { expandedIds };
      }),
    setExpanded: (id, expanded) =>
      set((s) => {
        if (s.expandedIds.has(id) === expanded) {
          return {};
        }
        const expandedIds = new Set(s.expandedIds);
        if (expanded) {
          expandedIds.add(id);
        } else {
          expandedIds.delete(id);
        }
        persistExpanded(s.project?.path, expandedIds);
        return { expandedIds };
      }),
    setParent: async (id, parentId) => {
      const previous = get().entities.find((e) => e.id === id)?.parentId;
      get().setDragActive(true);
      set((s) => ({
        entities: s.entities.map((e) =>
          e.id === id ? { ...e, parentId: parentId ?? undefined } : e,
        ),
      }));
      try {
        await client.setParent(id, parentId);
        get().pushEdit(
          {
            label: "Reparent",
            selectionId: id,
            undo: () => client.setParent(id, previous ?? null),
            redo: () => client.setParent(id, parentId),
          },
          "scene",
        );
      } catch (err) {
        // A rejected reparent never bumps sceneVersion, so the poll will not restore the row.
        set((s) => ({
          entities: s.entities.map((e) => (e.id === id ? { ...e, parentId: previous } : e)),
        }));
        throw err;
      } finally {
        get().setDragActive(false);
      }
    },
    setSceneVersion: (sceneVersion) => set({ sceneVersion }),
    setSelectionVersion: (selectionVersion) => set({ selectionVersion }),
    setComponentsBySelected: (componentsBySelected) => set({ componentsBySelected }),
    applyOptimisticComponent: (component, dto) =>
      set((s) => {
        if (!s.componentsBySelected) {
          return {};
        }
        return {
          componentsBySelected: {
            ...s.componentsBySelected,
            components: {
              ...s.componentsBySelected.components,
              [component]: dto,
            },
          },
        };
      }),
    setFocusComponent: (focusComponent) => set({ focusComponent }),
    setEnvironment: (environment) => set({ environment }),
    setAnimationState: (animationState, animationClips) => set({ animationState, animationClips }),
    // Identity-stable so the poll confirming an unchanged gizmo re-renders no subscriber.
    setGizmo: (patch) =>
      set((s) => {
        const gizmo = { ...s.gizmo, ...patch };
        return gizmo.op === s.gizmo.op &&
          gizmo.space === s.gizmo.space &&
          gizmo.preserveChildren === s.gizmo.preserveChildren
          ? {}
          : { gizmo };
      }),
    setPlayState: (playState) => set({ playState }),
    setDragActive: (dragActive) => set({ dragActive }),
    setSceneEntitiesLive: (sceneEntitiesLive) => set({ sceneEntitiesLive }),
  };
}
