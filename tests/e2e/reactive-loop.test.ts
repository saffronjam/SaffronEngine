// The reactive render loop, observed over the control plane: a static viewport goes idle (the
// engine stops rendering and the GPU goes quiet), a mutating command re-arms it, and the editor's
// viewport power-state suppresses rendering when the view is hidden. `render-stats` exposes the
// otherwise-invisible loop state (`idle`, `converged`, `redrawReasons`, `powerState`).
//
// Boots with SAFFRON_SCRATCH_PROJECT so a scene is loaded (the host seeds one redraw on attach,
// then settles to idle once the keep-warm window + temporal convergence elapse).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

const stats = () => engine.call("render-stats");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// Polls render-stats until `idle` reaches `want` (bounded), since the keep-warm window must elapse
// first. render-stats is read-only, so polling it never itself re-arms the loop.
async function waitForIdle(want: boolean, tries = 50): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if ((await stats()).idle === want) {
      return true;
    }
    await sleep(100);
  }
  return false;
}

test("a static viewport goes idle and reports it", async () => {
  expect(await waitForIdle(true)).toBe(true);
  const s = await stats();
  expect(s.idle).toBe(true);
  expect(s.converged).toBe(true);
  expect(s.redrawReasons).toEqual([]);
});

test("a mutating command re-arms rendering, then it settles back to idle", async () => {
  await waitForIdle(true);
  // A state-changing command pulls the loop out of idle.
  await engine.call("set-view-mode", { mode: "wireframe" });
  expect(await waitForIdle(false)).toBe(true);
  // With no further commands it converges and idles again.
  expect(await waitForIdle(true)).toBe(true);
  await engine.call("set-view-mode", { mode: "lit" });
  await waitForIdle(true);
});

test("occluded power-state suppresses rendering even after a mutation", async () => {
  const occluded = await engine.call("set-viewport-power-state", {
    state: "occluded",
  });
  expect(occluded.state).toBe("occluded");
  expect((await stats()).powerState).toBe("occluded");

  // A mutation that would normally render is suppressed while occluded — the loop stays idle.
  await engine.call("set-view-mode", { mode: "wireframe" });
  await sleep(200);
  expect((await stats()).idle).toBe(true);

  // Restore focus; the loop renders again.
  const focused = await engine.call("set-viewport-power-state", {
    state: "focused",
  });
  expect(focused.state).toBe("focused");
  expect(await waitForIdle(false)).toBe(true);
  await engine.call("set-view-mode", { mode: "lit" });
});

test("an unknown power-state is a typed error", async () => {
  await expect(engine.call("set-viewport-power-state", { state: "sideways" })).rejects.toThrow();
});
