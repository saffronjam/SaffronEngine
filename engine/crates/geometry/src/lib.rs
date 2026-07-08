//! glam math plus the CPU mesh/skin/vertex/ray types, the animation track/clip
//! types, and the import-graph aggregates.
//!
//! This crate adopts `glam` as the engine's math vocabulary with three locked
//! decisions that cascade through every downstream crate:
//!
//! - **`Vec3` is 12 bytes, never `Vec3A`.** All format-bearing fields use
//!   `Vec3`/`Vec4`/`Vec2`/`Mat4`, never the 16-byte SIMD `A` variants, so the
//!   byte strides match the frozen disk/GPU layouts (`Vertex` = 32, etc.).
//! - **Quaternion order is `xyzw`.** glam's `Quat` is the glTF storage order:
//!   [`ImportedNode::rotation`] reads the four glTF floats in declaration order.
//! - **No global depth flag.** The `[0, 1]` Vulkan clip depth is a per-projection
//!   choice (`Mat4::perspective_rh`). No projection lives in this crate, so the rule
//!   is recorded for downstream crates and not exercised here.
//!
//! Beyond the types it carries the byte formats (`.smesh`/`.sanim`), the picking
//! math, the model importers (glTF via the `gltf` crate, OBJ via `tobj`), the raster
//! image decoders (the `image` crate), and the stable sub-asset id hash.
//!
//! Depends on `saffron-core`.

#![deny(unsafe_code)]

mod conditioning;
mod error;
mod gltf_import;
mod image_decode;
mod meshlet;
mod obj_import;
mod picking;
mod primitives;
mod sanim;
mod sdf;
mod smesh;
mod smodel;
mod sub_id;
mod translate;
mod types;

pub use conditioning::{
    EDGE_BOUNDARY, EDGE_NON_MANIFOLD, EDGE_SEAM, EDGE_SEAM_OBJECT_SPACE, Edge, MeshConditioning,
    MinMaxLevel, TriEdges, WeldedVertex, build_min_max_pyramid,
};
pub use error::{Error, Result};
pub use gltf_import::import_gltf_model;
pub use image_decode::{
    decode_image, decode_image_from_memory, decode_image_from_memory_hdr, decode_image_hdr,
};
pub use meshlet::{
    MESHLET_MAX_TRIANGLES, MESHLET_MAX_VERTICES, Meshlet, MeshletSet, build_meshlets,
};
pub use obj_import::import_obj_model;
pub use picking::{
    MeshBvh, closest_point_on_triangle, generate_normals, ray_aabb_slab, ray_triangle,
    world_aabb_from_corners,
};
pub use primitives::{cube, plane, uv_sphere};
pub use sanim::{
    ANIM_FORMAT_VERSION, load_animation, load_animation_from_bytes, save_animation,
    save_animation_to_buffer,
};
pub use sdf::{
    GridDesc, SDF_BRICK_SIZE, SDF_BRICK_USEFUL, SDF_CHUNK_CORE_VOXELS, SDF_EMPTY_BRICK,
    SDF_FORMAT_VERSION, SDF_MAX_GRID_AXIS, SDF_MIN_GRID_AXIS, SDF_MIP_COUNT, SDF_VOXELS_PER_METRE,
    Sdf, SdfHeader, bake_grid, sdf_chunk_cores, sdf_set_from_bytes, sdf_set_to_bytes,
};
pub use smesh::{
    MESH_FORMAT_VERSION, load_mesh, load_mesh_conditioning_from_bytes, load_mesh_from_bytes,
    load_mesh_morph_from_bytes, load_mesh_skin, load_mesh_skin_from_bytes, mesh_counts_from_bytes,
    mesh_file_counts, save_mesh, save_mesh_to_buffer,
};
pub use smodel::{
    CONTAINER_FORMAT_VERSION, ChunkKind, ContainerChunk, ContainerReader, METADATA_SCHEMA_VERSION,
    SModelHeader, TocEntry, read_container, read_container_header, write_container,
};
pub use sub_id::sub_id_for;
pub use translate::translate_model;
pub use types::{
    AlphaMode, AnimClip, AnimInterp, AnimPath, AnimTarget, AnimTrack, DecodedImage,
    DecodedImageFloat, ImportedMaterial, ImportedModel, ImportedNode, ImportedSkin,
    MaterialMapRole, Mesh, MeshCounts, MorphData, MorphDelta, MorphTarget, Ray, SkinPayload,
    Submesh, TextureSource, Vertex, VertexSkin, compute_tangents,
};

// Re-export glam so downstream crates share this crate's pinned math vocabulary
// rather than depending on glam directly and risking a version split.
pub use glam;

/// The format-bearing strides, pinned at compile time. A stray `Vec3A` or a glam
/// bump that changed a layout fails the build here, not at a torn-mesh runtime.
const _: () = assert!(size_of::<Vertex>() == 48, "Vertex must stay 48 bytes");
const _: () = assert!(size_of::<Submesh>() == 16, "Submesh must stay 16 bytes");
const _: () = assert!(
    size_of::<VertexSkin>() == 24,
    "VertexSkin must stay 24 bytes"
);

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Vec2, Vec3, Vec4};

    /// A `const fn` that only compiles for a `Pod` type — proves the derive held
    /// for each format struct without naming `unsafe`.
    const fn assert_pod<T: bytemuck::Pod>() {}

    #[test]
    fn format_strides_are_pinned() {
        assert_eq!(size_of::<Vertex>(), 48);
        assert_eq!(size_of::<Submesh>(), 16);
        assert_eq!(size_of::<VertexSkin>(), 24);
    }

    #[test]
    fn format_structs_are_pod() {
        assert_pod::<Vertex>();
        assert_pod::<Submesh>();
        assert_pod::<VertexSkin>();
    }

    #[test]
    fn vertex_round_trips_through_bytes() {
        let vertex = Vertex {
            position: Vec3::new(1.0, 2.0, 3.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            uv0: Vec2::new(0.25, 0.75),
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(bytes.len(), 48);
        let back: &Vertex = bytemuck::from_bytes(bytes);
        assert_eq!(*back, vertex);
    }

    #[test]
    fn submesh_round_trips_through_bytes() {
        let submesh = Submesh {
            first_index: 6,
            index_count: 36,
            vertex_offset: -4,
            material_slot: 2,
        };
        let bytes = bytemuck::bytes_of(&submesh);
        assert_eq!(bytes.len(), 16);
        let back: &Submesh = bytemuck::from_bytes(bytes);
        assert_eq!(*back, submesh);
    }

    #[test]
    fn vertex_skin_round_trips_through_bytes() {
        let skin = VertexSkin {
            joints: [0, 1, 2, 3],
            weights: [0.5, 0.25, 0.15, 0.10],
        };
        let bytes = bytemuck::bytes_of(&skin);
        assert_eq!(bytes.len(), 24);
        let back: &VertexSkin = bytemuck::from_bytes(bytes);
        assert_eq!(*back, skin);
    }

    #[test]
    fn slice_cast_preserves_count_and_order() {
        // A Vec of format structs casts to bytes and back as a slice — the wire the
        // .smesh writer/reader uses (proves cast_slice is wired, no unsafe).
        let vertices = vec![
            Vertex {
                position: Vec3::X,
                normal: Vec3::Y,
                uv0: Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Z,
                normal: Vec3::X,
                uv0: Vec2::ONE,
                ..Vertex::default()
            },
        ];
        let bytes: &[u8] = bytemuck::cast_slice(&vertices);
        assert_eq!(bytes.len(), 2 * 48);
        let back: &[Vertex] = bytemuck::cast_slice(bytes);
        assert_eq!(back, vertices.as_slice());
    }

    #[test]
    fn anim_byte_discriminants_are_pinned() {
        // The on-disk byte values must stay fixed (the .sanim record stores them raw).
        assert_eq!(AnimPath::Translation as u8, 0);
        assert_eq!(AnimPath::Rotation as u8, 1);
        assert_eq!(AnimPath::Scale as u8, 2);
        assert_eq!(AnimPath::Weights as u8, 3);
        assert_eq!(AnimInterp::Step as u8, 0);
        assert_eq!(AnimInterp::Linear as u8, 1);
        assert_eq!(AnimInterp::CubicSpline as u8, 2);
        assert_eq!(AnimTarget::Bone as u8, 0);
        assert_eq!(AnimTarget::Node as u8, 1);
        // The discriminant round-trips through `from_u8`; out-of-range is rejected.
        assert_eq!(AnimPath::from_u8(3).unwrap(), AnimPath::Weights);
        assert_eq!(AnimTarget::from_u8(1).unwrap(), AnimTarget::Node);
        assert!(AnimPath::from_u8(4).is_err());
        assert!(AnimTarget::from_u8(2).is_err());
    }

    #[test]
    fn defaults_match_the_cpp_seed_values() {
        // ImportedNode: parent -1 (root), identity rotation, unit scale.
        let node = ImportedNode::default();
        assert_eq!(node.parent, -1);
        assert_eq!(node.rotation, glam::Quat::IDENTITY);
        assert_eq!(node.scale, Vec3::ONE);

        // ImportedMaterial: white base color, dielectric, fully rough.
        let material = ImportedMaterial::default();
        assert_eq!(material.base_color, Vec4::ONE);
        assert_eq!(material.metallic, 0.0);
        assert_eq!(material.roughness, 1.0);
        assert!(material.albedo.is_none());

        // Ray: forward -Z, origin at the world center.
        let ray = Ray::default();
        assert_eq!(ray.origin, Vec3::ZERO);
        assert_eq!(ray.dir, Vec3::new(0.0, 0.0, -1.0));
    }

    #[test]
    fn quat_reads_gltf_xyzw_in_declaration_order() {
        // glam's xyzw is the glTF storage order, so the four source floats map
        // straight through.
        let r = [0.1f32, 0.2, 0.3, 0.9];
        let q = glam::Quat::from_xyzw(r[0], r[1], r[2], r[3]);
        assert_eq!(q.x, r[0]);
        assert_eq!(q.y, r[1]);
        assert_eq!(q.z, r[2]);
        assert_eq!(q.w, r[3]);
    }
}
