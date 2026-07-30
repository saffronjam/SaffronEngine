//! Content-addressed storage for derived vegetation artifacts.

mod generation;
mod work_claims;

#[cfg(test)]
mod test_support;

use std::io::Write;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_geometry::decode_portable_virtual_hierarchy_sections;
use saffron_vegetation::{ContentHash, PlantCompiledSectionKind};

use crate::{Error, Result};

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
    /// Distributed cook work-plan bytes: a work manifest or one item's own-input payload.
    /// Never exported — payloads exist for claimants, and a package carries only results.
    WorkPayload,
}

impl VegetationArtifactKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Plant => "plants",
            Self::Cell => "cells",
            Self::Manifest => "manifests",
            Self::CookGraph => "cook-graphs",
            Self::WorkPayload => "work-payloads",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Plant => "splantc",
            Self::Cell => "svegcell",
            Self::Manifest => "svegmanifest",
            Self::CookGraph => "svegcook",
            Self::WorkPayload => "svegwork",
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

    /// Publishes one strictly validated canonical vegetation cook graph.
    pub fn publish_cook_graph(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::CookGraph, bytes, |bytes| {
            saffron_vegetation::CookGraph::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Publishes one strictly validated cook work manifest.
    pub fn publish_work_manifest(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::WorkPayload, bytes, |bytes| {
            saffron_vegetation::CookWorkManifest::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Publishes one strictly validated work-item own-input payload.
    pub fn publish_work_payload(&self, bytes: &[u8]) -> Result<VegetationArtifactPublication> {
        self.publish_typed(VegetationArtifactKind::WorkPayload, bytes, |bytes| {
            saffron_vegetation::CookWorkPayload::from_canonical_bytes(bytes)?;
            Ok(())
        })
    }

    /// Reads and validates one work-item own-input payload by exact content identity.
    pub fn read_work_payload(&self, hash: ContentHash) -> Result<Vec<u8>> {
        self.read_typed(VegetationArtifactKind::WorkPayload, hash, |bytes| {
            saffron_vegetation::CookWorkPayload::from_canonical_bytes(bytes)?;
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

/// The strict structural read-back every `.splantc` must pass, at cook time and at publication.
pub(crate) fn validate_plant_artifact(bytes: &[u8]) -> Result<()> {
    let index = saffron_vegetation::PlantCompiledArtifactIndex::open(
        bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )?;
    index.family_tags(bytes)?;
    for kind in PlantCompiledSectionKind::ALL {
        // Two facets are legitimately empty: a family that cooked no voxel brick has nothing
        // aggregate to occlude with, and one whose slots resolve to catalog materials packs no
        // atlas. Both write their section and leave it empty.
        if matches!(
            kind,
            PlantCompiledSectionKind::DistanceField | PlantCompiledSectionKind::TextureContainer
        ) {
            continue;
        }
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
    let texture =
        required_plant_section(&index, bytes, PlantCompiledSectionKind::TextureContainer)?;
    if !texture.is_empty() {
        saffron_vegetation::read_plant_texture_container(texture.as_ref())?;
    }
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

pub(crate) fn optional_hash(hash: Option<ContentHash>) -> String {
    hash.map_or_else(|| "none".to_owned(), |hash| hash.to_string())
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
    use super::test_support::{cook_graph_bytes, root};
    use super::*;

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
}
