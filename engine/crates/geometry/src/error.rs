//! The crate-root error type and `Result` alias.

/// Errors raised by the geometry formats, importers, and decoders.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem read or write failed. The payload is the OS message.
    #[error("io error: {0}")]
    Io(String),
    /// A format header carried the wrong four-byte magic tag.
    #[error("bad magic")]
    BadMagic,
    /// A format header declared a version this build does not accept.
    #[error("unsupported version {0}")]
    UnsupportedVersion(u32),
    /// A byte span ended before a header or section it was required to contain.
    #[error("truncated input")]
    Truncated,
    /// A header's stored offsets or counts did not agree with the recomputed layout.
    #[error("bad layout")]
    BadLayout,
    /// A skinned `.smesh` encode was given a skin stream that does not parallel the
    /// vertices one-for-one.
    #[error("skin stream ({skin}) does not parallel the vertices ({vertices})")]
    SkinLengthMismatch {
        /// The provided skin stream length.
        skin: usize,
        /// The mesh's vertex count.
        vertices: usize,
    },
    /// An image or accessor could not be decoded. The payload is the decoder's message.
    #[error("decode error: {0}")]
    Decode(String),
    /// A model source could not be translated into the import graph. The payload is
    /// the importer's message.
    #[error("import error: {0}")]
    Import(String),
    /// A portable hierarchy count, offset, or byte length overflowed its canonical width.
    #[error("portable hierarchy numeric overflow")]
    NumericOverflow,
    /// A portable hierarchy field violated the canonical format contract.
    #[error("invalid {format} field '{field}'")]
    HierarchyFormat {
        /// Hierarchy section or aggregate format.
        format: &'static str,
        /// Invalid field path.
        field: String,
    },
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
