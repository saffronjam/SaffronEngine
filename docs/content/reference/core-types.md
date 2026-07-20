+++
title = 'Core types'
weight = 1
math = false
+++

# Core types

The `saffron-core` crate is the dependency root for Anima's shared value types, error vocabulary, and identity constants. This page lists its complete public API.

## Public API

| Item | Definition | Behavior |
|---|---|---|
| `Error` | `enum Error { Message(String) }` | The typed root error. `Message` represents a failure with no more specific structure. |
| `Result<T>` | `core::result::Result<T, Error>` | The result alias for `saffron-core`. Each downstream library crate defines its own typed error and result alias. |
| `Ref<T>` | `std::sync::Arc<T>` | A read-shared handle. Shared mutable state spells out `Arc<Mutex<T>>` or `Arc<RwLock<T>>` at its declaration. |
| `Uuid` | `pub struct Uuid(pub u64)` | A stable identity independent of an ECS entity handle. `Default` is `Uuid(0)`. |
| `TimeSpan` | `pub struct TimeSpan { pub seconds: f32 }` | A duration stored as seconds. `Default` is zero seconds. |
| `BlendMode` | `Opaque`, `Masked`, `Blend` | Selects opaque, alpha-tested, or alpha-blended material rendering. `Opaque` is the default. |
| `HeightMode` | `Bump`, `Parallax`, `Displacement` | Selects shading-normal bump, parallax occlusion, or vertex displacement. `Bump` is the default. |
| `base64_encode` | `fn(&[u8]) -> String` | Encodes bytes as standard padded [Base64](https://www.rfc-editor.org/rfc/rfc4648#section-4). |
| `ENGINE_NAME` | `&str` | `"Saffron Anima"` |
| `ENGINE_VERSION` | `&str` | `"0.1.0-vulkan"` |

## Identity

`Uuid::new()` generates a value at or above `1024`; values below `1024` identify built-in or synthetic assets. `Uuid::value()` returns the underlying `u64`.

`Uuid` implements `Display` and `FromStr` using an unsigned decimal string. Protocol fields serialize that string form so values above JavaScript's safe-integer limit remain exact.

```rust
use saffron_core::Uuid;

let id = Uuid::new();
let encoded = id.to_string();
let decoded: Uuid = encoded.parse().expect("decimal UUID");
assert_eq!(decoded, id);
```

## Time

`TimeSpan::from_seconds(seconds)` constructs a span. `TimeSpan::to_milliseconds()` multiplies the stored seconds by `1000.0`. Both methods are `const fn`.

```rust
use saffron_core::TimeSpan;

let frame = TimeSpan::from_seconds(0.016);
assert_eq!(frame.to_milliseconds(), 16.0);
```

## Material modes

`BlendMode::as_wire()` and `HeightMode::as_wire()` return the tokens used in scene JSON and `.smat` documents. Each `from_wire()` method accepts its listed tokens and returns the default variant for any other value.

| Type | Variant | Wire token |
|---|---|---|
| `BlendMode` | `Opaque` | `opaque` |
| `BlendMode` | `Masked` | `masked` |
| `BlendMode` | `Blend` | `translucent` |
| `HeightMode` | `Bump` | `bump` |
| `HeightMode` | `Parallax` | `parallax` |
| `HeightMode` | `Displacement` | `displacement` |

## Source map

| What | File | Symbols |
|---|---|---|
| Public exports, shared handle, identity constants | `engine/crates/core/src/lib.rs` | `Ref`, `ENGINE_NAME`, `ENGINE_VERSION` |
| Root error vocabulary | `engine/crates/core/src/error.rs` | `Error`, `Result` |
| Stable identity | `engine/crates/core/src/uuid.rs` | `Uuid`, `Uuid::new`, `Uuid::value` |
| Duration value | `engine/crates/core/src/time.rs` | `TimeSpan`, `TimeSpan::from_seconds`, `TimeSpan::to_milliseconds` |
| Material alpha behavior | `engine/crates/core/src/blend.rs` | `BlendMode`, `BlendMode::as_wire`, `BlendMode::from_wire` |
| Material height technique | `engine/crates/core/src/height.rs` | `HeightMode`, `HeightMode::as_wire`, `HeightMode::from_wire` |
| Binary-to-text encoding | `engine/crates/core/src/base64.rs` | `base64_encode` |

## Related

- [Error handling](../../explanations/core-and-conventions/error-handling/)
- [Type aliases and primitives](../../explanations/core-and-conventions/type-aliases-and-primitives/)
- [Ownership and RAII](../../explanations/core-and-conventions/ownership-and-raii/)
- [Logging](../../explanations/core-and-conventions/logging/)
