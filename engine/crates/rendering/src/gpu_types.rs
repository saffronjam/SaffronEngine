//! The std430 GPU-upload structs and the übershader material selector.
//!
//! Every struct here is a GPU-layout type the Slang shaders read by raw bytes:
//! [`InstanceData`], [`MaterialParamsData`], and [`GpuLight`]. Each is
//! `#[repr(C)]` + [`bytemuck::Pod`] / [`bytemuck::Zeroable`] with a pinned
//! `const _: () = assert!(size_of == N)`, and the field byte offsets are checked in
//! the tests (README §3: a wrong offset corrupts the per-frame material dedup that
//! hashes [`MaterialParamsData`] by raw bytes — not just a pixel).
//!
//! The glam types come through `saffron_geometry::glam` (the engine's one pinned
//! math vocabulary) so a glam version split cannot silently change a stride. `Vec4`
//! / `Mat4` / `UVec4` are all 16-byte aligned, which is exactly the std430 vec4/mat4
//! alignment, so a `#[repr(C)]` field sequence of them lays out with no implicit
//! padding.

use saffron_geometry::glam::{Mat4, UVec4, Vec3, Vec4};

/// The übershader variant selector for a renderable.
///
/// One übershader backs every renderable, and `unlit` selects a distinct cached PSO
/// permutation (the unlit fragment branch, baked as a specialization constant).
/// `shader` names the SPIR-V the PSO compiles — `shaders/mesh.spv` for the default
/// übershader; a node-graph material names its codegen'd `.spv`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Material {
    /// The shader the PSO compiles, relative to the runtime shader dir (or an
    /// absolute path for a codegen'd per-material shader). Defaults to the
    /// übershader, `shaders/mesh.spv`.
    pub shader: String,
    /// Selects the unlit übershader permutation — a distinct cached PSO.
    pub unlit: bool,
    /// Selects the translucent (alpha-blended) PSO permutation — blend enabled, depth-write
    /// off. A distinct cached PSO; the item is then recorded in the sorted translucent pass.
    pub blend: bool,
    /// Alpha-tested (glTF `MASK`). Under MSAA this enables alpha-to-coverage (a distinct PSO
    /// permutation) for anti-aliased cutout edges; at 1× it is a plain runtime discard on the
    /// opaque PSO, so it does not split the cache.
    pub masked: bool,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            shader: "shaders/mesh.spv".to_string(),
            unlit: false,
            blend: false,
            masked: false,
        }
    }
}

/// One entry per drawn entity in the per-frame instance storage buffer (set 2,
/// binding 0). The vertex shader indexes it by `InstanceIndex`. std430-compatible:
/// every member is 16-byte aligned.
///
/// Eight 16-byte blocks: three `Mat4` (model, normal matrix, prev-frame model) then
/// five `vec4`-class fields.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceData {
    /// The world matrix.
    pub model: Mat4,
    /// `transpose(inverse(mat3(model)))` — correct normals under non-uniform scale.
    pub normal_matrix: Mat4,
    /// Last frame's world matrix (TAA object-motion reprojection).
    pub prev_model: Mat4,
    /// The base color (RGBA).
    pub base_color: Vec4,
    /// `.x` albedo bindless index, `.y` joint-palette offset, `.z` metallic-roughness
    /// bindless index, `.w` material-params index.
    pub texture: UVec4,
    /// `.x` metallic, `.y` roughness (the rest reserved).
    pub pbr: Vec4,
    /// RGB emissive radiance (strength baked in).
    pub emissive: Vec4,
}

const _: () = assert!(
    size_of::<InstanceData>() == 256,
    "InstanceData must match the std430 shader layout (8x 16-byte blocks)"
);

impl Default for InstanceData {
    fn default() -> Self {
        Self {
            model: Mat4::IDENTITY,
            normal_matrix: Mat4::IDENTITY,
            prev_model: Mat4::IDENTITY,
            base_color: Vec4::ONE,
            texture: UVec4::ZERO,
            pbr: Vec4::new(0.0, 1.0, 0.0, 0.0),
            emissive: Vec4::ZERO,
        }
    }
}

/// One static instance's sparse signed distance field (SDST v3), for the lighting
/// cone-trace's per-instance iteration. For each instance whose world AABB a cone sample is
/// near, the shader maps the world sample into the local fine-voxel grid, resolves the
/// owning 8³ brick through the bindless indirection volume at `params.x`, and takes a
/// cone-footprint mip-selected tap in the bindless 3-mip brick atlas at the same slot — or,
/// for an empty brick, reads the open distance from the coarse coverage volume at that slot.
///
/// Twelve 16-byte blocks: one `Mat4` (world→local) then four `vec4` (world/local AABB), the
/// `params` vec4, and three `uvec4` brick locators (fine dims, indirection dims, atlas
/// tiling). The world AABB culls a cone sample before the lookup; the local AABB + voxel
/// dims map the local sample to a continuous fine-voxel coordinate.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SdfInstance {
    /// The inverse world matrix (world→local), mapping a world sample into the field's
    /// local grid space.
    pub world_to_local: Mat4,
    /// The instance's world-space AABB minimum (`xyz`; `w` = base-color **red**) — the cone-march
    /// culls a sample against `xyz` before transforming into local space. The `.w` carries the
    /// per-instance base color (with `world_max.w` = green, `local_min.w` = blue) for the GDF lite
    /// albedo cache the DDGI trace reads; the brick sample touches only `.xyz`.
    pub world_min: Vec4,
    /// The instance's world-space AABB maximum (`xyz`; `w` = base-color **green**).
    pub world_max: Vec4,
    /// The SDF grid's padded lower corner in local (rest) space (`xyz`; `w` = base-color **blue**) —
    /// the local sample maps to a fine-voxel coordinate via `local_min.xyz` + the voxel dims.
    pub local_min: Vec4,
    /// The SDF grid's padded upper corner in local space (`xyz`; `w` reserved).
    pub local_max: Vec4,
    /// `.x` the shared bindless SDF slot (atlas binding 1 + indirection binding 2 + coverage
    /// binding 3) as a float, `.y` the `R16_SNORM` encode clamp (`max_dist`, local units),
    /// `.z` the instance's world scale (local→world distance factor); `.w` the atlas mip
    /// count (read as `mipCount` in `sdf.slang` for the cone-footprint mip select).
    pub params: Vec4,
    /// `.xyz` the fine voxel dims; `.w` reserved.
    pub voxel_dims: UVec4,
    /// `.xyz` the brick indirection-volume dims (bricks per axis); `.w` reserved.
    pub indir_dims: UVec4,
    /// `.xyz` the atlas tiling (occupied bricks per axis in the atlas image); `.w` reserved.
    pub atlas_bricks: UVec4,
}

const _: () = assert!(
    size_of::<SdfInstance>() == 192,
    "SdfInstance must match the std430 shader layout (12x 16-byte blocks)"
);

impl Default for SdfInstance {
    fn default() -> Self {
        Self {
            world_to_local: Mat4::IDENTITY,
            world_min: Vec4::ZERO,
            world_max: Vec4::ZERO,
            local_min: Vec4::ZERO,
            local_max: Vec4::ZERO,
            params: Vec4::ZERO,
            voxel_dims: UVec4::ZERO,
            indir_dims: UVec4::ZERO,
            atlas_bricks: UVec4::ZERO,
        }
    }
}

/// Per-distinct-material data (set 2, binding 2), indexed by `InstanceData.texture.w`.
/// Many instances of one material share one entry (deduplicated per frame by hashing
/// the raw bytes — so the layout below is load-bearing past correctness).
///
/// Six 16-byte blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialParamsData {
    /// Base color (RGBA).
    pub base_color: Vec4,
    /// `metallic, roughness, normalStrength, alphaCutoff`.
    pub pbr: Vec4,
    /// `rgb` emissive radiance, `w` height scale.
    pub emissive: Vec4,
    /// `tiling.xy, offset.xy`.
    pub uv: Vec4,
    /// Bindless indices: `albedo, orm/mr, normal, emissive`.
    pub tex0: UVec4,
    /// `height, reserved, reserved, featureBits`.
    pub tex1: UVec4,
}

const _: () = assert!(
    size_of::<MaterialParamsData>() == 96,
    "MaterialParamsData must match the std430 shader layout (6x 16-byte blocks)"
);

// The per-frame dedup key is the struct's raw bytes. `glam::Vec4` is float-backed,
// so it implements neither `Eq` nor `Hash`;
// instead `Eq`/`Hash` here are defined over `bytemuck::bytes_of` — byte-exact, and
// the struct has no padding (every field is a 16-byte block) so two values are equal
// exactly when their factors + indices are bit-identical.
impl PartialEq for MaterialParamsData {
    fn eq(&self, other: &Self) -> bool {
        bytemuck::bytes_of(self) == bytemuck::bytes_of(other)
    }
}

impl Eq for MaterialParamsData {}

impl std::hash::Hash for MaterialParamsData {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        bytemuck::bytes_of(self).hash(state);
    }
}

impl Default for MaterialParamsData {
    fn default() -> Self {
        Self {
            base_color: Vec4::ONE,
            pbr: Vec4::new(0.0, 1.0, 1.0, 0.5),
            emissive: Vec4::ZERO,
            uv: Vec4::new(1.0, 1.0, 0.0, 0.0),
            tex0: UVec4::ZERO,
            tex1: UVec4::ZERO,
        }
    }
}

/// One punctual (point or spot) light in the per-frame light storage buffer (set 1,
/// binding 1). Positions/directions are world space. std430-compatible — four `vec4`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuLight {
    /// `xyz` world position, `w` range.
    pub position_range: Vec4,
    /// `rgb` color, `a` intensity.
    pub color_intensity: Vec4,
    /// `xyz` world direction (spot), `w` type (0 = point, 1 = spot).
    pub direction_type: Vec4,
    /// `x` cos(inner angle), `y` cos(outer angle), `z` volumetric-scattering multiplier (per-light
    /// fog in-scatter), `w` cast-volumetric-shadow gate (`0`/`1`).
    pub spot_cos: Vec4,
}

const _: () = assert!(
    size_of::<GpuLight>() == 64,
    "GpuLight must match the std430 shader layout (4x vec4)"
);

impl MaterialParamsData {
    /// Packs PBR factors + bindless texture indices into the std430 record. The
    /// per-frame dedup hashes the result, so two instances with identical factors +
    /// textures collapse to one entry.
    #[allow(clippy::too_many_arguments)]
    pub fn from_factors(
        base_color: Vec4,
        metallic: f32,
        roughness: f32,
        normal_strength: f32,
        alpha_cutoff: f32,
        emissive: Vec3,
        height_scale: f32,
        uv_tiling: [f32; 2],
        uv_offset: [f32; 2],
        tex0: UVec4,
        tex1: UVec4,
    ) -> Self {
        Self {
            base_color,
            pbr: Vec4::new(metallic, roughness, normal_strength, alpha_cutoff),
            emissive: emissive.extend(height_scale),
            uv: Vec4::new(uv_tiling[0], uv_tiling[1], uv_offset[0], uv_offset[1]),
            tex0,
            tex1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    /// `MaterialParamsData` is exactly 96 bytes with each field at the std430 offset
    /// the Slang shader reads — the contract the per-frame material dedup hashes by
    /// raw bytes (README §3). The phase's named layout gate.
    #[test]
    fn material_params_data_byte_layout_matches_std430() {
        assert_eq!(size_of::<MaterialParamsData>(), 96);
        assert_eq!(align_of::<MaterialParamsData>(), 16);
        assert_eq!(offset_of!(MaterialParamsData, base_color), 0);
        assert_eq!(offset_of!(MaterialParamsData, pbr), 16);
        assert_eq!(offset_of!(MaterialParamsData, emissive), 32);
        assert_eq!(offset_of!(MaterialParamsData, uv), 48);
        assert_eq!(offset_of!(MaterialParamsData, tex0), 64);
        assert_eq!(offset_of!(MaterialParamsData, tex1), 80);
    }

    /// `InstanceData` is exactly 256 bytes with each field at the std430 offset the
    /// vertex shader indexes — three mat4 then five vec4-class blocks, no padding.
    #[test]
    fn instance_data_byte_layout_matches_std430() {
        assert_eq!(size_of::<InstanceData>(), 256);
        assert_eq!(align_of::<InstanceData>(), 16);
        assert_eq!(offset_of!(InstanceData, model), 0);
        assert_eq!(offset_of!(InstanceData, normal_matrix), 64);
        assert_eq!(offset_of!(InstanceData, prev_model), 128);
        assert_eq!(offset_of!(InstanceData, base_color), 192);
        assert_eq!(offset_of!(InstanceData, texture), 208);
        assert_eq!(offset_of!(InstanceData, pbr), 224);
        assert_eq!(offset_of!(InstanceData, emissive), 240);
    }

    /// `GpuLight` is exactly 64 bytes — four contiguous vec4, no padding.
    #[test]
    fn gpu_light_byte_layout_matches_std430() {
        assert_eq!(size_of::<GpuLight>(), 64);
        assert_eq!(align_of::<GpuLight>(), 16);
        assert_eq!(offset_of!(GpuLight, position_range), 0);
        assert_eq!(offset_of!(GpuLight, color_intensity), 16);
        assert_eq!(offset_of!(GpuLight, direction_type), 32);
        assert_eq!(offset_of!(GpuLight, spot_cos), 48);
    }

    /// Two material-params records with identical factors hash and compare equal —
    /// the per-frame dedup key. A differing texture index makes them distinct.
    #[test]
    fn material_params_dedup_key_is_byte_exact() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |m: &MaterialParamsData| {
            let mut h = DefaultHasher::new();
            m.hash(&mut h);
            h.finish()
        };

        let a = MaterialParamsData::from_factors(
            Vec4::new(1.0, 0.5, 0.25, 1.0),
            0.2,
            0.7,
            1.0,
            0.5,
            Vec3::ZERO,
            0.05,
            [1.0, 1.0],
            [0.0, 0.0],
            UVec4::new(3, 0, 0, 0),
            UVec4::ZERO,
        );
        let b = a;
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b), "identical factors dedup to one entry");

        let mut c = a;
        c.tex0.x = 4;
        assert_ne!(a, c, "a differing bindless index is a distinct material");
        assert_ne!(hash(&a), hash(&c));
    }

    /// The default `Material` is the lit übershader; toggling `unlit` produces a
    /// distinct value (the PSO-cache key discriminator).
    #[test]
    fn material_default_is_lit_ubershader() {
        let m = Material::default();
        assert_eq!(m.shader, "shaders/mesh.spv");
        assert!(!m.unlit);

        let unlit = Material {
            unlit: true,
            ..Material::default()
        };
        assert_ne!(m, unlit);

        // The blend permutation is a distinct PSO key too.
        let blend = Material {
            blend: true,
            ..Material::default()
        };
        assert!(!m.blend);
        assert_ne!(m, blend);
    }
}
