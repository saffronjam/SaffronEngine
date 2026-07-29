//! OBJ (`.obj`) import onto the `tobj` crate.
//!
//! The load-bearing concern is **dedup determinism**. `(vertex, normal, texcoord)`
//! index triples are deduped into unique vertices via a [`BTreeMap`] over the
//! `[i32; 3]` key, not a `HashMap`: the ordered map preserves both the dedup result
//! **and the emitted vertex order**, so a given OBJ always emits its vertices in the
//! same order and the bytes of the subsequent `.smesh` bake stay stable. A `HashMap`
//! would dedup correctly but emit vertices in a nondeterministic order, silently
//! changing every baked `.smesh`'s bytes. That choice is the reason this module is
//! split out and is pinned by a re-import-determinism test.
//!
//! Faces are grouped into first-seen material slots (empty slots skipped), the OBJ
//! **V-flip** (`uv0.y = 1.0 - v`) is applied because OBJ's texture origin is
//! bottom-left and Vulkan samples top-left, and out-of-range indices are guarded.

use std::collections::BTreeMap;
use std::path::Path;

use glam::{Vec2, Vec3, Vec4};

use crate::error::{Error, Result};
use crate::picking::generate_normals;
use crate::types::{
    ImportedMaterial, ImportedModel, ImportedNode, Mesh, Submesh, TextureSource, Vertex,
};

/// Import an `.obj` model into the in-memory [`ImportedModel`] graph.
///
/// Resolves the `.mtl` and any textures relative to the OBJ's own directory, dedups
/// `(vertex, normal, texcoord)` triples into unique vertices (deterministic emit
/// order), groups faces into first-seen material slots, applies the OBJ V-flip, and
/// recomputes normals when the source provides none.
pub fn import_obj_model(path: impl AsRef<Path>) -> Result<ImportedModel> {
    let path = path.as_ref();
    let base_dir = path.parent().filter(|p| !p.as_os_str().is_empty());

    let load_options = tobj::LoadOptions {
        triangulate: true,
        single_index: false,
        ..Default::default()
    };
    let (models, materials_result) = tobj::load_obj(path, &load_options)
        .map_err(|e| Error::Import(format!("tobj: cannot load '{}': {e}", path.display())))?;
    // tinyobjloader resolves the `.mtl` next to the OBJ; tobj returns the materials as
    // a separate Result so a missing/broken `.mtl` does not fail the geometry load.
    let materials = materials_result.unwrap_or_default();

    let mut mesh = Mesh::default();

    // Faces are grouped into slots in first-seen material order. `slot_to_obj_material`
    // maps a slot to its tobj material index (`-1` == no material); `indices_by_slot`
    // collects the slot's triangle indices.
    let mut slots = SlotMap::default();

    for model in &models {
        // De-duplicate (position, normal, texcoord) index triples into unique vertices.
        // BTreeMap (an ordered tree) emits vertices deterministically across runs.
        //
        // The map is PER MODEL because tobj re-indexes each object and each `usemtl` run against
        // its own arrays: two models both start at triple (0, 0, 0) while meaning different
        // vertices, so one shared map folds every later object onto the first one's geometry.
        let mut unique_vertices: BTreeMap<[i32; 3], u32> = BTreeMap::new();
        let m = &model.mesh;
        // tobj splits a `usemtl` change mid-object into a fresh `Model`, so every
        // model is a run of faces sharing one `material_id`; grouping by that id lands
        // faces of the same OBJ material in one slot, even across shapes.
        let obj_material = normalize_material(m.material_id, materials.len());
        let face_count = m.indices.len() / 3;
        for f in 0..face_count {
            let slot = slots.slot_for(obj_material);
            for c in 0..3 {
                let i = f * 3 + c;
                let resolved = resolve_vertex(m, i, &mut mesh, &mut unique_vertices, path)?;
                slots.indices_by_slot[slot as usize].push(resolved);
            }
        }
    }

    // The slot number is the first-seen order, so iterating slots in index order
    // emits submeshes in encounter order; empty slots are skipped.
    for slot in 0..slots.indices_by_slot.len() as u32 {
        let bucket = &slots.indices_by_slot[slot as usize];
        if bucket.is_empty() {
            continue;
        }
        let submesh = Submesh {
            first_index: mesh.indices.len() as u32,
            index_count: bucket.len() as u32,
            // Indices already reference the shared vertex array.
            vertex_offset: 0,
            material_slot: slot,
        };
        mesh.indices.extend_from_slice(bucket);
        mesh.submeshes.push(submesh);
    }

    if mesh.vertices.is_empty() {
        return Err(Error::Import(format!(
            "tobj: '{}' has no geometry",
            path.display()
        )));
    }
    if !any_normals_present(&mesh) {
        generate_normals(&mut mesh);
    }
    crate::compute_tangents(&mut mesh);

    let mut out_materials: Vec<ImportedMaterial> =
        Vec::with_capacity(slots.slot_to_obj_material.len());
    for &obj_material in &slots.slot_to_obj_material {
        out_materials.push(extract_obj_material(obj_material, &materials, base_dir));
    }

    // One mesh-ownership shape: the OBJ geometry rides a single identity root node, so
    // spawn collapses it to one entity exactly like a single-node glTF.
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mesh")
        .to_owned();
    let node = ImportedNode {
        name,
        mesh: Some(mesh),
        ..ImportedNode::default()
    };

    Ok(ImportedModel {
        nodes: vec![node],
        materials: out_materials,
        animations: Vec::new(),
        skin: None,
        morph: None,
        // OBJ has no asset block, so it states nothing about its origin.
        origin: crate::types::ImportedOrigin::default(),
    })
}

/// First-seen material-slot bookkeeping for the OBJ face grouping.
///
/// Faces are grouped into slots in the order their OBJ material is first encountered;
/// the slot number *is* that encounter index. The `BTreeMap` keys by material id
/// (`-1` == no material), so the same material in two shapes merges into one slot.
#[derive(Default)]
struct SlotMap {
    /// Maps a slot to its tobj material index (`-1` == no material).
    slot_to_obj_material: Vec<i32>,
    /// Maps a material id to its assigned slot.
    obj_material_to_slot: BTreeMap<i32, u32>,
    /// The triangle indices collected per slot.
    indices_by_slot: Vec<Vec<u32>>,
}

impl SlotMap {
    /// The slot for `obj_material`, creating it on first encounter.
    fn slot_for(&mut self, obj_material: i32) -> u32 {
        if let Some(&slot) = self.obj_material_to_slot.get(&obj_material) {
            return slot;
        }
        let slot = self.slot_to_obj_material.len() as u32;
        self.obj_material_to_slot.insert(obj_material, slot);
        self.slot_to_obj_material.push(obj_material);
        self.indices_by_slot.push(Vec::new());
        slot
    }
}

/// Normalize a tobj per-model material id to the `-1`-means-none convention,
/// rejecting an out-of-range id.
fn normalize_material(material_id: Option<usize>, material_count: usize) -> i32 {
    match material_id {
        Some(id) if id < material_count => id as i32,
        _ => -1,
    }
}

/// Resolve one face corner to a unique vertex index, deduping the
/// `(vertex, normal, texcoord)` triple. A new triple appends a [`Vertex`] (applying the
/// OBJ V-flip); a seen triple returns its existing index.
fn resolve_vertex(
    m: &tobj::Mesh,
    corner: usize,
    mesh: &mut Mesh,
    unique_vertices: &mut BTreeMap<[i32; 3], u32>,
    path: &Path,
) -> Result<u32> {
    let vertex_index = m.indices[corner] as i32;
    let normal_index = m.normal_indices.get(corner).map_or(-1, |&n| n as i32);
    let texcoord_index = m.texcoord_indices.get(corner).map_or(-1, |&t| t as i32);
    let key = [vertex_index, normal_index, texcoord_index];

    if let Some(&existing) = unique_vertices.get(&key) {
        return Ok(existing);
    }

    if vertex_index < 0 || (3 * vertex_index as usize + 2) >= m.positions.len() {
        return Err(Error::Import(format!(
            "tobj: '{}' has an out-of-range vertex index",
            path.display()
        )));
    }

    let p = 3 * vertex_index as usize;
    let mut vertex = Vertex {
        position: Vec3::new(m.positions[p], m.positions[p + 1], m.positions[p + 2]),
        normal: Vec3::ZERO,
        uv0: Vec2::ZERO,
        // OBJ carries no tangents; the finalize computes them from the UVs (`compute_tangents`).
        tangent: [0.0, 0.0, 0.0, 0.0],
    };
    if normal_index >= 0 && (3 * normal_index as usize + 2) < m.normals.len() {
        let n = 3 * normal_index as usize;
        vertex.normal = Vec3::new(m.normals[n], m.normals[n + 1], m.normals[n + 2]);
    }
    if texcoord_index >= 0 && (2 * texcoord_index as usize + 1) < m.texcoords.len() {
        let t = 2 * texcoord_index as usize;
        // OBJ texture V origin is bottom-left; Vulkan samples top-left.
        vertex.uv0 = Vec2::new(m.texcoords[t], 1.0 - m.texcoords[t + 1]);
    }

    let new_index = mesh.vertices.len() as u32;
    mesh.vertices.push(vertex);
    unique_vertices.insert(key, new_index);
    Ok(new_index)
}

/// Extract one OBJ material's PBR factors and the optional diffuse texture bytes.
///
/// `obj_material == -1` yields the default material. The diffuse texture is read
/// relative to the OBJ's directory; an unreadable file leaves the albedo slot empty.
fn extract_obj_material(
    obj_material: i32,
    materials: &[tobj::Material],
    base_dir: Option<&Path>,
) -> ImportedMaterial {
    if obj_material < 0 {
        return ImportedMaterial::default();
    }
    let mat = &materials[obj_material as usize];
    let mut material = ImportedMaterial {
        name: mat.name.clone(),
        // tinyobjloader seeds its PBR `metallic`/`roughness` to 0 when the `.mtl`
        // omits them, so an OBJ material starts metallic 0 / roughness 0 (overriding
        // the dielectric/full-rough `ImportedMaterial::default`).
        metallic: 0.0,
        roughness: 0.0,
        ..Default::default()
    };
    if let Some(d) = mat.diffuse {
        material.base_color = Vec4::new(d[0], d[1], d[2], 1.0);
    }
    // tobj does not surface tinyobj's PBR extension as typed fields; the `Pm`/`Pr`
    // MTL keys (the same source tinyobj parsed into `.metallic`/`.roughness`) are
    // read from the unrecognized-parameter map when present.
    if let Some(metallic) = mat.unknown_param.get("Pm").and_then(|v| v.parse().ok()) {
        material.metallic = metallic;
    }
    if let Some(roughness) = mat.unknown_param.get("Pr").and_then(|v| v.parse().ok()) {
        material.roughness = roughness;
    }
    if let Some(e) = mat.emissive {
        material.emissive = Vec3::new(e[0], e[1], e[2]);
    }
    if let Some(texname) = &mat.diffuse_texture {
        let dir = base_dir.unwrap_or_else(|| Path::new("."));
        let full = dir.join(texname);
        if let Ok(bytes) = std::fs::read(&full) {
            material.albedo = Some(TextureSource {
                bytes,
                ext: extension_of(texname),
            });
        }
    }
    material
}

/// The extension of a path (the substring after the last `.`), empty if none.
fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) => name[dot + 1..].to_owned(),
        None => String::new(),
    }
}

/// Whether any vertex carries a non-zero normal (the importer recomputes normals only
/// when a source provides none).
fn any_normals_present(mesh: &Mesh) -> bool {
    mesh.vertices.iter().any(|v| v.normal.dot(v.normal) > 1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// The first mesh-bearing node's node-local mesh (OBJ rides a single root node).
    fn mesh_of(model: &ImportedModel) -> &Mesh {
        model
            .nodes
            .iter()
            .find_map(|n| n.mesh.as_ref())
            .expect("a mesh-bearing node")
    }

    #[test]
    fn cube_obj_imports_with_expected_counts() {
        // cube.obj: 24 verts / 24 normals / 24 texcoords, 12 triangulated faces, no
        // material. Every (v, n, t) triple is distinct, so dedup keeps 24 vertices,
        // 36 indices, one submesh, and one default material slot.
        let model = import_obj_model(fixture("cube.obj")).expect("import cube.obj");
        let mesh = mesh_of(&model);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(mesh.submeshes.len(), 1);
        assert_eq!(model.materials.len(), 1);
        assert!(model.skin.is_none());
        // The lone submesh covers the whole index range against the default slot.
        assert_eq!(mesh.submeshes[0].first_index, 0);
        assert_eq!(mesh.submeshes[0].index_count, 36);
        assert_eq!(mesh.submeshes[0].material_slot, 0);
    }

    #[test]
    fn cube_obj_normals_survive_import() {
        // cube.obj ships normals, so the importer keeps them (no generate_normals
        // fallback); every vertex normal is unit length.
        let model = import_obj_model(fixture("cube.obj")).expect("import cube.obj");
        for (i, v) in mesh_of(&model).vertices.iter().enumerate() {
            let len = v.normal.length();
            assert!((len - 1.0).abs() < 1e-4, "vertex {i} normal length {len}");
        }
    }

    #[test]
    fn cube_obj_applies_the_v_flip() {
        // OBJ texcoord V origin is bottom-left; the importer flips to Vulkan's
        // top-left, so every uv0.v lands in [0, 1] as `1 - source_v` (the cube uses
        // 0/1 texcoords, so the flipped values stay 0 or 1 but swapped).
        let model = import_obj_model(fixture("cube.obj")).expect("import cube.obj");
        for v in &mesh_of(&model).vertices {
            assert!((0.0..=1.0).contains(&v.uv0.y));
        }
    }

    #[test]
    fn obj_import_is_deterministic_in_vertex_order() {
        // The single decision this phase locks: the BTreeMap dedup emits the exact
        // same vertex vector across two imports (a HashMap would not). Assert the
        // FULL vertex vector is identical, not just the counts.
        let first = import_obj_model(fixture("cube.obj")).expect("first import");
        let second = import_obj_model(fixture("cube.obj")).expect("second import");
        assert_eq!(mesh_of(&first).vertices, mesh_of(&second).vertices);
        assert_eq!(mesh_of(&first).indices, mesh_of(&second).indices);
        assert_eq!(mesh_of(&first).submeshes, mesh_of(&second).submeshes);
        assert_eq!(first, second);
    }

    /// Every object in a multi-object file keeps its own vertices.
    ///
    /// tobj re-indexes each object and each `usemtl` run against its own arrays, so two objects
    /// both start at the triple `(0, 0, 0)` while meaning different vertices. Deduplicating across
    /// models folds every later object onto the first one's geometry: the index count stays right,
    /// the vertex array is short, and the later objects draw on top of the first — present in every
    /// counter and absent from the picture.
    #[test]
    fn each_object_in_a_multi_object_obj_keeps_its_own_vertices() {
        let dir = std::env::temp_dir().join(format!("saffron-obj-multi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("pair.obj");
        // Two triangles that share no position, with the same texcoord and normal indices — the
        // arrangement whose per-model triples collide.
        std::fs::write(
            &path,
            concat!(
                "o First\nv -1 0 0\nv 1 0 0\nv 0 1 0\n",
                "vt 0 0\nvt 1 0\nvt 0.5 1\nvn 0 0 1\n",
                "f 1/1/1 2/2/1 3/3/1\n",
                "o Second\nv -1 2 0\nv 1 2 0\nv 0 3 0\n",
                "f 4/1/1 5/2/1 6/3/1\n",
            ),
        )
        .expect("write obj");
        let model = import_obj_model(&path).expect("import");
        let mesh = mesh_of(&model);
        assert_eq!(mesh.indices.len(), 6, "both triangles survive");
        assert_eq!(
            mesh.vertices.len(),
            6,
            "and neither borrows the other's vertices"
        );
        // The second triangle must sit where the file put it, two units up.
        let top = mesh
            .vertices
            .iter()
            .map(|vertex| vertex.position.y)
            .fold(f32::MIN, f32::max);
        assert!(
            (top - 3.0).abs() < 1e-5,
            "second object collapsed onto the first: top {top}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
