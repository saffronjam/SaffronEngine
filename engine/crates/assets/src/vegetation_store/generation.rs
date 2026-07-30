//! The per-map generation root: the transaction locks that order a publication, the closure
//! check the manifest must pass, and the atomic root advance.

use std::path::PathBuf;

use saffron_core::Uuid;
use saffron_vegetation::ContentHash;
use std::str::FromStr;

use super::{
    VegetationArtifactPublication, VegetationArtifactStore, VegetationAuthoredLock,
    VegetationGenerationLock, atomic_write, optional_hash,
};
use crate::{Error, Result};

impl VegetationArtifactStore {
    /// Acquires the project-wide authored vegetation transaction lock.
    pub fn lock_authored(&self) -> Result<VegetationAuthoredLock> {
        let file = self.lock_file(self.root.join("transactions").join("authored.lock"))?;
        Ok(VegetationAuthoredLock { _file: file })
    }

    /// Acquires one map's generation-root transaction lock.
    pub fn lock_generation(&self, map: Uuid) -> Result<VegetationGenerationLock> {
        let file = self.lock_file(
            self.root
                .join("generations")
                .join(format!("{}.lock", map.value())),
        )?;
        Ok(VegetationGenerationLock { map, _file: file })
    }

    /// Publishes a complete manifest and atomically advances the map's visible generation root.
    pub fn publish_generation(
        &self,
        map: Uuid,
        expected_current: Option<ContentHash>,
        bytes: &[u8],
    ) -> Result<VegetationArtifactPublication> {
        let lock = self.lock_generation(map)?;
        self.publish_generation_locked(&lock, expected_current, bytes)
    }

    /// Publishes a generation while retaining the caller's already ordered map lock.
    pub fn publish_generation_locked(
        &self,
        lock: &VegetationGenerationLock,
        expected_current: Option<ContentHash>,
        bytes: &[u8],
    ) -> Result<VegetationArtifactPublication> {
        let map = lock.map;
        let manifest = saffron_vegetation::VegetationBaseManifest::from_canonical_bytes(bytes)?;
        if manifest.map != map {
            return Err(Error::Io(
                "vegetation generation manifest belongs to a different map".to_owned(),
            ));
        }
        let current = self.current_manifest_hash(map)?;
        if current != expected_current {
            return Err(Error::VegetationGenerationSuperseded {
                map: map.value(),
                expected: optional_hash(expected_current),
                current: optional_hash(current),
            });
        }
        self.validate_generation_closure(&manifest)?;
        let publication = self.publish_manifest(bytes)?;
        let root_path = self.generation_root_path(map);
        let root_bytes = generation_root_bytes(publication.content_hash);
        atomic_write(&root_path, &root_bytes)?;
        let committed = std::fs::read(&root_path).map_err(|error| Error::Io(error.to_string()))?;
        if parse_generation_root(&committed)? != publication.content_hash {
            return Err(Error::VegetationArtifactHash {
                path: root_path.display().to_string(),
            });
        }
        Ok(publication)
    }

    fn lock_file(&self, path: PathBuf) -> Result<std::fs::File> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::Io("vegetation transaction lock has no parent".to_owned()))?;
        std::fs::create_dir_all(parent).map_err(|error| Error::Io(error.to_string()))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.lock().map_err(|error| Error::Io(error.to_string()))?;
        Ok(file)
    }

    fn validate_generation_closure(
        &self,
        manifest: &saffron_vegetation::VegetationBaseManifest,
    ) -> Result<()> {
        let graph_bytes = self.read_cook_graph(manifest.cook_graph_hash)?;
        let graph = saffron_vegetation::CookGraph::from_canonical_bytes(&graph_bytes)?;
        if graph.identity()? != manifest.cook_graph_hash
            || graph.versions != manifest.versions
            || graph.platform != manifest.platform
        {
            return Err(Error::Io(
                "vegetation manifest and cook graph contracts disagree".to_owned(),
            ));
        }
        let platform_profile = manifest.platform.identity()?;
        let graph_plants = graph
            .nodes
            .iter()
            .filter_map(|node| match &node.address {
                saffron_vegetation::CookNodeAddress::Plant { family } => {
                    Some((family.value(), (node.output_hash, node.cook_key)))
                }
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let graph_cells = graph
            .nodes
            .iter()
            .filter_map(|node| match &node.address {
                saffron_vegetation::CookNodeAddress::Cell { map, cell } if *map == manifest.map => {
                    Some((*cell, (node.output_hash, node.cook_key)))
                }
                _ => None,
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        if graph_plants.len() != manifest.plants.len() || graph_cells.len() != manifest.cells.len()
        {
            return Err(Error::Io(
                "vegetation manifest directory and cook graph outputs disagree".to_owned(),
            ));
        }
        for plant in &manifest.plants {
            let Some((output_hash, cook_key)) = graph_plants.get(&plant.family.value()) else {
                return Err(Error::Io(
                    "vegetation manifest plant row is absent from its cook graph".to_owned(),
                ));
            };
            if *output_hash != plant.artifact_hash {
                return Err(Error::Io(
                    "vegetation manifest plant row disagrees with its cook graph".to_owned(),
                ));
            }
            let bytes = self.read_plant(plant.artifact_hash)?;
            let index = saffron_vegetation::PlantCompiledArtifactIndex::open(
                &bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )?;
            let artifact_tags = index.family_tags(&bytes)?;
            if artifact_tags != plant.tags {
                return Err(Error::VegetationPlantTagMismatch {
                    family: plant.family.value(),
                });
            }
            if index.family != plant.family
                || index.cook_key != *cook_key
                || index.platform_profile != platform_profile
            {
                return Err(Error::Io(
                    "vegetation manifest plant row disagrees with its artifact".to_owned(),
                ));
            }
        }
        let manifest_cells = manifest
            .cells
            .iter()
            .map(|cell| (cell.cell, cell.artifact_hash))
            .collect::<std::collections::BTreeMap<_, _>>();
        for cell in &manifest.cells {
            let Some((output_hash, cook_key)) = graph_cells.get(&cell.cell) else {
                return Err(Error::Io(
                    "vegetation manifest cell row is absent from its cook graph".to_owned(),
                ));
            };
            if *output_hash != cell.artifact_hash {
                return Err(Error::Io(
                    "vegetation manifest cell row disagrees with its cook graph".to_owned(),
                ));
            }
            if cell.dependencies.iter().any(|dependency| {
                dependency.cell == cell.cell
                    || manifest_cells.get(&dependency.cell) != Some(&dependency.content_hash)
            }) {
                return Err(Error::Io(
                    "vegetation manifest cell dependency is absent or has the wrong content hash"
                        .to_owned(),
                ));
            }
            let bytes = self.read_cell(cell.artifact_hash)?;
            let index = saffron_vegetation::VegetationCellArtifactIndex::open(
                &bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )?;
            if index.cell != cell.cell
                || index.cook_key != *cook_key
                || index.platform_profile != platform_profile
                || index.payload_hash != cell.payload_hash
                || index.sections.len() != cell.sections.len()
                || index
                    .sections
                    .iter()
                    .zip(&cell.sections)
                    .any(|(artifact, manifest)| {
                        artifact.kind != manifest.kind
                            || artifact.version != manifest.version
                            || artifact.codec != manifest.codec
                            || artifact.alignment != manifest.alignment
                            || artifact.stored_size != manifest.stored_size
                            || artifact.decoded_size != manifest.decoded_size
                            || artifact.content_hash != manifest.content_hash
                    })
            {
                return Err(Error::Io(
                    "vegetation manifest cell row disagrees with its artifact".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Resolves the current immutable manifest identity for a map.
    pub fn current_manifest_hash(&self, map: Uuid) -> Result<Option<ContentHash>> {
        let path = self.generation_root_path(map);
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(parse_generation_root(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(error.to_string())),
        }
    }

    /// Reads the current manifest after validating both its root and canonical bytes.
    pub fn read_current_manifest(&self, map: Uuid) -> Result<Option<Vec<u8>>> {
        self.current_manifest_hash(map)?
            .map(|hash| self.read_manifest(hash))
            .transpose()
    }

    fn generation_root_path(&self, map: Uuid) -> PathBuf {
        self.root
            .join("generations")
            .join(format!("{}.current", map.value()))
    }
}

fn generation_root_bytes(hash: ContentHash) -> Vec<u8> {
    format!("SVEGROOT1\n{hash}\n").into_bytes()
}

fn parse_generation_root(bytes: &[u8]) -> Result<ContentHash> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| Error::Io(format!("vegetation generation root is not UTF-8: {error}")))?;
    let hash = text
        .strip_prefix("SVEGROOT1\n")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| Error::Io("vegetation generation root is malformed".to_owned()))?;
    let parsed = ContentHash::from_str(hash)?;
    if generation_root_bytes(parsed) != bytes {
        return Err(Error::Io(
            "vegetation generation root is not canonical".to_owned(),
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{plant_sections, root};
    use super::*;
    use saffron_geometry::glam as _;
    use saffron_spatial::DecisionScalar;
    use saffron_vegetation::{
        CookGraph, CookNodeAddress, CookNodeRecord, CookPlatformProfile, CookVersionSet,
        CookWorkActual, CookWorkEstimate, PlantCompiledArtifactHeader, PlantTagId,
        VegetationManifestPlant, write_plant_compiled_artifact,
    };

    #[test]
    fn generation_root_advances_only_after_a_valid_manifest_is_published() {
        let root = root();
        let _ = std::fs::remove_dir_all(&root);
        let store = VegetationArtifactStore::new(&root);
        let map = Uuid(17);
        let versions = saffron_vegetation::CookVersionSet::current();
        let platform = saffron_vegetation::CookPlatformProfile {
            target: "test-target".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "test-toolchain".to_owned(),
            features: vec!["indexed-mdi".to_owned()],
        };
        let graph_bytes = saffron_vegetation::CookGraph {
            versions,
            platform: platform.clone(),
            nodes: Vec::new(),
        }
        .canonical_bytes()
        .unwrap();
        let graph = store.publish_cook_graph(&graph_bytes).unwrap();
        let manifest = saffron_vegetation::VegetationBaseManifest::current(
            Uuid(9),
            map,
            ContentHash::of(b"map"),
            versions,
            platform,
            graph.content_hash,
        );
        let bytes = manifest.canonical_bytes().unwrap();

        let publication = store.publish_generation(map, None, &bytes).unwrap();
        assert_eq!(
            store.current_manifest_hash(map).unwrap(),
            Some(publication.content_hash)
        );
        assert_eq!(store.read_current_manifest(map).unwrap(), Some(bytes));

        assert!(matches!(
            store.publish_generation(map, None, &manifest.canonical_bytes().unwrap()),
            Err(Error::VegetationGenerationSuperseded { .. })
        ));
        assert_eq!(
            store.current_manifest_hash(map).unwrap(),
            Some(publication.content_hash)
        );

        assert!(
            store
                .publish_generation(Uuid(18), None, &manifest.canonical_bytes().unwrap())
                .is_err()
        );
        assert_eq!(store.current_manifest_hash(Uuid(18)).unwrap(), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generation_rejects_manifest_tags_that_disagree_with_the_plant_artifact() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let map = Uuid(17);
        let family = Uuid(23);
        let versions = CookVersionSet::current();
        let platform = CookPlatformProfile {
            target: "test-target".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "test-toolchain".to_owned(),
            features: vec!["indexed-mdi".to_owned()],
        };
        let mut node = CookNodeRecord {
            address: CookNodeAddress::Plant { family },
            cook_key: ContentHash::default(),
            output_hash: ContentHash::default(),
            dependencies: Vec::new(),
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
        };
        node.cook_key = node.calculate_cook_key(versions, &platform).unwrap();
        let artifact_tags = vec![PlantTagId::new(31).unwrap()];
        let artifact_bytes = write_plant_compiled_artifact(
            PlantCompiledArtifactHeader {
                family,
                cook_key: node.cook_key,
                platform_profile: platform.identity().unwrap(),
            },
            &plant_sections(&artifact_tags),
        )
        .unwrap();
        let artifact = store.publish_plant(&artifact_bytes).unwrap();
        node.output_hash = artifact.content_hash;
        let graph_bytes = CookGraph {
            versions,
            platform: platform.clone(),
            nodes: vec![node],
        }
        .canonical_bytes()
        .unwrap();
        let graph = store.publish_cook_graph(&graph_bytes).unwrap();
        let mut manifest = saffron_vegetation::VegetationBaseManifest::current(
            Uuid(9),
            map,
            ContentHash::of(b"map"),
            versions,
            platform,
            graph.content_hash,
        );
        manifest.plants.push(VegetationManifestPlant {
            family,
            tags: vec![PlantTagId::new(37).unwrap()],
            source_hash: ContentHash::of(b"plant source"),
            artifact_hash: artifact.content_hash,
            local_bounds_min: [DecisionScalar::from_integer(-1).unwrap(); 3],
            local_bounds_max: [DecisionScalar::from_integer(1).unwrap(); 3],
            variation_count: 1,
            phenotype_count: 1,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        });

        assert!(matches!(
            store.publish_generation(map, None, &manifest.canonical_bytes().unwrap()),
            Err(Error::VegetationPlantTagMismatch { family: 23 })
        ));
        assert_eq!(store.current_manifest_hash(map).unwrap(), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
