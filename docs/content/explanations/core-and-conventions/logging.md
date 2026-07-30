+++
title = 'Logging'
weight = 5
+++

# Logging

Logging writes a tagged diagnostic line to a stream so a running process reports what it is
doing. The engine builds this on [`tracing`](https://docs.rs/tracing), a structured logging and
diagnostics library: call sites emit events through the `tracing::{error, warn, info, debug,
trace}!` macros, and one global subscriber renders every event as a single compact line on
stdout. The host, the player, and the editor bridge all install the same `saffron-log`
subscriber, so their logs share one format.

## Line format

```
12:30:01.234  INFO   rendering  vulkan ready — gpu 'NVIDIA GeForce RTX 3070 Ti' (discrete)
12:30:01.250  INFO   script     [entity=7423988312044896256] fired thruster
12:30:01.251  ERROR  vulkan     [validation] VUID-vkCmdDraw-None-08114: descriptor was never bound
```

Each line is a millisecond wall-clock timestamp, a fixed-width level label, the subsystem padded
to ten columns, any span context in brackets, then the message. `CompactFormatter` captures the
local UTC offset once at startup and colors the level only when stdout is a terminal, so piped or
captured output stays plain ASCII. The CI smoke and the e2e harness grep that plain text.

Filtering is tracing-subscriber's
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html).
The default directive enables everything at `debug` but pins the editor connectors' HTTP/TLS
stack (`hyper`, `reqwest`, `rustls`, …) at `warn` so it does not drown the engine's own lines.
Setting `RUST_LOG` replaces that whole default rather than merging into it, e.g.
`RUST_LOG=info,saffron_script=trace`.

## Subsystem from the target

A call site never spells its tag. `tracing` defaults an event's target to the emitting module
path, and `subsystem_of` reduces that to the column: strip the `saffron_` prefix and keep the
first path segment, so `saffron_rendering::renderer` prints as `rendering`. An explicit target
passes through verbatim, and an empty one falls back to `engine`.

```rust
tracing::info!("delete-unused: removed '{path}' ({bytes} bytes)"); // saffron_assets::manage → assets
tracing::error!(target: "vulkan", "{body}");                       // explicit target, kept as-is
```

The explicit form lets one component speak on another's behalf. The Vulkan debug messenger lives
in `saffron-rendering` but logs under `vulkan`, and the editor's presenter tags its lines
`viewport`.

## Context from spans

Per-event context rides on `tracing` spans rather than message prefixes: the formatter walks the
event's span scope from the root and prints each span's fields in brackets before the message.
The script runtime enters `info_span!("script", entity = …)` around every handler call, so each
line emitted inside a handler carries `[entity=…]` with the instance's 64-bit uuid. That covers a
script's own `sa.log(...)`, which is a plain `tracing::info!`, and any engine-side warning the
handler triggers.

## The Vulkan messenger

Validation-layer and loader messages arrive through a
[`VkDebugUtilsMessengerEXT`](https://docs.vulkan.org/spec/latest/chapters/debugging.html)
callback registered at instance creation. `debug_callback` maps the message severity to a level,
prefixes the body with `[validation]`, `[performance]`, or `[general]`, and emits it under the
`vulkan` target. Loader chatter (general-type messages below error severity) is dropped unless
`SAFFRON_VK_VERBOSE` is set.

The callback also tallies every validation or performance message at warning-or-error severity
into a process-wide counter, `validation_issue_count`. The rendering crate's render tests read
the counter before and after a frame and assert it did not move. The e2e harness and the CI
smoke instead match the captured log against `ERROR  vulkan  [validation]`; the counter and the
grep are two readings of the same events.

## One install per process

`saffron_log::init_logging()` runs once at each process entry: the host's `run_host`, the
player's `main`, and the editor shell's `main` (browser process only — the CEF helper
subprocesses exit before it). A `Once` guard plus `try_init` make it idempotent — a second call,
or a subscriber another component installed first, is a no-op rather than a panic.

`saffron-log` depends on no other Saffron crate. The editor shell sits outside the
engine workspace, and the leaf dependency lets it print the identical format without pulling the
engine into its build.

## In the code

| What | File | Symbols |
|---|---|---|
| Subscriber install + default filter | `engine/crates/log/src/lib.rs` | `init_logging`, `DEFAULT_FILTER` |
| Line format + subsystem column | `engine/crates/log/src/lib.rs` | `CompactFormatter`, `subsystem_of` |
| Emit at a call site | anywhere | `tracing::{error, warn, info, debug, trace}!` |
| Script span context | `engine/crates/script/src/runtime.rs` | `call_instance_method` |
| Vulkan funnel + validation tally | `engine/crates/rendering/src/device/` | `debug_callback`, `validation_issue_count` |
| Log-grep clean gates | `tests/e2e/harness.ts`, `tools/ci/check.sh` | `validationErrors` |

## Related

- [Error handling](../error-handling/) — where a failed `Result` is logged before bailing
- [Control plane architecture](../../tooling-and-control/control-plane-architecture/) — inspecting a running editor beyond the log
- [Script components and runtime](../../scripting/script-components-and-runtime/) — the handler calls the script span wraps
