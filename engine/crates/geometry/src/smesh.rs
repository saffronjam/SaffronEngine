//! The `.smesh` (`SMSH`) byte format: a 64-byte header followed by three contiguous
//! sections (vertices, indices, submeshes) plus two optional sections (skin, morph)
//! selected by the header flags.
//!
//! The image is the canonical triple contract: disk bytes == in-memory payload == the
//! GPU vertex buffer, so a `.smodel` MESH chunk slice and a standalone `.smesh` file
//! read the same. The bytes are reinterpreted with **safe** `bytemuck` over
//! `#[repr(C)]` Pod structs (`bytes_of` / `cast_slice` to write, `from_bytes` /
//! `cast_slice` to read), so the crate's `#![deny(unsafe_code)]` holds.
//!
//! One version lives in the format ([`MESH_FORMAT_VERSION`] = 3). A flags word
//! ([`MESH_FLAG_SKIN`] / [`MESH_FLAG_MORPH`]) selects the optional skin and morph
//! sections; the loader accepts only version 3 and reads each optional section when its
//! flag is set. Morph target names are not in the binary — they ride in the container
//! META, so the `.smesh` is pure fixed-stride Pod arrays.

use std::fs;
use std::path::Path;

use bytemuck::{Pod, Zeroable};

use crate::error::{Error, Result};
use crate::types::{
    Mesh, MeshCounts, MorphData, MorphDelta, MorphTarget, Submesh, Vertex, VertexSkin,
};

/// The `.smesh` format version: a 64-byte header, three required sections (vertices,
/// indices, submeshes), and two optional sections (skin, morph) behind the flags word.
pub const MESH_FORMAT_VERSION: u32 = 4;

/// Header flag: a `VertexSkin` section (parallel to the vertices) follows the submeshes.
const MESH_FLAG_SKIN: u32 = 1 << 0;
/// Header flag: a morph section (at `morph_offset`) carries sparse per-vertex deltas.
const MESH_FLAG_MORPH: u32 = 1 << 1;

/// The four-byte tag at the head of every `.smesh` image.
const MAGIC: [u8; 4] = *b"SMSH";

/// The 64-byte fixed header; the required sections follow at the offsets, the optional
/// sections behind the flags.
///
/// `#[repr(C)]` Pod with a fixed field order and width. The offsets are self-relative
/// (from the start of the image), so an embedded `.smodel` chunk slice reads
/// identically to a standalone file.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
struct SMeshHeader {
    /// `b"SMSH"`.
    magic: [u8; 4],
    /// Format version; only [`MESH_FORMAT_VERSION`] (4) is accepted.
    version: u32,
    /// `MESH_FLAG_SKIN | MESH_FLAG_MORPH` selecting the optional sections.
    flags: u32,
    /// Bytes per vertex; must equal `size_of::<Vertex>()` (48).
    vertex_stride: u32,
    /// Number of vertices.
    vertex_count: u32,
    /// Number of indices.
    index_count: u32,
    /// Bytes per index; must equal 4.
    index_width: u32,
    /// Number of submeshes.
    submesh_count: u32,
    /// Offset to the vertex section; equals `size_of::<SMeshHeader>()` (64).
    vertices_offset: u64,
    /// Offset to the index section.
    indices_offset: u64,
    /// Offset to the submesh section.
    submeshes_offset: u64,
    /// Offset to the morph section, or 0 when `MESH_FLAG_MORPH` is clear. The morph
    /// section follows the skin section when both are present.
    morph_offset: u64,
}

const _: () = assert!(
    size_of::<SMeshHeader>() == 64,
    "SMeshHeader must be exactly 64 bytes"
);

/// The morph section sub-header (one per morph mesh, at `morph_offset`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
struct MorphSectionHeader {
    /// Number of morph targets that follow.
    target_count: u32,
    /// Total `MorphDelta` records across all targets.
    delta_count: u32,
}

/// One per target, immediately after the section header: where this target's deltas sit
/// and its authored rest weight. Names live in META, not the binary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct MorphTargetDesc {
    /// First `MorphDelta` index for this target.
    first_delta: u32,
    /// Number of `MorphDelta` records for this target.
    delta_count: u32,
    /// Authored rest weight.
    rest_weight: f32,
    /// Pad to 16 bytes, always 0.
    _pad: u32,
}

/// Builds the `.smesh` byte image: the header, the three required sections, then the
/// optional skin section (when `skin` is non-empty) and morph section (when `morph` is
/// `Some` and non-empty), in that order.
fn encode_mesh_image(mesh: &Mesh, skin: &[VertexSkin], morph: Option<&MorphData>) -> Vec<u8> {
    let vertex_count = mesh.vertices.len() as u32;
    let index_count = mesh.indices.len() as u32;
    let submesh_count = mesh.submeshes.len() as u32;

    let vertices_offset = size_of::<SMeshHeader>() as u64;
    let indices_offset = vertices_offset + u64::from(vertex_count) * size_of::<Vertex>() as u64;
    let submeshes_offset = indices_offset + u64::from(index_count) * size_of::<u32>() as u64;
    let submeshes_end = submeshes_offset + u64::from(submesh_count) * size_of::<Submesh>() as u64;

    let has_skin = !skin.is_empty();
    let skin_end = if has_skin {
        submeshes_end + u64::from(vertex_count) * size_of::<VertexSkin>() as u64
    } else {
        submeshes_end
    };

    let morph = morph.filter(|m| !m.targets.is_empty());
    let mut flags = 0u32;
    if has_skin {
        flags |= MESH_FLAG_SKIN;
    }
    let morph_offset = if morph.is_some() {
        flags |= MESH_FLAG_MORPH;
        skin_end
    } else {
        0
    };

    let header = SMeshHeader {
        magic: MAGIC,
        version: MESH_FORMAT_VERSION,
        flags,
        vertex_stride: size_of::<Vertex>() as u32,
        vertex_count,
        index_count,
        index_width: size_of::<u32>() as u32,
        submesh_count,
        vertices_offset,
        indices_offset,
        submeshes_offset,
        morph_offset,
    };

    let mut bytes = Vec::with_capacity(skin_end as usize);
    bytes.extend_from_slice(bytemuck::bytes_of(&header));
    bytes.extend_from_slice(bytemuck::cast_slice(&mesh.vertices));
    bytes.extend_from_slice(bytemuck::cast_slice(&mesh.indices));
    bytes.extend_from_slice(bytemuck::cast_slice(&mesh.submeshes));
    if has_skin {
        bytes.extend_from_slice(bytemuck::cast_slice(skin));
    }
    if let Some(morph) = morph {
        let delta_count: u32 = morph.targets.iter().map(|t| t.deltas.len() as u32).sum();
        let section = MorphSectionHeader {
            target_count: morph.targets.len() as u32,
            delta_count,
        };
        bytes.extend_from_slice(bytemuck::bytes_of(&section));
        let mut first_delta = 0u32;
        for target in &morph.targets {
            let desc = MorphTargetDesc {
                first_delta,
                delta_count: target.deltas.len() as u32,
                rest_weight: target.rest_weight,
                _pad: 0,
            };
            bytes.extend_from_slice(bytemuck::bytes_of(&desc));
            first_delta += target.deltas.len() as u32;
        }
        for target in &morph.targets {
            bytes.extend_from_slice(bytemuck::cast_slice(&target.deltas));
        }
    }
    bytes
}

/// Encodes `mesh` to a `.smesh` byte image with optional skin and morph sections.
///
/// A non-empty `skin` must parallel the vertices one-for-one (a length mismatch is an
/// [`Error::SkinLengthMismatch`]); an empty `skin` writes no skin section. A `None` /
/// empty `morph` writes no morph section.
pub fn save_mesh_to_buffer(
    mesh: &Mesh,
    skin: &[VertexSkin],
    morph: Option<&MorphData>,
) -> Result<Vec<u8>> {
    if !skin.is_empty() && skin.len() != mesh.vertices.len() {
        return Err(Error::SkinLengthMismatch {
            skin: skin.len(),
            vertices: mesh.vertices.len(),
        });
    }
    Ok(encode_mesh_image(mesh, skin, morph))
}

/// Reads and validates a `.smesh` header from the front of `bytes`.
fn read_header(bytes: &[u8]) -> Result<SMeshHeader> {
    let head = bytes
        .get(..size_of::<SMeshHeader>())
        .ok_or(Error::Truncated)?;
    let header: &SMeshHeader = bytemuck::from_bytes(head);
    if header.magic != MAGIC {
        return Err(Error::BadMagic);
    }
    Ok(*header)
}

/// The byte offset just past the submesh section (where the skin section begins).
fn submeshes_end(header: &SMeshHeader) -> u64 {
    header.submeshes_offset + u64::from(header.submesh_count) * size_of::<Submesh>() as u64
}

/// Decodes the vertices, indices, and submeshes of a `.smesh` image.
///
/// Validates the magic, the version (3), the vertex/index strides, then recomputes the
/// section layout from the counts and requires the header's stored offsets to match and
/// the span to be long enough. The span length is the chunk length, not a file size, so
/// an embedded `.smodel` chunk reads identically to a standalone file.
pub fn load_mesh_from_bytes(bytes: &[u8]) -> Result<Mesh> {
    let header = read_header(bytes)?;
    if header.version != MESH_FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(header.version));
    }
    if header.vertex_stride != size_of::<Vertex>() as u32
        || header.index_width != size_of::<u32>() as u32
    {
        return Err(Error::BadLayout);
    }

    let vertices_end = size_of::<SMeshHeader>() as u64
        + u64::from(header.vertex_count) * size_of::<Vertex>() as u64;
    let indices_end = vertices_end + u64::from(header.index_count) * size_of::<u32>() as u64;
    let submeshes_end = indices_end + u64::from(header.submesh_count) * size_of::<Submesh>() as u64;
    if header.vertices_offset != size_of::<SMeshHeader>() as u64
        || header.indices_offset != vertices_end
        || header.submeshes_offset != indices_end
        || (bytes.len() as u64) < submeshes_end
    {
        return Err(Error::BadLayout);
    }

    let vertices: &[Vertex] =
        bytemuck::cast_slice(&bytes[header.vertices_offset as usize..vertices_end as usize]);
    let indices: &[u32] =
        bytemuck::cast_slice(&bytes[header.indices_offset as usize..indices_end as usize]);
    let submeshes: &[Submesh] =
        bytemuck::cast_slice(&bytes[header.submeshes_offset as usize..submeshes_end as usize]);

    Ok(Mesh {
        vertices: vertices.to_vec(),
        indices: indices.to_vec(),
        submeshes: submeshes.to_vec(),
    })
}

/// Decodes the skin section of a `.smesh` image.
///
/// Returns an empty stream (not an error) when `MESH_FLAG_SKIN` is clear; a set flag with
/// a truncated skin section is an [`Error::Truncated`].
pub fn load_mesh_skin_from_bytes(bytes: &[u8]) -> Result<Vec<VertexSkin>> {
    let header = read_header(bytes)?;
    if header.flags & MESH_FLAG_SKIN == 0 {
        return Ok(Vec::new());
    }
    let start = submeshes_end(&header);
    let skin_end = start + u64::from(header.vertex_count) * size_of::<VertexSkin>() as u64;
    if (bytes.len() as u64) < skin_end {
        return Err(Error::Truncated);
    }
    let skin: &[VertexSkin] = bytemuck::cast_slice(&bytes[start as usize..skin_end as usize]);
    Ok(skin.to_vec())
}

/// Decodes the morph section of a `.smesh` image.
///
/// Returns `None` when `MESH_FLAG_MORPH` is clear; otherwise reads the section at
/// `morph_offset` into a [`MorphData`] with **empty** target names (the caller fills
/// names from META). A truncated section is an [`Error::Truncated`].
pub fn load_mesh_morph_from_bytes(bytes: &[u8]) -> Result<Option<MorphData>> {
    let header = read_header(bytes)?;
    if header.flags & MESH_FLAG_MORPH == 0 {
        return Ok(None);
    }
    let mut pos = header.morph_offset as usize;
    let section_bytes = bytes
        .get(pos..pos + size_of::<MorphSectionHeader>())
        .ok_or(Error::Truncated)?;
    let section: MorphSectionHeader = bytemuck::pod_read_unaligned(section_bytes);
    pos += size_of::<MorphSectionHeader>();

    let descs_len = section.target_count as usize * size_of::<MorphTargetDesc>();
    let desc_bytes = bytes.get(pos..pos + descs_len).ok_or(Error::Truncated)?;
    let descs: Vec<MorphTargetDesc> = desc_bytes
        .chunks_exact(size_of::<MorphTargetDesc>())
        .map(bytemuck::pod_read_unaligned)
        .collect();
    pos += descs_len;

    let deltas_len = section.delta_count as usize * size_of::<MorphDelta>();
    let delta_bytes = bytes.get(pos..pos + deltas_len).ok_or(Error::Truncated)?;
    let deltas: Vec<MorphDelta> = delta_bytes
        .chunks_exact(size_of::<MorphDelta>())
        .map(bytemuck::pod_read_unaligned)
        .collect();

    let mut targets = Vec::with_capacity(section.target_count as usize);
    for desc in &descs {
        let first = desc.first_delta as usize;
        let end = first
            .checked_add(desc.delta_count as usize)
            .filter(|&e| e <= deltas.len())
            .ok_or(Error::BadLayout)?;
        targets.push(MorphTarget {
            name: String::new(),
            rest_weight: desc.rest_weight,
            deltas: deltas[first..end].to_vec(),
        });
    }
    Ok(Some(MorphData { targets }))
}

/// Reads the vertex/index totals from a `.smesh` header without loading the data.
pub fn mesh_counts_from_bytes(bytes: &[u8]) -> Result<MeshCounts> {
    let header = read_header(bytes)?;
    Ok(MeshCounts {
        vertex_count: header.vertex_count,
        index_count: header.index_count,
    })
}

/// Reads a `.smesh` file and decodes its mesh.
pub fn load_mesh(path: impl AsRef<Path>) -> Result<Mesh> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    load_mesh_from_bytes(&bytes)
}

/// Reads a `.smesh` file and decodes its skin stream (empty when the file has no skin).
pub fn load_mesh_skin(path: impl AsRef<Path>) -> Result<Vec<VertexSkin>> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    load_mesh_skin_from_bytes(&bytes)
}

/// Reads the vertex/index totals from a `.smesh` file's header without loading the data.
pub fn mesh_file_counts(path: impl AsRef<Path>) -> Result<MeshCounts> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    mesh_counts_from_bytes(&bytes)
}

/// Encodes `mesh` plus optional `skin`/`morph` and writes the image to `path`.
pub fn save_mesh(
    mesh: &Mesh,
    skin: &[VertexSkin],
    morph: Option<&MorphData>,
    path: impl AsRef<Path>,
) -> Result<()> {
    let bytes = save_mesh_to_buffer(mesh, skin, morph)?;
    let path = path.as_ref();
    fs::write(path, &bytes).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MorphTarget;
    use glam::{Vec2, Vec3};

    fn sample_mesh() -> Mesh {
        Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(1.0, 2.0, 3.0),
                    normal: Vec3::Y,
                    uv0: Vec2::new(0.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(-1.0, 0.5, 4.0),
                    normal: Vec3::X,
                    uv0: Vec2::new(0.25, 0.75),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(2.0, -3.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    fn sample_skin() -> Vec<VertexSkin> {
        vec![
            VertexSkin {
                joints: [0, 1, 2, 3],
                weights: [0.5, 0.25, 0.15, 0.10],
            },
            VertexSkin {
                joints: [4, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            },
            VertexSkin {
                joints: [2, 3, 0, 0],
                weights: [0.6, 0.4, 0.0, 0.0],
            },
        ]
    }

    fn sample_morph() -> MorphData {
        MorphData {
            targets: vec![
                MorphTarget {
                    name: String::new(),
                    rest_weight: 0.0,
                    deltas: vec![
                        MorphDelta {
                            vertex_index: 0,
                            d_position: Vec3::new(0.0, 1.0, 0.0),
                            d_normal: Vec3::ZERO,
                        },
                        MorphDelta {
                            vertex_index: 2,
                            d_position: Vec3::new(0.5, 0.0, 0.0),
                            d_normal: Vec3::Y,
                        },
                    ],
                },
                MorphTarget {
                    name: String::new(),
                    rest_weight: 0.25,
                    deltas: vec![MorphDelta {
                        vertex_index: 1,
                        d_position: Vec3::new(0.0, 0.0, -1.0),
                        d_normal: Vec3::ZERO,
                    }],
                },
            ],
        }
    }

    #[test]
    fn unskinned_round_trip() {
        let mesh = sample_mesh();
        let baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        let loaded = load_mesh_from_bytes(&baked).unwrap();
        assert_eq!(loaded, mesh);
        assert!(load_mesh_skin_from_bytes(&baked).unwrap().is_empty());
        assert!(load_mesh_morph_from_bytes(&baked).unwrap().is_none());
    }

    #[test]
    fn skinned_round_trip() {
        let mesh = sample_mesh();
        let skin = sample_skin();
        let baked = save_mesh_to_buffer(&mesh, &skin, None).unwrap();
        let loaded = load_mesh_from_bytes(&baked).unwrap();
        let loaded_skin = load_mesh_skin_from_bytes(&baked).unwrap();
        assert_eq!(loaded, mesh);
        assert_eq!(loaded_skin, skin);
        assert!(load_mesh_morph_from_bytes(&baked).unwrap().is_none());
    }

    #[test]
    fn morph_only_round_trip() {
        let mesh = sample_mesh();
        let morph = sample_morph();
        let baked = save_mesh_to_buffer(&mesh, &[], Some(&morph)).unwrap();
        assert_eq!(load_mesh_from_bytes(&baked).unwrap(), mesh);
        assert!(load_mesh_skin_from_bytes(&baked).unwrap().is_empty());
        let loaded = load_mesh_morph_from_bytes(&baked).unwrap().unwrap();
        assert_eq!(loaded.targets.len(), 2);
        assert_eq!(loaded.targets[0].deltas, morph.targets[0].deltas);
        assert_eq!(loaded.targets[1].rest_weight, 0.25);
        assert_eq!(loaded.targets[1].deltas, morph.targets[1].deltas);
    }

    #[test]
    fn skin_and_morph_round_trip() {
        let mesh = sample_mesh();
        let skin = sample_skin();
        let morph = sample_morph();
        let baked = save_mesh_to_buffer(&mesh, &skin, Some(&morph)).unwrap();
        assert_eq!(load_mesh_from_bytes(&baked).unwrap(), mesh);
        assert_eq!(load_mesh_skin_from_bytes(&baked).unwrap(), skin);
        let loaded = load_mesh_morph_from_bytes(&baked).unwrap().unwrap();
        assert_eq!(loaded.targets.len(), 2);
        assert_eq!(loaded.targets[0].deltas, morph.targets[0].deltas);
    }

    #[test]
    fn empty_morph_writes_no_section() {
        let mesh = sample_mesh();
        let empty = MorphData::default();
        let baked = save_mesh_to_buffer(&mesh, &[], Some(&empty)).unwrap();
        assert!(load_mesh_morph_from_bytes(&baked).unwrap().is_none());
    }

    #[test]
    fn counts_read_from_header_only() {
        let mesh = sample_mesh();
        let baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        let counts = mesh_counts_from_bytes(&baked).unwrap();
        assert_eq!(counts.vertex_count, 3);
        assert_eq!(counts.index_count, 3);
    }

    #[test]
    fn golden_bytes_header_is_frozen() {
        // The byte image is the frozen contract; this asserts the exact header fields,
        // the flag bits, and the section offsets.
        let mesh = sample_mesh();
        let baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();

        // 64-byte header + 3*48 vertices + 3*4 indices + 1*16 submesh = 64+144+12+16.
        assert_eq!(baked.len(), 64 + 3 * 48 + 3 * 4 + 16);

        let header: &SMeshHeader = bytemuck::from_bytes(&baked[..64]);
        assert_eq!(&header.magic, b"SMSH");
        assert_eq!(header.version, 4);
        assert_eq!(header.flags, 0);
        assert_eq!(header.vertex_stride, 48);
        assert_eq!(header.index_width, 4);
        assert_eq!(header.vertex_count, 3);
        assert_eq!(header.index_count, 3);
        assert_eq!(header.submesh_count, 1);
        assert_eq!(header.vertices_offset, 64);
        assert_eq!(header.indices_offset, 64 + 3 * 48);
        assert_eq!(header.submeshes_offset, 64 + 3 * 48 + 3 * 4);
        assert_eq!(header.morph_offset, 0);

        // A skinned image sets MESH_FLAG_SKIN and is one VertexSkin stride longer per vertex.
        let skin = sample_skin();
        let baked_skin = save_mesh_to_buffer(&mesh, &skin, None).unwrap();
        let header_skin: &SMeshHeader = bytemuck::from_bytes(&baked_skin[..64]);
        assert_eq!(header_skin.flags, MESH_FLAG_SKIN);
        assert_eq!(header_skin.morph_offset, 0);
        assert_eq!(baked_skin.len(), baked.len() + 3 * 24);

        // A skin+morph image sets both flags and points morph_offset past the skin section.
        let morph = sample_morph();
        let baked_both = save_mesh_to_buffer(&mesh, &skin, Some(&morph)).unwrap();
        let header_both: &SMeshHeader = bytemuck::from_bytes(&baked_both[..64]);
        assert_eq!(header_both.flags, MESH_FLAG_SKIN | MESH_FLAG_MORPH);
        assert_eq!(header_both.morph_offset, baked_skin.len() as u64);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mesh = sample_mesh();
        let mut baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        baked[0] = b'X';
        assert!(matches!(load_mesh_from_bytes(&baked), Err(Error::BadMagic)));
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mesh = sample_mesh();
        let mut baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        // Overwrite the version field (bytes 4..8) with a non-4 value.
        baked[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            load_mesh_from_bytes(&baked),
            Err(Error::UnsupportedVersion(2))
        ));
        baked[4..8].copy_from_slice(&5u32.to_le_bytes());
        assert!(matches!(
            load_mesh_from_bytes(&baked),
            Err(Error::UnsupportedVersion(5))
        ));
    }

    #[test]
    fn truncated_header_is_rejected() {
        let mesh = sample_mesh();
        let baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        assert!(matches!(
            load_mesh_from_bytes(&baked[..32]),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn truncated_body_is_bad_layout() {
        let mesh = sample_mesh();
        let baked = save_mesh_to_buffer(&mesh, &[], None).unwrap();
        // Drop the last submesh bytes: header is intact but the span is too short.
        let truncated = &baked[..baked.len() - 8];
        assert!(matches!(
            load_mesh_from_bytes(truncated),
            Err(Error::BadLayout)
        ));
    }

    #[test]
    fn truncated_skin_section_is_rejected() {
        let mesh = sample_mesh();
        let skin = sample_skin();
        let baked = save_mesh_to_buffer(&mesh, &skin, None).unwrap();
        let truncated = &baked[..baked.len() - 8];
        assert!(matches!(
            load_mesh_skin_from_bytes(truncated),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn mismatched_skin_length_is_rejected() {
        let mesh = sample_mesh();
        let skin = vec![VertexSkin::default()]; // 1 vs 3 vertices
        assert!(matches!(
            save_mesh_to_buffer(&mesh, &skin, None),
            Err(Error::SkinLengthMismatch {
                skin: 1,
                vertices: 3
            })
        ));
    }

    #[test]
    fn file_round_trip() {
        let mesh = sample_mesh();
        let skin = sample_skin();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("saffron-smesh-test-{}.smesh", std::process::id()));
        save_mesh(&mesh, &skin, None, &path).unwrap();

        let loaded = load_mesh(&path).unwrap();
        let loaded_skin = load_mesh_skin(&path).unwrap();
        let counts = mesh_file_counts(&path).unwrap();
        assert_eq!(loaded, mesh);
        assert_eq!(loaded_skin, skin);
        assert_eq!(counts.vertex_count, 3);
        assert_eq!(counts.index_count, 3);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = std::env::temp_dir().join("saffron-smesh-does-not-exist.smesh");
        assert!(matches!(load_mesh(&path), Err(Error::Io(_))));
    }
}
