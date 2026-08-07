+++
title = 'Ecology and telemetry panels'
weight = 21
+++

# Ecology and telemetry panels

Two panels in the Diagnostics group of the Tools menu cover the vegetation runtime while a scene is loaded. The Ecology Timeline drives the world's biological clock; the Vegetation Telemetry panel reports where that runtime's time and memory go and which content is over budget.

## Stepping biological time

[Biological time](../../scene-and-ecs/ecology-catchup/) only moves forward by executing ticks. There is no analytical fast-forward to scrub against, so the transport is step and run rather than a seek bar; a slider would promise something the simulation cannot do.

Step advances one tick. Run keeps advancing until it is pressed again, and Refresh re-reads the status without advancing anything.

Above the transport sits the world clock, which is what ages biology while play runs. Starting it hands the ticks to the world; stopping it holds biology still, arrears included, so step and run are the only thing moving time. The water and warmth fields belong to that clock rather than to a single advance: one place holds the conditions every tick runs under, whether the clock earned it or the transport asked for it.

A run is chunked at eight ticks per call, which keeps a catch-up that owes thousands interruptible and lets the panel repaint while it works. One call executes at most 64 ticks, which bounds how long the engine holds the frame.

```sh
sa vegetation-ecology-status
sa vegetation-ecology-clock --running false --water 32768 --warmth 32768
# a biological tick crosses the wire as a string, so the quotes are part of the value
sa vegetation-advance-ecology --targetTick '"120"' --maxTicks 8
```

The status readout names the world tick, the rule-set version the ticks ran under, the dependency-region radius in cells, the region count, and the checkpoint generation. Beside those sit two separate counts: regions that owe work, and regions waiting on residency.

The region table repeats that distinction per row as `current`, `behind`, or `waiting`. A region only advances while every cell it spans is resident, so ground that has not loaded is a different problem from work the region owes. Collapsing them into one status would hide which of the two is holding the world back. After an advance the panel also shows the ticks it ran, the ticks still owed, and how many regions caught up.

## Where the frame's vegetation time goes

The [telemetry](../../scene-and-ecs/vegetation-telemetry/) panel re-reads at 1 Hz. The numbers are compact counters and averaged stage times, so a poll is a handful of values over the socket. A world with no bound vegetation runtime answers with an error rather than zeros, and the panel shows that in place instead of raising a toast every second.

| Group | Content |
|---|---|
| Stage times | Residency, promotion, collision, navigation, and ecology microseconds, averaged, plus the total and the last sync |
| Resident bytes | Per facet: render, physics, simulation, editing, navigation, network |
| Work | Synchronizations, queries, query hits, mutations, ecology ticks, collision bodies, navigation contributions, promoted plants |
| Cook queue | Live, completed, cancelled, superseded, and failed jobs |

Stage times answer where the frame's vegetation work went. They are per stage rather than one frame total, because a frame that got slower is only useful if it says which stage slowed.

## Budgets name the content

The budget half answers whose content is responsible when the cost is too high. The panel edits plants per cell and instances per family, and commits each on blur; zero turns that budget off.

```sh
sa vegetation-budgets --cellPlants 4000
# { "cellPlants": 4000, "familyInstances": 16384, "familyMicroPredicted": "4000000" }
```

A breach raises an alarm naming the cell or family that broke it, not the pass that noticed. The panel filters the active-alarm list to metrics with the `vegetation-` prefix and shows each one's owner, metric, and value against its threshold. The same alarms reach the [metrics dashboard](../metrics-dashboard/) and the editor's toast stream.

## In the code

| What | File | Symbols |
|---|---|---|
| Clock transport and region table | `editor/src/panels/EcologyTimelinePanel.tsx` | `EcologyTimelinePanel`, `RUN_CHUNK`, `MAX_TICKS_PER_CALL`, `configure`, `advance`, `run` |
| Stage times, counters, and budgets | `editor/src/panels/VegetationTelemetryPanel.tsx` | `VegetationTelemetryPanel`, `REFRESH_MS`, `commitBudget` |
| Panel registration | `editor/src/components/dock/panelRegistry.tsx` | `SCENE_PANEL_REGISTRY` |
| Typed commands | `editor/src/control/client/vegetation.ts` | `vegetationEcologyStatus`, `vegetationEcologyClock`, `vegetationAdvanceEcology`, `vegetationTelemetry`, `vegetationBudgets` |
| Ecology and telemetry command handlers | `engine/crates/control/src/commands_vegetation_runtime/` | `register_runtime_vegetation_commands` |

## Related

- [Ecology ticks and catch-up](../../scene-and-ecs/ecology-catchup/) — one tick, dependency regions, and how budgets delay work
- [Vegetation telemetry](../../scene-and-ecs/vegetation-telemetry/) — the counters, stage times, and budget alarms behind the panel
- [Vegetation mode](../vegetation-mode/) — the authoring surface whose cooks and strokes show up in these counters
- [Metrics dashboard](../metrics-dashboard/) — frame graphs, pass timings, and the shared alarm list
- [Plant promotion](../../scene-and-ecs/plant-promotion/) — what the promotion stage time and promoted count are measuring
