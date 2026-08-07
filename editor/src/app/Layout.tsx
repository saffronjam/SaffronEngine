/// The Scene dockspace: a recursive `DockRoot` over the Scene dock tree. Every region is a dock
/// leaf, so a tab can retab or split it and an empty side region collapses into the viewport. The
/// viewport leaf is the only host the engine paints over — `locked`, no strip, no drops — and reveal
/// bands stand in for the empty regions during a torn drag so a panel can be dropped back.
import { useEffect } from "react";
import { useEditorStore } from "../state/store";
import { DockRoot } from "@/components/dock/DockRoot";
import { RevealBands, type RevealBand } from "@/components/dock/RevealBands";
import { logRender } from "../lib/renderLog";

/// The empty Scene edge regions that accept a torn tab while collapsed. The right/bottom docks
/// are single persistent leaves, so their band tracks that leaf directly. The left column holds
/// two leaves (`hierarchy` over the persistent `leftBottom`); its band tracks *both* and docks
/// into `leftBottom`, so it appears only once the entire left column is empty — and stops as soon
/// as either leaf is repopulated, handing the drop back to the real (full-width) leaf.
const SCENE_REVEAL_BANDS: RevealBand[] = [
  { leafId: "leaf:leftBottom", regionLeaves: ["leaf:hierarchy", "leaf:leftBottom"], edge: "left" },
  { leafId: "leaf:right", edge: "right" },
  { leafId: "leaf:bottom", edge: "bottom" },
];

export function Layout() {
  logRender("Layout");
  const playState = useEditorStore((s) => s.playState);

  // Load the per-project dock trees on mount; Layout remounts per project via its `key`, so
  // this hydrates once per project and no-ops without a loaded project.
  useEffect(() => {
    useEditorStore.getState().hydrateDockLayouts();
  }, []);

  // Play-mode tint: an amber inset ring around the whole dock marks the editor as live
  // (Unity's playmode-tint lesson). The viewport interior stays untinted; it is the game view.
  const playRing = playState === "edit" ? "" : "ring-2 ring-inset ring-amber-500/60 rounded-sm";

  return (
    <div className={`relative flex min-h-0 min-w-0 flex-1 ${playRing}`}>
      <RevealBands space="scene" bands={SCENE_REVEAL_BANDS} />
      <div className="min-h-0 min-w-0 flex-1">
        <DockRoot space="scene" />
      </div>
    </div>
  );
}
