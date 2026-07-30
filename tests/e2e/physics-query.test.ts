// Scene queries. raycast / shapecast hit the physics shapes at their
// simulated transforms (not the render AABBs the editor `pick` uses), mapping the hit body back to
// its entity. A shape-sweep tolerates an edge a thin ray misses. Queries refuse in Edit (the world
// exists only in play). The same entry point backs the control commands and the Lua sa.raycast.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { Engine, REPO } from "./harness.ts";

let engine: Engine;
let floor = "";
let box = "";

interface Vec3 {
  x: number;
  y: number;
  z: number;
}
interface RayHit {
  hit: boolean;
  target?: { kind: "scene-entity"; id: string } | { kind: "vegetation"; plant: string };
  point: { x: number; y: number; z: number };
  normal: { x: number; y: number; z: number };
  distance: number;
}

async function collider(name: string, y: number, half: Vec3): Promise<string> {
  const id = (await engine.call<{ id: string }>("create-entity", { name })).id;
  await engine.call("set-transform", { entity: id, translation: { x: 0, y, z: 0 } });
  await engine.call("add-component", { entity: id, component: "Collider" });
  await engine.call("set-component-field", { entity: id, component: "Collider", field: "halfExtents", value: half });
  return id;
}

beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  floor = await collider("Floor", 0, { x: 10, y: 0.1, z: 10 }); // static, top at y=0.1
  box = await collider("Box", 1, { x: 0.5, y: 0.5, z: 0.5 }); // static, spans x[-0.5,0.5] y[0.5,1.5]

  // A Lua probe: casts straight down from its own position; on a hit it flags itself by jumping x
  // to 99 and snapping y to the hit point — so the test reads the entity transform to prove the hit.
  const project = await engine.call<{ root: string }>("get-project");
  const root = isAbsolute(project.root) ? project.root : join(REPO, project.root);
  const srcDir = join(root, "src");
  mkdirSync(srcDir, { recursive: true });
  writeFileSync(
    join(srcDir, "prober.lua"),
    `local Prober = {}
function Prober:on_update(dt)
  local p = self.entity:get_position()
  local hit = sa.raycast(p.x, p.y, p.z, 0, -1, 0, 20)
  if hit.hit and hit.entity ~= nil then
    self.entity:set_position(sa.vec3(99, hit.point.y, p.z))
  end
end
return Prober
`,
  );
});
afterAll(async () => {
  await engine?.shutdown();
});

test("raycast refuses in Edit (no world yet)", async () => {
  await expect(
    engine.call("raycast", { origin: { x: 0, y: 5, z: 0 }, dir: { x: 0, y: -1, z: 0 } }),
  ).rejects.toThrow(/no physics world/);
});

test("a down-ray hits the floor; an up-ray misses", async () => {
  await engine.call("play");
  await engine.settle(200);

  // Down from above empty floor (x=5, clear of the box) -> hits the floor top, normal up.
  const down = await engine.call<RayHit>("raycast", {
    origin: { x: 5, y: 5, z: 0 },
    dir: { x: 0, y: -1, z: 0 },
    maxDist: 20,
  });
  expect(down.hit).toBe(true);
  expect(down.target).toEqual({ kind: "scene-entity", id: floor });
  expect(down.point.y).toBeCloseTo(0.1, 1);
  expect(down.normal.y).toBeGreaterThan(0.8);
  expect(down.distance).toBeCloseTo(4.9, 1);

  // Down over the box -> hits the box top first (y=1.5), not the floor.
  const onBox = await engine.call<RayHit>("raycast", {
    origin: { x: 0, y: 5, z: 0 },
    dir: { x: 0, y: -1, z: 0 },
    maxDist: 20,
  });
  expect(onBox.hit).toBe(true);
  expect(onBox.target).toEqual({ kind: "scene-entity", id: box });
  expect(onBox.point.y).toBeCloseTo(1.5, 1);

  // Straight up into empty space -> no hit.
  const up = await engine.call<RayHit>("raycast", {
    origin: { x: 0, y: 5, z: 0 },
    dir: { x: 0, y: 1, z: 0 },
    maxDist: 20,
  });
  expect(up.hit).toBe(false);
  expect(up.target).toBeUndefined();
});

test("a sphere-cast grazes the box edge where a thin ray misses it", async () => {
  // At x=0.7 the box edge (x=0.5) is past a thin ray, which falls through to the floor...
  const thin = await engine.call<RayHit>("raycast", {
    origin: { x: 0.7, y: 5, z: 0 },
    dir: { x: 0, y: -1, z: 0 },
    maxDist: 20,
  });
  expect(thin.hit).toBe(true);
  expect(thin.target).toEqual({ kind: "scene-entity", id: floor }); // missed the box, hit the floor

  // ...but a radius-0.5 sphere sweep reaches the box edge and hits it first.
  const sphere = await engine.call<RayHit>("shapecast", {
    origin: { x: 0.7, y: 5, z: 0 },
    dir: { x: 0, y: -1, z: 0 },
    radius: 0.5,
    maxDist: 20,
  });
  expect(sphere.hit).toBe(true);
  expect(sphere.target).toEqual({ kind: "scene-entity", id: box });

  await engine.call("stop");
  await engine.settle();
});

test("a Lua script can query the world via sa.raycast", async () => {
  // Prober above clear floor (x=3); its on_update casts down and flags itself on a hit.
  const prober = (await engine.call<{ id: string }>("create-entity", { name: "Prober" })).id;
  await engine.call("set-transform", { entity: prober, translation: { x: 3, y: 5, z: 0 } });
  await engine.call("add-component", { entity: prober, component: "Script" });
  await engine.call("set-component", {
    entity: prober,
    component: "Script",
    json: { scripts: [{ scriptPath: "prober.lua", overrides: {} }] },
  });

  await engine.call("play");
  await engine.settle(500);
  const t = await engine.call<{ translation: Vec3 }>("get-world-transform", { entity: prober });
  expect(t.translation.x).toBeCloseTo(99, 1); // the script's hit flag — sa.raycast reached the world
  expect(t.translation.y).toBeCloseTo(0.1, 1); // snapped to the floor-top hit point
  await engine.call("stop");
  await engine.settle();
});

test("the query run is validation-clean", () => {
  expect(engine.validationErrors()).toEqual([]);
});
