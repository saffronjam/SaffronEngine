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
    /// A plant-family tag identity is zero.
    #[error("invalid plant-family tag identity")]
    InvalidPlantTagId,
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
    /// A runtime operation referenced a cell outside the bound immutable manifest.
    #[error("vegetation cell {cell} is not present in the bound manifest")]
    UnknownRuntimeCell {
        /// Missing canonical cell.
        cell: saffron_spatial::WorldCellKey,
    },
    /// A runtime plant lookup referenced an identity absent from every resident macro facet.
    #[error("vegetation plant {plant} is not resident")]
    PlantNotResident {
        /// Missing stable plant identity.
        plant: String,
    },
    /// A generation-tagged handle was used after its immutable cell generation was replaced.
    #[error("stale vegetation generation for cell {cell}: expected {expected}, current {current}")]
    StaleGeneration {
        /// Canonical cell owning the handle.
        cell: saffron_spatial::WorldCellKey,
        /// Generation carried by the handle.
        expected: u64,
        /// Currently published generation.
        current: u64,
    },
    /// A requested runtime facet is not resident in the published cell generation.
    #[error("vegetation {facet} facet is not resident for cell {cell}")]
    FacetNotResident {
        /// Canonical cell.
        cell: saffron_spatial::WorldCellKey,
        /// Stable logical facet name.
        facet: &'static str,
    },
    /// Reading or seeking a derived artifact failed before validation completed.
    #[error("{format} artifact I/O failed")]
    ArtifactIo {
        /// Logical artifact format.
        format: &'static str,
        /// Underlying filesystem or stream failure.
        #[source]
        source: std::io::Error,
    },
    /// A derived artifact ended before its declared structure was complete.
    #[error("truncated {format} artifact")]
    ArtifactTruncated {
        /// Logical artifact format.
        format: &'static str,
    },
    /// A derived artifact's schema identity is not understood by this build.
    #[error("unsupported {format} schema")]
    ArtifactSchema {
        /// Logical artifact format.
        format: &'static str,
    },
    /// A derived artifact contains a section kind outside its exact format vocabulary.
    #[error("unknown {format} section kind {section}")]
    ArtifactUnknownSection {
        /// Logical artifact format.
        format: &'static str,
        /// Numeric section kind read from the TOC.
        section: u16,
    },
    /// A derived artifact requests a codec outside its exact format vocabulary.
    #[error("unknown {format} section codec {codec}")]
    ArtifactUnknownCodec {
        /// Logical artifact format.
        format: &'static str,
        /// Numeric codec identifier read from the TOC.
        codec: u8,
    },
    /// Encoding or decoding a recognized artifact section codec failed.
    #[error("{format} section {section} codec failed")]
    ArtifactCodec {
        /// Logical artifact format.
        format: &'static str,
        /// Numeric section kind being encoded or decoded.
        section: u16,
        /// Underlying codec failure.
        #[source]
        source: std::io::Error,
    },
    /// A declared artifact section size exceeds the bounded container contract.
    #[error("{format} section {section} {size_kind} size {requested} exceeds limit {limit}")]
    ArtifactSectionLimit {
        /// Logical artifact format.
        format: &'static str,
        /// Numeric section kind from the TOC.
        section: u16,
        /// Whether the stored or decoded size exceeded its bound.
        size_kind: &'static str,
        /// Declared byte count.
        requested: u64,
        /// Maximum accepted byte count.
        limit: u64,
    },
    /// The sum of stored or decoded artifact sections exceeds the caller's budget.
    #[error("{format} total {size_kind} size {requested} exceeds limit {limit}")]
    ArtifactTotalLimit {
        /// Logical artifact format.
        format: &'static str,
        /// Whether stored or decoded bytes exceeded the aggregate bound.
        size_kind: &'static str,
        /// Declared byte count across every section.
        requested: u64,
        /// Maximum accepted byte count.
        limit: u64,
    },
    /// A derived artifact declares one section kind more than once.
    #[error("duplicate {format} section kind {section}")]
    ArtifactDuplicateSection {
        /// Logical artifact format.
        format: &'static str,
        /// Duplicated numeric section kind.
        section: u16,
    },
    /// A derived artifact section violates its declared canonical alignment.
    #[error("misaligned {format} section kind {section}")]
    ArtifactMisalignedSection {
        /// Logical artifact format.
        format: &'static str,
        /// Misaligned numeric section kind.
        section: u16,
    },
    /// Two derived artifact section spans overlap.
    #[error("overlapping {format} section kind {section}")]
    ArtifactOverlappingSection {
        /// Logical artifact format.
        format: &'static str,
        /// Numeric section kind whose span overlaps a prior span.
        section: u16,
    },
    /// A derived artifact or section digest does not match its canonical bytes.
    #[error("{format} content hash mismatch at {subject}")]
    ArtifactHashMismatch {
        /// Logical artifact format.
        format: &'static str,
        /// Stable header, payload, section, or manifest subject.
        subject: String,
    },
    /// A derived artifact violates a structural invariant not represented by the narrower errors.
    #[error("invalid {format} artifact at '{field}'")]
    ArtifactFormat {
        /// Logical artifact format.
        format: &'static str,
        /// Exact structural field that failed validation.
        field: String,
    },
    /// A checked integer or fixed-point operation overflowed.
    #[error("vegetation numeric operation overflowed")]
    NumericOverflow,
    /// A vegetation-owned collection could not reserve its checked capacity.
    #[error("vegetation memory reservation failed for {resource}")]
    MemoryReservation {
        /// Stable evaluator collection or stage name.
        resource: &'static str,
        /// Allocation failure reported by the standard collection.
        #[source]
        source: std::collections::TryReserveError,
    },
    /// An evaluator worker could not be created with its bounded stack.
    #[error("failed to spawn vegetation graph worker")]
    GraphWorkerSpawn {
        /// Thread creation failure reported by the standard library.
        #[source]
        source: std::io::Error,
    },
    /// An evaluator worker panicked before returning its atomic cell results.
    #[error("vegetation graph worker panicked")]
    GraphWorkerPanicked,
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
    /// A material surface or coverage contract failed validation.
    #[error("material error: {0}")]
    Material(#[from] saffron_material::Error),
    /// A nested canonical JSON operation failed.
    #[error("json error: {0}")]
    Json(#[from] saffron_json::Error),
}

/// The vegetation crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;
