// Calendar-driven sky end-to-end: round-trip the complete time-of-day block, verify manual
// override remains the sole authored-light path, scrub distinct celestial frames, and keep the
// ephemeris/night-sky GPU work Vulkan-validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { SetTimeOfDayParams, Vec3 } from "@saffron/protocol";
import { bootEngine, captureViewport, Cleaner, prepareScene, trackEntity } from "./test-utils.ts";

let engine: Engine;
let sunId = "";
const cleaner = new Cleaner();

async function screenshot(tag: string): Promise<Buffer> {
  return captureViewport(engine, cleaner, `time-of-day-${tag}`, 0);
}

async function sunDirection(): Promise<Vec3> {
  const inspected = await engine.call("inspect", { entity: sunId });
  const light = inspected.components.DirectionalLight;
  if (!light) {
    throw new Error("starter Sun has no DirectionalLight component");
  }
  return light.direction;
}

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await prepareScene(engine);
  const entities = await engine.call("list-entities");
  const sun = entities.entities.find((entity) => entity.name === "Sun");
  if (!sun) {
    throw new Error("scratch project has no Sun entity");
  }
  sunId = sun.id;

  const moon = trackEntity(
    cleaner,
    engine,
    await engine.call("add-entity", { preset: "directional-light" }),
  );
  await engine.call("set-component-field", {
    entity: moon.id,
    component: "DirectionalLight",
    field: "atmosphereRole",
    value: "moon",
  });
  await engine.call("set-atmosphere", { enabled: true, skyCaptureCadence: 1 });
});

afterAll(async () => {
  await cleaner.cleanup();
});

test("the complete time-of-day model round-trips through its single command", async () => {
  const params: SetTimeOfDayParams = {
    enabled: true,
    manualOverride: false,
    timeOfDay: 0.25,
    year: 2026,
    month: 7,
    day: 18,
    latitude: 59.3293,
    longitude: 18.0686,
    dayLengthSeconds: 0,
    exposureCurve: [
      [0, 0.2],
      [0.5, 0.7],
      [1, 1],
    ],
    tintCurve: {
      master: [
        [0, 0.4],
        [1, 1],
      ],
      red: [
        [0, 0.7],
        [1, 1],
      ],
      green: [
        [0, 0.5],
        [1, 1],
      ],
      blue: [
        [0, 1],
        [1, 1],
      ],
    },
    coverageCurve: [
      [0, 0.8],
      [1, 0.2],
    ],
    cloudTypeCurve: [
      [0, 0.85],
      [1, 0.25],
    ],
  };
  const applied = await engine.call("set-time-of-day", params);
  expect(applied.timeOfDay.enabled).toBe(true);
  expect(applied.timeOfDay.manualOverride).toBe(false);
  expect(applied.timeOfDay.timeOfDay).toBeCloseTo(0.25, 6);
  expect(applied.timeOfDay.year).toBe(2026);
  expect(applied.timeOfDay.month).toBe(7);
  expect(applied.timeOfDay.day).toBe(18);
  expect(applied.timeOfDay.latitude).toBeCloseTo(59.3293, 4);
  expect(applied.timeOfDay.longitude).toBeCloseTo(18.0686, 4);
  expect(applied.timeOfDay.dayLengthSeconds).toBe(0);
  expect(applied.timeOfDay.exposureCurve).toHaveLength(3);
  expect(applied.timeOfDay.exposureCurve[0]).toEqual({ x: 0, y: expect.any(Number) });
  expect(applied.timeOfDay.exposureCurve[0].y).toBeCloseTo(0.2, 6);
  expect(applied.timeOfDay.exposureCurve[1].x).toBeCloseTo(0.5, 6);
  expect(applied.timeOfDay.exposureCurve[1].y).toBeCloseTo(0.7, 6);
  expect(applied.timeOfDay.exposureCurve[2]).toEqual({ x: 1, y: 1 });
  expect(applied.timeOfDay.tintCurve.blue).toEqual([
    { x: 0, y: 1 },
    { x: 1, y: 1 },
  ]);
  expect(applied.timeOfDay.coverageCurve[0].y).toBeCloseTo(0.8, 6);
  expect(applied.timeOfDay.coverageCurve[1].y).toBeCloseTo(0.2, 6);
  expect(applied.timeOfDay.cloudTypeCurve[0].y).toBeCloseTo(0.85, 6);
  expect(applied.timeOfDay.cloudTypeCurve[1].y).toBeCloseTo(0.25, 6);

  const readBack = await engine.call("get-environment");
  expect(readBack.timeOfDay).toEqual(applied.timeOfDay);
});

test("manual override preserves the authored sun direction", async () => {
  const authored = { x: 0.25, y: -0.9, z: 0.35 };
  await engine.call("set-light", { entity: sunId, direction: authored });
  await engine.call("set-time-of-day", {
    enabled: true,
    manualOverride: true,
    timeOfDay: 0.05,
    dayLengthSeconds: 0,
  });
  await engine.settle(1_200);
  const manualFrame = await screenshot("manual-sun");
  const preserved = await sunDirection();
  expect(preserved.x).toBeCloseTo(authored.x, 6);
  expect(preserved.y).toBeCloseTo(authored.y, 6);
  expect(preserved.z).toBeCloseTo(authored.z, 6);

  await engine.call("set-time-of-day", { manualOverride: false });
  await engine.settle(1_200);
  const ephemerisFrame = await screenshot("ephemeris-sun");
  expect(Buffer.compare(manualFrame, ephemerisFrame)).not.toBe(0);

  const stillAuthored = await sunDirection();
  expect(stillAuthored.x).toBeCloseTo(authored.x, 6);
  expect(stillAuthored.y).toBeCloseTo(authored.y, 6);
  expect(stillAuthored.z).toBeCloseTo(authored.z, 6);
});

test("pre-dawn and noon produce distinct validation-clean frames", async () => {
  await engine.call("set-time-of-day", {
    enabled: true,
    manualOverride: false,
    year: 2026,
    month: 3,
    day: 20,
    latitude: 0,
    longitude: 0,
    dayLengthSeconds: 0,
    exposureCurve: [],
    tintCurve: { master: [], red: [], green: [], blue: [] },
    timeOfDay: 0.05,
  });
  await engine.settle(1200);
  const preDawn = await screenshot("pre-dawn");

  const noon = await engine.call("set-time-of-day", { timeOfDay: 0.5 });
  expect(noon.timeOfDay.timeOfDay).toBeCloseTo(0.5, 6);
  await engine.settle(1200);
  const midday = await screenshot("noon");

  expect(midday.equals(preDawn)).toBe(false);
  expect(engine.validationErrors()).toEqual([]);
});

test("invalid calendar and curve inputs are rejected", async () => {
  await expect(engine.call("set-time-of-day", { year: 2025, month: 2, day: 29 })).rejects.toThrow(
    /valid Gregorian date/,
  );
  await expect(
    engine.call("set-time-of-day", {
      exposureCurve: [
        [0.8, 0.2],
        [0.2, 0.8],
      ],
    }),
  ).rejects.toThrow(/strictly increasing/);
});
