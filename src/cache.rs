//! Process-level snapshot cache for deserialized project indexes.
//!
//! Stores immutable `Arc<SymbolIndex>` snapshots keyed by canonical project
//! path. Callers clone the Arc (cheap pointer bump) and query against their
//! own reference without holding any lock.
//!
//! Lifecycle:
//! - `index_project` populates the cache after writing the index to disk.
//! - `load_project_index` checks the cache first; on a miss it deserializes
//!   from disk and populates the cache before returning.
//! - `reindex_batch` (watcher) invalidates the entry after saving a new index
//!   to disk, so the next query picks up the fresh snapshot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use crate::index::SymbolIndex;
use crate::sync_utils::{rw_read, rw_write};

static CACHE: LazyLock<RwLock<HashMap<PathBuf, Arc<SymbolIndex>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Returns the cached index for `path`, or `None` on a cache miss.
pub fn get(path: &Path) -> Option<Arc<SymbolIndex>> {
    rw_read(&CACHE).get(path).cloned()
}

/// Wraps `index` in an `Arc`, stores it under `path`, and returns the Arc.
pub fn insert(path: PathBuf, index: SymbolIndex) -> Arc<SymbolIndex> {
    let arc = Arc::new(index);
    rw_write(&CACHE).insert(path, arc.clone());
    arc
}

/// Removes the entry for `path`. The next `load_project_index` call will
/// reload from disk and repopulate the cache.
pub fn invalidate(path: &Path) {
    rw_write(&CACHE).remove(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses a distinct fake path to avoid cross-test cache pollution
    // when tests run in parallel.
    fn fake_path(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/pitlane_cache_test_{label}"))
    }

    #[test]
    fn test_miss_returns_none() {
        assert!(get(&fake_path("miss")).is_none());
    }

    #[test]
    fn test_hit_after_insert() {
        let path = fake_path("hit");
        insert(path.clone(), SymbolIndex::new());
        assert!(get(&path).is_some());
        invalidate(&path); // clean up
    }

    #[test]
    fn test_invalidate_removes_entry() {
        let path = fake_path("invalidate");
        insert(path.clone(), SymbolIndex::new());
        assert!(get(&path).is_some());
        invalidate(&path);
        assert!(get(&path).is_none());
    }

    #[test]
    fn test_insert_returns_same_arc_as_stored() {
        let path = fake_path("arc_eq");
        let returned = insert(path.clone(), SymbolIndex::new());
        let stored = get(&path).unwrap();
        assert!(Arc::ptr_eq(&returned, &stored));
        invalidate(&path); // clean up
    }
}
