//! Canonical vegetation assets, identity, point schema, and mutation reduction.
//!
//! This crate owns authored and persistent vegetation value contracts. It performs no asset-server
//! I/O, rendering, physics, scene-ECS mutation, control transport, or editor work. Those systems
//! consume these formats without becoming alternate sources of vegetation truth.

#![deny(unsafe_code)]

mod asset;
mod codec;
mod error;
mod evaluator;
mod graph;
mod graph_gpu;
mod hash;
mod identity;
mod layer;
mod material;
mod mutation;
mod point;

pub use asset::*;
pub use codec::*;
pub use error::{Error, Result};
pub use evaluator::*;
pub use graph::*;
pub use graph_gpu::*;
pub use identity::*;
pub use layer::*;
pub use material::*;
pub use mutation::*;
pub use point::*;

/// Computes the pinned SHA-256 identity used by vegetation assets and graph dependencies.
#[must_use]
pub fn vegetation_content_hash(bytes: &[u8]) -> [u8; 32] {
    hash::sha256(bytes)
}
