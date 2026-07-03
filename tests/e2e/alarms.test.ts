// Perf-alarm delivery smoke test (Phase 3). Detectors run each frame and append FIRING/
// RESOLVED events to a non-blocking, seq-cursored ring; the editor drains it alongside the
// usual reconcile poll. This drives a frame-budget breach by tightening the budget so every
// real frame is over it, then relaxes it and checks the matching RESOLVED — exercising the
// fingerprint coalescing, the hysteresis exit, and the never-miss / never-double-count cursor.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";
import type { DrainAlarmsResult, ActiveAlarmsDto } from "@saffron/protocol";


let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("add-entity", { preset: "cube" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a tight budget raises a frame-budget alarm; relaxing it resolves the same fingerprint", async () => {
  // Budget so tight that every real frame is over it → frame-budget fires after the debounce.
  await engine.call("set-perf-config", { targetFps: 5000 });
  await engine.settle(800);

  const fired = await engine.call<DrainAlarmsResult>("drain-alarms", { since: 0 });
  // Events come back in seq order, strictly increasing — the cursor's monotonic contract.
  let prev = 0;
  for (const e of fired.events) {
    expect(e.seq).toBeGreaterThan(prev);
    prev = e.seq;
  }
  const firing = fired.events.find((e) => e.metric === "frame-budget" && e.state === "firing");
  expect(firing).toBeDefined();
  expect(firing!.seq).toBeGreaterThan(0);
  expect(["warning", "critical"]).toContain(firing!.severity);
  expect(fired.highWaterSeq).toBeGreaterThanOrEqual(firing!.seq);
  expect(fired.overflowed).toBe(false);

  // It should also be in the active set (the badge source).
  const active = await engine.call<ActiveAlarmsDto>("list-active-alarms");
  expect(active.alarms.some((a) => a.metric === "frame-budget")).toBe(true);

  // Relax the budget so the smoothed frame time falls under the hysteresis exit → RESOLVED.
  await engine.call("set-perf-config", { targetFps: 1 });
  await engine.settle(500);

  const cleared = await engine.call<DrainAlarmsResult>("drain-alarms", { since: fired.highWaterSeq });
  // No double-counting: nothing at or below the previous high-water is re-sent.
  for (const e of cleared.events) {
    expect(e.seq).toBeGreaterThan(fired.highWaterSeq);
  }
  const resolved = cleared.events.find((e) => e.metric === "frame-budget" && e.state === "resolved");
  expect(resolved).toBeDefined();
  expect(resolved!.fingerprint).toBe(firing!.fingerprint);
  expect(resolved!.durationMs).toBeGreaterThan(0);

  // The active set no longer holds it.
  const activeAfter = await engine.call<ActiveAlarmsDto>("list-active-alarms");
  expect(activeAfter.alarms.some((a) => a.metric === "frame-budget")).toBe(false);

  // A follow-up drain from the new cursor never re-sends the resolved event.
  const tail = await engine.call<DrainAlarmsResult>("drain-alarms", { since: cleared.highWaterSeq });
  for (const e of tail.events) {
    expect(e.seq).toBeGreaterThan(cleared.highWaterSeq);
  }
  expect(engine.validationErrors()).toEqual([]);
});
