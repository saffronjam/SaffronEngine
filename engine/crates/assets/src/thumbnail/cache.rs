use std::path::{Path, PathBuf};

use crate::{AssetServer, Error, Result};

use super::ThumbnailPng;

/// The thumbnail cache version. It prefixes every on-disk cache filename
/// (`v<VERSION>-<contentHash>-<size>.png` under the app-level cache dir), so a render-behaviour
/// change retires the whole cache — every kind, not just materials — by bumping this one number:
/// the new prefix simply never matches the old files (which age out via the size-cap eviction).
/// Bump it whenever the rendered look of a tile changes.
pub const THUMBNAIL_CACHE_VERSION: u32 = 12;

/// The app-level cache is bounded to this many bytes; a write that pushes it over the cap
/// evicts the oldest files (by mtime) down to [`THUMBNAIL_CACHE_EVICT_BYTES`].
const THUMBNAIL_CACHE_MAX_BYTES: u64 = 1 << 30;
/// The low-water mark eviction drains down to, so a burst of writes does not re-trigger a
/// full eviction on every file.
const THUMBNAIL_CACHE_EVICT_BYTES: u64 = THUMBNAIL_CACHE_MAX_BYTES / 5 * 4;

/// What the on-disk thumbnail cache holds: entry count + total bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailCacheStats {
    /// Number of cached thumbnail files.
    pub entries: u32,
    /// Total bytes across the cache files.
    pub bytes: u64,
}

/// A cached thumbnail's bytes + the dimensions read from its PNG header (so a hit reports
/// truthful width/height without a decode). `None` if absent or not a readable PNG.
pub(super) fn read_thumbnail_cache(path: &Path) -> Option<ThumbnailPng> {
    let bytes = std::fs::read(path).ok()?;
    // 8-byte signature + IHDR length/type + the width/height fields.
    if bytes.len() < 24 {
        return None;
    }
    const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes[..8] != PNG_SIG {
        return None;
    }
    let be32 = |at: usize| -> u32 {
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    };
    let width = be32(16);
    let height = be32(20);
    Some(ThumbnailPng {
        bytes,
        width,
        height,
    })
}

/// Writes a generated PNG into the cache dir, creating the parent dir, then bounds the shared cache
/// with a size-cap eviction.
///
/// # Errors
///
/// [`Error::Io`] if the parent dir cannot be created or the file cannot be written.
pub fn write_thumbnail_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    std::fs::write(path, bytes).map_err(|e| {
        Error::Io(format!(
            "write failed for thumbnail cache '{}': {e}",
            path.display()
        ))
    })?;
    if let Some(parent) = path.parent() {
        evict_thumbnail_cache(
            parent,
            THUMBNAIL_CACHE_MAX_BYTES,
            THUMBNAIL_CACHE_EVICT_BYTES,
        );
    }
    Ok(())
}

/// When `dir`'s total size exceeds `max`, deletes the oldest files (by mtime) until it is
/// back under `target`. A no-op while under `max` (the common case).
fn evict_thumbnail_cache(dir: &Path, max: u64, target: u64) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in read.flatten() {
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            files.push((entry.path(), mtime, meta.len()));
        }
    }
    if total <= max {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime);
    for (file, _, size) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&file).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

impl AssetServer {
    /// The cache path for a content hash + size (`v<VERSION>-<contentHash>-<size>.png` under the
    /// app-level thumbnail cache dir). The [`THUMBNAIL_CACHE_VERSION`] prefix makes a version bump
    /// retire every kind's tiles (mesh/texture/model key on the stored `content_hash`, which carries
    /// no version of its own), so the one constant is authoritative for the whole cache.
    pub(super) fn thumbnail_content_cache_path(&self, content_hash: u64, size: u32) -> PathBuf {
        self.thumbnail_cache_dir().join(format!(
            "v{THUMBNAIL_CACHE_VERSION}-{content_hash}-{size}.png"
        ))
    }

    /// What the on-disk thumbnail cache holds (count + bytes).
    #[must_use]
    pub fn thumbnail_cache_stats(&self) -> ThumbnailCacheStats {
        let mut stats = ThumbnailCacheStats::default();
        let dir = self.thumbnail_cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return stats;
        };
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                stats.entries += 1;
                stats.bytes += meta.len();
            }
        }
        stats
    }

    /// Empties the app-level cache dir, returning what was removed.
    pub fn clear_thumbnail_cache_dir(&self) -> ThumbnailCacheStats {
        let removed = self.thumbnail_cache_stats();
        let dir = self.thumbnail_cache_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::temp_root;

    /// The cache is content-addressed with a version prefix (`v<VERSION>-<contentHash>-<size>.png`),
    /// no project/uuid in it, so identical content shares one file across projects and a version
    /// bump retires every kind's tiles.
    #[test]
    fn content_cache_path_is_version_hash_and_size() {
        let assets = AssetServer::new(temp_root("path"));
        let path = assets.thumbnail_content_cache_path(0xABCD, 128);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("v{THUMBNAIL_CACHE_VERSION}-43981-128.png")).as_deref(),
            "the filename is the version prefix, decimal content hash, and size"
        );
        assert_eq!(path.parent(), Some(assets.thumbnail_cache_dir().as_path()));
    }

    #[test]
    fn eviction_drops_oldest_files_down_to_target() {
        let dir = temp_root("evict")
            .parent()
            .expect("parent")
            .join("thumb-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // Five 100-byte files, written oldest-first with strictly increasing mtimes so the
        // eviction order is deterministic.
        for i in 0..5u32 {
            let path = dir.join(format!("{i}.png"));
            std::fs::write(&path, [0u8; 100]).expect("write");
            filetime_set(&path, 1_000 + u64::from(i));
        }

        evict_thumbnail_cache(&dir, 450, 250);

        let survivors: Vec<u32> = (0..5)
            .filter(|i| dir.join(format!("{i}.png")).exists())
            .collect();
        assert_eq!(
            survivors,
            vec![3, 4],
            "the two newest files survive; the three oldest are evicted to reach the target"
        );
    }

    /// Sets a file's mtime to `secs` past the epoch, so the eviction order is deterministic rather
    /// than dependent on write timing.
    fn filetime_set(path: &Path, secs: u64) {
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::open(path)
            .and_then(|f| f.set_modified(mtime))
            .expect("set mtime");
    }
}
