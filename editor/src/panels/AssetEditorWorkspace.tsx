/// The asset editor: a full work-area main tab (see App.tsx / openAssetEditorTab) that previews ANY
/// model — rigged or static — outside the authored scene. The engine spawns the model into an isolated
/// preview scene and publishes it through the one viewport subsurface (glued into the center pane here).
/// The side panels appear by capability: the skeleton tree (left) only for a rigged model, the clip list
/// + details (right) and the bottom timeline only when the model has clips. A static model is just the
/// framed viewport (orbit, materials, floor) — no rig chrome.
///
/// Orbit is eased: input moves a target, a rAF loop drains current→target with the engine's tau (refs
/// only, no React re-render), so a slight lag reads as smooth motion. Loading is masked: the panels +
/// subsurface mount only once the model's capabilities are known (so the first frame already has the
/// right panels at the final width), behind a "Preparing…" spinner that lifts after the viewport settles.
///
/// Lifecycle is keyed to the mount: App renders this with key={assetId}, so switching to a different
/// model remounts (cleanup exits model A, mount enters model B) — an activeKind-only effect would keep
/// previewing A under B's panels. enter-asset-preview / exit-asset-preview stash + restore the camera
/// engine-side, so orbiting never dirties the saved editorCamera.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Axis3d, Bone, Box, Grid2x2, Wrench } from "lucide-react";
import { client } from "../control/client";
import { useSubsurfaceBounds } from "../lib/useSubsurfaceBounds";
import { useOrbitCamera, type OrbitState } from "../lib/useOrbitCamera";
import { errorText, notifyError } from "../lib/flash";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { DockRoot } from "@/components/dock/DockRoot";
import { DockPanelsHost } from "@/components/dock/DockPanelsHost";
import { RevealBands, type RevealBand } from "@/components/dock/RevealBands";
import {
  AssetPreviewProvider,
  Preparing,
  type AssetPreviewContextValue,
} from "./assetEditorPanels";
import { useEditorStore } from "../state/store";
import type { AssetModelResult } from "../protocol";

/// The empty asset-editor edge regions that accept a torn tab while collapsed: the right dock and
/// the bottom timeline strip (the persistent `aeRight` / `assetTimeline` leaves).
const AE_REVEAL_BANDS: RevealBand[] = [
  { leafId: "leaf:aeLeft", edge: "left" },
  { leafId: "leaf:aeRight", edge: "right" },
  { leafId: "leaf:aeBottom", edge: "bottom" },
];

export function AssetEditorWorkspace({ assetId, active }: { assetId: string; active: boolean }) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [errorMessage, setErrorMessage] = useState("");
  const [model, setModel] = useState<AssetModelResult | null>(null);
  const [rootEntity, setRootEntity] = useState<string | null>(null);
  const [floor, setFloor] = useState(true);
  // Overlay toggles default on (the engine forces show=on while previewing); local mirror for the chips.
  const [showBones, setShowBones] = useState(true);
  const [showAxes, setShowAxes] = useState(false);
  // A plant subject's authored (variation, phenotype) combinations — the scrub domain
  // reported by enter-asset-preview; empty for every other subject.
  const [combinations, setCombinations] = useState<{ variation: number; phenotype: number }[]>([]);
  const [combinationIndex, setCombinationIndex] = useState(0);
  // The bone the tree has highlighted (a get-asset-model node index); local view state, not selection.
  const [highlightJoint, setHighlightJoint] = useState(-1);

  // The catalog row (for a texture subject: drives the pan-only orbit).
  const asset = useEditorStore(
    useCallback((s) => s.assets.find((a) => a.id === assetId), [assetId]),
  );
  // A texture previews as its map on the studio sphere: orbit-only (no dolly — the framed sphere is
  // the subject). An HDRI is the exception: a full environment scene (three balls) that keeps dolly
  // + gains an exposure sweep.
  const isTexture = asset?.type === "texture";
  // A material subject gets the Material editor pinned to it in the right dock (no selector). For a
  // self-container material the catalog id IS the material id, so `assetId` is what the panel edits.
  const isMaterial = asset?.type === "material";
  // Vegetation subjects: a plant previews its compiled renderable form like any model;
  // biome/map have no 3D subject — no enter-asset-preview, no subsurface, the summary
  // panel carries the workspace.
  const vegetationType =
    asset?.type === "plant" || asset?.type === "biome" || asset?.type === "vegetation-map"
      ? asset.type
      : null;
  const summaryOnly = vegetationType === "biome" || vegetationType === "vegetation-map";
  const isHdr = asset?.role === "hdri" || asset?.colorspace === "hdr";
  const panOnly = isTexture && !isHdr;
  // The HDRI preview's exposure sweep (EV, exp2). Restored engine-side on exit-asset-preview, so it
  // never dirties the authored viewport's exposure.
  const [exposureEv, setExposureEv] = useState(0);
  const onExposure = useCallback((ev: number) => {
    setExposureEv(ev);
    void client.setExposure(ev).catch((err: unknown) => notifyError(errorText(err)));
  }, []);

  // What the model can do gates the panels: the skeleton tree only for a rigged model, the clip list +
  // timeline only when it has clips. A static model shows just the viewport + floor toggle.
  const caps = model?.capabilities ?? null;
  const hasRig = caps?.hasRig ?? false;
  const hasClips = (caps?.clipCount ?? 0) > 0;
  const ready = status === "ready";

  // Drive this pane's OWN "assetPreview" viewport surface (permanently sized to the pane). Gated on
  // `active && ready` so a parked/loading pane emits nothing; App.tsx parks the surface when inactive.
  useSubsurfaceBounds(hostRef, "assetPreview", {
    enabled: active && ready && !summaryOnly,
  });

  // Highlight a bone in the live overlay (a get-asset-model node index) — view state, not selection.
  const onBoneSelect = useCallback((joint: number) => {
    setHighlightJoint(joint);
    void client.setSkeletonHighlight(joint).catch((err: unknown) => notifyError(errorText(err)));
  }, []);

  // A click (no drag) on a rigged model picks the nearest joint and selects its bone in the tree.
  const pickJoint = useCallback(
    (u: number, v: number) => {
      void client
        .pickSkeletonJoint(u, v)
        .then((res) => {
          if (res.found && res.nodeIndex >= 0) {
            onBoneSelect(res.nodeIndex);
          }
        })
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [onBoneSelect],
  );

  // The eased orbit for this pane: a lone texture sphere is orbit-only (no dolly — the framed sphere
  // is the whole subject); a model / the HDRI three-ball rig dollies; a click picks a joint only on a
  // rigged model.
  const orbit = useOrbitCamera({ enableZoom: !panOnly, onClick: hasRig ? pickJoint : undefined });

  // Enter the preview on mount, exit on unmount. Remounting (a different assetId) runs cleanup first,
  // so a model A -> model B switch is a real exit/enter. A real failure lands the workspace in its
  // error state; a static (skinless) model is NOT a failure — it opens with an empty bone tree. The
  // framed camera SNAPS (target == current, no ease on first show).
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        if (summaryOnly) {
          // No 3D subject: the summary panel is the workspace.
          setModel(null);
          setStatus("ready");
          return;
        }
        const entered = await client.enterAssetPreview(assetId);
        if (cancelled) {
          return;
        }
        setRootEntity(entered.rootEntity);
        setCombinations(entered.plantCombinations ?? []);
        setCombinationIndex(0);
        const cam = await client.getCamera();
        if (cancelled) {
          return;
        }
        const framed: OrbitState = {
          target: { ...entered.target },
          distance: entered.distance,
          yaw: cam.yaw,
          pitch: cam.pitch,
        };
        orbit.setFramed(framed);
        // A container-less preview subject (a standalone texture on the studio sphere, a built-in
        // primitive) has no `.smodel`, so `get-asset-model` fails — that is not an error: show just
        // the viewport + floor toggle (no rig, no clips), the same as a static model.
        let loaded: AssetModelResult | null = null;
        try {
          loaded = await client.getAssetModel(assetId);
        } catch {
          loaded = null;
        }
        if (cancelled) {
          return;
        }
        setModel(loaded);
        setStatus("ready");
      } catch (err) {
        if (!cancelled) {
          setErrorMessage(errorText(err));
          setStatus("error");
        }
      }
    })();
    return () => {
      cancelled = true;
      void client.exitAssetPreview().catch(() => {});
    };
  }, [assetId, orbit, summaryOnly]);

  // This pane owns its OWN viewport surface (the "assetPreview" view), permanently sized to the pane.
  // App.tsx drives set-active-view + per-view park on a tab switch — switching is instant (the surface
  // keeps its last frame frozen, no re-spawn), so this workspace has no per-`active` lifecycle work: it
  // enters the preview on mount, exits on unmount, and otherwise just drives its surface bounds (gated on
  // `active` so the parked pane's 0x0 host emits nothing).

  // Re-apply the HDRI preview's EV whenever this tab becomes active: leaving the preview view
  // restores the authored exposure engine-side (so the scene tab is never wrong), so returning must
  // re-assert the sweep. No-op for a non-HDR subject (EV stays 0).
  useEffect(() => {
    if (active && ready && isHdr && exposureEv !== 0) {
      void client.setExposure(exposureEv).catch((err: unknown) => notifyError(errorText(err)));
    }
  }, [active, ready, isHdr, exposureEv]);

  // Capability gating runs through the dock model, not a render branch: once the model's
  // capabilities are known, open the panels it supports (rig → skeleton; clips → clips +
  // the timeline) and close the rest. Their leaves are persistent, so DockRoot collapses
  // an empty one — a static model shows just the preview. `preview` is always open.
  useEffect(() => {
    if (!ready) {
      return;
    }
    const { openPanel, closePanel } = useEditorStore.getState();
    if (hasRig) {
      openPanel("skeleton");
    } else {
      closePanel("skeleton");
    }
    if (hasClips) {
      openPanel("clips");
      openPanel("assetTimeline");
    } else {
      closePanel("clips");
      closePanel("assetTimeline");
    }
    if (isMaterial) {
      openPanel("materialEdit");
    } else {
      closePanel("materialEdit");
    }
    if (vegetationType !== null) {
      openPanel("vegSummary");
    } else {
      closePanel("vegSummary");
    }
    if (vegetationType === "biome") {
      openPanel("biomeGraph");
    } else {
      closePanel("biomeGraph");
    }
  }, [ready, hasRig, hasClips, isMaterial, vegetationType]);

  // Space = play/pause while THIS tab is active (the workspace stays mounted-but-hidden when parked, so
  // the window listener must no-op unless active) and no text field is focused.
  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (!active || e.code !== "Space" || !rootEntity) {
        return;
      }
      const el = document.activeElement;
      if (
        el instanceof HTMLElement &&
        el.closest("input, textarea, select, [contenteditable='true']")
      ) {
        return;
      }
      e.preventDefault();
      const st = useEditorStore.getState().animationState;
      if (st?.playing) {
        void client
          .setAnimationPlaying(rootEntity, false)
          .catch((err: unknown) => notifyError(errorText(err)));
      } else if (st?.clip) {
        // Resume from the current playhead — play-animation would restart the clip at frame 0.
        void client
          .setAnimationPlaying(rootEntity, true)
          .catch((err: unknown) => notifyError(errorText(err)));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, rootEntity]);

  // Scrubs the plant subject to another authored combination (variation + phenotype).
  const onCombination = useCallback(
    (index: number) => {
      setCombinationIndex(index);
      const combination = combinations[index];
      if (!combination) {
        return;
      }
      void client
        .setAssetPreviewOptions({
          variation: combination.variation,
          phenotype: combination.phenotype,
        })
        .catch((err: unknown) => notifyError(errorText(err)));
    },
    [combinations],
  );

  const toggleFloor = useCallback(() => {
    setFloor((prev) => {
      const next = !prev;
      void client
        .setAssetPreviewOptions({ floor: next })
        .catch((err: unknown) => notifyError(errorText(err)));
      return next;
    });
  }, []);

  const toggleBones = useCallback(() => {
    setShowBones((prev) => {
      const next = !prev;
      void client
        .setSkeletonOverlay({ show: next })
        .catch((err: unknown) => notifyError(errorText(err)));
      return next;
    });
  }, []);

  const toggleAxes = useCallback(() => {
    setShowAxes((prev) => {
      const next = !prev;
      void client
        .setSkeletonOverlay({ axes: next })
        .catch((err: unknown) => notifyError(errorText(err)));
      return next;
    });
  }, []);

  // The live preview state the dock panels read. Provided around this island's DockRoot +
  // DockPanelsHost so the portaled panel bodies inherit it across the leaves they land in.
  const previewContext: AssetPreviewContextValue = useMemo(
    () => ({
      model,
      rootEntity,
      highlightJoint,
      onBoneSelect,
      hostRef,
      orbit: {
        onPointerDown: orbit.onPointerDown,
        onPointerMove: orbit.onPointerMove,
        onPointerUp: orbit.onPointerUp,
        onWheel: orbit.onWheel,
      },
      active,
      ready,
      materialSubject: isMaterial ? assetId : null,
      assetId,
      vegetationType,
    }),
    [
      model,
      rootEntity,
      highlightJoint,
      onBoneSelect,
      orbit,
      active,
      ready,
      isMaterial,
      assetId,
      vegetationType,
    ],
  );

  if (status === "error") {
    return (
      <main className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 bg-background px-6 text-center">
        <Box className="size-8 text-muted-foreground" />
        <p className="text-sm text-foreground">Could not open this asset.</p>
        <p className="max-w-md text-xs text-muted-foreground">{errorMessage}</p>
        <p className="text-xs text-muted-foreground">
          Re-import the model if the problem persists.
        </p>
      </main>
    );
  }

  // No bg on <main>: the center preview pane must stay a transparent hole down to the engine's Wayland
  // subsurface (composited below the webview). Every other region paints its own opaque bg-background
  // (the toolbar below, the side panels, the timeline strip, the Preparing spinner), so only the pane
  // shows through. The panels + subsurface mount only once `ready` (capabilities known) so the first
  // subsurface frame is already at the final pane width — no panel pop-in, no resize stretch.
  return (
    <main className="flex min-h-0 flex-1 flex-col overflow-hidden">
      {/* min-h-12 reserves the ready-state toolbar height (a 32px icon-sm button row + py-2) from the
          first frame, so the header does not grow — and the title below shift down — when the async
          preview load flips `ready` and mounts the toolbar. */}
      <div className="flex min-h-12 items-center gap-3 border-b border-border bg-background px-3 py-2">
        <Box className="size-4 text-muted-foreground" />
        <span className="text-sm font-medium text-foreground">
          {asset?.name ?? model?.name ?? "Asset"}
        </span>
        {ready ? (
          <div className="ml-auto flex items-center gap-1">
            {isHdr ? (
              // The HDRI environment preview's exposure sweep: reveal clipped sun/window detail at
              // low EV, lift shadows at high EV. Engine-restored on exit, so it is preview-only.
              <div className="mr-1 flex items-center gap-2">
                <span className="text-xs text-muted-foreground">EV</span>
                <Slider
                  className="w-28"
                  value={[exposureEv]}
                  min={-6}
                  max={6}
                  step={0.1}
                  onValueChange={([v]) => onExposure(v)}
                  aria-label="Preview exposure (EV)"
                />
                <span className="w-8 text-right text-xs tabular-nums text-muted-foreground">
                  {exposureEv > 0 ? `+${exposureEv.toFixed(1)}` : exposureEv.toFixed(1)}
                </span>
              </div>
            ) : null}
            {hasRig ? (
              <>
                <Button
                  variant={showBones ? "secondary" : "ghost"}
                  size="icon-sm"
                  onClick={toggleBones}
                  aria-label="Toggle skeleton overlay"
                >
                  <Bone className="size-4" />
                </Button>
                <Button
                  variant={showAxes ? "secondary" : "ghost"}
                  size="icon-sm"
                  onClick={toggleAxes}
                  aria-label="Toggle joint axes"
                >
                  <Axis3d className="size-4" />
                </Button>
              </>
            ) : null}
            {combinations.length > 1 ? (
              <Select
                value={String(combinationIndex)}
                onValueChange={(value) => onCombination(Number(value))}
              >
                <SelectTrigger
                  size="sm"
                  className="h-7 w-40 text-[11px]"
                  aria-label="Plant combination"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {combinations.map((combination, index) => (
                    <SelectItem
                      key={`${combination.variation}-${combination.phenotype}`}
                      value={String(index)}
                      className="text-[11px]"
                    >
                      Variation {combination.variation} · Phenotype {combination.phenotype}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : null}
            <Button
              variant={floor ? "secondary" : "ghost"}
              size="icon-sm"
              onClick={toggleFloor}
              aria-label="Toggle preview floor"
            >
              <Grid2x2 className="size-4" />
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button type="button" variant="ghost" size="icon-sm" aria-label="Tools">
                  <Wrench className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-32">
                <DropdownMenuItem
                  onSelect={() => useEditorStore.getState().openPanel("assetStats")}
                >
                  Stats
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        ) : null}
      </div>
      {ready ? (
        // The asset-editor dock island: its skeleton / preview(locked) / clips / assetTimeline
        // panels are a draggable DockRoot tree (the second dockspace kind). The panels render
        // through this island's own DockPanelsHost, portaled into the leaves — so they inherit
        // the preview state via AssetPreviewProvider and survive retab/split moves.
        <AssetPreviewProvider value={previewContext}>
          <DockPanelsHost space="assetEditor" />
          <div className="relative min-h-0 flex-1">
            <RevealBands space="assetEditor" bands={AE_REVEAL_BANDS} />
            <DockRoot space="assetEditor" />
          </div>
        </AssetPreviewProvider>
      ) : (
        <Preparing className="flex min-h-0 flex-1 items-center justify-center bg-background" />
      )}
    </main>
  );
}
