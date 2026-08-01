// Read the recorded Phase 1 budgets back. Measure this machine with the same fixture that produced
// the records in `benchmarks/foliage-veg/`, then hold the fresh p95s and counters to the ceilings
// recorded for this exact device.
//
// A ceiling is compared with no tolerance of its own. `budgets` is already
// `max(p95 * 1.25, p95 + floor)` by derivation, so a second allowance would compound into a
// ceiling only a doubling could breach — the regression this step exists to catch would pass. What
// a breach must do instead is reproduce: a 32-frame capture on a shared machine is a noisy
// estimator, so a leg that goes over is measured again and fails only if it goes over twice. That
// discards a contaminated sample without ever moving the threshold.
//
// Exit codes: 0 the machine is inside its record, 1 a leg exceeded it twice, 2 deferred (a device
// this checkout has no record for, or one whose numbers are not comparable to any record's).

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { REPO } from "../../tests/e2e/harness.ts";
import type { RenderStatsDto } from "../../editor/src/protocol/sa-types.ts";
import {
  measureBaseline,
  type BaselineBudgets,
  type BaselineFixture,
  type BaselineObserved,
  type BaselinePlatform,
  type BaselineRecord,
} from "./baseline.ts";

const RECORD_DIRECTORY = join(REPO, "benchmarks", "foliage-veg");

// What the harness builds, as opposed to what the capture returned: `captureFrames` is an outcome,
// and a run that lands a frame short of the record still measures the same scene.
const FIXTURE_INPUTS = [
  "name",
  "primitiveInstances",
  "primitiveCycle",
  "pointLights",
  "spotLights",
  "starterDirectionalLights",
  "viewport",
] as const satisfies readonly (keyof BaselineFixture)[];

// Below this the p95 stops being a percentile.
const MINIMUM_CAPTURE_FRAMES = 16;

type Threshold = Exclude<keyof BaselineBudgets, "derivation">;

interface Leg {
  readonly threshold: Threshold;
  readonly label: string;
  readonly unit: string;
  readonly live: (observed: BaselineObserved) => number;
}

// Only the p95 legs are stable enough to grade: a record's own p99 sits an order of magnitude above
// its p95 because the capture window includes pipeline compilation.
const LEGS: readonly Leg[] = [
  {
    threshold: "sceneGatherP95Ms",
    label: "scene gather p95",
    unit: "ms",
    live: (observed) => observed.sceneGatherMs.p95,
  },
  {
    threshold: "cpuFrameP95Ms",
    label: "cpu frame p95",
    unit: "ms",
    live: (observed) => observed.cpuFrameMs.p95,
  },
  {
    threshold: "gpuFrameP95Ms",
    label: "gpu frame p95",
    unit: "ms",
    live: (observed) => observed.gpuFrameMs.p95,
  },
  {
    threshold: "drawCallsMax",
    label: "draw calls",
    unit: "",
    live: (observed) => observed.drawCalls,
  },
  {
    threshold: "instanceUploadBytesMax",
    label: "instance upload",
    unit: "B",
    live: (observed) => observed.instanceUploadBytes,
  },
  {
    threshold: "retainedMeshCpuBytesMax",
    label: "retained mesh cpu",
    unit: "B",
    live: (observed) => observed.retainedMeshCpuBytes,
  },
];

class Deferral extends Error {}

function defer(reason: string): never {
  process.stdout.write(`DEFER: ${reason}\n`);
  process.exit(2);
}

// The record set keys on the physical device, not on the vendor: a ceiling measured on one card is
// not an acceptance threshold for another card from the same vendor.
function deviceKey(platform: BaselinePlatform): string {
  return `${platform.os}/${platform.gpu.toLowerCase().replace(/\s+/g, " ").trim()}`;
}

function loadRecords(): BaselineRecord[] {
  return readdirSync(RECORD_DIRECTORY)
    .filter((name) => name.startsWith("phase-1-") && name.endsWith(".json"))
    .map(
      (name) => JSON.parse(readFileSync(join(RECORD_DIRECTORY, name), "utf8")) as BaselineRecord,
    );
}

function format(value: number, unit: string): string {
  return unit === "ms" ? `${value.toFixed(3)} ms` : `${value}${unit === "" ? "" : ` ${unit}`}`;
}

function measure(): Promise<BaselineRecord> {
  return measureBaseline({
    precheck: (stats: RenderStatsDto) => {
      if (stats.softwareGpu) {
        throw new Deferral("the reachable Vulkan device is the software rasterizer");
      }
    },
  }).catch((error: unknown) => {
    if (error instanceof Deferral) defer(error.message);
    throw error;
  });
}

// Defers unless this run measured the same scene on the same device as the record it will be
// graded against.
function requireComparable(record: BaselineRecord, measured: BaselineRecord): void {
  for (const field of FIXTURE_INPUTS) {
    const recorded = JSON.stringify(record.fixture[field]);
    const live = JSON.stringify(measured.fixture[field]);
    if (recorded !== live) {
      defer(`this run built ${field} ${live} where the record measured ${recorded}`);
    }
  }
  if (measured.fixture.captureFrames < MINIMUM_CAPTURE_FRAMES) {
    defer(
      `the capture returned ${measured.fixture.captureFrames} frames, too few for a p95 comparison`,
    );
  }
  if (record.platform.rtSupported !== measured.platform.rtSupported) {
    defer(
      `ray tracing is ${measured.platform.rtSupported ? "available" : "unavailable"} here and was ` +
        `${record.platform.rtSupported ? "available" : "unavailable"} when the record was captured`,
    );
  }
  if (measured.platform.profilerMode !== "timestamps" || !measured.platform.timestampsSupported) {
    defer("this device serves no GPU timestamps, so the frame-time legs are not measurable");
  }
}

function grade(record: BaselineRecord, measured: BaselineRecord): Map<Threshold, string> {
  process.stdout.write(
    `${measured.platform.gpu}: ${measured.fixture.captureFrames} frames captured, ` +
      `${record.fixture.captureFrames} in the record\n`,
  );
  const over = new Map<Threshold, string>();
  for (const leg of LEGS) {
    const ceiling = record.budgets[leg.threshold];
    const live = leg.live(measured.observed);
    process.stdout.write(
      `  ${live > ceiling ? "OVER " : "ok   "} ${leg.label.padEnd(18)} ` +
        `${format(live, leg.unit)} against ${format(ceiling, leg.unit)}\n`,
    );
    if (live > ceiling) {
      over.set(
        leg.threshold,
        `${leg.label} ${format(live, leg.unit)} exceeds ${format(ceiling, leg.unit)}`,
      );
    }
  }
  return over;
}

const measured = await measure();
const key = deviceKey(measured.platform);
const matches = loadRecords().filter((record) => deviceKey(record.platform) === key);
if (matches.length === 0) {
  defer(`no phase-1 record was captured on ${measured.platform.gpu} (${measured.platform.os})`);
}
if (matches.length > 1) {
  throw new Error(`${matches.length} phase-1 records claim ${key}`);
}
const record = matches[0];
requireComparable(record, measured);

let breaches = grade(record, measured);
if (breaches.size > 0) {
  process.stdout.write("measuring again to see whether the breach reproduces\n");
  const again = await measure();
  requireComparable(record, again);
  const repeated = grade(record, again);
  breaches = new Map([...repeated].filter(([threshold]) => breaches.has(threshold)));
}

if (breaches.size > 0) {
  process.stdout.write(
    `FAILED: ${measured.platform.gpu} regressed against its phase-1 record\n${[...breaches.values()]
      .map((breach) => `  - ${breach}\n`)
      .join("")}`,
  );
  process.exit(1);
}
process.stdout.write(`within the phase-1 budgets recorded for ${measured.platform.gpu}\n`);
