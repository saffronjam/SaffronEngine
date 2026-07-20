//! The crate-root error type and `Result` alias.

/// The scene crate error type.
///
/// The component-access surface returns `Result` where the ECS can fail (an
/// operation on a stale handle, a component query that finds nothing). The serde
/// path composes `saffron-json` errors into this enum with `#[from]`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operation referenced an entity handle the world no longer holds.
    #[error("entity handle is not valid in this scene")]
    InvalidEntity,
    /// A component read found no component of the requested type on the entity.
    #[error("entity has no component of the requested type")]
    MissingComponent,
    /// A second instance of a scene-singleton component was rejected.
    #[error("scene already has a {0} component")]
    SingletonComponent(&'static str),
    /// A reparent was rejected (invalid handle, self-parent, or a cycle).
    #[error("reparent rejected: {0}")]
    Reparent(String),
    /// A proposed component order did not match the entity's present component set
    /// (wrong length, a non-present name, or a duplicate).
    #[error("invalid component order: {0}")]
    ComponentOrder(String),
    /// A component body failed to deserialize, prefixed with the component name.
    #[error("deserialize failed: {0}")]
    Deserialize(String),
    /// A JSON shape was not what the serde path expected (e.g. components were not an
    /// object).
    #[error("invalid JSON: {0}")]
    Json(String),
    /// A scene document carried a version outside `[1, SceneVersion]`.
    #[error("unsupported scene version {0}")]
    UnsupportedVersion(i64),
    /// A scene document was malformed (a missing `entities` array, an entity entry that is
    /// not an object, an entity missing its `id`).
    #[error("malformed scene document: {0}")]
    Document(String),
    /// A `saffron-json` parse failure lifted into the scene error via `?`.
    #[error(transparent)]
    JsonGateway(#[from] saffron_json::Error),
    /// A file read/write failed, naming the path that could not be opened.
    #[error("scene file IO for '{path}': {source}")]
    Io {
        /// The path that could not be read or written.
        path: String,
        /// The underlying OS error.
        source: std::io::Error,
    },
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
