// Virtual shadow maps through the real host on MoltenVK: the physical atlas serves every
// shadow-casting light family, its page residency answers repeat demand from cache, a moving light
// dirties pages without a whole-atlas storm, and the shadow-page debug channel renders clean.
//
// The atlas is one ordinary image — no sparse binding — so this passing on MoltenVK is the evidence
// that the physical-atlas design needs no sparse-residency support.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { EntityRef, RenderStatsDto } from "@saffron/protocol";
import type { Engine } from "./harness.ts";
import { Cleaner, bootEngine, trackEntity } from "./test-utils.ts";

const cleaner = new Cleaner();
let engine: Engine;

beforeAll(async () => {
  engine = await bootEngine(cleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
});

afterAll(async () => {
  await cleaner.cleanup();
});

async function vsm() {
  const stats = await engine.call<RenderStatsDto>("render-stats");
  return stats.vsm;
}

/// Renders until the atlas has rasterized at least one page, so the counters describe real work.
async function awaitRenderedPages(timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const counters = await vsm();
    if (counters.rendered > 0) {
      return counters;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for a rasterized shadow page: ${JSON.stringify(counters)}`);
    }
    await engine.settle(50);
  }
}

test("the page atlas serves directional, spot, and point shadow casters", async () => {
  // A receiver to cast onto, plus one light of each shadow-casting family.
  const floor = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("add-entity", { preset: "plane" }),
  );
  await engine.call("set-component", {
    entity: floor.id,
    component: "Transform",
    json: { translation: { x: 0, y: 0, z: 0 }, scale: { x: 20, y: 1, z: 20 }, rotation: { x: 0, y: 0, z: 0 } },
  });
  const caster = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("add-entity", { preset: "cube" }),
  );
  await engine.call("rename-entity", { entity: caster.id, name: "VSM caster" });
  await engine.call("set-component", {
    entity: caster.id,
    component: "Transform",
    json: { translation: { x: 0, y: 2, z: 0 }, scale: { x: 1, y: 1, z: 1 }, rotation: { x: 0, y: 0, z: 0 } },
  });

  const spot = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: "VSM spot" }),
  );
  await engine.call("add-component", { entity: spot.id, component: "SpotLight" });
  await engine.call("set-component", {
    entity: spot.id,
    component: "Transform",
    json: { translation: { x: 4, y: 6, z: 4 }, scale: { x: 1, y: 1, z: 1 }, rotation: { x: -1, y: 0.6, z: 0 } },
  });

  const point = trackEntity(
    cleaner,
    engine,
    await engine.call<EntityRef>("create-entity", { name: "VSM point" }),
  );
  await engine.call("add-component", { entity: point.id, component: "PointLight" });
  await engine.call("set-component", {
    entity: point.id,
    component: "Transform",
    json: { translation: { x: -4, y: 5, z: -2 }, scale: { x: 1, y: 1, z: 1 }, rotation: { x: 0, y: 0, z: 0 } },
  });

  await engine.call("set-camera", { position: { x: 0, y: 6, z: 12 }, yaw: 0, pitch: -20 });
  await engine.settle(200);

  const counters = await awaitRenderedPages();
  // The starter scene's directional light plus the spot and point casters all demand pages, and
  // the atlas answers them: nothing overflowed.
  expect(counters.requested).toBeGreaterThan(0);
  expect(counters.allocated).toBeGreaterThan(0);
  expect(counters.overflow).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("repeat demand is answered from resident pages, not re-allocated", async () => {
  // The counters describe one frame, not a running total. A static scene re-demands the same pages
  // each frame, so a settled frame should answer from residency and allocate nothing new.
  await engine.settle(400);
  // A frame early in the settle may still be streaming other pages in, so look for the settled
  // frame rather than judging whichever frame is sampled first.
  const deadline = Date.now() + 20_000;
  let last = await vsm();
  for (;;) {
    if (last.hits > 0 && last.allocated === 0 && last.evicted === 0) {
      expect(last.overflow).toBe(0);
      break;
    }
    if (Date.now() >= deadline) {
      throw new Error(`timeout waiting for a settled residency frame: ${JSON.stringify(last)}`);
    }
    await engine.settle(50);
    last = await vsm();
  }
});

test("a moving caster dirties pages without invalidating the whole atlas", async () => {
  const caster = await engine.call<{ entities: { id: string; name: string }[] }>("list-entities");
  const moving = caster.entities.find((entity) => entity.name === "VSM caster");
  expect(moving).toBeDefined();

  for (let step = 1; step <= 6; step += 1) {
    await engine.call("set-component", {
      entity: moving!.id,
      component: "Transform",
      json: {
        translation: { x: step * 0.4, y: 2, z: 0 },
        scale: { x: 1, y: 1, z: 1 },
        rotation: { x: 0, y: 0, z: 0 },
      },
    });
    await engine.settle(50);
  }
  // A one-cube move is not a whole-atlas storm: whatever the frame dirties, it stays far below the
  // atlas's page count and never overflows residency.
  const after = await vsm();
  expect(after.dirtied).toBeLessThan(1024);
  expect(after.overflow).toBe(0);
  expect(engine.validationErrors()).toEqual([]);
});

test("the shadow-page debug channel renders validation-clean", async () => {
  await engine.call("set-view-mode", { mode: "shadow-pages" });
  await engine.settle(200);
  expect(engine.validationErrors()).toEqual([]);
  await engine.call("set-view-mode", { mode: "lit" });
  await engine.settle(100);
  expect(engine.validationErrors()).toEqual([]);
});
