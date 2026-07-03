// Pick-ray convention over the control plane: viewport UV (0,0 = top-left) maps into
// the renderer's y-down clip space, so an entity in the upper half of the screen picks
// at v < 0.5. Guards against a double y-flip mirroring the ray about screen center
// (clicking above an object selected it, clicking on it missed).

import { afterAll, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";

const STRIP = join(REPO, "tests", "e2e", "fixtures", "skinned-strip.gltf");

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // yaw 0 / pitch 0 looks down -Z, so world +Y projects screen-up.
  await engine.call("set-camera", { yaw: 0, pitch: 0 });
});
afterAll(async () => {
  await engine?.shutdown();
});

interface Ref {
  id: string;
  name: string;
}
interface PickResult {
  hit: boolean;
  id?: string;
  name?: string;
  kind?: string;
}

test("an entity in the upper half of the screen picks at v < 0.5", async () => {
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: cube.id, name: "p-cube" });
  await engine.call("set-transform", { entity: cube.id, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("focus", { entity: cube.id });
  await engine.settle();

  // Centered after the focus: the symmetric center pick lands either way.
  const centered = await engine.call<PickResult>("pick", {});
  expect(centered.hit).toBe(true);
  expect(centered.id).toBe(cube.id);

  // Raise the cube one unit: world +Y is screen-up, so it now sits above center.
  await engine.call("set-transform", { entity: cube.id, translation: { x: 0, y: 1, z: 0 } });
  await engine.settle();

  // Scan down from just above center until the cube answers. A mirrored ray would
  // only hit in the lower half, so the scan failing means the convention regressed.
  let hitV = 0;
  for (let v = 0.46; v >= 0.1; v -= 0.04) {
    const r = await engine.call<PickResult>("pick", { u: 0.5, v });
    if (r.hit && r.id === cube.id) {
      hitV = v;
      break;
    }
  }
  expect(hitV).toBeGreaterThan(0);

  // The point mirrored about screen center is empty space.
  const mirrored = await engine.call<PickResult>("pick", { u: 0.5, v: 1 - hitV });
  expect(mirrored.hit === false || mirrored.id !== cube.id).toBe(true);
});

test("a click in a rotated cube's empty AABB corner misses (triangle precision)", async () => {
  // Isolate from the convention test's leftover raised cube (it would pollute the scan).
  const before = await engine.call<{ entities: Ref[] }>("list-entities");
  for (const e of before.entities) {
    if (e.name === "p-cube") await engine.call("destroy-entity", { entity: e.id });
  }

  // Rotated 45° about the view axis (Z), the cube's silhouette is a diamond inscribed in a
  // world AABB that is √2 larger — the four corner triangles are inside the box but off the
  // mesh. Triangle picking must not hit them.
  const cube = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: cube.id, name: "rot-cube" });
  await engine.call("set-transform", {
    entity: cube.id,
    translation: { x: 0, y: 0, z: 0 },
    rotation: { x: 0, y: 0, z: Math.PI / 4 },
  });
  await engine.call("focus", { entity: cube.id });
  await engine.settle();

  // The diamond's middle is solid geometry.
  const centered = await engine.call<PickResult>("pick", { u: 0.5, v: 0.5 });
  expect(centered.hit).toBe(true);
  expect(centered.id).toBe(cube.id);

  // The diamond's right/top vertices sit on the world-AABB edges; find them by scanning out
  // from center until the silhouette ends.
  let uRight = 0.5;
  for (let u = 0.5; u <= 0.96; u += 0.01) {
    const r = await engine.call<PickResult>("pick", { u, v: 0.5 });
    if (r.hit && r.id === cube.id) uRight = u;
    else if (u > 0.5) break;
  }
  let vTop = 0.5;
  for (let v = 0.5; v >= 0.04; v -= 0.01) {
    const r = await engine.call<PickResult>("pick", { u: 0.5, v });
    if (r.hit && r.id === cube.id) vTop = v;
    else if (v < 0.5) break;
  }
  expect(uRight).toBeGreaterThan(0.5);
  expect(vTop).toBeLessThan(0.5);

  // 70% of the way toward the top-right AABB corner: inside the box (each axis < the edge),
  // outside the diamond (|x|+|y| ≈ 1.4 of the silhouette radius). Must miss the cube.
  const uCorner = 0.5 + 0.7 * (uRight - 0.5);
  const vCorner = 0.5 + 0.7 * (vTop - 0.5);
  const corner = await engine.call<PickResult>("pick", { u: uCorner, v: vCorner });
  expect(corner.hit === false || corner.id !== cube.id).toBe(true);

  await engine.call("destroy-entity", { entity: cube.id });
});

test("a skinned model is selectable (SkinnedMeshComponent was ignored by picking)", async () => {
  const root = await engine.importEntity(STRIP);
  const rig = await engine.rig(root.id);
  await engine.call("focus", { entity: rig });
  await engine.settle();

  // Scan a grid around screen center; a skinned mesh must register at least one hit that
  // resolves to the model root.
  let hitRoot = false;
  for (let v = 0.35; v <= 0.65 && !hitRoot; v += 0.05) {
    for (let u = 0.35; u <= 0.65; u += 0.05) {
      const r = await engine.call<PickResult>("pick", { u, v });
      if (r.hit && r.id === root.id) {
        hitRoot = true;
        break;
      }
    }
  }
  expect(hitRoot).toBe(true);
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
