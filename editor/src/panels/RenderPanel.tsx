/// The Render panel: the project's render configuration — anti-aliasing, quality, resolution, the
/// view transform, target FPS, the feature toggles, exposure, and debug overlays. These persist with
/// the project (the engine serializes them into the `renderSettings` block and reapplies them on
/// load), so they sit beside Environment as scene-presentation config, not in the Stats telemetry
/// tool. Bloom and the color grade live in the dedicated Post panel.
///
/// Values are read with a shallow-selected subset of `renderStats` so the panel only re-renders when
/// a config field actually changes — not on the 20 Hz render-stats poll that rewrites the full bag. A
/// write optimistically folds the new value in (and the echoed result) so the control reflects the
/// change at once; the reconcile poll re-reads the full bag right after.
import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { NumberDrag } from "../components/NumberDrag";
import { errorText, notifyError } from "../lib/flash";
import type { RenderStats } from "../protocol";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

type AaMode = RenderStats["aa"];

const AA_MODES: { value: AaMode; label: string }[] = [
  { value: "off", label: "Off" },
  { value: "fxaa", label: "FXAA" },
  { value: "taa", label: "TAA" },
  { value: "msaa2", label: "MSAA 2x" },
  { value: "msaa4", label: "MSAA 4x" },
  { value: "msaa8", label: "MSAA 8x" },
];

/// The render-quality tier — one knob for the SSGI / GTAO / contact-shadow stack. Higher tiers
/// spend more GPU; the editor can run a cheaper tier than the shipped game.
const QUALITY_TIERS: { value: string; label: string }[] = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "ultra", label: "Ultra" },
];

/// The frame-rate cap that paces the render loop. `default` tracks the display refresh (the
/// vsync-locked rAF cadence); the rest are fixed Hz. The value is the store's `targetFpsMode`.
const TARGET_FPS_OPTIONS: { value: string; label: string }[] = [
  { value: "default", label: "Default (vsync)" },
  { value: "30", label: "30 Hz" },
  { value: "60", label: "60 Hz" },
  { value: "120", label: "120 Hz" },
  { value: "144", label: "144 Hz" },
  { value: "240", label: "240 Hz" },
];

/// The TAAU input:display render-scale presets. The temporal upsampler reconstructs the sub-native
/// input to a sharp display image; a lower ratio trades reconstruction softness for frame time.
const RESOLUTION_PRESETS: { value: string; label: string }[] = [
  { value: "1", label: "Native (100%)" },
  { value: "0.83", label: "Quality (83%)" },
  { value: "0.67", label: "Balanced (67%)" },
  { value: "0.5", label: "Performance (50%)" },
];

/// Maps a live ratio to the nearest preset value string (for the Select's controlled value).
function nearestResolutionPreset(ratio: number): string {
  let best = RESOLUTION_PRESETS[0];
  for (const p of RESOLUTION_PRESETS) {
    if (Math.abs(Number(p.value) - ratio) < Math.abs(Number(best.value) - ratio)) {
      best = p;
    }
  }
  return best.value;
}

/// The HDR→display tonemap operator.
const TONEMAP_OPTIONS: { value: string; label: string }[] = [
  { value: "aces", label: "ACES" },
  { value: "agx", label: "AgX" },
  { value: "pbr-neutral", label: "PBR Neutral" },
  { value: "reinhard", label: "Reinhard" },
];

/// Resolves the target-fps mode to a concrete Hz: a fixed mode is itself; `default` rounds the
/// presenter's reported display refresh, falling back to the engine's current target until known.
function resolveTargetFps(mode: "default" | number, refreshHz: number, current: number): number {
  if (typeof mode === "number") {
    return mode;
  }
  return refreshHz > 1 ? Math.round(refreshHz) : Math.round(current);
}

/// The boolean feature toggles (label + the stat field + its setter). RT-gated rows
/// carry `rtGated` so the panel disables them when the device lacks support.
const TOGGLES: {
  label: string;
  field: keyof RenderStats;
  set: (on: boolean) => Promise<unknown>;
  rtGated?: boolean;
}[] = [
  { label: "Clustered", field: "clustered", set: (on) => client.setClustered(on) },
  { label: "Depth Pre-pass", field: "depthPrepass", set: (on) => client.setDepthPrepass(on) },
  { label: "Shadows", field: "shadows", set: (on) => client.setShadows(on) },
  { label: "IBL", field: "ibl", set: (on) => client.setIbl(on) },
  { label: "DDGI", field: "ddgi", set: (on) => client.setGi(on ? "ddgi" : "off") },
  { label: "RT Shadows", field: "rtShadows", set: (on) => client.setRtShadows(on), rtGated: true },
  { label: "ReSTIR", field: "restir", set: (on) => client.setRestir(on), rtGated: true },
  { label: "SSR", field: "ssr", set: (on) => client.setSsr(on) },
  {
    label: "RT Reflections",
    field: "rtReflections",
    set: (on) => client.setRtReflections(on),
    rtGated: true,
  },
];

/// Debug-visualization overlays (set-debug-overlays). Persisted with the project but not undoable —
/// distinct from the feature toggles above, which are project render config.
const DEBUG_OVERLAYS: {
  label: string;
  field: "bounds" | "sceneAabb" | "lightVolumes" | "grid" | "colliders";
}[] = [
  { label: "Bounding Boxes", field: "bounds" },
  { label: "Scene AABB", field: "sceneAabb" },
  { label: "Light Volumes", field: "lightVolumes" },
  { label: "Grid", field: "grid" },
  { label: "Colliders", field: "colliders" },
];

function ToggleRow({
  label,
  checked,
  disabled,
  tooltip,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  disabled: boolean;
  tooltip?: string;
  onCheckedChange(next: boolean): void;
}) {
  const row = (
    <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onCheckedChange} />
    </div>
  );
  if (!tooltip) {
    return row;
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div>{row}</div>
      </TooltipTrigger>
      <TooltipContent>{tooltip}</TooltipContent>
    </Tooltip>
  );
}

export function RenderPanel() {
  const ready = useEditorStore((s) => s.engineStatus.phase === "ready");
  const hasStats = useEditorStore((s) => s.renderStats !== null);
  const setRenderStats = useEditorStore((s) => s.setRenderStats);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const debugOverlays = useEditorStore((s) => s.debugOverlays);
  const setDebugOverlays = useEditorStore((s) => s.setDebugOverlays);
  const targetFpsMode = useEditorStore((s) => s.targetFpsMode);
  const setTargetFpsMode = useEditorStore((s) => s.setTargetFpsMode);
  const setPerfConfig = useEditorStore((s) => s.setPerfConfig);
  const perfTargetFps = useEditorStore((s) => s.perfConfig?.targetFps ?? null);
  const upscale = useEditorStore((s) => s.upscale);
  const setUpscale = useEditorStore((s) => s.setUpscale);
  // The true display refresh from the Wayland presenter (the webview's rAF is 60-pinned, useless
  // here). `0` until the first presented frame reports it, so we poll until it settles.
  const [displayRefreshHz, setDisplayRefreshHz] = useState(0);
  const cfg = useEditorStore(
    useShallow((s) => {
      const r = s.renderStats;
      return {
        aa: (r?.aa ?? "off") as AaMode,
        exposureEv: r?.exposureEv ?? 0,
        rtSupported: r?.rtSupported ?? false,
        clustered: r?.clustered ?? false,
        depthPrepass: r?.depthPrepass ?? false,
        shadows: r?.shadows ?? false,
        ibl: r?.ibl ?? false,
        quality: r?.quality ?? "high",
        tonemap: r?.tonemap ?? "aces",
        ddgi: r?.ddgi ?? false,
        rtShadows: r?.rtShadows ?? false,
        restir: r?.restir ?? false,
        ssr: r?.ssr ?? false,
        rtReflections: r?.rtReflections ?? false,
      };
    }),
  );

  const optimistic = (patch: Partial<RenderStats>): void => {
    const cur = useEditorStore.getState().renderStats;
    if (cur) {
      setRenderStats({ ...cur, ...patch });
    }
  };

  // Debug overlays persist with the project but are not undoable (view state, not scene content).
  // Fetch once on mount; the render-panel-gated poll keeps them live (and reflects external `sa`).
  useEffect(() => {
    if (ready && debugOverlays === null) {
      void client
        .getDebugOverlays()
        .then(setDebugOverlays)
        .catch(() => {
          // Engine briefly busy; the render-panel poll picks the overlays up on its next tick.
        });
    }
  }, [ready, debugOverlays, setDebugOverlays]);

  // Poll the presenter for the true display refresh. It's 0 until the first presented frame reports
  // it, so poll until it settles (then stop), and re-poll on (re)ready.
  useEffect(() => {
    if (!ready || displayRefreshHz > 1) {
      return;
    }
    let cancelled = false;
    const tick = (): void => {
      void client
        .viewportRefreshHz()
        .then((hz) => {
          if (!cancelled && hz > 1) {
            setDisplayRefreshHz(hz);
          }
        })
        .catch(() => {});
    };
    tick();
    const id = window.setInterval(tick, 500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [ready, displayRefreshHz]);

  // Keep the engine's frame budget in sync with the selected target-FPS mode. The budget lives on
  // the TAAU surface now (set-upscale's targetMs = 1000 / fps); get-perf-config stays the budget
  // telemetry read, so we refresh it after the write to settle this effect. `Default` follows the
  // display refresh, so this re-pushes when the measured refresh settles or the mode changes.
  useEffect(() => {
    if (!ready || perfTargetFps === null) {
      return;
    }
    const want = resolveTargetFps(targetFpsMode, displayRefreshHz, perfTargetFps);
    if (want >= 1 && Math.round(perfTargetFps) !== want) {
      void client
        .setUpscale({ targetMs: 1000 / want })
        .then(() => client.getPerfConfig())
        .then((config) => setPerfConfig(config))
        .catch((err: unknown) => notifyError(errorText(err)));
    }
  }, [ready, targetFpsMode, displayRefreshHz, perfTargetFps, setPerfConfig]);

  const onDebugToggle = (field: (typeof DEBUG_OVERLAYS)[number]["field"], next: boolean): void => {
    const previous = useEditorStore.getState().debugOverlays;
    if (previous) {
      setDebugOverlays({ ...previous, [field]: next });
    }
    void client
      .setDebugOverlays({ [field]: next })
      .then(setDebugOverlays)
      .catch((err: unknown) => {
        if (previous) {
          setDebugOverlays(previous);
        }
        notifyError(errorText(err));
      });
  };

  // Render settings persist with the project, so their edits are scene-tab undoable. A
  // toggle/AA records inline (discrete); exposure scrubbing records one entry per gesture.
  const recordRender = (
    label: string,
    undo: () => Promise<unknown>,
    redo: () => Promise<unknown>,
  ): void => {
    useEditorStore.getState().pushEdit({ label, undo, redo }, "scene");
  };

  const onAa = (mode: AaMode): void => {
    const prior = useEditorStore.getState().renderStats?.aa ?? "off";
    optimistic({ aa: mode });
    if (prior !== mode) {
      recordRender(
        "Anti-aliasing",
        () => client.setAa(prior),
        () => client.setAa(mode),
      );
    }
    void client
      .setAa(mode)
      .then((res) => optimistic({ aa: res.aa }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const onQuality = (tier: string): void => {
    const prior = useEditorStore.getState().renderStats?.quality ?? "high";
    optimistic({ quality: tier });
    if (prior !== tier) {
      recordRender(
        "Render quality",
        () => client.setRenderQuality(prior),
        () => client.setRenderQuality(tier),
      );
    }
    void client
      .setRenderQuality(tier)
      .then((res) =>
        // Fold the resolved per-effect flags back so the Stats panel reflects the tier at once
        // (`ssao` is the render-stats name for GTAO).
        optimistic({
          quality: res.tier,
          ssgi: res.ssgi,
          ssao: res.gtao,
          contactShadows: res.contactShadows,
        }),
      )
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// The TAAU render scale (input:display ratio). A partial set-upscale; the reply carries the
  /// merged state (including the recomputed input extent).
  const onRatio = (ratio: number): void => {
    void client
      .setUpscale({ ratio })
      .then((res) => setUpscale(res.upscale))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  /// Dynamic resolution: hand the input extent to the frame-budget controller (targetMs).
  const onDynamic = (next: boolean): void => {
    void client
      .setUpscale({ dynamic: next })
      .then((res) => setUpscale(res.upscale))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const onTonemap = (mode: string): void => {
    const prior = useEditorStore.getState().renderStats?.tonemap ?? "aces";
    optimistic({ tonemap: mode });
    if (prior !== mode) {
      recordRender(
        "Tonemap",
        () => client.setTonemap(prior as "aces"),
        () => client.setTonemap(mode as "aces"),
      );
    }
    void client
      .setTonemap(mode as "aces")
      .then((res) => optimistic({ tonemap: res.mode }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const onToggle = (
    field: keyof RenderStats,
    label: string,
    set: (on: boolean) => Promise<unknown>,
    next: boolean,
  ): void => {
    const cur = useEditorStore.getState().renderStats;
    const previous = cur ? cur[field] === true : !next;
    optimistic({ [field]: next } as Partial<RenderStats>);
    if (previous !== next) {
      recordRender(
        label,
        () => set(previous),
        () => set(next),
      );
    }
    void set(next)
      .then((res) => {
        const echoed = (res as Record<string, unknown>)[field];
        if (typeof echoed === "boolean") {
          optimistic({ [field]: echoed } as Partial<RenderStats>);
        }
      })
      .catch((err: unknown) => {
        optimistic({ [field]: previous } as Partial<RenderStats>);
        notifyError(errorText(err));
      });
  };

  // Exposure scrub: capture the prior at drag start, record once at drag end. A typed
  // edit (no gesture) records inline.
  const exposurePrior = useRef<number | null>(null);
  const onExposureDragStart = (): void => {
    exposurePrior.current = useEditorStore.getState().renderStats?.exposureEv ?? 0;
    setDragActive(true);
  };
  const onExposureDragEnd = (): void => {
    setDragActive(false);
    const prior = exposurePrior.current;
    exposurePrior.current = null;
    if (prior === null) {
      return;
    }
    const after = useEditorStore.getState().renderStats?.exposureEv ?? 0;
    if (prior !== after) {
      recordRender(
        "Exposure",
        () => client.setExposure(prior),
        () => client.setExposure(after),
      );
    }
  };
  const onExposure = (ev: number): void => {
    if (exposurePrior.current === null) {
      const prior = useEditorStore.getState().renderStats?.exposureEv ?? 0;
      if (prior !== ev) {
        recordRender(
          "Exposure",
          () => client.setExposure(prior),
          () => client.setExposure(ev),
        );
      }
    }
    optimistic({ exposureEv: ev });
    void client
      .setExposure(ev)
      .then((res) => optimistic({ exposureEv: res.exposureEv }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  if (!hasStats) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="p-3.5 text-center italic text-muted-foreground">
          {ready ? "Waiting for stats…" : "Engine not ready"}
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-2 p-2.5">
          <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              Anti-aliasing
            </Label>
            <Select value={cfg.aa} disabled={!ready} onValueChange={(v) => onAa(v as AaMode)}>
              <SelectTrigger size="sm" className="h-7 w-[112px] font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AA_MODES.map((m) => (
                  <SelectItem key={m.value} value={m.value} className="text-[11px]">
                    {m.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              Quality
            </Label>
            <Select value={cfg.quality} disabled={!ready} onValueChange={(v) => onQuality(v)}>
              <SelectTrigger size="sm" className="h-7 w-[112px] font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {QUALITY_TIERS.map((q) => (
                  <SelectItem key={q.value} value={q.value} className="text-[11px]">
                    {q.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              Resolution
            </Label>
            <Select
              value={upscale ? nearestResolutionPreset(upscale.ratio) : "1"}
              disabled={!ready || upscale === null || upscale.dynamic}
              onValueChange={(v) => onRatio(Number(v))}
            >
              <SelectTrigger size="sm" className="h-7 w-[112px] font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RESOLUTION_PRESETS.map((p) => (
                  <SelectItem key={p.value} value={p.value} className="text-[11px]">
                    {p.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <ToggleRow
            label="Dynamic resolution"
            checked={upscale?.dynamic ?? false}
            disabled={!ready || upscale === null}
            tooltip="Let the frame-budget controller drive the render scale toward the target FPS"
            onCheckedChange={onDynamic}
          />

          {upscale && (
            <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
              <Label className="truncate text-[11px] font-normal text-muted-foreground">
                Render → display
              </Label>
              <span className="font-mono text-[11px] text-muted-foreground">
                {upscale.inputWidth}×{upscale.inputHeight} → {upscale.displayWidth}×
                {upscale.displayHeight}
              </span>
            </div>
          )}

          <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              View transform
            </Label>
            <Select value={cfg.tonemap} disabled={!ready} onValueChange={(v) => onTonemap(v)}>
              <SelectTrigger size="sm" className="h-7 w-[112px] font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TONEMAP_OPTIONS.map((o) => (
                  <SelectItem key={o.value} value={o.value} className="text-[11px]">
                    {o.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              Target FPS
            </Label>
            <Select
              value={typeof targetFpsMode === "number" ? String(targetFpsMode) : "default"}
              disabled={!ready}
              onValueChange={(v) => setTargetFpsMode(v === "default" ? "default" : Number(v))}
            >
              <SelectTrigger size="sm" className="h-7 w-[112px] font-mono text-[11px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TARGET_FPS_OPTIONS.map((o) => (
                  <SelectItem key={o.value} value={o.value} className="text-[11px]">
                    {o.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {TOGGLES.map((t) => {
            const disabled = !ready || (t.rtGated === true && !cfg.rtSupported);
            const tooltip =
              t.rtGated === true && !cfg.rtSupported
                ? "Ray tracing not supported on this device"
                : undefined;
            return (
              <ToggleRow
                key={t.field}
                label={t.label}
                checked={cfg[t.field as keyof typeof cfg] === true}
                disabled={disabled}
                tooltip={tooltip}
                onCheckedChange={(next) => onToggle(t.field, t.label, t.set, next)}
              />
            );
          })}

          <div className="grid grid-cols-[1fr_120px] items-center gap-1.5">
            <Label className="truncate text-[11px] font-normal text-muted-foreground">
              Exposure (EV)
            </Label>
            <NumberDrag
              value={cfg.exposureEv}
              min={-8}
              max={8}
              step={0.05}
              onChange={onExposure}
              onDragStart={onExposureDragStart}
              onDragEnd={onExposureDragEnd}
            />
          </div>

          <div className="mt-1 border-t border-border pt-2.5">
            <Label className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
              Debug
            </Label>
          </div>
          {DEBUG_OVERLAYS.map((d) => (
            <ToggleRow
              key={d.field}
              label={d.label}
              checked={debugOverlays ? debugOverlays[d.field] : false}
              disabled={!ready}
              onCheckedChange={(next) => onDebugToggle(d.field, next)}
            />
          ))}
        </div>
      </ScrollArea>
    </div>
  );
}
