+++
title = 'Core primitives'
weight = 3
+++

# Core primitives

`saffron-core` is the root of the crate DAG: it depends on no other Saffron crate, and every
other crate depends on it. It holds the small value types the whole engine shares — a stable
identity newtype, a duration, two material vocabulary enums, the `Ref` ownership alias, a base64
helper, and the engine identity strings. Rust's own `u8`…`u64` / `f32` / `f64` cover the numbers;
the primitives here are the ones that carry engine meaning. The crate's other export, the typed
`Error`/`Result` pair, has [its own page](../error-handling/).

## Uuid, the stable identity

`Uuid` is a stable 64-bit identity, a newtype over `u64`:

```rust
pub struct Uuid(pub u64);

impl Uuid {
    pub fn new() -> Self { /* mint at or above 1024 */ }
    pub fn value(self) -> u64 { self.0 }
}
```

A [hecs](https://docs.rs/hecs) entity value is not stable across runs, because entity slots are
reused as entities are created and destroyed. Anything serialized and reloaded carries a `Uuid`
instead. Catalog assets and saved-scene entities are keyed by `Uuid`, which is how a reloaded
project reconnects a mesh component to the right mesh.

`Uuid::new` mints from a per-thread [SplitMix64](https://prng.di.unimi.it/splitmix64.c)
generator, seeded once per thread from the wall clock's nanosecond count mixed with a stack
address. The contract is uniqueness, not reproducibility, so this keeps the crate free of an RNG
dependency. Ids below `1024` belong to built-in and synthetic assets (the default material is
`Uuid(1)`); a minted id is always at least `1024`, so it never collides with a built-in one.

## The decimal-string wire form

On the JSON wire a `Uuid` crosses as a decimal string, never a number. Ids span the full `u64`
range, far past JavaScript's
[2⁵³ − 1 safe-integer limit](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Number/MAX_SAFE_INTEGER),
so a bare JSON number would silently corrupt a high id in any JS client. `Uuid(u64::MAX)` crosses
as `"18446744073709551615"` and survives `JSON.parse` exactly.

The newtype itself carries no serde derive, only `Display` and `FromStr` in the decimal form. The
encoding is applied outside the core crate by two encoders a contract test keeps byte-identical:
the [protocol crate's wire `Uuid` newtype](../../tooling-and-control/shared-types/) and the
[JSON gateway's](../json-gateway/) `WireUuid` adapter. Both emit the decimal string and accept a
string or a number on read.

## TimeSpan, a duration

A duration is a `TimeSpan`: a one-field struct over seconds, with a `const` constructor and a
unit read.

```rust
pub struct TimeSpan {
    pub seconds: f32,
}

impl TimeSpan {
    pub const fn from_seconds(seconds: f32) -> Self { Self { seconds } }
    pub const fn to_milliseconds(self) -> f32 { self.seconds * 1000.0 }
}
```

The [main loop](../../app-lifecycle-and-window/main-loop-and-run/) builds one per iteration from
the inter-frame `Instant` delta and passes it to every layer's `on_update` as the frame delta.

## Ref, the ownership alias

`Ref<T>` is `Arc<T>`, the shared-read default of the [ownership policy](../ownership-and-raii/):
a value fully constructed and then only read through every shared handle. It lives in the core
crate so every downstream crate names the shared-read shape the same way. A shared-*mutable* site
does not use `Ref`; it spells `Arc<Mutex<T>>` (or `Arc<RwLock<T>>`) at its declaration, so the
exception is visible where it occurs.

## BlendMode and HeightMode, the material vocabulary

`BlendMode` (`Opaque` / `Masked` / `Blend`) is the
[glTF `alphaMode`](https://github.com/KhronosGroup/glTF/blob/main/specification/2.0/Specification.adoc)
axis: how a material's alpha resolves at raster time. `HeightMode` (`Bump` / `Parallax` /
`Displacement`) selects how its grayscale height map is realized. Both live in core so the scene
components, the asset resolve, and the renderer share one enum instead of parallel spellings.

Each enum pairs `as_wire`/`from_wire` for its lowercase wire token in scene JSON and `.smat`
documents. `BlendMode::Blend`'s token is `"translucent"`; an unrecognized token parses to the
safe default, `Opaque` or `Bump`. What each mode does at render time is covered by
[native materials](../../materials-and-pipelines/native-materials/).

## base64_encode, small blobs on the wire

`base64_encode` renders a byte buffer as standard base64
([RFC 4648](https://datatracker.ietf.org/doc/html/rfc4648): the `A-Za-z0-9+/` alphabet with `=`
padding), so `base64_encode(b"foo")` is `"Zm9v"`. It carries small binary blobs, thumbnail PNGs
for example, over the JSON control plane.

## Engine identity

`ENGINE_NAME` (`"Saffron Anima"`) and `ENGINE_VERSION` (`"0.1.0-vulkan"`) are the two identity
constants. The `ping` control command reports them, so a shell can confirm which engine answered:

```sh
sa ping
# pong  engine=Saffron Anima  version=0.1.0-vulkan  pid=41253
```

## In the code

| What | File | Symbols |
|---|---|---|
| Stable identity | `engine/crates/core/src/uuid.rs` | `Uuid`, `Uuid::new`, `Uuid::value`, `SplitMix64` |
| Duration | `engine/crates/core/src/time.rs` | `TimeSpan`, `from_seconds`, `to_milliseconds` |
| Ownership alias, identity strings | `engine/crates/core/src/lib.rs` | `Ref`, `ENGINE_NAME`, `ENGINE_VERSION` |
| Material vocabulary | `engine/crates/core/src/blend.rs`, `engine/crates/core/src/height.rs` | `BlendMode`, `HeightMode`, `as_wire`, `from_wire` |
| Base64 helper | `engine/crates/core/src/base64.rs` | `base64_encode` |
| Reserved built-in ids | `engine/crates/assets/src/lib.rs` | `DEFAULT_MATERIAL_ID` |
| Wire encoders for `Uuid` | `engine/crates/protocol/src/uuid.rs`, `engine/crates/json/src/lib.rs` | `Uuid` (wire), `WireUuid` |

## Related

- [Rust house style](../go-flavored-design/) — why a duration is a struct plus methods
- [Ownership](../ownership-and-raii/) — `Ref<T>`, the shared-read alias, and teardown order
- [JSON gateway](../json-gateway/) — where `Uuid`'s decimal-string wire form is applied
- [Shared types](../../tooling-and-control/shared-types/) — the protocol-side wire `Uuid` and its schema
- [Error handling](../error-handling/) — the core crate's `Error`/`Result` model
- [Native materials](../../materials-and-pipelines/native-materials/) — `BlendMode` and `HeightMode` at render time
