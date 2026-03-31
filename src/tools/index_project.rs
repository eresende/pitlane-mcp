use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::index::format::{index_dir, load_meta, save_index, save_meta, IndexMeta};
use crate::index::SymbolIndex;
use crate::indexer::{load_gitignore_patterns, registry, Indexer};

pub struct IndexProjectParams {
    pub path: String,
    pub exclude: Option<Vec<String>>,
    pub force: Option<bool>,
}

pub async fn index_project(params: IndexProjectParams) -> anyhow::Result<Value> {
    let start = Instant::now();

    let path = Path::new(&params.path);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", params.path))?;

    let force = params.force.unwrap_or(false);

    let mut exclude = params.exclude.unwrap_or_default();
    if exclude.is_empty() {
        exclude = default_excludes();
    }

    // Extend with patterns from the project's .gitignore (if present).
    exclude.extend(load_gitignore_patterns(&canonical));

    let idx_dir = index_dir(&canonical)?;
    let index_path = idx_dir.join("index.bin");
    let meta_path = idx_dir.join("meta.json");

    // Check if we can use the up-to-date on-disk index.
    if !force && index_path.exists() && meta_path.exists() {
        if let Ok(meta) = load_meta(&meta_path) {
            if is_index_up_to_date(&canonical, &meta) {
                if let Ok(index) = crate::index::format::load_index(&index_path) {
                    let symbol_count = index.symbol_count();
                    let file_count = index.file_count();
                    // Populate the in-memory cache so subsequent queries skip disk I/O.
                    crate::cache::insert(canonical.clone(), index);
                    let elapsed = start.elapsed().as_millis() as u64;
                    return Ok(json!({
                        "status": "cached",
                        "symbol_count": symbol_count,
                        "file_count": file_count,
                        "index_path": index_path.display().to_string(),
                        "elapsed_ms": elapsed,
                    }));
                }
            }
        }
    }

    // Create index directory
    std::fs::create_dir_all(&idx_dir)?;

    // Run the indexer
    let parsers = registry::build_default_registry();
    let indexer = Indexer::new(parsers);

    let (index, file_count) =
        tokio::task::spawn_blocking(move || indexer.index_project(&canonical, &exclude))
            .await
            .context("Indexing task panicked")??;

    let symbol_count = index.symbol_count();

    // Compute file mtimes
    let mut file_mtimes = HashMap::new();
    for file_path in index.by_file.keys() {
        if let Ok(meta) = std::fs::metadata(file_path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    file_mtimes.insert(file_path.display().to_string(), dur.as_secs());
                }
            }
        }
    }

    // Save index to disk, then populate the in-memory cache.
    save_index(&index, &index_path)?;

    // Save meta
    let canonical_for_meta = Path::new(&params.path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&params.path).to_path_buf());
    let mut meta = IndexMeta::new(&canonical_for_meta);
    meta.file_mtimes = file_mtimes;
    save_meta(&meta, &meta_path)?;

    crate::cache::insert(canonical_for_meta, index);

    let elapsed = start.elapsed().as_millis() as u64;

    Ok(json!({
        "status": "indexed",
        "symbol_count": symbol_count,
        "file_count": file_count,
        "index_path": index_path.display().to_string(),
        "elapsed_ms": elapsed,
    }))
}

fn default_excludes() -> Vec<String> {
    vec![
        "target/**".to_string(),
        ".git/**".to_string(),
        "__pycache__/**".to_string(),
        "node_modules/**".to_string(),
        ".venv/**".to_string(),
        "venv/**".to_string(),
        "*.pyc".to_string(),
    ]
}

fn is_index_up_to_date(project_path: &Path, meta: &IndexMeta) -> bool {
    // Simple check: compare stored mtimes with current mtimes
    for (path_str, stored_mtime) in &meta.file_mtimes {
        let path = Path::new(path_str);
        if !path.exists() {
            return false;
        }
        if let Ok(file_meta) = std::fs::metadata(path) {
            if let Ok(modified) = file_meta.modified() {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    if dur.as_secs() != *stored_mtime {
                        return false;
                    }
                }
            }
        }
    }

    // Also check if there are new files that aren't in the index
    // (simplified: just check the stored path matches)
    meta.project_path == project_path.display().to_string()
}

/// Load an index for a project path, returning a shared `Arc`.
///
/// Checks the in-memory cache first. On a miss, deserializes from disk,
/// populates the cache, and returns the new Arc. Subsequent calls for the
/// same project return the cached Arc immediately without any disk I/O.
pub fn load_project_index(project: &str) -> anyhow::Result<Arc<SymbolIndex>> {
    let path = Path::new(project);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", project))?;

    // Cache hit — no disk I/O needed.
    if let Some(cached) = crate::cache::get(&canonical) {
        return Ok(cached);
    }

    // Cache miss — load from disk and populate the cache.
    let idx_dir = index_dir(&canonical)?;
    let index_path = idx_dir.join("index.bin");

    if !index_path.exists() {
        return Err(ToolError::ProjectNotIndexed {
            project: project.to_string(),
        }
        .into());
    }

    let index = crate::index::format::load_index(&index_path)?;
    Ok(crate::cache::insert(canonical, index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;

    /// Helper: index a temp project to disk and return its path string.
    fn setup_project_on_disk(dir: &TempDir) -> String {
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[test]
    fn test_load_project_index_cache_miss_then_hit() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
        let project = setup_project_on_disk(&dir);
        let canonical = dir.path().canonicalize().unwrap();

        // Ensure no stale entry from a previous run.
        crate::cache::invalidate(&canonical);

        // First call: cache miss — loads from disk.
        let arc1 = load_project_index(&project).unwrap();
        assert!(arc1.symbol_count() > 0, "index should have symbols");

        // Delete the on-disk index. A second call must still succeed via cache.
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::remove_file(idx_dir.join("index.bin")).unwrap();

        // Second call: cache hit — no disk I/O.
        let arc2 = load_project_index(&project).unwrap();
        assert_eq!(arc1.symbol_count(), arc2.symbol_count());

        // Both calls return the same Arc allocation.
        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "cache hit must return the same Arc"
        );

        // Clean up.
        crate::cache::invalidate(&canonical);
    }

    #[test]
    fn test_load_project_index_not_indexed_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        // No index written — load must fail with PROJECT_NOT_INDEXED.
        let project = dir.path().to_string_lossy().to_string();
        let canonical = dir.path().canonicalize().unwrap();
        crate::cache::invalidate(&canonical);

        let err = load_project_index(&project).unwrap_err();
        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "PROJECT_NOT_INDEXED");
        assert!(tool_err.to_string().contains(project.as_str()));
        assert_eq!(tool_err.hint(), "Call index_project first.");
    }
}
