//! The crate-root error type and `Result` alias.

/// The foundation error type downstream crates compose against with `#[from]`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A failure whose underlying cause genuinely has no further structure.
    #[error("{0}")]
    Message(String),
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = core::result::Result<T, Error>;
