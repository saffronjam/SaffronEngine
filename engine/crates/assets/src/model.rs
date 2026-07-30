//! The `.smodel` container metadata record and the opened-model handle.
//!
//! [`ContainerMetadata`] is the parsed META chunk of a `.smodel` — the header-first
//! record a catalog scan and a deterministic reimport need without touching the
//! payload bytes. [`encode_container_metadata`] / [`read_container_metadata`] are its
//! deterministic codec: object keys serialize in a stable (sorted) order, so the bytes
//! are reproducible for source hashing and the contract test.
//!
//! [`ModelAsset`] is an opened container (`{meta, reader}`), negative-cached in
//! `AssetServer::model_by_uuid` by model id. [`ByteSource`] is the file-or-chunk-slice
//! reader that lets the loaders treat a standalone file and an embedded chunk
//! identically; [`AssetServer::chunk_source_for`] resolves a sub-id to one with the
//! remap-wins / embedded-fallback order.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, ContainerReader, ImportedNode, MeshCounts, SModelHeader, glam::Mat4,
    mesh_counts_from_bytes, mesh_file_counts, read_container, read_container_header,
};
use saffron_json::{
    Value, dump_json_sorted, json_f32_or, json_string_or, json_u64_or, parse_json, uuid_to_json,
};
use saffron_scene::{AssetEntry, AssetType};

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::names::{asset_type_from_name, asset_type_name};

/// Composes one validated imported node forest into source-root transforms.
pub(crate) fn imported_node_world_transforms(nodes: &[ImportedNode]) -> Result<Vec<Mat4>> {
    let locals = nodes
        .iter()
        .map(|node| {
            let transform =
                Mat4::from_scale_rotation_translation(node.scale, node.rotation, node.translation);
            if transform.is_finite() {
                Ok(transform)
            } else {
                Err(Error::Io("model node transform is non-finite".to_owned()))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let mut world = Vec::with_capacity(nodes.len());
    for index in 0..nodes.len() {
        let mut transform = locals[index];
        let mut parent = nodes[index].parent;
        let mut active = std::collections::BTreeSet::new();
        active.insert(index);
        while parent >= 0 {
            let parent_index = usize::try_from(parent)
                .map_err(|_| Error::Io("model node parent is invalid".to_owned()))?;
            let parent_transform = locals
                .get(parent_index)
                .ok_or_else(|| Error::Io("model node parent is out of range".to_owned()))?;
            if !active.insert(parent_index) {
                return Err(Error::Io(
                    "model node hierarchy contains a cycle".to_owned(),
                ));
            }
            transform = *parent_transform * transform;
            parent = nodes[parent_index].parent;
        }
        if parent != -1 {
            return Err(Error::Io("model node parent is invalid".to_owned()));
        }
        world.push(transform);
    }
    Ok(world)
}

/// The metadata-chunk schema version this build writes and accepts. Forward
/// compatible: a reader ignores unknown keys, so a v1 reader survives a later schema
/// growing new fields.
pub const METADATA_SCHEMA_VERSION: u32 = 2;

/// The reimport recipe baked into a container's META chunk.
///
/// Carries the deterministic provenance a reimport needs: the project-relative source
/// path, the *content* hash of the source bytes (not its mtime), the importer version,
/// and the original import options stored verbatim as opaque JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// Project-relative source file path.
    pub source_path: String,
    /// Content hash of the source bytes (not the mtime).
    pub source_hash: String,
    /// The importer version that produced the container.
    pub importer_version: u32,
    /// The `ImportOptions` JSON, stored verbatim.
    pub options: Value,
}

impl Default for Import {
    fn default() -> Self {
        Self {
            source_path: String::new(),
            source_hash: String::new(),
            importer_version: 1,
            options: Value::Null,
        }
    }
}

/// One embedded sub-asset's META record: its stable sub-id, kind, name, the TOC chunk
/// index it lives at, plus type-specific extras (texture colorspace, animation
/// duration / track count).
#[derive(Clone, Debug, PartialEq)]
pub struct SubAsset {
    /// The stable sub-asset id.
    pub sub_id: Uuid,
    /// The sub-asset kind.
    pub asset_type: AssetType,
    /// The sub-asset name.
    pub name: String,
    /// The TOC chunk index inside the container.
    pub chunk: u32,
    /// Texture: `srgb` | `linear` | `hdr` | `auto` (empty otherwise).
    pub colorspace: String,
    /// Animation: clip length in seconds.
    pub duration: f32,
    /// Animation: animated joint-channel count.
    pub tracks: i32,
    /// FNV-1a hash of this sub-asset's baked chunk bytes, the content-addressed thumbnail
    /// cache key; `0` for kinds that carry no thumbnail (animation).
    pub content_hash: u64,
}

impl Default for SubAsset {
    fn default() -> Self {
        Self {
            sub_id: Uuid(0),
            asset_type: AssetType::Mesh,
            name: String::new(),
            chunk: 0,
            colorspace: String::new(),
            duration: 0.0,
            tracks: 0,
            content_hash: 0,
        }
    }
}

/// The parsed META chunk of a `.smodel`.
///
/// The header-first record a catalog scan and a deterministic reimport need without
/// touching payloads. On disk, uuids are decimal strings; the `materials` / `nodes` /
/// `skin` / `remap` blocks stay as opaque JSON (parsed on demand by instantiate /
/// reimport in later phases).
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerMetadata {
    /// The metadata schema version.
    pub schema: u32,
    /// The model's id.
    pub model_id: Uuid,
    /// The model's name.
    pub name: String,
    /// The source format (`gltf` | `obj`).
    pub source_format: String,
    /// The reimport recipe.
    pub import: Import,
    /// The embedded sub-assets.
    pub sub_assets: Vec<SubAsset>,
    /// Per-material flat factors, opaque JSON array (`[{subId, baseColor, ...}]`).
    pub materials: Value,
    /// The glTF nodes block (index-referenced), opaque JSON array.
    pub nodes: Value,
    /// The skin descriptor, or `null` when unskinned.
    pub skin: Value,
    /// The morph block `{targetNames, targetCount, restWeights}`, or `null` when the
    /// model has no blend shapes.
    pub morph: Value,
    /// The extract/remap table `{subId: {external: relPath}}`, opaque JSON object.
    pub remap: Value,
}

impl Default for ContainerMetadata {
    fn default() -> Self {
        Self {
            schema: METADATA_SCHEMA_VERSION,
            model_id: Uuid(0),
            name: String::new(),
            source_format: String::new(),
            import: Import::default(),
            sub_assets: Vec::new(),
            materials: Value::Array(Vec::new()),
            nodes: Value::Array(Vec::new()),
            skin: Value::Null,
            morph: Value::Null,
            remap: Value::Object(serde_json::Map::new()),
        }
    }
}

/// Returns `value` if it is not JSON `null`, else `fallback`.
fn or_default(value: &Value, fallback: Value) -> Value {
    if value.is_null() {
        fallback
    } else {
        value.clone()
    }
}

/// Builds the META-chunk bytes from a populated [`ContainerMetadata`].
///
/// Object keys serialize in a stable (sorted) order via [`dump_json_sorted`], so the bytes
/// are deterministic for source hashing and the contract test. The output is compact (no
/// indent).
#[must_use]
pub fn encode_container_metadata(meta: &ContainerMetadata) -> Vec<u8> {
    let mut subs = Vec::with_capacity(meta.sub_assets.len());
    for sub in &meta.sub_assets {
        let mut record = serde_json::Map::new();
        record.insert("subId".to_owned(), uuid_to_json(sub.sub_id.value()));
        record.insert(
            "type".to_owned(),
            Value::String(asset_type_name(sub.asset_type).to_owned()),
        );
        record.insert("name".to_owned(), Value::String(sub.name.clone()));
        record.insert("chunk".to_owned(), Value::from(sub.chunk));
        if !sub.colorspace.is_empty() {
            record.insert(
                "colorspace".to_owned(),
                Value::String(sub.colorspace.clone()),
            );
        }
        if sub.asset_type == AssetType::Animation {
            record.insert("duration".to_owned(), Value::from(sub.duration));
            record.insert("tracks".to_owned(), Value::from(sub.tracks));
        }
        if sub.content_hash != 0 {
            record.insert("contentHash".to_owned(), Value::from(sub.content_hash));
        }
        subs.push(Value::Object(record));
    }

    let options = or_default(&meta.import.options, Value::Object(serde_json::Map::new()));

    let doc = serde_json::json!({
        "schema": meta.schema,
        "model": {
            "id": uuid_to_json(meta.model_id.value()),
            "name": meta.name,
            "sourceFormat": meta.source_format,
        },
        "import": {
            "sourcePath": meta.import.source_path,
            "sourceHash": meta.import.source_hash,
            "importerVersion": meta.import.importer_version,
            "options": options,
        },
        "subAssets": Value::Array(subs),
        "materials": or_default(&meta.materials, Value::Array(Vec::new())),
        "nodes": or_default(&meta.nodes, Value::Array(Vec::new())),
        "remap": or_default(&meta.remap, Value::Object(serde_json::Map::new())),
    });

    // `skin` and `morph` are emitted only when present; a null key is omitted entirely.
    let mut map = doc.as_object().cloned().unwrap_or_default();
    if !meta.skin.is_null() {
        map.insert("skin".to_owned(), meta.skin.clone());
    }
    if !meta.morph.is_null() {
        map.insert("morph".to_owned(), meta.morph.clone());
    }
    let doc = Value::Object(map);

    dump_json_sorted(&doc, -1).into_bytes()
}

/// Prefix-reads only the 64-byte header + the META chunk of a `.smodel`, touching no
/// payload bytes.
///
/// Forward compatible (unknown keys are ignored) so a v1 reader survives a later schema
/// growing new fields. Rejects a container with no META chunk, or whose META span lies
/// outside the file.
///
/// # Errors
///
/// [`Error::Geometry`] if the header is unreadable / invalid; [`Error::Io`] for the
/// META read; [`Error::Json`] if the META chunk is not valid JSON; [`Error::Io`] with a
/// descriptive message if the META chunk is absent or out of bounds.
pub fn read_container_metadata(path: impl AsRef<Path>) -> Result<ContainerMetadata> {
    let path = path.as_ref();
    let header: SModelHeader = read_container_header(path)?;

    if header.meta_length == 0 {
        return Err(Error::Io(format!(
            "'{}' has no metadata chunk",
            path.display()
        )));
    }
    if header.meta_offset < size_of::<SModelHeader>() as u64
        || header.meta_offset + header.meta_length > header.total_length
    {
        return Err(Error::Io(format!(
            "'{}' metadata chunk is out of bounds",
            path.display()
        )));
    }

    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    let start = header.meta_offset as usize;
    let end = start + header.meta_length as usize;
    let slice = bytes.get(start..end).ok_or_else(|| {
        Error::Io(format!(
            "'{}' metadata chunk read past end of file",
            path.display()
        ))
    })?;
    let text = std::str::from_utf8(slice)
        .map_err(|e| Error::Io(format!("'{}' metadata is not UTF-8: {e}", path.display())))?;
    let doc = parse_json(text)?;
    if !doc.is_object() {
        return Err(Error::Io(format!(
            "'{}' has an invalid metadata chunk",
            path.display()
        )));
    }

    Ok(metadata_from_doc(&doc))
}

/// Decodes a parsed META JSON object into a [`ContainerMetadata`], reading lenient
/// fallbacks for every field (forward compatible: unknown keys are ignored, missing
/// keys default).
fn metadata_from_doc(doc: &Value) -> ContainerMetadata {
    let mut meta = ContainerMetadata {
        schema: json_u64_or(doc, "schema", u64::from(METADATA_SCHEMA_VERSION)) as u32,
        ..ContainerMetadata::default()
    };

    if let Some(model) = doc.get("model").filter(|v| v.is_object()) {
        meta.model_id = Uuid(json_u64_or(model, "id", 0));
        meta.name = json_string_or(model, "name", String::new());
        meta.source_format = json_string_or(model, "sourceFormat", String::new());
    }
    if let Some(import) = doc.get("import").filter(|v| v.is_object()) {
        meta.import.source_path = json_string_or(import, "sourcePath", String::new());
        meta.import.source_hash = json_string_or(import, "sourceHash", String::new());
        meta.import.importer_version = json_u64_or(import, "importerVersion", 1) as u32;
        meta.import.options = import
            .get("options")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    }
    if let Some(records) = doc.get("subAssets").and_then(Value::as_array) {
        for record in records {
            if !record.is_object() {
                continue;
            }
            meta.sub_assets.push(SubAsset {
                sub_id: Uuid(json_u64_or(record, "subId", 0)),
                asset_type: asset_type_from_name(&json_string_or(
                    record,
                    "type",
                    "mesh".to_owned(),
                )),
                name: json_string_or(record, "name", String::new()),
                chunk: json_u64_or(record, "chunk", 0) as u32,
                colorspace: json_string_or(record, "colorspace", String::new()),
                duration: json_f32_or(record, "duration", 0.0),
                tracks: json_u64_or(record, "tracks", 0) as i32,
                content_hash: json_u64_or(record, "contentHash", 0),
            });
        }
    }
    meta.materials = doc
        .get("materials")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    meta.nodes = doc
        .get("nodes")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    meta.skin = doc.get("skin").cloned().unwrap_or(Value::Null);
    meta.morph = doc.get("morph").cloned().unwrap_or(Value::Null);
    meta.remap = doc
        .get("remap")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    meta
}

/// A whole file, or a byte slice of one.
///
/// `length == 0` means the whole file; a non-zero `length` reads exactly
/// `[offset, offset + length)` — a `.smodel` chunk. This lets the loaders treat a
/// standalone file and an embedded chunk identically. A plain value type, not an
/// `Arc`: an empty `path` means the sub-id resolves to no source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ByteSource {
    /// The file to read from; empty means "no source".
    pub path: String,
    /// The byte offset of the slice (`0` for a whole file).
    pub offset: u64,
    /// The slice length (`0` means the whole file).
    pub length: u64,
}

impl ByteSource {
    /// Reads the source's bytes: the whole file when `length == 0`, else the
    /// `[offset, offset + length)` slice.
    ///
    /// Seeks to the slice and reads exactly its bytes rather than reading the whole file —
    /// a `.smodel` chunk read must not pull a 50 MB container into memory to hand back one
    /// mesh (the thumbnail preview reads many chunks of one container).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be opened/read, or the requested slice exceeds the
    /// file's size.
    pub fn read(&self) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = fs::File::open(&self.path)
            .map_err(|e| Error::Io(format!("cannot open '{}': {e}", self.path)))?;
        let file_size = file
            .metadata()
            .map_err(|e| Error::Io(format!("cannot stat '{}': {e}", self.path)))?
            .len();
        let (begin, count) = if self.length == 0 {
            (0u64, file_size)
        } else {
            (self.offset, self.length)
        };
        if begin + count > file_size {
            return Err(Error::Io(format!(
                "slice [{begin}, {}) exceeds '{}' ({file_size} bytes)",
                begin + count,
                self.path
            )));
        }
        if begin != 0 {
            file.seek(SeekFrom::Start(begin))
                .map_err(|e| Error::Io(format!("cannot seek '{}': {e}", self.path)))?;
        }
        let mut buf = vec![0u8; count as usize];
        file.read_exact(&mut buf)
            .map_err(|e| Error::Io(format!("cannot read '{}': {e}", self.path)))?;
        Ok(buf)
    }

    /// Whether this source resolves to nothing (an empty path).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }
}

/// An opened `.smodel`: its prefix metadata plus a chunk reader that slices payloads
/// lazily.
///
/// Negative-cached in `AssetServer::model_by_uuid` like a mesh; dropped on a project
/// switch with the GPU idle.
#[derive(Clone, Debug)]
pub struct ModelAsset {
    /// The parsed container metadata (the prefix-read META chunk).
    pub meta: ContainerMetadata,
    /// The chunk reader over the container's TOC.
    pub reader: ContainerReader,
}

impl AssetServer {
    /// Opens (or returns the cached) `.smodel` container for `model_id`.
    ///
    /// Negative-cached in `model_by_uuid`: a container that fails to open inserts
    /// `None` so it is not re-read every frame. A missing catalog row, a non-`Model`
    /// catalog entry, an unparseable META chunk, or an invalid container all
    /// negative-cache and return `None` (a logged warn, not an `Err`).
    pub fn load_model_asset(&mut self, model_id: Uuid) -> Option<Arc<ModelAsset>> {
        if let Some(cached) = self.model_by_uuid.get(&model_id.value()) {
            return cached.clone();
        }

        let opened = self.open_container(model_id);
        self.model_by_uuid.insert(model_id.value(), opened.clone());
        opened
    }

    /// Reads + validates the container at `container_id`'s catalog row, or returns `None` (with a
    /// warn) on any failure; the caller caches the outcome. A container parent is a `.smodel`
    /// **Model** or a texture-embedding **Material** — both open identically, and no other asset
    /// type is a container.
    fn open_container(&self, container_id: Uuid) -> Option<Arc<ModelAsset>> {
        let entry = self.catalog.find(container_id)?;
        if !matches!(entry.asset_type, AssetType::Model | AssetType::Material) {
            return None;
        }
        let full_path = format!("{}/{}", self.root.display(), entry.path);
        let meta = match read_container_metadata(&full_path) {
            Ok(meta) => meta,
            Err(err) => {
                tracing::warn!("container {}: {err}", container_id.value());
                return None;
            }
        };
        let reader = match read_container(&full_path) {
            Ok(reader) => reader,
            Err(err) => {
                tracing::warn!("container {}: {err}", container_id.value());
                return None;
            }
        };
        Some(Arc::new(ModelAsset { meta, reader }))
    }

    /// Resolves a sub-id to the [`ByteSource`] its chunk's bytes live in.
    ///
    /// Resolution order: a remap entry (an extracted/external file) wins, falling back
    /// to the embedded chunk with a warn if the external file is gone; otherwise the
    /// embedded chunk. An empty-path [`ByteSource`] means the sub-id has no such chunk.
    #[must_use]
    pub fn chunk_source_for(
        &self,
        model: &ModelAsset,
        kind: ChunkKind,
        sub_id: Uuid,
    ) -> ByteSource {
        let key = sub_id.value().to_string();
        if let Some(remap) = model.meta.remap.as_object().and_then(|m| m.get(&key))
            && let Some(external) = remap.get("external").and_then(Value::as_str)
        {
            let external_path = format!("{}/{}", self.root.display(), external);
            if Path::new(&external_path).exists() {
                return ByteSource {
                    path: external_path,
                    ..ByteSource::default()
                };
            }
            tracing::warn!(
                "model {}: remap target '{external}' for sub-asset {} is missing; using the embedded chunk",
                model.meta.model_id.value(),
                sub_id.value()
            );
        }
        match model.reader.find(kind, sub_id.value()) {
            Some(entry) => ByteSource {
                path: model.reader.path().display().to_string(),
                offset: entry.offset,
                length: entry.length,
            },
            None => ByteSource::default(),
        }
    }

    /// Vertex / index counts for a mesh asset — a standalone `.smesh`, or a mesh chunk
    /// inside a `.smodel`.
    ///
    /// A standalone entry (`container == 0`) reads its `.smesh` header directly; a
    /// sub-asset opens the container, slices its mesh chunk via [`Self::chunk_source_for`],
    /// and reads the counts from the sliced bytes — proving the chunk-slice path.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the container cannot be opened or the mesh sub-asset is absent;
    /// [`Error::Geometry`] if the mesh bytes are malformed.
    pub fn mesh_counts_for_asset(&mut self, entry: &AssetEntry) -> Result<MeshCounts> {
        if entry.container.value() == 0 {
            let full_path = format!("{}/{}", self.root.display(), entry.path);
            return Ok(mesh_file_counts(&full_path)?);
        }
        let container = entry.container;
        let model = self.load_model_asset(container).ok_or_else(|| {
            Error::Io(format!(
                "model {}: cannot open container",
                container.value()
            ))
        })?;
        let source = self.chunk_source_for(&model, ChunkKind::Mesh, entry.id);
        if source.is_empty() {
            return Err(Error::Io(format!(
                "model {}: no mesh sub-asset {}",
                container.value(),
                entry.id.value()
            )));
        }
        let bytes = source.read()?;
        Ok(mesh_counts_from_bytes(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{
        ContainerChunk, Mesh, Submesh, Vertex, save_mesh_to_buffer, write_container,
    };
    use saffron_scene::Colorspace;
    use std::path::PathBuf;

    /// A unique scratch dir under the system temp, removed and recreated per test.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("saffron-assets-model-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fully-populated `ContainerMetadata` exercising every field the round-trip and
    /// golden tests assert.
    fn sample_metadata() -> ContainerMetadata {
        let mut remap = serde_json::Map::new();
        remap.insert(
            "12".to_owned(),
            serde_json::json!({ "external": "textures/town_albedo.png" }),
        );
        ContainerMetadata {
            schema: METADATA_SCHEMA_VERSION,
            model_id: Uuid(4242),
            name: "town".to_owned(),
            source_format: "gltf".to_owned(),
            import: Import {
                source_path: "raw/town.glb".to_owned(),
                source_hash: "abc123".to_owned(),
                importer_version: 1,
                options: serde_json::json!({ "scale": 1.0, "axis": "y-up" }),
            },
            sub_assets: vec![
                SubAsset {
                    sub_id: Uuid(11),
                    asset_type: AssetType::Mesh,
                    name: "town_mesh".to_owned(),
                    chunk: 1,
                    content_hash: 0x1111_2222_3333_4444,
                    ..SubAsset::default()
                },
                SubAsset {
                    sub_id: Uuid(12),
                    asset_type: AssetType::Texture,
                    name: "town_albedo".to_owned(),
                    chunk: 2,
                    colorspace: "srgb".to_owned(),
                    content_hash: 0x5555_6666_7777_8888,
                    ..SubAsset::default()
                },
                SubAsset {
                    sub_id: Uuid(13),
                    asset_type: AssetType::Material,
                    name: "stone".to_owned(),
                    chunk: 3,
                    content_hash: 0x9999_aaaa_bbbb_cccc,
                    ..SubAsset::default()
                },
                SubAsset {
                    sub_id: Uuid(14),
                    asset_type: AssetType::Animation,
                    name: "walk".to_owned(),
                    chunk: 4,
                    duration: 1.2,
                    tracks: 7,
                    ..SubAsset::default()
                },
            ],
            materials: serde_json::json!([{
                "subId": "13",
                "baseColor": [1.0, 1.0, 1.0, 1.0],
                "metallic": 0.0,
                "roughness": 1.0,
            }]),
            nodes: serde_json::json!([{ "name": "root", "parent": -1, "mesh": 0 }]),
            skin: serde_json::json!({ "joints": [0], "skeletonRoot": 0, "meshNode": 1 }),
            morph: Value::Null,
            remap: Value::Object(remap),
        }
    }

    /// A single-triangle `.smesh` byte image (3 verts, 3 indices) for the mesh-chunk
    /// counts test.
    fn sample_mesh_bytes() -> Vec<u8> {
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(0.0, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, 0.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
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
        };
        save_mesh_to_buffer(&mesh, &[], None).unwrap()
    }

    #[test]
    fn metadata_round_trips_every_field() {
        let dir = scratch("roundtrip");
        let meta = sample_metadata();
        let meta_bytes = encode_container_metadata(&meta);
        let payload = vec![0x5Au8; 8192];
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 11,
                flags: 0,
                bytes: &payload,
            },
        ];
        let path = dir.join("town.smodel");
        write_container(&path, &chunks).unwrap();

        let read = read_container_metadata(&path).unwrap();
        assert_eq!(read.schema, meta.schema);
        assert_eq!(read.model_id, meta.model_id);
        assert_eq!(read.name, meta.name);
        assert_eq!(read.source_format, meta.source_format);
        assert_eq!(read.import, meta.import);
        assert_eq!(read.sub_assets, meta.sub_assets);
        assert_eq!(read.materials, meta.materials);
        assert_eq!(read.nodes, meta.nodes);
        assert_eq!(read.skin, meta.skin);
        assert_eq!(read.remap, meta.remap);
        assert_eq!(read, meta);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn animation_extras_only_serialize_for_animation_subassets() {
        let meta = sample_metadata();
        let bytes = encode_container_metadata(&meta);
        let text = std::str::from_utf8(&bytes).unwrap();
        let doc = parse_json(text).unwrap();
        let subs = doc.get("subAssets").and_then(Value::as_array).unwrap();
        assert!(subs[0].get("duration").is_none());
        assert!(subs[0].get("tracks").is_none());
        assert_eq!(
            subs[1].get("colorspace").and_then(Value::as_str),
            Some("srgb")
        );
        assert!(subs[1].get("duration").is_none());
        assert!(subs[3].get("duration").is_some());
        assert_eq!(subs[3].get("tracks").and_then(Value::as_i64), Some(7));
    }

    #[test]
    fn meta_encoding_is_deterministic_sorted_keys() {
        // The same metadata encodes to byte-identical output across calls (the source
        // hash depends on this), and the top-level keys are in sorted order.
        let meta = sample_metadata();
        let a = encode_container_metadata(&meta);
        let b = encode_container_metadata(&meta);
        assert_eq!(a, b, "encoding must be deterministic for source hashing");

        let text = std::str::from_utf8(&a).unwrap();
        // `dump_json_sorted` emits object keys lexicographically, so the top-level block
        // order is fixed: import < materials < model < nodes < remap < schema < skin <
        // subAssets.
        let import_pos = text.find("\"import\"").unwrap();
        let materials_pos = text.find("\"materials\"").unwrap();
        let model_pos = text.find("\"model\"").unwrap();
        let nodes_pos = text.find("\"nodes\"").unwrap();
        let remap_pos = text.find("\"remap\"").unwrap();
        let schema_pos = text.find("\"schema\"").unwrap();
        let skin_pos = text.find("\"skin\"").unwrap();
        let subs_pos = text.find("\"subAssets\"").unwrap();
        assert!(
            import_pos < materials_pos
                && materials_pos < model_pos
                && model_pos < nodes_pos
                && nodes_pos < remap_pos
                && remap_pos < schema_pos
                && schema_pos < skin_pos
                && skin_pos < subs_pos,
            "top-level META keys must be sorted: {text}"
        );
    }

    #[test]
    fn golden_meta_bytes_are_frozen() {
        // A drift in these bytes means the source hash sees a different container for identical
        // input. Sub-id `12` is a texture (carries `colorspace`); `13` is a mesh (no colorspace /
        // duration / tracks).
        let mut meta = ContainerMetadata {
            schema: 1,
            model_id: Uuid(7),
            name: "m".to_owned(),
            source_format: "obj".to_owned(),
            import: Import {
                source_path: "raw/m.obj".to_owned(),
                source_hash: "h".to_owned(),
                importer_version: 1,
                options: Value::Object(serde_json::Map::new()),
            },
            sub_assets: vec![
                SubAsset {
                    sub_id: Uuid(12),
                    asset_type: AssetType::Texture,
                    name: "t".to_owned(),
                    chunk: 1,
                    colorspace: "srgb".to_owned(),
                    ..SubAsset::default()
                },
                SubAsset {
                    sub_id: Uuid(13),
                    asset_type: AssetType::Mesh,
                    name: "g".to_owned(),
                    chunk: 2,
                    ..SubAsset::default()
                },
            ],
            ..ContainerMetadata::default()
        };
        meta.skin = Value::Null;

        let bytes = encode_container_metadata(&meta);
        let text = std::str::from_utf8(&bytes).unwrap();
        let expected = concat!(
            "{",
            r#""import":{"importerVersion":1,"options":{},"sourceHash":"h","sourcePath":"raw/m.obj"},"#,
            r#""materials":[],"#,
            r#""model":{"id":"7","name":"m","sourceFormat":"obj"},"#,
            r#""nodes":[],"#,
            r#""remap":{},"#,
            r#""schema":1,"#,
            r#""subAssets":[{"chunk":1,"colorspace":"srgb","name":"t","subId":"12","type":"texture"},"#,
            r#"{"chunk":2,"name":"g","subId":"13","type":"mesh"}]"#,
            "}",
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn meta_encoding_is_compact_not_pretty() {
        let bytes = encode_container_metadata(&sample_metadata());
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains('\n'), "META encoding must be compact");
    }

    #[test]
    fn unskinned_metadata_omits_the_skin_key() {
        // A null skin is omitted from the doc entirely, and reads back as null.
        let mut meta = sample_metadata();
        meta.skin = Value::Null;
        let bytes = encode_container_metadata(&meta);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("\"skin\""), "a null skin is not serialized");

        let dir = scratch("unskinned");
        let chunks = [ContainerChunk {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: &bytes,
        }];
        let path = dir.join("flat.smodel");
        write_container(&path, &chunks).unwrap();
        let read = read_container_metadata(&path).unwrap();
        assert!(read.skin.is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_meta_chunk_is_rejected() {
        let dir = scratch("nometa");
        let mesh_bytes = sample_mesh_bytes();
        let chunks = [ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: 11,
            flags: 0,
            bytes: &mesh_bytes,
        }];
        let path = dir.join("nometa.smodel");
        write_container(&path, &chunks).unwrap();
        assert!(matches!(read_container_metadata(&path), Err(Error::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_meta_length_is_rejected_not_crashed() {
        // A header claiming a META span larger than the file is rejected by the bounds check.
        let dir = scratch("badlen");
        let meta_bytes = encode_container_metadata(&sample_metadata());
        let chunks = [ContainerChunk {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: &meta_bytes,
        }];
        let path = dir.join("badlen.smodel");
        write_container(&path, &chunks).unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        {
            // Inflate meta_length to 4x the file — the header total-length check fires.
            let header: &mut SModelHeader =
                bytemuck::from_bytes_mut(&mut raw[..size_of::<SModelHeader>()]);
            header.meta_length = header.total_length * 4;
        }
        std::fs::write(&path, &raw).unwrap();
        assert!(read_container_metadata(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builds a `.smodel` at `<root>/models/<name>.smodel` from `meta` plus an embedded
    /// mesh chunk for sub-id 11, and registers a `Model` catalog row for it.
    fn write_model_fixture(assets: &mut AssetServer, meta: &ContainerMetadata, name: &str) -> Uuid {
        let meta_bytes = encode_container_metadata(meta);
        let mesh_bytes = sample_mesh_bytes();
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 11,
                flags: 0,
                bytes: &mesh_bytes,
            },
        ];
        let rel = format!("models/{name}.smodel");
        let full = format!("{}/{rel}", assets.root.display());
        write_container(&full, &chunks).unwrap();
        assets.catalog.put(AssetEntry {
            id: meta.model_id,
            name: name.to_owned(),
            asset_type: AssetType::Model,
            path: rel,
            container: Uuid(0),
            chunk: -1,
            ..AssetEntry::default()
        });
        meta.model_id
    }

    #[test]
    fn load_model_asset_opens_once_and_caches() {
        let dir = scratch("loadcache");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let meta = sample_metadata();
        let id = write_model_fixture(&mut assets, &meta, "town");

        let first = assets.load_model_asset(id).expect("opens the container");
        assert_eq!(first.meta.model_id, id);
        assert_eq!(first.meta.sub_assets.len(), 4);
        assert!(assets.model_by_uuid.contains_key(&id.value()));

        // Delete the file: a re-read would now fail, so a surviving handle proves the cache hit.
        let full = format!("{}/models/town.smodel", assets.root.display());
        std::fs::remove_file(&full).unwrap();
        let second = assets.load_model_asset(id).expect("served from cache");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the second call reuses the cached Arc"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_container_negative_caches() {
        let dir = scratch("missing");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let id = Uuid(7000);
        assets.catalog.put(AssetEntry {
            id,
            name: "ghost".to_owned(),
            asset_type: AssetType::Model,
            path: "models/ghost.smodel".to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });
        assert!(assets.load_model_asset(id).is_none());
        assert!(matches!(assets.model_by_uuid.get(&id.value()), Some(None)));
        assert!(assets.load_model_asset(id).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_container_negative_caches() {
        let dir = scratch("corrupt");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let id = Uuid(7100);
        let rel = "models/corrupt.smodel";
        let full = format!("{}/{rel}", assets.root.display());
        std::fs::write(&full, b"not a container at all").unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: "corrupt".to_owned(),
            asset_type: AssetType::Model,
            path: rel.to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });
        assert!(assets.load_model_asset(id).is_none());
        assert!(matches!(assets.model_by_uuid.get(&id.value()), Some(None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_model_catalog_entry_negative_caches() {
        let dir = scratch("nonmodel");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let id = Uuid(7200);
        assets.catalog.put(AssetEntry {
            id,
            name: "tex".to_owned(),
            asset_type: AssetType::Texture,
            path: "textures/tex.png".to_owned(),
            chunk: -1,
            colorspace: Colorspace::Srgb,
            ..AssetEntry::default()
        });
        assert!(assets.load_model_asset(id).is_none());
        assert!(matches!(assets.model_by_uuid.get(&id.value()), Some(None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunk_source_for_embedded_sub_asset_returns_the_toc_slice() {
        let dir = scratch("embedded");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let mut meta = sample_metadata();
        meta.remap = Value::Object(serde_json::Map::new());
        let id = write_model_fixture(&mut assets, &meta, "town");
        let model = assets.load_model_asset(id).unwrap();

        let source = assets.chunk_source_for(&model, ChunkKind::Mesh, Uuid(11));
        assert!(!source.is_empty());
        assert!(source.path.ends_with("town.smodel"));
        assert_ne!(source.offset, 0, "an embedded chunk has a non-zero offset");
        assert_ne!(source.length, 0);
        let sliced = source.read().unwrap();
        assert_eq!(sliced, sample_mesh_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunk_source_for_remap_returns_the_external_path() {
        let dir = scratch("remap");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        // Remap sub-id 11 (the mesh) to an external file that exists.
        let mut remap = serde_json::Map::new();
        remap.insert(
            "11".to_owned(),
            serde_json::json!({ "external": "meshes/town_extracted.smesh" }),
        );
        let mut meta = sample_metadata();
        meta.remap = Value::Object(remap);
        let id = write_model_fixture(&mut assets, &meta, "town");

        std::fs::create_dir_all(root.join("meshes")).unwrap();
        let external = root.join("meshes").join("town_extracted.smesh");
        std::fs::write(&external, sample_mesh_bytes()).unwrap();

        let model = assets.load_model_asset(id).unwrap();
        let source = assets.chunk_source_for(&model, ChunkKind::Mesh, Uuid(11));
        assert!(source.path.ends_with("town_extracted.smesh"));
        assert_eq!(source.offset, 0, "an external file is read whole");
        assert_eq!(source.length, 0);
        assert_eq!(source.read().unwrap(), sample_mesh_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunk_source_for_missing_remap_target_falls_back_to_embedded() {
        let dir = scratch("remapgone");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        // Remap sub-id 11 to an external file that does NOT exist.
        let mut remap = serde_json::Map::new();
        remap.insert(
            "11".to_owned(),
            serde_json::json!({ "external": "meshes/gone.smesh" }),
        );
        let mut meta = sample_metadata();
        meta.remap = Value::Object(remap);
        let id = write_model_fixture(&mut assets, &meta, "town");

        let model = assets.load_model_asset(id).unwrap();
        let source = assets.chunk_source_for(&model, ChunkKind::Mesh, Uuid(11));
        // Falls back to the embedded chunk: the container path with a non-zero slice.
        assert!(source.path.ends_with("town.smodel"));
        assert_ne!(source.length, 0);
        assert_eq!(source.read().unwrap(), sample_mesh_bytes());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunk_source_for_unknown_sub_id_is_empty() {
        let dir = scratch("unknown");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let id = write_model_fixture(&mut assets, &sample_metadata(), "town");
        let model = assets.load_model_asset(id).unwrap();

        let source = assets.chunk_source_for(&model, ChunkKind::Mesh, Uuid(9999));
        assert!(source.is_empty());
        assert_eq!(source, ByteSource::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mesh_counts_for_asset_reads_an_embedded_chunk() {
        let dir = scratch("counts");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let id = write_model_fixture(&mut assets, &sample_metadata(), "town");

        let entry = AssetEntry {
            id: Uuid(11),
            name: "town_mesh".to_owned(),
            asset_type: AssetType::Mesh,
            path: "models/town.smodel".to_owned(),
            container: id,
            chunk: 1,
            ..AssetEntry::default()
        };
        let counts = assets.mesh_counts_for_asset(&entry).unwrap();
        assert_eq!(counts.vertex_count, 3);
        assert_eq!(counts.index_count, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn byte_source_slice_out_of_bounds_errors() {
        let dir = scratch("oob");
        let file = dir.join("blob.bin");
        std::fs::write(&file, [0u8; 16]).unwrap();
        let source = ByteSource {
            path: file.display().to_string(),
            offset: 8,
            length: 32,
        };
        assert!(matches!(source.read(), Err(Error::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
