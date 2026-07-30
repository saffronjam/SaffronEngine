//! Job inputs, preflight budgets, and job results.

use super::*;

use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, SurfaceField, WorldBounds, WorldCellKey, WorldPosition};

use crate::{Error, PlantPoint, Result};

/// One explicit authored anchor with its stable vegetation-layer ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluationAnchor {
    /// Stable authored vegetation layer owning the anchor.
    pub layer: u128,
    /// Complete canonical explicit point.
    pub point: PlantPoint,
}

/// Immutable inputs available to one cell job, including its halo.
#[derive(Clone)]
pub struct GraphEvaluationInputs {
    pub map: Uuid,
    /// Stable local biome-instance identity used by procedural plant IDs and provenance.
    pub biome_instance: u128,
    pub output_cell: WorldCellKey,
    pub output_bounds: WorldBounds,
    /// Immutable halo bounds read by partitioned nodes.
    pub read_bounds: WorldBounds,
    /// Regions available to region-input nodes.
    pub regions: Vec<EvaluationRegion>,
    /// Splines available to spline-input nodes.
    pub splines: Vec<EvaluationSpline>,
    pub anchors: Vec<EvaluationAnchor>,
    /// Complete plant prototypes for every family the graph can emit.
    pub plant_prototypes: Vec<PlantPrototype>,
    /// Immutable ecology snapshot tick for read-only succession inputs.
    pub ecology_tick: u64,
    /// Quantized authored field tiles.
    pub fields: Vec<EvaluationFieldTile>,
    /// Precomputed canonical surface projection tiles for authoritative nodes.
    pub surface_projection_tiles: Vec<QuantizedSurfaceProjectionTile>,
    /// Exact canonical surface-field queries for authoritative nodes.
    pub surface_field_query_tiles: Vec<QuantizedSurfaceFieldQueryTile>,
    /// Exact content identity shared by every surface-derived tile in this job.
    pub surface_provider_set_hash: [u8; 32],
    /// Immutable surface providers sorted by descriptor ID during evaluation.
    pub surface_providers: Vec<Arc<dyn SurfaceField>>,
    /// Temporary render origin. It never enters candidate or plant identity.
    pub render_origin: WorldPosition,
}

impl GraphEvaluationInputs {
    /// Creates a cell job with an exact symmetric halo in Q15.16 metres.
    pub fn for_cell(
        map: Uuid,
        biome_instance: u128,
        cell: WorldCellKey,
        halo: DecisionScalar,
    ) -> Result<Self> {
        let output_bounds = cell.bounds();
        let halo_ticks = fixed_meters_to_ticks(halo)?.unsigned_abs() as i128;
        let minimum = output_bounds.min_ticks();
        let maximum = output_bounds.max_ticks_exclusive();
        let mut read_minimum = [0_i128; 3];
        let mut read_maximum = [0_i128; 3];
        for axis in 0..3 {
            read_minimum[axis] = minimum[axis]
                .checked_sub(halo_ticks)
                .ok_or(Error::NumericOverflow)?;
            read_maximum[axis] = maximum[axis]
                .checked_add(halo_ticks)
                .ok_or(Error::NumericOverflow)?;
        }
        let read_bounds = WorldBounds::new(read_minimum, read_maximum)?;
        Ok(Self {
            map,
            biome_instance,
            output_cell: cell,
            output_bounds,
            read_bounds,
            regions: canonical_cell_regions(read_bounds, cell.level(), 0)?,
            splines: Vec::new(),
            anchors: Vec::new(),
            plant_prototypes: Vec::new(),
            ecology_tick: 0,
            fields: Vec::new(),
            surface_projection_tiles: Vec::new(),
            surface_field_query_tiles: Vec::new(),
            surface_provider_set_hash: [0; 32],
            surface_providers: Vec::new(),
            render_origin: WorldPosition::origin(),
        })
    }

    /// Replaces the default halo regions with one clipped hierarchical authored region.
    pub fn set_hierarchical_region(&mut self, namespace: u128, bounds: WorldBounds) -> Result<()> {
        let bounds =
            intersect_bounds(bounds, self.read_bounds)?.ok_or_else(|| Error::GraphDocument {
                path: "evaluation.regions".to_owned(),
                reason: "hierarchical region does not intersect the immutable read bounds"
                    .to_owned(),
            })?;
        self.regions = canonical_cell_regions(bounds, self.output_cell.level(), namespace)?;
        Ok(())
    }
}

/// Immutable inputs for one compiler-owned ancestor/global stage tile.
#[derive(Clone)]
pub struct GlobalStageEvaluationInputs {
    /// Stable compiled stage identity.
    pub stage: [u8; 32],
    /// Canonical ancestor cell that owns the solved tile.
    pub owner: WorldCellKey,
    /// Exact finite solve domain. It must equal the owner cell bounds.
    pub solve_bounds: WorldBounds,
    /// Canonical identity of every immutable input visible to this stage tile.
    pub input_snapshot: [u8; 32],
    /// Complete immutable inputs over the stage's expanded read domain.
    pub inputs: GraphEvaluationInputs,
}

/// One atomic graph job containing output cells and every unique global-stage input tile.
#[derive(Clone, Default)]
pub struct GraphEvaluationJobInputs {
    /// Partitioned output-cell inputs.
    pub cells: Vec<GraphEvaluationInputs>,
    /// Deduplicated global-stage tiles in arbitrary request order.
    pub global_stages: Vec<GlobalStageEvaluationInputs>,
}

/// Checked work and retained-memory prediction for one complete atomic graph job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEvaluationPreflight {
    /// Partitioned output cells admitted by the job.
    pub output_cells: u64,
    /// Unique compiler-owned global-stage tiles admitted by the job.
    pub global_stage_tiles: u64,
    /// Caller-supplied and preparation-generated input tiles admitted by the job.
    pub input_tiles: u64,
    /// Caller-supplied immutable input allocations retained while the job runs.
    pub retained_input_bytes: u64,
    /// Canonical input allocations created by preparation and retained for replay and publication.
    pub generated_input_bytes: u64,
    /// Conservative total candidate-stream peak across every evaluated scope.
    pub candidate_count: u64,
    /// Conservative total accepted macro points.
    pub accepted_count: u64,
    /// Exact total quantized micro samples.
    pub micro_samples: u64,
    /// Peak requested evaluator-owned bytes during symbolic admission.
    pub preflight_peak_bytes: u64,
    /// Peak requested evaluator-owned bytes during execution and atomic result assembly.
    pub execution_peak_bytes: u64,
    /// Greater of the preflight and execution peaks.
    pub memory_bytes: u64,
    /// Exact resident-program transfer bytes for the selected execution plan.
    pub transfer_bytes: u64,
    /// Bounded cell workers participating in the job.
    pub worker_count: u16,
    /// Maximum wall-clock duration admitted for execution.
    pub time_limit_ms: u64,
    /// Hard limits enforced by this preflight and the matching evaluator run.
    pub limits: crate::GraphSafetyLimits,
}

/// Published result and retained-memory accounting for one global-stage tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalStageEvaluationResult {
    /// Stable compiled stage identity.
    pub stage: [u8; 32],
    /// Canonical ancestor cell that owns the solved tile.
    pub owner: WorldCellKey,
    /// Public macro, micro, provenance, and diagnostic products of the stage.
    pub result: GraphEvaluationResult,
    /// Conservative evaluator-owned requested bytes retained for downstream replay in this job.
    pub resident_bytes: u64,
}

/// Complete atomic result of one graph job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphEvaluationJobResult {
    /// Canonically sorted partitioned output-cell results.
    pub cells: Vec<GraphEvaluationResult>,
    /// Canonically sorted global-stage tile results, published exactly once.
    pub global_stages: Vec<GlobalStageEvaluationResult>,
}
