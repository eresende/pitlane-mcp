//! Tracks projects that are currently being indexed in the background.
//!
//! When `index_project` spawns a background task it calls `mark`; when the
//! task completes (success or error) it calls `unmark`. Other tools call
//! `is_indexing` via `load_project_index` and return `INDEXING_IN_PROGRESS`
//! so the LLM knows to wait rather than query a partial index.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use crate::sync_utils::{rw_read, rw_write};

static IN_PROGRESS: LazyLock<RwLock<HashSet<PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Mark `path` as currently being indexed.
pub fn mark(path: PathBuf) {
    rw_write(&IN_PROGRESS).insert(path);
}

/// Unmark `path` — indexing has finished (success or failure).
pub fn unmark(path: &Path) {
    rw_write(&IN_PROGRESS).remove(path);
}

/// Returns `true` if `path` is currently being indexed in the background.
pub fn is_indexing(path: &Path) -> bool {
    rw_read(&IN_PROGRESS).contains(path)
}
