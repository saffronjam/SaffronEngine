//! The pure-value CPU types every downstream crate builds on: the mesh vocabulary,
//! the picking ray, the animation track/clip types, and the import-graph aggregates.
//!
//! The format-bearing structs (`Vertex`, `Submesh`, `VertexSkin`) are `#[repr(C)]`
//! Pod with byte strides pinned by [`super::tests`]. All format fields use glam's
//! 12-byte `Vec3` (never the 16-byte SIMD `Vec3A`), so the strides stay fixed.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};

use crate::error::{Error, Result};

/// One interleaved vertex stream entry: position, normal, the first UV set, and a
/// UV-aligned tangent.
///
/// Exactly 48 bytes — the `.smesh` on-disk vertex stride and the GPU vertex buffer layout.
/// `tangent` is `[f32; 4]` (raw, like [`VertexSkin::weights`]) so the struct stays 4-byte
/// aligned — `xyz` is the object-space tangent, `w` is the ±1 bitangent handedness (glTF
/// convention: `bitangent = w · cross(normal, tangent)`). The importers compute it (or read
/// the glTF `TANGENT` accessor) via [`compute_tangents`]; it lets a UV-space vector field —
/// a normal map's frame or vector displacement — be transformed by a true, UV-aligned TBN
/// rather than an arbitrary branchless basis.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// Object-space position.
    pub position: Vec3,
    /// Object-space normal.
    pub normal: Vec3,
    /// First UV set.
    pub uv0: Vec2,
    /// UV-aligned tangent: `xyz` object-space tangent, `w` the ±1 bitangent handedness.
    pub tangent: [f32; 4],
}

/// One `drawIndexed` range over the shared vertex+index buffers.
///
/// Exactly 16 bytes (baked directly into a `.smesh`). `vertex_offset` is signed to
/// match `vkCmdDrawIndexed`; `material_slot` indexes the model's material table.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct Submesh {
    /// First index of this range in the shared index buffer.
    pub first_index: u32,
    /// Number of indices in this range.
    pub index_count: u32,
    /// Value added to each index before vertex fetch (signed, per Vulkan).
    pub vertex_offset: i32,
    /// Index into the model's material table.
    pub material_slot: u32,
}

/// Per-vertex skin influences: a second stream parallel to [`Mesh::vertices`].
///
/// Exactly 24 bytes (the `.smesh` v2 skin stride and the GPU skin-stream stride).
/// Kept out of [`Vertex`] so the unskinned layout and the v1 `.smesh` stay intact;
/// an empty skin stream means the mesh is unskinned.
///
/// `weights` is a raw `[f32; 4]` rather than glam's `Vec4`: glam's `Vec4` is
/// 16-byte SIMD-aligned, which would pad this struct to 32 bytes and break the
/// stride — the same reason format-bearing 3-vectors use `Vec3`, not `Vec3A`.
/// The 24-byte stride is the load-bearing invariant; the array is its expression.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct VertexSkin {
    /// Indices into the skin's joint list.
    pub joints: [u16; 4],
    /// Normalized blend weights, one per joint.
    pub weights: [f32; 4],
}

/// The canonical CPU-side mesh every importer converts into.
///
/// Not `#[repr(C)]`: it is the in-memory aggregate the byte formats serialize *from*,
/// not a byte layout itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    /// The interleaved vertex stream.
    pub vertices: Vec<Vertex>,
    /// 32-bit triangle indices into [`Mesh::vertices`].
    pub indices: Vec<u32>,
    /// The draw ranges, each over a slice of the shared buffers.
    pub submeshes: Vec<Submesh>,
}

/// Computes per-vertex UV-aligned tangents (Lengyel's method) into [`Vertex::tangent`], with
/// the ±1 bitangent handedness in `w`. Accumulates each triangle's UV-gradient tangent/bitangent
/// into its vertices, then Gram-Schmidt-orthonormalizes the tangent against the normal. A
/// degenerate vertex (no usable UVs, a zero-area triangle) falls back to a stable branchless
/// basis from the normal so every tangent is finite and unit-length.
///
/// The importers call this after building positions/normals/uvs; the glTF importer instead keeps
/// a provided `TANGENT` accessor (already UV-aligned with handedness) and only computes when the
/// asset omits it.
pub fn compute_tangents(mesh: &mut Mesh) {
    let n = mesh.vertices.len();
    let mut tan = vec![Vec3::ZERO; n];
    let mut bit = vec![Vec3::ZERO; n];
    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if i0 >= n || i1 >= n || i2 >= n {
            continue;
        }
        let p0 = mesh.vertices[i0].position;
        let e1 = mesh.vertices[i1].position - p0;
        let e2 = mesh.vertices[i2].position - p0;
        let w0 = mesh.vertices[i0].uv0;
        let d1 = mesh.vertices[i1].uv0 - w0;
        let d2 = mesh.vertices[i2].uv0 - w0;
        let denom = d1.x * d2.y - d2.x * d1.y;
        if denom.abs() < 1e-12 {
            continue;
        }
        let f = 1.0 / denom;
        let t = (e1 * d2.y - e2 * d1.y) * f;
        let b = (e2 * d1.x - e1 * d2.x) * f;
        for &i in &[i0, i1, i2] {
            tan[i] += t;
            bit[i] += b;
        }
    }
    for (i, v) in mesh.vertices.iter_mut().enumerate() {
        let normal = v.normal.normalize_or_zero();
        // Gram-Schmidt: drop the normal component, then normalize.
        let mut tangent = (tan[i] - normal * normal.dot(tan[i])).normalize_or_zero();
        if tangent.length_squared() < 1e-8 || !tangent.is_finite() {
            // No usable UV gradient: any consistent perpendicular frame (Duff et al. 2017).
            let s = if normal.z >= 0.0 { 1.0 } else { -1.0 };
            let a = -1.0 / (s + normal.z);
            tangent = Vec3::new(
                1.0 + s * normal.x * normal.x * a,
                s * normal.x * normal.y * a,
                -s * normal.x,
            );
            tangent = tangent.normalize_or_zero();
        }
        let handed = if normal.cross(tangent).dot(bit[i]) < 0.0 {
            -1.0
        } else {
            1.0
        };
        v.tangent = [tangent.x, tangent.y, tangent.z, handed];
    }
}

/// One sparse per-vertex morph contribution: the position+normal delta applied at full
/// weight.
///
/// Exactly 28 bytes (`4 + 12 + 12`, no trailing pad): the leading `u32` keeps the two
/// 12-byte `Vec3`s 4-byte aligned, and the struct's own alignment is 4. The on-disk
/// `.smesh` morph stride and the GPU morph-delta stride. No tangent delta is stored — the
/// engine [`Vertex`] has no tangent stream, so the deform shader re-derives the tangent by
/// Gram-Schmidt against the morphed normal.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct MorphDelta {
    /// Index into the base vertex stream this delta applies to.
    pub vertex_index: u32,
    /// Position delta at weight 1.0.
    pub d_position: Vec3,
    /// Normal delta at weight 1.0.
    pub d_normal: Vec3,
}

const _: () = assert!(
    size_of::<MorphDelta>() == 28,
    "MorphDelta must be exactly 28 bytes"
);

/// One named morph target: its sparse deltas and its authored (rest) weight.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphTarget {
    /// The target name (glTF `mesh.extras.targetNames`, else synthesized `morph_{k}`).
    pub name: String,
    /// The authored rest weight (the mesh-level default for this target).
    pub rest_weight: f32,
    /// The sparse per-vertex deltas (only moved vertices).
    pub deltas: Vec<MorphDelta>,
}

/// All morph targets of one mesh, in channel order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphData {
    /// The morph targets, in glTF channel order (the weight-vector order).
    pub targets: Vec<MorphTarget>,
}

/// A world-space ray for picking and spatial queries.
///
/// `dir` is assumed unit-length; the caller normalizes before constructing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// Ray origin.
    pub origin: Vec3,
    /// Ray direction, caller-normalized to unit length.
    pub dir: Vec3,
}

impl Default for Ray {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            dir: Vec3::new(0.0, 0.0, -1.0),
        }
    }
}

/// Vertex/index totals read from a `.smesh` header without loading the data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshCounts {
    /// Number of vertices in the mesh.
    pub vertex_count: u32,
    /// Number of indices in the mesh.
    pub index_count: u32,
}

/// The channel an [`AnimTrack`] targets.
///
/// `#[repr(u8)]`: the discriminants are the pinned on-disk byte values
/// (`Translation = 0`, `Rotation = 1`, `Scale = 2`, `Weights = 3`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimPath {
    /// The target's translation.
    #[default]
    Translation = 0,
    /// The target's rotation.
    Rotation = 1,
    /// The target's scale.
    Scale = 2,
    /// Morph-target weights (N per keyframe; the count is the track's `morph_count`).
    Weights = 3,
}

impl AnimPath {
    /// Maps an on-disk discriminant byte back to the enum.
    ///
    /// An out-of-range byte is rejected with [`Error::BadLayout`] rather than
    /// transmuted, so a malformed `.sanim` track record can never produce UB
    /// (the crate's `#![deny(unsafe_code)]` holds).
    pub fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Translation),
            1 => Ok(Self::Rotation),
            2 => Ok(Self::Scale),
            3 => Ok(Self::Weights),
            _ => Err(Error::BadLayout),
        }
    }
}

/// What kind of thing an [`AnimTrack`] drives.
///
/// `#[repr(u8)]`: the discriminants are the pinned on-disk byte values
/// (`Bone = 0`, `Node = 1`). A morph-weight track is `Node` + [`AnimPath::Weights`];
/// there is no separate morph-weight target arm.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimTarget {
    /// A skinned-mesh bone, resolved to a bone index by name at import.
    #[default]
    Bone = 0,
    /// A plain scene-graph node, bound by durable name at runtime.
    Node = 1,
}

impl AnimTarget {
    /// Maps an on-disk discriminant byte back to the enum.
    ///
    /// An out-of-range byte is rejected with [`Error::BadLayout`] rather than
    /// transmuted, so a malformed `.sanim` track record can never produce UB
    /// (the crate's `#![deny(unsafe_code)]` holds).
    pub fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Bone),
            1 => Ok(Self::Node),
            _ => Err(Error::BadLayout),
        }
    }
}

/// The interpolation a sampler applies between keyframes.
///
/// `#[repr(u8)]`: the discriminants are the pinned on-disk byte values
/// (`Step = 0`, `Linear = 1`, `CubicSpline = 2`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimInterp {
    /// Hold the previous keyframe value (no interpolation).
    Step = 0,
    /// Linear interpolation between keyframes.
    #[default]
    Linear = 1,
    /// Cubic-spline interpolation; values store 3x (in-tangent, value, out-tangent).
    CubicSpline = 2,
}

impl AnimInterp {
    /// Maps an on-disk discriminant byte back to the enum.
    ///
    /// An out-of-range byte is rejected with [`Error::BadLayout`] rather than
    /// transmuted, so a malformed `.sanim` track record can never produce UB
    /// (the crate's `#![deny(unsafe_code)]` holds).
    pub fn from_u8(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Step),
            1 => Ok(Self::Linear),
            2 => Ok(Self::CubicSpline),
            _ => Err(Error::BadLayout),
        }
    }
}

/// One animated channel: a sampled curve targeting a bone's, a node's, or a node's
/// morph weights.
///
/// A faithful, lossless mirror of a glTF animation channel + sampler, bound to its
/// target by a stable bone index (`Bone`) or the durable node name (`Node`/weights).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnimTrack {
    /// What kind of thing this track drives.
    pub target: AnimTarget,
    /// Stable index into the skinned mesh's bones for a `Bone` target (resolved at
    /// import by name); `-1` for `Node`/`Weights` targets, which bind by name.
    pub index: i32,
    /// The glTF node name — the durable binding key that survives reorder/reimport.
    pub target_name: String,
    /// The targeted channel.
    pub path: AnimPath,
    /// The sampler's interpolation.
    pub interp: AnimInterp,
    /// Weights-per-keyframe for an [`AnimPath::Weights`] track (the morph-target
    /// count); `0` for T/R/S tracks.
    pub morph_count: u32,
    /// `sampler.input` — strictly increasing keyframe times, in seconds.
    pub times: Vec<f32>,
    /// `sampler.output`, flat: a `Vec3` per key for T/S, a quaternion `xyzw` per key
    /// for R, `morph_count` floats per key for Weights; `CubicSpline` stores 3x
    /// (in-tangent, value, out-tangent) per key.
    pub values: Vec<f32>,
}

/// A named animation clip: a bundle of heterogeneous tracks (bone, node, and
/// morph-weight) with a total duration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnimClip {
    /// The clip name.
    pub name: String,
    /// Total duration in seconds (the max track end time).
    pub duration: f32,
    /// The tracks that make up the clip.
    pub tracks: Vec<AnimTrack>,
}

/// One node of the imported scene graph: name, parent index, and the local TRS.
///
/// `rotation` is the source quaternion in glam's `xyzw` order (the glTF storage
/// order — no swizzle); consumers convert to their own Euler convention.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedNode {
    /// The glTF node name.
    pub name: String,
    /// Parent node index, or `-1` for a root node.
    pub parent: i32,
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation (glam `xyzw`, the glTF storage order).
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
    /// The node-local merged mesh for the primitives under this glTF node, if any.
    /// This is the single mesh-ownership shape — there is no top-level model mesh.
    pub mesh: Option<Mesh>,
}

impl Default for ImportedNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            parent: -1,
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            mesh: None,
        }
    }
}

/// One glTF skin: the ordered joint node indices (the `jointMatrices[]` order) and
/// the parallel inverse bind matrices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedSkin {
    /// Joint node indices, in `jointMatrices[]` order.
    pub joints: Vec<i32>,
    /// The inverse bind matrix per joint, parallel to [`ImportedSkin::joints`].
    pub inverse_bind: Vec<Mat4>,
    /// The skin's declared root node index, or `-1` if unspecified.
    pub skeleton_root: i32,
    /// The node carrying the skinned mesh, or `-1` if unspecified.
    pub mesh_node: i32,
}

/// One imported material texture: the encoded (png/jpg) bytes plus their extension.
///
/// Carried as an `Option<TextureSource>`, so "is the texture present?" and the payload
/// are one field and a presence flag can never disagree with the bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextureSource {
    /// The encoded image bytes (png/jpg), as read from an external file or embedded.
    pub bytes: Vec<u8>,
    /// The source extension, e.g. `"png"` / `"jpg"`.
    pub ext: String,
}

/// How a material's alpha channel resolves at raster time, mirroring the glTF
/// `alphaMode`: blend opacity, cutout test, or fully opaque.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    /// Alpha is ignored; the surface is fully opaque.
    #[default]
    Opaque,
    /// Alpha-tested cutout: a texel below `alpha_cutoff` is discarded.
    Mask,
    /// Alpha-blended translucency.
    Blend,
}

/// One material extracted from a model: the PBR factors and any optional textures.
///
/// Each optional texture is an `Option<TextureSource>`, so a presence flag can never
/// disagree with the bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedMaterial {
    /// The source material name (the stable key for its baked sub-id).
    pub name: String,
    /// Base color factor (RGBA).
    pub base_color: Vec4,
    /// Metallic factor.
    pub metallic: f32,
    /// Roughness factor.
    pub roughness: f32,
    /// Emissive factor (RGB).
    pub emissive: Vec3,
    /// Emissive strength multiplier.
    pub emissive_strength: f32,
    /// The base-color (albedo) texture, if any (sRGB color).
    pub albedo: Option<TextureSource>,
    /// The glTF metallic-roughness texture, if any (roughness in G, metalness in B; linear).
    pub metallic_roughness: Option<TextureSource>,
    /// The tangent-space normal map, if any (linear).
    pub normal: Option<TextureSource>,
    /// The ambient-occlusion texture, if any (AO in R; linear).
    pub occlusion: Option<TextureSource>,
    /// The emissive texture, if any (sRGB).
    pub emissive_tex: Option<TextureSource>,
    /// How the alpha channel resolves (glTF `alphaMode`).
    pub alpha_mode: AlphaMode,
    /// The cutout threshold for [`AlphaMode::Mask`] (glTF `alphaCutoff`).
    pub alpha_cutoff: f32,
    /// Whether both faces are rasterized (glTF `doubleSided`).
    pub double_sided: bool,
}

impl Default for ImportedMaterial {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: Vec4::ONE,
            metallic: 0.0,
            roughness: 1.0,
            emissive: Vec3::ZERO,
            emissive_strength: 1.0,
            albedo: None,
            metallic_roughness: None,
            normal: None,
            occlusion: None,
            emissive_tex: None,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
        }
    }
}

/// The skin payload of a skinned model.
///
/// Carried as one `Option<SkinPayload>` on [`ImportedModel`]: present means skinned,
/// and the skin-only fields travel together, so no presence flag can disagree. The
/// node forest and clips are not here — they are top-level on [`ImportedModel`],
/// decoded for skinned and unskinned alike.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkinPayload {
    /// Per-vertex skin influences, parallel to the skinned node's [`Mesh::vertices`].
    pub stream: Vec<VertexSkin>,
    /// The skin descriptor; `desc.joints` indexes into [`ImportedModel::nodes`] in
    /// glTF joint order — the single source of `jointMatrices` order.
    pub desc: ImportedSkin,
}

/// The in-memory import graph a source model (`.gltf`/`.glb`/`.obj`) translates into.
///
/// Mesh ownership is uniform: every mesh lives node-local on an [`ImportedNode`] in
/// [`ImportedModel::nodes`]. There is no top-level mesh — OBJ and single-skinned-mesh
/// imports route their geometry through a node too.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedModel {
    /// The imported node forest; mesh-bearing nodes carry a node-local `mesh`.
    pub nodes: Vec<ImportedNode>,
    /// The material table; each [`Submesh::material_slot`] indexes it. Always at
    /// least one entry (a default material when the source declares none).
    pub materials: Vec<ImportedMaterial>,
    /// The clips decoded from the glTF animations — heterogeneous bone, node, and
    /// morph-weight tracks side by side.
    pub animations: Vec<AnimClip>,
    /// The skin payload (glTF only); `None` for an unskinned model.
    pub skin: Option<SkinPayload>,
    /// The morph targets (sparse deltas + names + rest weights); `None` when the model
    /// has no blend shapes. Mesh-global: the target names ride the mesh-level
    /// `extras.targetNames` and the weight vector is shared across the mesh's primitives.
    pub morph: Option<MorphData>,
}

impl ImportedModel {
    /// The model's primary mesh: the skinned mesh node's mesh when rigged, else the first
    /// mesh-bearing node's mesh. `None` if the model carries no geometry. Used by the
    /// single-mesh upload paths (preview/gizmo models) that predate the node forest.
    #[must_use]
    pub fn primary_mesh(&self) -> Option<&Mesh> {
        if let Some(skin) = &self.skin {
            let node = self.nodes.get(skin.desc.mesh_node.max(0) as usize);
            if let Some(mesh) = node.and_then(|n| n.mesh.as_ref()) {
                return Some(mesh);
            }
        }
        self.nodes.iter().find_map(|n| n.mesh.as_ref())
    }
}

/// Decoded RGBA8 pixels, tightly packed (`width * height * 4` bytes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodedImage {
    /// The tightly packed RGBA8 pixel bytes.
    pub rgba: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// Decoded linear-float RGBA, tightly packed (`width * height * 4` floats).
///
/// From `.hdr`/`.exr`-class sources; values are real radiance (may exceed `1.0`),
/// never sRGB-encoded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedImageFloat {
    /// The tightly packed linear-float RGBA values.
    pub rgba: Vec<f32>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// A material texture slot's semantic role.
///
/// The import-options colorspace policy keys on it (albedo/emissive → sRGB color;
/// the rest → linear data), so one source of truth decides how a baked or scanned
/// texture is interpreted.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaterialMapRole {
    /// Base color (sRGB).
    #[default]
    Albedo,
    /// Metallic-roughness (linear).
    MetallicRoughness,
    /// Tangent-space normal map (linear).
    Normal,
    /// Ambient occlusion (linear).
    Occlusion,
    /// Emissive (sRGB).
    Emissive,
    /// Height/displacement (linear).
    Height,
}
