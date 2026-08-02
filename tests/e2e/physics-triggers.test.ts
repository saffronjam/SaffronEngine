// Collision layers + sensors + the contact-event ring. A dynamic box
// falls through a Sensor collider and past it; drain-contacts surfaces a seq-cursored begin then end
// for the pair (sensor: true). The Debris layer collides with the world but not other debris.

import { afterAll, beforeAll, expect, test } from "bun:test";
import type { ContactEventDto, WorldHitTargetDto } from "@saffron/protocol";
import { Engine } from "./harness.ts";

let engine: Engine;

const entityOf = (target: WorldHitTargetDto | undefined): string =>
  target?.kind === "scene-entity" ? target.id : "";
const involves = (e: ContactEventDto, a: string, b: string): boolean =>
  (entityOf(e.targetA) === a && entityOf(e.targetB) === b) ||
  (entityOf(e.targetA) === b && entityOf(e.targetB) === a);

async function createCollider(
  name: string,
  y: number,
  half: { x: number; y: number; z: number },
  opts: { sensor?: boolean; rigidbody?: boolean; layer?: number } = {},
): Promise<string> {
  const id = (await engine.call("create-entity", { name })).id;
  await engine.call("set-transform", { entity: id, translation: { x: 0, y, z: 0 } });
  await engine.call("add-component", { entity: id, component: "Collider" });
  await engine.call("set-component-field", {
    entity: id,
    component: "Collider",
    field: "halfExtents",
    value: half,
  });
  if (opts.sensor) {
    await engine.call("set-component-field", {
      entity: id,
      component: "Collider",
      field: "isSensor",
      value: true,
    });
  }
  if (opts.rigidbody) {
    await engine.call("add-component", { entity: id, component: "Rigidbody" });
    if (opts.layer !== undefined) {
      await engine.call("set-component-field", {
        entity: id,
        component: "Rigidbody",
        field: "collisionLayer",
        value: opts.layer,
      });
    }
  }
  return id;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
});
afterAll(async () => {
  await engine?.shutdown();
});

test("a body falling through a sensor fires begin then end (seq-cursored)", async () => {
  const floor = await createCollider("Floor", 0, { x: 10, y: 0.1, z: 10 });
  const sensor = await createCollider("Trigger", 3, { x: 2, y: 0.4, z: 2 }, { sensor: true });
  const box = await createCollider("Faller", 8, { x: 0.5, y: 0.5, z: 0.5 }, { rigidbody: true });

  await engine.call("play");
  await engine.settle(2500); // let the box fall through the trigger and land on the floor

  const drain = await engine.call("drain-contacts", { since: 0 });
  const sensorEvents = drain.events.filter((e) => e.sensor && involves(e, box, sensor));
  // It entered the trigger (begin) and left it (end) on the way down.
  expect(sensorEvents.some((e) => e.kind === "begin")).toBe(true);
  expect(sensorEvents.some((e) => e.kind === "end")).toBe(true);
  // It also landed on the floor — a solid (non-sensor) contact.
  expect(drain.events.some((e) => !e.sensor && involves(e, box, floor) && e.kind === "begin")).toBe(
    true,
  );
  expect(drain.highWaterSeq).toBeGreaterThan(0);

  // A tail drain from the high-water cursor re-sends nothing (the body has settled).
  await engine.settle(400);
  const tail = await engine.call("drain-contacts", { since: drain.highWaterSeq });
  expect(tail.events.length).toBe(0);

  await engine.call("stop");
  await engine.settle();
});

test("Debris collides with the static floor but not with other debris", async () => {
  const floor = await createCollider("Floor2", 0, { x: 10, y: 0.1, z: 10 });
  // Two debris bodies stacked: if debris-debris collided, the upper would rest on the lower and a
  // contact between them would fire. It must not.
  const lower = await createCollider(
    "Debris1",
    1,
    { x: 0.5, y: 0.5, z: 0.5 },
    { rigidbody: true, layer: 2 },
  );
  const upper = await createCollider(
    "Debris2",
    2.2,
    { x: 0.5, y: 0.5, z: 0.5 },
    { rigidbody: true, layer: 2 },
  );

  await engine.call("play");
  await engine.settle(2500);

  const drain = await engine.call("drain-contacts", { since: 0 });
  // Each debris body touches the floor...
  expect(drain.events.some((e) => involves(e, lower, floor) && e.kind === "begin")).toBe(true);
  expect(drain.events.some((e) => involves(e, upper, floor) && e.kind === "begin")).toBe(true);
  // ...but the two debris bodies never contact each other (the matrix excludes Debris-vs-Debris).
  expect(drain.events.some((e) => involves(e, lower, upper))).toBe(false);

  await engine.call("stop");
  await engine.settle();
});

test("the triggers run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
