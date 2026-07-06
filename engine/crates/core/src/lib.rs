//! Saffron Anima foundation primitives: the `Result`/`Error` model, `Uuid`, the
//! `Ref = Arc` policy, base64, and time/identity types.
//!
//! DAG root — depends on no other Saffron crate. Logging lives in `saffron-log`.

#![deny(unsafe_code)]

mod base64;
mod blend;
mod error;
mod height;
mod time;
mod uuid;

use std::sync::Arc;

pub use base64::base64_encode;
pub use blend::BlendMode;
pub use error::{Error, Result};
pub use height::HeightMode;
pub use time::TimeSpan;
pub use uuid::Uuid;

/// A shared, read-only reference to a logical resource.
///
/// This is the read-shared default of the ownership policy: a value fully
/// constructed and then only read through every shared handle (loaded assets,
/// meshes, materials). It is a *readability* alias only — a shared-*mutable*
/// site does not use `Ref`; it spells `Arc<Mutex<T>>` (or `Arc<RwLock<T>>`)
/// explicitly at its declaration, so the exception is visible where it occurs.
pub type Ref<T> = Arc<T>;

/// The engine product name.
pub const ENGINE_NAME: &str = "Saffron Anima";

/// The engine version string.
pub const ENGINE_VERSION: &str = "0.1.0-vulkan";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_identity_strings() {
        assert_eq!(ENGINE_NAME, "Saffron Anima");
        assert_eq!(ENGINE_VERSION, "0.1.0-vulkan");
    }

    #[test]
    fn ref_is_shared_read() {
        let a: Ref<u32> = Ref::new(7);
        let b = Ref::clone(&a);
        assert_eq!(*a, 7);
        assert_eq!(*b, 7);
        assert_eq!(Arc::strong_count(&a), 2);
    }
}
