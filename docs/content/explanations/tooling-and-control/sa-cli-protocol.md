+++
title = 'sa CLI'
weight = 2
+++

# sa CLI

`sa` is Anima's shell client. A forwarded command becomes one JSON request over the [control socket](../control-plane-architecture/), and the decoded result becomes text or JSON on standard output.

The binary depends on `saffron-protocol`, `saffron-control-client`, Clap, and JSON support. It does not link the renderer, scene, assets, physics, or host crates, so command scripts remain separate from the engine process they drive.

## Command routes

Clap handles four top-level routes:

| Form | Action |
|---|---|
| `sa start [--build] [--attach]` | Launches `saffron-host`; no control request |
| `sa completions <shell>` | Writes a completion script |
| `sa export <output-dir> [flags]` | Builds typed `export-app` parameters and forwards them |
| `sa <command> [arguments]` | Forwards an arbitrary control command |

The external command arm does not validate names. A command can be forwarded as soon as the running host registers it, even if the CLI's compiled command table does not contain it.

## Arguments to parameters

`build_params` separates positional tokens from double-dash flags:

| Shell token | JSON parameter |
|---|---|
| `value` | Appended to `params.args` |
| `--key value` | `params.key = value` |
| `--key=value` | `params.key = value` |
| `--key` | `params.key = true` |

For example:

```sh
sa set-transform 123 --translation '{"x":0,"y":1,"z":0}'
```

produces this request on a new client:

```json
{"cmd":"set-transform","params":{"args":[123],"translation":{"x":0,"y":1,"z":0}},"id":1}
```

The host maps `args` onto the parameter DTO's declaration-ordered fields. A named key wins over its corresponding positional value. These forms are therefore equivalent:

```sh
sa set-aa msaa4
sa set-aa --mode msaa4
```

## Token coercion

Each value token becomes a JSON value through a fixed precedence order:

1. `true`, `false`, and `null` become JSON literals.
2. A token beginning with `{`, `[`, or `"` is parsed as inline JSON.
3. A non-negative integer is parsed as `u64`.
4. Other integers are parsed as `i64`.
5. A finite decimal is parsed as `f64`.
6. Everything else remains a string.

Parsing unsigned integers before floating-point values preserves positive IDs through `u64::MAX`. Invalid inline JSON falls through the remaining steps and normally becomes a string, leaving typed DTO deserialization to report the mismatch.

## Wire round trip

`saffron-control-client::Client` owns a socket path and a monotonic request ID. Each call opens a Unix stream, writes one request line, reads one reply line, then closes the connection.

`Client::call_raw` returns the reply's `result` value. An `ok: false` envelope becomes
`Error::Engine` and retains its complete `ControlFailureDto`. Connection, malformed-envelope, and
typed-result failures use distinct variants. The Rust end-to-end harness uses the same client and
can deserialize results directly into protocol DTOs.

The client and server share the same socket-path precedence: `SAFFRON_CONTROL_SOCK`, then `$XDG_RUNTIME_DIR/saffron-control.sock`, then the per-user `/tmp` path.

## Output and exit status

`-o text` is the default. Command-specific formatters render common results such as `ping`, entity and asset lists, render statistics, play state, physics queries, and profiler captures. Unmatched results use readable pretty JSON.

`-o json` prints successful results as pretty JSON for reliable use with `jq`:

```sh
sa -o json get-selection | jq -r '.id // empty'
```

In text mode, transport and engine errors print an `sa:`-prefixed message to standard error. In JSON
mode, an error prints the complete typed failure object to standard error, including a diagnostic
payload when present:

```json
{
  "code": "diagnostic",
  "message": "graph estimate exceeds the candidate limit",
  "diagnostic": {
    "domain": "vegetation-graph",
    "detail": {
      "category": "limit",
      "resource": "candidates",
      "requested": "1000001",
      "limit": "1000000"
    }
  }
}
```

A successful engine result exits `0`. Transport and engine failures exit `1`. A missing command or
Clap usage failure exits `2`.

## Discovery

`sa --help` appends the static names from `saffron_protocol::COMMANDS`. `sa help` is forwarded to the running host and returns its live registry, including the reflective `help` row and host-owned commands.

Completion scripts use the same static table. A registry test checks set equality between that table and runtime handlers, with explicit exceptions for reflective and host-owned rows.

When the engine rejects an unknown name, the CLI compares it with known names using [Levenshtein distance](https://en.wikipedia.org/wiki/Levenshtein_distance) and appends a suggestion when the nearest candidate is close enough. The original command still reaches the host.

## Host launcher

`sa start` checks whether the resolved socket accepts a connection. If the path exists but refuses connections, the launcher removes the stale socket before starting a host.

The launcher resolves `saffron-host` beside the `sa` executable unless `SAFFRON_ANIMA_BIN` names another binary. `--build` runs `cargo build --bin saffron-host` inside the `saffron-build` toolbox. Launch also uses that toolbox.

Detached mode discards host standard streams and polls the socket for five seconds. `--attach` keeps the host in the foreground. A readiness timeout reports that initialization may still be in progress rather than terminating the launched process.

`sa export` is different from a local file conversion: it forwards `export-app` to a running host with the loaded project and supplies a typed app-manifest patch.

## Source map

| What | File | Symbols |
|---|---|---|
| Clap surface and route selection | `engine/crates/sa/src/main.rs` | `Cli`, `Subcmd`, `main` |
| Argument mapping and coercion | `engine/crates/sa/src/main.rs` | `build_params`, `coerce`, `forward` |
| Text and JSON presentation | `engine/crates/sa/src/main.rs` | `print_result`, `format_text`, `OutputMode` |
| Help, completions, and suggestions | `engine/crates/sa/src/main.rs` | `enriched_command`, `completion_command`, `did_you_mean` |
| Start and export routes | `engine/crates/sa/src/main.rs` | `start`, `export`, `engine_binary_path` |
| Shared wire implementation | `engine/crates/control-client/src/lib.rs` | `Client`, `request_envelope`, `socket_path` |
| Position-to-DTO folding | `engine/crates/control/src/registry.rs` | `fold_positional_args` |
| Static command metadata | `engine/crates/protocol/src/command/` | `COMMANDS`, `CommandSpec` |

## Related

- [Control plane](../control-plane-architecture/)
- [Shared types](../shared-types/)
- [Scene commands](../scene-commands/)
- [Render commands](../render-commands/)
- [Asset commands](../asset-commands/)
