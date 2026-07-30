import type { SetEditorState, VegetationSlice } from "../types";

export function createVegetationSlice(set: SetEditorState): VegetationSlice {
  return {
    vegetationTool: "select",
    vegetationBrush: { radius: 4, falloff: 0.5, spacing: 1, projection: "view", maxSlopeDeg: 90 },
    vegetationSpecies: new Set<string>(),
    vegetationWeights: {},
    vegetationSelectedPlant: null,
    vegetationActiveLayer: null,
    vegetationCookJob: null,
    vegetationLastStroke: null,

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
    setVegetationSelectedPlant: (plant) =>
      set((s) => (s.vegetationSelectedPlant === plant ? {} : { vegetationSelectedPlant: plant })),
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
          vegetationBrush.maxSlopeDeg === s.vegetationBrush.maxSlopeDeg
          ? {}
          : { vegetationBrush };
      }),
  };
}
