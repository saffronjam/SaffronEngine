use std::collections::HashSet;

use saffron_core::Uuid;
use saffron_json::Value;
use saffron_scene::{AssetType, Scene, Script};

use crate::{AssetServer, Error, Result};

use super::references::{RefEdgeKind, build_dependency_graph};

/// How a cleanup candidate is classified. Only [`CleanCategory::Unused`] is auto-deletable,
/// and even then only after explicit confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanCategory {
    /// Unreachable from the active scene + not script-referenced.
    Unused,
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

/// Every catalog-id string referenced (recursively) by a `Script` override field. These are
/// invisible to the static dependency graph, so an asset only reachable this way is review,
/// not unused.
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
    // A container and its embedded sub-assets are one deletable unit: keeping any one keeps all.
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
        // A self-container (material import: `container == id`) is the deletable unit itself, not
        // an embedded sub-asset — deleting its `.smatx` takes the maps with it — so it must stay
        // eligible; only a true embedded sub-asset (`container` points at a *different* row) is
        // skipped here.
        let embedded_sub_asset = entry.container.value() != 0 && entry.container != entry.id;
        if reachable.contains(&entry.id.value()) || embedded_sub_asset {
            continue;
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
        let Some(entry) = assets.catalog.find(*id).cloned() else {
            continue;
        };
        crate::vegetation::remove_vegetation_map_package(assets, &entry)?;
        let path = entry.path.clone();
        let full = format!("{}/{path}", assets.root.display());
        let bytes = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&full);
        let _ = std::fs::remove_file(format!("{full}.smeta"));
        let removed = assets.delete_asset_entry(*id);
        debug_assert!(
            removed.is_some(),
            "validated unused asset remains catalogued"
        );
        result.deleted += 1;
        result.reclaimed_bytes += bytes;
        tracing::info!("delete-unused: removed '{path}' ({bytes} bytes)");
    }
    let _ = assets.scan_assets();
    assets.write_catalog_cache();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_scene::{AssetEntry, Mesh as MeshComponent};

    use super::super::material_import::import_material_folder;
    use super::super::test_support::{png_2x2, scratch, tri_mesh};

    #[test]
    fn clean_flags_an_unreferenced_standalone_asset_as_unused() {
        let dir = scratch("clean");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

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

        let report = analyze_clean(&mut scene, &mut assets, &[orphan]);
        assert!(
            !report
                .candidates
                .iter()
                .any(|c| c.id.value() == orphan.value() && c.category == CleanCategory::Unused),
            "an excluded asset is not Unused"
        );

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

    /// A material self-container is ONE deletable unit: an unused import is flagged `Unused` (its
    /// `.smatx`), its embedded maps are never separate candidates, and `delete_unused` removes the
    /// container file so the rescan drops the map rows with it.
    #[test]
    fn clean_deletes_an_unused_material_container_as_a_unit() {
        let dir = scratch("clean-mat");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        let folder = dir.join("Rock063");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Color.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_NormalGL.png"), png_2x2()).unwrap();
        std::fs::write(folder.join("Rock063_2K-PNG_Roughness.png"), png_2x2()).unwrap();
        let imported = import_material_folder(&mut assets, &folder.to_string_lossy(), "Rock 063")
            .expect("import");
        let smatx = assets
            .catalog
            .find(imported.material)
            .expect("material row")
            .path
            .clone();

        let mut scene = Scene::new();
        let report = analyze_clean(&mut scene, &mut assets, &[]);
        let unused: Vec<u64> = report
            .candidates
            .iter()
            .filter(|c| c.category == CleanCategory::Unused)
            .map(|c| c.id.value())
            .collect();
        assert!(
            unused.contains(&imported.material.value()),
            "the unused material self-container is a deletion candidate"
        );
        let map_ids: Vec<u64> = assets
            .catalog
            .entries
            .iter()
            .filter(|e| e.container == imported.material && e.id != imported.material)
            .map(|e| e.id.value())
            .collect();
        assert!(
            map_ids.iter().all(|id| !unused.contains(id)),
            "embedded maps are never flagged individually — the container is the unit"
        );

        let deleted =
            delete_unused(&mut assets, &mut scene, &[imported.material], true).expect("delete");
        assert_eq!(deleted.deleted, 1);
        assert!(!root.join(&smatx).exists(), "the .smatx container is gone");
        assert!(
            assets.catalog.find(imported.material).is_none(),
            "the material row is gone"
        );
        for id in &map_ids {
            assert!(
                assets.catalog.find(Uuid(*id)).is_none(),
                "the embedded map row is gone with its container"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
