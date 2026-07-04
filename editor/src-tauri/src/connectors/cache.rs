//! The shared resource cache: the one place the bridge fetches a remote resource, keeps it on
//! disk (persisting across restarts), and serves it back — to the webview (thumbnails / gallery
//! previews, via the `saffron-img://` scheme) and to Rust callers (connector downloads, extracted
//! deliverables). Anything that needs "fetch a URL once, keep it, serve it" routes through here
//! rather than minting its own directory and re-inventing this.
//!
//! Two storage shapes share one root:
//! - **Blobs** — content-addressed single files under `blobs/{key}.{ext}`, each with a
//!   `{key}.meta.json` sidecar (the blob name is only a hash, so the sidecar records what it is:
//!   the source url, the content type used to serve it, and when it landed). Fetch-once.
//! - **Derived** — materialized directories under `derived/{key}/` (an extracted map set, a
//!   multi-file glTF), built once into a `.partial` sibling and atomically renamed into place so a
//!   crashed or half-finished build never looks cached.
//!
//! A bounded [`Semaphore`] caps upstream concurrency: only cache misses acquire a permit, so a
//! screenful of thumbnails drains through the gate instead of stampeding a provider (which is what
//! made the CDN drop requests and show broken images).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use super::{ConnectorError, ProgressFn, user_agent};

/// Max simultaneous upstream fetches. Cache hits don't take a permit.
const MAX_CONCURRENT_FETCHES: usize = 6;

/// A resource-cache failure.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("http error: {0}")]
    Http(String),
    #[error("bad status {0} for {1}")]
    Status(u16, String),
    #[error("cache io error: {0}")]
    Io(String),
}

impl From<CacheError> for ConnectorError {
    fn from(e: CacheError) -> Self {
        match e {
            CacheError::Http(m) => ConnectorError::Http(m),
            CacheError::Status(s, url) => ConnectorError::Download(format!("{s} for {url}")),
            CacheError::Io(m) => ConnectorError::Download(m),
        }
    }
}

/// Sidecar written next to each blob (once, at fetch time). Records what a hash-named blob is, how
/// to serve it, and when it was fetched — enough for reverse lookup, the `Content-Type` header,
/// and any future freshness / eviction pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlobMeta {
    url: String,
    ext: String,
    content_type: String,
    fetched_at: u64,
}

/// A cached blob on disk.
pub struct CachedBlob {
    pub path: PathBuf,
    pub content_type: String,
}

/// A materialized directory: `dir` is the final cached path, `tmp` its build sibling. `exists`
/// reports whether the final directory is already built; `begin` prepares a clean `tmp` to build
/// into and `commit` atomically renames it into place.
pub struct Derived {
    dir: PathBuf,
    tmp: PathBuf,
}

impl Derived {
    /// The final cached directory path.
    pub fn path(&self) -> &std::path::Path {
        &self.dir
    }

    /// Whether the directory is already built (a cache hit).
    pub fn exists(&self) -> bool {
        self.dir.is_dir()
    }

    /// Prepare a clean staging directory to build into; returns its path.
    pub fn begin(&self) -> Result<PathBuf, CacheError> {
        let _ = std::fs::remove_dir_all(&self.tmp);
        std::fs::create_dir_all(&self.tmp).map_err(|e| CacheError::Io(e.to_string()))?;
        Ok(self.tmp.clone())
    }

    /// Atomically publish the staging directory as the final cached one.
    pub fn commit(&self) -> Result<PathBuf, CacheError> {
        let _ = std::fs::remove_dir_all(&self.dir);
        if let Some(parent) = self.dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CacheError::Io(e.to_string()))?;
        }
        std::fs::rename(&self.tmp, &self.dir).map_err(|e| CacheError::Io(e.to_string()))?;
        Ok(self.dir.clone())
    }
}

/// The bridge's one HTTP + disk-cache + throttle layer.
pub struct ResourceCache {
    http: reqwest::Client,
    root: PathBuf,
    gate: Semaphore,
}

impl ResourceCache {
    /// Build a cache rooted at `root` (a persistent location, so it survives restarts). The HTTP
    /// client carries the shared `User-Agent` every provider expects, so callers using
    /// [`client`](Self::client) need not set it per request.
    pub fn new(root: PathBuf) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Arc::new(Self {
            http,
            root,
            gate: Semaphore::new(MAX_CONCURRENT_FETCHES),
        })
    }

    /// The raw client, for **dynamic** provider calls that must not be cached — search listings,
    /// download-manifest lookups, HEAD probes. Cacheable bytes go through the blob/fetch methods.
    pub fn client(&self) -> &reqwest::Client {
        &self.http
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    /// A handle to a materialized directory keyed by `key` (an extracted set, a multi-file model).
    pub fn derived(&self, key: &str) -> Derived {
        let base = self.root.join("derived");
        let name = sanitize(key);
        Derived {
            dir: base.join(&name),
            tmp: base.join(format!("{name}.partial")),
        }
    }

    /// A cached blob's bytes + content type, fetching once (throttled) on a miss. Backs the image
    /// URI scheme.
    pub async fn bytes(&self, url: &str) -> Result<(Vec<u8>, String), CacheError> {
        let blob = self.blob(url, url, None, None).await?;
        let data = std::fs::read(&blob.path).map_err(|e| CacheError::Io(e.to_string()))?;
        Ok((data, blob.content_type))
    }

    /// The on-disk path to a cached blob (fetch-once, keyed by the url). `ext` names the file for a
    /// format-sniffing importer. Reports progress on a miss.
    pub async fn file(
        &self,
        url: &str,
        ext: &str,
        progress: Option<&ProgressFn<'_>>,
    ) -> Result<PathBuf, CacheError> {
        Ok(self.blob(url, url, Some(ext), progress).await?.path)
    }

    /// Like [`file`](Self::file) but cached under an explicit `cache_key` rather than the url — for
    /// resources whose download url is signed/ephemeral (a Sketchfab archive keyed by model uid),
    /// so a repeat resolves to the same cached file.
    pub async fn file_keyed(
        &self,
        cache_key: &str,
        url: &str,
        ext: &str,
        progress: Option<&ProgressFn<'_>>,
    ) -> Result<PathBuf, CacheError> {
        Ok(self.blob(cache_key, url, Some(ext), progress).await?.path)
    }

    /// A throttled, **un**-cached fetch (bytes + content type) with optional progress — for content
    /// the caller materializes elsewhere (a zip it extracts, a file set it lays into a derived
    /// dir). Shares the same concurrency gate as cached fetches.
    pub async fn fetch(
        &self,
        url: &str,
        progress: Option<&ProgressFn<'_>>,
    ) -> Result<(Vec<u8>, String), CacheError> {
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|e| CacheError::Io(e.to_string()))?;
        self.stream(url, progress).await
    }

    /// Blob-cache core: return the cached file when its sidecar + blob are present, else fetch
    /// (throttled), write the blob then its sidecar, and return.
    async fn blob(
        &self,
        cache_key: &str,
        url: &str,
        ext_hint: Option<&str>,
        progress: Option<&ProgressFn<'_>>,
    ) -> Result<CachedBlob, CacheError> {
        let key = hash_key(cache_key);
        let dir = self.blobs_dir();
        let meta_path = dir.join(format!("{key}.meta.json"));
        if let Ok(text) = std::fs::read_to_string(&meta_path) {
            if let Ok(meta) = serde_json::from_str::<BlobMeta>(&text) {
                let blob_path = dir.join(format!("{key}.{}", meta.ext));
                if blob_path.exists() {
                    if let Some(p) = progress {
                        p(1.0);
                    }
                    return Ok(CachedBlob {
                        path: blob_path,
                        content_type: meta.content_type,
                    });
                }
            }
        }

        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|e| CacheError::Io(e.to_string()))?;
        let (bytes, content_type) = self.stream(url, progress).await?;
        let ext = ext_hint
            .map(str::to_owned)
            .unwrap_or_else(|| ext_for(&content_type, url));
        std::fs::create_dir_all(&dir).map_err(|e| CacheError::Io(e.to_string()))?;
        let blob_path = dir.join(format!("{key}.{ext}"));
        write_atomic(&blob_path, &bytes)?;
        let meta = BlobMeta {
            url: url.to_owned(),
            ext: ext.clone(),
            content_type: content_type.clone(),
            fetched_at: now_secs(),
        };
        let meta_json = serde_json::to_vec(&meta).map_err(|e| CacheError::Io(e.to_string()))?;
        // Sidecar last: its presence implies a fully-written blob.
        write_atomic(&meta_path, &meta_json)?;
        Ok(CachedBlob {
            path: blob_path,
            content_type,
        })
    }

    /// Streaming GET into memory (bytes + content type), reporting a 0..1 fraction. The caller
    /// holds the concurrency permit.
    async fn stream(
        &self,
        url: &str,
        progress: Option<&ProgressFn<'_>>,
    ) -> Result<(Vec<u8>, String), CacheError> {
        let mut resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| CacheError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CacheError::Status(status.as_u16(), url.to_owned()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_owned())
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let total = resp.content_length();
        let mut buf = Vec::with_capacity(total.unwrap_or(0) as usize);
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| CacheError::Http(e.to_string()))?
        {
            buf.extend_from_slice(&chunk);
            if let (Some(p), Some(t)) = (progress, total) {
                if t > 0 {
                    p(buf.len() as f64 / t as f64);
                }
            }
        }
        if let Some(p) = progress {
            p(1.0);
        }
        Ok((buf, content_type))
    }
}

/// Content-address key for a cache key string (stable across restarts within a toolchain).
fn hash_key(key: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A filesystem-safe directory name for a derived-artifact key.
fn sanitize(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A file extension for a fetched blob: from the content type, else the url's own extension, else
/// `bin`. Only used when the caller gives no explicit `ext` (i.e. images served by content type).
fn ext_for(content_type: &str, url: &str) -> String {
    match content_type {
        "image/png" => return "png".to_owned(),
        "image/jpeg" => return "jpg".to_owned(),
        "image/webp" => return "webp".to_owned(),
        "image/gif" => return "gif".to_owned(),
        "image/avif" => return "avif".to_owned(),
        "image/svg+xml" => return "svg".to_owned(),
        _ => {}
    }
    url.rsplit('/')
        .next()
        .and_then(|f| f.split('?').next())
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.len() <= 5)
        .unwrap_or_else(|| "bin".to_owned())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write `bytes` to `path` via a temp sibling + rename, so a reader never sees a partial file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), CacheError> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("part")
    ));
    std::fs::write(&tmp, bytes).map_err(|e| CacheError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| CacheError::Io(e.to_string()))?;
    Ok(())
}
