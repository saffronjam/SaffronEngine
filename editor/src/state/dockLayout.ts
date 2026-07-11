/// The pure, DOM-free dock-layout tree shared by every dockspace island. A `DockBranch`
/// is an n-ary split (orientation alternates per depth — the VS Code gridview shape) with
/// per-child percent sizes; a `DockLeaf` is a tab group. Every function here is a pure
/// transform `DockLayout -> DockLayout` (or a query), so the store, the persistence layer,
/// and the unit tests all share one implementation with no React or store coupling.
///
/// The two `DockPanelId` spaces are DISJOINT by construction: a Scene panel id can never
/// index into the asset-editor tree and vice-versa. That disjointness IS the structural
/// no-cross-main-tab guarantee — there is no runtime cross-kind check anywhere.

export type DockSpaceKind = "scene" | "assetEditor";

/// The Scene island's panels — the leaf, tool, and structural (hierarchy/assets/viewport)
/// panels that can live in the Scene dock tree.
export const SCENE_PANEL_IDS = [
  "inspector",
  "environment",
  "render",
  "postProcess",
  "stats",
  "profiler",
  "physics",
  "scriptLogs",
  "material",
  "timeline",
  "hierarchy",
  "assets",
  "viewport",
] as const;

/// The asset-editor island's panels (disjoint from the Scene set).
export const ASSET_EDITOR_PANEL_IDS = [
  "skeleton",
  "preview",
  "clips",
  "assetTimeline",
  "materialEdit",
  "assetStats",
] as const;

export type SceneDockPanelId = (typeof SCENE_PANEL_IDS)[number];
export type AssetEditorDockPanelId = (typeof ASSET_EDITOR_PANEL_IDS)[number];
export type DockPanelId = SceneDockPanelId | AssetEditorDockPanelId;

export type DockNodeId = string;

export interface DockLeaf {
  type: "leaf";
  id: DockNodeId;
  tabs: DockPanelId[];
  activeTab: DockPanelId | null;
  /// Viewport/preview: no drops, the tab is not draggable, the strip is hidden.
  locked?: boolean;
  /// `normalize` never deletes this leaf, even when empty (the interim well-known leaves).
  persistent?: boolean;
}

export interface DockBranch {
  type: "branch";
  id: DockNodeId;
  orientation: "horizontal" | "vertical";
  children: DockNodeId[];
  /// Percent of the branch extent per child id (the rrp v4 `Layout` shape).
  sizes: Record<DockNodeId, number>;
}

export type DockNode = DockLeaf | DockBranch;

export interface DockLayout {
  version: 1;
  rootId: DockNodeId;
  nodes: Record<DockNodeId, DockNode>;
}

export type DockEdge = "left" | "right" | "top" | "bottom";

export type DropTarget =
  | { kind: "tab"; leafId: DockNodeId; index: number }
  | { kind: "split"; leafId: DockNodeId; edge: DockEdge };

const SCENE_PANEL_SET: ReadonlySet<string> = new Set(SCENE_PANEL_IDS);

/// Which island a panel id belongs to. Total over `DockPanelId` because the two unions
/// partition the id space.
export function panelKind(id: DockPanelId): DockSpaceKind {
  return SCENE_PANEL_SET.has(id) ? "scene" : "assetEditor";
}

export function isLeaf(node: DockNode | undefined): node is DockLeaf {
  return node?.type === "leaf";
}

export function isBranch(node: DockNode | undefined): node is DockBranch {
  return node?.type === "branch";
}

function leafAt(layout: DockLayout, id: DockNodeId): DockLeaf | null {
  const node = layout.nodes[id];
  return isLeaf(node) ? node : null;
}

export function leafTabs(layout: DockLayout, leafId: DockNodeId): DockPanelId[] {
  return leafAt(layout, leafId)?.tabs ?? [];
}

/// The leaf id currently holding `panelId`, or null when the panel is closed.
export function findPanelLeaf(layout: DockLayout, panelId: DockPanelId): DockNodeId | null {
  for (const node of Object.values(layout.nodes)) {
    if (isLeaf(node) && node.tabs.includes(panelId)) {
      return node.id;
    }
  }
  return null;
}

export function isPanelOpenIn(layout: DockLayout, panelId: DockPanelId): boolean {
  return findPanelLeaf(layout, panelId) !== null;
}

/// Every panel id present in any leaf (open, active or not).
export function allOpenPanels(layout: DockLayout): DockPanelId[] {
  const out: DockPanelId[] = [];
  for (const node of Object.values(layout.nodes)) {
    if (isLeaf(node)) {
      out.push(...node.tabs);
    }
  }
  return out;
}

/// Every panel that is the active tab of its leaf (the visible-in-model set).
export function visiblePanels(layout: DockLayout): DockPanelId[] {
  const out: DockPanelId[] = [];
  for (const node of Object.values(layout.nodes)) {
    if (isLeaf(node) && node.activeTab !== null) {
      out.push(node.activeTab);
    }
  }
  return out;
}

/// Whether a node renders: a locked or non-empty leaf, or a branch with any rendered child.
/// An empty non-locked leaf collapses (the region reclaims its space) while staying in the
/// model (persistent leaves) so its drop target never dangles.
export function isNodeRendered(layout: DockLayout, nodeId: DockNodeId): boolean {
  const node = layout.nodes[nodeId];
  if (isLeaf(node)) {
    return node.locked === true || node.tabs.length > 0;
  }
  if (isBranch(node)) {
    return node.children.some((childId) => isNodeRendered(layout, childId));
  }
  return false;
}

export function renderedChildIds(layout: DockLayout, branch: DockBranch): DockNodeId[] {
  return branch.children.filter((childId) => isNodeRendered(layout, childId));
}

/// Per-panel px minimums, by axis — the cannot-collapse-while-attaching guarantee. The
/// viewport's 520 px width is the load-bearing one; other panels share a sane default.
const DEFAULT_MIN_PX = { width: 200, height: 80 };
const LEAF_MIN_PX: Partial<Record<DockPanelId, { width: number; height: number }>> = {
  viewport: { width: 520, height: 200 },
  preview: { width: 320, height: 200 },
};

function leafMinPx(leaf: DockLeaf, axis: "width" | "height"): number {
  let min = DEFAULT_MIN_PX[axis];
  for (const tab of leaf.tabs) {
    min = Math.max(min, (LEAF_MIN_PX[tab] ?? DEFAULT_MIN_PX)[axis]);
  }
  return min;
}

/// The minimum px extent of a subtree along an axis: a branch along its own orientation sums
/// its rendered children's minima, otherwise takes the max. Set as the rrp `minSize` so a
/// nested viewport's 520 px width propagates up to its column.
export function subtreeMinPx(
  layout: DockLayout,
  nodeId: DockNodeId,
  axis: "width" | "height",
): number {
  const node = layout.nodes[nodeId];
  if (isLeaf(node)) {
    return leafMinPx(node, axis);
  }
  if (isBranch(node)) {
    const mins = renderedChildIds(layout, node).map((childId) =>
      subtreeMinPx(layout, childId, axis),
    );
    if (mins.length === 0) {
      return DEFAULT_MIN_PX[axis];
    }
    const alongAxis = (node.orientation === "horizontal") === (axis === "width");
    return alongAxis ? mins.reduce((sum, m) => sum + m, 0) : Math.max(...mins);
  }
  return DEFAULT_MIN_PX[axis];
}

export function hasNode(layout: DockLayout, id: DockNodeId): boolean {
  return layout.nodes[id] !== undefined;
}

/// Make `panelId` the active tab of whichever leaf holds it (no-op when closed).
export function setLeafActiveTab(layout: DockLayout, panelId: DockPanelId): DockLayout {
  const leafId = findPanelLeaf(layout, panelId);
  if (leafId === null) {
    return layout;
  }
  const leaf = leafAt(layout, leafId);
  return leaf ? setNode(layout, { ...leaf, activeTab: panelId }) : layout;
}

/// Replace a branch's child→percent sizes (the rrp `onLayoutChanged` round-trip).
export function setBranchSizes(
  layout: DockLayout,
  branchId: DockNodeId,
  sizes: Record<DockNodeId, number>,
): DockLayout {
  const node = layout.nodes[branchId];
  if (!isBranch(node)) {
    return layout;
  }
  const next: Record<DockNodeId, number> = {};
  for (const id of node.children) {
    next[id] = sizes[id] ?? node.sizes[id] ?? 0;
  }
  return setNode(layout, { ...node, sizes: next });
}

function parentBranchOf(layout: DockLayout, childId: DockNodeId): DockBranch | null {
  for (const node of Object.values(layout.nodes)) {
    if (isBranch(node) && node.children.includes(childId)) {
      return node;
    }
  }
  return null;
}

/// A fresh node id that collides with nothing in the layout. Deterministic given the
/// layout so the pure functions stay testable and resume-safe.
function freshNodeId(layout: DockLayout, kind: "leaf" | "branch"): DockNodeId {
  let n = 1;
  while (layout.nodes[`${kind}:gen${n}`] !== undefined) {
    n += 1;
  }
  return `${kind}:gen${n}`;
}

function withNodes(layout: DockLayout, nodes: Record<DockNodeId, DockNode>): DockLayout {
  return { version: 1, rootId: layout.rootId, nodes };
}

function setNode(layout: DockLayout, node: DockNode): DockLayout {
  return withNodes(layout, { ...layout.nodes, [node.id]: node });
}

/// Even percent split summing to 100 over the given ids.
function evenSizes(ids: DockNodeId[]): Record<DockNodeId, number> {
  const each = ids.length > 0 ? 100 / ids.length : 0;
  return Object.fromEntries(ids.map((id) => [id, each]));
}

/// Rescale a child→percent map so it sums to 100 (preserving ratios; even split if zero).
function renormalize(
  sizes: Record<DockNodeId, number>,
  ids: DockNodeId[],
): Record<DockNodeId, number> {
  const total = ids.reduce((sum, id) => sum + (sizes[id] ?? 0), 0);
  if (total <= 0) {
    return evenSizes(ids);
  }
  return Object.fromEntries(ids.map((id) => [id, ((sizes[id] ?? 0) / total) * 100]));
}

/// Insert `panelId` into an existing leaf at `index` and make it the active tab.
export function insertPanel(
  layout: DockLayout,
  panelId: DockPanelId,
  leafId: DockNodeId,
  index: number,
): DockLayout {
  const leaf = leafAt(layout, leafId);
  if (!leaf) {
    return layout;
  }
  const tabs = leaf.tabs.filter((id) => id !== panelId);
  const clamped = Math.max(0, Math.min(index, tabs.length));
  tabs.splice(clamped, 0, panelId);
  return setNode(layout, { ...leaf, tabs, activeTab: panelId });
}

/// Remove `panelId` from whichever leaf holds it; the active tab falls back to the
/// index−1 neighbor. Leaves the (possibly empty) leaf in place — callers `normalize`.
export function removePanel(layout: DockLayout, panelId: DockPanelId): DockLayout {
  const leafId = findPanelLeaf(layout, panelId);
  if (leafId === null) {
    return layout;
  }
  const leaf = leafAt(layout, leafId);
  if (!leaf) {
    return layout;
  }
  const index = leaf.tabs.indexOf(panelId);
  const tabs = leaf.tabs.filter((id) => id !== panelId);
  const activeTab =
    leaf.activeTab === panelId ? (tabs[Math.max(0, index - 1)] ?? null) : leaf.activeTab;
  return setNode(layout, { ...leaf, tabs, activeTab });
}

/// Reorder `panelId` within its leaf. `index` is in the without-moving-tab space — the
/// exact semantics of `insertionIndexForPointer` in the drag hook.
export function reorderTab(
  layout: DockLayout,
  leafId: DockNodeId,
  panelId: DockPanelId,
  index: number,
): DockLayout {
  const leaf = leafAt(layout, leafId);
  if (!leaf || !leaf.tabs.includes(panelId)) {
    return layout;
  }
  const without = leaf.tabs.filter((id) => id !== panelId);
  const clamped = Math.max(0, Math.min(index, without.length));
  without.splice(clamped, 0, panelId);
  return setNode(layout, { ...leaf, tabs: without });
}

/// Split a leaf (or append into a branch) with a new leaf holding `panelId` on `edge`.
/// Accepts the root branch id with a trailing edge (the `openPanel` terminal fallback).
export function splitLeaf(
  layout: DockLayout,
  targetId: DockNodeId,
  panelId: DockPanelId,
  edge: DockEdge,
  ids?: { leafId?: DockNodeId; branchId?: DockNodeId },
): DockLayout {
  const target = layout.nodes[targetId];
  if (!target) {
    return layout;
  }
  const orientation = edge === "left" || edge === "right" ? "horizontal" : "vertical";
  const before = edge === "left" || edge === "top";
  const newLeafId = ids?.leafId ?? freshNodeId(layout, "leaf");
  const newLeaf: DockLeaf = { type: "leaf", id: newLeafId, tabs: [panelId], activeTab: panelId };

  // Appending into a same-orientation branch (the root-append terminal fallback).
  if (isBranch(target) && target.orientation === orientation) {
    const children = before ? [newLeafId, ...target.children] : [...target.children, newLeafId];
    const total = target.children.reduce((sum, id) => sum + (target.sizes[id] ?? 0), 0) || 100;
    const share = total / children.length;
    const scaled: Record<DockNodeId, number> = {};
    for (const id of target.children) {
      scaled[id] = (target.sizes[id] ?? 0) * (1 - 1 / children.length);
    }
    scaled[newLeafId] = share;
    let next = setNode(layout, { ...target, children, sizes: scaled });
    next = setNode(next, newLeaf);
    return next;
  }

  const parent = parentBranchOf(layout, targetId);

  // Insert as an adjacent sibling, splitting the target's slot in half.
  if (isLeaf(target) && parent && parent.orientation === orientation) {
    const at = parent.children.indexOf(targetId);
    const children = [...parent.children];
    children.splice(before ? at : at + 1, 0, newLeafId);
    const slot = parent.sizes[targetId] ?? 100 / parent.children.length;
    const sizes = { ...parent.sizes, [targetId]: slot / 2, [newLeafId]: slot / 2 };
    let next = setNode(layout, { ...parent, children, sizes });
    next = setNode(next, newLeaf);
    return next;
  }

  // Wrap the target node in a fresh branch beside the new leaf.
  const newBranchId = ids?.branchId ?? freshNodeId(layout, "branch");
  const branchChildren = before ? [newLeafId, targetId] : [targetId, newLeafId];
  const newBranch: DockBranch = {
    type: "branch",
    id: newBranchId,
    orientation,
    children: branchChildren,
    sizes: evenSizes(branchChildren),
  };
  let next = setNode(layout, newBranch);
  next = setNode(next, newLeaf);
  if (parent) {
    const children = parent.children.map((id) => (id === targetId ? newBranchId : id));
    const sizes = { ...parent.sizes };
    sizes[newBranchId] = sizes[targetId] ?? 100 / parent.children.length;
    delete sizes[targetId];
    next = setNode(next, { ...parent, children, sizes });
  } else {
    next = { version: 1, rootId: newBranchId, nodes: next.nodes };
  }
  return next;
}

/// Move `panelId` to a drop target (tab-merge or split), then normalize. The panel
/// becomes the destination leaf's active tab.
export function movePanel(
  layout: DockLayout,
  panelId: DockPanelId,
  target: DropTarget,
): DockLayout {
  const removed = removePanel(layout, panelId);
  const placed =
    target.kind === "tab"
      ? insertPanel(removed, panelId, target.leafId, target.index)
      : splitLeaf(removed, target.leafId, panelId, target.edge);
  return normalize(placed);
}

function reachableIds(layout: DockLayout): Set<DockNodeId> {
  const seen = new Set<DockNodeId>();
  const stack = [layout.rootId];
  while (stack.length > 0) {
    const id = stack.pop();
    if (id === undefined || seen.has(id)) {
      continue;
    }
    const node = layout.nodes[id];
    if (!node) {
      continue;
    }
    seen.add(id);
    if (isBranch(node)) {
      stack.push(...node.children);
    }
  }
  return seen;
}

function pruneUnreachable(layout: DockLayout): DockLayout {
  const keep = reachableIds(layout);
  const nodes: Record<DockNodeId, DockNode> = {};
  for (const id of keep) {
    const node = layout.nodes[id];
    if (node) {
      nodes[id] = node;
    }
  }
  return withNodes(layout, nodes);
}

/// Clamp a leaf's activeTab to its tab membership.
function fixActiveTab(leaf: DockLeaf): DockLeaf {
  if (leaf.tabs.length === 0) {
    return leaf.activeTab === null ? leaf : { ...leaf, activeTab: null };
  }
  if (leaf.activeTab !== null && leaf.tabs.includes(leaf.activeTab)) {
    return leaf;
  }
  return { ...leaf, activeTab: leaf.tabs[0] };
}

/// One normalize pass; returns the new layout and whether it changed.
function normalizeOnce(layout: DockLayout): { layout: DockLayout; changed: boolean } {
  let nodes = { ...layout.nodes };
  let rootId = layout.rootId;
  let changed = false;

  // Clamp active tabs.
  for (const node of Object.values(nodes)) {
    if (isLeaf(node)) {
      const fixed = fixActiveTab(node);
      if (fixed !== node) {
        nodes[node.id] = fixed;
        changed = true;
      }
    }
  }

  // Delete empty, non-locked, non-persistent leaves from their parent branch.
  for (const node of Object.values(nodes)) {
    if (!isLeaf(node) || node.tabs.length > 0 || node.locked || node.persistent) {
      continue;
    }
    const parent = isBranch(nodes[node.id])
      ? null
      : Object.values(nodes).find(
          (candidate): candidate is DockBranch =>
            isBranch(candidate) && candidate.children.includes(node.id),
        );
    if (!parent) {
      continue; // an empty root leaf is left alone
    }
    const children = parent.children.filter((id) => id !== node.id);
    const sizes = { ...parent.sizes };
    delete sizes[node.id];
    nodes[parent.id] = { ...parent, children, sizes: renormalize(sizes, children) };
    delete nodes[node.id];
    changed = true;
  }

  // Collapse single-child branches and inline same-orientation nesting.
  for (const node of Object.values(nodes)) {
    if (!isBranch(node)) {
      continue;
    }
    const branch = nodes[node.id];
    if (!isBranch(branch)) {
      continue;
    }

    if (branch.children.length === 1) {
      const onlyChild = branch.children[0];
      const grandparent = Object.values(nodes).find(
        (candidate): candidate is DockBranch =>
          isBranch(candidate) && candidate.children.includes(branch.id),
      );
      if (grandparent) {
        const children = grandparent.children.map((id) => (id === branch.id ? onlyChild : id));
        const sizes = { ...grandparent.sizes };
        sizes[onlyChild] = sizes[branch.id] ?? 100 / grandparent.children.length;
        delete sizes[branch.id];
        nodes[grandparent.id] = { ...grandparent, children, sizes };
      } else {
        rootId = onlyChild;
      }
      delete nodes[branch.id];
      changed = true;
      continue;
    }

    // Merge a same-orientation child branch into this branch in place.
    const mergeChildId = branch.children.find((id) => {
      const child = nodes[id];
      return isBranch(child) && child.orientation === branch.orientation;
    });
    if (mergeChildId !== undefined) {
      const child = nodes[mergeChildId];
      if (isBranch(child)) {
        const at = branch.children.indexOf(mergeChildId);
        const slot = branch.sizes[mergeChildId] ?? 100 / branch.children.length;
        const inner = renormalize(child.sizes, child.children);
        const children = [
          ...branch.children.slice(0, at),
          ...child.children,
          ...branch.children.slice(at + 1),
        ];
        const sizes = { ...branch.sizes };
        delete sizes[mergeChildId];
        for (const id of child.children) {
          sizes[id] = ((inner[id] ?? 0) / 100) * slot;
        }
        nodes[branch.id] = { ...branch, children, sizes };
        delete nodes[mergeChildId];
        changed = true;
      }
    }
  }

  if (!changed) {
    return { layout, changed: false };
  }
  return { layout: pruneUnreachable({ version: 1, rootId, nodes }), changed: true };
}

/// Delete empty (non-locked, non-persistent) leaves, collapse single-child branches, and
/// merge same-orientation nesting until the tree is stable; prune orphaned nodes.
export function normalize(layout: DockLayout): DockLayout {
  let current = layout;
  for (let guard = 0; guard < 64; guard += 1) {
    const step = normalizeOnce(current);
    current = step.layout;
    if (!step.changed) {
      break;
    }
  }
  return current;
}

/// Structural + membership repair on load. Drops unknown panel ids, clamps active tabs,
/// and normalizes. Returns null when the tree is irreparably broken (caller falls back to
/// the kind's default factory).
export function validate(layout: DockLayout, knownIds: ReadonlySet<string>): DockLayout | null {
  if (layout.version !== 1 || typeof layout.rootId !== "string" || !layout.nodes[layout.rootId]) {
    return null;
  }
  const nodes: Record<DockNodeId, DockNode> = {};
  for (const [id, node] of Object.entries(layout.nodes)) {
    if (!node || node.id !== id) {
      return null;
    }
    if (isLeaf(node)) {
      const tabs = node.tabs.filter((tab) => knownIds.has(tab));
      nodes[id] = { ...node, tabs };
    } else if (isBranch(node)) {
      if (node.children.some((childId) => !layout.nodes[childId])) {
        return null;
      }
      nodes[id] = { ...node, children: [...node.children], sizes: { ...node.sizes } };
    } else {
      return null;
    }
  }
  const repaired = normalize(withNodes(layout, nodes));
  return repaired.nodes[repaired.rootId] ? repaired : null;
}

/// Drop `lastLocation` entries whose target leaf no longer exists in the layout.
export function pruneLastLocation(
  layout: DockLayout,
  lastLocation: Partial<Record<DockPanelId, DockNodeId>>,
): Partial<Record<DockPanelId, DockNodeId>> {
  const pruned: Partial<Record<DockPanelId, DockNodeId>> = {};
  for (const [panelId, leafId] of Object.entries(lastLocation)) {
    if (leafId !== undefined && isLeaf(layout.nodes[leafId])) {
      pruned[panelId as DockPanelId] = leafId;
    }
  }
  return pruned;
}

export interface OpenPanelHints {
  defaultLeafId: DockNodeId;
  lastLeafId?: DockNodeId;
}

/// The focus-or-open resolution: already open ⇒ activate in place; else last-location ⇒
/// default leaf ⇒ first non-locked leaf ⇒ terminal fallback (append a fresh leaf to the
/// root). Each step is validated against the live tree. Returns the new layout + the leaf
/// the panel landed in.
export function openPanelResolve(
  layout: DockLayout,
  panelId: DockPanelId,
  hints: OpenPanelHints,
): { layout: DockLayout; leafId: DockNodeId } {
  const existing = findPanelLeaf(layout, panelId);
  if (existing !== null) {
    const leaf = leafAt(layout, existing);
    return {
      layout: leaf ? setNode(layout, { ...leaf, activeTab: panelId }) : layout,
      leafId: existing,
    };
  }

  const usableLeaf = (id: DockNodeId | undefined): id is DockNodeId => {
    if (id === undefined) {
      return false;
    }
    const leaf = leafAt(layout, id);
    return leaf !== null && !leaf.locked;
  };

  const candidate =
    [hints.lastLeafId, hints.defaultLeafId].find(usableLeaf) ?? firstNonLockedLeaf(layout);
  if (candidate !== null) {
    const leaf = leafAt(layout, candidate);
    const index = leaf ? leaf.tabs.length : 0;
    return { layout: insertPanel(layout, panelId, candidate, index), leafId: candidate };
  }

  // Terminal fallback: append a fresh leaf as the root's trailing child.
  const newLeafId = freshNodeId(layout, "leaf");
  const root = layout.nodes[layout.rootId];
  const edge: DockEdge = isBranch(root) && root.orientation === "vertical" ? "bottom" : "right";
  const next = splitLeaf(layout, layout.rootId, panelId, edge, { leafId: newLeafId });
  return { layout: next, leafId: findPanelLeaf(next, panelId) ?? newLeafId };
}

function firstNonLockedLeaf(layout: DockLayout): DockNodeId | null {
  const stack = [layout.rootId];
  while (stack.length > 0) {
    const id = stack.shift();
    if (id === undefined) {
      continue;
    }
    const node = layout.nodes[id];
    if (isLeaf(node) && !node.locked) {
      return id;
    }
    if (isBranch(node)) {
      stack.unshift(...node.children);
    }
  }
  return null;
}

/// The full Scene tree: a horizontal root [left sidebar | center column | right dock] over
/// the center's vertical [viewport | assets | bottom dock]. The viewport leaf is `locked`
/// (live subsurface); the three well-known docks (leftBottom trio, right, bottom) are
/// `persistent` so they never vanish from the model — an empty non-locked leaf is simply not
/// rendered (DockRoot skips it), so the region collapses while its drop target still exists.
export function defaultSceneLayout(): DockLayout {
  return {
    version: 1,
    rootId: "branch:scene-root",
    nodes: {
      "branch:scene-root": {
        type: "branch",
        id: "branch:scene-root",
        orientation: "horizontal",
        children: ["branch:scene-left", "branch:scene-center", "leaf:right"],
        sizes: { "branch:scene-left": 18, "branch:scene-center": 57, "leaf:right": 25 },
      },
      "branch:scene-left": {
        type: "branch",
        id: "branch:scene-left",
        orientation: "vertical",
        children: ["leaf:hierarchy", "leaf:leftBottom"],
        sizes: { "leaf:hierarchy": 45, "leaf:leftBottom": 55 },
      },
      "branch:scene-center": {
        type: "branch",
        id: "branch:scene-center",
        orientation: "vertical",
        children: ["leaf:viewport", "leaf:assets", "leaf:bottom"],
        sizes: { "leaf:viewport": 64, "leaf:assets": 21, "leaf:bottom": 15 },
      },
      "leaf:hierarchy": {
        type: "leaf",
        id: "leaf:hierarchy",
        tabs: ["hierarchy"],
        activeTab: "hierarchy",
      },
      "leaf:leftBottom": {
        type: "leaf",
        id: "leaf:leftBottom",
        tabs: ["inspector", "environment", "render", "postProcess"],
        activeTab: "inspector",
        persistent: true,
      },
      "leaf:viewport": {
        type: "leaf",
        id: "leaf:viewport",
        tabs: ["viewport"],
        activeTab: "viewport",
        locked: true,
      },
      "leaf:assets": {
        type: "leaf",
        id: "leaf:assets",
        tabs: ["assets"],
        activeTab: "assets",
        persistent: true,
      },
      "leaf:bottom": {
        type: "leaf",
        id: "leaf:bottom",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:right": {
        type: "leaf",
        id: "leaf:right",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
    },
  };
}

/// The asset-editor tree: a horizontal root [aeLeft | content | aeRight], where `content` is a
/// vertical [skeleton | preview(locked) | clips horizontal row, over `assetTimeline`, over
/// `aeBottom`]. `aeLeft`/`aeRight`/`aeBottom` are persistent empty drop regions (mirroring the
/// Scene's docks) so a panel can dock to any edge even while the timeline occupies its own leaf;
/// each collapses (reclaims space) while empty. Presence of skeleton/clips/assetTimeline stays
/// capability-gated via `openPanel`/`normalize`, never a second render branch.
export function defaultAssetEditorLayout(): DockLayout {
  return {
    version: 1,
    rootId: "branch:ae-root",
    nodes: {
      "branch:ae-root": {
        type: "branch",
        id: "branch:ae-root",
        orientation: "horizontal",
        children: ["leaf:aeLeft", "branch:ae-content", "leaf:aeRight"],
        sizes: { "leaf:aeLeft": 16, "branch:ae-content": 68, "leaf:aeRight": 16 },
      },
      "branch:ae-content": {
        type: "branch",
        id: "branch:ae-content",
        orientation: "vertical",
        children: ["branch:ae-row", "leaf:assetTimeline", "leaf:aeBottom"],
        sizes: { "branch:ae-row": 72, "leaf:assetTimeline": 16, "leaf:aeBottom": 12 },
      },
      "branch:ae-row": {
        type: "branch",
        id: "branch:ae-row",
        orientation: "horizontal",
        children: ["leaf:skeleton", "leaf:preview", "leaf:clips"],
        sizes: { "leaf:skeleton": 18, "leaf:preview": 67, "leaf:clips": 15 },
      },
      // skeleton/clips/assetTimeline start empty + persistent: capability gating opens them
      // per the previewed model (rig → skeleton; clips → clips + assetTimeline), and DockRoot
      // skips an empty leaf so a static model shows just the preview. The leaves persist so
      // re-previewing a richer model reopens each panel at its canonical position.
      "leaf:skeleton": {
        type: "leaf",
        id: "leaf:skeleton",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:preview": {
        type: "leaf",
        id: "leaf:preview",
        tabs: ["preview"],
        activeTab: "preview",
        locked: true,
      },
      "leaf:clips": {
        type: "leaf",
        id: "leaf:clips",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:assetTimeline": {
        type: "leaf",
        id: "leaf:assetTimeline",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:aeRight": {
        type: "leaf",
        id: "leaf:aeRight",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:aeLeft": {
        type: "leaf",
        id: "leaf:aeLeft",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
      "leaf:aeBottom": {
        type: "leaf",
        id: "leaf:aeBottom",
        tabs: [],
        activeTab: null,
        persistent: true,
      },
    },
  };
}

export function defaultDockLayout(kind: DockSpaceKind): DockLayout {
  return kind === "scene" ? defaultSceneLayout() : defaultAssetEditorLayout();
}

export function defaultDockLayouts(): Record<DockSpaceKind, DockLayout> {
  return { scene: defaultSceneLayout(), assetEditor: defaultAssetEditorLayout() };
}

/// The `openPanel` fallback leaf per panel — where a panel opens when it has no last-location.
export const DEFAULT_LEAF: Record<DockPanelId, DockNodeId> = {
  inspector: "leaf:leftBottom",
  environment: "leaf:leftBottom",
  render: "leaf:leftBottom",
  postProcess: "leaf:leftBottom",
  stats: "leaf:right",
  profiler: "leaf:right",
  physics: "leaf:right",
  scriptLogs: "leaf:assets",
  material: "leaf:right",
  timeline: "leaf:assets",
  hierarchy: "leaf:hierarchy",
  assets: "leaf:assets",
  viewport: "leaf:viewport",
  skeleton: "leaf:skeleton",
  preview: "leaf:preview",
  clips: "leaf:clips",
  assetTimeline: "leaf:assetTimeline",
  materialEdit: "leaf:aeRight",
  assetStats: "leaf:aeLeft",
};

/// Reset a kind's tree to its default positions WITHOUT closing the panels currently open:
/// rebuild the default layout, then re-insert every open panel the default doesn't already carry
/// into its canonical `DEFAULT_LEAF`. Structural panels (already in the default) keep their spot;
/// the closable tools snap back to their home leaf. Pure — the caller passes the open panel ids.
export function resetLayoutPreservingOpen(kind: DockSpaceKind, open: DockPanelId[]): DockLayout {
  let layout = defaultDockLayout(kind);
  const present = new Set<DockPanelId>(allOpenPanels(layout));
  for (const id of open) {
    if (present.has(id)) {
      continue;
    }
    const leafId = DEFAULT_LEAF[id];
    layout =
      hasNode(layout, leafId) && isLeaf(layout.nodes[leafId])
        ? insertPanel(layout, id, leafId, leafTabs(layout, leafId).length)
        : openPanelResolve(layout, id, { defaultLeafId: leafId }).layout;
    present.add(id);
  }
  return normalize(layout);
}

const PANEL_IDS_BY_KIND: Record<DockSpaceKind, ReadonlySet<string>> = {
  scene: new Set(SCENE_PANEL_IDS),
  assetEditor: new Set(ASSET_EDITOR_PANEL_IDS),
};

export function knownPanelIds(kind: DockSpaceKind): ReadonlySet<string> {
  return PANEL_IDS_BY_KIND[kind];
}

/// The panels a kind's tree must always carry — the structural (non-closable) ones, plus the
/// locked subsurface leaf. A loaded layout missing any is incompatible; it is discarded for the
/// default factory, since there is no layout migration.
const REQUIRED_PANELS: Record<DockSpaceKind, DockPanelId[]> = {
  scene: ["inspector", "environment", "render", "postProcess", "hierarchy", "viewport"],
  assetEditor: ["preview"],
};

/// Whether every structural panel for the kind is present in the tree.
export function hasRequiredPanels(layout: DockLayout, kind: DockSpaceKind): boolean {
  return REQUIRED_PANELS[kind].every((id) => isPanelOpenIn(layout, id));
}
