//! Canonical content identities and dependency records for vegetation cooking.

mod address;
mod content_hash;
mod dependency;
mod graph;
mod profile;
#[cfg(test)]
mod tests;

pub use address::{CookDependencyAddress, CookNodeAddress};
pub use content_hash::ContentHash;
pub(crate) use dependency::canonical_dependencies;
pub use dependency::{CookDependency, CookWorkActual, CookWorkEstimate};
pub use graph::{CookGraph, CookNodeRecord};
pub use profile::{CookPlatformProfile, CookVersionSet};
