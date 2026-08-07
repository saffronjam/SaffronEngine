/// The botanical operator vocabulary as the editor's node-graph surface needs it: typed pins, a
/// palette label, and the operator payload a freshly added node starts from. It mirrors the engine's
/// closed operator set the way `materials/graph.ts` mirrors the material one — the engine still
/// validates every document, and a mistyped edge or an out-of-range parameter comes back as a
/// rejection rather than a silently different plant.
import type { BotanicalOperatorDto } from "../../protocol";

export type BotanicalOperatorKind = BotanicalOperatorDto["kind"];

/// One Q15.16 metre and one full `UnitInterval`, the units botanical parameters cross the wire in.
export const Q16 = 65536;
export const UNIT = 65535;

/// The ceilings the engine's operator validation enforces. A field editor clamps to them so a
/// document is refused for a reason the artist can see rather than by the engine.
export const MAX_SEGMENTS = 64;
export const MAX_NODES = 64;
export const MAX_WHORL = 16;
export const MAX_SIDES = 32;
export const MAX_VARIATIONS = 16;

export interface BotanicalOperatorSpec {
  label: string;
  category: string;
  inputs: string[];
  outputs: string[];
}

/// Pins in the engine's canonical order, per operator.
export const BOTANICAL_OPERATORS: Record<BotanicalOperatorKind, BotanicalOperatorSpec> = {
  drawn: { label: "Drawn spine", category: "Axes", inputs: [], outputs: ["axes"] },
  trunk: { label: "Trunk", category: "Axes", inputs: [], outputs: ["axes"] },
  branch: { label: "Branch", category: "Axes", inputs: ["frames"], outputs: ["axes"] },
  roots: { label: "Roots", category: "Axes", inputs: ["axes"], outputs: ["axes"] },
  tropism: { label: "Tropism", category: "Shaping", inputs: ["axes"], outputs: ["axes"] },
  prune: { label: "Prune", category: "Shaping", inputs: ["axes"], outputs: ["axes"] },
  phyllotaxis: {
    label: "Phyllotaxis",
    category: "Shaping",
    inputs: ["axes"],
    outputs: ["frames"],
  },
  shell: { label: "Shell", category: "Surface", inputs: ["axes"], outputs: ["shells"] },
  instance: { label: "Instance", category: "Surface", inputs: ["frames"], outputs: ["elements"] },
  "module-call": {
    label: "Module call",
    category: "Surface",
    inputs: ["frames"],
    outputs: ["shells", "elements"],
  },
  family: { label: "Family", category: "Output", inputs: ["shells", "elements"], outputs: [] },
};

export const BOTANICAL_CATEGORIES = ["Axes", "Shaping", "Surface", "Output"];

/// The operators the add palette offers. Two are not on it: `family` is the single sink a document
/// already carries, and a second one has no defined result; `module-call` names a binding, so it is
/// added by choosing the module it grows rather than as a bare node.
export const BOTANICAL_PALETTE = (
  Object.keys(BOTANICAL_OPERATORS) as BotanicalOperatorKind[]
).filter((kind) => kind !== "family" && kind !== "module-call");

/// The payload a freshly added node of `kind` starts from — every value inside the engine's bounds,
/// so a new node grows something rather than failing validation on arrival.
export function defaultOperator(kind: BotanicalOperatorKind): BotanicalOperatorDto {
  switch (kind) {
    case "drawn":
      return {
        kind: "drawn",
        element: "branch",
        points: [
          { positionBits: [0, 0, 0], radiusBits: Math.round(0.08 * Q16) },
          { positionBits: [0, Q16, 0], radiusBits: Math.round(0.04 * Q16) },
        ],
      };
    case "trunk":
      return {
        kind: "trunk",
        element: "trunk",
        lengthBits: 4 * Q16,
        baseRadiusBits: Math.round(0.15 * Q16),
        taper: [
          { at: 0, factorBits: Q16 },
          { at: UNIT, factorBits: Math.round(0.1 * Q16) },
        ],
        segments: 6,
      };
    case "branch":
      return {
        kind: "branch",
        element: "branch",
        lengthRatio: 40_000,
        radiusRatio: 26_000,
        declination: 14_000,
        jitter: 8_000,
        segments: 4,
      };
    case "phyllotaxis":
      return {
        kind: "phyllotaxis",
        pattern: "spiral",
        count: 1,
        nodes: 5,
        start: 26_000,
        end: 62_000,
        divergence: 22_800,
      };
    case "tropism":
      return {
        kind: "tropism",
        kindOf: "gravitropism",
        strength: 20_000,
        stimulusBits: [0, Q16, 0],
        planeOffsetBits: 0,
      };
    case "prune":
      return { kind: "prune", rule: "below-height", thresholdBits: Q16, count: 1 };
    case "roots":
      return { kind: "roots", depthRatio: 18_000, spreadRatio: 40_000, count: 3 };
    case "shell":
      return { kind: "shell", materialSlot: 0, sides: 6 };
    case "instance":
      return {
        kind: "instance",
        element: "leaf",
        materialSlot: 1,
        sizeBits: Math.round(0.1 * Q16),
        jitter: 24_000,
      };
    case "module-call":
      return { kind: "module-call", callGuid: "" };
    case "family":
      return { kind: "family" };
  }
}
