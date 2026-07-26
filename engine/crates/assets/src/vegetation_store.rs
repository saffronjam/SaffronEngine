//! Content-addressed storage for derived vegetation artifacts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::{Error, Result};
use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_geometry::decode_portable_virtual_hierarchy_sections;
use saffron_vegetation::{ContentHash, PlantCompiledSectionKind};

/// One derived vegetation artifact namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VegetationArtifactKind {
    /// Compiled plant-family data.
    Plant,
    /// Immutable cooked world-cell data.
    Cell,
    /// Canonical cooked-world manifest.
    Manifest,
    /// Canonical dependency graph for one cooked generation.
    CookGraph,
    /// Initial persistent-state baseline for one cooked generation.
    ///
    /// A shipped world often starts with authored disturbance or growth already in it. The baseline
    /// is that starting state, keyed by the manifest it belongs to, so a package boots into the world
    /// the author saw rather than into an untouched one.
    Baseline,
}

impl VegetationArtifactKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Plant => "plants",
            Self::Cell => "cells",
            Self::Manifest => "manifests",
            Self::CookGraph => "cook-graphs",
            Self::Baseline => "baselines",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Plant => "splantc",
            Self::Cell => "svegcell",
            Self::Baseline => "svegstate",
            Self::Manifest => "svegmanifest",
            Self::CookGraph => "svegcook",
        }
    }
}

/// Result of atomically publishing one validated artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VegetationArtifactPublication {
    /// SHA-256 identity of the complete artifact bytes.
    pub content_hash: ContentHash,
    /// Final content-addressed path.
    pub path: PathBuf,
    /// Encoded artifact size.
    pub bytes: u64,
    /// Whether byte-identical validated data already occupied the final path.
    pub cache_hit: bool,
}

/// Project-local CAS for disposable plant, cell, cook-graph, and manifest artifacts.
#[derive(Clone, Debug)]
pub struct VegetationArtifactStore {
    root: PathBuf,
}

/// Held project-wide lock for authored vegetation source acceptance.
pub struct VegetationAuthoredLock {
    _file: std::fs::File,
}

/// Held per-map lock for generation-root validation and publication.
pub struct VegetationGenerationLock {
    map: Uuid,
    _file: std::fs::File,
}

impl VegetationArtifactStore {
    /// Binds the store to a cache root outside the authored asset catalog.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store root containing the typed artifact namespaces.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves an artifact's final content-addressed path.
    #[must_use]
    pub fn path(&self, kind: VegetationArtifactKind, hash: ContentHash) -> PathBuf {
        self.root
            .join(kind.directory())
            .join(format!("{hash}.{}", kind.extension()))
    }

    /// Publishes one strictly validated `.splantc` artifact.
    pub fn publish_plant(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(
            VegetationArtifactKind::Plant,
            bytes,
            validate_plant_artifact,
        )
    }

    /// Publishes one strictly validated `.svegcell` artifact.
    pub fn publish_cell(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::Cell, bytes, |bytes| {
            saffron_vegetation::VegetationCellArtifactIndex::open(
                bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )?;
            Ok(())
        })
    }

    /// Publishes one strictly validated canonical cooked-world manifest.
    pub fn publish_manifest(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::Manifest, bytes, |bytes| {
            saffron_vegetation::VegetationBaseManifest::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Publishes one initial persistent-state baseline under the manifest it belongs to.
    ///
    /// Keyed by the manifest rather than content-addressed: a generation has exactly one starting
    /// state, and a second baseline for the same generation would be an ambiguity nothing resolves.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the write fails, and a vegetation error when the snapshot does not decode
    /// against `manifest` — a baseline for another generation would import as garbage.
    pub fn publish_baseline(&self, manifest: ContentHash, bytes: &[u8]) -> Result<PathBuf> {
        saffron_vegetation::VegetationState::from_canonical_bytes(bytes, manifest.bytes())?;
        let path = self.path(VegetationArtifactKind::Baseline, manifest);
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

    /// Reads the baseline for one generation, absent when the generation ships none.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the read fails.
    pub fn read_baseline_if_present(&self, manifest: ContentHash) -> Result<Option<Vec<u8>>> {
        let path = self.path(VegetationArtifactKind::Baseline, manifest);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Io(error.to_string())),
        }
    }

    /// Publishes one strictly validated canonical vegetation cook graph.
    pub fn publish_cook_graph(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::CookGraph, bytes, |bytes| {
            saffron_vegetation::CookGraph::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Reads and validates one `.splantc` artifact by exact content identity.
    pub fn read_plant(&self, hash: ContentHash) -> Result<Vec<u8>> {
        self.read_typed(VegetationArtifactKind::Plant, hash, validate_plant_artifact)
    }

    /// Reads and validates one `.splantc` when it is present in the disposable cache.
    pub fn read_plant_if_present(&self, hash: ContentHash) -> Result<Option<Vec<u8>>> {
        self.read_typed_if_present(VegetationArtifactKind::Plant, hash, validate_plant_artifact)
    }

    /// Reads and validates one `.svegcell` artifact by exact content identity.
    pub fn read_cell(&self, hash: ContentHash) -> Result<Vec<u8>> {
        self.read_typed(VegetationArtifactKind::Cell, hash, |bytes| {
            saffron_vegetation::VegetationCellArtifactIndex::open(
                bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )?;
            Ok(())
        })
    }

    /// Reads and validates one `.svegcell` when it is present in the disposable cache.
    pub fn read_cell_if_present(&self, hash: ContentHash) -> Result<Option<Vec<u8>>> {
        self.read_typed_if_present(VegetationArtifactKind::Cell, hash, |bytes| {
            saffron_vegetation::VegetationCellArtifactIndex::open(
                bytes,
                saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
            )?;
            Ok(())
        })
    }

    /// Opens one cell through the bounded-memory ranged facet reader.
    pub fn open_cell(
        &self,
        hash: ContentHash,
    ) -> Result<saffron_vegetation::VegetationCellArtifactReader<std::fs::File>> {
        let path = self.path(VegetationArtifactKind::Cell, hash);
        let file = std::fs::File::open(&path).map_err(|error| Error::Io(error.to_string()))?;
        let reader = saffron_vegetation::VegetationCellArtifactReader::open(
            file,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )?;
        if reader.artifact_hash() != hash {
            return Err(Error::VegetationArtifactHash {
                path: path.display().to_string(),
            });
        }
        Ok(reader)
    }

    /// Reads and validates exactly one cell facet without allocating unrelated sections.
    pub fn read_cell_section(
        &self,
        hash: ContentHash,
        kind: saffron_vegetation::VegetationCellSectionKind,
    ) -> Result<Option<Vec<u8>>> {
        self.open_cell(hash)?
            .read_section(kind)
            .map_err(Error::from)
    }

    /// Reads and validates one cooked-world manifest by exact content identity.
    pub fn read_manifest(&self, hash: ContentHash) -> Result<Vec<u8>> {
        self.read_typed(VegetationArtifactKind::Manifest, hash, |bytes| {
            saffron_vegetation::VegetationBaseManifest::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Reads one canonical cook graph by exact content identity.
    pub fn read_cook_graph(&self, hash: ContentHash) -> Result<Vec<u8>> {
        self.read_typed(VegetationArtifactKind::CookGraph, hash, |bytes| {
            saffron_vegetation::CookGraph::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

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

    fn publish_typed(
        &self,
        kind: VegetationArtifactKind,
        bytes: &[u8],
        validate: impl Fn(&[u8]) -> Result<()>,
    ) -> Result<VegetationArtifactPublication> {
        validate(bytes)?;
        let content_hash = ContentHash::of(bytes);
        let path = self.path(kind, content_hash);
        if let Ok(existing) = std::fs::read(&path) {
            validate(&existing)?;
            if existing != bytes {
                return Err(Error::VegetationArtifactCollision {
                    path: path.display().to_string(),
                });
            }
            return Ok(VegetationArtifactPublication {
                content_hash,
                path,
                bytes: u64::try_from(bytes.len()).map_err(|_| Error::VegetationArtifactSize)?,
                cache_hit: true,
            });
        }

        let directory = path
            .parent()
            .ok_or_else(|| Error::Io("vegetation artifact path has no parent".to_owned()))?;
        std::fs::create_dir_all(directory).map_err(|error| Error::Io(error.to_string()))?;
        let mut file = AtomicWriteFile::options()
            .open(&path)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| Error::Io(error.to_string()))?;
        file.commit()
            .map_err(|error| Error::Io(error.to_string()))?;

        let published = std::fs::read(&path).map_err(|error| Error::Io(error.to_string()))?;
        validate(&published)?;
        if ContentHash::of(&published) != content_hash {
            return Err(Error::VegetationArtifactHash {
                path: path.display().to_string(),
            });
        }
        Ok(VegetationArtifactPublication {
            content_hash,
            path,
            bytes: u64::try_from(bytes.len()).map_err(|_| Error::VegetationArtifactSize)?,
            cache_hit: false,
        })
    }

    fn read_typed(
        &self,
        kind: VegetationArtifactKind,
        hash: ContentHash,
        validate: impl Fn(&[u8]) -> Result<()>,
    ) -> Result<Vec<u8>> {
        let path = self.path(kind, hash);
        let bytes = std::fs::read(&path).map_err(|error| Error::Io(error.to_string()))?;
        if ContentHash::of(&bytes) != hash {
            return Err(Error::VegetationArtifactHash {
                path: path.display().to_string(),
            });
        }
        validate(&bytes)?;
        Ok(bytes)
    }

    fn read_typed_if_present(
        &self,
        kind: VegetationArtifactKind,
        hash: ContentHash,
        validate: impl Fn(&[u8]) -> Result<()>,
    ) -> Result<Option<Vec<u8>>> {
        let path = self.path(kind, hash);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::Io(error.to_string())),
        };
        if ContentHash::of(&bytes) != hash {
            return Err(Error::VegetationArtifactHash {
                path: path.display().to_string(),
            });
        }
        validate(&bytes)?;
        Ok(Some(bytes))
    }
}

fn validate_plant_artifact(bytes: &[u8]) -> Result<()> {
    let index = saffron_vegetation::PlantCompiledArtifactIndex::open(
        bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
    index.family_tags(bytes)?;
    for kind in PlantCompiledSectionKind::ALL {
        if index
            .section(bytes, kind)?
            .is_none_or(|section| section.is_empty())
        {
            return Err(Error::Io(format!(
                "compiled plant artifact is missing {kind:?}"
            )));
        }
    }
    let triangle =
        required_plant_section(&index, bytes, PlantCompiledSectionKind::TriangleHierarchy)?;
    let voxel = required_plant_section(&index, bytes, PlantCompiledSectionKind::VoxelHierarchy)?;
    let deformation = required_plant_section(&index, bytes, PlantCompiledSectionKind::Deformation)?;
    let pages = required_plant_section(&index, bytes, PlantCompiledSectionKind::PageDirectory)?;
    let ray_tracing = required_plant_section(&index, bytes, PlantCompiledSectionKind::RayTracing)?;
    decode_portable_virtual_hierarchy_sections(
        triangle.as_ref(),
        voxel.as_ref(),
        deformation.as_ref(),
        pages.as_ref(),
        ray_tracing.as_ref(),
    )?;
    Ok(())
}

fn required_plant_section<'a>(
    index: &saffron_vegetation::PlantCompiledArtifactIndex,
    bytes: &'a [u8],
    kind: PlantCompiledSectionKind,
) -> Result<std::borrow::Cow<'a, [u8]>> {
    index
        .section(bytes, kind)?
        .ok_or_else(|| Error::Io(format!("compiled plant artifact is missing {kind:?}")))
}

fn generation_root_bytes(hash: ContentHash) -> Vec<u8> {
    format!("SVEGROOT1\n{hash}\n").into_bytes()
}

fn optional_hash(hash: Option<ContentHash>) -> String {
    hash.map_or_else(|| "none".to_owned(), |hash| hash.to_string())
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| Error::Io("vegetation root path has no parent".to_owned()))?;
    std::fs::create_dir_all(directory).map_err(|error| Error::Io(error.to_string()))?;
    let mut file = AtomicWriteFile::options()
        .open(path)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.commit().map_err(|error| Error::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use saffron_geometry::{
        AppearanceError, HierarchyRepresentation, PortableBounds, PortableHierarchyNode,
        PortableHierarchyPage, PortableMaterialMoments, PortableRayTracingRecord,
        PortableVirtualHierarchy, PortableVoxelBrick, PortableVoxelVertex, VirtualMaterialClass,
    };
    use saffron_spatial::{DecisionScalar, WorldCellKey};
    use saffron_vegetation::{
        CookDependency, CookDependencyAddress, CookGraph, CookNodeAddress, CookNodeRecord,
        CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
        PlantCompiledArtifactHeader, PlantCompiledSection, PlantCompiledSectionKind, PlantTagId,
        VegetationManifestPlant, write_plant_compiled_artifact,
    };

    use super::*;

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "saffron-vegetation-cas-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn cook_graph_bytes() -> Vec<u8> {
        let versions = CookVersionSet::current();
        let platform = CookPlatformProfile {
            target: "aarch64-apple-darwin".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-1.96.0".to_owned(),
            features: vec!["canonical-fixed".to_owned()],
        };
        let mut node = CookNodeRecord {
            address: CookNodeAddress::Cell {
                map: Uuid(10),
                cell: WorldCellKey::base(-1, 2, 0),
            },
            cook_key: ContentHash::default(),
            output_hash: ContentHash::new([7; 32]),
            dependencies: vec![CookDependency {
                address: CookDependencyAddress::Contract {
                    namespace: "test/schema".to_owned(),
                },
                content_hash: ContentHash::new([8; 32]),
                bounds: None,
                halo: DecisionScalar::from_bits(0),
                ancestor_level: None,
            }],
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
        };
        node.cook_key = node.calculate_cook_key(versions, &platform).unwrap();
        CookGraph {
            versions,
            platform,
            nodes: vec![node],
        }
        .canonical_bytes()
        .unwrap()
    }

    fn part_table(tags: &[PlantTagId]) -> Vec<u8> {
        let domain = b"saffron-anima/splantc/part-table/v2";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u64::try_from(domain.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(domain);
        bytes.extend_from_slice(&u64::try_from(tags.len()).unwrap().to_be_bytes());
        for tag in tags {
            bytes.extend_from_slice(&tag.value().to_be_bytes());
        }
        bytes
    }

    fn plant_sections(tags: &[PlantTagId]) -> Vec<PlantCompiledSection> {
        let bounds = PortableBounds {
            min_bits: [-65_536; 3],
            max_bits: [65_536; 3],
        };
        let mut hierarchy = PortableVirtualHierarchy::default();
        hierarchy.voxel_bricks.push(PortableVoxelBrick {
            id: 0,
            dimensions: [8; 3],
            bounds,
            deformed_bounds: bounds,
            occupancy: vec![u8::MAX; 64],
            moments: PortableMaterialMoments::default(),
            material_class: VirtualMaterialClass::Opaque,
            opacity_micromap: false,
            vertices: vec![
                PortableVoxelVertex {
                    position_bits: [-65_536, -65_536, 0],
                    normal_oct: [0, 0],
                },
                PortableVoxelVertex {
                    position_bits: [65_536, -65_536, 0],
                    normal_oct: [0, 0],
                },
                PortableVoxelVertex {
                    position_bits: [0, 65_536, 0],
                    normal_oct: [0, 0],
                },
            ],
            indices: vec![0, 1, 2],
            page: 0,
            appearance_error: AppearanceError::default(),
        });
        hierarchy.nodes.push(PortableHierarchyNode {
            id: 0,
            representation: HierarchyRepresentation::Voxel { brick: 0 },
            parent: None,
            children: Vec::new(),
            page: 0,
            bounds,
            deformed_bounds: bounds,
            appearance_error: AppearanceError::default(),
        });
        hierarchy.pages.push(PortableHierarchyPage {
            id: 0,
            dependency: None,
            node: 0,
            bounds,
            deformed_bounds: bounds,
            transition_error: AppearanceError::default(),
            guaranteed_root: true,
        });
        hierarchy.roots.push(0);
        hierarchy.ray_tracing.push(PortableRayTracingRecord {
            node: 0,
            material_class: VirtualMaterialClass::Opaque,
            opacity_micromap: false,
            requires_any_hit: false,
        });
        PlantCompiledSectionKind::ALL
            .into_iter()
            .map(|kind| {
                let bytes = match kind {
                    PlantCompiledSectionKind::PartTable => part_table(tags),
                    PlantCompiledSectionKind::TriangleHierarchy => {
                        hierarchy.triangle_hierarchy_bytes().unwrap()
                    }
                    PlantCompiledSectionKind::VoxelHierarchy => {
                        hierarchy.voxel_hierarchy_bytes().unwrap()
                    }
                    PlantCompiledSectionKind::Deformation => hierarchy.deformation_bytes().unwrap(),
                    PlantCompiledSectionKind::PageDirectory => {
                        hierarchy.page_directory_bytes().unwrap()
                    }
                    PlantCompiledSectionKind::RayTracing => hierarchy.ray_tracing_bytes().unwrap(),
                    _ => vec![u8::try_from(kind as u16).unwrap()],
                };
                PlantCompiledSection::new(kind, bytes)
            })
            .collect()
    }

    #[test]
    fn publication_is_content_addressed_atomic_and_idempotent() {
        let root = root();
        let _ = std::fs::remove_dir_all(&root);
        let store = VegetationArtifactStore::new(&root);
        let validate = |bytes: &[u8]| {
            if bytes.starts_with(b"valid:") {
                Ok(())
            } else {
                Err(Error::Io("invalid fixture".to_owned()))
            }
        };

        let first = store
            .publish_typed(VegetationArtifactKind::Cell, b"valid:cell", validate)
            .expect("publish");
        assert!(!first.cache_hit);
        assert_eq!(first.content_hash, ContentHash::of(b"valid:cell"));
        assert_eq!(std::fs::read(&first.path).unwrap(), b"valid:cell");

        let second = store
            .publish_typed(VegetationArtifactKind::Cell, b"valid:cell", validate)
            .expect("cache hit");
        assert!(second.cache_hit);
        assert_eq!(second.path, first.path);
        assert_eq!(
            store
                .read_typed(VegetationArtifactKind::Cell, first.content_hash, validate)
                .unwrap(),
            b"valid:cell"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cook_graph_publication_is_typed_and_content_addressed() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let bytes = cook_graph_bytes();

        let publication = store.publish_cook_graph(&bytes).unwrap();
        assert_eq!(publication.content_hash, ContentHash::of(&bytes));
        assert_eq!(
            store.read_cook_graph(publication.content_hash).unwrap(),
            bytes
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validation_failure_never_publishes() {
        let root = root();
        let store = VegetationArtifactStore::new(&root);
        let result = store.publish_typed(VegetationArtifactKind::Plant, b"bad", |_| {
            Err(Error::Io("invalid fixture".to_owned()))
        });
        assert!(result.is_err());
        assert!(!root.exists());
    }

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
