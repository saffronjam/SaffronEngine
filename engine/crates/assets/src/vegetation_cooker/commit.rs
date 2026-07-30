//! Committing a staged generation: the authored-source transaction journal, the live-input
//! revalidation, and the atomic generation-root advance.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Arc;

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_spatial::SurfaceField;
use saffron_vegetation::{
    ContentHash, CookDependencyAddress, GraphCancellationToken, write_plant_asset,
};

use crate::{AssetServer, Error, Result, VegetationArtifactStore, update_plant_family_asset};

use super::{
    PlantSourceAcceptance, StagedVegetationCook, VegetationCookOutput, cancellation_checkpoint,
};
use crate::vegetation_store::optional_hash;

/// Commits one fully staged generation after revalidating every live authored input.
pub fn commit_staged_vegetation_cook(
    assets: &mut AssetServer,
    current_surfaces: &[Arc<dyn SurfaceField>],
    staged: StagedVegetationCook,
    cancellation: &GraphCancellationToken,
) -> Result<VegetationCookOutput> {
    cancellation_checkpoint(cancellation)?;
    if assets.root != staged.project.asset_root
        || assets.vegetation_cache_root != staged.project.cache_root
    {
        return Err(Error::VegetationCookInputChanged {
            path: staged.project.asset_root.display().to_string(),
        });
    }
    let store = assets.vegetation_artifact_store();
    let _authored_lock = store.lock_authored()?;
    let generation_lock = store.lock_generation(staged.map)?;
    recover_source_transaction(assets, &store, staged.map)?;
    validate_live_catalog(assets, &staged)?;
    for guard in &staged.authored_guards {
        guard.validate()?;
    }
    let mut surface_descriptors = current_surfaces
        .iter()
        .map(|provider| provider.descriptor())
        .collect::<Vec<_>>();
    surface_descriptors.sort_by_key(|descriptor| (descriptor.id, descriptor.revision));
    if surface_descriptors != staged.surface_descriptors {
        return Err(Error::VegetationCookInputChanged {
            path: "surface-providers".to_owned(),
        });
    }
    if store.current_manifest_hash(staged.map)? != staged.expected_manifest {
        return Err(Error::VegetationGenerationSuperseded {
            map: staged.map.value(),
            expected: optional_hash(staged.expected_manifest),
            current: optional_hash(store.current_manifest_hash(staged.map)?),
        });
    }

    let mut originals = Vec::with_capacity(staged.source_acceptances.len());
    for acceptance in &staged.source_acceptances {
        let original = crate::load_plant_family_asset(assets, acceptance.family)?;
        if ContentHash::of(&write_plant_asset(&original)?) != acceptance.expected_authored_hash {
            return Err(Error::VegetationCookInputChanged {
                path: format!("plant-family/{}", acceptance.family.value()),
            });
        }
        originals.push(original);
    }
    let mut installed = 0_usize;
    let journal = SourceTransactionJournal {
        map: staged.map,
        expected_manifest: staged.expected_manifest,
        new_manifest: staged.manifest_identity,
        entries: staged
            .source_acceptances
            .iter()
            .zip(&originals)
            .map(|(acceptance, original)| {
                Ok(SourceTransactionEntry {
                    family: acceptance.family,
                    old_bytes: write_plant_asset(original)?,
                    new_bytes: write_plant_asset(&acceptance.accepted_asset)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let journal_path = source_transaction_path(&store, staged.map);
    write_source_transaction(&journal_path, &journal)?;
    for acceptance in &staged.source_acceptances {
        if let Err(error) = cancellation_checkpoint(cancellation) {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
        if let Err(error) =
            update_plant_family_asset(assets, acceptance.family, &acceptance.accepted_asset)
        {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
        installed += 1;
    }
    let publication = match store.publish_generation_locked(
        &generation_lock,
        staged.expected_manifest,
        &staged.manifest_bytes,
    ) {
        Ok(publication) => publication,
        Err(error) => {
            rollback_source_acceptances(assets, &staged.source_acceptances, &originals, installed)?;
            let _ = std::fs::remove_file(&journal_path);
            return Err(error);
        }
    };
    let _ = std::fs::remove_file(journal_path);
    Ok(VegetationCookOutput {
        cook_graph: staged.cook_graph,
        manifest: staged.manifest,
        manifest_identity: staged.manifest_identity,
        publication,
        statistics: staged.statistics,
    })
}

const SOURCE_TRANSACTION_MAGIC: &[u8; 8] = b"SVTXN001";

struct SourceTransactionEntry {
    family: Uuid,
    old_bytes: Vec<u8>,
    new_bytes: Vec<u8>,
}

struct SourceTransactionJournal {
    map: Uuid,
    expected_manifest: Option<ContentHash>,
    new_manifest: ContentHash,
    entries: Vec<SourceTransactionEntry>,
}

fn source_transaction_path(store: &VegetationArtifactStore, map: Uuid) -> std::path::PathBuf {
    store
        .root()
        .join("transactions")
        .join(format!("map-{}.txn", map.value()))
}

fn write_source_transaction(
    path: &std::path::Path,
    journal: &SourceTransactionJournal,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Io("vegetation transaction journal has no parent".to_owned()))?;
    std::fs::create_dir_all(parent).map_err(|error| Error::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SOURCE_TRANSACTION_MAGIC);
    bytes.extend_from_slice(&journal.map.value().to_be_bytes());
    bytes.push(u8::from(journal.expected_manifest.is_some()));
    bytes.extend_from_slice(&journal.expected_manifest.unwrap_or_default().bytes());
    bytes.extend_from_slice(&journal.new_manifest.bytes());
    bytes.extend_from_slice(
        &u32::try_from(journal.entries.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?
            .to_be_bytes(),
    );
    for entry in &journal.entries {
        bytes.extend_from_slice(&entry.family.value().to_be_bytes());
        append_journal_bytes(&mut bytes, &entry.old_bytes)?;
        append_journal_bytes(&mut bytes, &entry.new_bytes)?;
    }
    let mut file = AtomicWriteFile::options()
        .open(path)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.write_all(&bytes)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.commit().map_err(|error| Error::Io(error.to_string()))
}

fn append_journal_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    output.extend_from_slice(
        &u64::try_from(bytes.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?
            .to_be_bytes(),
    );
    output.extend_from_slice(bytes);
    Ok(())
}

fn recover_source_transaction(
    assets: &mut AssetServer,
    store: &VegetationArtifactStore,
    map: Uuid,
) -> Result<()> {
    let path = source_transaction_path(store, map);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::Io(error.to_string())),
    };
    let journal = read_source_transaction(&bytes)?;
    if journal.map != map {
        return Err(Error::Io(
            "vegetation transaction journal belongs to another map".to_owned(),
        ));
    }
    let current = store.current_manifest_hash(map)?;
    let use_new = if current == journal.expected_manifest {
        false
    } else if current == Some(journal.new_manifest) {
        true
    } else {
        return Err(Error::VegetationTransactionConflict {
            map: map.value(),
            current: optional_hash(current),
        });
    };
    for entry in &journal.entries {
        let bytes = if use_new {
            &entry.new_bytes
        } else {
            &entry.old_bytes
        };
        let asset = saffron_vegetation::read_plant_asset(bytes)?;
        if asset.id != entry.family {
            return Err(Error::Io(
                "vegetation transaction plant identity is invalid".to_owned(),
            ));
        }
        update_plant_family_asset(assets, entry.family, &asset)?;
    }
    std::fs::remove_file(path).map_err(|error| Error::Io(error.to_string()))
}

fn read_source_transaction(bytes: &[u8]) -> Result<SourceTransactionJournal> {
    let mut cursor = 0_usize;
    if take_journal(bytes, &mut cursor, SOURCE_TRANSACTION_MAGIC.len())? != SOURCE_TRANSACTION_MAGIC
    {
        return Err(Error::Io(
            "vegetation transaction journal magic is invalid".to_owned(),
        ));
    }
    let map = Uuid(read_journal_u64(bytes, &mut cursor)?);
    let expected_present = match take_journal(bytes, &mut cursor, 1)?[0] {
        0 => false,
        1 => true,
        _ => {
            return Err(Error::Io(
                "vegetation transaction expected-root flag is invalid".to_owned(),
            ));
        }
    };
    let expected_hash = ContentHash::new(read_journal_array(bytes, &mut cursor)?);
    if expected_present == expected_hash.is_zero() {
        return Err(Error::Io(
            "vegetation transaction expected root is invalid".to_owned(),
        ));
    }
    let expected_manifest = expected_present.then_some(expected_hash);
    let new_manifest = ContentHash::new(read_journal_array(bytes, &mut cursor)?);
    if new_manifest.is_zero() {
        return Err(Error::Io(
            "vegetation transaction new root is invalid".to_owned(),
        ));
    }
    let count = usize::try_from(read_journal_u32(bytes, &mut cursor)?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    if count > bytes.len().saturating_sub(cursor) / 24 {
        return Err(Error::Io(
            "vegetation transaction entry count exceeds its payload".to_owned(),
        ));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let family = Uuid(read_journal_u64(bytes, &mut cursor)?);
        let old_bytes = read_journal_bytes(bytes, &mut cursor)?;
        let new_bytes = read_journal_bytes(bytes, &mut cursor)?;
        if family.value() == 0 {
            return Err(Error::Io(
                "vegetation transaction plant identity is invalid".to_owned(),
            ));
        }
        entries.push(SourceTransactionEntry {
            family,
            old_bytes,
            new_bytes,
        });
    }
    if cursor != bytes.len() {
        return Err(Error::Io(
            "vegetation transaction journal has trailing bytes".to_owned(),
        ));
    }
    Ok(SourceTransactionJournal {
        map,
        expected_manifest,
        new_manifest,
        entries,
    })
}

fn read_journal_bytes(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>> {
    let length = usize::try_from(read_journal_u64(bytes, cursor)?)
        .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?;
    Ok(take_journal(bytes, cursor, length)?.to_vec())
}

fn read_journal_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32> {
    Ok(u32::from_be_bytes(read_journal_array(bytes, cursor)?))
}

fn read_journal_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    Ok(u64::from_be_bytes(read_journal_array(bytes, cursor)?))
}

fn read_journal_array<const N: usize>(bytes: &[u8], cursor: &mut usize) -> Result<[u8; N]> {
    take_journal(bytes, cursor, N)?
        .try_into()
        .map_err(|_| Error::Io("vegetation transaction journal is truncated".to_owned()))
}

fn take_journal<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| Error::Io("vegetation transaction journal length overflowed".to_owned()))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| Error::Io("vegetation transaction journal is truncated".to_owned()))?;
    *cursor = end;
    Ok(value)
}

fn validate_live_catalog(assets: &AssetServer, staged: &StagedVegetationCook) -> Result<()> {
    let mut ids = BTreeSet::from([staged.map.value()]);
    ids.extend(
        staged
            .manifest
            .plants
            .iter()
            .map(|plant| plant.family.value()),
    );
    for dependency in &staged.manifest.dependencies {
        match dependency.address {
            CookDependencyAddress::SourceAsset { asset }
            | CookDependencyAddress::MaterialCoverage { material: asset } => {
                ids.insert(asset.value());
            }
            CookDependencyAddress::SourceFile { .. }
            | CookDependencyAddress::BiomeIr { .. }
            | CookDependencyAddress::MapManifest { .. }
            | CookDependencyAddress::MapObject { .. }
            | CookDependencyAddress::SurfaceProvider { .. }
            | CookDependencyAddress::SurfaceTile { .. }
            | CookDependencyAddress::Contract { .. }
            | CookDependencyAddress::Node(_) => {}
        }
    }
    let mut pending = ids.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let expected = staged.project.catalog.find(Uuid(id));
        let current = assets.catalog.find(Uuid(id));
        if expected != current {
            return Err(Error::VegetationCookInputChanged {
                path: format!("catalog/{id}"),
            });
        }
        if let Some(entry) = expected
            && entry.container.value() != 0
            && ids.insert(entry.container.value())
        {
            pending.push(entry.container.value());
        }
    }
    Ok(())
}

fn rollback_source_acceptances(
    assets: &mut AssetServer,
    acceptances: &[PlantSourceAcceptance],
    originals: &[saffron_vegetation::PlantFamilyAsset],
    installed: usize,
) -> Result<()> {
    for index in (0..installed).rev() {
        update_plant_family_asset(assets, acceptances[index].family, &originals[index])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::stage_vegetation_cook;
    use super::super::test_support::{Scratch, cook_request, save_populated_map};
    use super::*;
    use crate::CookProjectView;
    use saffron_spatial::WorldCellKey;

    #[test]
    fn cancelled_staged_cook_never_advances_the_generation_root() {
        let scratch = Scratch::new("cancelled-stage");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(0, 0, 0);
        let map = save_populated_map(&mut assets, &[cell], 0).unwrap().map;
        let cancellation = GraphCancellationToken::default();
        let staged = stage_vegetation_cook(
            CookProjectView::capture(&assets),
            cook_request(Uuid(99), map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        cancellation.cancel();
        assert!(matches!(
            commit_staged_vegetation_cook(&mut assets, &[], staged, &cancellation),
            Err(Error::Vegetation(saffron_vegetation::Error::GraphCancelled))
        ));
        assert_eq!(
            assets
                .vegetation_artifact_store()
                .current_manifest_hash(map)
                .unwrap(),
            None
        );
    }

    #[test]
    fn authored_edit_after_staging_rejects_commit_without_a_root() {
        let scratch = Scratch::new("changed-input");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let cell = WorldCellKey::base(0, 0, 0);
        let map = save_populated_map(&mut assets, &[cell], 0).unwrap().map;
        let cancellation = GraphCancellationToken::default();
        let staged = stage_vegetation_cook(
            CookProjectView::capture(&assets),
            cook_request(Uuid(100), map, None, vec![cell], 1),
            &cancellation,
            |_| {},
        )
        .unwrap();
        let entry = assets.catalog.find(map).unwrap();
        let path = assets.root.join(&entry.path);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0);
        std::fs::write(path, bytes).unwrap();
        assert!(matches!(
            commit_staged_vegetation_cook(&mut assets, &[], staged, &cancellation),
            Err(Error::VegetationCookInputChanged { .. })
        ));
        assert_eq!(
            assets
                .vegetation_artifact_store()
                .current_manifest_hash(map)
                .unwrap(),
            None
        );
    }

    #[test]
    fn source_transaction_journal_is_strict_and_roundtrips() {
        let scratch = Scratch::new("transaction-codec");
        let path = scratch.path().join("journal.txn");
        let journal = SourceTransactionJournal {
            map: Uuid(41),
            expected_manifest: Some(ContentHash::of(b"old")),
            new_manifest: ContentHash::of(b"new"),
            entries: vec![SourceTransactionEntry {
                family: Uuid(42),
                old_bytes: vec![1, 2, 3],
                new_bytes: vec![4, 5],
            }],
        };
        write_source_transaction(&path, &journal).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let decoded = read_source_transaction(&bytes).unwrap();
        assert_eq!(decoded.map, journal.map);
        assert_eq!(decoded.expected_manifest, journal.expected_manifest);
        assert_eq!(decoded.new_manifest, journal.new_manifest);
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].family, Uuid(42));
        assert_eq!(decoded.entries[0].old_bytes, vec![1, 2, 3]);
        assert_eq!(decoded.entries[0].new_bytes, vec![4, 5]);
        assert!(read_source_transaction(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(read_source_transaction(&trailing).is_err());
    }
}
