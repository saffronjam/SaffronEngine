import { call } from "./call";
import type {
  ApplyEnvironmentProfileParams,
  CommandParamsMap,
  Environment,
  EnvironmentProfileListDto,
  EnvironmentProfileSummaryDto,
  SampleWindResult,
  WindInteractionFieldResult,
} from "../../protocol";

/// Sky, atmosphere, fog, clouds, wind, and the world clock. Every setter is a server-side merge
/// over its block and echoes the full updated environment.
export const environmentCommands = {
  getEnvironment(): Promise<Environment> {
    return call("get-environment");
  },
  getEnvironmentDefaults(): Promise<Environment> {
    return call("get-environment-defaults");
  },
  listEnvironmentProfiles(): Promise<EnvironmentProfileListDto> {
    return call("list-environment-profiles");
  },
  saveEnvironmentProfile(name: string, folder?: string): Promise<EnvironmentProfileSummaryDto> {
    return call("save-environment-profile", { name, folder });
  },
  updateEnvironmentProfile(profile: string): Promise<EnvironmentProfileSummaryDto> {
    return call("update-environment-profile", { profile });
  },
  applyEnvironmentProfile(profile: ApplyEnvironmentProfileParams["profile"]): Promise<Environment> {
    return call("apply-environment-profile", { profile });
  },
  setEnvironment(env: Partial<Environment>): Promise<Environment> {
    return call("set-environment", env);
  },
  replaceEnvironment(env: Environment): Promise<Environment> {
    return call("set-environment", { json: env });
  },
  /// Merge atmosphere fields over the current environment's `atmosphere` block; the
  /// engine re-bakes the LUT chain next frame. Returns the full updated environment.
  setAtmosphere(atmosphere: Partial<Environment["atmosphere"]>): Promise<Environment> {
    return call("set-atmosphere", atmosphere);
  },
  /// Merge fog fields over the current environment's `fog` block; the height-fog composite picks
  /// them up next frame. Returns the full updated environment.
  setFog(fog: Partial<Environment["fog"]>): Promise<Environment> {
    return call("set-fog", fog);
  },
  /// Merge volumetric-cloud shape, lighting, and reconstruction fields over the current block.
  setClouds(cloud: Partial<Environment["cloud"]>): Promise<Environment> {
    return call("set-clouds", cloud);
  },
  /// Merge shared global wind fields over the current environment's wind block.
  /// Stages one push into the world interaction field, applied on the next frame.
  emitInteractionImpulse(params: CommandParamsMap["emit-interaction-impulse"]) {
    return call("emit-interaction-impulse", params);
  },
  setWind(wind: Partial<Environment["wind"]>): Promise<Environment> {
    return call("set-wind", wind);
  },
  /// The composed wind velocity at a world position, taken apart: the mean advection, the
  /// turbulence, its per-octave spectrum, and what every local source contributed there.
  sampleWind(params: CommandParamsMap["sample-wind"]): Promise<SampleWindResult> {
    return call("sample-wind", params);
  },
  /// One whole cascade of the world interaction field, reduced to a grid of block means.
  windInteractionField(
    params: CommandParamsMap["wind-interaction-field"],
  ): Promise<WindInteractionFieldResult> {
    return call("wind-interaction-field", params);
  },
  /// Merge calendar, ephemeris, playback, and appearance-curve fields over the scene's
  /// time-of-day block. Environment curves use point objects; the command wire uses tuples.
  setTimeOfDay(time: Partial<Environment["timeOfDay"]>): Promise<Environment> {
    const tupleCurve = (curve: { x: number; y: number }[]): [number, number][] =>
      curve.map(({ x, y }) => [x, y]);
    const params: CommandParamsMap["set-time-of-day"] = {
      ...time,
      exposureCurve: time.exposureCurve ? tupleCurve(time.exposureCurve) : undefined,
      tintCurve: time.tintCurve
        ? {
            master: tupleCurve(time.tintCurve.master),
            red: tupleCurve(time.tintCurve.red),
            green: tupleCurve(time.tintCurve.green),
            blue: tupleCurve(time.tintCurve.blue),
          }
        : undefined,
      coverageCurve: time.coverageCurve ? tupleCurve(time.coverageCurve) : undefined,
      cloudTypeCurve: time.cloudTypeCurve ? tupleCurve(time.cloudTypeCurve) : undefined,
    };
    return call("set-time-of-day", params);
  },
};
