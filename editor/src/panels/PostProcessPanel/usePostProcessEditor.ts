import { useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { client } from "../../control/client";
import { invoke } from "../../shell";
import { useEditorStore } from "../../state/store";
import { identityChannels, type ToneCurveChannels } from "../../components/ToneCurve";
import { errorText, notifyError } from "../../lib/flash";
import type {
  GradeRangeDto,
  SetBloomParams,
  SetColorGradingParams,
  SplitToneDto,
} from "../../protocol";
import {
  CURVE_LUT_SIZE,
  IDENTITY_MIXER,
  NEUTRAL_RANGE,
  NEUTRAL_SPLIT,
  curveToCube,
  vec3Equal,
  type ChannelMixer,
  type RangeKey,
  type Triplet,
} from "./grading";
import {
  applyOptimisticRenderStats as optimistic,
  recordRenderEdit as recordRender,
} from "../../lib/renderSettings";
import { useFullSetWriter } from "./useFullSetWriter";

/// Every write path the panel's three tabs drive: the shallow-selected value subset, the optimistic
/// fold, and the gesture-coalesced bloom / grade / tone-curve writers.
export function usePostProcessEditor() {
  const ready = useEditorStore((s) => s.engineStatus.phase === "ready");
  const hasStats = useEditorStore((s) => s.renderStats !== null);
  const setDragActive = useEditorStore((s) => s.setDragActive);
  const [tab, setTab] = useState<"tone" | "color" | "effects">("tone");
  const [mixerOutput, setMixerOutput] = useState(0);
  const [curve, setCurve] = useState<ToneCurveChannels>(identityChannels);

  const cfg = useEditorStore(
    useShallow((s) => {
      const r = s.renderStats;
      const g = r?.colorGrading;
      return {
        tonemap: r?.tonemap ?? "aces",
        exposureEv: r?.exposureEv ?? 0,
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

  const onTonemap = (mode: string): void => {
    const prior = useEditorStore.getState().renderStats?.tonemap ?? "aces";
    optimistic({ tonemap: mode });
    if (prior !== mode) {
      recordRender(
        "View transform",
        () => client.setTonemap(prior as "aces"),
        () => client.setTonemap(mode as "aces"),
      );
    }
    void client
      .setTonemap(mode as "aces")
      .then((res) => optimistic({ tonemap: res.mode }))
      .catch((err: unknown) => notifyError(errorText(err)));
  };

  const exposurePrior = useRef<number | null>(null);
  const onExposureDragStart = (): void => {
    exposurePrior.current = useEditorStore.getState().renderStats?.exposureEv ?? 0;
    setDragActive(true);
  };
  const onExposureDragEnd = (): void => {
    setDragActive(false);
    const prior = exposurePrior.current;
    exposurePrior.current = null;
    if (prior === null) return;
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
  const {
    write: writeBloom,
    onDragStart: onBloomDragStart,
    onDragEnd: onBloomDragEnd,
  } = useFullSetWriter<SetBloomParams>({
    label: "Bloom",
    from: bloomFrom,
    equal: bloomEqual,
    apply: applyBloom,
    send: (params) => client.setBloom(params),
    record: recordRender,
    setDragActive,
  });

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
  const {
    write: writeGrade,
    onDragStart: onGradeDragStart,
    onDragEnd: onGradeDragEnd,
    cancelGesture: cancelGradeGesture,
  } = useFullSetWriter<SetColorGradingParams>({
    label: "Color grade",
    from: gradeFrom,
    equal: gradeEqual,
    apply: applyGrade,
    send: (params) => client.setColorGrading(params),
    record: recordRender,
    setDragActive,
  });
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
    // The bake below records the whole curve as one grade entry, so drop the gesture's own.
    cancelGradeGesture();
    void bakeCurve(curveRef.current);
  };

  return {
    ready,
    hasStats,
    cfg,
    tab,
    setTab,
    mixerOutput,
    setMixerOutput,
    curve,
    writeBloom,
    onBloomDragStart,
    onBloomDragEnd,
    writeGrade,
    onGradeDragStart,
    onGradeDragEnd,
    writeRange,
    writeMixerCoeff,
    writeSplit,
    onTonemap,
    onExposure,
    onExposureDragStart,
    onExposureDragEnd,
    onCurveChange,
    onCurveDragStart,
    onCurveDragEnd,
  };
}
