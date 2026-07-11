/// The Post panel: the project's pre-tonemap post-processing — the scene-linear bloom pyramid and the
/// scene-linear color grade folded into the tonemap pass. It sections **Bloom** and **Grade** as the
/// two top-level tabs; Grade sub-sections (Global / Shadows / Midtones / Highlights / mixer / split /
/// look) use a Separator + uppercase label. Like Render and Environment these persist with the project
/// (`renderSettings`), so this panel lives beside them and is scene-tab undoable.
///
/// Values are read from a shallow-selected subset of `renderStats` so the body re-renders only when a
/// bloom/grade field changes, never on the 20 Hz stats poll. Writes optimistically fold in the new
/// value (and the echoed result); a scrub gesture records one undo entry and gates the poll via
/// `dragActive`. Bloom scalars go through `client.setBloom`, grade fields through
/// `client.setColorGrading` — the same commands the CLI drives.
import { useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { client } from "../control/client";
import { invoke } from "../shell";
import { useEditorStore } from "../state/store";
import { NumberDrag } from "../components/NumberDrag";
import { ColorField } from "../components/ColorField";
import { SliderField } from "../components/SliderField";
import { GradingWheel } from "../components/GradingWheel";
import {
  ToneCurve,
  identityChannels,
  evalCurve,
  type ToneCurveChannels,
} from "../components/ToneCurve";
import { errorText, notify, notifyError } from "../lib/flash";
import { ASSET_DND_MIME, assetIdsFromPayload, readAssetPayload } from "../components/AssetTile";
import type {
  GradeRangeDto,
  RenderStats,
  SetBloomParams,
  SetColorGradingParams,
  SplitToneDto,
} from "../protocol";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

type Triplet = [number, number, number];
type ChannelMixer = [number, number, number, number, number, number, number, number, number];
type RangeKey = "shadows" | "midtones" | "highlights";

/// The identity per-range correction (slope 1, offset 0, power 1, saturation/contrast 1).
const NEUTRAL_RANGE: GradeRangeDto = {
  slope: [1, 1, 1],
  offset: [0, 0, 0],
  power: [1, 1, 1],
  saturation: 1,
  contrast: 1,
};
const IDENTITY_MIXER: ChannelMixer = [1, 0, 0, 0, 1, 0, 0, 0, 1];
const NEUTRAL_SPLIT: SplitToneDto = {
  shadow: [0.5, 0.5, 0.5],
  highlight: [0.5, 0.5, 0.5],
  balance: 0,
};
const MIXER_OUTPUTS: { value: string; label: string }[] = [
  { value: "0", label: "Red" },
  { value: "1", label: "Green" },
  { value: "2", label: "Blue" },
];

/// The `.cube` the tone curve bakes to; a small 17³ table keeps the import cheap.
const CURVE_LUT_SIZE = 17;

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mt-1 border-t border-border pt-2.5">
      <Label className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
        {children}
      </Label>
    </div>
  );
}

function FieldRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[1fr_120px] items-center gap-1.5">
      <Label className="truncate text-[11px] font-normal text-muted-foreground">{label}</Label>
      {children}
    </div>
  );
}

/// A row of Lift/Gamma/Gain trackballs over one CDL block (global or a tonal range). Lift patches
/// `offset` (neutral 0), Gamma `power`, Gain `slope` (both neutral 1) — the canonical Resolve mapping.
function CdlWheels({
  cdl,
  onPatch,
  onDragStart,
  onDragEnd,
}: {
  cdl: { slope: Triplet; offset: Triplet; power: Triplet };
  onPatch(patch: { slope?: Triplet; offset?: Triplet; power?: Triplet }): void;
  onDragStart(): void;
  onDragEnd(): void;
}) {
  return (
    <div className="flex items-start justify-around gap-1 py-1">
      <GradingWheel
        label="Lift"
        value={cdl.offset}
        neutral={0}
        chromaRange={0.15}
        masterMin={-0.25}
        masterMax={0.25}
        onChange={(rgb) => onPatch({ offset: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <GradingWheel
        label="Gamma"
        value={cdl.power}
        neutral={1}
        chromaRange={0.3}
        masterMin={0.25}
        masterMax={2}
        onChange={(rgb) => onPatch({ power: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      <GradingWheel
        label="Gain"
        value={cdl.slope}
        neutral={1}
        chromaRange={0.3}
        masterMin={0}
        masterMax={2}
        onChange={(rgb) => onPatch({ slope: rgb })}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
    </div>
  );
}

const vec3Equal = (a: Triplet, b: Triplet): boolean =>
  a[0] === b[0] && a[1] === b[1] && a[2] === b[2];

/// Sample the per-channel tone curves into a red-fastest `.cube` text (per-channel then master).
function curveToCube(channels: ToneCurveChannels, size: number): string {
  let text = `TITLE "tone-curve"\nLUT_3D_SIZE ${size}\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n`;
  const step = (i: number): number => i / (size - 1);
  for (let b = 0; b < size; b += 1) {
    for (let g = 0; g < size; g += 1) {
      for (let r = 0; r < size; r += 1) {
        const or = evalCurve(channels.master, evalCurve(channels.r, step(r)));
        const og = evalCurve(channels.master, evalCurve(channels.g, step(g)));
        const ob = evalCurve(channels.master, evalCurve(channels.b, step(b)));
        text += `${or.toFixed(5)} ${og.toFixed(5)} ${ob.toFixed(5)}\n`;
      }
    }
  }
  return text;
}

export function PostProcessPanel() {
  const ready = useEditorStore((s) => s.engineStatus.phase === "ready");
  const hasStats = useEditorStore((s) => s.renderStats !== null);
  const setRenderStats = useEditorStore((s) => s.setRenderStats);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const [tab, setTab] = useState<"bloom" | "grade">("grade");
  const [mixerOutput, setMixerOutput] = useState(0);
  const [curve, setCurve] = useState<ToneCurveChannels>(identityChannels);

  const cfg = useEditorStore(
    useShallow((s) => {
      const r = s.renderStats;
      const g = r?.colorGrading;
      return {
        gradeTemperature: g?.temperature ?? 6500,
        gradeTint: g?.tint ?? 0,
        gradeContrast: g?.contrast ?? 1,
        gradeSaturation: g?.saturation ?? 1,
        gradeSlope: (g?.slope ?? [1, 1, 1]) as Triplet,
        gradeOffset: (g?.offset ?? [0, 0, 0]) as Triplet,
        gradePower: (g?.power ?? [1, 1, 1]) as Triplet,
        gradeShadows: g?.shadows ?? NEUTRAL_RANGE,
        gradeMidtones: g?.midtones ?? NEUTRAL_RANGE,
        gradeHighlights: g?.highlights ?? NEUTRAL_RANGE,
        gradeShadowsMax: g?.shadowsMax ?? 0.09,
        gradeHighlightsMin: g?.highlightsMin ?? 0.5,
        gradeChannelMixer: (g?.channelMixer ?? IDENTITY_MIXER) as ChannelMixer,
        gradeSplitTone: g?.splitTone ?? NEUTRAL_SPLIT,
        creativeLutAsset: g?.creativeLutAsset ?? "0",
        creativeLutIntensity: g?.creativeLutIntensity ?? 0,
        creativeLutSize: r?.creativeLut?.size ?? 0,
        bloomEnabled: r?.bloomEnabled ?? false,
        bloomIntensity: r?.bloomIntensity ?? 0.05,
        bloomScatter: r?.bloomScatter ?? 0.005,
        bloomTint: (r?.bloomTint ?? [1, 1, 1]) as Triplet,
        bloomDirtIntensity: r?.bloomDirtIntensity ?? 0,
        bloomDirtTint: (r?.bloomDirtTint ?? [1, 1, 1]) as Triplet,
        bloomAnamorphicEnabled: r?.bloomAnamorphic?.enabled ?? false,
        bloomAnamorphicRatio: r?.bloomAnamorphic?.ratio ?? 2,
        bloomAnamorphicIntensity: r?.bloomAnamorphic?.intensity ?? 0,
        bloomAnamorphicTint: (r?.bloomAnamorphic?.tint ?? [0.6, 0.8, 1]) as Triplet,
      };
    }),
  );

  const optimistic = (patch: Partial<RenderStats>): void => {
    const cur = useEditorStore.getState().renderStats;
    if (cur) {
      setRenderStats({ ...cur, ...patch });
    }
  };

  const recordRender = (
    label: string,
    undo: () => Promise<unknown>,
    redo: () => Promise<unknown>,
  ): void => {
    useEditorStore.getState().pushEdit({ label, undo, redo }, "scene");
  };

  // Bloom is one command over its fields; each gesture reads the current state and writes the full
  // param set, folding the echoed result optimistically. A scrub captures the prior at drag start and
  // records once at drag end; a toggle records inline.
  const bloomFrom = (patch: Partial<SetBloomParams>): SetBloomParams => {
    const r = useEditorStore.getState().renderStats;
    return {
      enabled: r?.bloomEnabled ?? false,
      intensity: r?.bloomIntensity ?? 0.05,
      scatter: r?.bloomScatter ?? 0.005,
      tint: (r?.bloomTint ?? [1, 1, 1]) as Triplet,
      threshold: r?.bloomThreshold ?? 0,
      dirtIntensity: r?.bloomDirtIntensity ?? 0,
      dirtTint: (r?.bloomDirtTint ?? [1, 1, 1]) as Triplet,
      anamorphic: r?.bloomAnamorphic ?? {
        enabled: false,
        ratio: 2,
        tint: [0.6, 0.8, 1],
        intensity: 0,
      },
      perMipTint: r?.bloomPerMipTint ?? [],
      ...patch,
    };
  };
  const bloomEqual = (a: SetBloomParams, b: SetBloomParams): boolean =>
    a.enabled === b.enabled &&
    a.intensity === b.intensity &&
    a.scatter === b.scatter &&
    a.threshold === b.threshold &&
    vec3Equal(a.tint, b.tint) &&
    a.dirtIntensity === b.dirtIntensity &&
    vec3Equal(a.dirtTint ?? [1, 1, 1], b.dirtTint ?? [1, 1, 1]) &&
    a.anamorphic?.enabled === b.anamorphic?.enabled &&
    a.anamorphic?.ratio === b.anamorphic?.ratio &&
    a.anamorphic?.intensity === b.anamorphic?.intensity &&
    vec3Equal(a.anamorphic?.tint ?? [0.6, 0.8, 1], b.anamorphic?.tint ?? [0.6, 0.8, 1]);
  const applyBloom = (params: SetBloomParams): void => {
    optimistic({
      bloomEnabled: params.enabled,
      bloomIntensity: params.intensity,
      bloomScatter: params.scatter,
      bloomTint: params.tint,
      bloomThreshold: params.threshold,
      bloomDirtIntensity: params.dirtIntensity,
      bloomDirtTint: params.dirtTint,
      bloomAnamorphic: params.anamorphic,
    });
    void client
      .setBloom(params)
      .then((res) =>
        optimistic({
          bloomEnabled: res.enabled,
          bloomIntensity: res.intensity,
          bloomScatter: res.scatter,
          bloomTint: res.tint,
          bloomThreshold: res.threshold,
          bloomDirtIntensity: res.dirtIntensity,
          bloomDirtTint: res.dirtTint,
          bloomAnamorphic: res.anamorphic,
        }),
      )
      .catch((err: unknown) => notifyError(errorText(err)));
  };
  const bloomPrior = useRef<SetBloomParams | null>(null);
  const onBloomDragStart = (): void => {
    bloomPrior.current = bloomFrom({});
    setDragActive(true);
  };
  const onBloomDragEnd = (): void => {
    setDragActive(false);
    const prior = bloomPrior.current;
    bloomPrior.current = null;
    if (!prior) {
      return;
    }
    const after = bloomFrom({});
    if (!bloomEqual(prior, after)) {
      recordRender(
        "Bloom",
        () => client.setBloom(prior),
        () => client.setBloom(after),
      );
    }
  };
  const writeBloom = (patch: Partial<SetBloomParams>): void => {
    if (bloomPrior.current === null) {
      const prior = bloomFrom({});
      const after = bloomFrom(patch);
      if (!bloomEqual(prior, after)) {
        recordRender(
          "Bloom",
          () => client.setBloom(prior),
          () => client.setBloom(after),
        );
      }
      applyBloom(after);
    } else {
      applyBloom(bloomFrom(patch));
    }
  };

  // The scene-linear grade is one command over its flat fields; each gesture reads the current grade
  // and writes the full set, folding the echo optimistically. CDL rides the wire as slope/offset/power;
  // the panel surfaces it as the Lift/Gamma/Gain trackballs (a display renaming).
  const gradeFrom = (patch: Partial<SetColorGradingParams>): SetColorGradingParams => {
    const g = useEditorStore.getState().renderStats?.colorGrading;
    return {
      temperature: g?.temperature ?? 6500,
      tint: g?.tint ?? 0,
      contrast: g?.contrast ?? 1,
      pivot: g?.pivot ?? 0.18,
      saturation: g?.saturation ?? 1,
      slope: (g?.slope ?? [1, 1, 1]) as Triplet,
      offset: (g?.offset ?? [0, 0, 0]) as Triplet,
      power: (g?.power ?? [1, 1, 1]) as Triplet,
      shadows: g?.shadows ?? NEUTRAL_RANGE,
      midtones: g?.midtones ?? NEUTRAL_RANGE,
      highlights: g?.highlights ?? NEUTRAL_RANGE,
      shadowsMax: g?.shadowsMax ?? 0.09,
      highlightsMin: g?.highlightsMin ?? 0.5,
      channelMixer: (g?.channelMixer ?? IDENTITY_MIXER) as ChannelMixer,
      splitTone: g?.splitTone ?? NEUTRAL_SPLIT,
      creativeLutAsset: g?.creativeLutAsset ?? "0",
      creativeLutIntensity: g?.creativeLutIntensity ?? 0,
      ...patch,
    };
  };
  const rangeEqual = (a: GradeRangeDto, b: GradeRangeDto): boolean =>
    vec3Equal(a.slope, b.slope) &&
    vec3Equal(a.offset, b.offset) &&
    vec3Equal(a.power, b.power) &&
    a.saturation === b.saturation &&
    a.contrast === b.contrast;
  const gradeEqual = (a: SetColorGradingParams, b: SetColorGradingParams): boolean =>
    a.temperature === b.temperature &&
    a.tint === b.tint &&
    a.contrast === b.contrast &&
    a.pivot === b.pivot &&
    a.saturation === b.saturation &&
    vec3Equal(a.slope, b.slope) &&
    vec3Equal(a.offset, b.offset) &&
    vec3Equal(a.power, b.power) &&
    rangeEqual(a.shadows, b.shadows) &&
    rangeEqual(a.midtones, b.midtones) &&
    rangeEqual(a.highlights, b.highlights) &&
    a.shadowsMax === b.shadowsMax &&
    a.highlightsMin === b.highlightsMin &&
    a.channelMixer.every((v, i) => v === b.channelMixer[i]) &&
    vec3Equal(a.splitTone.shadow, b.splitTone.shadow) &&
    vec3Equal(a.splitTone.highlight, b.splitTone.highlight) &&
    a.splitTone.balance === b.splitTone.balance &&
    a.creativeLutAsset === b.creativeLutAsset &&
    a.creativeLutIntensity === b.creativeLutIntensity;
  const applyGrade = (params: SetColorGradingParams): void => {
    optimistic({ colorGrading: params });
    void client
      .setColorGrading(params)
      .then((res) => optimistic({ colorGrading: res }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };
  const gradePrior = useRef<SetColorGradingParams | null>(null);
  const onGradeDragStart = (): void => {
    gradePrior.current = gradeFrom({});
    setDragActive(true);
  };
  const onGradeDragEnd = (): void => {
    setDragActive(false);
    const prior = gradePrior.current;
    gradePrior.current = null;
    if (!prior) {
      return;
    }
    const after = gradeFrom({});
    if (!gradeEqual(prior, after)) {
      recordRender(
        "Color grade",
        () => client.setColorGrading(prior),
        () => client.setColorGrading(after),
      );
    }
  };
  const writeGrade = (patch: Partial<SetColorGradingParams>): void => {
    if (gradePrior.current === null) {
      const prior = gradeFrom({});
      const after = gradeFrom(patch);
      if (!gradeEqual(prior, after)) {
        recordRender(
          "Color grade",
          () => client.setColorGrading(prior),
          () => client.setColorGrading(after),
        );
      }
      applyGrade(after);
    } else {
      applyGrade(gradeFrom(patch));
    }
  };
  const writeRange = (key: RangeKey, patch: Partial<GradeRangeDto>): void => {
    const cur = gradeFrom({})[key];
    writeGrade({ [key]: { ...cur, ...patch } } as Partial<SetColorGradingParams>);
  };
  const writeMixerCoeff = (col: number, value: number): void => {
    const mixer = [...gradeFrom({}).channelMixer] as ChannelMixer;
    mixer[mixerOutput * 3 + col] = value;
    writeGrade({ channelMixer: mixer });
  };
  const writeSplit = (patch: Partial<SplitToneDto>): void => {
    writeGrade({ splitTone: { ...gradeFrom({}).splitTone, ...patch } });
  };

  // The tone curve authors a display-space per-channel spline that bakes into the creative-LUT slot:
  // sample it to a `.cube`, import it, and assign the resulting LUT asset through the same
  // `set-color-grading` grade merge (no separate curve param — the engine tail is unchanged). A bake
  // rides one grade gesture, so undo replays the whole curve as one entry.
  const curveRef = useRef<ToneCurveChannels>(curve);
  curveRef.current = curve;
  const bakePending = useRef(false);
  const bakeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const bakeCurve = async (channels: ToneCurveChannels): Promise<void> => {
    if (bakePending.current) {
      return;
    }
    bakePending.current = true;
    try {
      const info = await client.appDataInfo();
      const path = `${info.userdataDir}/tone-curve.cube`;
      const bytes = Array.from(new TextEncoder().encode(curveToCube(channels, CURVE_LUT_SIZE)));
      await invoke("write_file", { path, bytes });
      const { lut } = await client.importLut(path);
      const intensity = gradeFrom({}).creativeLutIntensity > 0 ? undefined : 1;
      writeGrade(
        intensity === undefined
          ? { creativeLutAsset: lut }
          : { creativeLutAsset: lut, creativeLutIntensity: intensity },
      );
    } catch (err) {
      notifyError(errorText(err));
    } finally {
      bakePending.current = false;
    }
  };
  const scheduleBake = (): void => {
    if (bakeTimer.current !== null) {
      clearTimeout(bakeTimer.current);
    }
    bakeTimer.current = setTimeout(() => {
      bakeTimer.current = null;
      void bakeCurve(curveRef.current);
    }, 150);
  };
  const gesturingCurve = useRef(false);
  const onCurveChange = (channels: ToneCurveChannels): void => {
    setCurve(channels);
    curveRef.current = channels;
    // A discrete edit (point removal outside a drag) bakes on a short debounce; a drag bakes at end.
    if (!gesturingCurve.current) {
      scheduleBake();
    }
  };
  const onCurveDragStart = (): void => {
    gesturingCurve.current = true;
    onGradeDragStart();
  };
  const onCurveDragEnd = (): void => {
    gesturingCurve.current = false;
    setDragActive(false);
    gradePrior.current = null;
    void bakeCurve(curveRef.current);
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

  const renderRange = (title: string, key: RangeKey, r: GradeRangeDto) => (
    <>
      <SectionLabel>{title}</SectionLabel>
      <CdlWheels
        cdl={r}
        onPatch={(patch) => writeRange(key, patch)}
        onDragStart={onGradeDragStart}
        onDragEnd={onGradeDragEnd}
      />
      <FieldRow label="Saturation">
        <NumberDrag
          value={r.saturation}
          min={0}
          max={2}
          step={0.01}
          onChange={(v) => writeRange(key, { saturation: v })}
          onDragStart={onGradeDragStart}
          onDragEnd={onGradeDragEnd}
        />
      </FieldRow>
      <FieldRow label="Contrast">
        <NumberDrag
          value={r.contrast}
          min={0}
          max={2}
          step={0.01}
          onChange={(v) => writeRange(key, { contrast: v })}
          onDragStart={onGradeDragStart}
          onDragEnd={onGradeDragEnd}
        />
      </FieldRow>
    </>
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <Tabs
        value={tab}
        onValueChange={(v) => setTab(v as "bloom" | "grade")}
        className="flex min-h-0 flex-1 flex-col gap-0"
      >
        <div className="p-2.5 pb-1.5">
          <TabsList>
            <TabsTrigger value="bloom">Bloom</TabsTrigger>
            <TabsTrigger value="grade">Grade</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="bloom" className="min-h-0">
          <ScrollArea className="h-full min-h-0">
            <div className="flex flex-col gap-2 p-2.5 pt-1">
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Bloom
                </Label>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <div>
                      <Switch
                        checked={cfg.bloomEnabled}
                        disabled={!ready}
                        onCheckedChange={(next) => writeBloom({ enabled: next })}
                      />
                    </div>
                  </TooltipTrigger>
                  <TooltipContent>
                    Energy-conserving scene-linear glow, composited before the tonemap
                  </TooltipContent>
                </Tooltip>
              </div>

              <FieldRow label="Intensity">
                <NumberDrag
                  value={cfg.bloomIntensity}
                  min={0}
                  max={1}
                  step={0.005}
                  onChange={(v) => writeBloom({ intensity: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>

              <FieldRow label="Scatter">
                <NumberDrag
                  value={cfg.bloomScatter}
                  min={0}
                  max={0.05}
                  step={0.0005}
                  onChange={(v) => writeBloom({ scatter: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>

              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Tint
                </Label>
                <ColorField
                  kind="color3"
                  value={{ x: cfg.bloomTint[0], y: cfg.bloomTint[1], z: cfg.bloomTint[2] }}
                  onChange={(patch) =>
                    writeBloom({
                      tint: [
                        patch.x ?? cfg.bloomTint[0],
                        patch.y ?? cfg.bloomTint[1],
                        patch.z ?? cfg.bloomTint[2],
                      ],
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </div>

              <SectionLabel>Lens dirt</SectionLabel>
              <FieldRow label="Intensity">
                <NumberDrag
                  value={cfg.bloomDirtIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeBloom({ dirtIntensity: v })}
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Tint
                </Label>
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.bloomDirtTint[0],
                    y: cfg.bloomDirtTint[1],
                    z: cfg.bloomDirtTint[2],
                  }}
                  onChange={(patch) =>
                    writeBloom({
                      dirtTint: [
                        patch.x ?? cfg.bloomDirtTint[0],
                        patch.y ?? cfg.bloomDirtTint[1],
                        patch.z ?? cfg.bloomDirtTint[2],
                      ],
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </div>

              <SectionLabel>Anamorphic streak</SectionLabel>
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Enabled
                </Label>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <div>
                      <Switch
                        checked={cfg.bloomAnamorphicEnabled}
                        disabled={!ready}
                        onCheckedChange={(next) =>
                          writeBloom({
                            anamorphic: {
                              enabled: next,
                              ratio: cfg.bloomAnamorphicRatio,
                              tint: cfg.bloomAnamorphicTint,
                              intensity: cfg.bloomAnamorphicIntensity,
                            },
                          })
                        }
                      />
                    </div>
                  </TooltipTrigger>
                  <TooltipContent>
                    A horizontally-squeezed streak (Wronski) added over the radial bloom
                  </TooltipContent>
                </Tooltip>
              </div>
              <FieldRow label="Streak ratio">
                <NumberDrag
                  value={cfg.bloomAnamorphicRatio}
                  min={1}
                  max={4}
                  step={0.05}
                  onChange={(v) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: v,
                        tint: cfg.bloomAnamorphicTint,
                        intensity: cfg.bloomAnamorphicIntensity,
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <FieldRow label="Streak intensity">
                <NumberDrag
                  value={cfg.bloomAnamorphicIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: cfg.bloomAnamorphicRatio,
                        tint: cfg.bloomAnamorphicTint,
                        intensity: v,
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </FieldRow>
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Streak tint
                </Label>
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.bloomAnamorphicTint[0],
                    y: cfg.bloomAnamorphicTint[1],
                    z: cfg.bloomAnamorphicTint[2],
                  }}
                  onChange={(patch) =>
                    writeBloom({
                      anamorphic: {
                        enabled: cfg.bloomAnamorphicEnabled,
                        ratio: cfg.bloomAnamorphicRatio,
                        intensity: cfg.bloomAnamorphicIntensity,
                        tint: [
                          patch.x ?? cfg.bloomAnamorphicTint[0],
                          patch.y ?? cfg.bloomAnamorphicTint[1],
                          patch.z ?? cfg.bloomAnamorphicTint[2],
                        ],
                      },
                    })
                  }
                  onDragStart={onBloomDragStart}
                  onDragEnd={onBloomDragEnd}
                />
              </div>
            </div>
          </ScrollArea>
        </TabsContent>

        <TabsContent value="grade" className="min-h-0">
          <ScrollArea className="h-full min-h-0">
            <div className="flex flex-col gap-2 p-2.5 pt-1">
              <SectionLabel>Global</SectionLabel>
              <FieldRow label="Temperature">
                <NumberDrag
                  value={cfg.gradeTemperature}
                  min={1667}
                  max={25000}
                  step={50}
                  onChange={(v) => writeGrade({ temperature: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Tint">
                <NumberDrag
                  value={cfg.gradeTint}
                  min={-1}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ tint: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Contrast">
                <NumberDrag
                  value={cfg.gradeContrast}
                  min={0}
                  max={2}
                  step={0.01}
                  onChange={(v) => writeGrade({ contrast: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Saturation">
                <NumberDrag
                  value={cfg.gradeSaturation}
                  min={0}
                  max={2}
                  step={0.01}
                  onChange={(v) => writeGrade({ saturation: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <CdlWheels
                cdl={{ slope: cfg.gradeSlope, offset: cfg.gradeOffset, power: cfg.gradePower }}
                onPatch={(patch) => writeGrade(patch)}
                onDragStart={onGradeDragStart}
                onDragEnd={onGradeDragEnd}
              />

              <FieldRow label="Shadows max">
                <SliderField
                  value={cfg.gradeShadowsMax}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ shadowsMax: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>
              <FieldRow label="Highlights min">
                <SliderField
                  value={cfg.gradeHighlightsMin}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ highlightsMin: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              {renderRange("Shadows", "shadows", cfg.gradeShadows)}
              {renderRange("Midtones", "midtones", cfg.gradeMidtones)}
              {renderRange("Highlights", "highlights", cfg.gradeHighlights)}

              <SectionLabel>Channel mixer</SectionLabel>
              <FieldRow label="Output">
                <Select
                  value={String(mixerOutput)}
                  onValueChange={(v) => setMixerOutput(Number(v))}
                >
                  <SelectTrigger className="h-7 text-[11px]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MIXER_OUTPUTS.map((o) => (
                      <SelectItem key={o.value} value={o.value} className="text-[11px]">
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FieldRow>
              {(["Red", "Green", "Blue"] as const).map((label, col) => (
                <FieldRow key={label} label={label}>
                  <NumberDrag
                    value={cfg.gradeChannelMixer[mixerOutput * 3 + col]}
                    min={-2}
                    max={2}
                    step={0.01}
                    onChange={(v) => writeMixerCoeff(col, v)}
                    onDragStart={onGradeDragStart}
                    onDragEnd={onGradeDragEnd}
                  />
                </FieldRow>
              ))}

              <SectionLabel>Split tone</SectionLabel>
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Shadow
                </Label>
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.gradeSplitTone.shadow[0],
                    y: cfg.gradeSplitTone.shadow[1],
                    z: cfg.gradeSplitTone.shadow[2],
                  }}
                  onChange={(patch) =>
                    writeSplit({
                      shadow: [
                        patch.x ?? cfg.gradeSplitTone.shadow[0],
                        patch.y ?? cfg.gradeSplitTone.shadow[1],
                        patch.z ?? cfg.gradeSplitTone.shadow[2],
                      ],
                    })
                  }
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </div>
              <div className="grid grid-cols-[1fr_auto] items-center gap-1.5">
                <Label className="truncate text-[11px] font-normal text-muted-foreground">
                  Highlight
                </Label>
                <ColorField
                  kind="color3"
                  value={{
                    x: cfg.gradeSplitTone.highlight[0],
                    y: cfg.gradeSplitTone.highlight[1],
                    z: cfg.gradeSplitTone.highlight[2],
                  }}
                  onChange={(patch) =>
                    writeSplit({
                      highlight: [
                        patch.x ?? cfg.gradeSplitTone.highlight[0],
                        patch.y ?? cfg.gradeSplitTone.highlight[1],
                        patch.z ?? cfg.gradeSplitTone.highlight[2],
                      ],
                    })
                  }
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </div>
              <FieldRow label="Balance">
                <NumberDrag
                  value={cfg.gradeSplitTone.balance}
                  min={-1}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeSplit({ balance: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              <SectionLabel>Creative look</SectionLabel>
              <div
                className="flex items-center justify-between gap-2 rounded border border-dashed border-border bg-muted/40 px-2 py-2 text-[11px] text-muted-foreground"
                onDragOver={(e) => {
                  if (e.dataTransfer.types.includes(ASSET_DND_MIME)) {
                    e.preventDefault();
                    e.dataTransfer.dropEffect = "copy";
                  }
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  const ids = assetIdsFromPayload(readAssetPayload(e.dataTransfer));
                  const id = ids[0];
                  if (id !== undefined) {
                    writeGrade({ creativeLutAsset: id });
                  }
                }}
              >
                <span className="truncate">
                  {cfg.creativeLutAsset !== "0"
                    ? `${cfg.creativeLutSize > 0 ? `${cfg.creativeLutSize}³` : "LUT"} · Tetrahedral`
                    : "Drop a .cube look"}
                </span>
                {cfg.creativeLutAsset !== "0" && (
                  <button
                    type="button"
                    className="text-[11px] text-muted-foreground hover:text-foreground"
                    onClick={() => writeGrade({ creativeLutAsset: "0" })}
                  >
                    Clear
                  </button>
                )}
              </div>
              <FieldRow label="Look intensity">
                <SliderField
                  value={cfg.creativeLutIntensity}
                  min={0}
                  max={1}
                  step={0.01}
                  onChange={(v) => writeGrade({ creativeLutIntensity: v })}
                  onDragStart={onGradeDragStart}
                  onDragEnd={onGradeDragEnd}
                />
              </FieldRow>

              <SectionLabel>Tone curve</SectionLabel>
              <ToneCurve
                channels={curve}
                onChange={onCurveChange}
                onDragStart={onCurveDragStart}
                onDragEnd={onCurveDragEnd}
              />

              <div className="flex justify-end pt-1">
                <button
                  type="button"
                  disabled={!ready}
                  className="rounded border border-border px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-50"
                  onClick={() => {
                    void client
                      .bakeLook()
                      .then((res) => notify(`Baked look → ${res.path}`))
                      .catch((err: unknown) => notifyError(errorText(err)));
                  }}
                >
                  Bake look
                </button>
              </div>
            </div>
          </ScrollArea>
        </TabsContent>
      </Tabs>
    </div>
  );
}
