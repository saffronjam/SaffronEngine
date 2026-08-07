//! Deterministic authored-map to immutable vegetation-generation cooking.

mod commit;
mod dependencies;
mod measure;
mod platform;
mod stage;
#[cfg(test)]
pub(crate) mod test_support;
mod work;

pub use commit::commit_staged_vegetation_cook;
pub use platform::{portable_vegetation_platform_profile, vegetation_cook_versions};
pub use stage::stage_vegetation_cook;

use std::sync::Arc;

use saffron_core::Uuid;
use saffron_spatial::{SurfaceField, WorldCellKey};
use saffron_vegetation::{
    CandidateRejectionReason, ContentHash, CookGraph, CookNodeAddress, CookPlatformProfile,
    GraphCancellationToken, VegetationBaseManifest,
};

use crate::{AuthoredInputGuard, CookProjectView, Error, Result, VegetationArtifactPublication};

/// Complete immutable input scope for one world cook.
pub struct VegetationCookRequest {
    /// Stable world identity owning the scene-level vegetation field.
    pub world: Uuid,
    /// Authored vegetation-map asset.
    pub map: Uuid,
    /// Current generation root captured before this job was queued.
    pub expected_manifest: Option<ContentHash>,
    /// Exact output cells. The cooker canonicalizes order and rejects duplicates.
    pub cells: Vec<WorldCellKey>,
    /// Read-only ecology snapshot tick visible to graph rules.
    pub ecology_tick: u64,
    /// Bounded evaluator worker count.
    pub workers: u16,
    /// Complete platform profile participating in every cook key.
    pub platform: CookPlatformProfile,
    /// Immutable surface snapshots captured before the worker starts.
    pub surface_providers: Vec<Arc<dyn SurfaceField>>,
}

/// Monotonic event emitted by the synchronous cooker for asynchronous job observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VegetationCookEvent {
    /// The exact graph node count is known.
    Planned { total_nodes: u64 },
    /// Work began for one logical output.
    Started { node: CookNodeAddress },
    /// One node completed validation and any required atomic publication.
    Completed {
        node: CookNodeAddress,
        cache_hit: bool,
        published_cell: bool,
    },
}

/// Aggregate measured work and typed rejection counts for one generation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationCookStatistics {
    /// Cook-graph node count.
    pub nodes: u64,
    /// Total measured wall time.
    pub elapsed_micros: u64,
    /// Maximum measured resident-memory requirement of any node.
    pub peak_memory_bytes: u64,
    /// Canonical input bytes consumed by all nodes.
    pub input_bytes: u64,
    /// Validated output bytes produced by all nodes.
    pub output_bytes: u64,
    /// Nodes satisfied by a validated existing artifact.
    pub cache_hits: u64,
    /// Nodes that executed and produced new bytes.
    pub cache_misses: u64,
    /// Immutable cells published by this generation.
    pub published_cells: u64,
    /// Typed rejection totals in stable enum order.
    pub rejections: Vec<(CandidateRejectionReason, u64)>,
}

/// Atomically published result of one complete vegetation generation cook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationCookOutput {
    /// Complete canonical dependency DAG and node measurements.
    pub cook_graph: CookGraph,
    /// Complete immutable base manifest.
    pub manifest: VegetationBaseManifest,
    /// Manifest identity used by saves and future sessions.
    pub manifest_identity: ContentHash,
    /// CAS publication and current-root update result.
    pub publication: VegetationArtifactPublication,
    /// Aggregate observational statistics.
    pub statistics: VegetationCookStatistics,
}

/// One authored plant update admitted only if the staged generation commits.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantSourceAcceptance {
    /// Plant-family catalog identity.
    pub family: Uuid,
    /// Exact authored bytes read by the worker.
    pub expected_authored_hash: ContentHash,
    /// Accepted source-observation state produced by the compiler.
    pub accepted_asset: saffron_vegetation::PlantFamilyAsset,
}

/// Complete immutable cook result awaiting main-thread authored/root publication.
#[derive(Clone, Debug)]
pub struct StagedVegetationCook {
    /// Project roots and catalog snapshot used by the worker.
    pub project: CookProjectView,
    /// Vegetation map whose visible generation may advance.
    pub map: Uuid,
    /// Generation root captured when the job was queued.
    pub expected_manifest: Option<ContentHash>,
    /// Exact file spans read while staging.
    pub authored_guards: Vec<AuthoredInputGuard>,
    /// Surface snapshot identities read by the evaluator.
    pub surface_descriptors: Vec<saffron_spatial::SurfaceProviderDescriptor>,
    /// Authored source hashes accepted by successful plant compilation.
    pub source_acceptances: Vec<PlantSourceAcceptance>,
    /// Complete canonical dependency graph.
    pub cook_graph: CookGraph,
    /// Complete immutable base manifest.
    pub manifest: VegetationBaseManifest,
    /// Canonical manifest bytes already resident in CAS.
    pub manifest_bytes: Vec<u8>,
    /// Manifest content identity.
    pub manifest_identity: ContentHash,
    /// Observational work statistics retained by the job manager.
    pub statistics: VegetationCookStatistics,
}

pub(super) fn clone_surface_providers(
    providers: &[Arc<dyn SurfaceField>],
) -> Vec<Arc<dyn SurfaceField>> {
    providers.iter().map(Arc::clone).collect()
}

pub(super) fn bounds_intersect(
    left: saffron_spatial::WorldBounds,
    right: saffron_spatial::WorldBounds,
) -> bool {
    let left_minimum = left.min_ticks();
    let left_maximum = left.max_ticks_exclusive();
    let right_minimum = right.min_ticks();
    let right_maximum = right.max_ticks_exclusive();
    (0..3).all(|axis| {
        left_minimum[axis] < right_maximum[axis] && right_minimum[axis] < left_maximum[axis]
    })
}

pub(super) fn cancellation_checkpoint(cancellation: &GraphCancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Vegetation(saffron_vegetation::Error::GraphCancelled))
    } else {
        Ok(())
    }
}
