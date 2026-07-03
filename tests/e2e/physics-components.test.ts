// physics-ui plan, phase 1: the physics components are addable + their wire shapes are stable.
// The editor's generic inspector types Rigidbody/Collider/etc from sa-types.ts (hand-written TS
// interfaces in gen.ts), so a drift between those interfaces and the C++ serde would silently
// mis-type the editor with no compiler catch. This pins the exact wire shapes the interfaces claim:
// the motion/shape enums as lowercase strings, lockPosition/lockRotation as {x,y,z} booleans, and
// ColliderComponent.material as a nested {friction, restitution} object — round-tripped through the
// full-DTO set-component path the inspector uses.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
let entity = "";

interface Inspect {
  components: Record<string, Record<string, unknown>>;
}

const inspect = async (): Promise<Record<string, Record<string, unknown>>> =>
  (await engine.call<Inspect>("inspect", { entity })).components;

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  entity = (await engine.call<{ id: string }>("create-entity", { name: "Body" })).id;
  await engine.call("add-component", { entity, component: "Rigidbody" });
  await engine.call("add-component", { entity, component: "Collider" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("Rigidbody/Collider add with their default wire shapes", async () => {
  const c = await inspect();
  const rb = c.Rigidbody;
  expect(rb).toBeDefined();
  // The enum serializes as a lowercase string, never an integer.
  expect(rb.motion).toBe("dynamic");
  // bvec3 locks are an {x,y,z} boolean object.
  expect(rb.lockPosition).toEqual({ x: false, y: false, z: false });
  expect(rb.lockRotation).toEqual({ x: false, y: false, z: false });
  expect(typeof rb.collisionLayer).toBe("number");

  const col = c.Collider;
  expect(col.shape).toBe("box");
  // The physics material is nested, not flattened.
  expect(col.material).toEqual({ friction: expect.any(Number), restitution: expect.any(Number) });
  expect(col.isSensor).toBe(false);
});

test("the inspector's full-DTO set-component round-trips the enum/lock/nested shapes", async () => {
  const before = await inspect();

  // Mirror the inspector's read-modify-write: patch the full DTO and send the whole thing.
  await engine.call("set-component", {
    entity,
    component: "Rigidbody",
    json: { ...before.Rigidbody, motion: "kinematic", lockPosition: { x: true, y: false, z: true } },
  });
  await engine.call("set-component", {
    entity,
    component: "Collider",
    json: { ...before.Collider, shape: "sphere", material: { friction: 0.9, restitution: 0.5 } },
  });

  const after = await inspect();
  // Enum survives as the lowercase string the TS union claims.
  expect(after.Rigidbody.motion).toBe("kinematic");
  // The lock patch round-trips per-axis (x/z locked, y free).
  expect(after.Rigidbody.lockPosition).toEqual({ x: true, y: false, z: true });
  // The nested material keeps BOTH sub-fields (the full-DTO write does not drop restitution);
  // friction/restitution are f32 on the wire, so compare with tolerance, not exact equality.
  expect(after.Collider.shape).toBe("sphere");
  const mat = after.Collider.material as { friction: number; restitution: number };
  expect(Object.keys(mat).sort()).toEqual(["friction", "restitution"]);
  expect(mat.friction).toBeCloseTo(0.9, 5);
  expect(mat.restitution).toBeCloseTo(0.5, 5);
});

test("the physics-components run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
