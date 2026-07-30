//! Typed biome-graph IR, module compilation, authority flow, and execution planning.

mod compile;
mod compiled;
mod demand;
mod document;
mod domain;
mod estimate;
mod gpu_qualification;
mod halo;
mod json;
mod operator;
mod spatial;
mod validate;

#[cfg(test)]
mod tests;

pub use compile::*;
pub use compiled::*;
pub use document::*;
pub use domain::*;
pub use estimate::*;
pub use gpu_qualification::*;
pub use operator::*;
pub use spatial::*;

pub(crate) use demand::{CompiledDemandPlan, CompiledDemandSlice, CompiledDemandUnitSlice};

use demand::*;
use halo::*;
use json::*;
use validate::*;

/// Current typed biome-graph document version.
pub const BIOME_GRAPH_VERSION: u32 = 1;
/// Current strict public-interface schema version.
pub const BIOME_INTERFACE_VERSION: u32 = 1;
/// Current semantic version of every initial graph operator.
pub const BIOME_NODE_VERSION: u32 = 1;
