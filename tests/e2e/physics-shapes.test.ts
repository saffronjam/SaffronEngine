// The five collision shapes + PhysicsMaterial + shape-aware auto-fit.
// Sphere/Capsule are analytic (sized from the mesh AABB); ConvexHull/Mesh are cooked from the
// entity's .smesh vertices. Auto-fit sizes whatever shape the collider holds; fit-collider re-fits
// on demand. A Mesh collider on a Dynamic body is rejected (Jolt MeshShape is Static/Kinematic only).

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { Engine, REPO } from "./harness.ts";
import { bootEngine, Cleaner, trackEntity } from "./test-utils.ts";

let engine: Engine;
let meshSub = "";
const caseCleaner = new Cleaner();
const suiteCleaner = new Cleaner();

const FIXTURE = join(REPO, "tests", "e2e", "fixtures", "two-materials.gltf");

const worldY = async (entity: string): Promise<number> =>
  (await engine.call("get-world-transform", { entity })).translation.y;

const FLOOR_TOP = 0.1;

async function spawn(name: string): Promise<string> {
  const id = (await engine.call("create-entity", { name })).id;
  return trackEntity(caseCleaner, engine, id);
}
async function setCollider(entity: string, field: string, value: unknown): Promise<void> {
  await engine.call("set-component-field", { entity, component: "Collider", field, value });
}
async function makeFloor(): Promise<string> {
  const id = await spawn("Floor");
  await engine.call("set-transform", { entity: id, translation: { x: 0, y: 0, z: 0 } });
  await engine.call("add-component", { entity: id, component: "Collider" });
  await setCollider(id, "halfExtents", { x: 20, y: 0.1, z: 20 });
  return id;
}

beforeAll(async () => {
  engine = await bootEngine(suiteCleaner, { SAFFRON_SCRATCH_PROJECT: "1" });
  const model = await engine.call("import-model", { path: FIXTURE });
  await engine.settle();
  const info = await engine.call("model-info", { asset: model.id });
  meshSub = info.subAssets.find((s) => s.type === "mesh")!.id;
});
afterEach(async () => {
  await engine.call("stop").catch(() => {}); // back to Edit (idempotent)
  await caseCleaner.cleanup();
});
afterAll(async () => {
  await suiteCleaner.cleanup();
});

test("auto-fit sizes the shape the collider holds (box -> sphere -> capsule)", async () => {
  const e = await spawn("Fitted");
  await engine.call("add-component", { entity: e, component: "Mesh" });
  await engine.call("set-component-field", {
    entity: e,
    component: "Mesh",
    field: "mesh",
    value: meshSub,
  });
  await engine.call("add-component", { entity: e, component: "Collider" }); // auto-fits a box

  const box = await engine.call("fit-collider", { entity: e });
  expect(box.shape).toBe("box");
  const h = box.halfExtents;
  // The fixture has volume in at least its planar axes; the shape relationships below are the point.
  expect(Math.max(h.x, h.y, h.z)).toBeGreaterThan(0);

  await setCollider(e, "shape", "sphere");
  const sphere = await engine.call("fit-collider", { entity: e });
  expect(sphere.halfExtents.x).toBeCloseTo(Math.max(h.x, h.y, h.z), 3);

  await setCollider(e, "shape", "capsule");
  const capsule = await engine.call("fit-collider", { entity: e });
  const radius = Math.max(h.x, h.z);
  expect(capsule.halfExtents.x).toBeCloseTo(radius, 3);
  expect(capsule.halfExtents.y).toBeCloseTo(Math.max(0, h.y - radius), 3);
});

test("a sphere falls and settles on the floor at floor-top + radius", async () => {
  await makeFloor();
  const s = await spawn("Ball");
  await engine.call("set-transform", { entity: s, translation: { x: 0, y: 5, z: 0 } });
  await engine.call("add-component", { entity: s, component: "Collider" });
  await setCollider(s, "shape", "sphere");
  await setCollider(s, "halfExtents", { x: 0.5, y: 0.5, z: 0.5 });
  await engine.call("add-component", { entity: s, component: "Rigidbody" });

  await engine.call("play");
  await engine.settle(2200);
  const y = await worldY(s);
  expect(y).toBeGreaterThan(FLOOR_TOP + 0.5 - 0.2);
  expect(y).toBeLessThan(FLOOR_TOP + 0.5 + 0.3);
});

test("a convex hull cooked from the .smesh falls and rests above the floor", async () => {
  await makeFloor();
  const hull = await spawn("Hull");
  await engine.call("set-transform", { entity: hull, translation: { x: 0, y: 5, z: 0 } });
  await engine.call("add-component", { entity: hull, component: "Collider" });
  await setCollider(hull, "shape", "convexhull");
  await setCollider(hull, "sourceMesh", meshSub);
  await engine.call("add-component", { entity: hull, component: "Rigidbody" });

  await engine.call("play");
  await engine.settle(300);
  expect((await engine.call("physics-state")).dynamicCount).toBe(1); // hull cooked + dynamic
  await engine.settle(2200);
  const y = await worldY(hull);
  expect(y).toBeGreaterThan(-0.5); // rested above the floor, did not tunnel away
  expect(y).toBeLessThan(5); // it fell
});

test("a Mesh collider on a Dynamic body is rejected (the body is skipped)", async () => {
  await makeFloor();
  const bad = await spawn("BadMesh");
  await engine.call("set-transform", { entity: bad, translation: { x: 0, y: 3, z: 0 } });
  await engine.call("add-component", { entity: bad, component: "Collider" });
  await setCollider(bad, "shape", "mesh");
  await setCollider(bad, "sourceMesh", meshSub);
  await engine.call("add-component", { entity: bad, component: "Rigidbody" }); // Dynamic

  await engine.call("play");
  await engine.settle(300);
  const state = await engine.call("physics-state");
  // The mesh-on-dynamic body is skipped: only the static floor body exists, nothing dynamic.
  expect(state.dynamicCount).toBe(0);
  expect(state.bodyCount).toBe(1);
});

test("restitution makes a sphere rebound", async () => {
  await makeFloor();
  const bouncy = await spawn("Bouncy");
  await engine.call("set-transform", { entity: bouncy, translation: { x: 0, y: 4, z: 0 } });
  await engine.call("add-component", { entity: bouncy, component: "Collider" });
  await setCollider(bouncy, "shape", "sphere");
  await setCollider(bouncy, "halfExtents", { x: 0.5, y: 0.5, z: 0.5 });
  await setCollider(bouncy, "material", { friction: 0.2, restitution: 0.9 });
  await engine.call("add-component", { entity: bouncy, component: "Rigidbody" });

  await engine.call("play");
  let maxAfterContact = 0;
  for (let i = 0; i < 25; i++) {
    await engine.settle(120);
    const y = await worldY(bouncy);
    if (i > 6) {
      maxAfterContact = Math.max(maxAfterContact, y);
    }
  }
  expect(maxAfterContact).toBeGreaterThan(FLOOR_TOP + 0.5 + 0.5); // rebounded well above rest
});

test("the shapes run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
