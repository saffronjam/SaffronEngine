// The partitioned top-level acceleration structure: same picture, less work per frame.
//
// A KHR top level is rebuilt whole every frame. A partitioned one holds its instances in partitions
// and advances by an op stream naming only what changed, so a settled frame costs the instances it
// touched rather than the instances that exist. The picture claim is the load-bearing one: a
// structure that traces a different picture has broken something, not saved anything.
//
// The comparison runs two hosts over one scene, because on a device that has the extension there is
// no other way to reach the KHR path, and a self-comparison cannot tell a correct structure from a
// consistently wrong one.
//
// Two validation messages are expected and whitelisted by VUID, which is why this suite sits apart
// from `rt-telemetry`. The SDK layers ship the extension's header but do not model it: a partitioned
// structure has no SPIR-V form for a shader variable to declare, so the shader declares an ordinary
// acceleration structure and the layer calls that a descriptor mismatch, and it is memory rather
// than an object, so the layer cannot resolve its address. Neither is reachable from engine code,
// and every other validation message still fails this suite.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { RenderStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { decodeRgb8Png, meanAbsoluteDifference } from "./image.ts";
import { Cleaner, bootEngine, captureViewport, prepareScene } from "./test-utils.ts";

// The layer gaps this suite tolerates, by VUID. Anything else is a real failure.
const EXPECTED_VUIDS = [
  "VUID-VkGraphicsPipelineCreateInfo-layout-07990",
  "VUID-vkCmdDrawIndexedIndirectCount-None-08114",
];

function unexpectedValidation(engine: Engine): string[] {
  return engine
    .validationErrors()
    .filter((line) => !EXPECTED_VUIDS.some((vuid) => line.includes(vuid)));
}

const cleaner = new Cleaner();
let partitioned: Engine;
let supported = false;

/** Builds the identical ray-traced scene both hosts are compared over. */
async function buildScene(engine: Engine): Promise<void> {
  await prepareScene(engine, { camera: { yaw: 0, pitch: 0 } });
  await engine.call("set-rt-shadows", { enabled: true });
  await engine.call("add-entity", { preset: "cube" });
  await engine.settle(900);
}

beforeAll(async () => {
  partitioned = await bootEngine(cleaner, {
    SAFFRON_SCRATCH_PROJECT: "1",
    SAFFRON_PTLAS: "1",
  });
  await buildScene(partitioned);
  supported = (await partitioned.call<RenderStatsDto>("render-stats")).ptlasSupported;
}, 120_000);

afterAll(async () => {
  await cleaner.cleanup();
});

test("a partitioned top level traces the same picture as the KHR one", async () => {
  if (!supported) {
    return;
  }
  const withPartitions = decodeRgb8Png(await captureViewport(partitioned, cleaner, "ptlas-on"));

  const khr = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  await buildScene(khr);
  const khrStats = await khr.call<RenderStatsDto>("render-stats");
  // The control: this host really did take the other path, and really did trace.
  expect(khrStats.ptlasSupported).toBe(false);
  expect(khrStats.rtShadows).toBe(true);
  expect(khrStats.rtInstances).toBeGreaterThan(0);
  const withoutPartitions = decodeRgb8Png(await captureViewport(khr, cleaner, "ptlas-off"));

  // Exactly equal, not merely close: both structures place the same instances over the same
  // geometry, so any difference at all would be the partitioned build placing them wrongly.
  expect(meanAbsoluteDifference(withPartitions, withoutPartitions)).toBe(0);
  expect(unexpectedValidation(khr)).toEqual([]);
}, 180_000);

/**
 * The largest per-frame op count seen over `samples` readings.
 *
 * The op counters are per-frame, and the frame that writes an instance is over long before
 * a single reading taken afterwards — which reports a clean zero that is true and useless.
 * Both directions of the assertion below therefore sample a window: a peak for work that
 * must have happened, and a peak for work that must not have.
 */
async function peakOps(engine: Engine, samples: number): Promise<{ writes: number; ops: number }> {
  let writes = 0;
  let ops = 0;
  for (let i = 0; i < samples; i += 1) {
    const stats = await engine.call<RenderStatsDto>("render-stats");
    writes = Math.max(writes, stats.ptlasWrites);
    ops = Math.max(ops, stats.ptlasWrites + stats.ptlasUpdates);
    await engine.settle(30);
  }
  return { writes, ops };
}

test("a settled frame advances the structure instead of rebuilding it", async () => {
  if (!supported) {
    return;
  }
  // The instances are placed and nothing has changed since, so the ops a frame emits are
  // the whole point: a rebuilt-whole structure would write every instance every frame.
  await partitioned.settle(600);
  const before = await partitioned.call<RenderStatsDto>("render-stats");
  expect(before.rtInstances).toBeGreaterThan(0);
  expect(before.ptlasPartitions).toBeGreaterThan(0);
  const settled = await peakOps(partitioned, 20);
  expect(settled.ops).toBe(0);

  // Adding geometry must move it off zero, or the counter reports nothing rather than
  // reporting no work — the reading that would make the assertion above vacuous.
  await partitioned.call("add-entity", { preset: "cube" });
  const churned = await peakOps(partitioned, 20);
  const after = await partitioned.call<RenderStatsDto>("render-stats");
  expect(after.rtInstances).toBeGreaterThan(before.rtInstances);
  expect(churned.writes).toBeGreaterThan(0);
  // And only what changed is written, not the table.
  expect(churned.writes).toBeLessThan(after.rtInstances);
  expect(unexpectedValidation(partitioned)).toEqual([]);
}, 120_000);
