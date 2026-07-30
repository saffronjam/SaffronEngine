//! The control-plane DTO crate: the single source of truth for the wire types, shared by the
//! engine, the `sa` CLI, and the protocol codegen (`serde`/`schemars`/`ts-rs` derives).
//!
//! Field declaration order is load-bearing — it is the positional-CLI-argument order and the
//! OpenRPC `required` order. Depends only on `saffron-core` so the `sa` CLI links the DTOs
//! without the engine.

#![deny(unsafe_code)]

mod codegen;
mod command;
mod control_dto;
mod dto;
mod scene_dto;
mod schema;
mod uuid;
mod vegetation_dto;

pub use codegen::{schema_fragments, ts_decls};
pub use command::{
    COMMAND_FIXTURES, COMMAND_SKIPS, COMMANDS, CommandSpec, DTO_TYPE_NAMES, HELP_COMMAND,
    HELP_SKIP_REASON, fixture_for, skip_for,
};
pub use control_dto::*;
pub use dto::*;
pub use scene_dto::*;
pub use schema::{fragment_for, positional_field_order, standalone_schema_for};
pub use uuid::Uuid;
pub use vegetation_dto::*;
