//! Canonical reference and parallel biome-graph evaluation.

use saffron_spatial::MAX_HIERARCHY_LEVEL;

mod cancel;
mod cell;
mod community;
mod cost;
mod diagnostics;
mod encode;
mod fields;
mod filters;
mod generate;
mod global;
mod inputs;
mod job;
mod math;
mod node;
mod outputs;
mod params;
mod preflight;
mod project;
mod regions;
mod resident;
mod result;
mod selection;
mod spacing;
mod splines;
mod state;
mod surface;
mod symbolic;
mod symbolic_bound;
mod symbolic_node;
mod transform;
mod traversal;
mod types;
mod unit;

#[cfg(test)]
mod tests;

pub use cancel::*;
pub use cell::*;
pub use diagnostics::*;
pub use inputs::*;
pub use result::*;
pub use surface::*;
pub use types::*;

pub(crate) use encode::*;

use community::*;
use cost::*;
use fields::*;
use filters::*;
use generate::*;
use global::*;
use job::*;
use math::*;
use node::*;
use outputs::*;
use params::*;
use preflight::*;
use project::*;
use regions::*;
use resident::*;
use selection::*;
use spacing::*;
use splines::*;
use state::*;
use symbolic::*;
use symbolic_bound::*;
use symbolic_node::*;
use transform::*;
use traversal::*;
use unit::*;

const EVALUATOR_WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;
const HIERARCHY_LEVEL_COUNT: usize = MAX_HIERARCHY_LEVEL as usize + 1;
