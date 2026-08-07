import { evalCurve, type ToneCurveChannels } from "../../components/ToneCurve";
import type { GradeRangeDto, SplitToneDto } from "../../protocol";

export type Triplet = [number, number, number];
export type ChannelMixer = [number, number, number, number, number, number, number, number, number];
export type RangeKey = "shadows" | "midtones" | "highlights";

/// The identity per-range correction (slope 1, offset 0, power 1, saturation/contrast 1).
export const NEUTRAL_RANGE: GradeRangeDto = {
  slope: [1, 1, 1],
  offset: [0, 0, 0],
  power: [1, 1, 1],
  saturation: 1,
  contrast: 1,
};
export const IDENTITY_MIXER: ChannelMixer = [1, 0, 0, 0, 1, 0, 0, 0, 1];
export const NEUTRAL_SPLIT: SplitToneDto = {
  shadow: [0.5, 0.5, 0.5],
  highlight: [0.5, 0.5, 0.5],
  balance: 0,
};
export const MIXER_OUTPUTS: { value: string; label: string }[] = [
  { value: "0", label: "Red" },
  { value: "1", label: "Green" },
  { value: "2", label: "Blue" },
];

export const TONEMAP_OPTIONS = [
  { value: "aces", label: "ACES" },
  { value: "agx", label: "AgX" },
  { value: "pbr-neutral", label: "PBR Neutral" },
  { value: "reinhard", label: "Reinhard" },
] as const;

/// The `.cube` the tone curve bakes to; a small 17³ table keeps the import cheap.
export const CURVE_LUT_SIZE = 17;

export const vec3Equal = (a: Triplet, b: Triplet): boolean =>
  a[0] === b[0] && a[1] === b[1] && a[2] === b[2];

/// Sample the per-channel tone curves into a red-fastest `.cube` text (per-channel then master).
export function curveToCube(channels: ToneCurveChannels, size: number): string {
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
