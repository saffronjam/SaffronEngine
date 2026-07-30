//! Runtime vegetation residency, query, inspection, and strict state snapshot commands.

use saffron_protocol::VegetationRuntimeUnavailableReasonDto;

use crate::registry::CommandRegistry;

mod convert;
mod plants;
mod state;
#[cfg(test)]
mod tests;

pub(crate) use convert::*;
pub(crate) use plants::*;
pub(crate) use state::*;

pub(crate) const MAX_QUERY_RESULTS: usize = 100_000;

pub(crate) const DEFAULT_QUERY_RESULTS: usize = 10_000;

pub(crate) enum RuntimeAvailability {
    Unavailable {
        reason: VegetationRuntimeUnavailableReasonDto,
        detail: Option<String>,
    },
    Available,
}

/// Registers the runtime vegetation commands in the frozen manifest order.
pub(crate) fn register_runtime_vegetation_commands(reg: &mut CommandRegistry) {
    state::register_runtime_state(reg);
    plants::register_runtime_plants(reg);
    convert::register_runtime_mutation(reg);
}
