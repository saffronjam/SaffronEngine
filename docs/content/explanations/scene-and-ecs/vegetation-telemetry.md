+++
title = 'Vegetation telemetry'
weight = 17
+++

# Vegetation telemetry

A scene with a million plants is exactly where you need to know what the vegetation runtime is
spending, and exactly where a diagnostic that walks every plant stops being usable. So the runtime
keeps compact counters and stage durations, and nothing else: no per-instance readback, no pass over a
resident cell to answer a question.

## Stage times, not a frame total

Every synchronization is timed by stage, because a frame that got slower is only useful if it says
which stage did:

| Stage | Spends on |
|---|---|
| `residency` | Deciding which cells are wanted, taking arrivals, retiring departures |
| `promotion` | Committing promotion and demotion transitions |
| `collision` | Deriving and batching proxy bodies |
| `navigation` | Republishing changed cells on the navigation seam |
| `ecology` | Executing owed ecology ticks |

The stages accumulate into a pending block that publishes only when the synchronization closes, so a
half-timed frame is never reported as a frame. Alongside the last sample sits an eighth-weighted
exponential average — one multiply per stage, no history to walk.

```sh
sa vegetation-telemetry
#   last     residency=0.42ms  promotion=0.00ms  collision=0.11ms  nav=0.03ms  ecology=0.00ms  total=0.56ms
#   average  residency=0.39ms  promotion=0.01ms  collision=0.09ms  nav=0.04ms  ecology=0.00ms  total=0.53ms
#   syncs=1284  queries=17 (hits 402)  mutations=3 (612 bytes)  snapshots=1 (48210 bytes)  ecologyTicks=96
#   bodies=142  navContributions=88  promoted=1
#   cook live=0  submitted=9  completed=8  cancelled=1  superseded=0  failed=0
```

## Counters that cost what they measure

The work counters accumulate from the moment a world is bound and reset when a different one binds.
Each is incremented where the work happens, so the count cannot drift from the work:

- a query records itself and how many plants it returned, because the hit count is what makes a query
  expensive;
- a mutation records the exact canonical bytes the reducer hashed, not the JSON the wire carried;
- a snapshot records the bytes it produced;
- an ecology tick records itself as it executes.

Resident bytes by facet, collision bodies, navigation contributions, and promoted plants come from the
authorities that already track them, so the telemetry command adds no bookkeeping of its own.

The cook queue reports the same way: submitted, completed, cancelled, superseded, failed, live, and
summed acceptance-to-terminal latency. A tally on the poll the manager already walks its jobs in counts
each job's terminal state exactly once, so reading the queue's state costs seven numbers rather than
every retained job's payload.

## Why no per-instance mode

An instrumentation path that reads back per-instance data every frame changes the thing it measures
and gets slower as the scene grows. Per-plant detail is available, but through an explicit request —
`vegetation-runtime-inspect` for one plant, `vegetation-runtime-query` for a region,
`vegetation-cell-inspect` for a cell generation. Those are modes a caller enters deliberately, not a
tax every frame pays.

## In the code

| What | File | Symbols |
|---|---|---|
| Counters and stage timing | `runtime/src/vegetation_telemetry.rs` | `VegetationTelemetry`, `VegetationStage`, `VegetationStageTimes` |
| Stage instrumentation | `runtime/src/session.rs` | `synchronize_vegetation` |
| Control surface | `control/src/commands_vegetation_runtime.rs` | `vegetation-telemetry` |
| Canonical mutation size | `vegetation/src/mutation.rs` | `VegetationMutationRecord::canonical_byte_len` |

## Related

- [Vegetation state](../vegetation-state/) — the residency report the byte figures come from
- [Plant promotion](../plant-promotion/) — the promotion counters
- [Vegetation navigation](../vegetation-navigation/) — the contribution counters
- [Ecology ticks and catch-up](../ecology-catchup/) — the tick counter
