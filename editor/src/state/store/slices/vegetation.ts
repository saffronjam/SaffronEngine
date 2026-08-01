import type { SetEditorState, VegetationSlice } from "../types";

export function createVegetationSlice(set: SetEditorState): VegetationSlice {
  return {
    vegetationTool: "select",
    vegetationBrush: {
      radius: 4,
      falloff: 0.5,
      spacing: 1,
      projection: "view",
      maxSlopeDeg: 90,
      density: 1,
    },
    vegetationSpecies: new Set<string>(),
    vegetationWeights: {},
    vegetationSelectedPlants: new Set<string>(),
    vegetationActiveLayer: null,
    vegetationCookJob: null,
    vegetationLastStroke: null,
    vegetationShapePoints: [],

    setVegetationTool: (tool) =>
      set((s) => (s.vegetationTool === tool ? {} : { vegetationTool: tool })),
    toggleVegetationSpecies: (id, additive) =>
      set((s) => {
        const next = new Set(additive ? s.vegetationSpecies : []);
        if (s.vegetationSpecies.has(id) && (additive || s.vegetationSpecies.size === 1)) {
          next.delete(id);
        } else {
          next.add(id);
        }
        return next.size === s.vegetationSpecies.size &&
          [...next].every((entry) => s.vegetationSpecies.has(entry))
          ? {}
          : { vegetationSpecies: next };
      }),
    setVegetationSelectedPlants: (plants) =>
      set((s) => {
        const next = new Set(plants);
        return next.size === s.vegetationSelectedPlants.size &&
          [...next].every((plant) => s.vegetationSelectedPlants.has(plant))
          ? {}
          : { vegetationSelectedPlants: next };
      }),
    setVegetationActiveLayer: (target) =>
      set((s) =>
        s.vegetationActiveLayer === target ||
        (s.vegetationActiveLayer !== null &&
          target !== null &&
          JSON.stringify(s.vegetationActiveLayer) === JSON.stringify(target))
          ? {}
          : { vegetationActiveLayer: target },
      ),
    setVegetationCookJob: (job) =>
      set((s) => (s.vegetationCookJob === job ? {} : { vegetationCookJob: job })),
    setVegetationLastStroke: (bounds) => set({ vegetationLastStroke: bounds }),
    addVegetationShapePoint: (point) =>
      set((s) => ({ vegetationShapePoints: [...s.vegetationShapePoints, point] })),
    clearVegetationShapePoints: () =>
      set((s) => (s.vegetationShapePoints.length === 0 ? {} : { vegetationShapePoints: [] })),
    setVegetationWeight: (id, weight) =>
      set((s) =>
        s.vegetationWeights[id] === weight
          ? {}
          : { vegetationWeights: { ...s.vegetationWeights, [id]: weight } },
      ),
    setVegetationBrush: (patch) =>
      set((s) => {
        const vegetationBrush = { ...s.vegetationBrush, ...patch };
        return vegetationBrush.radius === s.vegetationBrush.radius &&
          vegetationBrush.falloff === s.vegetationBrush.falloff &&
          vegetationBrush.spacing === s.vegetationBrush.spacing &&
          vegetationBrush.projection === s.vegetationBrush.projection &&
          vegetationBrush.maxSlopeDeg === s.vegetationBrush.maxSlopeDeg &&
          vegetationBrush.density === s.vegetationBrush.density
          ? {}
          : { vegetationBrush };
      }),
  };
}
