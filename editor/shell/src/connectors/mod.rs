//! The store-connector framework: a small async trait plus the normalized result shape
//! the webview consumes. Connectors call external asset services over HTTP (editor-side
//! only — the engine never makes outbound requests), map each provider response onto the
//! canonical [`StoreResult`], and download a deliverable to a local path the host importer
//! reads.
//!
//! [`AuthKind`] is the single switch every credential-dependent capability branches on.

mod ambientcg;
mod cache;
mod credentials;
mod oauth_loopback;
mod polyhaven;
mod polypizza;
mod registry;
mod session;
mod sketchfab;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

pub use cache::ResourceCache;
pub use credentials::Credentials;
pub use oauth_loopback::{OAuthLoopbackConfig, run_loopback_login};
pub use registry::{ConnectorInfo, ConnectorRegistry};
pub use session::SearchSession;

/// How a connector authenticates — the one switch the framework branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    /// No credential; a unique `User-Agent` header only (Poly Haven, ambientCG).
    None,
    /// A pasted API key held in the OS keyring (Poly Pizza).
    ApiKey,
    /// An OAuth implicit-flow token obtained via the loopback capability (Sketchfab).
    OauthLoopback,
}

/// The asset kind a result resolves to; drives the host-side importer choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StoreKind {
    Model,
    Hdri,
    Material,
    Texture,
}

/// A structured license — never a free string, so attribution can be enforced at import.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreLicense {
    /// Canonical license id (`cc0`, `cc-by`, `cc-by-sa`, …).
    pub id: String,
    /// Whether the license requires visible attribution (CC-BY / Sketchfab).
    pub requires_attribution: bool,
    /// Canonical license url.
    pub url: String,
}

/// The source service shown to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreRef {
    pub id: String,
    pub display_name: String,
}

/// The handle a connector's `download()` needs, plus the deliverable format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreImportDescriptor {
    /// `glb` | `gltf` | `usdz` | `texture-zip` | …
    pub format: String,
    /// Download url / asset handle / api id (connector-defined).
    #[serde(rename = "ref")]
    pub ref_: String,
    /// The resolution chosen at import (`1K`/`2K`/`4K`/`8K`); the connector selects the
    /// matching variant, falling back to the nearest the asset offers. Set by the bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// One normalized search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreResult {
    pub id: String,
    pub store: StoreRef,
    pub kind: StoreKind,
    pub name: String,
    pub author: String,
    pub thumbnail_url: String,
    pub source_url: String,
    pub license: StoreLicense,
    pub import_descriptor: StoreImportDescriptor,
    /// Whether this asset exposes individually-selectable parts (drives the split button).
    pub has_parts: bool,
    /// Whether the asset offers resolution variants (drives the resolution picker). False
    /// for single-file deliverables (Poly Pizza GLB, Sketchfab archive).
    pub supports_resolution: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tri_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// One selectable constituent of an asset (a single map, a mesh, …). `ref`/`bundle` are
/// connector-opaque download handles; `role` drives the import colorspace for a texture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPart {
    pub id: String,
    pub label: String,
    pub import_kind: StoreKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The connector's download handle for this part (a url, an inner filename, …).
    #[serde(rename = "ref")]
    pub ref_: String,
    /// The bundle (zip/archive) to fetch+cache before extracting `ref`, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
}

/// One preview image in an asset's gallery: a URL the webview loads directly, plus an
/// optional human label (a map name for a model's textures, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryImage {
    /// The display URL — small enough for the card thumbnail (which avoids a re-fetch blink
    /// on hover by matching the card's existing thumbnail).
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A higher-res variant shown only in the large detail/modal view; falls back to `url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_url: Option<String>,
}

/// A committed search: free text, an optional kind filter, and the store to search.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub kind: Option<StoreKind>,
    /// The single connector id the search runs against (chosen in the Store's store dropdown).
    #[serde(default)]
    pub provider: String,
}

/// An opaque, connector-defined cursor (offset, page token, …).
#[derive(Debug, Clone)]
pub struct StoreCursor(pub String);

/// One page of results from a single connector.
pub struct SearchPage {
    pub results: Vec<StoreResult>,
    pub next_cursor: Option<StoreCursor>,
    pub exhausted: bool,
}

/// A connector failure. Typed per the workspace convention — no stringly errors.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("http error: {0}")]
    Http(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("connector '{0}' is not configured")]
    NotConfigured(String),
}

/// A live connection to one external asset service.
#[async_trait]
pub trait StoreConnector: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn auth_kind(&self) -> AuthKind;
    /// A one-line description shown in the provider detail panel.
    fn description(&self) -> &'static str {
        ""
    }
    /// The provider's homepage, linked from the detail panel.
    fn website(&self) -> &'static str {
        ""
    }
    /// The OAuth loopback descriptor for an `oauth_loopback` connector, or `None`.
    fn oauth_config(&self) -> Option<OAuthLoopbackConfig> {
        None
    }
    async fn search(
        &self,
        query: &SearchQuery,
        cursor: Option<StoreCursor>,
    ) -> Result<SearchPage, ConnectorError>;
    /// Downloads the whole-asset deliverable, reporting a 0.0–1.0 fraction via `progress`
    /// as bytes arrive.
    async fn download(
        &self,
        descriptor: &StoreImportDescriptor,
        progress: &ProgressFn,
    ) -> Result<PathBuf, ConnectorError>;
    /// The asset's preview images (lazy; resolved when the gallery/detail view opens). The
    /// default is just the card thumbnail; a connector that exposes per-map or per-render
    /// preview URLs overrides this to return several, which drives the gallery's nav arrows.
    async fn gallery(&self, result: &StoreResult) -> Result<Vec<GalleryImage>, ConnectorError> {
        Ok(vec![GalleryImage {
            url: result.thumbnail_url.clone(),
            label: None,
            full_url: None,
        }])
    }
    /// The individually-selectable parts of a result (lazy; resolved when the dropdown opens).
    async fn parts(&self, _result: &StoreResult) -> Result<Vec<AssetPart>, ConnectorError> {
        Ok(Vec::new())
    }
    /// Downloads one part to a local file the host importer reads, at the chosen resolution
    /// when the asset offers variants (falling back to the part's own default).
    async fn download_part(
        &self,
        _part: &AssetPart,
        _resolution: Option<&str>,
    ) -> Result<PathBuf, ConnectorError> {
        Err(ConnectorError::UnsupportedFormat(
            "connector has no selectable parts".to_owned(),
        ))
    }
}

/// The connector runtime: the registry, the shared resource cache, plus live search sessions.
pub struct ConnectorRuntime {
    registry: ConnectorRegistry,
    cache: Arc<ResourceCache>,
    sessions: StdMutex<HashMap<String, Arc<AsyncMutex<SearchSession>>>>,
    next_id: AtomicU64,
}

impl ConnectorRuntime {
    pub fn new() -> Self {
        // A persistent root (survives restarts), shared by every connector and the image scheme.
        let cache = ResourceCache::new(crate::geometry::app_data_dir().join("cache"));
        Self {
            registry: ConnectorRegistry::new(Arc::clone(&cache)),
            cache,
            sessions: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// The shared resource cache the `saffron-img://` scheme serves thumbnails from.
    pub fn cache(&self) -> Arc<ResourceCache> {
        Arc::clone(&self.cache)
    }

    pub fn infos(&self) -> Vec<ConnectorInfo> {
        self.registry.infos()
    }

    pub fn connector(&self, id: &str) -> Option<Arc<dyn StoreConnector>> {
        self.registry.by_id(id)
    }

    /// The OAuth loopback descriptor for `id`, if it is an `oauth_loopback` connector.
    pub fn oauth_config(&self, id: &str) -> Option<OAuthLoopbackConfig> {
        self.registry.by_id(id).and_then(|c| c.oauth_config())
    }

    /// Starts a session searching `query.provider` and returns its id. Errors when the id names
    /// no known connector. Old sessions are dropped when the webview stops polling them.
    pub fn start_session(&self, query: SearchQuery) -> Result<String, ConnectorError> {
        let connector = self
            .registry
            .by_id(&query.provider)
            .ok_or_else(|| ConnectorError::NotConfigured(query.provider.clone()))?;
        let session = SearchSession::new(query, connector);
        let id = format!("s{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), Arc::new(AsyncMutex::new(session)));
        Ok(id)
    }

    pub fn session(&self, id: &str) -> Option<Arc<AsyncMutex<SearchSession>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }
}

impl Default for ConnectorRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// The unique `User-Agent` every connector request carries.
pub fn user_agent() -> String {
    format!(
        "saffron-anima/{} (+https://github.com/saffronjam/saffron-anima)",
        env!("CARGO_PKG_VERSION")
    )
}

/// A download-progress sink: receives a 0.0–1.0 fraction as bytes arrive.
pub type ProgressFn<'a> = dyn Fn(f64) + Send + Sync + 'a;

/// Extracts a zip's files (flattened to basenames) into `dir`. Material map sets ship as a
/// flat zip of role-named images, which the host's `import_material_folder` scans by name.
pub fn extract_zip(bytes: &[u8], dir: &std::path::Path) -> Result<(), ConnectorError> {
    std::fs::create_dir_all(dir).map_err(|e| ConnectorError::Download(e.to_string()))?;
    let reader = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| ConnectorError::Download(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ConnectorError::Download(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|f| f.to_owned()))
        else {
            continue;
        };
        let mut out = std::fs::File::create(dir.join(name))
            .map_err(|e| ConnectorError::Download(e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| ConnectorError::Download(e.to_string()))?;
    }
    Ok(())
}
