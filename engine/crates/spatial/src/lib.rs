//! Shared world coordinates, deterministic numerics, surface fields, and facet residency.
//!
//! This crate is a leaf foundation. It knows no scene, renderer, physics world, asset server,
//! editor, or network transport. Systems publish providers and sources through these value-level
//! contracts instead of defining feature-specific grids or coordinate policies.

#![deny(unsafe_code)]

mod coordinate;
mod error;
mod numeric;
mod plant_identity;
mod random;
mod residency;
mod surface;

pub use coordinate::{
    BASE_CELL_EDGE_METERS, BASE_CELL_TICKS, LOCAL_FRACTION_BITS, LOCAL_TICKS_PER_METER,
    MAX_HIERARCHY_LEVEL, QuantizedLocalPosition, WorldBounds, WorldCellKey, WorldPosition,
    world_cell_count_covering_bounds, world_cells_covering_bounds,
};
pub use error::{Error, Result};
pub use numeric::{
    CanonicalF32, DecisionCurve, DecisionHessian3, DecisionScalar, DecisionVec3, FixedI32,
    QuantizedOrientation, SignedUnit, UnitInterval, div_round_ties_even,
};
pub use plant_identity::{PlantId, PlantIdNamespace};
pub use random::{PHILOX4X32_ZERO_VECTOR, RandomDomain, RandomStream, philox4x32_10};
pub use residency::{
    DEFAULT_SOURCE_CLAIM_BUDGET, FACET_COUNT, GenerationSlot, GenerationToken, ResidencyFacet,
    ResidencyManager, ResidencyMask, ResidencySnapshot, SourceLevel, SpatialSource,
    SpatialSourceId,
};
pub use surface::{
    FieldAvailability, FieldChannel, FieldDerivative, FieldSample, HessianFieldSample,
    SurfaceAttachment, SurfaceCapabilities, SurfaceCoordinates, SurfaceDirtyRegion, SurfaceField,
    SurfaceFrame, SurfaceHit, SurfaceNearestQuery, SurfacePrimitiveId, SurfaceProjection,
    SurfaceProviderDescriptor, SurfaceProviderId, SurfaceRay, SurfaceRevision, SurfaceTagId,
    SurfaceTileDescriptor, VectorFieldSample, WeightedSurfaceTag,
};
