// Scene-hierarchy interaction over the control plane, the legs that genuinely need a live host:
// parenting composes world transforms so a child picks/focuses at its world position (incl. a
// parented light's billboard), and the gizmo drags a parented child — or its parent under
// preserve-children — in world space while writing a rebased local transform that survives a
// scene save/load. Parenting is written through the generic set-component path (the raw
// Relationship write relinks server-side).
//
// The two `set-parent` / `set-transform` reparent-math cases below are pure transform-compose
// checks (keepWorld / preserve-children rebasing) that could move to a saffron-scene unit test;
// they are kept here as e2e for now so no coverage is dropped.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { Engine } from "./harness.ts";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  // yaw 0 / pitch 0 looks down -Z, so after a focus the world X axis projects screen-right
  // and world Y screen-up — deterministic gizmo probing.
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
interface Vec3 {
  x: number;
  y: number;
  z: number;
}

async function makeCube(name: string, translation: Vec3): Promise<Ref> {
  const ref = await engine.call<Ref>("add-entity", { args: ["cube"] });
  await engine.call("rename-entity", { entity: ref.id, name });
  await engine.call("set-transform", { entity: ref.id, translation });
  return ref;
}

async function parentTo(child: string, parent: string): Promise<void> {
  await engine.call("set-component", { entity: child, component: "Relationship", json: { parent } });
}

async function focusPick(entity: string): Promise<PickResult> {
  await engine.call("focus", { entity });
  await engine.settle();
  return engine.call<PickResult>("pick", {});
}

test("a parented child picks at its world position, not its local origin", async () => {
  const parent = await makeCube("h-parent", { x: 10, y: 0, z: 0 });
  const child = await makeCube("h-child", { x: 0, y: 2, z: 0 });
  await engine.settle();

  // Unparented: the child sits at its local translation.
  const before = await focusPick(child.id);
  expect(before.hit).toBe(true);
  expect(before.id).toBe(child.id);

  await parentTo(child.id, parent.id);
  await engine.settle();

  // Parented: focus aims at the composed world position and the pick still lands.
  const atWorld = await focusPick(child.id);
  expect(atWorld.hit).toBe(true);
  expect(atWorld.id).toBe(child.id);

  // The child's local origin no longer holds it once parented.
  const probe = await engine.call<Ref>("create-entity", { args: ["h-probe"] });
  await engine.call("set-transform", { entity: probe.id, translation: { x: 0, y: 2, z: 0 } });
  const atLocal = await focusPick(probe.id);
  expect(atLocal.hit === false || atLocal.id !== child.id).toBe(true);
});

test("a parented light's billboard picks and focuses at its world position", async () => {
  const anchor = await makeCube("h-anchor", { x: -6, y: 1, z: 2 });
  const light = await engine.call<Ref>("add-entity", { args: ["point-light"] });
  await engine.call("set-transform", { entity: light.id, translation: { x: 0, y: 3, z: 0 } });
  await parentTo(light.id, anchor.id);
  await engine.settle();

  const p = await focusPick(light.id);
  expect(p.hit).toBe(true);
  expect(p.id).toBe(light.id);
  expect(p.kind).toBe("billboard");
});

test("gizmo drag on a parented child moves it in world space and rebases the local", async () => {
  const parent = await makeCube("g-parent", { x: 10, y: 0, z: 0 });
  const child = await makeCube("g-child", { x: 0, y: 2, z: 0 });
  await parentTo(child.id, parent.id);
  await engine.call("set-gizmo", { op: "translate", space: "world" });
  await engine.call("select", { entity: child.id });
  await engine.call("focus", { entity: child.id });
  await engine.settle();

  // Probe outward from the center until the X handle answers, then drag it right.
  let beginX = 0;
  for (let x = 0.02; x <= 0.5; x += 0.02) {
    const r = await engine.call<{ hovered: string }>("gizmo-pointer", { phase: "hover", x, y: 0 });
    if (r.hovered === "x") {
      beginX = x;
      break;
    }
  }
  expect(beginX).toBeGreaterThan(0);
  const begin = await engine.call<{ dragging: boolean }>("gizmo-pointer", {
    phase: "begin",
    x: beginX,
    y: 0,
  });
  expect(begin.dragging).toBe(true);
  await engine.call("gizmo-pointer", { phase: "drag", x: beginX + 0.2, y: 0 });
  await engine.call("gizmo-pointer", { phase: "end", x: beginX + 0.2, y: 0 });

  // A world +X drag rebased into the parent frame: local x moved, y/z untouched.
  const info = await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
    entity: child.id,
  });
  const t = info.components.Transform.translation;
  expect(t.x).toBeGreaterThan(0.05);
  expect(t.y).toBeCloseTo(2, 3);
  expect(t.z).toBeCloseTo(0, 3);

  // The dragged placement is durable: save, reload, and the local transform survives.
  const path = `/tmp/saffron-e2e-hierarchy-${process.pid}.json`;
  await engine.call("save-scene", { path });
  await engine.call("load-scene", { path });
  await engine.settle();
  const reloaded = await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
    entity: "g-child",
  });
  const rt = reloaded.components.Transform.translation;
  expect(rt.x).toBeCloseTo(t.x, 4);
  expect(rt.y).toBeCloseTo(t.y, 4);
  expect(rt.z).toBeCloseTo(t.z, 4);

  // And it still picks at the composed world position after the reload.
  const picked = await focusPick("g-child");
  expect(picked.hit).toBe(true);
  expect(picked.name).toBe("g-child");
});

test("set-parent preserves world position, keeps the selection, and detaches clean", async () => {
  const parent = await makeCube("sp-parent", { x: 4, y: 1, z: -2 });
  const child = await makeCube("sp-child", { x: -1, y: 0.5, z: 3 });
  await engine.call("select", { entity: child.id });
  const before = await engine.call<{ sceneVersion: number }>("get-selection");

  await engine.call("set-parent", { entity: child.id, parent: parent.id });
  const info = await engine.call<{
    components: { Transform: { translation: Vec3 }; Relationship: { parent: string } };
  }>("inspect", { entity: child.id });
  expect(info.components.Relationship.parent).toBe(parent.id);
  // keepWorld rebased the local translation so the world position is unchanged.
  expect(info.components.Transform.translation.x).toBeCloseTo(-5, 4);
  expect(info.components.Transform.translation.y).toBeCloseTo(-0.5, 4);
  expect(info.components.Transform.translation.z).toBeCloseTo(5, 4);

  // The reparent bumped sceneVersion and left the selection intact.
  const after = await engine.call<{ sceneVersion: number; entity?: { id: string } }>("get-selection");
  expect(after.sceneVersion).toBeGreaterThan(before.sceneVersion);
  expect(after.entity?.id).toBe(child.id);

  const list = await engine.call<{ entities: { id: string; parentId?: string }[] }>("list-entities");
  expect(list.entities.find((e) => e.id === child.id)?.parentId).toBe(parent.id);

  // Detach restores the original local translation (world preserved both ways).
  await engine.call("set-parent", { entity: child.id, parent: "0" });
  const detached = await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
    entity: child.id,
  });
  expect(detached.components.Transform.translation.x).toBeCloseTo(-1, 4);
  expect(detached.components.Transform.translation.y).toBeCloseTo(0.5, 4);
  expect(detached.components.Transform.translation.z).toBeCloseTo(3, 4);
  const relisted = await engine.call<{ entities: { id: string; parentId?: string }[] }>("list-entities");
  expect(relisted.entities.find((e) => e.id === child.id)?.parentId).toBeUndefined();
});

test("preserve-children: set-transform on a parent rebases the child to hold its world pose", async () => {
  const parent = await makeCube("pc-parent", { x: 5, y: 0, z: 0 });
  const child = await makeCube("pc-child", { x: 2, y: 1, z: 0 });
  await parentTo(child.id, parent.id);
  const gizmo = await engine.call<{ preserveChildren: boolean }>("set-gizmo", {
    preserveChildren: true,
  });
  expect(gizmo.preserveChildren).toBe(true);

  // Child world (7,1,0). Parent moves to (9,0,0) at scale (4,1,2): the child's local
  // must rebase to ((7-9)/4, 1, 0) with scale (1/4, 1, 1/2) for the world pose to hold.
  await engine.call("set-transform", {
    entity: parent.id,
    translation: { x: 9, y: 0, z: 0 },
    scale: { x: 4, y: 1, z: 2 },
  });
  const info = await engine.call<{
    components: { Transform: { translation: Vec3; scale: Vec3 } };
  }>("inspect", { entity: child.id });
  const t = info.components.Transform.translation;
  expect(t.x).toBeCloseTo(-0.5, 4);
  expect(t.y).toBeCloseTo(1, 4);
  expect(t.z).toBeCloseTo(0, 4);
  const s = info.components.Transform.scale;
  expect(s.x).toBeCloseTo(0.25, 4);
  expect(s.y).toBeCloseTo(1, 4);
  expect(s.z).toBeCloseTo(0.5, 4);

  await engine.call("set-gizmo", { preserveChildren: false });
});

test("preserve-children: a gizmo drag on the parent leaves the child's world position fixed", async () => {
  const parent = await makeCube("pg-parent", { x: 10, y: 0, z: 0 });
  const child = await makeCube("pg-child", { x: 0, y: 2, z: 0 });
  await parentTo(child.id, parent.id);
  await engine.call("set-gizmo", { op: "translate", space: "world", preserveChildren: true });
  await engine.call("select", { entity: parent.id });
  await engine.call("focus", { entity: parent.id });
  await engine.settle();

  let beginX = 0;
  for (let x = 0.02; x <= 0.5; x += 0.02) {
    const r = await engine.call<{ hovered: string }>("gizmo-pointer", { phase: "hover", x, y: 0 });
    if (r.hovered === "x") {
      beginX = x;
      break;
    }
  }
  expect(beginX).toBeGreaterThan(0);
  await engine.call("gizmo-pointer", { phase: "begin", x: beginX, y: 0 });
  await engine.call("gizmo-pointer", { phase: "drag", x: beginX + 0.2, y: 0 });
  await engine.call("gizmo-pointer", { phase: "end", x: beginX + 0.2, y: 0 });

  // The parent moved along world +X; the child's local compensated so its drag-begin
  // world position (parent 10 + local 0, y 2) is unchanged: parent.x + child.x == 10.
  const parentT = (
    await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
      entity: parent.id,
    })
  ).components.Transform.translation;
  const childT = (
    await engine.call<{ components: { Transform: { translation: Vec3 } } }>("inspect", {
      entity: child.id,
    })
  ).components.Transform.translation;
  expect(parentT.x).toBeGreaterThan(10.05);
  expect(parentT.x + childT.x).toBeCloseTo(10, 3);
  expect(childT.y).toBeCloseTo(2, 3);
  expect(childT.z).toBeCloseTo(0, 3);

  await engine.call("set-gizmo", { preserveChildren: false });
});

test("the engine logged no validation errors", async () => {
  await engine.settle(500);
  expect(engine.validationErrors()).toEqual([]);
});
