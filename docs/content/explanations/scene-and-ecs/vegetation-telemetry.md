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
#   last     residency=0.42ms  promotion=0.00ms  collision=0.11ms  nav=0.03ms  ecology=0.18ms  total=0.74ms
#   average  residency=0.39ms  promotion=0.01ms  collision=0.09ms  nav=0.04ms  ecology=0.06ms  total=0.59ms
#   syncs=1284  queries=17 (hits 402, cells 34, nodes 511, rows 1980)  mutations=3 (612 bytes)
#   snapshots=1 (48210 bytes)  ecologyTicks=96
#   bodies=142  navContributions=88  promoted=1
#   cook live=0  submitted=9  completed=8  cancelled=1  superseded=0  failed=0
```

## Counters that cost what they measure

The work counters accumulate from the moment a world is bound and reset when a different one binds.
Each is incremented where the work happens, so the count cannot drift from the work:

- a query records itself, the plants it matched, and the traversal it paid for them — the resident
  generations it walked, the bounds-hierarchy nodes it tested, and the macro rows it tested exactly;
- a mutation records the exact canonical bytes the reducer hashed, not the JSON the wire carried;
- a snapshot records the bytes it produced;
- an ecology tick records itself as it executes, whether the world clock earned it or an explicit
  step asked for it.

Resident bytes by facet, collision bodies, navigation contributions, and promoted plants come from the
authorities that already track them, so the telemetry command adds no bookkeeping of its own.

## Why the traversal terms are reported at all

Cosmetic grass outnumbers macro plants by orders of magnitude, and the whole design rests on it never
reaching the CPU: micro vegetation is a quantized density field, so a cell that reconstructs fifty
thousand blades occupies the same bytes as one that reconstructs fifty, and the bounds hierarchy the
queries walk is built over macro rows alone. Hit counts alone cannot show that holding — a query that
scanned every blade and discarded the result returns exactly the same plants. The node and row counts
are the terms that would move, so they are the ones reported, and
`cpu_bytes_and_query_work_track_macro_rows_never_micro_blade_count` holds two worlds identical but for
a sixty-thousand-fold difference in blade density to the same resident bytes and the same traversal.

The cook queue reports the same way: submitted, completed, cancelled, superseded, failed, live, and
summed acceptance-to-terminal latency. A tally on the poll the manager already walks its jobs in counts
each job's terminal state exactly once, so reading the queue's state costs seven numbers rather than
every retained job's payload.

## Budgets name the content, not the pass

Stage times say the residency stage got slower. They cannot say which cell filled up or which family
filled it, and that is the question an author actually has to answer. So the resident population is
measured against three budgets — plants per cell, instances per family, and a family's cooked
blade-candidate upper bound — and a breach raises an alarm through the same machinery the renderer's
frame-budget and VRAM detectors use, carrying the cell coordinates or the family's catalog name as its
owner.

The renderer could not compute these itself: it sees passes and counters, not cells and families, and
has no vegetation dependency to grow. The breach is derived where the population is known and handed
to the alarm state as an owned budget, so coalescing, escalation, and the `FIRING`/`RESOLVED` event
stream apply unchanged. The owner is part of the alarm fingerprint, so two cells over the same budget
stay two alarms rather than collapsing into whichever breached last.

Resolution is by absence: the complete set of live breaches is published every frame, and an alarm
whose breach stops being reported resolves. A reporter that published only on breach would leave its
alarms firing after the condition cleared.

```sh
sa vegetation-budgets --cellPlants 4096 --familyInstances 16384
sa vegetation-budgets                     # omit every field to read
sa list-active-alarms -o json | jq '.alarms[] | select(.metric | startswith("vegetation-"))'
```

The defaults are generous on purpose. The point is to catch content that has run away, not to narrate
an ordinary scene; a project tightens them to what it intends to ship. A budget of zero turns that
budget off.

The Vegetation Telemetry panel shows the same three things together — stage times, resident bytes, and
the live breaches with their owners — so the numbers are noticed rather than only being available to
someone who already suspected a problem.

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
| Control surface | `control/src/commands_vegetation_runtime.rs` | `vegetation-telemetry`, `vegetation-budgets` |
| Owned budget breaches | `assets/src/gpu_scene_mirror.rs` | `VegetationBudgets`, `vegetation_budget_breaches` |
| Owned alarms | `rendering/src/frame_history.rs` | `OwnedBudgetBreach`, `AlarmKey`, `ActiveAlarm::owner` |
| Editor panel | `editor/src/panels/VegetationTelemetryPanel.tsx` | `VegetationTelemetryPanel` |
| Canonical mutation size | `vegetation/src/mutation.rs` | `VegetationMutationRecord::canonical_byte_len` |

## Related

- [Vegetation state](../vegetation-state/) — the residency report the byte figures come from
- [Plant promotion](../plant-promotion/) — the promotion counters
- [Vegetation navigation](../vegetation-navigation/) — the contribution counters
- [Ecology ticks and catch-up](../ecology-catchup/) — the tick counter
