//! Asset-management operations the control plane drives: sub-asset extraction, model
//! reimport, the read-only dependency graph + cleanup analysis, and drag-a-folder
//! material import.
//!
//! These sit on top of the container reader ([`crate::model`]) and the material/import
//! layers, and are the substrate the `extract-subasset`, `clear-extraction`,
//! `reimport-model`, `asset-references`, `clean-assets`, `delete-unused`, and
//! `material-import` control commands call.

use std::collections::HashSet;

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, ContainerChunk, ContainerReader, translate_model, write_container,
};
use saffron_json::Value;
use saffron_scene::{
    AssetEntry, AssetType, IdComponent, MaterialSet, Mesh as MeshComponent, ModelInstance, Scene,
    Script, SkinnedMesh,
};

use crate::gpu::GpuUploader;
use crate::import::{IMPORTER_VERSION, ImportOptions, catalog_rows_for_model, hash_file_fnv};
use crate::material::{MaterialAsset, material_asset_from_json, save_material_asset};
use crate::model::{ContainerMetadata, encode_container_metadata, read_container_metadata};
use crate::names::colorspace_from_name;
use crate::scan::detect_material_role;
use crate::{AssetServer, Error, Result};

/// The standalone destination path an extracted sub-asset defaults to, by type. Mesh →
/// `models/<id>.smesh`, material → `materials/<id>.smat`, animation → `models/<id>.sanim`,
/// texture → `textures/<id>.<ext>`.
fn default_extract_dest(asset_type: AssetType, sub_id: Uuid, image_ext: &str) -> String {
    let id = sub_id.value();
    match asset_type {
        AssetType::Material => format!("materials/{id}.smat"),
        AssetType::Mesh => format!("models/{id}.smesh"),
        AssetType::Animation => format!("models/{id}.sanim"),
        _ => {
            let ext = if image_ext.is_empty() {
                "png"
            } else {
                image_ext
            };
            format!("textures/{id}.{ext}")
        }
    }
}

/// The image extension implied by a texture chunk's leading bytes (png/jpg/hdr), default
/// png.
fn image_ext_from_bytes(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && bytes[..4] == [0x89, 0x50, 0x4E, 0x47] {
        return "png";
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return "jpg";
    }
    if bytes.len() >= 2 && bytes[..2] == [0x23, 0x3F] {
        // "#?" — Radiance HDR.
        return "hdr";
    }
    "png"
}

/// Maps a TOC fourcc back to its [`ChunkKind`], or `None` for an unknown tag.
fn chunk_kind_from_fourcc(fourcc: u32) -> Option<ChunkKind> {
    [
        ChunkKind::Meta,
        ChunkKind::Mesh,
        ChunkKind::Texture,
        ChunkKind::Material,
        ChunkKind::Animation,
        ChunkKind::Thumbnail,
    ]
    .into_iter()
    .find(|&kind| kind as u32 == fourcc)
}

/// Rewrites a container with a fresh META chunk, preserving every payload chunk verbatim.
/// The simplest correct way to grow/shrink the metadata (remap edits) without tracking
/// payload offsets.
///
/// # Errors
///
/// Propagates a chunk-read or container-write failure.
fn rewrite_container_meta(
    full_path: &str,
    reader: &ContainerReader,
    new_meta: &ContainerMetadata,
) -> Result<()> {
    let meta_bytes = encode_container_metadata(new_meta);
    let mut payloads: Vec<(ChunkKind, u64, u32, Vec<u8>)> = Vec::new();
    for entry in reader.toc() {
        if entry.fourcc == ChunkKind::Meta as u32 {
            continue;
        }
        let Some(kind) = chunk_kind_from_fourcc(entry.fourcc) else {
            continue;
        };
        let bytes = reader.read_chunk(entry)?;
        payloads.push((kind, entry.sub_id, entry.flags, bytes));
    }
    let mut chunks = Vec::with_capacity(payloads.len() + 1);
    chunks.push(ContainerChunk {
        kind: ChunkKind::Meta,
        sub_id: 0,
        flags: 0,
        bytes: &meta_bytes,
    });
    for (kind, sub_id, flags, bytes) in &payloads {
        chunks.push(ContainerChunk {
            kind: *kind,
            sub_id: *sub_id,
            flags: *flags,
            bytes,
        });
    }
    write_container(full_path, &chunks)?;
    Ok(())
}

/// Resolves an embedded material sub-asset by reading + parsing its container chunk.
/// Standalone materials use [`crate::material::load_material_asset`].
fn resolve_container_material(
    assets: &mut AssetServer,
    model_id: Uuid,
    sub_id: Uuid,
) -> Option<MaterialAsset> {
    let model = assets.load_model_asset(model_id)?;
    let source = assets.chunk_source_for(&model, ChunkKind::Material, sub_id);
    if source.is_empty() {
        return None;
    }
    let bytes = source.read().ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let doc = saffron_json::parse_json(&text).ok()?;
    Some(material_asset_from_json(&doc))
}

/// The container file's project-relative path, from its catalog row.
fn container_path(assets: &AssetServer, model_id: Uuid) -> Option<String> {
    assets.catalog.find(model_id).map(|e| e.path.clone())
}

/// Slices a sub-asset's chunk out of its container to a standalone file (keeping the same
/// sub-id), registers a standalone catalog row for it, and writes a remap entry so
/// resolution prefers the external file. The container's bytes are otherwise untouched;
/// [`clear_extraction`] reverts. Returns the standalone asset's id (`== sub_id`). `dest`
/// is project-relative; empty picks the per-type default.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable, lacks the sub-asset/chunk, or the external
/// file cannot be written; propagates a container chunk-read / rewrite failure.
pub fn extract_sub_asset(
    assets: &mut AssetServer,
    model_id: Uuid,
    sub_id: Uuid,
    dest: &str,
) -> Result<Uuid> {
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let sub = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.sub_id.value() == sub_id.value())
        .cloned()
        .ok_or(Error::ContainerMissingSubAsset {
            container: model_id.value(),
            sub: sub_id.value(),
        })?;
    let entry = model
        .reader
        .toc()
        .iter()
        .find(|e| e.sub_id == sub_id.value() && e.fourcc != ChunkKind::Meta as u32)
        .copied()
        .ok_or_else(|| {
            Error::Io(format!(
                "model {} has no chunk for sub-asset {}",
                model_id.value(),
                sub_id.value()
            ))
        })?;
    let bytes = model.reader.read_chunk(&entry)?;

    let relative_dest = if dest.is_empty() {
        let image_ext = if sub.asset_type == AssetType::Texture {
            image_ext_from_bytes(&bytes)
        } else {
            ""
        };
        default_extract_dest(sub.asset_type, sub_id, image_ext)
    } else {
        dest.to_owned()
    };
    let full_dest = format!("{}/{relative_dest}", assets.root.display());
    if let Some(parent) = std::path::Path::new(&full_dest).parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    std::fs::write(&full_dest, &bytes)
        .map_err(|e| Error::Io(format!("cannot write '{relative_dest}': {e}")))?;

    let mut updated = model.meta.clone();
    if !updated.remap.is_object() {
        updated.remap = Value::Object(serde_json::Map::new());
    }
    if let Some(object) = updated.remap.as_object_mut() {
        object.insert(
            sub_id.value().to_string(),
            serde_json::json!({ "external": relative_dest }),
        );
    }
    let container = container_path(assets, model_id)
        .ok_or_else(|| Error::Io(format!("model {} not in catalog", model_id.value())))?;
    let container_full = format!("{}/{container}", assets.root.display());
    rewrite_container_meta(&container_full, &model.reader, &updated)?;

    assets.catalog.put(AssetEntry {
        id: sub_id,
        name: sub.name.clone(),
        asset_type: sub.asset_type,
        path: relative_dest,
        colorspace: colorspace_from_name(&sub.colorspace),
        duration: sub.duration,
        tracks: sub.tracks,
        ..AssetEntry::default()
    });

    // Drop the stale reader (its TOC offsets shifted) and the sub-asset's GPU ref so the
    // next resolve reads the external file.
    assets.model_by_uuid.remove(&model_id.value());
    assets.mesh_by_uuid.remove(&sub_id.value());
    assets.texture_by_uuid.remove(&sub_id.value());
    Ok(sub_id)
}

/// Drops a sub-asset's extraction: removes the remap entry, deletes the external file (so
/// its uuid name can never alias the embedded chunk on a later scan), reverts the catalog
/// row to the embedded chunk, and refreshes caches.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable / not in the catalog; propagates a container
/// rewrite failure.
pub fn clear_extraction(assets: &mut AssetServer, model_id: Uuid, sub_id: Uuid) -> Result<()> {
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let key = sub_id.value().to_string();
    let external = model
        .meta
        .remap
        .as_object()
        .and_then(|m| m.get(&key))
        .and_then(|entry| entry.get("external"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut updated = model.meta.clone();
    if let Some(object) = updated.remap.as_object_mut() {
        object.remove(&key);
    }
    let container = container_path(assets, model_id)
        .ok_or_else(|| Error::Io(format!("model {} not in catalog", model_id.value())))?;
    let container_full = format!("{}/{container}", assets.root.display());
    rewrite_container_meta(&container_full, &model.reader, &updated)?;
    if let Some(external) = external {
        let _ = std::fs::remove_file(format!("{}/{external}", assets.root.display()));
    }

    // Revert the catalog row to the embedded sub-asset (container + chunk).
    if let Some(sub) = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.sub_id.value() == sub_id.value())
    {
        assets.catalog.put(AssetEntry {
            id: sub_id,
            name: sub.name.clone(),
            asset_type: sub.asset_type,
            path: container.clone(),
            container: model_id,
            chunk: sub.chunk as i32,
            colorspace: colorspace_from_name(&sub.colorspace),
            duration: sub.duration,
            tracks: sub.tracks,
            ..AssetEntry::default()
        });
    }
    assets.model_by_uuid.remove(&model_id.value());
    assets.mesh_by_uuid.remove(&sub_id.value());
    assets.texture_by_uuid.remove(&sub_id.value());
    Ok(())
}

/// What a reimport changed, diffed by stable sub-id.
///
/// `skipped` is true when the source bytes + importer version are unchanged (the
/// content-addressed fast path). `removed_from_source` lists sub-assets the source no
/// longer produces — kept + reported (cleanup decides their fate), never silently dropped.
#[derive(Clone, Debug, Default)]
pub struct ReimportDelta {
    /// Sub-ids the source still produces (re-baked bytes).
    pub updated: Vec<Uuid>,
    /// Sub-ids the source newly produces.
    pub added: Vec<Uuid>,
    /// Sub-ids the source no longer produces (kept + reported).
    pub removed_from_source: Vec<Uuid>,
    /// The source bytes + importer version are unchanged — nothing was re-baked.
    pub skipped: bool,
}

/// Re-bakes a container from its stored source + options when the source bytes changed
/// (else a content-addressed skip). Sub-ids are stable (by source name), so the diff
/// matches; an extracted (remapped) sub-asset's external override is preserved — the
/// freshly baked chunk is the dormant fallback. Live instances resolve by (model_id,
/// sub_id), so they pick up the new bytes with no re-instantiation. The caller idles the
/// GPU; this drops sub-id caches.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable or the source is unreadable; propagates a
/// translate/bake/container failure.
pub fn reimport_model(assets: &mut AssetServer, model_id: Uuid) -> Result<ReimportDelta> {
    let mut delta = ReimportDelta::default();
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let old_meta = model.meta.clone();
    let source = old_meta.import.source_path.clone();
    let current_hash = hash_file_fnv(&source);
    if current_hash.is_empty() {
        return Err(Error::Io(format!("source '{source}' is unreadable")));
    }
    if current_hash == old_meta.import.source_hash
        && old_meta.import.importer_version == IMPORTER_VERSION
    {
        delta.skipped = true;
        return Ok(delta);
    }

    let old_subs: HashSet<u64> = old_meta
        .sub_assets
        .iter()
        .map(|s| s.sub_id.value())
        .collect();

    let graph = translate_model(&source)?;
    let options = ImportOptions::from_json(&old_meta.import.options);
    let bake = assets.bake_model(&graph, options, &source, model_id)?;

    let container_full = format!("{}/{}", assets.root.display(), bake.path);
    let mut new_meta = read_container_metadata(&container_full)?;
    let new_subs: HashSet<u64> = new_meta
        .sub_assets
        .iter()
        .map(|s| s.sub_id.value())
        .collect();

    // Preserve the remap for sub-assets that still exist (the extracted edit survives the
    // reimport).
    let mut kept_remap = serde_json::Map::new();
    if let Some(object) = old_meta.remap.as_object() {
        for (key, value) in object {
            let sid: u64 = key.parse().unwrap_or(0);
            if new_subs.contains(&sid) {
                kept_remap.insert(key.clone(), value.clone());
            }
        }
    }
    if !kept_remap.is_empty() {
        new_meta.remap = Value::Object(kept_remap);
        if let Ok(reader) = saffron_geometry::read_container(&container_full)
            && let Err(err) = rewrite_container_meta(&container_full, &reader, &new_meta)
        {
            tracing::warn!(
                "reimport: could not preserve remap for model {}: {err}",
                model_id.value()
            );
        }
    }

    for sid in &new_subs {
        if old_subs.contains(sid) {
            delta.updated.push(Uuid(*sid));
        } else {
            delta.added.push(Uuid(*sid));
        }
    }
    for sid in &old_subs {
        if !new_subs.contains(sid) {
            delta.removed_from_source.push(Uuid(*sid));
        }
    }

    // Refresh the catalog rows (remap-aware) and drop the stale reader + every affected
    // sub-id's GPU ref so live instances re-resolve the new bytes.
    if let Ok(final_meta) = read_container_metadata(&container_full) {
        for row in catalog_rows_for_model(&final_meta, &bake.path) {
            assets.catalog.put(row);
        }
    }
    assets.model_by_uuid.remove(&model_id.value());
    for sid in new_subs.union(&old_subs) {
        assets.mesh_by_uuid.remove(sid);
        assets.texture_by_uuid.remove(sid);
    }
    Ok(delta)
}

/// One node in the asset dependency graph: an asset and its on-disk byte cost.
#[derive(Clone, Copy, Debug)]
pub struct RefNode {
    /// The asset id.
    pub id: Uuid,
    /// The asset kind.
    pub asset_type: AssetType,
    /// The owning container (`0` for a standalone asset).
    pub container: Uuid,
    /// The on-disk byte cost.
    pub bytes: u64,
}

/// How an [`RefEdge`] arises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefEdgeKind {
    /// A container references its embedded sub-asset.
    ContainerChild,
    /// A material references a texture slot.
    MaterialTexture,
    /// A scene entity references an asset.
    EntityAsset,
}

/// One directed edge: `from` references `to`. `from` may be an entity uuid (`EntityAsset`)
/// rather than a catalog asset.
#[derive(Clone, Copy, Debug)]
pub struct RefEdge {
    /// The referrer id.
    pub from: Uuid,
    /// The referenced id.
    pub to: Uuid,
    /// How the reference arises.
    pub kind: RefEdgeKind,
}

/// The scene → asset → sub-asset reference graph (UE's Reference Viewer + Size Map):
/// who-references-this, what-this-references, and a byte footprint. Read-only/diagnostic,
/// rebuilt on demand.
#[derive(Clone, Debug, Default)]
pub struct DependencyGraph {
    /// The catalog assets as nodes.
    pub nodes: Vec<RefNode>,
    /// The directed reference edges.
    pub edges: Vec<RefEdge>,
}

impl DependencyGraph {
    /// The referrers of `id` (every edge's `from` where `to == id`).
    #[must_use]
    pub fn referenced_by(&self, id: Uuid) -> Vec<Uuid> {
        self.edges
            .iter()
            .filter(|e| e.to.value() == id.value())
            .map(|e| e.from)
            .collect()
    }

    /// The references of `id` (every edge's `to` where `from == id`).
    #[must_use]
    pub fn references_of(&self, id: Uuid) -> Vec<Uuid> {
        self.edges
            .iter()
            .filter(|e| e.from.value() == id.value())
            .map(|e| e.to)
            .collect()
    }

    /// The on-disk bytes recorded for `id` (`0` when absent).
    #[must_use]
    pub fn bytes_of(&self, id: Uuid) -> u64 {
        self.nodes
            .iter()
            .find(|n| n.id.value() == id.value())
            .map_or(0, |n| n.bytes)
    }

    /// The on-disk footprint of `id` — its own bytes (a container's `.smodel` size already
    /// counts its embedded sub-assets, so there is no double-counting).
    #[must_use]
    pub fn footprint(&self, id: Uuid) -> u64 {
        self.bytes_of(id)
    }
}

/// The on-disk bytes of a catalog row: a model / standalone file's size, or an embedded
/// sub-asset's chunk length (read from its container's TOC).
#[must_use]
pub fn asset_bytes(assets: &mut AssetServer, entry: &AssetEntry) -> u64 {
    if entry.container.value() == 0 {
        return std::fs::metadata(format!("{}/{}", assets.root.display(), entry.path))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    let Some(model) = assets.load_model_asset(entry.container) else {
        return 0;
    };
    let kind = match entry.asset_type {
        AssetType::Mesh => ChunkKind::Mesh,
        AssetType::Material => ChunkKind::Material,
        AssetType::Animation => ChunkKind::Animation,
        _ => ChunkKind::Texture,
    };
    model
        .reader
        .find(kind, entry.id.value())
        .map_or(0, |toc| toc.length)
}

/// Gathers each entity's stable id plus the asset ids it references, by component query.
/// Returns `(entity_id, asset_id)` pairs — the `EntityAsset` edges. Done in two passes
/// because [`Scene::for_each`](saffron_scene::Scene::for_each) borrows the world mutably
/// during the callback (the id read happens after).
fn entity_asset_pairs(scene: &mut Scene) -> Vec<(Uuid, Uuid)> {
    let mut refs: Vec<(saffron_scene::Entity, Uuid)> = Vec::new();
    scene.for_each::<&MeshComponent, _>(|entity, mesh| refs.push((entity, mesh.mesh)));
    scene.for_each::<&SkinnedMesh, _>(|entity, skin| refs.push((entity, skin.mesh)));
    scene.for_each::<&MaterialSet, _>(|entity, set| {
        for slot in &set.slots {
            for tex in [
                slot.albedo_texture,
                slot.metallic_roughness_texture,
                slot.normal_texture,
                slot.occlusion_texture,
                slot.emissive_texture,
            ] {
                refs.push((entity, tex));
            }
        }
    });
    scene.for_each::<&ModelInstance, _>(|entity, instance| refs.push((entity, instance.model_id)));
    // The shared-material-asset component is `saffron_scene::MaterialAsset` (id-by-ref),
    // imported aliased so it never collides with the asset crate's `MaterialAsset` value.
    scene.for_each::<&saffron_scene::MaterialAsset, _>(|entity, mat| {
        refs.push((entity, mat.material))
    });

    let mut out = Vec::new();
    for (entity, asset) in refs {
        if asset.value() == 0 {
            continue;
        }
        let entity_id = scene
            .component::<IdComponent>(entity)
            .map_or(0, |id| id.id.value());
        out.push((Uuid(entity_id), asset));
    }
    out
}

/// Builds the dependency graph: catalog assets as nodes; container→child,
/// material→texture, and scene-entity→asset edges. A snapshot — rebuilt on demand.
pub fn build_dependency_graph(scene: &mut Scene, assets: &mut AssetServer) -> DependencyGraph {
    let mut graph = DependencyGraph::default();
    let entries: Vec<AssetEntry> = assets.catalog.entries.clone();
    for entry in &entries {
        let bytes = asset_bytes(assets, entry);
        graph.nodes.push(RefNode {
            id: entry.id,
            asset_type: entry.asset_type,
            container: entry.container,
            bytes,
        });
    }
    for entry in &entries {
        if entry.asset_type == AssetType::Model {
            for child in &entries {
                if child.container.value() == entry.id.value() {
                    graph.edges.push(RefEdge {
                        from: entry.id,
                        to: child.id,
                        kind: RefEdgeKind::ContainerChild,
                    });
                }
            }
        }
        if entry.asset_type == AssetType::Material {
            let material = if entry.container.value() != 0 {
                resolve_container_material(assets, entry.container, entry.id)
            } else {
                crate::material::load_material_asset(assets, entry.id).ok()
            };
            if let Some(material) = material {
                for tex in [
                    material.albedo_texture,
                    material.orm_texture,
                    material.normal_texture,
                    material.emissive_texture,
                    material.height_texture,
                ] {
                    if tex.value() != 0 {
                        graph.edges.push(RefEdge {
                            from: entry.id,
                            to: tex,
                            kind: RefEdgeKind::MaterialTexture,
                        });
                    }
                }
            }
        }
    }
    for (entity_id, asset) in entity_asset_pairs(scene) {
        graph.edges.push(RefEdge {
            from: entity_id,
            to: asset,
            kind: RefEdgeKind::EntityAsset,
        });
    }
    graph
}

/// How a cleanup candidate is classified. Only [`CleanCategory::Unused`] is auto-deletable,
/// and even then only after explicit confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanCategory {
    /// Unreachable from the active scene + not script-referenced.
    Unused,
    /// A file on disk with no catalog row (reported, never auto-deleted here).
    OrphanedFile,
    /// A scene/material edge to a missing id.
    BrokenReference,
    /// Reachable only through a script override field — review before deleting.
    IndirectReview,
}

impl CleanCategory {
    /// The wire name the control layer reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::OrphanedFile => "orphaned",
            Self::BrokenReference => "broken",
            Self::IndirectReview => "review",
            Self::Unused => "unused",
        }
    }
}

/// One asset the cleanup analysis flagged.
#[derive(Clone, Debug)]
pub struct CleanCandidate {
    /// The candidate asset id.
    pub id: Uuid,
    /// The candidate's project-relative path.
    pub path: String,
    /// How it was classified.
    pub category: CleanCategory,
    /// Its on-disk byte cost.
    pub bytes: u64,
    /// Why it was flagged.
    pub reason: String,
}

/// The cleanup analysis report — candidates plus the reclaimable byte total (only `Unused`
/// candidates count toward it).
#[derive(Clone, Debug, Default)]
pub struct CleanReportData {
    /// Every flagged candidate.
    pub candidates: Vec<CleanCandidate>,
    /// Bytes recoverable by deleting the `Unused` candidates.
    pub reclaimable_bytes: u64,
}

/// Every catalog-id string referenced (recursively) by a `Script` override field. These
/// are invisible to the static dependency graph, so an asset only reachable this way is
/// review, not unused.
fn collect_script_referenced_ids(scene: &mut Scene) -> HashSet<u64> {
    fn walk(value: &Value, out: &mut HashSet<u64>) {
        match value {
            Value::String(text) => {
                if let Ok(id) = text.parse::<u64>()
                    && id != 0
                {
                    out.insert(id);
                }
            }
            Value::Object(map) => {
                for child in map.values() {
                    walk(child, out);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    let mut referenced = HashSet::new();
    scene.for_each::<&Script, _>(|_, script| {
        for slot in &script.scripts {
            walk(&slot.overrides, &mut referenced);
        }
    });
    referenced
}

/// Classifies every catalog asset as kept or a cleanup candidate, by reachability from the
/// active scene's asset refs + `exclude`. Read-only — produces a report, deletes nothing.
#[must_use]
pub fn analyze_clean(
    scene: &mut Scene,
    assets: &mut AssetServer,
    exclude: &[Uuid],
) -> CleanReportData {
    let mut report = CleanReportData::default();
    let graph = build_dependency_graph(scene, assets);

    let mut reachable: HashSet<u64> = HashSet::new();
    for edge in &graph.edges {
        if edge.kind == RefEdgeKind::EntityAsset {
            reachable.insert(edge.to.value());
        }
    }
    for id in exclude {
        reachable.insert(id.value());
    }
    let mut work: Vec<u64> = reachable.iter().copied().collect();
    while let Some(id) = work.pop() {
        for target in graph.references_of(Uuid(id)) {
            if reachable.insert(target.value()) {
                work.push(target.value());
            }
        }
    }
    // A container and its embedded sub-assets are one deletable unit: keeping any one keeps
    // all.
    let catalog = &assets.catalog;
    let mut kept_containers: HashSet<u64> = HashSet::new();
    for entry in &catalog.entries {
        if reachable.contains(&entry.id.value()) {
            if entry.asset_type == AssetType::Model {
                kept_containers.insert(entry.id.value());
            }
            if entry.container.value() != 0 {
                kept_containers.insert(entry.container.value());
            }
        }
    }
    for entry in &catalog.entries {
        if kept_containers.contains(&entry.id.value())
            || (entry.container.value() != 0 && kept_containers.contains(&entry.container.value()))
        {
            reachable.insert(entry.id.value());
        }
    }

    let script_refs = collect_script_referenced_ids(scene);
    let catalog = &assets.catalog;

    for edge in &graph.edges {
        if edge.kind == RefEdgeKind::ContainerChild {
            continue;
        }
        if !catalog.by_id.contains_key(&edge.to.value()) {
            report.candidates.push(CleanCandidate {
                id: edge.to,
                path: String::new(),
                category: CleanCategory::BrokenReference,
                bytes: 0,
                reason: format!("referenced by {} but not in the catalog", edge.from.value()),
            });
        }
    }

    for entry in &catalog.entries {
        if reachable.contains(&entry.id.value()) || entry.container.value() != 0 {
            continue; // kept, or an embedded sub-asset (the container is the deletable unit)
        }
        let bytes = graph.bytes_of(entry.id);
        let candidate = if script_refs.contains(&entry.id.value()) {
            CleanCandidate {
                id: entry.id,
                path: entry.path.clone(),
                category: CleanCategory::IndirectReview,
                bytes,
                reason: "referenced only by a script field — review before deleting".to_owned(),
            }
        } else {
            report.reclaimable_bytes += bytes;
            CleanCandidate {
                id: entry.id,
                path: entry.path.clone(),
                category: CleanCategory::Unused,
                bytes,
                reason: "not reachable from the active scene".to_owned(),
            }
        };
        report.candidates.push(candidate);
    }
    report
}

/// What [`delete_unused`] removed.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeleteUnusedData {
    /// Number of assets deleted.
    pub deleted: i32,
    /// Bytes reclaimed on disk.
    pub reclaimed_bytes: u64,
}

/// Deletes only the listed ids that [`analyze_clean`] classifies as `Unused` (refusing
/// without confirm), then rescans so any newly-orphaned cascade resurfaces. Outward-facing
/// + irreversible. The caller idles the GPU + clears caches first.
///
/// # Errors
///
/// [`Error::Io`] (with a `confirm` message) when `confirm` is false.
pub fn delete_unused(
    assets: &mut AssetServer,
    scene: &mut Scene,
    ids: &[Uuid],
    confirm: bool,
) -> Result<DeleteUnusedData> {
    if !confirm {
        return Err(Error::Io("delete-unused requires confirm=true".to_owned()));
    }
    let report = analyze_clean(scene, assets, &[]);
    let deletable: HashSet<u64> = report
        .candidates
        .iter()
        .filter(|c| c.category == CleanCategory::Unused)
        .map(|c| c.id.value())
        .collect();
    let mut result = DeleteUnusedData::default();
    for id in ids {
        if !deletable.contains(&id.value()) {
            tracing::warn!(
                "delete-unused: refusing {} (not classified Unused)",
                id.value()
            );
            continue;
        }
        let Some(path) = assets.catalog.find(*id).map(|e| e.path.clone()) else {
            continue;
        };
        let full = format!("{}/{path}", assets.root.display());
        let bytes = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&full);
        let _ = std::fs::remove_file(format!("{full}.smeta")); // foreign-file sidecar, if any
        result.deleted += 1;
        result.reclaimed_bytes += bytes;
        tracing::info!("delete-unused: removed '{path}' ({bytes} bytes)");
    }
    let _ = assets.scan_assets(); // rebuild the catalog + surface any cascade
    assets.write_catalog_cache();
    Ok(result)
}

/// The result of a folder material import: the saved material's id plus the space-joined
/// detected roles (for the editor's confirmation proposal).
#[derive(Clone, Debug, Default)]
pub struct MaterialImportResult {
    /// The saved `.smat` material id.
    pub material: Uuid,
    /// Space-joined detected map roles.
    pub roles: String,
}

/// Drag-a-folder material import: scans `dir` for textures, detects each map's role by
/// filename suffix, imports it with the right colorspace, assembles a `.smat`, and saves
/// it. Normal maps assume OpenGL convention; a packed ARM/ORM also feeds the occlusion
/// slot.
///
/// # Errors
///
/// [`Error::Io`] if `dir` is not a directory; propagates the texture-register /
/// material-save failure.
pub fn import_material_folder(
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    dir: &str,
    name: &str,
) -> Result<MaterialImportResult> {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        return Err(Error::Io(format!("not a directory: {dir}")));
    }

    let mut material = MaterialAsset::default();
    let mut roles = String::new();
    let register = |assets: &mut AssetServer, path: &std::path::Path, srgb: bool| -> Uuid {
        let Ok(bytes) = std::fs::read(path) else {
            return Uuid(0);
        };
        if bytes.is_empty() {
            return Uuid(0);
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        assets
            .register_texture_bytes(gpu, &bytes, ext, stem, srgb)
            .unwrap_or(Uuid(0))
    };

    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir_path)
        .map_err(|e| Error::Io(e.to_string()))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    files.sort();

    for path in &files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "tga") {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        match detect_material_role(filename) {
            "albedo" => {
                material.albedo_texture = register(assets, path, true);
                roles.push_str("albedo ");
            }
            "normal" => {
                material.normal_texture = register(assets, path, false);
                roles.push_str("normal ");
            }
            role @ ("orm" | "roughness" | "metallic") => {
                material.orm_texture = register(assets, path, false);
                roles.push_str(role);
                roles.push(' ');
            }
            "ao" => {
                if material.orm_texture.value() == 0 {
                    material.orm_texture = register(assets, path, false);
                }
                roles.push_str("ao ");
            }
            "height" => {
                material.height_texture = register(assets, path, false);
                roles.push_str("height ");
            }
            "emissive" => {
                material.emissive_texture = register(assets, path, true);
                roles.push_str("emissive ");
            }
            _ => {}
        }
    }

    let mut material_name = name.to_owned();
    if material_name.is_empty() {
        material_name = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
    }
    if material_name.is_empty() {
        material_name = "Material".to_owned();
    }
    let id = save_material_asset(assets, &material, &material_name, "")?;
    Ok(MaterialImportResult {
        material: id,
        roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{ImportedMaterial, ImportedModel, Mesh, Submesh, TextureSource, Vertex};

    use crate::import::{ImportOptions, catalog_rows_for_model};
    use crate::model::read_container_metadata;

    /// A unique scratch dir under the system temp, removed and recreated per test.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "saffron-assets-manage-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A single-triangle mesh so a baked mesh chunk decodes.
    fn tri_mesh() -> Mesh {
        Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::ZERO,
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                },
                Vertex {
                    position: Vec3::X,
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                },
                Vertex {
                    position: Vec3::Y,
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
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

    /// A static model with one material that references an embedded texture (albedo).
    fn one_material_graph() -> ImportedModel {
        ImportedModel {
            nodes: vec![saffron_geometry::ImportedNode {
                name: "mesh".to_owned(),
                mesh: Some(tri_mesh()),
                ..saffron_geometry::ImportedNode::default()
            }],
            materials: vec![ImportedMaterial {
                name: "paint".to_owned(),
                albedo: Some(TextureSource {
                    bytes: vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4],
                    ext: "png".to_owned(),
                }),
                ..ImportedMaterial::default()
            }],
            animations: Vec::new(),
            skin: None,
            morph: None,
        }
    }

    /// Bakes `graph` from `source_path`, registers its catalog rows, and returns the
    /// container's model id.
    fn bake_and_register(
        assets: &mut AssetServer,
        graph: &ImportedModel,
        source_path: &str,
    ) -> Uuid {
        let bake = assets
            .bake_model(graph, ImportOptions::default(), source_path, Uuid(0))
            .expect("bake");
        let full = format!("{}/{}", assets.root.display(), bake.path);
        let meta = read_container_metadata(&full).expect("meta");
        for row in catalog_rows_for_model(&meta, &bake.path) {
            assets.catalog.put(row);
        }
        bake.model_id
    }

    /// The first sub-asset of `asset_type` in the catalog (by container id).
    fn first_sub(assets: &AssetServer, container: Uuid, asset_type: AssetType) -> Uuid {
        assets
            .catalog
            .entries
            .iter()
            .find(|e| e.container.value() == container.value() && e.asset_type == asset_type)
            .map(|e| e.id)
            .expect("sub-asset present")
    }

    #[test]
    fn extract_then_clear_round_trips_a_material_sub_asset() {
        let dir = scratch("extract");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/tmp/paint.obj");
        let material_sub = first_sub(&assets, model_id, AssetType::Material);

        // Before extraction the material row points at the container, container != 0.
        assert_ne!(
            assets.catalog.find(material_sub).unwrap().container.value(),
            0
        );

        let extracted =
            extract_sub_asset(&mut assets, model_id, material_sub, "").expect("extract");
        assert_eq!(extracted, material_sub, "extraction keeps the sub-id");

        // The row is now a standalone .smat (container == 0) at the default dest, and the
        // file exists on disk.
        let row = assets.catalog.find(material_sub).unwrap();
        assert_eq!(row.container.value(), 0);
        assert_eq!(row.path, format!("materials/{}.smat", material_sub.value()));
        assert!(root.join(&row.path).exists(), "external file written");

        // The container's META now carries the remap entry.
        let container_path = assets.catalog.find(model_id).unwrap().path.clone();
        let meta = read_container_metadata(format!("{}/{container_path}", root.display())).unwrap();
        assert!(
            meta.remap
                .as_object()
                .is_some_and(|m| m.contains_key(&material_sub.value().to_string())),
            "remap records the extraction"
        );

        // Clearing reverts: the external file is gone, the row points back at the
        // container, and the remap entry is dropped.
        clear_extraction(&mut assets, model_id, material_sub).expect("clear");
        assert!(
            !root
                .join(format!("materials/{}.smat", material_sub.value()))
                .exists()
        );
        let reverted = assets.catalog.find(material_sub).unwrap();
        assert_eq!(reverted.container.value(), model_id.value());
        assert_eq!(reverted.path, container_path);
        let meta = read_container_metadata(format!("{}/{container_path}", root.display())).unwrap();
        assert!(
            meta.remap
                .as_object()
                .is_none_or(|m| !m.contains_key(&material_sub.value().to_string())),
            "remap entry cleared"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reimport_skips_when_the_source_is_unchanged() {
        let dir = scratch("reimport-skip");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // A real source file so the stored hash matches the recomputed one.
        let source = dir.join("paint.obj");
        std::fs::write(&source, b"o cube\nv 0 0 0\n").unwrap();
        let source_path = source.to_string_lossy().into_owned();
        let model_id = bake_and_register(&mut assets, &one_material_graph(), &source_path);

        let delta = reimport_model(&mut assets, model_id).expect("reimport");
        assert!(
            delta.skipped,
            "unchanged source is a content-addressed skip"
        );
        assert!(delta.updated.is_empty());
        assert!(delta.added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reimport_errors_when_the_source_is_unreadable() {
        let dir = scratch("reimport-missing");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/no/such/source.obj");

        let err = reimport_model(&mut assets, model_id).expect_err("unreadable source errors");
        assert!(matches!(err, Error::Io(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_graph_links_container_children_material_textures_and_entities() {
        let dir = scratch("graph");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/tmp/paint.obj");
        let mesh_sub = first_sub(&assets, model_id, AssetType::Mesh);
        let material_sub = first_sub(&assets, model_id, AssetType::Material);
        let texture_sub = first_sub(&assets, model_id, AssetType::Texture);

        // A scene entity references the mesh sub-asset.
        let mut scene = Scene::new();
        let entity = scene.create_entity("Tri");
        scene
            .add_component(entity, MeshComponent { mesh: mesh_sub })
            .unwrap();
        let entity_id = scene.component::<IdComponent>(entity).unwrap().id;

        let graph = build_dependency_graph(&mut scene, &mut assets);

        // The container references each of its sub-assets (ContainerChild).
        let children = graph.references_of(model_id);
        assert!(children.iter().any(|c| c.value() == mesh_sub.value()));
        assert!(children.iter().any(|c| c.value() == material_sub.value()));

        // The material references the embedded albedo texture (MaterialTexture).
        assert!(
            graph
                .references_of(material_sub)
                .iter()
                .any(|t| t.value() == texture_sub.value()),
            "material → texture edge"
        );

        // The entity references the mesh (EntityAsset).
        assert!(
            graph
                .referenced_by(mesh_sub)
                .iter()
                .any(|r| r.value() == entity_id.value()),
            "entity → mesh edge"
        );

        // The model's footprint is its .smodel size (non-zero).
        assert!(graph.footprint(model_id) > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_flags_an_unreferenced_standalone_asset_as_unused() {
        let dir = scratch("clean");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        // Two standalone meshes; only one is referenced by the scene.
        std::fs::create_dir_all(root.join("models")).unwrap();
        let used = Uuid::new();
        let orphan = Uuid::new();
        for (id, name) in [(used, "used"), (orphan, "orphan")] {
            let rel = format!("models/{}.smesh", id.value());
            std::fs::write(
                root.join(&rel),
                saffron_geometry::save_mesh_to_buffer(&tri_mesh(), &[], None).unwrap(),
            )
            .unwrap();
            assets.catalog.put(AssetEntry {
                id,
                name: name.to_owned(),
                asset_type: AssetType::Mesh,
                path: rel,
                chunk: -1,
                ..AssetEntry::default()
            });
        }

        let mut scene = Scene::new();
        let entity = scene.create_entity("Tri");
        scene
            .add_component(entity, MeshComponent { mesh: used })
            .unwrap();

        let report = analyze_clean(&mut scene, &mut assets, &[]);
        let unused: Vec<u64> = report
            .candidates
            .iter()
            .filter(|c| c.category == CleanCategory::Unused)
            .map(|c| c.id.value())
            .collect();
        assert!(unused.contains(&orphan.value()), "the orphan is Unused");
        assert!(
            !unused.contains(&used.value()),
            "the referenced mesh is kept"
        );
        assert!(report.reclaimable_bytes > 0);

        // Excluding the orphan keeps it (reachable via the exclude root).
        let report = analyze_clean(&mut scene, &mut assets, &[orphan]);
        assert!(
            !report
                .candidates
                .iter()
                .any(|c| c.id.value() == orphan.value() && c.category == CleanCategory::Unused),
            "an excluded asset is not Unused"
        );

        // delete-unused refuses without confirm; with confirm it removes the orphan.
        assert!(delete_unused(&mut assets, &mut scene, &[orphan], false).is_err());
        let deleted = delete_unused(&mut assets, &mut scene, &[orphan], true).expect("delete");
        assert_eq!(deleted.deleted, 1);
        assert!(
            !root
                .join(format!("models/{}.smesh", orphan.value()))
                .exists()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_material_folder_rejects_a_non_directory() {
        let dir = scratch("matfolder-bad");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // A GPU is never reached on the not-a-directory guard, so a stub is unnecessary.
        struct NoGpu;
        impl GpuUploader for NoGpu {
            fn upload_mesh(
                &self,
                _: &Mesh,
                _: &[saffron_geometry::VertexSkin],
                _morph: Option<&saffron_geometry::MorphData>,
                _sdf_bake: Option<&saffron_rendering::SdfBake>,
            ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuMesh>> {
                unreachable!("guard fails before any upload")
            }
            fn upload_texture(
                &self,
                _: &[u8],
                _: u32,
                _: u32,
                _: bool,
            ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>>
            {
                unreachable!("guard fails before any upload")
            }
            fn upload_texture_float(
                &self,
                _: &[f32],
                _: u32,
                _: u32,
            ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>>
            {
                unreachable!("guard fails before any upload")
            }
            fn skinning_enabled(&self) -> bool {
                false
            }
        }
        let err = import_material_folder(&mut assets, &NoGpu, "/no/such/dir", "Mat")
            .expect_err("not a directory");
        assert!(matches!(err, Error::Io(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(test)]
    mod gpu {
        use super::*;
        use crate::RendererUploader;
        use saffron_rendering::{
            BindlessFreeList, Descriptors, Device, GpuQueue, SurfaceSource, Uploader,
        };

        fn png_2x2() -> Vec<u8> {
            let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            buffer
                .write_to(&mut out, image::ImageFormat::Png)
                .expect("encode png");
            out.into_inner()
        }

        #[test]
        fn import_material_folder_detects_roles_and_saves_a_material() {
            let device = match Device::new(&SurfaceSource::Offscreen) {
                Ok(device) => device,
                Err(err) => {
                    eprintln!("skipping (no Vulkan device): {err}");
                    return;
                }
            };
            let free_list: BindlessFreeList =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let queue = GpuQueue::new(device.graphics_queue);
            let uploader = Uploader::new(&device, &queue).expect("uploader");

            let dir = scratch("matfolder");
            let root = dir.join("project").join("assets");
            let mut assets = AssetServer::new(&root);
            // A folder of role-named textures.
            let folder = dir.join("brick");
            std::fs::create_dir_all(&folder).unwrap();
            // "normal" contains the "orm" substring detect_material_role keys on, so a
            // normal map is named with the `_nor` token to avoid the collision.
            std::fs::write(folder.join("brick_albedo.png"), png_2x2()).unwrap();
            std::fs::write(folder.join("brick_nor.png"), png_2x2()).unwrap();
            std::fs::write(folder.join("brick_roughness.png"), png_2x2()).unwrap();

            let gpu = RendererUploader::new(&uploader, &descriptors, false);
            let result =
                import_material_folder(&mut assets, &gpu, &folder.to_string_lossy(), "Brick")
                    .expect("import");

            assert!(result.roles.contains("albedo"));
            assert!(result.roles.contains("normal"));
            assert!(result.roles.contains("roughness"));
            // The saved material row exists and its texture slots are populated.
            let entry = assets.catalog.find(result.material).expect("material row");
            assert_eq!(entry.asset_type, AssetType::Material);
            let material =
                crate::material::load_material_asset(&assets, result.material).expect("load");
            assert_ne!(material.albedo_texture.value(), 0);
            assert_ne!(material.normal_texture.value(), 0);
            assert_ne!(material.orm_texture.value(), 0);

            device.wait_idle().expect("idle");
            assets.clear_asset_caches();
            drop(assets);
            drop(uploader);
            drop(descriptors);
            drop(device);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
