//! Canonical vegetation assets, identity, point schema, and mutation reduction.
//!
//! This crate owns authored and persistent vegetation value contracts. It performs no asset-server
//! I/O, rendering, physics, scene-ECS mutation, control transport, or editor work. Those systems
//! consume these formats without becoming alternate sources of vegetation truth.

#![deny(unsafe_code)]

mod artifact;
mod asset;
mod binary;
mod botanical;
mod botanical_compile;
mod botanical_edit;
mod canonical;
mod cell_facet;
mod codec;
mod cook;
mod ecology;
mod ecology_region;
mod ecology_tick;
mod error;
mod evaluator;
mod graph;
mod graph_gpu;
mod hash;
mod identity;
mod interchange;
mod interchange_usd;
mod layer;
mod manifest;
mod memory;
mod merge;
mod mutation;
mod plant_compile;
mod point;
mod runtime_world;
mod season;
mod state_codec;
mod virtual_hierarchy;

pub use artifact::*;
pub use asset::*;
pub use botanical::*;
pub use botanical_compile::*;
pub use botanical_edit::*;
pub use cell_facet::*;
pub use codec::*;
pub use cook::*;
pub use ecology::*;
pub use ecology_region::*;
pub use ecology_tick::*;
pub use error::{Error, Result};
pub use evaluator::*;
pub use graph::*;
pub use graph_gpu::*;
pub use hash::VegetationContentHasher;
pub use identity::*;
pub use interchange::*;
pub use interchange_usd::*;
pub use layer::*;
pub use manifest::*;
pub use merge::*;
pub use mutation::*;
pub use plant_compile::*;
pub use point::*;
pub use runtime_world::*;
pub use saffron_material::*;
pub use saffron_spatial::QuantizedOrientation;
pub use season::{
    resolve_rendered_phenotype, role_season_window, season_in_window, season_phase_mille,
};
pub use virtual_hierarchy::*;

/// Computes the pinned SHA-256 identity used by vegetation assets and graph dependencies.
#[must_use]
pub fn vegetation_content_hash(bytes: &[u8]) -> [u8; 32] {
    hash::sha256(bytes)
}
