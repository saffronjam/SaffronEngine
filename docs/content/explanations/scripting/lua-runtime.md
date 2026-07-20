+++
title = 'Lua runtime'
weight = 1
+++

# Lua runtime

Anima embeds [Luau](https://luau.org/) through [`mlua`](https://docs.rs/mlua/) so gameplay scripts can run without rebuilding the engine. `saffron-script` owns the VM boundary; other crates exchange scene values, binding descriptors, and typed `Result` values without holding `mlua` handles.

## VM ownership

`ScriptVm` owns one `mlua::Lua`. Dropping the wrapper frees the VM. It is single-threaded and not `Send`, matching the main-thread simulation loop and allowing the budget state to use `Rc<Cell<_>>`.

`ScriptHost` owns one `ScriptVm` for a play session. The edit-time field-schema reader creates a separate short-lived VM, executes the module to inspect its `properties` table, and drops that VM after the request.

```rust
use saffron_script::ScriptVm;

let vm = ScriptVm::with_limits(100_000, 32 * 1024 * 1024)?;
vm.run_string("assert(math.floor(2.7) == 2)", "budgeted-example")?;
```

## Library sandbox

The VM exposes core globals plus the coroutine, string, math, table, and UTF-8 libraries. Filesystem, process, debug, package, and native-module facilities are absent.

| Available | Withheld |
|---|---|
| Core functions | `io` |
| `coroutine` | `os` |
| `string` | `debug` |
| `math` | `package` |
| `table` and `utf8` | Native loading through package facilities |

After limits and callbacks are installed, `Lua::sandbox(true)` freezes standard library tables and gives loaded chunks isolated environments. Engine bindings then add value types and the permitted `sa` functions explicitly.

The crate uses `#![deny(unsafe_code)]`; `mlua` contains the raw VM integration behind its safe Rust API.

## Execution budgets

Two independent limits bound a script call:

| Limit | Default | Enforcement |
|---|---:|---|
| Instruction guard | `1,000,000` interrupt callbacks | A Luau interrupt increments a counter and returns an error after the limit. `0` disables this guard. |
| VM memory | `256 MiB` | `Lua::set_memory_limit` rejects allocation beyond the ceiling. |

The instruction value counts periodic Luau interrupt callbacks, not individual source instructions. It guarantees a finite host-side threshold for a runaway loop without claiming an exact instruction total.

`run_string` resets the counter before loading and calling a chunk. `ScriptHost` performs the same reset before each lifecycle or event callback, so one handler's work does not consume another handler's allowance. The memory ceiling applies to the whole VM for its lifetime.

## Error classification

The crate maps VM failures into three typed variants:

| Variant | Cause |
|---|---|
| `Error::Load` | Syntax or chunk-loading failure |
| `Error::Runtime` | Raised error, invalid operation, or faulting binding |
| `Error::Budget` | Instruction guard or memory ceiling |

`run_string` compiles the named chunk first, which separates load failures from faults that occur during execution. Runtime messages retain the Luau traceback supplied by `mlua`; the play runtime can record that text and decide whether the failed callback pauses simulation.

This layer does not decide play-mode policy. Slot loading, instance construction, callback containment, and the error ring belong to [Script components and the play runtime](../script-components-and-runtime/).

## Source map

| What | File | Symbols |
|---|---|---|
| VM construction and sandbox | `engine/crates/script/src/vm.rs` | `ScriptVm::new`, `ScriptVm::with_limits`, `sandbox_libs` |
| Limits and chunk execution | `engine/crates/script/src/vm.rs` | `DEFAULT_INSTRUCTION_BUDGET`, `DEFAULT_MEMORY_LIMIT`, `ScriptVm::run_string` |
| Error mapping | `engine/crates/script/src/vm.rs` | `classify_load`, `map_lua_error` |
| Typed error surface | `engine/crates/script/src/error.rs` | `Error`, `Result` |
| Per-play owner | `engine/crates/script/src/runtime.rs` | `ScriptHost` |

## Related

- [Script components and the play runtime](../script-components-and-runtime/)
- [Script-declared fields](../script-declared-fields/)
- [Error handling](../../core-and-conventions/error-handling/)
