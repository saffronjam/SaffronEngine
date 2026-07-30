//! The render-domain control commands: statistics, the profiler and capture group, perf config,
//! frame history, alarms, the render-feature toggles, anti-aliasing, quality tiers, tonemap and
//! grading, native viewport info and size, and reflection-probe management.
//!
//! Every handler reaches only [`EngineContext::renderer`][crate::registry::EngineContext], which
//! makes this the thinnest domain: a DTO-to-renderer query/setter bridge over the
//! [`ControlRenderer`][crate::registry::ControlRenderer] trait.

use crate::registry::CommandRegistry;

mod probes;
mod profiling;
mod settings;
mod stats;
#[cfg(test)]
mod tests;

pub(crate) use profiling::*;
pub(crate) use settings::*;
pub(crate) use stats::*;

/// Registers the render-domain commands in the frozen manifest order.
pub fn register_render_commands(reg: &mut CommandRegistry) {
    stats::register_stats(reg);
    profiling::register_profiling(reg);
    settings::register_upscale(reg);
    profiling::register_alarms(reg);
    settings::register_toggles(reg);
    probes::register_probes(reg);
}
