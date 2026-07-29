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

/// Capabilities the Slang shaders use that the `glsl_450` profile does not imply: bindless
/// non-uniform indexing, sparse residency + min-LOD sampling, fragment-fully-covered, inline ray
/// query, the `VK_EXT_mesh_shader` stages, and the SPIR-V debug-info extensions.
///
/// Declared explicitly so Slang does not implicitly upgrade the profile, and shared so the
/// offline compiler (`xtask shaders`) and the runtime material compiler cannot drift apart —
/// they did, and a module needing `SPV_EXT_mesh_shader` was rejected for targeting a SPIR-V
/// version the missing capability would have raised.
pub const SLANGC_CAPABILITIES: &str = "SPV_KHR_non_semantic_info+SPV_GOOGLE_user_type+spvSparseResidency+spvMinLod+spvFragmentFullyCoveredEXT+spvShaderNonUniformEXT+spvRayQueryKHR+spvMeshShadingEXT+spvGroupNonUniform+spvGroupNonUniformBallot";

/// The `slangc` flag vector every SPIR-V compile shares, offline and at runtime.
pub const SLANGC_SPV_FLAGS: &[&str] = &[
    "-profile",
    "glsl_450",
    "-target",
    "spirv",
    "-emit-spirv-directly",
    "-fvk-use-entrypoint-name",
    "-matrix-layout-column-major",
    "-capability",
    SLANGC_CAPABILITIES,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_identity_strings() {
        assert_eq!(ENGINE_NAME, "Saffron Anima");
        assert_eq!(ENGINE_VERSION, "0.1.0-vulkan");
    }

    #[test]
    fn slangc_flags_declare_every_capability_the_shaders_use() {
        // The flag vector must carry the capability atoms, not merely define them: a compile
        // missing `spvMeshShadingEXT` silently emits a SPIR-V version too old for a mesh entry.
        assert!(SLANGC_SPV_FLAGS.contains(&"-capability"));
        assert!(SLANGC_SPV_FLAGS.contains(&SLANGC_CAPABILITIES));
        for atom in [
            "spvMeshShadingEXT",
            "spvRayQueryKHR",
            "spvShaderNonUniformEXT",
        ] {
            assert!(
                SLANGC_CAPABILITIES.contains(atom),
                "{atom} missing from the shared capability set"
            );
        }
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
