+++
title = 'JSON gateway'
weight = 7
+++

# JSON gateway

The JSON gateway is `saffron-json`, a thin layer over [`serde_json`](https://docs.rs/serde_json)
for code that works with untyped `Value` trees: one parse/dump entry point, checked typed readers,
and the decimal-string-`u64` id encoding the engine and the editor share byte-for-byte. Scene
documents, the project file, the JSON asset formats, and every control request the host drains
pass through its parse and dump functions.

Every fallible operation returns the crate's typed [`Result`](../error-handling/) over a
structured error. A parse failure, a missing key, and a wrong-type read are distinct variants, so
a caller can react to each:

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
```

## Parse and dump

`parse_json` wraps `serde_json::from_str` and maps a parse error to `Error::Parse`. `dump_json`
serializes a `Value`: a negative indent produces compact output, zero or more pretty-prints with
that many spaces per level. The control plane's per-frame drain is the highest-traffic caller:
`ControlContext::poll` parses each request line with `parse_json` and serializes every reply
compact with `dump_json` (see [control plane architecture](../../tooling-and-control/control-plane-architecture/)).

```rust
pub fn parse_json(text: &str) -> Result<Value>;
pub fn dump_json(value: &Value, indent: i32) -> String;
```

`serde_json` is built workspace-wide with `preserve_order`, so an object emits its keys in
insertion order — the control wire needs result keys in DTO field order. Byte-frozen formats need
key order independent of insertion order, so `dump_json_sorted` re-emits every object's keys
lexicographically sorted, recursively. The scene document, the `.smat` encoder, and the `.smodel`
META chunk serialize through it; an unchanged asset re-saves to identical bytes and its source
hash holds.

## Typed reads

A typed read asks a value for a type it may not hold. Each reader locates the key, checks the
stored type, and only then extracts. A missing key is `Error::MissingKey` and a wrong type is
`Error::WrongType`, never a panic.

```rust
pub fn json_u64(object: &Value, key: &str) -> Result<u64>;
pub fn json_string(object: &Value, key: &str) -> Result<String>;
pub fn json_f64(object: &Value, key: &str) -> Result<f64>;
pub fn json_bool(object: &Value, key: &str) -> Result<bool>;
```

`json_u64` is deliberately lenient: it accepts an unsigned number or a decimal string whose
entire content parses. Ids cross the wire as strings (below), so a stored id loads either way. A
trailing-garbage string (`"42x"`) and a negative number are rejected.

## Ids as strings

A `u64` id spans the full 64-bit range. JavaScript stores numbers as IEEE 754 doubles, which hold
integers exactly only up to [`Number.MAX_SAFE_INTEGER`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Number/MAX_SAFE_INTEGER),
9,007,199,254,740,991. An id above that, emitted as a JSON number, is rounded the moment a JS
client runs the reply through `JSON.parse`: `u64::MAX` (18,446,744,073,709,551,615) comes back as
the double 2^64, one too high. `uuid_to_json` sidesteps the loss by emitting every id as a
decimal JSON string, which survives `JSON.parse` exactly:

```json
{ "id": "18446744073709551615" }
```

`WireUuid` is the [`serde_with`](https://docs.rs/serde_with) adapter form of the same rule:
`#[serde_as(as = "WireUuid")]` on a `Uuid` field emits the decimal string and accepts a string or
a number on read. The [protocol crate's wire `Uuid` newtype](../../tooling-and-control/shared-types/)
encodes the identical union through its own derive (`PickFirst<(DisplayFromStr, _)>`), and the
`cross_encoder_identity_with_saffron_json` test pins the two encoders to byte-identical output
across the full `u64` range. Every id has one wire form, whether it sits in a control reply or a
saved scene.

## Value-or-default reads

Optional fields do not want a `Result` at every call site. Each strict reader has an `_or` twin
that swallows the error and returns a fallback: `json_u64_or`, `json_string_or`, `json_f32_or`,
`json_bool_or`. `json_f32_or` reads the `f64` wire value and narrows it, since JSON carries one
number type.

The registry-driven [scene serde](../../scene-and-ecs/scene-serialization/) and the asset and
[project](../../geometry-and-assets/project-serialization/) loaders read through these twins. A
field absent from a save loads as its default rather than failing the whole file, so a save that
predates a component field opens cleanly with that field at its default.

## In the code

| What | File | Symbols |
|---|---|---|
| Typed gateway error | `engine/crates/json/src/lib.rs` | `Error`, `Result` |
| Parse / serialize | `engine/crates/json/src/lib.rs` | `parse_json`, `dump_json`, `dump_json_sorted` |
| Id wire encoding | `engine/crates/json/src/lib.rs` | `uuid_to_json`, `WireUuid` |
| Checked typed reads | `engine/crates/json/src/lib.rs` | `json_u64`, `json_string`, `json_f64`, `json_bool` |
| Value-or-default reads | `engine/crates/json/src/lib.rs` | `json_u64_or`, `json_string_or`, `json_f32_or`, `json_bool_or` |
| Control envelope parse/dump | `engine/crates/control/src/context.rs` | `ControlContext::poll` |
| Cross-encoder byte identity | `engine/crates/protocol/src/uuid.rs` | `Uuid`, `cross_encoder_identity_with_saffron_json` |

## Related

- [Error handling](../error-handling/) — the typed `Result` style the readers return
- [Core primitives](../type-aliases-and-primitives/) — `Uuid`, the `u64` identity newtype these encoders carry
- [Shared types](../../tooling-and-control/shared-types/) — the protocol-side wire `Uuid` and its schema
- [Scene serialization](../../scene-and-ecs/scene-serialization/) — the registry-driven save/load built on these readers
- [Project serialization](../../geometry-and-assets/project-serialization/) — the unified project file
