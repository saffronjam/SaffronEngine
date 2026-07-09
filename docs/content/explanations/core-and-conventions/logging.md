+++
title = 'Logging'
weight = 5
+++

# Logging

Logging is the act of writing a tagged diagnostic line to a stream so a running program reports
what it is doing. Anima logs through [`tracing`](https://docs.rs/tracing): call sites emit events
with the `tracing::{info, warn, error, debug, trace}!` macros, and a single subscriber — installed
once per process by `saffron-log` — renders them as one compact, colored line. Routing the events
to a file or the editor UI later is one extra layer on that subscriber, not a change at any call
site.

This pairs with the engine's other diagnosis surfaces — Vulkan validation layers and the
[`sa` control plane](../../tooling-and-control/control-plane-architecture/).

## The line format

Every line has the shape

```
12:30:01.234  INFO   rendering  vulkan ready — gpu 'RTX 3070 Ti' (discrete)
12:30:01.235  WARN   assets     decode error: cannot decode '…smodel'
12:30:01.236  INFO   script     [entity=42] ran on_update
```

— a millisecond wall-clock timestamp, the level (colored on a real terminal), the **subsystem**
column, any **span context** in brackets, then the message. The subsystem is derived from the
event's target: the emitting crate with its `saffron_` prefix stripped (`saffron_rendering::renderer`
→ `rendering`). The level defaults to `debug` and up; `RUST_LOG` overrides it per target
(`RUST_LOG=saffron_script=trace`).

Color is gated on whether stdout is a terminal, so a human sees colored levels while piped or
captured output (the e2e harness, the CI smoke) stays plain ASCII — which is what keeps the
validation gate's `grep` stable.

## Subsystem from the target, context from spans

A call site never spells its tag — `tracing` sets the event target to the module path, and the
formatter reduces it to the subsystem:

```rust
tracing::info!("loaded {n} meshes");        // → … rendering  loaded 12 meshes
tracing::error!("failed to create window: {err}");
```

Per-event context comes from **spans**. The script runtime enters
`info_span!("script", entity = …)` around every handler call, so every line emitted under a script
— including engine-side warnings the script triggered — carries `[entity=42]`. Adding richer
context later (which script, which slot) is one more span field, with no formatter change.

A component speaking on another's behalf sets the target explicitly. The Vulkan debug messenger does
exactly this — `tracing::error!(target: "vulkan", …)` — so its lines read `… vulkan  [validation] …`
even though the code lives in `saffron-rendering`.

## The Vulkan messenger funnels here too

Validation-layer and loader messages arrive through `ash`'s debug-utils callback (`debug_callback`
in `saffron-rendering`'s `device.rs`) and come out as one event tagged `vulkan`. A separate
process-wide counter, `validation_issue_count`, tallies validation/performance messages at
warning-or-error severity; the validation-clean smoke reads it before and after a render and asserts
it did not move — that is the e2e oracle. The harness also greps the log for an `ERROR`-level
`vulkan` line containing `[validation]`.

> [!NOTE]
> The `ERROR  vulkan  [validation]` lines and the `validation_issue_count` tally are the two faces
> of the same gate. A run is clean when no validation error line appears and the counter does not move.

## Installing the subscriber

`saffron_log::init_logging()` is called once at process start — the host's `run_host`, the player's
`main`, and the editor bridge's `run`. It is idempotent (a second call is a no-op, never a panic),
and it is the single seam where a future file sink (`tracing-appender`) or an editor-channel sink is
added as one more `.with(layer)`. The editor shell — a separate process outside the engine
workspace — depends only on the leaf `saffron-log` crate, so it shares this exact format without
pulling the engine into its build.

## In the code

| What | File | Symbols |
|---|---|---|
| Subscriber install + line format | `engine/crates/log/src/lib.rs` | `init_logging`, `CompactFormatter`, `subsystem_of` |
| Emit at the call site | anywhere | `tracing::{info, warn, error, debug, trace}!` |
| Span context | `engine/crates/script/src/runtime.rs` | `info_span!("script", entity = …)` |
| The Vulkan funnel | `engine/crates/rendering/src/device.rs` | `debug_callback`, `validation_issue_count` |

## Related

- [Error handling](../error-handling/) — where a failed `Result` is logged before bailing
- [Control plane architecture](../../tooling-and-control/control-plane-architecture/) — the richer way to inspect a running editor
