// `set-transform smooth:1` animates fields toward the target over a few frames (the
// gizmo-style exponential step) and snaps exactly on convergence, so a settled read-back
// must equal the target verbatim. A non-smooth write cancels any pending animation — the
// exact value always wins. Targets use f32-exact literals so the JSON round-trip compares
// with toEqual.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot();
});
afterAll(async () => {
  await engine?.shutdown();
});

interface TransformInspect {
  components: {
    Transform: {
      translation: { x: number; y: number; z: number };
      scale: { x: number; y: number; z: number };
    };
  };
}

test("smooth set-transform converges exactly to the target", async () => {
  const name = "e2e-smooth-transform";
  await engine.call("create-entity", { args: [name] }); // createEntity adds a Transform

  const translation = { x: 1.5, y: -2, z: 3.25 };
  const scale = { x: 2, y: 0.5, z: 1 };
  await engine.call("set-transform", { entity: name, translation, scale, smooth: true });
  await engine.settle(400);

  const info = await engine.call<TransformInspect>("inspect", { entity: name });
  expect(info.components.Transform.translation).toEqual(translation);
  expect(info.components.Transform.scale).toEqual(scale);
});

test("a non-smooth set-transform overrides a pending smooth animation", async () => {
  const name = "e2e-smooth-transform-cancel";
  await engine.call("create-entity", { args: [name] });

  await engine.call("set-transform", {
    entity: name,
    translation: { x: 10, y: 0, z: 0 },
    smooth: true,
  });
  const exact = { x: -1, y: 2.5, z: 0.75 };
  await engine.call("set-transform", { entity: name, translation: exact });
  await engine.settle(400);

  const info = await engine.call<TransformInspect>("inspect", { entity: name });
  expect(info.components.Transform.translation).toEqual(exact);
});

test("destroying the entity mid-smooth is harmless", async () => {
  const name = "e2e-smooth-destroyed";
  await engine.call("create-entity", { args: [name] }); // createEntity adds a Transform
  await engine.call("set-transform", {
    entity: name,
    translation: { x: 0, y: 5, z: 0 },
    smooth: true,
  });
  await engine.call("destroy-entity", { entity: name });
  // The stepper must drop the orphaned entry without touching freed state; the
  // suite's validation-clean log assertion covers the rest.
  await engine.settle(200);
});
