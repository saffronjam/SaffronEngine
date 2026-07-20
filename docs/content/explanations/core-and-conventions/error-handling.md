+++
title = 'Error handling'
weight = 2
+++

# Error handling

Every fallible operation in the engine returns a typed `Result`, propagated with the `?`
operator. A panic marks a broken invariant, never an expected failure like a bad file or a
failed Vulkan call. The failure travels through ordinary control flow, so a caller can
match on the exact cause, and cleanup falls out of `Drop` rather than an unwind.

## A typed enum per crate

Each library crate owns one error enum, derived with [`thiserror`](https://docs.rs/thiserror),
and exports a `Result<T>` alias bound to it. A variant carries the fields a caller needs to
react; the `#[error("…")]` attribute renders the `Display` message. `saffron-json` shows the
shape: a parse failure, a missing key, and a wrong-type read are three distinct variants.

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid JSON: {0}")]
    Parse(String),
    #[error("missing key '{0}'")]
    MissingKey(String),
    #[error("key '{key}' is not {expected}")]
    WrongType { key: String, expected: &'static str },
}

pub type Result<T> = std::result::Result<T, Error>;
```

A `String` payload appears only where the cause genuinely has no further structure, such as the
parser's own message in `Parse`. Where a failure has structure, the variant carries it:
`WrongType` names the key and the expected type, so the handler can report both without parsing
a message back apart.

`saffron-core` defines the root of the family, a single `Message(String)` variant. The crate has
almost no fallible functions of its own; the typed root exists so downstream crates have a
common foundation to compose against.

## Composing across crates

A crate that calls into another lifts the callee's error into its own enum with a `#[from]`
variant, so `?` converts as it propagates. `saffron-app`'s bring-up errors wrap the window and
renderer crates this way:

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to create window: {0}")]
    Window(#[from] saffron_window::Error),
    #[error("failed to create renderer: {0}")]
    Renderer(#[from] saffron_rendering::Error),
    #[error("event loop failed: {0}")]
    EventLoop(String),
}
```

The typed chain collapses exactly once, at the top of the stack: `run` logs the error and maps a
bring-up failure to process exit code `1`, a clean exit to `0`. Everything beneath it stays
typed.

A callback that cannot return a `Result` stashes instead of panicking. winit's `resumed`
callback returns `()`, and the windowed host must create its window there; a window or renderer
failure inside it is stored on the handler as `bring_up_error` and re-raised as the typed error
once `run_windowed` regains control.

## At the call site

```rust
let value = json::parse_json(text)?;       // propagate
let id = json::json_u64(&value, "id")?;    // MissingKey / WrongType on failure
```

Propagate with `?` when the caller has nothing to add; match when different failures need
different reactions. Because nothing unwinds, the failure path needs no manual teardown — a
half-built resource dropped on the early return frees itself through
[RAII](../ownership-and-raii/).

## The third-party boundary

The libraries the engine wraps are driven through their `Result` surfaces and converted at the
seam. In `saffron-rendering`, every [ash](https://docs.rs/ash) Vulkan call passes through one
helper that maps the raw `VkResult` onto the crate's error, tagging the failing operation:

```rust
pub(crate) fn checked<T>(
    result: std::result::Result<T, vk::Result>,
    context: &'static str,
) -> Result<T> {
    result.map_err(|result| Error::Vk { context, result })
}
```

`Error::Vk` keeps the raw `vk::Result`, so a caller can match the exact failure code rather than
a formatted string. VMA allocation calls flow through the same mapping (`checked_vma`), and
`serde_json` parse errors become `json::Error::Parse`.

One tool sits outside the pattern: `xtask`, the build-task runner, reports through
[`anyhow`](https://docs.rs/anyhow). A build tool's caller is a human reading a message, not code
matching a variant, so a typed enum buys nothing there.

> [!NOTE]
> Panics cover invariants the type system cannot express. The workspace release profile pins
> `panic = "unwind"` so the FFI and shared-memory seams unwind cleanly and `#[should_panic]`
> tests work; `abort` would defeat both.

## In the code

| What | File | Symbols |
|---|---|---|
| Root error and alias | `crates/core/src/error.rs` | `Error`, `Result` |
| Structured variants | `crates/json/src/lib.rs` | `Error::Parse`, `Error::MissingKey`, `Error::WrongType` |
| Cross-crate composition | `crates/app/src/lib.rs` | `Error::Window`, `Error::Renderer` |
| Exit-code collapse, stashed callback failure | `crates/app/src/lib.rs` | `run`, `run_windowed`, `WindowedApp` |
| The ash seam | `crates/rendering/src/lib.rs` | `Error::Vk`, `checked` |
| The VMA seam | `crates/rendering/src/resources.rs` | `checked_vma` |
| Panic-strategy pin | `engine/Cargo.toml` | `[profile.release] panic = "unwind"` |

## Related

- [Go-flavored design](../go-flavored-design/) — the API ethos this error model belongs to
- [Ownership and RAII](../ownership-and-raii/) — why the failure path needs no manual teardown
- [JSON gateway](../json-gateway/) — the typed readers that return these errors
