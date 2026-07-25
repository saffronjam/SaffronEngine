// The persistent GPU scene's page residency runs in every host frame: a scene with a
// mesh entity registers the mesh's hierarchy pages, the guaranteed root streams from the
// source artifact through the page worker, and `gpu-scene-stats` reports the resident
// payload — all while the frame stays Vulkan-validation-clean.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { GpuSceneMirrorStatsDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

async function stats(): Promise<GpuSceneMirrorStatsDto> {
  return await engine.call<GpuSceneMirrorStatsDto>("gpu-scene-stats");
}

test("a mesh entity's hierarchy pages register and its root becomes resident", async () => {
  await engine.call("add-entity", { args: ["cube"] });

  // Registration is same-frame with the mirror sync; the root payload streams through
  // the worker within a few frames.
  const deadline = Date.now() + 15_000;
  let current = await stats();
  while (Date.now() < deadline) {
    current = await stats();
    if (current.pageResidency.registered > 0 && current.pageResidency.resident > 0) {
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  expect(current.pageResidency.registered).toBeGreaterThan(0);
  expect(current.pageResidency.resident).toBeGreaterThan(0);
  expect(current.pageResidency.residentBytes).toBeGreaterThan(0);
  expect(current.pageResidency.budgetBytes).toBeGreaterThan(0);
});

test("the GPU visibility chain classifies the instance and emits draw records", async () => {
  // The cube from the previous test is on screen; the cull marks it visible, the
  // traversal emits at least one record for its resident root, and no list overflows.
  const deadline = Date.now() + 15_000;
  let current = await stats();
  while (Date.now() < deadline) {
    current = await stats();
    if (current.visibility.visible > 0 && current.visibility.records > 0) {
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  expect(current.visibility.visible).toBeGreaterThan(0);
  expect(current.visibility.records).toBeGreaterThan(0);
  expect(current.visibility.overflowFlags).toBe(0);
  expect(current.visibility.pressureFlags).toBe(0);
});

test("rapid camera cuts and teleports keep the cut hole-free and validation-clean", async () => {
  // Hammer the camera: teleports far away and back, spins, and framing cuts. Each
  // jump invalidates occlusion history; the cull's history bypass + the retest +
  // survivor chain must re-emit the cube's records every time with no overflow (the
  // harness asserts the log stays validation-clean at shutdown).
  const jumps = [
    { position: { x: 0, y: 1, z: 9 }, yaw: 0, pitch: -5 },
    { position: { x: 400, y: 50, z: -300 }, yaw: 120, pitch: -40 },
    { position: { x: -2, y: 0.5, z: 3 }, yaw: -15, pitch: 5 },
    { position: { x: 0, y: 800, z: 0 }, yaw: 0, pitch: -89 },
    { position: { x: 0, y: 1, z: 6 }, yaw: 0, pitch: 0 },
  ];
  for (const jump of jumps) {
    await engine.call("set-camera", jump);
    await new Promise((resolve) => setTimeout(resolve, 120));
  }

  // Settled back on the cube: the chain recovers the full cut, and every
  // return-to-cut crossfade the hammering started runs out (a fabricated
  // crossfade would keep re-arming and never settle to zero).
  const deadline = Date.now() + 15_000;
  let current = await stats();
  while (Date.now() < deadline) {
    current = await stats();
    if (
      current.visibility.visible > 0 &&
      current.visibility.records > 0 &&
      current.visibility.transitioning === 0
    ) {
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  expect(current.visibility.visible).toBeGreaterThan(0);
  expect(current.visibility.records).toBeGreaterThan(0);
  expect(current.visibility.overflowFlags).toBe(0);
  expect(current.visibility.pressureFlags).toBe(0);
  // The cube's hierarchy is a single node: no cut flip is possible, so the
  // camera hammering must never fabricate a representation crossfade.
  expect(current.visibility.transitioning).toBe(0);
  expect(current.pageResidency.resident).toBeGreaterThan(0);
});
