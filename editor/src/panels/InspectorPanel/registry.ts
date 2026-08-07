import { COMPONENT_ORDER } from "../../lib/componentOrder";

/// Components that cannot be removed. Name/Transform are the entity baseline;
/// ModelInstance/SkinnedMesh/Morph are import-managed identity, so removing them would strand a rig
/// with no way back.
export const NON_REMOVABLE = new Set<string>([
  "Name",
  "Transform",
  "ModelInstance",
  "SkinnedMesh",
  "Morph",
]);

/// Components not offered in the Add Component menu: MaterialSet's slots come from a multi-material
/// import, and the rest are written by model/rig import, never created on a bare entity.
const NON_ADDABLE = new Set<string>([
  "Name",
  "MaterialSet",
  "ModelInstance",
  "SkinnedMesh",
  "Morph",
]);

/// The exposed PBR parameters a `MaterialSet` slot can override, with their engine defaults
/// (colours and vectors are arrays, matching the `.smat` + override wire shape). Overrides are
/// opt-in and sparse: only the keys a user added render as rows, and everything else inherits the
/// referenced material. `uvTiling`/`uvOffset` are omitted — no 2-vector widget, rarely tweaked per
/// object. This list is both the "+ Override" menu order and the overridden-row order.
export const MATERIAL_PARAMS: readonly { field: string; default: unknown }[] = [
  { field: "baseColor", default: [1, 1, 1, 1] },
  { field: "albedoTexture", default: "0" },
  { field: "metallic", default: 0 },
  { field: "roughness", default: 1 },
  { field: "ormTexture", default: "0" },
  { field: "normalTexture", default: "0" },
  { field: "normalStrength", default: 1 },
  { field: "emissive", default: [0, 0, 0] },
  { field: "emissiveStrength", default: 1 },
  { field: "emissiveTexture", default: "0" },
  { field: "heightTexture", default: "0" },
  { field: "heightScale", default: 0.05 },
  { field: "blend", default: "opaque" },
  { field: "alphaCutoff", default: 0.5 },
  { field: "unlit", default: false },
  { field: "doubleSided", default: false },
];

/// The vector axes a colour/vector field kind maps to, or `null` for a scalar kind — the override
/// editor converts the array wire form ↔ the `{x,y,…}` widget shape across it.
export function vectorAxes(kind: string): readonly string[] | null {
  if (kind === "color4" || kind === "vec4") {
    return ["x", "y", "z", "w"];
  }
  if (kind === "color3" || kind === "vec3") {
    return ["x", "y", "z"];
  }
  return null;
}

export const ADDABLE_COMPONENTS = COMPONENT_ORDER.filter((c) => !NON_ADDABLE.has(c));

/// Components only addable on a skinned entity: the rig sidecars index `SkinnedMesh.bones`, and the
/// animation player and foot IK have no meaning without a skeleton to drive.
export const RIG_ONLY = new Set<string>([
  "KinematicBones",
  "BonePhysics",
  "AnimationPlayer",
  "FootIk",
]);

export const SECTION_DRAG_THRESHOLD_PX = 4;

/// The in-flight component-reorder drag: the pointer/scroll offsets, the order snapshot, and the
/// section centers the insertion index is resolved against.
export interface ComponentDragState {
  id: string;
  startY: number;
  currentY: number;
  startScrollTop: number;
  currentScrollTop: number;
  dragging: boolean;
  startIndex: number;
  previewIndex: number;
  height: number;
  order: string[];
  centers: Record<string, number>;
}
