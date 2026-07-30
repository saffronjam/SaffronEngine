// Analytic height-fog control-plane end-to-end: drive a real headless engine, merge the
// `set-fog` block over the wire, and assert the `EnvironmentDto` echo, the `get-environment`
// read-back, and a Vulkan-validation-clean log across the depth-read + in-place composite pass.

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import { prepareScene } from "./test-utils.ts";
import type {
  EntityRef,
  EnvironmentDto,
  InspectResult,
  RenderStatsDto,
  SetFogParams,
  SetViewModeResult,
} from "@saffron/protocol";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // Size the scene view small so the extra fullscreen fog dispatch stays cheap when the host
  // falls back to a software rasterizer.
  await prepareScene(engine);
  // A cube so the depth buffer carries a real near-surface (the closed-form term integrates over
  // reconstructed world position; the validation oracle is content-independent, this exercises it).
  await engine.call("add-entity", { preset: "cube" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("fog is disabled by default in get-environment", async () => {
  const env = await engine.call<EnvironmentDto>("get-environment");
  expect(env.fog.enabled).toBe(false);
});

test("set-fog echoes the merged fog block", async () => {
  const params: SetFogParams = {
    enabled: true,
    density: 0.05,
    heightFalloff: 0.35,
    directionalExponent: 16,
    maxOpacity: 0.8,
  };
  const env = await engine.call<EnvironmentDto>("set-fog", params);
  expect(env.fog.enabled).toBe(true);
  expect(env.fog.density).toBeCloseTo(0.05, 5);
  expect(env.fog.heightFalloff).toBeCloseTo(0.35, 5);
  expect(env.fog.directionalExponent).toBeCloseTo(16, 5);
  expect(env.fog.maxOpacity).toBeCloseTo(0.8, 5);
});

test("get-environment reflects the merged fog + runs validation-clean", async () => {
  await engine.settle(300);
  const env = await engine.call<EnvironmentDto>("get-environment");
  expect(env.fog.enabled).toBe(true);
  expect(env.fog.density).toBeCloseTo(0.05, 5);
  expect(engine.validationErrors()).toEqual([]);
});

// The composite is a fullscreen depth-read + in-place blend into the scene-linear HDR offscreen
// before bloom, plus an optional summed ground layer. Scrub the authored parameters and toggle a
// second layer on, asserting each change stays validation-clean.
describe("fog stays validation-clean across parameter changes", () => {
  const DENSITIES = [0.02, 0.1, 0.3];
  for (const density of DENSITIES) {
    test(`density=${density}`, async () => {
      await engine.call<EnvironmentDto>("set-fog", {
        enabled: true,
        density,
        heightFalloff: 0.2,
        layer2Density: 0.05,
        layer2Falloff: 0.5,
      });
      await engine.settle(200);
      expect(engine.validationErrors()).toEqual([]);
    });
  }

  test("disabling fog returns to a clean frame", async () => {
    const env = await engine.call<EnvironmentDto>("set-fog", { enabled: false });
    expect(env.fog.enabled).toBe(false);
    await engine.settle(200);
    const back = await engine.call<EnvironmentDto>("get-environment");
    expect(back.fog.enabled).toBe(false);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// The volumetric backend: `mode: "volumetric"` arms the froxel inject → integrate → composite
// passes (the analytic height density becomes the froxel base medium — never applied twice). Assert
// the `EnvironmentDto` echo carries the mode + froxel medium params and that the 3D-dispatch passes
// stay validation-clean.
describe("volumetric fog mode", () => {
  test("set-fog volumetric echoes the mode + medium params", async () => {
    const env = await engine.call<EnvironmentDto>("set-fog", {
      enabled: true,
      mode: "volumetric",
      baseDensity: 0.05,
      scatterAlbedo: 0.9,
      phaseG: 0.6,
    });
    expect(env.fog.enabled).toBe(true);
    expect(env.fog.mode).toBe("volumetric");
    expect(env.fog.baseDensity).toBeCloseTo(0.05, 5);
    expect(env.fog.scatterAlbedo).toBeCloseTo(0.9, 5);
    expect(env.fog.phaseG).toBeCloseTo(0.6, 5);
  });

  test("get-environment reflects volumetric mode + runs validation-clean", async () => {
    await engine.settle(300);
    const env = await engine.call<EnvironmentDto>("get-environment");
    expect(env.fog.mode).toBe("volumetric");
    expect(engine.validationErrors()).toEqual([]);
  });

  test("switching back to analytic is validation-clean", async () => {
    const env = await engine.call<EnvironmentDto>("set-fog", { mode: "analytic" });
    expect(env.fog.mode).toBe("analytic");
    await engine.settle(200);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// The fog debug view mode (`ViewMode::Fog`): the composite outputs the froxel in-scatter + opacity
// directly instead of compositing, so the froxel volume is inspectable over the existing view-mode
// path. This also exercises the forward transparent fog sample — the übershader now statically
// references the froxel integration volume (light set binding 11) on every mesh draw, so a
// validation-clean render under volumetric fog proves that descriptor is correctly wired.
describe("fog debug view mode", () => {
  test("set-view-mode fog echoes the mode + render-stats reflects it", async () => {
    await engine.call<EnvironmentDto>("set-fog", { enabled: true, mode: "volumetric" });
    const res = await engine.call<SetViewModeResult>("set-view-mode", { mode: "fog" });
    expect(res.viewMode).toBe("fog");
    const stats = await engine.call<RenderStatsDto>("render-stats");
    expect(stats.viewMode).toBe("fog");
  });

  test("the fog view renders volumetric fog validation-clean", async () => {
    await engine.settle(300);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("restoring the lit view is validation-clean", async () => {
    const res = await engine.call<SetViewModeResult>("set-view-mode", { mode: "lit" });
    expect(res.viewMode).toBe("lit");
    await engine.settle(200);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// Temporal reprojection, quality tiers, and per-light volumetric controls. The grid
// quality tier reallocates the ping-pong history + integration volumes and rebinds the composite
// sample; the temporal knobs ride the same `set-fog` merge; and the per-light fields round-trip
// through the generic `set-component-field` / `inspect` with no per-command code.
describe("volumetric quality tiers + temporal knobs", () => {
  for (const quality of ["low", "medium", "high"] as const) {
    test(`set-fog quality=${quality} echoes + reallocates validation-clean`, async () => {
      const env = await engine.call<EnvironmentDto>("set-fog", {
        enabled: true,
        mode: "volumetric",
        quality,
      });
      expect(env.fog.quality).toBe(quality);
      await engine.settle(250);
      expect(engine.validationErrors()).toEqual([]);
    });
  }

  test("temporal reprojection knobs echo + stay validation-clean", async () => {
    const env = await engine.call<EnvironmentDto>("set-fog", {
      enabled: true,
      mode: "volumetric",
      historyBlend: 0.08,
      neighborhoodClamp: true,
      lightClamp: 4,
    });
    expect(env.fog.historyBlend).toBeCloseTo(0.08, 5);
    expect(env.fog.neighborhoodClamp).toBe(true);
    expect(env.fog.lightClamp).toBeCloseTo(4, 5);
    await engine.settle(250);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("an out-of-range historyBlend is rejected", async () => {
    await expect(engine.call<EnvironmentDto>("set-fog", { historyBlend: 2 })).rejects.toThrow();
  });
});

describe("per-light volumetric controls", () => {
  let lightId = "";

  test("a point light exposes the volumetric fields with defaults", async () => {
    const ref = await engine.call<EntityRef>("add-entity", { preset: "point-light" });
    lightId = ref.id;
    const info = await engine.call<InspectResult>("inspect", { entity: lightId });
    const light = info.components.PointLight as
      | { volumetricScattering?: number; castVolumetricShadow?: boolean }
      | undefined;
    expect(light?.volumetricScattering).toBeCloseTo(1, 5);
    expect(light?.castVolumetricShadow).toBe(true);
  });

  test("set-component-field round-trips the per-light fields + stays validation-clean", async () => {
    await engine.call("set-fog", { enabled: true, mode: "volumetric" });
    await engine.call("set-component-field", {
      entity: lightId,
      component: "PointLight",
      field: "castVolumetricShadow",
      value: false,
    });
    await engine.call("set-component-field", {
      entity: lightId,
      component: "PointLight",
      field: "volumetricScattering",
      value: 2,
    });
    const info = await engine.call<InspectResult>("inspect", { entity: lightId });
    const light = info.components.PointLight as {
      volumetricScattering: number;
      castVolumetricShadow: boolean;
    };
    expect(light.volumetricScattering).toBeCloseTo(2, 5);
    expect(light.castVolumetricShadow).toBe(false);
    await engine.settle(250);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// Local FogVolume components. A `fog-volume` preset spawns a box/sphere of local
// participating media injected into the *same* froxel grid during density evaluation; its
// fields round-trip through the generic `set-component-field` / `inspect` with no per-command code,
// and the injection loop (soft edge + height slab + tiling-noise erosion) stays validation-clean.
describe("local fog volumes", () => {
  let volumeId = "";

  test("add-entity fog-volume spawns a FogVolume with box defaults", async () => {
    await engine.call("set-fog", { enabled: true, mode: "volumetric" });
    const ref = await engine.call<EntityRef>("add-entity", { preset: "fog-volume" });
    volumeId = ref.id;
    const info = await engine.call<InspectResult>("inspect", { entity: volumeId });
    const volume = info.components.FogVolume as
      | { shape?: string; density?: number; extents?: { x: number }; noiseIntensity?: number }
      | undefined;
    expect(volume?.shape).toBe("box");
    expect(volume?.density).toBeCloseTo(0.5, 5);
    expect(volume?.extents?.x).toBeCloseTo(5, 5);
    expect(volume?.noiseIntensity).toBeCloseTo(0, 5);
  });

  test("set-component-field round-trips density, noise, and wind + stays validation-clean", async () => {
    await engine.call("set-component-field", {
      entity: volumeId,
      component: "FogVolume",
      field: "density",
      value: 1.5,
    });
    await engine.call("set-component-field", {
      entity: volumeId,
      component: "FogVolume",
      field: "noiseIntensity",
      value: 0.8,
    });
    await engine.call("set-component-field", {
      entity: volumeId,
      component: "FogVolume",
      field: "wind",
      value: { x: 1, y: 0, z: 0.5 },
    });
    const info = await engine.call<InspectResult>("inspect", { entity: volumeId });
    const volume = info.components.FogVolume as {
      density: number;
      noiseIntensity: number;
      wind: { x: number; y: number; z: number };
    };
    expect(volume.density).toBeCloseTo(1.5, 5);
    expect(volume.noiseIntensity).toBeCloseTo(0.8, 5);
    expect(volume.wind.x).toBeCloseTo(1, 5);
    expect(volume.wind.z).toBeCloseTo(0.5, 5);
    await engine.settle(300);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("a sphere volume injects validation-clean", async () => {
    await engine.call("set-component-field", {
      entity: volumeId,
      component: "FogVolume",
      field: "shape",
      value: "sphere",
    });
    await engine.settle(250);
    expect(engine.validationErrors()).toEqual([]);
  });
});

// Aerial perspective. `aerialPerspective` + `aerialIntensity` ride the same `set-fog`
// merge (no new command). With an active atmosphere they arm the 32³ AP fill (the atmosphere LUT
// march) + the shared-ledger composite fold; `set-atmosphere enabled:false` collapses AP to fog-only.
// The two fields round-trip through `EnvironmentDto`, `aerialIntensity` validates `>= 0`, and the
// fill + composite stay validation-clean.
describe("aerial perspective", () => {
  test("set-fog echoes aerialPerspective + aerialIntensity", async () => {
    await engine.call<EnvironmentDto>("set-atmosphere", { enabled: true });
    const env = await engine.call<EnvironmentDto>("set-fog", {
      enabled: true,
      mode: "analytic",
      aerialPerspective: true,
      aerialIntensity: 1.5,
    });
    expect(env.fog.aerialPerspective).toBe(true);
    expect(env.fog.aerialIntensity).toBeCloseTo(1.5, 5);
  });

  test("get-environment reflects AP + the fill/composite run validation-clean", async () => {
    await engine.settle(300);
    const env = await engine.call<EnvironmentDto>("get-environment");
    expect(env.fog.aerialPerspective).toBe(true);
    expect(env.fog.aerialIntensity).toBeCloseTo(1.5, 5);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("AP with fog disabled composites on the shared ledger validation-clean", async () => {
    const env = await engine.call<EnvironmentDto>("set-fog", { enabled: false });
    expect(env.fog.enabled).toBe(false);
    expect(env.fog.aerialPerspective).toBe(true);
    await engine.settle(250);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("disabling the atmosphere collapses AP to fog-only validation-clean", async () => {
    await engine.call<EnvironmentDto>("set-atmosphere", { enabled: false });
    await engine.call<EnvironmentDto>("set-fog", { enabled: true });
    await engine.settle(250);
    expect(engine.validationErrors()).toEqual([]);
  });

  test("a negative aerialIntensity is rejected", async () => {
    await expect(engine.call<EnvironmentDto>("set-fog", { aerialIntensity: -1 })).rejects.toThrow();
  });
});

// The `json` escape hatch merges an arbitrary object first (the same substrate the typed fields
// write into); a color triple round-trips through the environment block.
test("set-fog json escape hatch merges the albedo tint", async () => {
  const env = await engine.call<EnvironmentDto>("set-fog", {
    enabled: true,
    json: { albedo: { x: 0.8, y: 0.4, z: 0.2 } },
  });
  expect(env.fog.albedo.x).toBeCloseTo(0.8, 5);
  expect(env.fog.albedo.y).toBeCloseTo(0.4, 5);
  expect(env.fog.albedo.z).toBeCloseTo(0.2, 5);
  await engine.settle(200);
  expect(engine.validationErrors()).toEqual([]);
});
