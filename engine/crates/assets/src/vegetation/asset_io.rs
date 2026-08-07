//! Import, read, and rewrite of the authored `.splant` and `.sbiome` catalog assets.

use std::collections::BTreeSet;
use std::path::Path;

use saffron_core::Uuid;
use saffron_scene::AssetType;
use saffron_vegetation::{
    BiomeAsset, PlantFamilyAsset, read_biome_asset, read_plant_asset,
    read_vegetation_map_asset as decode_map, write_biome_asset, write_plant_asset,
};

use crate::cook_reader::CookAssetAccess;
use crate::{AssetServer, Error, Result};

use super::catalog_io::{
    import_typed_asset, read_typed_asset_from, save_typed_asset, update_typed_asset,
};
use super::map_objects::{
    map_package_directory, publish_imported_map_objects, read_source_map_chunks,
};

/// One validated native vegetation asset copied into the project catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationImport {
    /// Stable authored identity preserved from the source asset.
    pub id: Uuid,
    /// Unique catalog display name.
    pub name: String,
    /// Imported logical asset kind.
    pub asset_type: AssetType,
}

/// Imports one authored `.splant`, `.sbiome`, or complete `.svegmap` package.
pub fn import_vegetation_asset(
    assets: &mut AssetServer,
    source: impl AsRef<Path>,
    folder: &str,
) -> Result<VegetationImport> {
    let source = source.as_ref();
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let bytes = std::fs::read(source).map_err(|error| Error::Io(error.to_string()))?;
    let (id, asset_type) = match extension.as_str() {
        "splant" => {
            let asset = read_plant_asset(&bytes)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::Plant,
                format!("vegetation/plants/{}.splant", asset.id.value()),
                &bytes,
            )?;
            (id, AssetType::Plant)
        }
        "sbiome" => {
            let asset = read_biome_asset(&bytes)?;
            validate_biome_cycles(assets, &asset)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::Biome,
                format!("vegetation/biomes/{}.sbiome", asset.id.value()),
                &bytes,
            )?;
            (id, AssetType::Biome)
        }
        "svegmap" => {
            let asset = decode_map(&bytes)?;
            let chunks = read_source_map_chunks(source, &asset)?;
            let path = format!("vegetation/maps/{}.svegmap", asset.id.value());
            let package = assets.root.join(map_package_directory(&path));
            if package.exists() {
                return Err(Error::Io(format!(
                    "vegetation asset identity {} already has a map package in the project",
                    asset.id.value()
                )));
            }
            publish_imported_map_objects(&package, &asset, &chunks)?;
            let id = import_typed_asset(
                assets,
                asset.id,
                &asset.name,
                folder,
                AssetType::VegetationMap,
                path.clone(),
                &bytes,
            )
            .inspect_err(|_| {
                let _ = std::fs::remove_dir_all(&package);
            })?;
            (id, AssetType::VegetationMap)
        }
        "splantc" | "svegcell" => {
            return Err(Error::Io(
                "generated vegetation artifacts cannot be imported".to_owned(),
            ));
        }
        _ => {
            return Err(Error::Io(
                "expected an authored .splant, .sbiome, or .svegmap asset".to_owned(),
            ));
        }
    };
    let name = assets
        .catalog
        .find(id)
        .map(|entry| entry.name.clone())
        .ok_or(Error::NotInCatalog(id.value()))?;
    Ok(VegetationImport {
        id,
        name,
        asset_type,
    })
}

/// Reads a complete `.splant` from the catalog.
pub fn load_plant_family_asset(assets: &AssetServer, id: Uuid) -> Result<PlantFamilyAsset> {
    load_plant_family_asset_from(assets, id)
}

pub(crate) fn load_plant_family_asset_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
) -> Result<PlantFamilyAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::Plant, "plant")?;
    Ok(read_plant_asset(&bytes)?)
}

/// Writes a new `.splant` and registers it in the catalog.
pub fn save_plant_family_asset(
    assets: &mut AssetServer,
    mut asset: PlantFamilyAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    let bytes = write_plant_asset(&asset)?;
    save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::Plant,
        format!("vegetation/plants/{}.splant", asset.id.value()),
        &bytes,
    )
}

/// Rewrites an existing `.splant` without changing its catalog identity.
pub fn update_plant_family_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset: &PlantFamilyAsset,
) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "plant-family identity does not match catalog row".to_owned(),
        ));
    }
    let bytes = write_plant_asset(asset)?;
    update_typed_asset(assets, id, AssetType::Plant, "plant", &bytes)
}

/// Reads a complete `.sbiome` from the catalog.
pub fn load_biome_asset(assets: &AssetServer, id: Uuid) -> Result<BiomeAsset> {
    load_biome_asset_from(assets, id)
}

pub(super) fn load_biome_asset_from(assets: &dyn CookAssetAccess, id: Uuid) -> Result<BiomeAsset> {
    let bytes = read_typed_asset_from(assets, id, AssetType::Biome, "biome")?;
    Ok(read_biome_asset(&bytes)?)
}

/// Writes a new `.sbiome` and registers it in the catalog.
pub fn save_biome_asset(
    assets: &mut AssetServer,
    mut asset: BiomeAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    asset.id = Uuid::new();
    validate_biome_cycles(assets, &asset)?;
    let bytes = write_biome_asset(&asset)?;
    save_typed_asset(
        assets,
        asset.id,
        name,
        folder,
        AssetType::Biome,
        format!("vegetation/biomes/{}.sbiome", asset.id.value()),
        &bytes,
    )
}

/// Rewrites an existing `.sbiome` without changing its catalog identity.
pub fn update_biome_asset(assets: &mut AssetServer, id: Uuid, asset: &BiomeAsset) -> Result<()> {
    if asset.id != id {
        return Err(Error::Io(
            "biome identity does not match catalog row".to_owned(),
        ));
    }
    validate_biome_cycles(assets, asset)?;
    let bytes = write_biome_asset(asset)?;
    update_typed_asset(assets, id, AssetType::Biome, "biome", &bytes)
}

fn validate_biome_cycles(assets: &AssetServer, candidate: &BiomeAsset) -> Result<()> {
    let mut graph = std::collections::BTreeMap::<u64, Vec<u64>>::new();
    for entry in &assets.catalog.entries {
        if entry.asset_type != AssetType::Biome || entry.id == candidate.id {
            continue;
        }
        let biome = load_biome_asset(assets, entry.id)?;
        graph.insert(
            biome.id.value(),
            biome
                .modules
                .iter()
                .map(|module| module.biome.value())
                .collect(),
        );
    }
    graph.insert(
        candidate.id.value(),
        candidate
            .modules
            .iter()
            .map(|module| module.biome.value())
            .collect(),
    );
    let mut complete = BTreeSet::new();
    let mut active = BTreeSet::new();
    for id in graph.keys().copied().collect::<Vec<_>>() {
        visit_biome(id, &graph, &mut active, &mut complete)?;
    }
    Ok(())
}

fn visit_biome(
    id: u64,
    graph: &std::collections::BTreeMap<u64, Vec<u64>>,
    active: &mut BTreeSet<u64>,
    complete: &mut BTreeSet<u64>,
) -> Result<()> {
    if complete.contains(&id) || !graph.contains_key(&id) {
        return Ok(());
    }
    if !active.insert(id) {
        return Err(Error::Io("biome module dependency cycle".to_owned()));
    }
    for dependency in &graph[&id] {
        visit_biome(*dependency, graph, active, complete)?;
    }
    active.remove(&id);
    complete.insert(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        Scratch, biome_fixture, chunk_fixture, commit_chunks, layer_chunk, layer_fixture,
        map_fixture, plant_fixture,
    };
    use super::*;
    use crate::vegetation::{save_vegetation_map_asset, update_biome_asset};
    use saffron_spatial::{WorldBounds, WorldCellKey};
    use saffron_vegetation::BiomeModuleReference;

    #[test]
    fn cold_scan_recovers_authored_types_and_ignores_generated_artifacts() {
        let scratch = Scratch::new("cold-scan");
        let root = scratch.path().join("assets");
        let mut writer = AssetServer::new(&root);
        let plant =
            save_plant_family_asset(&mut writer, plant_fixture(Uuid(1), "Oak"), "Oak", "plants")
                .unwrap();
        let biome = save_biome_asset(
            &mut writer,
            biome_fixture(Uuid(2), "Forest", plant),
            "Forest",
            "biomes",
        )
        .unwrap();
        let map =
            save_vegetation_map_asset(&mut writer, map_fixture(Uuid(3), "World"), "World", "maps")
                .unwrap();
        for (id, name, folder) in [
            (plant, "Oak renamed", "catalog/plants"),
            (biome, "Forest renamed", "catalog/biomes"),
            (map, "World renamed", "catalog/maps"),
        ] {
            let entry = writer
                .catalog
                .entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .unwrap();
            entry.name = name.to_owned();
            entry.folder = folder.to_owned();
            writer.write_asset_sidecar(id).unwrap();
        }
        commit_chunks(
            &mut writer,
            map,
            vec![
                layer_chunk(
                    map,
                    layer_fixture(WorldBounds::new([0; 3], [1024; 3]).unwrap()),
                ),
                chunk_fixture(map, WorldCellKey::base(-1, 2, 0), 1, 0),
            ],
        );
        let expected_map_hash = writer.catalog.find(map).unwrap().content_hash;
        std::fs::write(root.join("vegetation/plants/999.splantc"), b"generated").unwrap();
        std::fs::write(root.join("vegetation/maps/998.svegcell"), b"generated").unwrap();

        let mut reader = AssetServer::new(&root);
        reader.scan_assets().expect("cold scan");
        let plant_entry = reader.catalog.find(plant).unwrap();
        assert_eq!(plant_entry.asset_type, AssetType::Plant);
        assert_eq!(plant_entry.name, "Oak renamed");
        assert_eq!(plant_entry.folder, "catalog/plants");
        let biome_entry = reader.catalog.find(biome).unwrap();
        assert_eq!(biome_entry.asset_type, AssetType::Biome);
        assert_eq!(biome_entry.name, "Forest renamed");
        assert_eq!(biome_entry.folder, "catalog/biomes");
        let map_entry = reader.catalog.find(map).unwrap();
        assert_eq!(map_entry.asset_type, AssetType::VegetationMap);
        assert_eq!(map_entry.name, "World renamed");
        assert_eq!(map_entry.folder, "catalog/maps");
        assert_eq!(map_entry.content_hash, expected_map_hash);
        assert!(reader.catalog.find(Uuid(998)).is_none());
        assert!(reader.catalog.find(Uuid(999)).is_none());
    }

    #[test]
    fn authored_types_have_cached_vector_thumbnails() {
        let scratch = Scratch::new("thumbnails");
        let root = scratch.path().join("assets");
        let mut assets = AssetServer::new(&root);
        assets.thumbnail_cache_root = scratch.path().join("thumbnail-cache");
        let plant =
            save_plant_family_asset(&mut assets, plant_fixture(Uuid(1), "Oak"), "Oak", "").unwrap();
        let biome = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(2), "Forest", plant),
            "Forest",
            "",
        )
        .unwrap();
        let map =
            save_vegetation_map_asset(&mut assets, map_fixture(Uuid(3), "World"), "World", "")
                .unwrap();

        for id in [biome, map] {
            let first = crate::request_thumbnail(&mut assets, id, 128).unwrap();
            assert!(!first.pending);
            assert_eq!((first.width, first.height), (128, 128));
            assert_eq!(&first.png[..8], b"\x89PNG\r\n\x1a\n");
            let cached = crate::request_thumbnail(&mut assets, id, 128).unwrap();
            assert_eq!(cached, first);
        }
        // A plant family renders through the main graph like a mesh/model tile: a cold
        // cache replies pending and enqueues one preview render for the host to drain.
        let pending = crate::request_thumbnail(&mut assets, plant, 128).unwrap();
        assert!(pending.pending);
        assert!(assets.take_preview_render_job().is_some());
        assert!(assets.take_preview_render_job().is_none());
        assert_eq!(assets.thumbnail_cache_stats().entries, 2);
    }

    #[test]
    fn biome_cycles_are_rejected_across_existing_assets() {
        let scratch = Scratch::new("cycles");
        let mut assets = AssetServer::new(scratch.path().join("assets"));
        let first = save_biome_asset(
            &mut assets,
            biome_fixture(Uuid(1), "First", Uuid(11)),
            "First",
            "",
        )
        .unwrap();
        let mut second_asset = biome_fixture(Uuid(2), "Second", Uuid(11));
        second_asset.modules.push(BiomeModuleReference {
            biome: first,
            call_guid: 100,
            bindings: Vec::new(),
        });
        let second = save_biome_asset(&mut assets, second_asset, "Second", "").unwrap();
        let mut first_asset = load_biome_asset(&assets, first).unwrap();
        first_asset.modules.push(BiomeModuleReference {
            biome: second,
            call_guid: 200,
            bindings: Vec::new(),
        });
        assert!(update_biome_asset(&mut assets, first, &first_asset).is_err());
    }
}
