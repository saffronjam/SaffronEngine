//! Saffron Anima foundation primitives: the `Result`/`Error` model, `Uuid`, the
//! `Ref = Arc` policy, base64, and time/identity types.

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
/// A shared-*mutable* site does not use `Ref`; it spells `Arc<Mutex<T>>` (or
/// `Arc<RwLock<T>>`) explicitly, so the exception is visible where it occurs.
pub type Ref<T> = Arc<T>;

/// The engine product name.
pub const ENGINE_NAME: &str = "Saffron Anima";

/// The engine version string.
pub const ENGINE_VERSION: &str = "0.1.0-vulkan";

/// Capabilities the Slang shaders use that the `glsl_450` profile does not imply: bindless
/// non-uniform indexing, sparse residency + min-LOD sampling, fragment-fully-covered, inline ray
/// query, the `VK_EXT_mesh_shader` stages, and the SPIR-V debug-info extensions.
///
/// Declaring them explicitly keeps Slang from implicitly upgrading the profile, and sharing the
/// set keeps the offline compiler (`xtask shaders`) and the runtime material compiler in step.
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
        // A compile missing `spvMeshShadingEXT` silently emits a SPIR-V version too old for a
        // mesh entry, so the flag vector must carry the atoms and not merely define them.
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
