// Record the Phase 1 foliage baseline for this machine on standard output. Name the file for the
// device it measures: `just bench-foliage-phase1 benchmarks/foliage-veg/phase-1-<device>.json`.

import { measureBaseline } from "./baseline.ts";

process.stdout.write(`${JSON.stringify(await measureBaseline(), null, 2)}\n`);
