//! The `.svegmap` root and its sparse immutable object package: snapshots, spatial tile
//! resolution, and the optimistic multi-object transaction that publishes the root last.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_scene::{AssetEntry, AssetType};
use saffron_spatial::WorldCellKey;
use saffron_vegetation::{
    VegetationMapAsset, VegetationMapChunk, VegetationMapChunkKey, VegetationMapChunkKind,
    VegetationMapChunkPayload, VegetationMapSnapshot, VegetationMapTileKey,
    VegetationMapTileSnapshot, read_vegetation_map_asset as decode_map,
    write_vegetation_map_asset as encode_map, write_vegetation_map_chunk,
};

use crate::cook_reader::CookAssetAccess;
use crate::import::hash_bytes_fnv;
use crate::{AssetServer, Error, Result};

use super::catalog_io::{atomic_write, read_typed_asset_from, save_typed_asset, typed_entry_from};
use super::map_objects::{
    lock_map_package, map_object_path, map_object_reference, map_package_directory,
    read_map_object, read_map_object_from, rollback_map_objects, rollback_new_asset,
};

/// One optimistic multi-object authored map transaction.
#[derive(Clone, Debug)]
pub struct VegetationMapTransaction {
    /// Root generation captured before editing began.
    pub expected_generation: u64,
    /// Complete immutable replacements for logical object keys.
    pub upserts: Vec<VegetationMapChunk>,
    /// Logical object keys removed from the sparse root inventory.
    pub removals: Vec<VegetationMapChunkKey>,
}

/// Reads the atomically visible `.svegmap` root from the catalog.
pub fn load_vegetation_map_root(assets: &AssetServer, id: Uuid) -> Result<VegetationMapAsset> {
    load_vegetation_map_root_from(assets, id)
}

pub(super) fn load_vegetation_map_root_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<VegetationMapAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    Ok(decode_map(&bytes)?)
}

/// Resolves one complete authored map snapshot from its root and immutable object inventory.
pub fn load_vegetation_map_snapshot(
    assets: &AssetServer,
    id: Uuid,
) -> Result<VegetationMapSnapshot> {
    load_vegetation_map_snapshot_from(assets, id)
}

/// Reads the authored chunks for the requested logical keys; a key with no
/// inventory entry contributes no row.
pub fn load_vegetation_map_chunks(
    assets: &AssetServer,
    id: Uuid,
    keys: &[VegetationMapChunkKey],
) -> Result<Vec<VegetationMapChunk>> {
    let entry = typed_entry_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, id)?;
    let package = assets.root().join(map_package_directory(&entry.path));
    let references = root
        .inventory
        .iter()
        .map(|reference| (reference.key, *reference))
        .collect::<BTreeMap<_, _>>();
    let mut chunks = Vec::new();
    for key in keys {
        if let Some(reference) = references.get(key) {
            chunks.push(read_map_object_from(assets, &package, id, reference)?);
        }
    }
    Ok(chunks)
}

pub(crate) fn load_vegetation_map_snapshot_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<VegetationMapSnapshot> {
    let entry = typed_entry_from(assets, id, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, id)?;
    let package = assets.root().join(map_package_directory(&entry.path));
    let mut chunks = Vec::with_capacity(root.inventory.len());
    for reference in &root.inventory {
        chunks.push(read_map_object_from(assets, &package, id, reference)?);
    }
    let mut layers = Vec::new();
    let mut biome_instances = Vec::new();
    let mut brush_history = Vec::new();
    for chunk in &chunks {
        match &chunk.payload {
            VegetationMapChunkPayload::LayerMetadata(layer) => layers.push(layer.clone()),
            VegetationMapChunkPayload::GraphInstance(instance) => {
                biome_instances.push(instance.clone());
            }
            VegetationMapChunkPayload::EditorMetadata(gestures) => {
                brush_history.extend(gestures.iter().cloned());
            }
            VegetationMapChunkPayload::Field(_) | VegetationMapChunkPayload::AnchorOverride(_) => {}
        }
    }
    layers.sort_by_key(saffron_vegetation::VegetationLayer::order_key);
    biome_instances.sort_by_key(|instance| instance.id);
    brush_history.sort_by_key(|gesture| (gesture.layer, gesture.gesture));
    let layer_ids = layers.iter().map(|layer| layer.id).collect::<BTreeSet<_>>();
    if chunks.iter().any(|chunk| match chunk.key.kind {
        VegetationMapChunkKind::Field
        | VegetationMapChunkKind::AnchorOverride
        | VegetationMapChunkKind::EditorMetadata => !layer_ids.contains(&chunk.key.layer),
        VegetationMapChunkKind::GraphInstance | VegetationMapChunkKind::LayerMetadata => false,
    }) {
        return Err(Error::Io(
            "vegetation-map object references missing layer metadata".to_owned(),
        ));
    }
    Ok(VegetationMapSnapshot {
        root,
        layers,
        biome_instances,
        brush_history,
        chunks,
    })
}

/// Writes a new empty `.svegmap` root and registers it.
pub fn save_vegetation_map_asset(
    assets: &mut AssetServer,
    mut asset: VegetationMapAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    if asset.generation != 0 || !asset.inventory.is_empty() {
        return Err(Error::Io(
            "a new vegetation-map root must have generation zero and an empty inventory".to_owned(),
        ));
    }
    let bytes = encode_map(&asset)?;
    let path = format!("vegetation/maps/{}.svegmap", asset.id.value());
    let id = save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::VegetationMap,
        path.clone(),
        &bytes,
    )?;
    if let Err(error) = std::fs::create_dir_all(
        assets
            .root
            .join(map_package_directory(&path))
            .join("objects"),
    ) {
        rollback_new_asset(assets, id);
        return Err(Error::Io(error.to_string()));
    }
    Ok(id)
}

/// Atomically updates root metadata while retaining the committed object inventory.
pub fn update_vegetation_map_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset: &VegetationMapAsset,
) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "vegetation-map identity does not match catalog row".to_owned(),
        ));
    }
    let entry = typed_entry_from(assets, id, AssetType::VegetationMap, "vegetation map")?.clone();
    let package = assets.root.join(map_package_directory(&entry.path));
    std::fs::create_dir_all(&package).map_err(|error| Error::Io(error.to_string()))?;
    let _lock = lock_map_package(&package)?;
    let current = load_vegetation_map_root(assets, id)?;
    if asset.generation != current.generation
        || asset.inventory != current.inventory
        || asset.chunk_layout != current.chunk_layout
    {
        return Err(Error::Io(
            "vegetation-map root metadata update is based on a stale or altered inventory"
                .to_owned(),
        ));
    }
    let mut next = current;
    next.name.clone_from(&asset.name);
    next.bounds = asset.bounds;
    next.generation = next.generation.checked_add(1).ok_or(Error::Vegetation(
        saffron_vegetation::Error::NumericOverflow,
    ))?;
    let bytes = encode_map(&next)?;
    atomic_write(&assets.root.join(&entry.path), &bytes)?;
    let content_hash = hash_bytes_fnv(&bytes);
    let updated = assets.update_asset_content_hash(id, content_hash);
    debug_assert!(updated, "validated vegetation map remains catalogued");
    Ok(())
}

/// Commits one optimistic authored-map transaction and publishes its root last.
pub fn commit_vegetation_map_transaction(
    assets: &mut AssetServer,
    map: Uuid,
    transaction: VegetationMapTransaction,
) -> Result<()> {
    let entry = typed_entry_from(assets, map, AssetType::VegetationMap, "vegetation map")?.clone();
    let package = assets.root.join(map_package_directory(&entry.path));
    let objects = package.join("objects");
    std::fs::create_dir_all(&objects).map_err(|error| Error::Io(error.to_string()))?;
    let _lock = lock_map_package(&package)?;
    let mut root = load_vegetation_map_root(assets, map)?;
    if root.generation != transaction.expected_generation {
        return Err(Error::VegetationMapGenerationConflict {
            expected: transaction.expected_generation,
            actual: root.generation,
        });
    }

    let mut keys = BTreeSet::new();
    let mut encoded = Vec::with_capacity(transaction.upserts.len());
    for chunk in &transaction.upserts {
        if chunk.map != map || !keys.insert(chunk.key) {
            return Err(Error::Io(
                "map object identity or batch key uniqueness is invalid".to_owned(),
            ));
        }
        if let VegetationMapTileKey::Cell(cell) = chunk.key.tile
            && cell.level() != root.chunk_layout.level
        {
            return Err(Error::Io(
                "map object cell level does not match its root".to_owned(),
            ));
        }
        let bytes = write_vegetation_map_chunk(chunk)?;
        let reference = map_object_reference(chunk, &bytes)?;
        encoded.push((reference, bytes));
    }
    let mut removals = transaction.removals;
    removals.sort_unstable();
    if removals.iter().any(|key| !keys.insert(*key))
        || removals.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(Error::Io(
            "map transaction contains duplicate or contradictory logical keys".to_owned(),
        ));
    }
    encoded.sort_by_key(|(reference, _)| reference.order_key());

    let previous = root
        .inventory
        .iter()
        .map(|reference| (reference.key, *reference))
        .collect::<BTreeMap<_, _>>();
    let mut inventory = previous.clone();
    for key in removals {
        inventory.remove(&key);
    }
    let mut published = Vec::new();
    for (reference, bytes) in &encoded {
        let path = map_object_path(&package, &reference.content_hash);
        if path.exists() {
            let existing = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    rollback_map_objects(&published);
                    return Err(Error::Io(error.to_string()));
                }
            };
            if existing != *bytes {
                rollback_map_objects(&published);
                return Err(Error::VegetationMapObjectCollision {
                    path: path.display().to_string(),
                });
            }
            read_map_object_from(assets, &package, map, reference)?;
        } else if let Err(error) = atomic_write(&path, bytes) {
            rollback_map_objects(&published);
            return Err(error);
        } else {
            published.push(path);
        }
        inventory.insert(reference.key, *reference);
    }
    let layer_ids = inventory
        .keys()
        .filter(|key| key.kind == VegetationMapChunkKind::LayerMetadata)
        .map(|key| key.layer)
        .collect::<BTreeSet<_>>();
    if inventory.keys().any(|key| {
        matches!(
            key.kind,
            VegetationMapChunkKind::Field
                | VegetationMapChunkKind::AnchorOverride
                | VegetationMapChunkKind::EditorMetadata
        ) && !layer_ids.contains(&key.layer)
    }) {
        rollback_map_objects(&published);
        return Err(Error::Io(
            "vegetation-map transaction leaves an object without layer metadata".to_owned(),
        ));
    }
    if inventory == previous {
        rollback_map_objects(&published);
        return Ok(());
    }
    root.inventory = inventory.into_values().collect();
    root.generation = root.generation.checked_add(1).ok_or(Error::Vegetation(
        saffron_vegetation::Error::NumericOverflow,
    ))?;
    let root_bytes = match encode_map(&root) {
        Ok(bytes) => bytes,
        Err(error) => {
            rollback_map_objects(&published);
            return Err(error.into());
        }
    };
    if let Err(error) = atomic_write(&assets.root.join(&entry.path), &root_bytes) {
        rollback_map_objects(&published);
        return Err(error);
    }
    assets
        .catalog
        .set_content_hash(map, hash_bytes_fnv(&root_bytes));
    Ok(())
}

/// Resolves all typed authored objects that contribute to one spatial map tile.
pub fn load_vegetation_map_tile_snapshot(
    assets: &AssetServer,
    map: Uuid,
    cell: WorldCellKey,
) -> Result<Option<VegetationMapTileSnapshot>> {
    load_vegetation_map_tile_snapshot_from(assets, map, cell)
}

pub(super) fn load_vegetation_map_tile_snapshot_from(
    assets: &dyn CookAssetAccess,
    map: Uuid,
    cell: WorldCellKey,
) -> Result<Option<VegetationMapTileSnapshot>> {
    let entry = typed_entry_from(assets, map, AssetType::VegetationMap, "vegetation map")?;
    let root = load_vegetation_map_root_from(assets, map)?;
    if cell.level() != root.chunk_layout.level {
        return Err(Error::Io(
            "requested vegetation-map tile level does not match its root".to_owned(),
        ));
    }
    let package = assets.root().join(map_package_directory(&entry.path));
    let references = root
        .inventory
        .iter()
        .filter(|reference| reference.key.tile == VegetationMapTileKey::Cell(cell));
    let mut snapshot = VegetationMapTileSnapshot {
        map,
        cell,
        fields: Vec::new(),
        blockers: Vec::new(),
        explicit_plants: Vec::new(),
        pins: Vec::new(),
        transform_overrides: Vec::new(),
        state_overrides: Vec::new(),
        provenance: Default::default(),
    };
    let mut found = false;
    for reference in references {
        found = true;
        let chunk = read_map_object(&package, map, reference)?;
        match chunk.payload {
            VegetationMapChunkPayload::Field(payload) => {
                snapshot.fields.extend(payload.fields);
                snapshot.blockers.extend(payload.blockers);
            }
            VegetationMapChunkPayload::AnchorOverride(payload) => {
                let handles = (0..payload.provenance.records().len())
                    .map(|index| saffron_vegetation::ProvenanceHandle(index as u32))
                    .collect::<Vec<_>>();
                let remap = snapshot
                    .provenance
                    .import_fragment(&payload.provenance, &handles)?;
                for mut anchor in payload.explicit_plants {
                    let source = saffron_vegetation::ProvenanceHandle(anchor.point.provenance);
                    anchor.point.provenance = remap
                        .records
                        .get(&source)
                        .ok_or_else(|| {
                            Error::Io("explicit map anchor provenance was not imported".to_owned())
                        })?
                        .0;
                    snapshot.explicit_plants.push(anchor);
                }
                snapshot.pins.extend(payload.pins);
                snapshot
                    .transform_overrides
                    .extend(payload.transform_overrides);
                snapshot.state_overrides.extend(payload.state_overrides);
            }
            VegetationMapChunkPayload::GraphInstance(_)
            | VegetationMapChunkPayload::LayerMetadata(_)
            | VegetationMapChunkPayload::EditorMetadata(_) => {
                return Err(Error::Io(
                    "global vegetation-map metadata was addressed as a spatial tile".to_owned(),
                ));
            }
        }
    }
    if !found {
        return Ok(None);
    }
    snapshot
        .fields
        .sort_by_key(|field| (field.layer, field.channel));
    snapshot
        .blockers
        .sort_by_key(|field| (field.layer, field.channel));
    snapshot.explicit_plants.sort_by_key(|anchor| anchor.id);
    snapshot.pins.sort_unstable();
    snapshot
        .transform_overrides
        .sort_by_key(|value| value.plant);
    snapshot.state_overrides.sort_by_key(|value| value.plant);
    if snapshot
        .explicit_plants
        .windows(2)
        .any(|pair| pair[0].id == pair[1].id)
        || snapshot.pins.windows(2).any(|pair| pair[0] == pair[1])
        || snapshot
            .transform_overrides
            .windows(2)
            .any(|pair| pair[0].plant == pair[1].plant)
        || snapshot
            .state_overrides
            .windows(2)
            .any(|pair| pair[0].plant == pair[1].plant)
    {
        return Err(Error::Io(
            "vegetation-map tile contains duplicate cross-layer identities".to_owned(),
        ));
    }
    Ok(Some(snapshot))
}

/// Removes the internal authored chunk directory owned by a vegetation-map catalog row.
pub fn remove_vegetation_map_package(assets: &AssetServer, entry: &AssetEntry) -> Result<()> {
    if entry.asset_type != AssetType::VegetationMap || entry.path.is_empty() {
        return Ok(());
    }
    let directory = assets.root.join(map_package_directory(&entry.path));
    match std::fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error.to_string())),
    }
}

pub(crate) fn vegetation_map_package_bytes(assets: &AssetServer, entry: &AssetEntry) -> u64 {
    let manifest = assets.root.join(&entry.path);
    let mut bytes = std::fs::metadata(&manifest).map_or(0, |metadata| metadata.len());
    let objects = assets
        .root
        .join(map_package_directory(&entry.path))
        .join("objects");
    if let Ok(entries) = std::fs::read_dir(objects) {
        for object in entries.filter_map(std::result::Result::ok) {
            if object
                .path()
                .extension()
                .is_some_and(|extension| extension == "svegmapc")
            {
                bytes =
                    bytes.saturating_add(object.metadata().map_or(0, |metadata| metadata.len()));
            }
        }
    }
    bytes
}

pub(crate) fn vegetation_map_dependencies(
    assets: &AssetServer,
    entry: &AssetEntry,
) -> Result<Vec<Uuid>> {
    let map = load_vegetation_map_snapshot(assets, entry.id)?;
    let mut dependencies: BTreeSet<u64> = map
        .biome_instances
        .iter()
        .map(|instance| instance.biome.value())
        .collect();
    for layer in &map.layers {
        if let saffron_vegetation::VegetationLayerOperator::SpeciesWeights(weights) =
            &layer.operator
        {
            dependencies.extend(weights.iter().map(|weight| weight.family.value()));
        }
    }

    for chunk in map.chunks {
        if let VegetationMapChunkPayload::AnchorOverride(payload) = chunk.payload {
            dependencies.extend(
                payload
                    .explicit_plants
                    .into_iter()
                    .map(|anchor| anchor.family.value()),
            );
        }
    }
    dependencies.remove(&0);
    Ok(dependencies.into_iter().map(Uuid).collect())
}

#[cfg(test)]
mod tests {
    use super::super::map_objects::{map_package_path, publish_imported_map_objects};
    use super::super::test_support::{
        Scratch, biome_fixture, chunk_fixture, commit_chunks, layer_chunk, layer_fixture,
        map_fixture, plant_fixture,
    };
    use super::*;
    use crate::vegetation::{
        import_vegetation_asset, load_biome_asset, load_plant_family_asset, save_biome_asset,
        save_plant_family_asset, save_vegetation_map_asset, vegetation_graph_dependency_hashes,
    };
    use saffron_scene::Scene;
    use saffron_spatial::{WorldBounds, WorldCellKey};
    use saffron_vegetation::{write_biome_asset, write_plant_asset};

    #[test]
    fn all_authored_asset_kinds_round_trip_and_chunk_writes_are_sparse() {
        let scratch = Scratch::new("round-trip");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);

        let plant_id =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "plants")
                .expect("save plant");
        let mut expected_plant = plant_fixture(plant_id, "Oak");
        expected_plant.id = plant_id;
        assert_eq!(
            load_plant_family_asset(&assets, plant_id).unwrap(),
            expected_plant
        );

        let biome_id = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(2), "Forest", plant_id),
            "Forest",
            "biomes",
        )
        .expect("save biome");
        let mut expected_biome = biome_fixture(biome_id, "Forest", plant_id);
        expected_biome.id = biome_id;
        assert_eq!(load_biome_asset(&assets, biome_id).unwrap(), expected_biome);

        let map_id =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(3), "World"), "World", "maps")
                .expect("save map");
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        commit_chunks(
            &mut assets,
            map_id,
            vec![layer_chunk(map_id, layer_fixture(bounds))],
        );
        let snapshot = load_vegetation_map_snapshot(&assets, map_id).unwrap();
        assert_eq!(snapshot.name, "World");
        assert_eq!(snapshot.layers, vec![layer_fixture(bounds)]);

        for (id, encoder) in [
            (
                plant_id,
                write_plant_asset(&load_plant_family_asset(&assets, plant_id).unwrap()).unwrap(),
            ),
            (
                biome_id,
                write_biome_asset(&load_biome_asset(&assets, biome_id).unwrap()).unwrap(),
            ),
            (
                map_id,
                encode_map(&load_vegetation_map_root(&assets, map_id).unwrap()).unwrap(),
            ),
        ] {
            let entry = assets.catalog.find(id).unwrap();
            assert_eq!(std::fs::read(root.join(&entry.path)).unwrap(), encoder);
        }

        let first = WorldCellKey::base(0, 0, 0);
        let second = WorldCellKey::base(1, 0, 0);
        commit_chunks(
            &mut assets,
            map_id,
            vec![
                chunk_fixture(map_id, first, 1, 0),
                chunk_fixture(map_id, second, 1, 0),
            ],
        );
        let entry = assets.catalog.find(map_id).unwrap().clone();
        let manifest_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let root_before = load_vegetation_map_root(&assets, map_id).unwrap();
        let first_reference = root_before
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
            .copied()
            .unwrap();
        let second_reference = root_before
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(second))
            .copied()
            .unwrap();
        let first_path = map_object_path(&package, &first_reference.content_hash);
        let second_path = map_object_path(&package, &second_reference.content_hash);
        let dependencies_before = vegetation_graph_dependency_hashes(&assets, map_id, &[]).unwrap();
        let manifest_before = std::fs::read(&manifest_path).unwrap();
        let second_before = std::fs::read(&second_path).unwrap();
        #[cfg(unix)]
        let second_inode_before =
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&second_path).unwrap());

        commit_chunks(
            &mut assets,
            map_id,
            vec![chunk_fixture(map_id, first, 2, 0)],
        );
        assert_ne!(std::fs::read(&manifest_path).unwrap(), manifest_before);
        assert!(first_path.exists());
        assert_eq!(std::fs::read(&second_path).unwrap(), second_before);
        let root_after = load_vegetation_map_root(&assets, map_id).unwrap();
        let dependencies_after = vegetation_graph_dependency_hashes(&assets, map_id, &[]).unwrap();
        assert_eq!(dependencies_after, dependencies_before);
        assert_eq!(root_after.generation, root_before.generation + 1);
        assert_eq!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
                .unwrap()
                .revision,
            2
        );
        assert_ne!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
                .unwrap()
                .content_hash,
            first_reference.content_hash
        );
        assert_eq!(
            root_after
                .inventory
                .iter()
                .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(second))
                .unwrap(),
            &second_reference
        );
        assert!(
            load_vegetation_map_tile_snapshot(&assets, map_id, first)
                .unwrap()
                .is_some()
        );
        #[cfg(unix)]
        {
            assert_eq!(
                std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&second_path).unwrap()),
                second_inode_before
            );
        }

        let current_first_reference = root_after
            .inventory
            .iter()
            .find(|reference| reference.key.tile == VegetationMapTileKey::Cell(first))
            .unwrap();
        let current_first_path = map_object_path(&package, &current_first_reference.content_hash);
        let authored_paths = [manifest_path, current_first_path, second_path];
        let authored_bytes: Vec<Vec<u8>> = authored_paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        assets.thumbnail_cache_root = root.join(".cache/thumbnails");
        for id in [biome_id, map_id] {
            assert!(
                !crate::request_thumbnail(&mut assets, id, 128)
                    .unwrap()
                    .pending
            );
        }
        // The plant renders through the main graph: a cold cache replies pending.
        assert!(
            crate::request_thumbnail(&mut assets, plant_id, 128)
                .unwrap()
                .pending
        );
        let removed = assets.clear_thumbnail_cache_dir();
        assert_eq!(removed.entries, 2);
        assets.clear_asset_caches();
        for (path, expected) in authored_paths.iter().zip(authored_bytes) {
            assert_eq!(std::fs::read(path).unwrap(), expected);
        }
    }

    #[test]
    fn stale_map_transactions_publish_nothing() {
        let scratch = Scratch::new("stale-transaction");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let chunk = chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1, 0);
        let reference = chunk.reference().unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let root_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let before = std::fs::read(&root_path).unwrap();

        let error = commit_vegetation_map_transaction(
            &mut assets,
            map,
            VegetationMapTransaction {
                expected_generation: 1,
                upserts: vec![chunk],
                removals: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::VegetationMapGenerationConflict {
                expected: 1,
                actual: 0
            }
        ));
        assert_eq!(std::fs::read(root_path).unwrap(), before);
        assert!(!map_object_path(&package, &reference.content_hash).exists());
    }

    #[test]
    fn failed_multi_object_transaction_rolls_back_before_root_publication() {
        let scratch = Scratch::new("transaction-rollback");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        let metadata = layer_chunk(map, layer_fixture(bounds));
        let first = chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1, 0);
        let second = chunk_fixture(map, WorldCellKey::base(1, 0, 0), 1, 0);
        let metadata_reference = metadata.reference().unwrap();
        let first_reference = first.reference().unwrap();
        let second_reference = second.reference().unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let root_path = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let before = std::fs::read(&root_path).unwrap();
        std::fs::create_dir(map_object_path(&package, &second_reference.content_hash)).unwrap();

        assert!(
            commit_vegetation_map_transaction(
                &mut assets,
                map,
                VegetationMapTransaction {
                    expected_generation: 0,
                    upserts: vec![second, first, metadata],
                    removals: Vec::new(),
                },
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root_path).unwrap(), before);
        assert!(!map_object_path(&package, &metadata_reference.content_hash).exists());
        assert!(!map_object_path(&package, &first_reference.content_hash).exists());
        assert!(
            load_vegetation_map_root(&assets, map)
                .unwrap()
                .inventory
                .is_empty()
        );
    }

    #[test]
    fn sparse_removal_hides_old_immutable_object_and_corruption_is_typed() {
        let scratch = Scratch::new("remove-corruption");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        let cell = WorldCellKey::base(-1, 2, 0);
        let chunk = chunk_fixture(map, cell, 1, 0);
        let bounds = WorldBounds::new([0; 3], [1024; 3]).unwrap();
        commit_chunks(
            &mut assets,
            map,
            vec![layer_chunk(map, layer_fixture(bounds)), chunk.clone()],
        );
        let reference = load_vegetation_map_root(&assets, map)
            .unwrap()
            .inventory
            .into_iter()
            .find(|reference| reference.key == chunk.key)
            .unwrap();
        let entry = assets.catalog.find(map).unwrap().clone();
        let package = root.join(map_package_directory(&entry.path));
        let path = map_object_path(&package, &reference.content_hash);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(matches!(
            load_vegetation_map_snapshot(&assets, map),
            Err(Error::Vegetation(
                saffron_vegetation::Error::ArtifactFormat { .. }
            ))
        ));
        std::fs::write(&path, bytes).unwrap();

        commit_vegetation_map_transaction(
            &mut assets,
            map,
            VegetationMapTransaction {
                expected_generation: 1,
                upserts: Vec::new(),
                removals: vec![chunk.key],
            },
        )
        .unwrap();
        assert_eq!(
            load_vegetation_map_root(&assets, map)
                .unwrap()
                .inventory
                .len(),
            1
        );
        assert!(
            load_vegetation_map_tile_snapshot(&assets, map, cell)
                .unwrap()
                .is_none()
        );
        assert!(path.exists());
    }

    #[test]
    fn authored_imports_preserve_identity_and_copy_complete_map_packages() {
        let scratch = Scratch::new("import");
        let source_root = scratch.path().join("source");
        let project_root = scratch.path().join("assets");
        std::fs::create_dir_all(&source_root).unwrap();
        let plant_path = source_root.join("oak.splant");
        let biome_path = source_root.join("forest.sbiome");
        let map_path = source_root.join("world.svegmap");
        std::fs::write(
            &plant_path,
            write_plant_asset(&plant_fixture(Uuid(4_101), "Oak")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &biome_path,
            write_biome_asset(&biome_fixture(Uuid(4_102), "Forest", Uuid(4_101))).unwrap(),
        )
        .unwrap();
        let mut source_map = map_fixture(Uuid(4_103), "World");
        let cell = WorldCellKey::base(4, -2, 0);
        let source_chunk = chunk_fixture(source_map.id, cell, 7, 0);
        source_map.inventory = vec![source_chunk.reference().unwrap()];
        source_map.generation = 1;
        std::fs::write(&map_path, encode_map(&source_map).unwrap()).unwrap();
        publish_imported_map_objects(&map_package_path(&map_path), &source_map, &[source_chunk])
            .unwrap();

        let mut assets = AssetServer::new(&project_root);
        let imported_plant = import_vegetation_asset(&mut assets, &plant_path, "imports").unwrap();
        let imported_biome = import_vegetation_asset(&mut assets, &biome_path, "imports").unwrap();
        let imported_map = import_vegetation_asset(&mut assets, &map_path, "imports").unwrap();
        assert_eq!(imported_plant.asset_type, AssetType::Plant);
        assert_eq!(imported_biome.asset_type, AssetType::Biome);
        assert_eq!(imported_map.asset_type, AssetType::VegetationMap);
        assert_eq!(imported_plant.id, Uuid(4_101));
        assert_eq!(imported_biome.id, Uuid(4_102));
        assert_eq!(imported_map.id, Uuid(4_103));
        assert_eq!(
            load_biome_asset(&assets, imported_biome.id)
                .unwrap()
                .palette[0]
                .plant,
            imported_plant.id
        );
        let imported_chunk = load_vegetation_map_tile_snapshot(&assets, imported_map.id, cell)
            .unwrap()
            .unwrap();
        assert_eq!(imported_chunk.map, imported_map.id);
        assert_eq!(
            load_vegetation_map_root(&assets, imported_map.id)
                .unwrap()
                .inventory[0]
                .revision,
            7
        );

        let generated = source_root.join("compiled.splantc");
        std::fs::write(&generated, b"generated").unwrap();
        assert!(import_vegetation_asset(&mut assets, generated, "").is_err());
    }

    #[test]
    fn deleting_unused_map_removes_root_and_authored_object_package() {
        let scratch = Scratch::new("delete-package");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(1), "World"), "World", "")
                .unwrap();
        commit_chunks(
            &mut assets,
            map,
            vec![
                layer_chunk(
                    map,
                    layer_fixture(WorldBounds::new([0; 3], [1024; 3]).unwrap()),
                ),
                chunk_fixture(map, WorldCellKey::base(0, 0, 0), 1, 0),
            ],
        );
        let entry = assets.catalog.find(map).unwrap().clone();
        let manifest = root.join(&entry.path);
        let package = root.join(map_package_directory(&entry.path));
        let mut scene = Scene::new();
        let deleted = crate::delete_unused(&mut assets, &mut scene, &[map], true).unwrap();
        assert_eq!(deleted.deleted, 1);
        assert!(!manifest.exists());
        assert!(!package.exists());
        assert!(assets.catalog.find(map).is_none());
    }
}
