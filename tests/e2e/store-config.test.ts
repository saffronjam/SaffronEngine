// The per-project stores block round-trips over the control plane: set-stores updates the
// enabled connector set, save-project persists it into project.json's `stores` block, and a
// reload restores it. Credentials live editor-side in the keyring and are not exercised here
// (the host never touches them); this targets the host-side enablement contract.

import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync, rmSync } from "node:fs";
import { Engine } from "./harness.ts";

let engine: Engine;
const projectDir = `/tmp/saffron-e2e-stores-${process.pid}`;

interface StoresDto {
  enabled: string[];
}

beforeAll(async () => {
  rmSync(projectDir, { recursive: true, force: true });
  engine = await Engine.boot({ SAFFRON_SCRATCH_PROJECT: "1" });
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.call("load-project", { path: `${projectDir}/project.json` });
});
afterAll(async () => {
  await engine?.shutdown();
  rmSync(projectDir, { recursive: true, force: true });
});

test("a fresh project has no enabled stores", async () => {
  const stores = await engine.call<StoresDto>("get-stores");
  expect(stores.enabled).toEqual([]);
});

test("set-stores updates the enabled set", async () => {
  const result = await engine.call<StoresDto>("set-stores", {
    enabled: ["polyhaven", "poly-pizza"],
  });
  expect(result.enabled).toEqual(["polyhaven", "poly-pizza"]);
  const read = await engine.call<StoresDto>("get-stores");
  expect(read.enabled).toEqual(["polyhaven", "poly-pizza"]);
});

test("save-project writes the stores block into project.json", async () => {
  await engine.call("save-project", { path: `${projectDir}/project.json` });
  await engine.settle();
  const doc = JSON.parse(readFileSync(`${projectDir}/project.json`, "utf8")) as {
    stores?: StoresDto;
  };
  expect(doc.stores?.enabled).toEqual(["polyhaven", "poly-pizza"]);
});

test("a reload restores the enabled set from disk", async () => {
  await engine.call("reload-project");
  await engine.settle();
  const stores = await engine.call<StoresDto>("get-stores");
  expect(stores.enabled).toEqual(["polyhaven", "poly-pizza"]);
});

test("the engine logged no validation errors", async () => {
  await engine.settle();
  expect(engine.validationErrors()).toEqual([]);
});
