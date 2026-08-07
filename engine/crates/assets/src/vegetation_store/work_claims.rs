//! The distributed cook's on-disk claim protocol: one exclusive claim per work item, a
//! separate completion marker, and a lease sweep for claimants that died mid-item.

use std::io::Write;
use std::path::PathBuf;

use atomic_write_file::AtomicWriteFile;
use saffron_vegetation::ContentHash;

use super::VegetationArtifactStore;
use crate::{Error, Result};

impl VegetationArtifactStore {
    /// The claim/completion directory for one work manifest's items.
    fn work_claims_dir(&self, manifest: ContentHash) -> PathBuf {
        self.root.join("work-claims").join(manifest.to_string())
    }

    /// Claims one work item for `claimant`. Atomic across processes: exactly one caller ever
    /// sees `true` for an index while its claim file exists. `false` means another claimant
    /// holds it.
    pub fn claim_work_item(
        &self,
        manifest: ContentHash,
        index: u32,
        claimant: &str,
    ) -> Result<bool> {
        let dir = self.work_claims_dir(manifest);
        std::fs::create_dir_all(&dir).map_err(|error| Error::Io(error.to_string()))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(format!("{index}.claim")))
        {
            Ok(mut file) => {
                file.write_all(claimant.as_bytes())
                    .map_err(|error| Error::Io(error.to_string()))?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(Error::Io(error.to_string())),
        }
    }

    /// Records one item's completion, atomically and idempotently. The record is validated
    /// before it lands; the completion marker is separate from the claim so a swept claim
    /// never erases a finished result.
    pub fn complete_work_item(
        &self,
        manifest: ContentHash,
        index: u32,
        bytes: &[u8],
    ) -> Result<()> {
        saffron_vegetation::CookWorkCompletion::from_canonical_bytes(bytes)?;
        let dir = self.work_claims_dir(manifest);
        std::fs::create_dir_all(&dir).map_err(|error| Error::Io(error.to_string()))?;
        let mut file = AtomicWriteFile::options()
            .open(dir.join(format!("{index}.done")))
            .map_err(|error| Error::Io(error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.commit().map_err(|error| Error::Io(error.to_string()))
    }

    /// Reads one item's validated completion record, absent while the item is unfinished.
    pub fn read_work_completion_if_present(
        &self,
        manifest: ContentHash,
        index: u32,
    ) -> Result<Option<saffron_vegetation::CookWorkCompletion>> {
        let path = self.work_claims_dir(manifest).join(format!("{index}.done"));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                saffron_vegetation::CookWorkCompletion::from_canonical_bytes(&bytes)?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(error.to_string())),
        }
    }

    /// Removes claims whose lease expired without a completion, returning how many were swept.
    /// Safe because publication is idempotent by content address: a swept claimant that later
    /// finishes republishes bytes that already exist.
    pub fn sweep_stale_work_claims(
        &self,
        manifest: ContentHash,
        lease: std::time::Duration,
    ) -> Result<u32> {
        let dir = self.work_claims_dir(manifest);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(Error::Io(error.to_string())),
        };
        let now = std::time::SystemTime::now();
        let mut swept = 0_u32;
        for entry in entries {
            let entry = entry.map_err(|error| Error::Io(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("claim") {
                continue;
            }
            let done = path.with_extension("done");
            if done.exists() {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(|error| Error::Io(error.to_string()))?;
            let expired = now.duration_since(modified).is_ok_and(|age| age > lease);
            if expired {
                std::fs::remove_file(&path).map_err(|error| Error::Io(error.to_string()))?;
                swept += 1;
            }
        }
        Ok(swept)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::root;
    use super::*;
    use saffron_core::Uuid;
    use saffron_spatial::WorldCellKey;
    use saffron_vegetation::{CookNodeAddress, CookNodeRecord, CookWorkActual, CookWorkEstimate};

    #[test]
    fn a_work_claim_is_exclusive_and_a_zero_lease_sweep_frees_it() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let plan = ContentHash::of(b"work plan");
        assert!(store.claim_work_item(plan, 3, "a").unwrap());
        assert!(!store.claim_work_item(plan, 3, "b").unwrap());
        assert!(store.claim_work_item(plan, 4, "b").unwrap());

        // Sweeping under a zero lease frees the unfinished claims, and a freed index claims
        // again — the resume path after a dead claimant.
        assert_eq!(
            store
                .sweep_stale_work_claims(plan, std::time::Duration::ZERO)
                .unwrap(),
            2
        );
        assert!(store.claim_work_item(plan, 3, "b").unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_completed_item_survives_the_sweep_and_round_trips() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let plan = ContentHash::of(b"work plan");
        let cell = WorldCellKey::base(0, 0, 0);
        let estimate = CookWorkEstimate {
            work_units: 1,
            peak_memory_bytes: 2,
            input_bytes: 3,
            output_bytes: 4,
        };
        let completion = saffron_vegetation::CookWorkCompletion {
            item: 5,
            node: CookNodeRecord {
                address: CookNodeAddress::Cell {
                    map: Uuid(10),
                    cell,
                },
                cook_key: ContentHash::of(b"key"),
                output_hash: ContentHash::of(b"output"),
                dependencies: Vec::new(),
                estimate,
                actual: CookWorkActual {
                    elapsed_micros: 6,
                    cache_hit: true,
                    ..CookWorkActual::default()
                },
            },
            actual: CookWorkActual {
                elapsed_micros: 6,
                cache_hit: true,
                ..CookWorkActual::default()
            },
            manifest_cell: saffron_vegetation::VegetationManifestCell {
                cell,
                bounds: cell.bounds(),
                artifact_hash: ContentHash::of(b"output"),
                payload_hash: ContentHash::of(b"payload"),
                dependencies: Vec::new(),
                species_counts: Vec::new(),
                macro_count: 0,
                micro_count: 0,
                resident_memory_bytes: 2,
                stored_bytes: 7,
                estimate,
                actual: CookWorkActual {
                    elapsed_micros: 6,
                    cache_hit: true,
                    ..CookWorkActual::default()
                },
                sections: Vec::new(),
            },
        };
        assert!(store.claim_work_item(plan, 5, "a").unwrap());
        store
            .complete_work_item(plan, 5, &completion.canonical_bytes().unwrap())
            .unwrap();
        assert_eq!(
            store
                .sweep_stale_work_claims(plan, std::time::Duration::ZERO)
                .unwrap(),
            0,
            "a finished claim is never swept"
        );
        let read = store.read_work_completion_if_present(plan, 5).unwrap();
        assert_eq!(read, Some(completion));
        assert_eq!(
            store.read_work_completion_if_present(plan, 6).unwrap(),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn racing_claimants_split_a_plan_without_sharing_an_item() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let plan = ContentHash::of(b"racing plan");
        let items = 64_u32;
        let claims: Vec<Vec<u32>> = std::thread::scope(|scope| {
            let store = &store;
            let workers: Vec<_> = (0..4)
                .map(|worker| {
                    scope.spawn(move || {
                        let claimant = format!("claimant-{worker}");
                        (0..items)
                            .filter(|&item| store.claim_work_item(plan, item, &claimant).unwrap())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("claimant"))
                .collect()
        });
        let mut seen = std::collections::BTreeSet::new();
        for claimed in &claims {
            for &item in claimed {
                assert!(seen.insert(item), "item {item} was claimed twice");
            }
        }
        assert_eq!(
            seen.len() as u32,
            items,
            "every item was claimed exactly once"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
