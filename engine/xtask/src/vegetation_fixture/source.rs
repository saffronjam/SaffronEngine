//! The authored geometry and material files a fixture recipe installs into the project.

use std::fmt::Write as _;
use std::io::Cursor;

use anyhow::Result;
use serde_json::{Value, json};

/// One file the E2E writes under `<project>/assets/` before importing the family.
pub(super) struct SourceFile {
    pub(super) path: String,
    pub(super) bytes: Vec<u8>,
}

/// The geometry and material content a plant family is authored from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlantContent {
    /// A box trunk written as OBJ; `canopy` adds a second material run above it.
    Trunk { canopy: bool },
    /// Broad leaf blades whose whole silhouette is modelled geometry.
    BroadLeaf,
    /// The same blades, with the serrated edge left to the material's alpha cutout.
    MaskedSerration,
    /// Dense needle slivers around the trunk.
    ConiferNeedle,
}

impl PlantContent {
    /// The project-relative path of the geometry source the family references. Every recipe
    /// authors its own stem: two families sharing one project cannot share a source path, or the
    /// later import silently re-reads the earlier one's bytes.
    pub(super) fn primary_path(self, stem: &str) -> String {
        match self {
            Self::Trunk { .. } => format!("models/{stem}.obj"),
            _ => format!("models/{stem}.gltf"),
        }
    }

    /// The imported material names, in document order — the names every material element id and
    /// path derives from.
    pub(super) fn material_names(self) -> &'static [&'static str] {
        match self {
            Self::Trunk { canopy: false } => &["material_0"],
            Self::Trunk { canopy: true } => &["material_0", "material_1"],
            _ => &["bark", "leaf"],
        }
    }

    /// Whether the family splits into a trunk part and a leaf part over two material slots.
    pub(super) fn two_parts(self) -> bool {
        self.material_names().len() == 2
    }

    /// How far the content rises above the trunk top, in whole metres of authored bounds.
    pub(super) fn crown_extent(self) -> i32 {
        match self {
            Self::Trunk { canopy: true } => 2,
            Self::Trunk { canopy: false } => 0,
            _ => 1,
        }
    }

    /// Every file the content is authored from. The first is the geometry source the family's
    /// content hash covers.
    pub(super) fn files(self, stem: &str, trunk_height: i32) -> Result<Vec<SourceFile>> {
        match self {
            Self::Trunk { canopy } => {
                let mut files = vec![SourceFile {
                    path: self.primary_path(stem),
                    bytes: trunk_obj(stem, trunk_height, canopy).into_bytes(),
                }];
                if canopy {
                    files.push(SourceFile {
                        path: format!("models/{stem}.mtl"),
                        bytes: trunk_mtl().into_bytes(),
                    });
                }
                Ok(files)
            }
            _ => leaf_gltf(self, stem, trunk_height),
        }
    }
}

/// A watertight 1x8x1 m box trunk with per-face normals and UVs, the smallest geometry the plant
/// compiler accepts. `canopy` adds a second material run above it, so the family cooks as two
/// submeshes with two material slots.
fn trunk_obj(stem: &str, height: i32, canopy: bool) -> String {
    let mut obj = String::new();
    if canopy {
        let _ = writeln!(obj, "mtllib {stem}.mtl");
    }
    let _ = writeln!(obj, "o {stem}-trunk");
    let (x, y, z) = (0.5_f32, height as f32, 0.5_f32);
    let mut corners = vec![
        [-x, 0.0, -z],
        [x, 0.0, -z],
        [x, y, -z],
        [-x, y, -z],
        [-x, 0.0, z],
        [x, 0.0, z],
        [x, y, z],
        [-x, y, z],
    ];
    if canopy {
        // The crown: a wider box from the trunk top, inside the widened authored bounds.
        let (cx, cz) = (1.5_f32, 1.5_f32);
        let (base, top) = (y, y + 2.0);
        corners.extend([
            [-cx, base, -cz],
            [cx, base, -cz],
            [cx, top, -cz],
            [-cx, top, -cz],
            [-cx, base, cz],
            [cx, base, cz],
            [cx, top, cz],
            [-cx, top, cz],
        ]);
    }
    for corner in &corners {
        let _ = writeln!(obj, "v {} {} {}", corner[0], corner[1], corner[2]);
    }
    for uv in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
        let _ = writeln!(obj, "vt {} {}", uv[0], uv[1]);
    }
    for normal in [
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
    ] {
        let _ = writeln!(obj, "vn {} {} {}", normal[0], normal[1], normal[2]);
    }
    // Counter-clockwise quads seen from outside, split into triangles.
    let faces: [([usize; 4], usize); 6] = [
        ([1, 4, 3, 2], 1),
        ([5, 6, 7, 8], 2),
        ([1, 5, 8, 4], 3),
        ([2, 3, 7, 6], 4),
        ([1, 2, 6, 5], 5),
        ([4, 8, 7, 3], 6),
    ];
    let write_box = |obj: &mut String, base: usize| {
        for (quad, normal) in faces {
            let _ = writeln!(
                obj,
                "f {}/1/{normal} {}/2/{normal} {}/3/{normal}",
                base + quad[0],
                base + quad[1],
                base + quad[2]
            );
            let _ = writeln!(
                obj,
                "f {}/1/{normal} {}/3/{normal} {}/4/{normal}",
                base + quad[0],
                base + quad[2],
                base + quad[3]
            );
        }
    };
    if canopy {
        obj.push_str("usemtl material_0\n");
    }
    write_box(&mut obj, 0);
    if canopy {
        obj.push_str("usemtl material_1\n");
        write_box(&mut obj, 8);
    }
    obj
}

/// Two named materials, so the OBJ's two `usemtl` runs import as two submeshes.
fn trunk_mtl() -> String {
    "newmtl material_0\nKd 0.55 0.45 0.35\nnewmtl material_1\nKd 0.85 0.55 0.20\n".to_owned()
}

/// One glTF primitive: interleaved-free position/normal/UV streams plus its index run.
struct Primitive {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    material: usize,
}

impl Primitive {
    fn new(material: usize) -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            material,
        }
    }

    /// Appends a triangle strip over `points`, flipping the odd triangles' winding so every face
    /// keeps `normal` as its front. A strip shares two vertices per triangle, which is what lets
    /// a needle spray stay inside one cluster's 64-vertex budget.
    fn strip(&mut self, points: &[[f32; 3]], normal: [f32; 3]) {
        let base = self.positions.len() as u32;
        let last = (points.len() - 1) as f32;
        for (index, point) in points.iter().enumerate() {
            self.positions.push(*point);
            self.normals.push(normal);
            self.uvs.push([index as f32 / last, (index % 2) as f32]);
        }
        for index in 0..(points.len() as u32 - 2) {
            if index % 2 == 0 {
                self.indices
                    .extend([base + index, base + index + 1, base + index + 2]);
            } else {
                self.indices
                    .extend([base + index, base + index + 2, base + index + 1]);
            }
        }
    }

    /// Appends one quad as two triangles, with a full 0..1 UV square so an alpha cutout maps
    /// once across the face.
    fn quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend(corners);
        self.normals.extend([normal; 4]);
        self.uvs
            .extend([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// A box from the origin, per-face normals, counter-clockwise seen from outside.
fn trunk_primitive(half: f32, height: f32, material: usize) -> Primitive {
    let mut primitive = Primitive::new(material);
    let (x, y, z) = (half, height, half);
    primitive.quad(
        [[-x, 0.0, -z], [-x, y, -z], [x, y, -z], [x, 0.0, -z]],
        [0.0, 0.0, -1.0],
    );
    primitive.quad(
        [[-x, 0.0, z], [x, 0.0, z], [x, y, z], [-x, y, z]],
        [0.0, 0.0, 1.0],
    );
    primitive.quad(
        [[-x, 0.0, -z], [x, 0.0, -z], [x, 0.0, z], [-x, 0.0, z]],
        [0.0, -1.0, 0.0],
    );
    primitive.quad(
        [[-x, y, -z], [-x, y, z], [x, y, z], [x, y, -z]],
        [0.0, 1.0, 0.0],
    );
    primitive.quad(
        [[-x, 0.0, -z], [-x, 0.0, z], [-x, y, z], [-x, y, -z]],
        [-1.0, 0.0, 0.0],
    );
    primitive.quad(
        [[x, 0.0, -z], [x, y, -z], [x, y, z], [x, 0.0, z]],
        [1.0, 0.0, 0.0],
    );
    primitive
}

/// The foliage primitive over the trunk top. The two broad-leaf contents share one blade layout,
/// so the only difference between them is the material's alpha cutout; the conifer content
/// replaces the blades with a shared-vertex spray of slivers a twentieth as wide.
fn foliage_primitive(content: PlantContent, trunk_height: f32, material: usize) -> Primitive {
    let mut primitive = Primitive::new(material);
    if content == PlantContent::ConiferNeedle {
        return needle_spray(primitive, trunk_height);
    }
    const COLUMNS: i32 = 4;
    const ROWS: i32 = 3;
    let (half_length, half_width, spacing) = (0.25_f32, 0.175_f32, 0.6_f32);
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let index = row * COLUMNS + column;
            let x = (column as f32 - (COLUMNS - 1) as f32 / 2.0) * spacing;
            let z = (row as f32 - (ROWS - 1) as f32 / 2.0) * spacing;
            // A tier per index keeps the blades off one plane: coplanar duplicates give the
            // hierarchy simplifier no error to order its cut by.
            let y = trunk_height - 0.9 + (index % 7) as f32 * 0.15;
            primitive.quad(
                [
                    [x - half_length, y, z - half_width],
                    [x - half_length, y, z + half_width],
                    [x + half_length, y, z + half_width],
                    [x + half_length, y, z - half_width],
                ],
                [0.0, 1.0, 0.0],
            );
        }
    }
    primitive
}

/// The conifer spray: two triangle strips whose alternating points make every face a needle
/// a centimetre across and a fifth of a metre long. Strips rather than loose quads because a
/// cluster holds 64 vertices, and a family whose submesh needs a second cluster does not cook.
fn needle_spray(mut primitive: Primitive, trunk_height: f32) -> Primitive {
    const POINTS: i32 = 31;
    let (half_width, step) = (0.012_f32, 0.09_f32);
    for (strip, offset) in [(0_i32, -0.45_f32), (1, 0.45)] {
        let points = (0..POINTS)
            .map(|index| {
                let x = (index as f32 - (POINTS - 1) as f32 / 2.0) * step;
                let z = offset
                    + if index % 2 == 0 {
                        -half_width
                    } else {
                        half_width
                    };
                let y = trunk_height - 0.9 + ((index + strip) % 5) as f32 * 0.05;
                [x, y, z]
            })
            .collect::<Vec<_>>();
        primitive.strip(&points, [0.0, 1.0, 0.0]);
    }
    primitive
}

/// The serration cutout: an opaque blade silhouette that tapers along the blade and is cut back
/// by regular teeth, with everything outside it fully transparent. The blade's remaining detail
/// is the alpha channel's, not the mesh's.
fn serration_png() -> Result<Vec<u8>> {
    const EXTENT: u32 = 64;
    let mut image = image::RgbaImage::new(EXTENT, EXTENT);
    let extent = EXTENT as f32;
    for y in 0..EXTENT {
        let taper = 1.0 - y as f32 / extent;
        let tooth = (y % 8) as f32 / 8.0;
        let half_width = (extent / 2.0) * (0.35 + 0.45 * taper) * (0.7 + 0.3 * tooth);
        for x in 0..EXTENT {
            let offset = (x as f32 - extent / 2.0).abs();
            let alpha = if offset <= half_width { 255 } else { 0 };
            image.put_pixel(x, y, image::Rgba([61, 115, 41, alpha]));
        }
    }
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// The leaf-content source set: one glTF whose single node carries the trunk and the foliage as
/// two primitives over two materials, its binary buffer, and — for the masked content — the
/// cutout the leaf material reads its coverage from.
fn leaf_gltf(content: PlantContent, stem: &str, trunk_height: i32) -> Result<Vec<SourceFile>> {
    let names = content.material_names();
    let primitives = [
        trunk_primitive(0.5, trunk_height as f32, 0),
        foliage_primitive(content, trunk_height as f32, 1),
    ];
    let masked = content == PlantContent::MaskedSerration;
    let mut leaf = json!({
        "name": names[1],
        "doubleSided": true,
        "pbrMetallicRoughness": {
            "baseColorFactor": [0.24, 0.45, 0.16, 1.0],
            "metallicFactor": 0.0,
            "roughnessFactor": 0.7
        }
    });
    if masked {
        leaf["alphaMode"] = json!("MASK");
        leaf["alphaCutoff"] = json!(0.5);
        leaf["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": 0 });
    }
    let materials = json!([
        {
            "name": names[0],
            "doubleSided": true,
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.35, 0.27, 0.2, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 0.9
            }
        },
        leaf
    ]);

    let mut buffer = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut prims = Vec::new();
    for primitive in &primitives {
        let position = push_f32_accessor(
            &mut buffer,
            &mut views,
            &mut accessors,
            primitive.positions.iter().flatten().copied(),
            "VEC3",
            primitive.positions.len(),
            true,
        );
        let normal = push_f32_accessor(
            &mut buffer,
            &mut views,
            &mut accessors,
            primitive.normals.iter().flatten().copied(),
            "VEC3",
            primitive.normals.len(),
            false,
        );
        let uv = push_f32_accessor(
            &mut buffer,
            &mut views,
            &mut accessors,
            primitive.uvs.iter().flatten().copied(),
            "VEC2",
            primitive.uvs.len(),
            false,
        );
        let view = push_view(&mut buffer, &mut views, |buffer| {
            for index in &primitive.indices {
                buffer.extend_from_slice(&index.to_le_bytes());
            }
        });
        let indices = accessors.len();
        accessors.push(json!({
            "bufferView": view,
            "componentType": 5125,
            "count": primitive.indices.len(),
            "type": "SCALAR"
        }));
        prims.push(json!({
            "attributes": { "POSITION": position, "NORMAL": normal, "TEXCOORD_0": uv },
            "indices": indices,
            "material": primitive.material
        }));
    }

    let mut document = json!({
        "asset": { "version": "2.0", "generator": "saffron-anima xtask" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "name": stem, "mesh": 0 }],
        "meshes": [{ "name": stem, "primitives": prims }],
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "uri": format!("{stem}.bin"), "byteLength": buffer.len() }]
    });
    if masked {
        document["images"] = json!([{ "uri": format!("{stem}.png") }]);
        document["textures"] = json!([{ "source": 0 }]);
    }
    let mut files = vec![
        SourceFile {
            path: format!("models/{stem}.gltf"),
            bytes: serde_json::to_vec_pretty(&document)?,
        },
        SourceFile {
            path: format!("models/{stem}.bin"),
            bytes: buffer,
        },
    ];
    if masked {
        files.push(SourceFile {
            path: format!("models/{stem}.png"),
            bytes: serration_png()?,
        });
    }
    Ok(files)
}

/// Appends one buffer view over bytes `write` emits, returning its index.
fn push_view(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    write: impl FnOnce(&mut Vec<u8>),
) -> usize {
    let offset = buffer.len();
    write(buffer);
    views.push(json!({
        "buffer": 0,
        "byteOffset": offset,
        "byteLength": buffer.len() - offset
    }));
    views.len() - 1
}

/// Appends a float stream plus the accessor over it, returning the accessor index. glTF requires
/// `min`/`max` on a POSITION accessor, so a component-wise bound rides the position streams.
fn push_f32_accessor(
    buffer: &mut Vec<u8>,
    views: &mut Vec<Value>,
    accessors: &mut Vec<Value>,
    values: impl Iterator<Item = f32>,
    kind: &str,
    count: usize,
    bounded: bool,
) -> usize {
    let components = if kind == "VEC3" { 3 } else { 2 };
    let values = values.collect::<Vec<_>>();
    let view = push_view(buffer, views, |buffer| {
        for value in &values {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
    });
    let mut accessor = json!({
        "bufferView": view,
        "componentType": 5126,
        "count": count,
        "type": kind
    });
    if bounded {
        let mut minimum = vec![f32::MAX; components];
        let mut maximum = vec![f32::MIN; components];
        for component in values.chunks_exact(components) {
            for axis in 0..components {
                minimum[axis] = minimum[axis].min(component[axis]);
                maximum[axis] = maximum[axis].max(component[axis]);
            }
        }
        accessor["min"] = json!(minimum);
        accessor["max"] = json!(maximum);
    }
    accessors.push(accessor);
    accessors.len() - 1
}
