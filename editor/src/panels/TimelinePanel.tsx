/// The bottom-dock Timeline: a read-only, canvas-rendered sequencer over the selected entity, built
/// as a thin composition of the shared TimelineTransport + TimelineSurface. The asset editor mounts
/// the same pieces against the previewed model; the two never render simultaneously.
import { useEditorStore } from "../state/store";
import { TimelineTransport } from "../components/timeline/TimelineTransport";
import { TimelineSurface } from "../components/timeline/TimelineSurface";
import type { TimelineTarget } from "../components/timeline/shared";

/// An animatable entity carries an `AnimationPlayer`, a `SkinnedMesh`, or a `Morph` component — the
/// three clip-driven sources (skeletal, node-TRS via the player, and blend-shape weights). `listClips`
/// returns the whole project catalog regardless of entity, so the clip list alone cannot gate the panel
/// — without this an unrigged cube would show a phantom track. The inspect result's component map
/// (filled by the reconcile poll on selection) is the reliable signal.
function isAnimatable(components: Record<string, unknown> | undefined): boolean {
  return (
    components !== undefined &&
    ("AnimationPlayer" in components || "SkinnedMesh" in components || "Morph" in components)
  );
}

export function TimelinePanel() {
  const selectedId = useEditorStore((s) => s.selectedId);
  const animationState = useEditorStore((s) => s.animationState);
  const animationClips = useEditorStore((s) => s.animationClips);
  const components = useEditorStore(
    (s) => s.componentsBySelected?.components as Record<string, unknown> | undefined,
  );

  const target: TimelineTarget = {
    entityId: selectedId,
    state: animationState,
    clips: animationClips,
    enabled: animationState !== null || isAnimatable(components),
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-background text-foreground">
      <TimelineTransport target={target} showClipSelect />
      <TimelineSurface target={target} />
    </div>
  );
}
