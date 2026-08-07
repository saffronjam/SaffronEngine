//! The uuid-keyed negative cache: a *present* key holding `None` means the load already failed and
//! must not be retried; only an *absent* key triggers a load attempt.

use std::collections::HashMap;
use std::sync::Arc;

/// A uuid-keyed GPU-asset cache: an id maps to a loaded `Arc<T>`, or to `None` as
/// the negative-cache marker for a load that failed.
pub type AssetCache<T> = HashMap<u64, Option<Arc<T>>>;

/// Returns the asset for `key`, running `load` only on an absent key and caching its outcome —
/// including a `None`, which becomes the negative marker.
pub fn resolve_cached<T, F>(cache: &mut AssetCache<T>, key: u64, load: F) -> Option<Arc<T>>
where
    F: FnOnce() -> Option<Arc<T>>,
{
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let loaded = load();
    cache.insert(key, loaded.clone());
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Increments a shared counter on `Drop`, so a test can assert teardown fired exactly once.
    struct DropCounter {
        counter: Arc<AtomicUsize>,
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn absent_key_loads_exactly_once() {
        let mut cache: AssetCache<u32> = AssetCache::new();
        let calls = AtomicUsize::new(0);
        let got = resolve_cached(&mut cache, 7, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(99u32))
        });
        assert_eq!(*got.unwrap(), 99);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.contains_key(&7));
    }

    #[test]
    fn present_some_returns_cached_without_loading() {
        let mut cache: AssetCache<u32> = AssetCache::new();
        cache.insert(42, Some(Arc::new(5u32)));
        let calls = AtomicUsize::new(0);
        let got = resolve_cached(&mut cache, 42, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(123u32))
        });
        assert_eq!(*got.unwrap(), 5);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "loader must not run on a hit"
        );
    }

    #[test]
    fn present_none_is_negative_cache_not_a_miss() {
        let mut cache: AssetCache<u32> = AssetCache::new();
        cache.insert(13, None);
        let calls = AtomicUsize::new(0);
        let got = resolve_cached(&mut cache, 13, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(1u32))
        });
        assert!(got.is_none(), "negative marker stays None");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a cached None is a negative-cache hit, not a retry"
        );
    }

    #[test]
    fn failed_load_caches_the_negative_marker() {
        let mut cache: AssetCache<u32> = AssetCache::new();
        let calls = AtomicUsize::new(0);
        let first = resolve_cached(&mut cache, 1, || {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert!(first.is_none());
        let second = resolve_cached(&mut cache, 1, || {
            calls.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert!(second.is_none());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the loader runs once; the second call is a negative-cache hit"
        );
    }

    #[test]
    fn last_arc_drop_runs_destroy_exactly_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut cache: AssetCache<DropCounter> = AssetCache::new();
        cache.insert(
            1,
            Some(Arc::new(DropCounter {
                counter: Arc::clone(&counter),
            })),
        );
        let alive = resolve_cached(&mut cache, 1, || None);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        cache.clear();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "an outstanding Arc keeps it alive"
        );
        drop(alive);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "destroy fires once on the last Arc"
        );
    }
}
