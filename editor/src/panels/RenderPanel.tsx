/// The Render panel owns rendering algorithms, quality, performance, and diagnostics; image
/// formation lives in Post. Values are read through a shallow-selected subset of `renderStats` so
/// the panel re-renders only when a config field changes, not on every stats poll. A write folds the
/// new value (and the echoed result) in optimistically.
import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { client } from "../control/client";
import { useEditorStore } from "../state/store";
import { NumberDrag } from "../components/NumberDrag";
import { ControlRow, FieldRow, SectionBreak } from "../components/PanelRows";
import {
  applyOptimisticRenderStats as optimistic,
  recordRenderEdit as recordRender,
} from "../lib/renderSettings";
import { errorText, notifyError } from "../lib/flash";
import type { Environment, RenderStats } from "../protocol";
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
type EnvironmentQualityBlock = "atmosphere" | "cloud" | "fog";

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
  field:
    | "bounds"
    | "sceneAabb"
    | "lightVolumes"
    | "grid"
    | "colliders"
    | "vegetationCells"
    | "vegetationBounds"
    | "vegetationRejections"
    | "vegetationHeatmap"
    | "vegetationNavigation"
    | "windVectors";
}[] = [
  { label: "Bounding Boxes", field: "bounds" },
  { label: "Scene AABB", field: "sceneAabb" },
  { label: "Light Volumes", field: "lightVolumes" },
  { label: "Grid", field: "grid" },
  { label: "Colliders", field: "colliders" },
  { label: "Vegetation Cells", field: "vegetationCells" },
  { label: "Vegetation Bounds", field: "vegetationBounds" },
  { label: "Vegetation Rejections", field: "vegetationRejections" },
  { label: "Vegetation Heatmap", field: "vegetationHeatmap" },
  { label: "Vegetation Navigation", field: "vegetationNavigation" },
  { label: "Wind Vectors", field: "windVectors" },
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
    <ControlRow label={label}>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onCheckedChange} />
    </ControlRow>
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
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const environment = useEditorStore((s) => s.environment);
  const setEnvironment = useEditorStore((s) => s.setEnvironment);
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
        rtSupported: r?.rtSupported ?? false,
        clustered: r?.clustered ?? false,
        depthPrepass: r?.depthPrepass ?? false,
        shadows: r?.shadows ?? false,
        ibl: r?.ibl ?? false,
        quality: r?.quality ?? "high",
        ddgi: r?.ddgi ?? false,
        rtShadows: r?.rtShadows ?? false,
        restir: r?.restir ?? false,
        ssr: r?.ssr ?? false,
        rtReflections: r?.rtReflections ?? false,
      };
    }),
  );

  useEffect(() => {
    if (ready && environment === null) {
      void client
        .getEnvironment()
        .then(setEnvironment)
        .catch(() => {});
    }
  }, [ready, environment, setEnvironment]);

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

  const setEnvironmentQualityBlock = (
    block: EnvironmentQualityBlock,
    value: Environment[EnvironmentQualityBlock],
  ): Promise<Environment> => {
    switch (block) {
      case "atmosphere":
        return client.setAtmosphere(value as Environment["atmosphere"]);
      case "cloud":
        return client.setClouds(value as Environment["cloud"]);
      case "fog":
        return client.setFog(value as Environment["fog"]);
    }
  };

  const qualityPrior = useRef<{
    block: EnvironmentQualityBlock;
    label: string;
    value: Environment[EnvironmentQualityBlock];
  } | null>(null);

  const onQualityDragStart = (block: EnvironmentQualityBlock, label: string): void => {
    const current = useEditorStore.getState().environment;
    if (!current) return;
    qualityPrior.current = { block, label, value: current[block] };
    setDragActive(true);
  };

  const onQualityDragEnd = (): void => {
    setDragActive(false);
    const prior = qualityPrior.current;
    qualityPrior.current = null;
    const current = useEditorStore.getState().environment;
    if (!prior || !current) return;
    const after = current[prior.block];
    if (JSON.stringify(prior.value) !== JSON.stringify(after)) {
      recordRender(
        prior.label,
        () => setEnvironmentQualityBlock(prior.block, prior.value),
        () => setEnvironmentQualityBlock(prior.block, after),
      );
    }
  };

  const patchEnvironmentQuality = <B extends EnvironmentQualityBlock>(
    block: B,
    patch: Partial<Environment[B]>,
    label: string,
  ): void => {
    const current = useEditorStore.getState().environment;
    if (!current) return;
    const prior = current[block];
    const after = { ...prior, ...patch } as Environment[B];
    setEnvironment({ ...current, [block]: after });
    if (qualityPrior.current === null && JSON.stringify(prior) !== JSON.stringify(after)) {
      recordRender(
        label,
        () => setEnvironmentQualityBlock(block, prior),
        () => setEnvironmentQualityBlock(block, after),
      );
    }
    void setEnvironmentQualityBlock(block, after)
      .then(setEnvironment)
      .catch((err: unknown) => {
        setEnvironment(current);
        notifyError(errorText(err));
      });
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
          <ControlRow label="Anti-aliasing">
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
          </ControlRow>

          <ControlRow label="Quality">
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
          </ControlRow>

          <ControlRow label="Resolution">
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
          </ControlRow>

          <ToggleRow
            label="Dynamic resolution"
            checked={upscale?.dynamic ?? false}
            disabled={!ready || upscale === null}
            tooltip="Let the frame-budget controller drive the render scale toward the target FPS"
            onCheckedChange={onDynamic}
          />

          {upscale && (
            <ControlRow label="Render → display">
              <span className="font-mono text-[11px] text-muted-foreground">
                {upscale.inputWidth}×{upscale.inputHeight} → {upscale.displayWidth}×
                {upscale.displayHeight}
              </span>
            </ControlRow>
          )}

          <ControlRow label="Target FPS">
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
          </ControlRow>

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

          <SectionBreak>Environment quality</SectionBreak>

          {environment ? (
            <>
              <ToggleRow
                label="Per-pixel transmittance"
                checked={environment.atmosphere.perPixelTransmittance}
                disabled={!ready}
                onCheckedChange={(next) =>
                  patchEnvironmentQuality(
                    "atmosphere",
                    { perPixelTransmittance: next },
                    "Atmosphere quality",
                  )
                }
              />
              <FieldRow label="Sky capture cadence">
                <NumberDrag
                  value={environment.atmosphere.skyCaptureCadence}
                  min={1}
                  max={60}
                  step={1}
                  onChange={(value) =>
                    patchEnvironmentQuality(
                      "atmosphere",
                      { skyCaptureCadence: Math.round(value) },
                      "Atmosphere quality",
                    )
                  }
                  onDragStart={() => onQualityDragStart("atmosphere", "Atmosphere quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
              <FieldRow label="Cloud primary steps">
                <NumberDrag
                  value={environment.cloud.primarySteps}
                  min={1}
                  max={256}
                  step={1}
                  onChange={(value) =>
                    patchEnvironmentQuality(
                      "cloud",
                      { primarySteps: Math.round(value) },
                      "Cloud quality",
                    )
                  }
                  onDragStart={() => onQualityDragStart("cloud", "Cloud quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
              <FieldRow label="Cloud light steps">
                <NumberDrag
                  value={environment.cloud.lightSteps}
                  min={1}
                  max={32}
                  step={1}
                  onChange={(value) =>
                    patchEnvironmentQuality(
                      "cloud",
                      { lightSteps: Math.round(value) },
                      "Cloud quality",
                    )
                  }
                  onDragStart={() => onQualityDragStart("cloud", "Cloud quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
              <FieldRow label="Cloud temporal factor">
                <NumberDrag
                  value={environment.cloud.temporalFactor}
                  min={0}
                  max={1}
                  step={0.005}
                  onChange={(value) =>
                    patchEnvironmentQuality("cloud", { temporalFactor: value }, "Cloud quality")
                  }
                  onDragStart={() => onQualityDragStart("cloud", "Cloud quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
              <ControlRow label="Volumetric fog quality">
                <Select
                  value={environment.fog.quality}
                  disabled={!ready}
                  onValueChange={(value) =>
                    patchEnvironmentQuality(
                      "fog",
                      { quality: value as Environment["fog"]["quality"] },
                      "Fog quality",
                    )
                  }
                >
                  <SelectTrigger size="sm" className="h-7 w-[112px] text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {(["low", "medium", "high"] as const).map((quality) => (
                      <SelectItem key={quality} value={quality} className="text-[11px] capitalize">
                        {quality}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </ControlRow>
              <FieldRow label="Fog history blend">
                <NumberDrag
                  value={environment.fog.historyBlend}
                  min={0}
                  max={1}
                  step={0.005}
                  onChange={(value) =>
                    patchEnvironmentQuality("fog", { historyBlend: value }, "Fog quality")
                  }
                  onDragStart={() => onQualityDragStart("fog", "Fog quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
              <ToggleRow
                label="Fog neighborhood clamp"
                checked={environment.fog.neighborhoodClamp}
                disabled={!ready}
                onCheckedChange={(next) =>
                  patchEnvironmentQuality("fog", { neighborhoodClamp: next }, "Fog quality")
                }
              />
              <FieldRow label="Fog light clamp">
                <NumberDrag
                  value={environment.fog.lightClamp}
                  min={0}
                  max={50}
                  step={0.1}
                  onChange={(value) =>
                    patchEnvironmentQuality("fog", { lightClamp: value }, "Fog quality")
                  }
                  onDragStart={() => onQualityDragStart("fog", "Fog quality")}
                  onDragEnd={onQualityDragEnd}
                />
              </FieldRow>
            </>
          ) : (
            <span className="text-[11px] text-muted-foreground">Loading environment quality…</span>
          )}

          <SectionBreak>Debug</SectionBreak>
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
