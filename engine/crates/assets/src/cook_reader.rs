//! Immutable project view used by background asset cookers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, load_mesh_from_bytes, load_mesh_skin_from_bytes, read_container,
};
use saffron_scene::{AssetCatalog, AssetType};
use saffron_vegetation::ContentHash;

use crate::load::CpuMeshSource;
use crate::model::{ByteSource, ModelAsset, read_container_metadata};
use crate::{AssetServer, Error, Result};

/// Immutable filesystem and catalog snapshot captured for one background cook.
#[derive(Clone, Debug)]
pub struct CookProjectView {
    /// Project `assets/` directory captured when the job was queued.
    pub asset_root: PathBuf,
    /// Project-local derived vegetation cache captured with the asset root.
    pub cache_root: PathBuf,
    /// Catalog snapshot used for every authored asset lookup in the worker.
    pub catalog: Arc<AssetCatalog>,
}

/// Exact file span read while producing a staged cook.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuthoredInputGuard {
    /// Canonical absolute source path.
    pub path: PathBuf,
    /// First byte read from the file.
    pub offset: u64,
    /// Byte count read, or zero for the whole file.
    pub length: u64,
    /// Content identity of the exact bytes read.
    pub content_hash: ContentHash,
}

impl AuthoredInputGuard {
    /// Re-reads and verifies the exact source span.
    pub fn validate(&self) -> Result<()> {
        let bytes = ByteSource {
            path: self.path.display().to_string(),
            offset: self.offset,
            length: self.length,
        }
        .read()?;
        if ContentHash::of(&bytes) != self.content_hash {
            return Err(Error::VegetationCookInputChanged {
                path: self.path.display().to_string(),
            });
        }
        Ok(())
    }
}

impl CookProjectView {
    /// Captures the read-only project identity required by a worker.
    #[must_use]
    pub fn capture(assets: &AssetServer) -> Self {
        Self {
            asset_root: assets.root.clone(),
            cache_root: assets.vegetation_cache_root.clone(),
            catalog: Arc::new(assets.catalog.clone()),
        }
    }
}

pub(crate) trait CookAssetAccess {
    fn root(&self) -> &Path;
    fn catalog(&self) -> &AssetCatalog;
    fn load_model(&mut self, id: Uuid) -> Option<Arc<ModelAsset>>;

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(|error| Error::Io(error.to_string()))
    }

    fn read_source(&self, source: &ByteSource) -> Result<Vec<u8>> {
        source.read()
    }

    fn chunk_source(&self, model: &ModelAsset, kind: ChunkKind, id: Uuid) -> ByteSource {
        let key = id.value().to_string();
        if let Some(remap) = model.meta.remap.as_object().and_then(|map| map.get(&key))
            && let Some(external) = remap.get("external").and_then(serde_json::Value::as_str)
        {
            let path = self.root().join(external);
            if path.exists() {
                return ByteSource {
                    path: path.display().to_string(),
                    ..ByteSource::default()
                };
            }
        }
        model
            .reader
            .find(kind, id.value())
            .map_or_else(ByteSource::default, |entry| ByteSource {
                path: model.reader.path().display().to_string(),
                offset: entry.offset,
                length: entry.length,
            })
    }

    fn load_mesh_source(&mut self, id: Uuid) -> Result<CpuMeshSource> {
        let entry = self
            .catalog()
            .find(id)
            .cloned()
            .ok_or(Error::NotInCatalog(id.value()))?;
        if entry.asset_type != AssetType::Mesh {
            return Err(Error::WrongAssetType {
                id: id.value(),
                wanted: "mesh",
            });
        }
        let bytes = if entry.container.value() == 0 {
            let direct = self.root().join(&entry.path);
            let path = if direct.exists() {
                direct
            } else if let Some(suffix) = entry.path.strip_prefix("meshes/") {
                self.root().join("models").join(suffix)
            } else {
                direct
            };
            self.read_file(&path)?
        } else {
            let model = self.load_model(entry.container).ok_or_else(|| {
                Error::Io(format!(
                    "mesh {}: container {} is not loadable",
                    id.value(),
                    entry.container.value()
                ))
            })?;
            let source = self.chunk_source(&model, ChunkKind::Mesh, id);
            if source.is_empty() {
                return Err(Error::ContainerMissingSubAsset {
                    container: entry.container.value(),
                    sub: id.value(),
                });
            }
            self.read_source(&source)?
        };
        Ok(CpuMeshSource {
            mesh: load_mesh_from_bytes(&bytes)?,
            skin: load_mesh_skin_from_bytes(&bytes)?,
        })
    }
}

impl CookAssetAccess for AssetServer {
    fn root(&self) -> &Path {
        &self.root
    }

    fn catalog(&self) -> &AssetCatalog {
        &self.catalog
    }

    fn load_model(&mut self, id: Uuid) -> Option<Arc<ModelAsset>> {
        self.load_model_asset(id)
    }

    fn chunk_source(&self, model: &ModelAsset, kind: ChunkKind, id: Uuid) -> ByteSource {
        self.chunk_source_for(model, kind, id)
    }

    fn load_mesh_source(&mut self, id: Uuid) -> Result<CpuMeshSource> {
        self.load_mesh_cpu_source(id)
    }
}

pub(crate) struct CookAssetReader {
    view: CookProjectView,
    models: BTreeMap<u64, Option<Arc<ModelAsset>>>,
    guards: Mutex<BTreeMap<(PathBuf, u64, u64), ContentHash>>,
}

impl CookAssetReader {
    pub(crate) fn new(view: CookProjectView) -> Self {
        Self {
            view,
            models: BTreeMap::new(),
            guards: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn cache_root(&self) -> &Path {
        &self.view.cache_root
    }

    pub(crate) fn guards(&self) -> Vec<AuthoredInputGuard> {
        self.guards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(
                |((path, offset, length), content_hash)| AuthoredInputGuard {
                    path: path.clone(),
                    offset: *offset,
                    length: *length,
                    content_hash: *content_hash,
                },
            )
            .collect()
    }

    fn record(&self, source: &ByteSource, bytes: &[u8]) -> Result<()> {
        let path = PathBuf::from(&source.path);
        let key = (path.clone(), source.offset, source.length);
        let hash = ContentHash::of(bytes);
        let mut guards = self
            .guards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = guards.insert(key, hash)
            && previous != hash
        {
            return Err(Error::VegetationCookInputChanged {
                path: path.display().to_string(),
            });
        }
        Ok(())
    }
}

impl CookAssetAccess for CookAssetReader {
    fn root(&self) -> &Path {
        &self.view.asset_root
    }

    fn catalog(&self) -> &AssetCatalog {
        &self.view.catalog
    }

    fn load_model(&mut self, id: Uuid) -> Option<Arc<ModelAsset>> {
        if let Some(cached) = self.models.get(&id.value()) {
            return cached.clone();
        }
        let opened = self.view.catalog.find(id).and_then(|entry| {
            if !matches!(entry.asset_type, AssetType::Model | AssetType::Material) {
                return None;
            }
            let path = self.view.asset_root.join(&entry.path);
            self.read_file(&path).ok()?;
            let meta = read_container_metadata(&path).ok()?;
            let reader = read_container(&path).ok()?;
            Some(Arc::new(ModelAsset { meta, reader }))
        });
        self.models.insert(id.value(), opened.clone());
        opened
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let source = ByteSource {
            path: path.display().to_string(),
            ..ByteSource::default()
        };
        self.read_source(&source)
    }

    fn read_source(&self, source: &ByteSource) -> Result<Vec<u8>> {
        let bytes = source.read()?;
        self.record(source, &bytes)?;
        Ok(bytes)
    }
}
