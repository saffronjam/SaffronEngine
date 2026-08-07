//! The control plane: the synchronous `AF_UNIX` command server, the fn-pointer command registry,
//! the `EngineContext` borrow seam, and the wire dispatch from request DTOs to engine subsystems.
//!
//! A non-blocking, single-threaded socket drained once per frame from the host's main loop — no
//! async runtime, no worker thread. One request is one newline-delimited compact-JSON line; the
//! reply echoes the request `id` and carries `ok` plus exactly one of `result` / `error`.

#![deny(unsafe_code)]

mod botanical_dto;
mod commands_animation;
mod commands_asset;
mod commands_physics;
mod commands_render;
mod commands_scene;
mod commands_vegetation;
mod commands_vegetation_runtime;
mod context;
mod error;
mod owned_worker;
mod project_loader;
mod registry;
mod selector;
mod server;
#[cfg(test)]
mod test_support;
mod vegetation_cook_dto;
mod vegetation_cook_jobs;
mod vegetation_jobs;
mod vegetation_layer_dto;
mod vegetation_mutation_dto;

pub use commands_asset::{PreviewSubject, build_preview_scene_for_thumbnail};
pub use context::{ControlContext, ControlPollContext};
pub use error::{Error, Result};
pub use registry::{
    Command, CommandRegistry, ControlRenderer, EngineContext, PlantWindRecord, SelectionPick,
    VegetationComputeExecutor, is_read_only_command, positional_or, register_builtin_commands,
};
pub use selector::{entity_ref_dto, entity_uuid, resolve_entity};
pub use server::{ControlServer, control_socket_path, start_control_server};
