/// The asset-editor island's dock panels. Each reads the live preview state from
/// `AssetPreviewContext` (model, orbit handlers, the subsurface host ref) which
/// `AssetEditorWorkspace` provides around its own `DockPanelsHost` + `DockRoot` — so these
/// portaled bodies inherit the workspace's state even though their DOM lands in the leaves.
/// They take no props; that is how a registry-rendered panel stays decoupled.
import { createContext, useContext } from "react";
import { Loader2 } from "lucide-react";
import type { PointerEvent, ReactNode, RefObject, WheelEvent } from "react";
import { SkeletonTree } from "./SkeletonTree";
import { ClipList } from "./ClipList";
import { MaterialEditorPanel } from "./MaterialEditorPanel";
import { TimelineTransport } from "../components/timeline/TimelineTransport";
import { TimelineSurface } from "../components/timeline/TimelineSurface";
import type { TimelineTarget } from "../components/timeline/shared";
import { useEditorStore } from "../state/store";
import type { AssetModelResult } from "../protocol";
import { VegetationAssetWorkspace } from "./VegetationAssetWorkspace";
import type { VegetationAssetType } from "../state/store";

export interface AssetPreviewOrbitHandlers {
  onPointerDown(event: PointerEvent<HTMLDivElement>): void;
  onPointerMove(event: PointerEvent<HTMLDivElement>): void;
  onPointerUp(event: PointerEvent<HTMLDivElement>): void;
  onWheel(event: WheelEvent<HTMLDivElement>): void;
}

export interface AssetPreviewContextValue {
  model: AssetModelResult | null;
  rootEntity: string | null;
  highlightJoint: number;
  onBoneSelect(joint: number): void;
  hostRef: RefObject<HTMLDivElement | null>;
  orbit: AssetPreviewOrbitHandlers;
  /// Whether this asset tab is the active main tab (its preview is live, not suspended).
  active: boolean;
  /// Whether the model's capabilities are known and the preview is entered.
  ready: boolean;
  /// The previewed catalog asset id (the workspace's subject).
  assetId: string;
  /// The subject's vegetation domain when it is a plant/biome/map (else `null`);
  /// vegetation subjects have no 3D preview surface.
  vegetationType: VegetationAssetType | null;
  /// The previewed material's id when the subject is a material (else `null`) — pins the
  /// Material panel's sidebar to this subject.
  materialSubject: string | null;
}

const AssetPreviewContext = createContext<AssetPreviewContextValue | null>(null);

export function AssetPreviewProvider({
  value,
  children,
}: {
  value: AssetPreviewContextValue;
  children: ReactNode;
}) {
  return <AssetPreviewContext.Provider value={value}>{children}</AssetPreviewContext.Provider>;
}

export function useAssetPreview(): AssetPreviewContextValue {
  const ctx = useContext(AssetPreviewContext);
  if (ctx === null) {
    throw new Error("asset-editor panel rendered outside AssetPreviewProvider");
  }
  return ctx;
}

/// The "Preparing…" overlay (mirrors LoadingOverlay's non-error visual). Opaque so the
/// unsettled subsurface frame never shows through the transparent viewport hole.
export function Preparing({ className }: { className: string }) {
  return (
    <div className={className} role="status" aria-live="polite">
      <div className="flex flex-col items-center gap-3.5 text-muted-foreground">
        <Loader2 className="size-8 animate-spin text-primary" aria-hidden="true" />
        <div className="text-[13px]">Preparing…</div>
      </div>
    </div>
  );
}

/// The locked preview leaf body: the transparent hole down to the engine's own "assetPreview" subsurface
/// (permanently sized to this pane — no resize mask needed). No bg — the pane stays transparent.
export function AssetPreviewPanel() {
  const { hostRef, orbit, vegetationType } = useAssetPreview();
  if (vegetationType === "biome" || vegetationType === "vegetation-map") {
    // Biome/map subjects render no preview surface (a plant previews its compiled
    // renderable form like any model): paint opaque so the parked subsurface region
    // never shows the desktop through the hole.
    return (
      <div className="flex h-full w-full items-center justify-center bg-background">
        <span className="text-[11px] text-muted-foreground">No 3D preview for this asset</span>
      </div>
    );
  }
  return (
    <div
      className="relative h-full w-full overflow-hidden"
      onPointerDown={orbit.onPointerDown}
      onPointerMove={orbit.onPointerMove}
      onPointerUp={orbit.onPointerUp}
      onPointerCancel={orbit.onPointerUp}
      onWheel={orbit.onWheel}
    >
      <div ref={hostRef} className="viewport-host" />
    </div>
  );
}

/// The vegetation summary dock panel: the read-only catalog view of the subject
/// plant/biome/map, hosted in the asset-editor island.
export function VegetationSummaryPanel() {
  const { assetId, vegetationType } = useAssetPreview();
  if (vegetationType === null) {
    return null;
  }
  return <VegetationAssetWorkspace assetId={assetId} assetType={vegetationType} />;
}

export function AssetSkeletonPanel() {
  const { model, highlightJoint, onBoneSelect } = useAssetPreview();
  return (
    <SkeletonTree
      bones={model?.bones ?? []}
      selectedIndex={highlightJoint}
      onSelect={onBoneSelect}
    />
  );
}

export function AssetClipsPanel() {
  const { model, rootEntity } = useAssetPreview();
  return <ClipList model={model} rootEntity={rootEntity} />;
}

/// The Material editor pinned to the previewed material (asset-editor right dock): the scene
/// dock's panel minus its selector row — the subject is fixed to the asset being previewed.
export function AssetMaterialPanel() {
  const { materialSubject } = useAssetPreview();
  // No inline preview sphere: the previewer already shows the material full-size in the viewport.
  return (
    <MaterialEditorPanel pinnedMaterialId={materialSubject ?? undefined} hideSelector hidePreview />
  );
}

export function AssetTimelinePanel() {
  const { rootEntity, active, ready } = useAssetPreview();
  // animationState is read here (not threaded through the context) so a playback tick never
  // re-renders the skeleton/clips panels — only this timeline.
  const animationState = useEditorStore((s) => s.animationState);
  const target: TimelineTarget = {
    entityId: rootEntity,
    state: animationState,
    clips: [],
    enabled: active && ready && rootEntity !== null,
  };
  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <TimelineTransport target={target} showClipSelect={false} />
      <TimelineSurface target={target} />
    </div>
  );
}
