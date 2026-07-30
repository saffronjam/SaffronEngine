//! Conservative work and memory estimates plus the hard limits that bound them.

use crate::{Error, Result};

use super::*;

/// Hard planning and evaluation limits. Exceeding one aborts; quality is never reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphSafetyLimits {
    /// Maximum parallel cell workers admitted by one evaluator.
    pub max_workers: u16,
    /// Maximum output cells admitted by one bounded multi-cell evaluation.
    pub max_output_cells: u64,
    /// Maximum unique ancestor/global stage tiles admitted by one bounded evaluation.
    pub max_global_stage_tiles: u64,
    /// Maximum caller-supplied plus preparation-generated input tiles.
    pub max_input_tiles: u64,
    /// Maximum candidates admitted by a bounded evaluation.
    pub max_candidates: u64,
    pub max_macro_points: u64,
    pub max_micro_samples: u64,
    /// Maximum evaluator-owned heap and worker-stack bytes, including allocator and ordered-map
    /// metadata. Caller-owned graph/provider data, executor internals, and thread bookkeeping sit
    /// outside the bound.
    pub max_memory_bytes: u64,
    /// Maximum estimated CPU/GPU transfer bytes.
    pub max_transfer_bytes: u64,
    /// Maximum nested module-call edges from the root, whose depth is zero.
    pub max_module_depth: u16,
    /// Maximum wall-clock evaluation time in milliseconds.
    pub max_time_ms: u64,
}

impl Default for GraphSafetyLimits {
    fn default() -> Self {
        Self {
            max_workers: 256,
            max_output_cells: 1_000_000,
            max_global_stage_tiles: 1_000_000,
            max_input_tiles: 1_000_000,
            max_candidates: 16_000_000,
            max_macro_points: 4_000_000,
            max_micro_samples: 256_000_000,
            max_memory_bytes: 4 * 1024 * 1024 * 1024,
            max_transfer_bytes: 1024 * 1024 * 1024,
            max_module_depth: 32,
            max_time_ms: 60_000,
        }
    }
}

/// Predicted work and memory for one node or complete graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphEstimate {
    /// Maximum candidates emitted by the stage.
    pub candidates: u64,
    /// Maximum accepted points emitted by the stage.
    pub accepted: u64,
    /// Maximum quantized micro samples emitted by the stage.
    pub micro_samples: u64,
    /// Maximum live bytes.
    pub memory_bytes: u64,
    /// Maximum transfer bytes at execution-domain boundaries.
    pub transfer_bytes: u64,
}

pub(super) fn estimate_node(
    node: &GraphNodeDefinition,
    upstream: GraphEstimate,
) -> Result<GraphEstimate> {
    let mut estimate = upstream;
    match node.operator {
        GraphOperator::StratifiedCoverage | GraphOperator::BlueNoisePoisson => {
            estimate.candidates = u64::from(required_u32(node, "count")?);
        }
        GraphOperator::WeightedElimination => {
            estimate.candidates = estimate
                .candidates
                .min(u64::from(required_u32(node, "targetCount")?));
            let adjacency = upstream
                .candidates
                .checked_mul(u64::from(required_u32(node, "maximumNeighbours")?))
                .and_then(|value| value.checked_mul(16))
                .ok_or(Error::NumericOverflow)?;
            estimate.memory_bytes = estimate
                .memory_bytes
                .checked_add(adjacency)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::ClusterPatchColony => {
            estimate.candidates = estimate
                .candidates
                .checked_mul(u64::from(required_u32(node, "children")?) + 1)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::RecursiveCompanion => {
            let children = u64::from(required_u32(node, "children")?);
            let depth = u64::from(required_u32(node, "maximumDepth")?);
            let mut generation = 1_u64;
            let mut factor = 1_u64;
            for _ in 0..depth {
                generation = generation
                    .checked_mul(children)
                    .ok_or(Error::NumericOverflow)?;
                factor = factor
                    .checked_add(generation)
                    .ok_or(Error::NumericOverflow)?;
            }
            estimate.candidates = estimate
                .candidates
                .checked_mul(factor)
                .ok_or(Error::NumericOverflow)?;
        }
        GraphOperator::MacroOutput => estimate.accepted = estimate.candidates,
        GraphOperator::MicroOutput => {
            let dimensions = required_u32_vec3(node, "dimensions")?;
            let samples = dimensions.iter().try_fold(1_u64, |product, value| {
                product
                    .checked_mul(u64::from(*value))
                    .ok_or(Error::NumericOverflow)
            })?;
            estimate.micro_samples = samples;
            let attribute_channels = match node.parameter("attributeChannels") {
                Some(GraphParameterValue::GuidList(channels)) => channels.len() as u64,
                _ => 0,
            };
            let bytes_per_sample = attribute_channels
                .checked_mul(20)
                .and_then(|value| value.checked_add(10))
                .ok_or(Error::NumericOverflow)?;
            estimate.memory_bytes = estimate
                .memory_bytes
                .checked_add(
                    samples
                        .checked_mul(bytes_per_sample)
                        .ok_or(Error::NumericOverflow)?,
                )
                .ok_or(Error::NumericOverflow)?;
        }
        _ => {}
    }
    let candidate_bytes = estimate
        .candidates
        .checked_mul(128)
        .ok_or(Error::NumericOverflow)?;
    estimate.memory_bytes = estimate.memory_bytes.max(candidate_bytes);
    if node.authority != GraphAuthority::Authoritative && node.operator.has_slang_executor() {
        estimate.transfer_bytes = estimate.transfer_bytes.max(candidate_bytes);
    }
    Ok(estimate)
}

pub(super) fn enforce_limits(estimate: GraphEstimate, limits: GraphSafetyLimits) -> Result<()> {
    for (resource, requested, limit) in [
        (
            "candidate count",
            estimate.candidates,
            limits.max_candidates,
        ),
        ("accepted count", estimate.accepted, limits.max_macro_points),
        (
            "micro samples",
            estimate.micro_samples,
            limits.max_micro_samples,
        ),
        (
            "memory bytes",
            estimate.memory_bytes,
            limits.max_memory_bytes,
        ),
        (
            "transfer bytes",
            estimate.transfer_bytes,
            limits.max_transfer_bytes,
        ),
    ] {
        if requested > limit {
            return Err(Error::GraphLimit {
                resource,
                requested,
                limit,
            });
        }
    }
    Ok(())
}

pub(super) fn max_estimate(left: GraphEstimate, right: GraphEstimate) -> GraphEstimate {
    GraphEstimate {
        candidates: left.candidates.max(right.candidates),
        accepted: left.accepted.max(right.accepted),
        micro_samples: left.micro_samples.max(right.micro_samples),
        memory_bytes: left.memory_bytes.max(right.memory_bytes),
        transfer_bytes: left.transfer_bytes.max(right.transfer_bytes),
    }
}

pub(super) fn merge_input_estimate(
    left: GraphEstimate,
    right: GraphEstimate,
) -> Result<GraphEstimate> {
    Ok(GraphEstimate {
        candidates: left.candidates.max(right.candidates),
        accepted: left.accepted.max(right.accepted),
        micro_samples: left.micro_samples.max(right.micro_samples),
        memory_bytes: left
            .memory_bytes
            .checked_add(right.memory_bytes)
            .ok_or(Error::NumericOverflow)?,
        transfer_bytes: left
            .transfer_bytes
            .checked_add(right.transfer_bytes)
            .ok_or(Error::NumericOverflow)?,
    })
}
