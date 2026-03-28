use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use serde_json::{json, Value};

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

    // Check if we can use cached index
    if !force && index_path.exists() && meta_path.exists() {
        if let Ok(meta) = load_meta(&meta_path) {
            if is_index_up_to_date(&canonical, &meta) {
                // Load and return the existing index stats
                if let Ok(index) = crate::index::format::load_index(&index_path) {
                    let elapsed = start.elapsed().as_millis() as u64;
                    return Ok(json!({
                        "status": "cached",
                        "symbol_count": index.symbol_count(),
                        "file_count": index.file_count(),
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

    // Save index
    save_index(&index, &index_path)?;

    // Save meta
    let canonical_for_meta = Path::new(&params.path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&params.path).to_path_buf());
    let mut meta = IndexMeta::new(&canonical_for_meta);
    meta.file_mtimes = file_mtimes;
    save_meta(&meta, &meta_path)?;

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

/// Load an index from disk for a project path
pub fn load_project_index(project: &str) -> anyhow::Result<SymbolIndex> {
    let path = Path::new(project);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", project))?;
    let idx_dir = index_dir(&canonical)?;
    let index_path = idx_dir.join("index.bin");

    if !index_path.exists() {
        anyhow::bail!(
            "Project '{}' has not been indexed yet. Run index_project first.",
            project
        );
    }

    crate::index::format::load_index(&index_path)
}
