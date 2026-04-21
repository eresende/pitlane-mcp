//! Process-level embedding progress registry.
//!
//! The background embedding task writes `(stored, total)` here after every
//! batch. `wait_for_embeddings` reads from this registry instead of polling
//! the on-disk store, so it sees live progress rather than 0 until the job
//! finishes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sync_utils::{rw_read, rw_write};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EmbedProgress {
    pub stored: usize,
    pub total: usize,
    /// Unix timestamp (seconds) when embedding started.
    pub started_at: u64,
    /// Unix timestamp (seconds) when embedding finished, if complete.
    pub finished_at: Option<u64>,
}

static REGISTRY: LazyLock<RwLock<HashMap<PathBuf, EmbedProgress>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Record that embedding has started for a project.
pub fn start(project: &Path, total: usize) {
    rw_write(&REGISTRY).insert(
        project.to_path_buf(),
        EmbedProgress {
            stored: 0,
            total,
            started_at: now_secs(),
            finished_at: None,
        },
    );
}

/// Set the current progress for a project (keyed by canonical path).
/// Preserves `started_at` from the existing entry if present.
pub fn set(project: &Path, stored: usize, total: usize) {
    let mut reg = rw_write(&REGISTRY);
    let started_at = reg
        .get(project)
        .map(|p| p.started_at)
        .unwrap_or_else(now_secs);
    reg.insert(
        project.to_path_buf(),
        EmbedProgress {
            stored,
            total,
            started_at,
            finished_at: None,
        },
    );
}

/// Mark embedding as finished, recording the completion timestamp.
pub fn finish(project: &Path) {
    let mut reg = rw_write(&REGISTRY);
    if let Some(entry) = reg.get_mut(project) {
        entry.finished_at = Some(now_secs());
    }
}

/// Get the current progress for a project, or `None` if not tracked.
pub fn get(project: &Path) -> Option<EmbedProgress> {
    rw_read(&REGISTRY).get(project).copied()
}

/// Remove the entry once embedding is complete (optional cleanup).
pub fn remove(project: &Path) {
    rw_write(&REGISTRY).remove(project);
}
