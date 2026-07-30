//! The control-plane DTOs, in field declaration order (the positional-CLI-argument /
//! OpenRPC-`required` order).
//!
//! Every struct is `camelCase` on the wire, every `Option` field is omitted rather than
//! serialized as `null`, and every enum is a kebab-case string whose unknown values are a
//! `Deserialize` error. `WireUuid` maps to the [`Uuid`](crate::Uuid) newtype; truly open JSON
//! payloads stay `serde_json::Value`.

mod animation;
mod asset;
pub(crate) mod coerce;
mod common;
mod environment;
mod grading;
mod material;
mod physics;
mod profiling;
mod project;
mod render;
mod render_stats;
mod scene;
mod script;
mod spatial;
mod viewport;

pub use animation::*;
pub use asset::*;
pub use common::*;
pub use environment::*;
pub use grading::*;
pub use material::*;
pub use physics::*;
pub use profiling::*;
pub use project::*;
pub use render::*;
pub use render_stats::*;
pub use scene::*;
pub use script::*;
pub use spatial::*;
pub use viewport::*;
