import { call } from "./call";
import type {
  BakeLookParams,
  BakeLookResult,
  MeshExecutorResult,
  RenderQualityResult,
  RenderStats,
  SetBloomParams,
  SetBloomResult,
  SetColorGradingParams,
  SetColorGradingResult,
} from "../../protocol";

/// Renderer feature toggles, the quality tier, and image formation.
export const renderCommands = {
  /// Anti-aliasing mode. Echoes `{ aa }`.
  setAa(mode: RenderStats["aa"]): Promise<{ aa: RenderStats["aa"] }> {
    return call("set-aa", { mode });
  },
  /// Debug render-output mode (read back via render-stats). Transient; not persisted. Echoes `{ viewMode }`.
  setViewMode(mode: RenderStats["viewMode"]): Promise<{ viewMode: RenderStats["viewMode"] }> {
    return call("set-view-mode", { mode });
  },
  /// Clustered (Forward+) light culling. Echoes `{ clustered }`.
  setClustered(on: boolean): Promise<{ clustered: boolean }> {
    return call("set-clustered", { enabled: on });
  },
  /// Image-based lighting. Echoes `{ ibl }`.
  setIbl(on: boolean): Promise<{ ibl: boolean }> {
    return call("set-ibl", { enabled: on });
  },
  /// The render-quality tier — the single knob for the SSGI / GTAO / contact-shadow stack.
  /// Echoes the resolved per-effect state.
  setRenderQuality(tier: string): Promise<RenderQualityResult> {
    return call("set-render-quality", { tier });
  },
  /// The active render-quality tier + resolved per-effect state.
  getRenderQuality(): Promise<RenderQualityResult> {
    return call("get-render-quality", {});
  },
  /// Reports the editor viewport visibility so the host idles a hidden view. `occluded` suppresses
  /// rendering entirely; `unfocused`/`focused` render on demand.
  setViewportPowerState(state: "focused" | "unfocused" | "occluded"): Promise<{ state: string }> {
    return call("set-viewport-power-state", { state });
  },
  /// The HDR→display tonemap operator. Echoes `{ mode }`.
  setTonemap(mode: "reinhard" | "aces" | "agx" | "pbr-neutral"): Promise<{ mode: string }> {
    return call("set-tonemap", { mode });
  },
  /// Shadow pass. Echoes `{ shadows }`.
  setShadows(on: boolean): Promise<{ shadows: boolean }> {
    return call("set-shadows", { enabled: on });
  },
  /// Global-illumination mode (`off` | `ddgi`). Echoes `{ ddgi }`.
  setGi(mode: "off" | "ddgi"): Promise<{ ddgi: boolean }> {
    return call("set-gi", { mode });
  },
  /// Depth pre-pass. Echoes `{ depthPrepass }`.
  setDepthPrepass(on: boolean): Promise<{ depthPrepass: boolean }> {
    return call("set-depth-prepass", { enabled: on });
  },
  /// Ray-traced shadows. Rejects with the typed error when ray tracing is
  /// unsupported on the device; echoes `{ rtShadows }` otherwise.
  setRtShadows(on: boolean): Promise<{ rtShadows: boolean }> {
    return call("set-rt-shadows", { enabled: on });
  },
  /// ReSTIR. Rejects with the typed error when ray tracing is unsupported;
  /// echoes `{ restir }` otherwise.
  setRestir(on: boolean): Promise<{ restir: boolean }> {
    return call("set-restir", { enabled: on });
  },
  /// Screen-space reflections (sharp mirror reflections on smooth surfaces).
  setSsr(on: boolean): Promise<{ ssr: boolean }> {
    return call("set-ssr", { enabled: on });
  },
  /// Ray-traced reflections (off-screen-aware). Rejects with the typed error
  /// when ray tracing is unsupported; echoes `{ rtReflections }` otherwise.
  setRtReflections(on: boolean): Promise<{ rtReflections: boolean }> {
    return call("set-rt-reflections", { enabled: on });
  },
  /// Routes the shaded executor through the mesh stage or the indexed path. `supported` reports
  /// whether the device qualifies at all; asking for the mesh stage on one that does not leaves
  /// `enabled` false rather than rejecting. Omit the argument to read which executor is in force.
  setMeshExecutor(enabled?: boolean): Promise<MeshExecutorResult> {
    return call("set-mesh-executor", enabled === undefined ? {} : { enabled });
  },
  /// Tonemap exposure in stops (exp2). This is the EFFECTIVE exposure; the env's
  /// `exposure` field is reserved on the wire. Echoes `{ exposureEv }`.
  setExposure(ev: number): Promise<{ exposureEv: number }> {
    return call("set-exposure", { ev });
  },
  /// Pre-tonemap scene-linear bloom pyramid: enable + energy-conserving intensity, tent scatter,
  /// tint, and the (default-off) soft-knee threshold. Echoes the applied state.
  setBloom(params: SetBloomParams): Promise<SetBloomResult> {
    return call("set-bloom", params);
  },
  /// Scene-linear color grade folded into the tonemap pass before the view/display transform: white
  /// balance (Temp/Tint), contrast around the 0.18 pivot, saturation, and the canonical ASC-CDL
  /// slope/offset/power. Echoes the applied grade.
  setColorGrading(params: SetColorGradingParams): Promise<SetColorGradingResult> {
    return call("set-color-grading", params);
  },
  /// Bake the current grade + view transform + creative LUT into one 33³ log2-shaper `.slut` for the
  /// exported player, registering it as a LUT asset. Returns the baked asset id / path / size.
  bakeLook(params: BakeLookParams = {}): Promise<BakeLookResult> {
    return call("bake-look", params);
  },
};
