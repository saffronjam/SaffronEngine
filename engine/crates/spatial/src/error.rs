//! The spatial foundation's typed error model.

/// Failures raised by coordinate, numeric, surface, and residency operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A hierarchy level exceeds the canonical key domain.
    #[error("hierarchy level {0} exceeds the maximum")]
    HierarchyLevel(u8),
    /// A cell coordinate cannot represent a complete cell at its hierarchy level.
    #[error("cell coordinate lies outside the level domain")]
    CellCoordinateRange,
    /// An exact cell operation would overflow the canonical domain.
    #[error("cell operation overflowed")]
    CellOverflow,
    /// A byte sequence is not a canonical cell encoding.
    #[error("invalid canonical cell encoding")]
    InvalidCellEncoding,
    /// Exact cell enumeration exceeds its caller-supplied hard bound.
    #[error("cell enumeration limit exceeded: requested {requested}, limit {limit}")]
    CellEnumerationLimit {
        /// Exact number of intersecting cells.
        requested: u64,
        /// Maximum number admitted by the caller.
        limit: u64,
    },
    /// A local tick lies outside the half-open base cell.
    #[error("local position lies outside the half-open base cell")]
    LocalPositionRange,
    /// A position carries a non-base-level cell.
    #[error("world positions require a level-zero cell")]
    PositionCellLevel,
    /// A floating input is NaN or infinite.
    #[error("numeric input must be finite")]
    NonFinite,
    /// A numeric conversion or checked operation exceeds its representation.
    #[error("numeric operation overflowed")]
    NumericOverflow,
    /// A divisor is zero.
    #[error("division by zero")]
    DivisionByZero,
    /// A normalized input lies outside its closed range.
    #[error("normalized input lies outside its range")]
    NormalizedRange,
    /// A curve has duplicate or descending abscissae.
    #[error("curve points must have unique ascending abscissae")]
    CurveOrder,
    /// A direction or frame axis is zero, non-finite, or degenerate.
    #[error("surface direction or frame is degenerate")]
    DegenerateDirection,
    /// A provider transform is non-finite or non-invertible.
    #[error("surface provider transform is non-finite or non-invertible")]
    InvalidSurfaceTransform,
    /// Surface barycentrics do not form a canonical unit sum.
    #[error("surface barycentric weights must sum to one")]
    InvalidBarycentrics,
    /// A surface query distance is negative or non-finite.
    #[error("surface query distance must be finite and non-negative")]
    InvalidDistance,
    /// A requested field channel is unavailable from the provider.
    #[error("surface field channel is unavailable")]
    FieldUnavailable,
    /// Residency radii or prediction values violate the source contract.
    #[error("invalid spatial source configuration")]
    InvalidSpatialSource,
    /// A residency reference count would overflow.
    #[error("residency reference count overflowed")]
    ResidencyOverflow,
    /// A residency request exceeds the manager's explicit cell budget.
    #[error("residency request exceeds the configured cell budget")]
    ResidencyBudgetExceeded,
    /// A generation token belongs to a different cell than its publication slot.
    #[error("generation token targets a different cell")]
    GenerationCellMismatch,
    /// A generation counter has exhausted its non-repeating identity space.
    #[error("generation identity space exhausted")]
    GenerationExhausted,
}

/// The spatial crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;
