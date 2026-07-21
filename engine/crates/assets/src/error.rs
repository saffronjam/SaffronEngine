//! The crate-root error type and `Result` alias.

/// Errors raised by the asset layer: project I/O, import/bake, material codegen,
/// and the container reader.
///
/// The negative-cache *load* path never surfaces an `Error` — a failed load is a logged
/// warn plus a cached `None`, not a fallible result.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem read or write failed. The payload is the OS message.
    #[error("io error: {0}")]
    Io(String),

    /// A JSON parse or typed-read failed (project / material documents).
    #[error("json error: {0}")]
    Json(#[from] saffron_json::Error),

    /// A geometry codec failed (mesh / clip / container byte format, image decode).
    #[error("geometry error: {0}")]
    Geometry(#[from] saffron_geometry::Error),

    /// A renderer upload or GPU operation failed.
    #[error("render error: {0}")]
    Render(#[from] saffron_rendering::Error),

    /// A calendar value could not be evaluated by the solar ephemeris.
    #[error("ephemeris error: {0}")]
    Ephemeris(#[from] solar_positioning::Error),

    /// A scene serde / ECS operation failed (the project's `scene` block load).
    #[error("scene error: {0}")]
    Scene(#[from] saffron_scene::Error),

    /// A shared coordinate or surface-field operation failed.
    #[error("spatial error: {0}")]
    Spatial(#[from] saffron_spatial::Error),

    /// A vegetation asset, point schema, identity, mutation, or package operation failed.
    #[error("vegetation error: {0}")]
    Vegetation(#[from] saffron_vegetation::Error),

    /// Two different byte streams resolved to the same vegetation CAS path.
    #[error("vegetation artifact content-address collision at {path}")]
    VegetationArtifactCollision {
        /// Final cache path whose bytes disagreed.
        path: String,
    },

    /// A stored vegetation artifact does not match the hash encoded by its path.
    #[error("vegetation artifact hash mismatch at {path}")]
    VegetationArtifactHash {
        /// Corrupt cache path.
        path: String,
    },

    /// An artifact length cannot be represented by the public byte-count contract.
    #[error("vegetation artifact size exceeds the supported range")]
    VegetationArtifactSize,

    /// A manifest plant row declares tags that differ from its compiled plant artifact.
    #[error("vegetation manifest tags disagree with compiled plant family {family}")]
    VegetationPlantTagMismatch {
        /// Plant-family catalog identity.
        family: u64,
    },

    /// A plant family failed source resolution or normalization and cannot be published.
    #[error("plant family {family} failed validation")]
    PlantCompilationRejected {
        /// Plant-family catalog identity.
        family: u64,
    },

    /// Another vegetation generation advanced the current root before this cook committed.
    #[error(
        "vegetation generation for map {map} was superseded (expected {expected}, current {current})"
    )]
    VegetationGenerationSuperseded {
        /// Vegetation-map identity.
        map: u64,
        /// Root identity captured when the cook began.
        expected: String,
        /// Root identity observed under the publication lock.
        current: String,
    },

    /// An exact authored file changed while a background vegetation cook was staged.
    #[error("vegetation cook input changed while cooking: {path}")]
    VegetationCookInputChanged {
        /// Canonical source path whose bytes no longer match the staged input.
        path: String,
    },

    /// A durable source/root transaction observed an unrelated generation root.
    #[error("vegetation transaction for map {map} conflicts with visible generation {current}")]
    VegetationTransactionConflict {
        /// Vegetation-map identity.
        map: u64,
        /// Unexpected visible generation identity.
        current: String,
    },

    /// An authored map transaction was prepared against a different visible root generation.
    #[error("vegetation-map generation conflict: expected {expected}, found {actual}")]
    VegetationMapGenerationConflict {
        /// Generation captured when the transaction was prepared.
        expected: u64,
        /// Generation visible after acquiring the map lock.
        actual: u64,
    },

    /// Two different authored object streams resolved to one immutable map-object path.
    #[error("vegetation-map object content-address collision at {path}")]
    VegetationMapObjectCollision {
        /// Final object path whose bytes disagreed.
        path: String,
    },

    /// A runtime `slangc` invocation for a material graph exited non-zero or
    /// produced no `.spv`. The payload names the material / shader.
    #[error("slangc failed for {0}")]
    SlangcFailed(String),

    /// A `project.json` declared a version this build does not accept.
    #[error("unsupported project version {found} (expected {expected})")]
    BadProjectVersion {
        /// The version the document declared.
        found: i64,
        /// The version this build accepts.
        expected: i64,
    },

    /// A native authored asset declared a version this build does not accept.
    #[error("unsupported {format} version {found} (expected {expected})")]
    BadAssetVersion {
        /// Native asset format name.
        format: &'static str,
        /// Version found in the document.
        found: i64,
        /// Only accepted version.
        expected: i64,
    },

    /// An id referenced by a load/resolve path is not present in the catalog.
    #[error("asset {0} not in catalog")]
    NotInCatalog(u64),

    /// A catalog entry exists for the id but carries the wrong [`AssetType`] for
    /// the requested operation.
    ///
    /// [`AssetType`]: saffron_scene::AssetType
    #[error("asset {id} is the wrong type (wanted {wanted})")]
    WrongAssetType {
        /// The asset id.
        id: u64,
        /// A short noun naming the expected type.
        wanted: &'static str,
    },

    /// A container was opened but does not carry the requested sub-asset chunk.
    #[error("container {container} has no sub-asset {sub}")]
    ContainerMissingSubAsset {
        /// The owning container's id.
        container: u64,
        /// The sub-asset id that was missing.
        sub: u64,
    },

    /// A project name failed validation (empty, or illegal path characters).
    #[error("invalid project name: {0}")]
    InvalidProjectName(String),

    /// A thumbnail could not be generated: the asset has no thumbnail, its bytes failed
    /// to load/decode, or the renderer's render/encode failed. The payload is the cause.
    #[error("thumbnail error: {0}")]
    Thumbnail(String),

    /// A creative LUT could not be parsed (a malformed `.cube` / `.slut`). The payload is the cause.
    #[error("lut error: {0}")]
    Lut(String),
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
