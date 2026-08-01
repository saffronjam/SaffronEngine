//! Durable project-owned storage for persistent vegetation state.
//!
//! Separate from the derived-artifact CAS on purpose. A generation's cells, families, manifest, and
//! cook graph are reproducible from authored sources, so `<project>/cache/vegetation/` may be
//! deleted at any time. A baseline is not: it is a snapshot of runtime mutations an author chose to
//! keep, and no authored source regenerates it. It therefore lives under
//! `<project>/state/vegetation/`, which nothing treats as a cache — one baseline per authored map,
//! naming inside itself the generation it was reduced against.

use std::io::Write;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_vegetation::{ContentHash, VegetationState};

use crate::{Error, Result};

/// Project-owned durable store for persistent vegetation state.
#[derive(Clone, Debug)]
pub struct VegetationStateStore {
    root: PathBuf,
}

impl VegetationStateStore {
    /// Binds the store to a durable project-owned root outside the disposable artifact cache.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store root holding the persistent-state namespaces.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves the baseline path for one authored vegetation map.
    #[must_use]
    pub fn baseline_path(&self, map: Uuid) -> PathBuf {
        self.root
            .join("baselines")
            .join(format!("{}.svegstate", map.value()))
    }

    /// Publishes the map's persistent-state baseline: the state a world binding this map boots
    /// into, keyed by the map rather than by a generation, because a recook replaces the
    /// generation and must not orphan the one file no cook reproduces.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the write fails, and a vegetation error when the snapshot does not decode
    /// against `manifest`.
    pub fn publish_baseline(
        &self,
        map: Uuid,
        manifest: ContentHash,
        bytes: &[u8],
    ) -> Result<PathBuf> {
        saffron_vegetation::VegetationState::from_canonical_bytes(bytes, manifest.bytes())?;
        let path = self.baseline_path(map);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| Error::Io(error.to_string()))?;
        }
        let mut file = AtomicWriteFile::options()
            .open(&path)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.commit()
            .map_err(|error| Error::Io(error.to_string()))?;
        Ok(path)
    }

    /// Reads the map's baseline, decoded against whichever generation it was recorded on, and
    /// absent when the map has none.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the read fails, and a vegetation error when the container is malformed.
    pub fn read_baseline_if_present(&self, map: Uuid) -> Result<Option<VegetationState>> {
        match std::fs::read(self.baseline_path(map)) {
            Ok(bytes) => Ok(Some(VegetationState::from_canonical_bytes_as_declared(
                &bytes,
            )?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::{PlantId, WorldCellKey};
    use saffron_vegetation::{
        GraphCancellationToken, MutationHeader, VegetationMutation, VegetationMutationRecord,
        VegetationState, reduce_mutations,
    };

    use crate::AssetServer;
    use crate::vegetation_cooker::test_support::{
        Scratch, cook_request, run_cook, save_populated_map,
    };

    /// Deleting the whole derived-artifact cache costs nothing that cannot be recomputed: the
    /// authored documents and the published baseline are untouched, and the recook lands on the
    /// same generation identity the baseline is keyed to.
    #[test]
    fn deleting_the_artifact_cache_loses_no_authored_or_persistent_bytes() {
        let scratch = Scratch::new("cache-deletion");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(0, 0, 0);
        let populated = save_populated_map(&mut assets, &[cell], 0).unwrap();
        let cancellation = GraphCancellationToken::default();
        let cooked = run_cook(
            &mut assets,
            cook_request(Uuid(303), populated.map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert!(
            cooked
                .manifest
                .cells
                .iter()
                .all(|cell| cell.macro_count > 0),
            "the survival below is measured over a populated generation"
        );

        // Persistent state an author chose to keep: no authored source reproduces it.
        let identity = cooked.manifest_identity;
        let mut state = VegetationState::new(identity.bytes());
        reduce_mutations(
            &mut state,
            identity.bytes(),
            &[VegetationMutationRecord {
                header: MutationHeader {
                    cell,
                    transaction: 1,
                    authority: 1,
                    logical_tick: 1,
                    idempotency_key: 1,
                    base_revision: None,
                },
                mutation: VegetationMutation::Tombstone {
                    plant: PlantId::explicit([9; 16]).unwrap(),
                },
            }],
        )
        .unwrap();
        let baseline_bytes = state.canonical_bytes().unwrap();
        let store = assets.vegetation_state_store();
        let baseline_path = store
            .publish_baseline(populated.map, identity, &baseline_bytes)
            .unwrap();
        assert!(
            !baseline_path.starts_with(&assets.vegetation_cache_root),
            "persistent state must not live inside the disposable cache"
        );

        let authored: Vec<(std::path::PathBuf, Vec<u8>)> =
            [populated.plant, populated.biome, populated.map]
                .into_iter()
                .flat_map(|id| {
                    let entry = assets.catalog().find(id).expect("catalogued asset").clone();
                    let path = assets.root.join(&entry.path);
                    let package = assets.root.join(format!("{}.data", entry.path));
                    std::iter::once(path).chain(walk(&package))
                })
                .map(|path| {
                    let bytes = std::fs::read(&path).expect("authored bytes");
                    (path, bytes)
                })
                .collect();
        assert!(
            authored.len() > 2,
            "the map's chunk objects are authored bytes too"
        );

        std::fs::remove_dir_all(&assets.vegetation_cache_root).unwrap();
        assert!(!assets.vegetation_cache_root.exists());
        for (path, expected) in &authored {
            assert_eq!(&std::fs::read(path).unwrap(), expected);
        }
        assert_eq!(
            store
                .read_baseline_if_present(populated.map)
                .unwrap()
                .map(|state| state.canonical_bytes().unwrap()),
            Some(baseline_bytes)
        );

        // The cache is reproducible, so the baseline still binds after the recook.
        let rebuilt = run_cook(
            &mut assets,
            cook_request(Uuid(303), populated.map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        assert_eq!(rebuilt.manifest_identity, identity);
    }

    fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
        found.sort();
        found
    }
}
