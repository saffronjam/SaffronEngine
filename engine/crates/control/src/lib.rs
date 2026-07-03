//! The control plane: the synchronous `AF_UNIX` command server, the fn-pointer
//! command registry, the `EngineContext` borrow seam, and the wire dispatch from
//! request DTOs (`saffron-protocol`) to engine subsystems.
//!
//! A non-blocking, single-threaded socket drained once per frame from the host's
//! main loop — no async runtime, no worker thread. A request is one
//! newline-delimited compact-JSON line; the reply echoes the request `id` and
//! carries `ok` plus exactly one of `result` / `error`. The two builtin commands
//! `ping` and `help` land here; the domain phases register their handlers onto
//! the registry.

#![deny(unsafe_code)]

mod commands_animation;
mod commands_asset;
mod commands_physics;
mod commands_render;
mod commands_scene;
mod context;
mod error;
mod project_loader;
mod registry;
mod selector;
mod server;
#[cfg(test)]
mod test_support;

pub use context::ControlContext;
pub use error::{Error, Result};
pub use registry::{
    Command, CommandRegistry, ControlRenderer, EngineContext, is_read_only_command, positional_or,
    register_builtin_commands,
};
pub use selector::{entity_ref_dto, entity_uuid, resolve_entity};
pub use server::{ControlServer, control_socket_path, start_control_server};
