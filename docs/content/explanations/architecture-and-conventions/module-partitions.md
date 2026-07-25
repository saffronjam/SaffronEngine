+++
title = 'Module partitions'
weight = 3
+++

# Module partitions

A large crate is many files, not one. Each feature lives in its own module file under `src/`,
declared with `mod name;` in the crate root, and the root publishes the crate's API in a `pub use`
block. Under [Rust's module system](https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-crates-and-modules.html)
the files are private organization and the re-exports are the entire public surface.

The split costs nothing at the call site. A crate is one compilation unit, so a call from one
module file into a sibling is an ordinary function call. The only decision the crate root makes is
what escapes.

## Module files and the re-export root

`saffron-rendering` has the most module files of any crate in the workspace: 54 under
`crates/rendering/src/`, one per feature (`lighting.rs`, `pipelines.rs`, `render_graph.rs`,
`ssao.rs`, …), each declared privately in `lib.rs`:

```rust
// crates/rendering/src/lib.rs
mod lighting;
mod pipelines;
mod render_graph;
mod renderer;
mod resources;
// … 54 `mod` lines in all

pub use render_graph::{
    ProfileRecorders, RenderGraph, RgAccess, RgAttachment, RgPass, RgPassKind, RgResource, RgUsage,
};
pub use renderer::{RenderStatsFull, Renderer, VIEW_COUNT, ViewId, ViewMode};
```

None of the `mod` declarations is `pub`, so a type inside a module stays unreachable to consumers
until `lib.rs` re-exports it, even when the type itself is written `pub`. A consumer writes
`use saffron_rendering::{Renderer, RenderGraph};` and sees exactly the curated list. Which file
produced each type is invisible from outside.

Between public and private sits `pub(crate)`: visible to every module in the crate, absent from
the API. The crate root's `checked` helper is the pattern. `pub(crate) fn checked` wraps an ash
`vk::Result` into the crate's typed error, callable from any pass module but never exported.

## Internal-only modules

Re-export is per item, so a module can contribute nothing to the API at all. Four rendering
modules appear in no `pub use` line: `budget`, `nested_scopes`, `present`, and
`render_settings`. Their types are plain `pub` (`pub struct BudgetController`), which lets sibling
files reach them through internal paths:

```rust
// crates/rendering/src/renderer.rs
use crate::budget::{BudgetController, BudgetStep};
use crate::present::PresentSync;
```

A `use crate::…` path is the wiring between sibling files. A `pub use` in the root is the separate
act of publishing to consumers, and one never implies the other.

## Where the file lines fall

The division is by responsibility, not by a size cap:

- The orchestration file (`renderer.rs`) owns the top-level `Renderer` aggregate and the frame
  entry points (`render_frame`, `submit`), and calls into the feature files.
- Each feature file owns one subsystem's types and logic: `lighting.rs` the clustered lighting and
  shadow-map constants, `ssao.rs` the ambient-occlusion passes, `render_graph.rs` the
  [`RgPass`/`RgUsage` graph](../../frame-and-render-graph/render-graph-overview/).
- A helper with a single caller stays in that caller's file and is never re-exported.

Modules partition code, not state. Every pass module works against the shared `Renderer`
aggregate, which is why the renderer is one crate of many files rather than many crates: the
aggregate cannot be cut across a crate boundary, while module files slice the code around it
freely.

Every file-level module in the workspace is a single flat file; no crate uses a module directory
(`foo/` with a `mod.rs`). Nesting appears only as an inline `mod name { … }` block where one file
wants a private namespace, such as the `coerce` block in the protocol crate's `dto.rs` that groups
its serde bool-coercion helpers, or a `#[cfg(test)] mod tests` block.

## In the code

| What | File | Symbols |
|---|---|---|
| Private module declarations | `crates/rendering/src/lib.rs` | `mod lighting;`, `mod pipelines;`, … |
| The re-export surface | `crates/rendering/src/lib.rs` | `pub use renderer::{Renderer, ...}` |
| A crate-wide internal helper | `crates/rendering/src/lib.rs` | `pub(crate) fn checked` |
| The orchestration module | `crates/rendering/src/renderer.rs` | `Renderer`, `render_frame`, `submit` |
| An internal-only module | `crates/rendering/src/budget.rs` | `BudgetController` (no re-export) |
| An inline module block | `crates/protocol/src/dto.rs` | `mod coerce` |

## Related

- [Cargo workspace and crate model](../cargo-workspace/) — crates vs modules
- [The crate DAG](../module-dag/) — where the rendering crate sits in the graph
- [Render graph overview](../../frame-and-render-graph/render-graph-overview/) — the `RgPass` API
