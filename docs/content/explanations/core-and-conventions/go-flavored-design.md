+++
title = 'Go-flavored design'
weight = 1
+++

# Go-flavored design

The engine's API design borrows the ethos [Effective Go](https://go.dev/doc/effective_go)
describes: plain data, small interfaces, and composition instead of subclassing. Anima expresses
that ethos in idiomatic Rust, so a design question resolves to a visible-field struct, a small
trait, or a table of function pointers — never a class hierarchy.

## The vocabulary

The whole codebase is built from a handful of shapes:

- **Structs with public fields.** Data stays visible rather than hidden behind accessors. A
  constructor is an associated `fn new(..) -> Self` (or `-> Result<Self>` when it can fail), and
  most logic is an inherent method or a free function over the data, which keeps it testable.
  [`TimeSpan`](../type-aliases-and-primitives/) is a duration built this way, and
  [`SubscriberList`](../signals-and-slots/) an event channel: a struct plus a small method set.
- **Traits as interfaces.** A behaviour boundary is a trait, dispatched statically through
  generics where possible and behind `Box<dyn Trait>` where a loop must hold a heterogeneous set.
  [`Layer`](../../app-lifecycle-and-window/layer-system/) (the lifecycle hooks) and `FrameHost`
  (the loop's GPU seam) are the two the main loop runs on.
- **Fn-pointer tables where dispatch must be data.**
  [`ComponentTraits`](../../scene-and-ecs/component-registry/) is a struct of plain `fn`
  pointers, one per structural operation: the itable a Go interface would generate, built by
  hand so the rows can live in a runtime registry keyed by `TypeId` or name.
- **Enums for sum types**, and a typed [`Result<T>`](../error-handling/) for anything that can
  fail.
- **Closures for the deferred-work seams.** `Renderer::submit` records an
  `FnOnce(vk::CommandBuffer)` into the current frame; `AppConfig` carries boxed
  `on_create` / `on_exit` closures so the config is a plain owned value.
- **`Drop` for cleanup, `Arc<T>` for sharing.** The [ownership page](../ownership-and-raii/)
  has the rules.

One signature per shape shows how uniform the pattern is across crates:

```rust
pub trait FrameHost { fn begin_frame(&mut self) -> Result<bool>; /* … */ }  // interface = trait
pub struct ComponentTraits { pub has: fn(&Scene, Entity) -> bool, /* … */ } // itable = fn table
pub fn submit(&mut self, body: impl FnOnce(vk::CommandBuffer) + 'static)    // deferral = closure
pub type Ref<T> = Arc<T>;                                                    // sharing = Arc
```

## Composition, not inheritance

Nothing in the tree extends a base class, because nothing is a class. A type that wants behaviour
from another type holds it as a field and delegates; a subsystem that wants to accept many types
takes a trait bound or a `Box<dyn Trait>`. The `Layer` trait defaults every hook to a no-op, so
an implementation overrides only the hooks it uses — the trait-with-provided-methods answer to
Go's small-interface habit.

## Why it holds up in a renderer

Graphics code is where engines usually grow the deepest hierarchies: a `Resource` base, a
`RenderPass` base, a `Material` base. Anima has none. A GPU buffer is a plain `Buffer` struct
whose `Drop` frees the allocation; a render pass is an
[`RgPass`](../../frame-and-render-graph/render-graph-overview/) the render graph walks; a
component is a plain struct the registry serializes through its fn-pointer row.

When something breaks, nothing stands between the data and the call site. The fields are public,
the control flow is explicit, and the borrow checker proves the lifetimes instead of a
hand-audited teardown order.

The style is enforced rather than aspirational: the Clippy gate, the `unsafe` policy, and the
comment rules live on the [Rust house style](../../architecture-and-conventions/rust-house-style/)
page.

## In the code

| What | File | Symbols |
|---|---|---|
| Traits as interfaces | `engine/crates/app/src/lib.rs` | `Layer`, `FrameHost`, `attach_layer` |
| The deferred-work closure seam | `engine/crates/rendering/src/renderer/` | `Renderer::submit` |
| The hand-built itable | `engine/crates/scene/src/registry.rs` | `ComponentTraits`, `ComponentRegistry` |
| Plain-data GPU wrappers | `engine/crates/rendering/src/resources/` | `Buffer`, `Image`, `GpuMesh`, `GpuTexture`, `Pipeline` |
| The sharing alias | `engine/crates/core/src/lib.rs` | `Ref` |

## Related

- [Rust house style](../../architecture-and-conventions/rust-house-style/) — the lint gate and conventions that enforce this style
- [Component registry](../../scene-and-ecs/component-registry/) — the fn-pointer itable in full
- [Layers](../../app-lifecycle-and-window/layer-system/) — the trait-of-default-hooks pattern in action
- [Error handling](../error-handling/) — the typed `Result<T>` half of the vocabulary
- [Ownership](../ownership-and-raii/) — `Drop`, `Arc<T>`, and teardown order
