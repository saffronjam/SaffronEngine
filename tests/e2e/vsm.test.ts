// Virtual shadow maps through the real host on MoltenVK: the physical atlas serves every
// shadow-casting light family, its page residency answers repeat demand from cache, a moving light
// dirties pages without a whole-atlas storm, and the shadow-page debug channel renders clean.
//
// The atlas is one ordinary image — no sparse binding — so this passing on MoltenVK is the evidence
// that the physical-atlas design needs no sparse-residency support.

import { afterAll, beforeAll, expect, test } from "bun:test";
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
  const stats = await engine.call("render-stats");
  return stats.vsm;
}

// Renders until the atlas has rasterized at least one page, returning the PEAK of each counter
// across the wait rather than one frame's sample.
//
// Every counter describes a single frame, and they do not peak together: a page is allocated on
// the frame that first demands it and rasterized on a later one. The peak is what lets an
// assertion talk about the window instead of whichever frame the poll landed on.
async function awaitRenderedPages(timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  const peak = { requested: 0, hits: 0, allocated: 0, rendered: 0, dirtied: 0, overflow: 0 };
  for (;;) {
    const counters = await vsm();
    for (const key of Object.keys(peak) as (keyof typeof peak)[]) {
      peak[key] = Math.max(peak[key], counters[key] ?? 0);
    }
    if (peak.rendered > 0) {
      return peak;
    }
    if (Date.now() >= deadline) {
      throw new Error(
        `timeout waiting for a rasterized shadow page: peak ${JSON.stringify(peak)}, ` +
          `last ${JSON.stringify(counters)}`,
      );
    }
    await engine.settle(50);
  }
}

test("the page atlas serves directional, spot, and point shadow casters", async () => {
  // A receiver to cast onto, plus one light of each shadow-casting family.
  const floor = trackEntity(cleaner, engine, await engine.call("add-entity", { preset: "plane" }));
  await engine.call("set-component", {
    entity: floor.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 0, z: 0 },
      scale: { x: 20, y: 1, z: 20 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });
  const caster = trackEntity(cleaner, engine, await engine.call("add-entity", { preset: "cube" }));
  await engine.call("rename-entity", { entity: caster.id, name: "VSM caster" });
  await engine.call("set-component", {
    entity: caster.id,
    component: "Transform",
    json: {
      translation: { x: 0, y: 2, z: 0 },
      scale: { x: 1, y: 1, z: 1 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });

  const spot = trackEntity(
    cleaner,
    engine,
    await engine.call("create-entity", { name: "VSM spot" }),
  );
  await engine.call("add-component", { entity: spot.id, component: "SpotLight" });
  await engine.call("set-component", {
    entity: spot.id,
    component: "Transform",
    json: {
      translation: { x: 4, y: 6, z: 4 },
      scale: { x: 1, y: 1, z: 1 },
      rotation: { x: -1, y: 0.6, z: 0 },
    },
  });

  const point = trackEntity(
    cleaner,
    engine,
    await engine.call("create-entity", { name: "VSM point" }),
  );
  await engine.call("add-component", { entity: point.id, component: "PointLight" });
  await engine.call("set-component", {
    entity: point.id,
    component: "Transform",
    json: {
      translation: { x: -4, y: 5, z: -2 },
      scale: { x: 1, y: 1, z: 1 },
      rotation: { x: 0, y: 0, z: 0 },
    },
  });

  await engine.call("set-camera", { position: { x: 0, y: 6, z: 12 }, yaw: 0, pitch: -20 });
  await engine.settle(200);

  const counters = await awaitRenderedPages();
  // The starter scene's directional light plus the spot and point casters all demand pages, the
  // atlas answers every one of them from residency, and it rasterizes them — with nothing
  // overflowing. `allocated` is deliberately not asserted here: allocation is a one-time event as
  // each page first becomes resident, so by the time a caster is set up and polled it reads 0 on
  // every frame. That it stays 0 while demand is served is the point of the next test.
  expect(counters.requested).toBeGreaterThan(0);
  expect(counters.hits).toBe(counters.requested);
  expect(counters.rendered).toBeGreaterThan(0);
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
  const caster = await engine.call("list-entities");
  const moving = caster.entities.find((entity) => entity.name === "VSM caster");
  expect(moving).toBeDefined();

  let peakMovedDirtied = 0;
  let peakPointDirtied = 0;
  let peakPointRequested = 0;
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
    const during = await vsm();
    peakMovedDirtied = Math.max(peakMovedDirtied, during.dirtiedMoved);
    peakPointDirtied = Math.max(peakPointDirtied, during.point.dirtied);
    peakPointRequested = Math.max(peakPointRequested, during.point.requested);
  }
  // A one-cube move is not a whole-atlas storm: whatever the frame dirties, it stays inside the
  // pages the frame actually demanded and never overflows residency. How TIGHT the dirty set is
  // around the caster's own directional footprint is a property of the page-span derivation, which
  // `a_thin_caster_dirties_a_strip_where_a_cube_dirties_a_square` pins exactly. Point shadows publish
  // by coherent cube face, so a moved caster dirties already-armed faces without projecting every
  // cooked leaf box through every face on the CPU.
  const after = await vsm();
  expect(after.dirtied).toBeLessThanOrEqual(after.requested);
  expect(after.overflow).toBe(0);
  expect(peakMovedDirtied).toBeGreaterThan(0);
  expect(peakPointDirtied).toBeGreaterThan(0);
  expect(peakPointRequested).toBeGreaterThan(0);
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
