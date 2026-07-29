#!/usr/bin/env bun
// Contract tripwire: launches a headless Saffron Anima, compares live `help`
// with the generated DTO manifest, and validates live command results against
// the generated OpenRPC schemas.

import { readFileSync, existsSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";
import net from "node:net";
import { BoundedTextLog, drainHostStream, withHostLog } from "./harness-utils.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..");
const SCHEMA_DIR = join(REPO, "schemas", "control");
const OPENRPC = join(SCHEMA_DIR, "openrpc.generated.json");
const MANIFEST = join(SCHEMA_DIR, "command-manifest.generated.json");
const ENGINE =
  process.env.SAFFRON_ANIMA_BIN ?? join(REPO, "engine", "target", "debug", "saffron-host");
const SOCK = process.env.SAFFRON_CONTROL_SOCK ?? `/tmp/saffron-contract-${process.pid}.sock`;
const CALL_TIMEOUT_MS = Number(process.env.SAFFRON_SCHEMA_CALL_TIMEOUT_MS) || 15_000;
const HOST_LOG_CAPACITY = 256 * 1024;
const HOST_LOG_TAIL = 16 * 1024;
const RT_COMMANDS = new Set(["set-rt-shadows", "set-restir", "set-rt-reflections"]);
const PROJECT_TRANSITIONS = new Set(["new-project", "open-project", "load-project"]);
let appDataPath = "";
let fixturesPath = "";
let modelFixturePath = "";

interface ManifestCommand {
  name: string;
  params: string;
  result: string;
  status: "typed";
  fixture?: string;
  skip?: string;
}

interface Manifest {
  commands: ManifestCommand[];
  skips: { name: string; reason: string }[];
}

const openrpc = JSON.parse(readFileSync(OPENRPC, "utf8"));
const manifest = JSON.parse(readFileSync(MANIFEST, "utf8")) as Manifest;
const generatedSchemas = openrpc.components.schemas as Record<string, unknown>;
const envelopeSchema = JSON.parse(readFileSync(join(SCHEMA_DIR, "envelope.schema.json"), "utf8"));
const hostLog = new BoundedTextLog(HOST_LOG_CAPACITY);
let callTimedOut = false;

function typeOk(v: unknown, t: string): boolean {
  switch (t) {
    case "object":
      return v !== null && typeof v === "object" && !Array.isArray(v);
    case "array":
      return Array.isArray(v);
    case "string":
      return typeof v === "string";
    case "number":
      return typeof v === "number";
    case "integer":
      return typeof v === "number" && Number.isInteger(v);
    case "boolean":
      return typeof v === "boolean";
    case "null":
      return v === null;
    default:
      return false;
  }
}

function resolveRef(ref: string, rootSchema: any): unknown {
  const defsPrefix = "#/$defs/";
  if (ref.startsWith(defsPrefix)) {
    const schema = rootSchema?.$defs?.[ref.slice(defsPrefix.length)];
    if (!schema) {
      throw new Error(`missing envelope definition ${ref}`);
    }
    return schema;
  }
  const prefix = "#/components/schemas/";
  if (!ref.startsWith(prefix)) {
    throw new Error(`unsupported schema ref ${ref}`);
  }
  const name = ref.slice(prefix.length);
  const schema = generatedSchemas[name];
  if (!schema) {
    throw new Error(`missing generated schema ${name}`);
  }
  return schema;
}

function validate(
  schema: any,
  value: any,
  path: string,
  errors: string[],
  rootSchema: any = schema,
): void {
  if (schema.$ref) {
    validate(resolveRef(schema.$ref, rootSchema), value, path, errors, rootSchema);
    return;
  }
  if (schema.oneOf) {
    const passes = schema.oneOf.filter((sub: any) => {
      const nested: string[] = [];
      validate(sub, value, path, nested, rootSchema);
      return nested.length === 0;
    });
    if (passes.length !== 1) {
      errors.push(`${path}: matched ${passes.length} of oneOf (expected 1)`);
    }
    return;
  }
  if (schema.const !== undefined && value !== schema.const) {
    errors.push(
      `${path}: expected const ${JSON.stringify(schema.const)}, got ${JSON.stringify(value)}`,
    );
  }
  if (schema.enum && !schema.enum.includes(value)) {
    errors.push(`${path}: ${JSON.stringify(value)} not in enum ${JSON.stringify(schema.enum)}`);
  }
  if (schema.type) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (!types.some((t: string) => typeOk(value, t))) {
      errors.push(
        `${path}: expected type ${types.join("|")}, got ${
          value === null ? "null" : Array.isArray(value) ? "array" : typeof value
        }`,
      );
      return;
    }
  }
  if (typeOk(value, "object") && schema.properties) {
    for (const key of schema.required ?? []) {
      if (!(key in value)) {
        errors.push(`${path}: missing required '${key}'`);
      }
    }
    for (const [key, sub] of Object.entries<any>(schema.properties)) {
      if (key in value) {
        validate(sub, value[key], `${path}.${key}`, errors, rootSchema);
      }
    }
    if (schema.additionalProperties === false) {
      for (const key of Object.keys(value)) {
        if (!(key in schema.properties)) {
          errors.push(`${path}: unexpected property '${key}'`);
        }
      }
    }
  }
  if (typeOk(value, "array") && schema.items) {
    value.forEach((item: any, i: number) =>
      validate(schema.items, item, `${path}[${i}]`, errors, rootSchema),
    );
  }
}

function assertRawU64(raw: string, label: string, errors: string[]): void {
  const result = raw.slice(raw.indexOf('"result"'));
  for (const m of result.matchAll(
    /"(?:id|mesh|albedoTexture|skyTexture|texture|entity|parent|parentId|rootBone)"\s*:\s*([^,}\s]+)/g,
  )) {
    const tok = m[1];
    if (tok === "null") {
      continue;
    }
    const quoted = /^"(\d+)"$/.exec(tok);
    if (!quoted) {
      errors.push(`${label}: id token '${tok}' is not a quoted decimal string`);
      continue;
    }
    const digits = quoted[1];
    if (BigInt(digits).toString() !== digits) {
      errors.push(`${label}: id token '${tok}' did not round-trip as BigInt`);
    }
  }
}

let nextId = 1;
function roundTrip(line: string, label: string): Promise<{ envelope: any; raw: string }> {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ path: SOCK });
    let buf = "";
    let settled = false;
    const rejectOnce = (error: Error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      reject(error);
    };
    const timer = setTimeout(() => {
      callTimedOut = true;
      rejectOnce(new Error(`timeout ${label}`));
    }, CALL_TIMEOUT_MS);
    socket.on("connect", () => socket.write(line));
    socket.on("data", (d) => {
      buf += d.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl < 0 || settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      socket.end();
      const raw = buf.slice(0, nl);
      try {
        resolve({ envelope: JSON.parse(raw), raw });
      } catch (error) {
        socket.destroy();
        reject(new Error(`invalid JSON reply ${label}: ${String(error)}`));
      }
    });
    socket.on("error", (e) => {
      rejectOnce(new Error(`control transport failed ${label}: ${String(e)}`));
    });
  });
}

function call(
  cmd: string,
  params: Record<string, unknown> = {},
): Promise<{ envelope: any; raw: string }> {
  return roundTrip(`${JSON.stringify({ id: nextId++, cmd, params })}\n`, `calling ${cmd}`);
}

function callRaw(line: string): Promise<{ envelope: any; raw: string }> {
  return roundTrip(`${line}\n`, "calling raw control request");
}

function failureMessage(envelope: any): string {
  return typeof envelope?.error?.message === "string"
    ? envelope.error.message
    : JSON.stringify(envelope?.error);
}

async function waitForProjectReady(): Promise<void> {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    const status = await call("project-status");
    if (status.envelope.ok !== true) {
      throw new Error(`failed to poll project status: ${failureMessage(status.envelope)}`);
    }
    if (status.envelope.result?.phase === "ready") {
      return;
    }
    if (status.envelope.result?.phase === "failed") {
      throw new Error(`project load failed: ${status.envelope.result.error}`);
    }
    await sleep(100);
  }
  throw new Error("project load did not become ready within 30 seconds");
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function firstResultId(raw: string): string | undefined {
  return raw.match(/"result"\s*:\s*[[{][\s\S]*?"id"\s*:\s*"(\d+)"/)?.[1];
}

function firstAssetId(result: any): string | undefined {
  return result?.assets?.find((asset: any) => asset.type === "mesh")?.id;
}

function schemaForResult(dto: string): unknown {
  const schema = generatedSchemas[dto];
  if (!schema) {
    throw new Error(`manifest result DTO ${dto} is missing from OpenRPC schemas`);
  }
  return schema;
}

async function entityId(name: string): Promise<string> {
  const created = await call("create-entity", { name });
  if (created.envelope.ok !== true) {
    throw new Error(
      `failed to create fixture entity '${name}': ${failureMessage(created.envelope)}`,
    );
  }
  const id = firstResultId(created.raw);
  if (!id) {
    throw new Error(`fixture entity '${name}' had no id`);
  }
  return id;
}

async function meshAssetId(): Promise<string> {
  let assets = await call("list-assets");
  if (assets.envelope.ok !== true) {
    throw new Error(`failed to list assets: ${failureMessage(assets.envelope)}`);
  }
  let id = firstAssetId(assets.envelope.result);
  if (!id) {
    writeFileSync(
      modelFixturePath,
      [
        "o ContractTriangle",
        "v -0.5 0 0",
        "v 0.5 0 0",
        "v 0 1 0",
        "vt 0 0",
        "vt 1 0",
        "vt 0.5 1",
        "vn 0 0 1",
        "f 1/1/1 2/2/1 3/3/1",
        "",
      ].join("\n"),
    );
    const imported = await call("import-model", { path: modelFixturePath });
    if (imported.envelope.ok !== true) {
      throw new Error(`failed to import mesh fixture: ${failureMessage(imported.envelope)}`);
    }
    assets = await call("list-assets");
    id = firstAssetId(assets.envelope.result);
  }
  if (!id) {
    throw new Error("no mesh asset fixture found");
  }
  return id;
}

async function paramsForFixture(
  fixture: string,
  state: { cubeId: string; rtSupported: boolean },
): Promise<Record<string, unknown>> {
  switch (fixture) {
    case "empty":
      return {};
    case "aa":
      return { mode: "fxaa" };
    case "taa-sharpness":
      return { sharpness: 0.5 };
    case "view-mode-wireframe":
      return { mode: "wireframe" };
    case "render-quality":
      return { tier: "medium" };
    case "tonemap":
      return { mode: "agx" };
    case "power-state-focused":
      return { state: "focused" };
    case "toggle-on":
      return { enabled: true };
    case "toggle-off":
      return { enabled: false };
    case "gi-off":
      return { mode: "off" };
    case "new-entity":
      return { name: `Contract Created ${process.pid}` };
    case "temp-entity":
      return { entity: await entityId(`Contract Destroy ${process.pid}`) };
    case "temp-child-under-cube":
      return { entity: await entityId(`Contract Reparent ${process.pid}`), parent: state.cubeId };
    case "temp-camera-entity":
      return { entity: await entityId(`Contract Add Camera ${process.pid}`), component: "Camera" };
    case "temp-camera-component": {
      const entity = await entityId(`Contract Remove Camera ${process.pid}`);
      await call("add-component", { entity, component: "Camera" });
      return { entity, component: "Camera" };
    }
    case "cube-name-component":
      return { entity: state.cubeId, component: "Name", json: { name: "Component Set Cube" } };
    case "cube-component-order": {
      const inspected = await call("inspect", { entity: state.cubeId });
      const components = inspected.envelope.result?.componentOrder;
      if (inspected.envelope.ok !== true || !Array.isArray(components)) {
        throw new Error(
          `failed to read component-order fixture: ${failureMessage(inspected.envelope)}`,
        );
      }
      return {
        entity: state.cubeId,
        components,
      };
    }
    case "cube-transform":
      return { entity: state.cubeId, translation: { x: 1, y: 2, z: 3 } };
    case "temp-directional-light": {
      const light = await call("add-entity", { preset: "directional-light" });
      const id = firstResultId(light.raw);
      if (light.envelope.ok !== true || !id) {
        throw new Error(
          `failed to create directional-light fixture: ${failureMessage(light.envelope)}`,
        );
      }
      return { entity: id, intensity: 3 };
    }
    case "cube-entity":
      return { entity: state.cubeId };
    case "viewport-center":
      return { u: 0.5, v: 0.5 };
    case "spatial-origin":
      return { world: { x: 0, y: 0, z: 0 }, level: 0 };
    case "spatial-sample-cube":
      return {
        provider: state.cubeId,
        channel: "altitude",
        position: { x: 0, y: 0, z: 0 },
      };
    case "environment-intensity":
      return { skyIntensity: 1 };
    case "environment-profile-save":
      return { name: `Contract Environment ${process.pid}` };
    case "environment-profile-update": {
      const saved = await call("save-environment-profile", {
        name: `Contract Environment Update ${process.pid}`,
      });
      const reference = saved.envelope.result?.reference;
      const profile =
        reference && typeof reference === "object" && "id" in reference ? reference.id : null;
      if (saved.envelope.ok !== true || typeof profile !== "string") {
        throw new Error(
          `failed to create environment-profile fixture: ${failureMessage(saved.envelope)}`,
        );
      }
      return { profile };
    }
    case "environment-profile-clear-day":
      return { profile: { kind: "builtin", profile: "clear-day" } };
    case "atmosphere-disabled":
      return { enabled: false };
    case "fog-disabled":
      return {
        enabled: false,
        mode: "volumetric",
        quality: "high",
        historyBlend: 0.05,
        neighborhoodClamp: true,
        lightClamp: 4,
        aerialPerspective: true,
        aerialIntensity: 1.2,
      };
    case "clouds-disabled":
      return { enabled: false };
    case "wind-calm":
      return { speed: 0, gust: 0 };
    case "interaction-impulse":
      return { positionM: [0, 0], radiusM: 2, strength: 3 };
    case "wind-sample-origin":
      return { positionM: [0, 1, 0] };
    case "surface-ray-down":
      return { originM: [0, 5, 0], direction: [0, -1, 0] };
    case "time-of-day-noon":
      return { timeOfDay: 0.5 };
    case "cube-preset":
      return { preset: "cube" };
    case "cube-rename":
      return { entity: state.cubeId, name: "Renamed Contract Cube" };
    case "cube-name-field":
      return { entity: state.cubeId, component: "Name", field: "name", value: "Field Set Cube" };
    case "camera-yaw":
      return { yaw: 12 };
    case "gizmo-rotate-local":
      return { op: "rotate", space: "local" };
    case "gizmo-hover":
      return { phase: "hover", x: 0, y: 0 };
    case "fly-idle":
      return { active: false };
    case "script-input-w":
      return { keys: ["w"] };
    case "viewport-size":
      return { view: "scene", width: 1280, height: 720 };
    case "active-view-scene":
      return { view: "scene" };
    case "exposure-zero":
      return { ev: 0 };
    case "bloom":
      return {
        enabled: true,
        intensity: 0.08,
        scatter: 0.005,
        tint: [1, 1, 1],
        threshold: 0,
        dirtIntensity: 0.5,
        dirtTint: [1, 0.9, 0.8],
        anamorphic: { enabled: true, ratio: 2, tint: [0.6, 0.8, 1], intensity: 0.3 },
        perMipTint: [[1, 0.5, 0.5]],
      };
    case "color-grading":
      return {
        temperature: 5000,
        tint: 0,
        contrast: 1.2,
        pivot: 0.18,
        saturation: 1,
        slope: [1, 1, 1],
        offset: [0, 0, 0],
        power: [1, 1, 1],
        creativeLutAsset: 0,
        creativeLutIntensity: 0,
      };
    case "bake-look":
      return { name: "Contract Baked Look" };
    case "tess-quality":
      return { factorCap: 16, minFactor: 1, edgeLengthTarget: 12 };
    case "new-project":
      return { name: `contract-second-${process.pid}`, displayName: "Contract Second Project" };
    case "project-name":
      return { path: `contract-second-${process.pid}` };
    case "mesh-asset":
      return { asset: await meshAssetId() };
    case "mesh-asset-view":
      return { asset: await meshAssetId(), size: 64 };
    case "thumbnail-cache-stats":
      return { action: "stats" };
    case "mesh-asset-rename":
      return { asset: await meshAssetId(), name: `contract-mesh-${process.pid}` };
    case "cube-mesh-asset": {
      const entity = await entityId(`Contract Mesh Assign ${process.pid}`);
      return { entity, slot: "mesh", asset: await meshAssetId() };
    }
    case "skeleton-overlay-on":
      return { show: true, axes: true, jointSize: 5 };
    case "debug-overlays-bounds":
      return { bounds: true };
    case "stores-polyhaven":
      return { enabled: ["polyhaven"] };
    case "step-one":
      return { frames: 1 };
    case "profiler-timestamps":
      return { mode: "timestamps" };
    case "capture-single":
      return { mode: "single" };
    case "frame-history-samples":
      return { samples: 16 };
    case "perf-config-30":
      // The frame budget / dynamic resolution moved to set-upscale; set-perf-config now carries
      // only the alarm thresholds.
      return { greenBudgetFrac: 0.5 };
    case "upscale":
      return { ratio: 0.67, dynamic: true, targetMs: 16.7 };
    case "alarms-since-0":
      return { since: 0 };
    case "script-schema-file": {
      // get-script-schema reads <projectRoot>/src/<path>; author the script there.
      // The engine runs with cwd HERE, so a relative root resolves against it.
      const project = await call("get-project");
      const root = (project.envelope.result as { root: string }).root;
      const src = join(isAbsolute(root) ? root : join(HERE, root), "src");
      mkdirSync(src, { recursive: true });
      writeFileSync(
        join(src, "contract-schema.lua"),
        'local C = {}\nC.properties = { speed = 2.0, label = "x" }\nfunction C.on_update(self, dt) end\nreturn C\n',
      );
      return { path: "contract-schema.lua" };
    }
    case "script-override-slot": {
      const entity = await entityId(`Contract Script ${process.pid}`);
      await call("add-component", { entity, component: "Script" });
      await call("set-component", {
        entity,
        component: "Script",
        json: { scripts: [{ scriptPath: "contract-schema.lua", overrides: {} }] },
      });
      return { entity, slot: 0, name: "speed", value: 9 };
    }
    default:
      throw new Error(`unknown manifest fixture '${fixture}'`);
  }
}

async function runContract(): Promise<number> {
  if (!existsSync(ENGINE)) {
    console.error(`engine binary not found: ${ENGINE}`);
    return 2;
  }
  const proc = Bun.spawn([ENGINE], {
    cwd: HERE,
    env: {
      ...process.env,
      SAFFRON_CONTROL_SOCK: SOCK,
      SAFFRON_APPDATA_DIR: appDataPath,
      SAFFRON_SCRATCH_PROJECT: "1",
      // No window, no compositor: the host takes the no-surface offscreen device, so the
      // contract test runs anywhere the GPU (or llvmpipe) does.
      SAFFRON_EDITOR_NATIVE_VIEWPORT: "1",
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const logDrains = [drainHostStream(proc.stdout, hostLog), drainHostStream(proc.stderr, hostLog)];

  try {
    let up = false;
    for (let i = 0; i < 60 && !up; i++) {
      if (proc.exitCode !== null) {
        await Promise.all(logDrains);
        console.error(
          withHostLog(`engine exited early (code ${proc.exitCode})`, hostLog, HOST_LOG_TAIL),
        );
        return 2;
      }
      if (existsSync(SOCK)) {
        try {
          await call("ping");
          up = true;
        } catch {
          /* not ready yet; keep polling */
        }
      }
      if (!up) {
        await sleep(500);
      }
    }
    if (!up) {
      console.error(withHostLog("engine control socket never came up", hostLog, HOST_LOG_TAIL));
      return 2;
    }

    let projectReady = false;
    for (let i = 0; i < 120 && !projectReady; i++) {
      const status = await call("project-status");
      if (status.envelope.ok !== true) {
        console.error(
          withHostLog(
            `project-status failed during startup: ${failureMessage(status.envelope)}`,
            hostLog,
            HOST_LOG_TAIL,
          ),
        );
        return 2;
      }
      const phase = status.envelope.result?.phase;
      if (phase === "failed") {
        console.error(
          withHostLog(
            `scratch project failed to load: ${status.envelope.result?.error ?? ""}`,
            hostLog,
            HOST_LOG_TAIL,
          ),
        );
        return 2;
      }
      projectReady = phase === "ready";
      if (!projectReady) {
        await sleep(100);
      }
    }
    if (!projectReady) {
      console.error(withHostLog("scratch project did not become ready", hostLog, HOST_LOG_TAIL));
      return 2;
    }

    const errors: string[] = [];
    const checked: string[] = [];

    const help = await call("help");
    if (help.envelope.ok !== true || !Array.isArray(help.envelope.result?.commands)) {
      errors.push("help: expected ok:true with result.commands");
    } else {
      const live = new Set(help.envelope.result.commands.map((command: any) => command.name));
      const known = new Set([
        ...manifest.commands.map((command) => command.name),
        ...manifest.skips.map((skip) => skip.name),
      ]);
      for (const name of live) {
        if (!known.has(name)) {
          errors.push(
            `manifest completeness: live help command '${name}' is missing from manifest`,
          );
        }
      }
      for (const name of known) {
        if (!live.has(name)) {
          errors.push(`manifest completeness: manifest command '${name}' is missing from help`);
        }
      }
      checked.push(`help <-> manifest (${live.size} commands)`);
    }

    const cube = await call("add-entity", { preset: "cube" });
    if (cube.envelope.ok !== true) {
      errors.push(`fixture add-entity: ${failureMessage(cube.envelope)}`);
      throw new Error("cannot seed cube fixture");
    }
    const cubeId = firstResultId(cube.raw);
    if (!cubeId) {
      errors.push("fixture add-entity: no id");
      throw new Error("cannot seed cube fixture id");
    }
    const capabilityStats = await call("render-stats");
    if (capabilityStats.envelope.ok !== true) {
      throw new Error(
        `failed to read renderer capabilities: ${failureMessage(capabilityStats.envelope)}`,
      );
    }
    const state = {
      cubeId,
      rtSupported: capabilityStats.envelope.result?.rtSupported === true,
    };

    for (const command of manifest.commands) {
      if (command.skip) {
        checked.push(`${command.name} skipped (${command.skip})`);
        continue;
      }
      if (!command.fixture) {
        errors.push(`${command.name}: missing fixture in manifest`);
        continue;
      }
      const params = await paramsForFixture(command.fixture, state);
      const { envelope, raw } = await call(command.name, params);
      validate(envelopeSchema, envelope, `${command.name} envelope`, errors);
      if (RT_COMMANDS.has(command.name) && !state.rtSupported) {
        if (
          envelope.ok !== false ||
          envelope.error?.code !== "command" ||
          envelope.error?.message !== "ray tracing not supported on this device"
        ) {
          errors.push(`${command.name}: expected the unsupported-device error`);
        } else {
          checked.push(`${command.name} -> unsupported-device envelope`);
        }
        continue;
      }
      if (envelope.ok !== true) {
        errors.push(`${command.name}: ok=${envelope.ok} error=${failureMessage(envelope)}`);
        continue;
      }
      validate(schemaForResult(command.result), envelope.result, command.name, errors);
      assertRawU64(raw, command.name, errors);
      checked.push(`${command.name} -> ${command.result}`);
      if (PROJECT_TRANSITIONS.has(command.name)) {
        await waitForProjectReady();
      }
    }

    // Hierarchy round-trip: reparent a fresh entity under a fresh parent (the loop's
    // project commands may have replaced the seeded scene), see parentId on the list,
    // refuse a cycle, then detach back to root.
    {
      const hierParentId = await entityId(`Contract Hierarchy Parent ${process.pid}`);
      const childId = await entityId(`Contract Hierarchy Child ${process.pid}`);
      const reparent = await call("set-parent", { entity: childId, parent: hierParentId });
      if (reparent.envelope.ok !== true) {
        errors.push(`hierarchy set-parent: ${failureMessage(reparent.envelope)}`);
      }
      const listed = await call("list-entities");
      validate(
        schemaForResult("EntityList"),
        listed.envelope.result,
        "hierarchy list-entities",
        errors,
      );
      assertRawU64(listed.raw, "hierarchy list-entities", errors);
      const entry = (listed.envelope.result as any)?.entities?.find((e: any) => e.id === childId);
      if (!entry || entry.parentId !== hierParentId) {
        errors.push(
          `hierarchy: child ${childId} should list parentId ${hierParentId}, got ${entry?.parentId}`,
        );
      } else {
        checked.push("set-parent -> list-entities parentId round-trip");
      }

      const cycle = await call("set-parent", { entity: hierParentId, parent: childId });
      if (
        cycle.envelope.ok !== false ||
        cycle.envelope.error?.code !== "command" ||
        typeof cycle.envelope.error?.message !== "string" ||
        cycle.envelope.error.message.length === 0
      ) {
        errors.push("hierarchy: parenting an entity under its own child must fail (cycle)");
      } else {
        checked.push("set-parent cycle -> envelope (ok:false)");
      }

      const detach = await call("set-parent", { entity: childId, parent: "0" });
      if (detach.envelope.ok !== true) {
        errors.push(`hierarchy detach: ${failureMessage(detach.envelope)}`);
      }
      const relisted = await call("list-entities");
      const detached = (relisted.envelope.result as any)?.entities?.find(
        (e: any) => e.id === childId,
      );
      if (!detached || "parentId" in detached) {
        errors.push("hierarchy: detached child must carry no parentId");
      } else {
        checked.push("set-parent detach -> root (no parentId)");
      }
    }

    const bad = await call("definitely-not-a-command");
    validate(envelopeSchema, bad.envelope, "bad-command", errors);
    if (
      bad.envelope.ok !== false ||
      bad.envelope.error?.code !== "command" ||
      typeof bad.envelope.error?.message !== "string"
    ) {
      errors.push("bad-command: expected a typed command failure");
    } else {
      checked.push("bad-command -> envelope (ok:false)");
    }

    const invalid = await callRaw("{not json");
    validate(envelopeSchema, invalid.envelope, "invalid-request", errors);
    if (
      invalid.envelope.id !== null ||
      invalid.envelope.ok !== false ||
      invalid.envelope.error?.code !== "invalid-request" ||
      invalid.envelope.error?.message !== "invalid JSON request"
    ) {
      errors.push("invalid-request: expected the generated invalid-request failure");
    } else {
      checked.push("invalid JSON -> typed invalid-request envelope");
    }

    const diagnosticFixture = {
      id: "schema-fixture",
      ok: false,
      error: {
        code: "diagnostic",
        message: "graph candidates limit exceeded: requested 16, limit 4",
        diagnostic: {
          domain: "vegetation-graph",
          detail: {
            category: "limit",
            resource: "candidates",
            requested: "16",
            limit: "4",
          },
        },
      },
    };
    validate(envelopeSchema, diagnosticFixture, "diagnostic-fixture", errors);
    checked.push("structured graph diagnostic -> generated envelope schema");

    const legacyFailureErrors: string[] = [];
    validate(
      envelopeSchema,
      { id: "schema-fixture", ok: false, error: "graph failed" },
      "string-failure-fixture",
      legacyFailureErrors,
    );
    if (legacyFailureErrors.length === 0) {
      errors.push("string-failure-fixture: generated envelope accepted the retired string shape");
    } else {
      checked.push("string failure shape -> rejected by generated envelope schema");
    }

    for (const item of checked) {
      console.log(`  ok  ${item}`);
    }
    if (errors.length) {
      console.error(`\n${errors.length} contract failure(s):`);
      for (const error of errors) {
        console.error(`  FAIL ${error}`);
      }
      return 1;
    }
    console.log(`\nall ${checked.length} manifest-driven control checks passed`);
    return 0;
  } finally {
    if (proc.exitCode === null && !callTimedOut) {
      await call("quit").catch(() => {});
    }
    if (proc.exitCode === null) {
      proc.kill("SIGTERM");
    }
    await proc.exited;
    await Promise.all(logDrains);
  }
}

async function main(): Promise<number> {
  const ownsAppData = process.env.SAFFRON_APPDATA_DIR === undefined;
  try {
    appDataPath =
      process.env.SAFFRON_APPDATA_DIR ?? mkdtempSync(join(tmpdir(), "saffron-contract-appdata."));
    fixturesPath = mkdtempSync(join(tmpdir(), "saffron-contract-fixtures."));
    modelFixturePath = join(fixturesPath, "contract-triangle.obj");
    return await runContract();
  } finally {
    if (fixturesPath) {
      rmSync(fixturesPath, { recursive: true, force: true });
    }
    if (ownsAppData && appDataPath) {
      rmSync(appDataPath, { recursive: true, force: true });
    }
  }
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    const message = err instanceof Error ? (err.stack ?? err.message) : String(err);
    console.error(withHostLog(message, hostLog, HOST_LOG_TAIL));
    process.exit(2);
  });
