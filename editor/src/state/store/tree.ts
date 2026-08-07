import type { EntityListEntry } from "../../protocol";
import type { TreeNode } from "./types";

/// Group the flat entity list into a forest by parentId. An absent/`"0"` parentId, an unknown
/// parent, or a self-reference all land the entry at the root, so a malformed list can never build
/// a cycle. Sibling order preserves the engine's array order.
export function buildTree(entities: EntityListEntry[]): TreeNode[] {
  const nodes = new Map<string, TreeNode>();
  for (const entity of entities) {
    nodes.set(entity.id, { entity, children: [] });
  }
  const roots: TreeNode[] = [];
  for (const entity of entities) {
    const node = nodes.get(entity.id)!;
    const parentId = entity.parentId;
    const parent =
      parentId && parentId !== "0" && parentId !== entity.id ? nodes.get(parentId) : undefined;
    if (parent) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }
  return roots;
}

/// The outliner's bone filter: drop rows flagged `bone` and re-anchor every surviving entity whose
/// ancestry passes through bones to its nearest visible ancestor. The walk is bounded so corrupt
/// data cannot loop. Pure — used before `buildTree`.
export function reanchorPastBones(entities: EntityListEntry[]): EntityListEntry[] {
  const byId = new Map(entities.map((e) => [e.id, e]));
  return entities
    .filter((e) => !e.bone)
    .map((e) => {
      let parent = e.parentId ? byId.get(e.parentId) : undefined;
      for (let steps = 0; parent?.bone && steps <= entities.length; steps++) {
        parent = parent.parentId ? byId.get(parent.parentId) : undefined;
      }
      const parentId = parent?.id;
      return parentId === e.parentId ? e : { ...e, parentId };
    });
}
