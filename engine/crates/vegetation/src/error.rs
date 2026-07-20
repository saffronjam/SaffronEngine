//! The vegetation domain's typed error model.

/// Failures raised by vegetation formats, identity, schemas, mutations, and manifests.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An asset document has an unsupported version.
    #[error("unsupported {format} version {found}; expected {expected}")]
    FormatVersion {
        /// Logical format name.
        format: &'static str,
        /// Version read from the document.
        found: u32,
        /// Version accepted by this build.
        expected: u32,
    },
    /// An asset document is missing required typed data or carries an invalid value.
    #[error("invalid {format} format at '{field}'")]
    InvalidFormat {
        /// Logical format name.
        format: &'static str,
        /// Field path that failed validation.
        field: String,
    },
    /// A plant ID string or namespace is not canonical.
    #[error("invalid plant identity")]
    InvalidPlantId,
    /// Two source records produced the same plant identity.
    #[error("duplicate plant identity {0}")]
    DuplicatePlantId(String),
    /// A point column is missing, duplicated, mistyped, or has the wrong row count.
    #[error("invalid point schema: {0}")]
    PointSchema(String),
    /// A mutation violates its target, precondition, transaction, or lifecycle contract.
    #[error("invalid vegetation mutation: {0}")]
    Mutation(String),
    /// Persistent state is bound to a different immutable base manifest.
    #[error("vegetation manifest mismatch")]
    ManifestMismatch,
    /// A checked integer or fixed-point operation overflowed.
    #[error("vegetation numeric operation overflowed")]
    NumericOverflow,
    /// A typed biome-graph document is malformed.
    #[error("invalid biome graph at '{path}': {reason}")]
    GraphDocument {
        /// Stable document path or node/pin label.
        path: String,
        /// Typed validation failure.
        reason: String,
    },
    /// The graph contains a dependency cycle.
    #[error("biome graph contains a cycle through node {node:032x}")]
    GraphCycle {
        /// Stable node or module-call GUID closing the cycle.
        node: u128,
    },
    /// A graph edge connects incompatible domains.
    #[error(
        "biome graph edge {from_node:032x}.{from_pin} -> {to_node:032x}.{to_pin} connects {from_domain} to {to_domain}"
    )]
    GraphTypeMismatch {
        /// Source node GUID.
        from_node: u128,
        /// Source pin.
        from_pin: String,
        /// Source domain.
        from_domain: &'static str,
        /// Destination node GUID.
        to_node: u128,
        /// Destination pin.
        to_pin: String,
        /// Destination domain.
        to_domain: &'static str,
    },
    /// A cosmetic or non-qualified value reaches authoritative state.
    #[error("graph authority violation at node {node:032x}: {reason}")]
    GraphAuthority {
        /// Stable node GUID.
        node: u128,
        /// Taint-flow explanation.
        reason: String,
    },
    /// A partition-local node has unbounded influence.
    #[error("graph node {node:032x} has unbounded local influence")]
    GraphUnboundedInfluence {
        /// Stable node GUID.
        node: u128,
    },
    /// A graph estimate or evaluation exceeds an explicit safety limit.
    #[error("graph {resource} limit exceeded: requested {requested}, limit {limit}")]
    GraphLimit {
        /// Count, bytes, transfer, recursion, or time budget.
        resource: &'static str,
        /// Requested amount.
        requested: u64,
        /// Configured maximum.
        limit: u64,
    },
    /// Evaluation was cancelled before atomic publication.
    #[error("biome graph evaluation cancelled")]
    GraphCancelled,
    /// Runtime-authoritative evaluation is missing a required canonical input tile.
    #[error("graph node {node:032x} is missing authoritative input '{input}'")]
    GraphAuthoritativeInput {
        /// Stable node GUID.
        node: u128,
        /// Missing field, projection tile, provider revision, or content identity.
        input: String,
    },
    /// An `EquivalentGpu` node lacks matching Rust/Slang evidence for the active profile.
    #[error("graph node {node:032x} is not GPU-equivalence-qualified for profile '{profile}'")]
    GraphGpuQualification {
        /// Stable node GUID.
        node: u128,
        /// Device/profile identity.
        profile: String,
    },
    /// A qualified runtime Slang dispatch failed before producing a complete batch.
    #[error("vegetation graph compute failed for profile '{profile}': {reason}")]
    GraphGpuExecution {
        /// Exact device/profile identity.
        profile: String,
        /// Renderer or output-contract failure.
        reason: String,
    },
    /// A nested shared spatial operation failed.
    #[error("spatial error: {0}")]
    Spatial(#[from] saffron_spatial::Error),
    /// A nested canonical JSON operation failed.
    #[error("json error: {0}")]
    Json(#[from] saffron_json::Error),
}

/// The vegetation crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;
