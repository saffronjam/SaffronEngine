//! `saffron-runtime`: the shared play-mode simulation spine.
//!
//! [`RuntimeSession`] bundles the per-frame "advance the world" work as one code path consumed by
//! both the editor host's play mode and the standalone `saffron-player`. It owns the simulation
//! subsystems but neither the scene nor the asset server — both are handed in per call — and has no
//! window, renderer, control-plane, or scene-edit dependency.

#![deny(unsafe_code)]

mod bridge;
mod session;
mod vegetation;
mod vegetation_collision;
mod vegetation_ecology;
mod vegetation_family;
mod vegetation_navigation;
mod vegetation_promotion;
mod vegetation_telemetry;

pub use bridge::{
    RuntimeScriptBridge, ScriptLogLine, SharedPhysics, SharedScene, SharedScriptSink,
};
pub use saffron_vegetation::{
    CandidateRejectionReason, PlantCollisionShape, PlantId, PlantLifecycle,
    VegetationCellSectionKind, VegetationWorld, decode_vegetation_rejection_diagnostics,
};
pub use session::RuntimeSession;
pub use vegetation::{
    VegetationRuntimeBindingStatus, VegetationRuntimeError, VegetationRuntimeUnavailableReason,
};
pub use vegetation_collision::VegetationCollisionReport;
pub use vegetation_ecology::VegetationEcologyClock;
pub use vegetation_navigation::{
    NavigationContribution, NavigationContributionKind, VegetationNavigationReport,
    VegetationNavigationSeam,
};
pub use vegetation_promotion::{
    PlantPromotionState, VegetationPromotion, VegetationPromotionError, VegetationPromotionReport,
};
pub use vegetation_telemetry::{
    VegetationStage, VegetationStageTimes, VegetationTelemetry, VegetationWorkCounters,
};
