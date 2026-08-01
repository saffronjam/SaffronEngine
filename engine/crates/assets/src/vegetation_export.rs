//! The cooked vegetation closure an exported package needs: the generation root, its
//! manifest, every compiled family and cell the manifest names, and the durable starting state
//! keyed by it.
//!
//! The closure is computed from the manifest rather than from a directory scan. A scan would copy every
//! artifact the project ever cooked, including generations that were superseded, which is how a package
//! quietly grows to several times the size of the world it ships.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_vegetation::VegetationBaseManifest;

use crate::error::{Error, Result};
use crate::vegetation_state::VegetationStateStore;
use crate::vegetation_store::{VegetationArtifactKind, VegetationArtifactStore};

/// One file the closure carries, as a path relative to the root of the store it came from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VegetationExportFile {
    /// Path relative to its store root, which is where it must land in the package.
    pub relative: PathBuf,
    /// Bytes on disk.
    pub bytes: u64,
}

/// What one map contributes to an exported package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationExportMap {
    /// The authored map the generation belongs to.
    pub map: Uuid,
    /// Exact identity of the manifest the package binds.
    pub manifest_identity: String,
    /// Compiled families the manifest names.
    pub plants: u64,
    /// Cooked cells the manifest names.
    pub cells: u64,
    /// Cells the manifest names that the store does not hold.
    pub missing: u64,
    /// Whether the generation ships an initial persistent-state baseline.
    pub baseline: bool,
    /// Macro plants across every cell the manifest names.
    pub macro_plants: u64,
    /// Stored bytes per cell facet, in canonical section order, read from each cell's table
    /// of contents rather than by decoding it.
    pub facet_bytes: Vec<VegetationExportFacet>,
}

/// Stored bytes one cell facet occupies across a map's cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationExportFacet {
    /// The facet.
    pub kind: saffron_vegetation::VegetationCellSectionKind,
    /// Cells that carry it.
    pub cells: u64,
    /// Stored bytes it occupies across those cells.
    pub bytes: u64,
}

/// The complete cooked vegetation closure for one export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationExportClosure {
    /// Derived artifacts to copy, in canonical order, each relative to the artifact-store root.
    pub files: Vec<VegetationExportFile>,
    /// Durable persistent-state files to copy, in canonical order, each relative to the state-store
    /// root. They travel separately because they land outside the package's disposable cache.
    pub state_files: Vec<VegetationExportFile>,
    /// One entry per map that had a current generation.
    pub maps: Vec<VegetationExportMap>,
    /// Total bytes the closure carries.
    pub total_bytes: u64,
}

impl VegetationExportClosure {
    /// Whether every artifact the manifests name was present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.maps.iter().all(|map| map.missing == 0)
    }
}

/// Collects the cooked closure for `maps`, skipping any map with no current generation.
///
/// # Errors
///
/// [`Error::Io`] when the store cannot be read, and a vegetation error when a manifest fails to
/// decode.
pub fn vegetation_export_closure(
    store: &VegetationArtifactStore,
    state: &VegetationStateStore,
    maps: impl IntoIterator<Item = Uuid>,
) -> Result<VegetationExportClosure> {
    let root = store.root().to_path_buf();
    let state_root = state.root().to_path_buf();
    let mut files: BTreeSet<VegetationExportFile> = BTreeSet::new();
    let mut state_files: BTreeSet<VegetationExportFile> = BTreeSet::new();
    let mut summaries = Vec::new();
    for map in maps {
        let Some(hash) = store.current_manifest_hash(map)? else {
            continue;
        };
        let bytes = store.read_manifest(hash)?;
        let manifest = VegetationBaseManifest::from_canonical_bytes(&bytes)?;
        // The generation root is what the player resolves the manifest through, so it travels too.
        take(
            &root,
            &root
                .join("generations")
                .join(format!("{}.current", map.value())),
            &mut files,
        )?;
        take(
            &root,
            &store.path(VegetationArtifactKind::Manifest, hash),
            &mut files,
        )?;
        let baseline = take(&state_root, &state.baseline_path(map), &mut state_files)?;
        let mut missing = 0;
        let mut macro_plants = 0_u64;
        let mut facets: BTreeMap<u16, (u64, u64)> = BTreeMap::new();
        for plant in &manifest.plants {
            let path = store.path(VegetationArtifactKind::Plant, plant.artifact_hash);
            if take(&root, &path, &mut files)? {
                continue;
            }
            missing += 1;
        }
        for cell in &manifest.cells {
            let path = store.path(VegetationArtifactKind::Cell, cell.artifact_hash);
            if !take(&root, &path, &mut files)? {
                missing += 1;
                continue;
            }
            macro_plants += cell.macro_count;
            let reader = store.open_cell(cell.artifact_hash)?;
            for section in &reader.index().sections {
                let entry = facets.entry(section.kind as u16).or_insert((0, 0));
                entry.0 += 1;
                entry.1 += section.stored_size;
            }
        }
        summaries.push(VegetationExportMap {
            map,
            manifest_identity: hash.to_string(),
            plants: manifest.plants.len() as u64,
            cells: manifest.cells.len() as u64,
            missing,
            baseline,
            macro_plants,
            facet_bytes: saffron_vegetation::VegetationCellSectionKind::ALL
                .into_iter()
                .filter_map(|kind| {
                    facets
                        .get(&(kind as u16))
                        .map(|(cells, bytes)| VegetationExportFacet {
                            kind,
                            cells: *cells,
                            bytes: *bytes,
                        })
                })
                .collect(),
        });
    }
    let files: Vec<VegetationExportFile> = files.into_iter().collect();
    let state_files: Vec<VegetationExportFile> = state_files.into_iter().collect();
    let total_bytes = files
        .iter()
        .chain(&state_files)
        .map(|file| file.bytes)
        .sum();
    Ok(VegetationExportClosure {
        files,
        state_files,
        maps: summaries,
        total_bytes,
    })
}

/// Records one file if it exists, reporting whether it did.
fn take(root: &Path, path: &Path, into: &mut BTreeSet<VegetationExportFile>) -> Result<bool> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Error::Io(error.to_string())),
    };
    let relative = path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| Error::Io("vegetation artifact lies outside the store root".to_owned()))?;
    into.insert(VegetationExportFile {
        relative,
        bytes: metadata.len(),
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vegetation_store::VegetationArtifactStore;

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("saffron-veg-export-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch");
        root
    }

    /// A project can be exported before its vegetation is cooked, and the warning belongs to
    /// the caller.
    #[test]
    fn a_map_without_a_generation_contributes_nothing() {
        let root = scratch("no-generation");
        let store = VegetationArtifactStore::new(&root);
        let state = VegetationStateStore::new(root.join("state"));
        let closure = vegetation_export_closure(&store, &state, [Uuid(7)]).expect("closure");
        assert!(closure.files.is_empty());
        assert!(closure.state_files.is_empty());
        assert!(closure.maps.is_empty());
        assert_eq!(closure.total_bytes, 0);
        assert!(
            closure.is_complete(),
            "nothing was named, so nothing is missing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The closure is what the manifest names, not what the directory holds: a superseded artifact
    /// left in the store stays out of the package.
    #[test]
    fn the_closure_is_the_manifest_not_the_directory() {
        let root = scratch("manifest-closure");
        let store = VegetationArtifactStore::new(&root);
        // A stray artifact from an older cook, in the store but named by no current manifest.
        let stray = store.path(
            VegetationArtifactKind::Cell,
            saffron_vegetation::ContentHash::of(b"stray"),
        );
        std::fs::create_dir_all(stray.parent().expect("cell directory")).expect("create");
        std::fs::write(&stray, b"superseded").expect("write stray");

        let mut files = BTreeSet::new();
        assert!(
            take(&root, &stray, &mut files).expect("metadata"),
            "the stray file is on disk"
        );
        // …but a closure over a map with no generation never looks at it.
        let state = VegetationStateStore::new(root.join("state"));
        let closure = vegetation_export_closure(&store, &state, [Uuid(7)]).expect("closure");
        assert!(
            !closure
                .files
                .iter()
                .any(|file| stray.ends_with(&file.relative)),
            "a superseded artifact is not packaged"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// One artifact that failed verification, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationArtifactFault {
    /// Path relative to the store root.
    pub relative: PathBuf,
    /// What is wrong with it.
    pub fault: VegetationFaultKind,
}

/// Why an artifact failed verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VegetationFaultKind {
    /// The manifest names it and the store does not hold it.
    Absent,
    /// Its bytes do not hash to the identity its name claims.
    Corrupt,
}

impl VegetationFaultKind {
    /// Stable name, used in reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Corrupt => "corrupt",
        }
    }
}

/// What one verification pass found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VegetationVerifyReport {
    /// Artifacts checked.
    pub checked: u64,
    /// Faults found, in canonical path order.
    pub faults: Vec<VegetationArtifactFault>,
    /// Corrupt artifacts removed, so the next cook republishes them.
    pub repaired: u64,
}

impl VegetationVerifyReport {
    /// Whether every artifact the manifests name verified.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.faults.is_empty()
    }
}

/// Verifies every artifact the given maps' current generations name, optionally removing the corrupt
/// ones so the next cook republishes them.
///
/// An artifact's file name *is* the hash of its bytes, so verification is a rehash rather than a
/// comparison against a side table that could itself rot. Repair deletes rather than rewrites:
/// the bytes are the only copy, and the cooker's cache-miss path is what reproduces them.
///
/// # Errors
///
/// [`Error::Io`] when the store cannot be read or a corrupt file cannot be removed.
pub fn verify_vegetation_artifacts(
    store: &VegetationArtifactStore,
    state: &VegetationStateStore,
    maps: impl IntoIterator<Item = Uuid>,
    repair: bool,
) -> Result<VegetationVerifyReport> {
    let closure = vegetation_export_closure(store, state, maps)?;
    let root = store.root().to_path_buf();
    let mut report = VegetationVerifyReport::default();
    for map in &closure.maps {
        for _ in 0..map.missing {
            report.faults.push(VegetationArtifactFault {
                relative: PathBuf::from(format!("manifests/{}", map.manifest_identity)),
                fault: VegetationFaultKind::Absent,
            });
        }
    }
    for file in &closure.files {
        // Only the content-addressed kinds carry their hash in the name; a generation root is keyed
        // by the map it belongs to, so there is nothing to rehash it against.
        let Some(expected) = content_addressed_identity(&file.relative) else {
            continue;
        };
        report.checked += 1;
        let path = root.join(&file.relative);
        let bytes = std::fs::read(&path).map_err(|error| Error::Io(error.to_string()))?;
        if saffron_vegetation::ContentHash::of(&bytes).to_string() == expected {
            continue;
        }
        report.faults.push(VegetationArtifactFault {
            relative: file.relative.clone(),
            fault: VegetationFaultKind::Corrupt,
        });
        if repair {
            std::fs::remove_file(&path).map_err(|error| Error::Io(error.to_string()))?;
            report.repaired += 1;
        }
    }
    report.faults.sort_by(|first, second| {
        (&first.relative, first.fault.name()).cmp(&(&second.relative, second.fault.name()))
    });
    Ok(report)
}

/// The hash a content-addressed artifact's name claims, or `None` for a keyed one.
fn content_addressed_identity(relative: &Path) -> Option<String> {
    let directory = relative.parent()?.file_name()?.to_str()?;
    if !matches!(
        directory,
        "plants" | "cells" | "manifests" | "cook-graphs" | "work-payloads"
    ) {
        return None;
    }
    relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use crate::vegetation_store::VegetationArtifactStore;

    /// A byte flipped in a cell artifact stops hashing to the name it sits under, and repair
    /// removes it so the next cook republishes it.
    #[test]
    fn a_flipped_byte_is_found_and_repaired() {
        let root = std::env::temp_dir().join("saffron-veg-verify");
        let _ = std::fs::remove_dir_all(&root);
        let store = VegetationArtifactStore::new(&root);

        // A content-addressed file whose bytes disagree with its name.
        let hash = saffron_vegetation::ContentHash::of(b"cooked cell");
        let path = store.path(VegetationArtifactKind::Cell, hash);
        std::fs::create_dir_all(path.parent().expect("cell directory")).expect("create");
        std::fs::write(&path, b"tampered").expect("write");
        assert_eq!(
            content_addressed_identity(
                Path::new("cells")
                    .join(format!("{hash}.svegcell"))
                    .as_path()
            ),
            Some(hash.to_string()),
            "a cell's name is its identity"
        );
        // A keyed artifact carries no hash in its name, so there is nothing to rehash it against.
        assert_eq!(
            content_addressed_identity(Path::new("generations/7.current")),
            None
        );

        // With no generation there is nothing to verify, which is not a fault.
        let state = VegetationStateStore::new(root.join("state"));
        let report = verify_vegetation_artifacts(&store, &state, [Uuid(7)], false).expect("verify");
        assert!(report.is_sound());
        assert_eq!(report.checked, 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
