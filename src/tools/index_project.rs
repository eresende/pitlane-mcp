use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::Peer;
use rmcp::RoleServer;
use serde_json::{json, Value};

use tracing::info;

use crate::embed::EmbedConfig;
use crate::error::ToolError;
use crate::index::format::{index_dir, load_meta, save_index, save_meta, IndexMeta};
use crate::index::SymbolIndex;
use crate::indexer::{load_gitignore_patterns, registry, Indexer};

/// Default cap on the number of eligible source files per walk.
/// Prevents accidental full-filesystem indexing (e.g. `index_project("/")`).
pub const DEFAULT_MAX_FILES: usize = 100_000;

pub struct IndexProjectParams {
    pub path: String,
    pub exclude: Option<Vec<String>>,
    pub force: Option<bool>,
    /// Maximum source files to index. Defaults to `DEFAULT_MAX_FILES`.
    /// Pass `usize::MAX` to disable.
    pub max_files: Option<usize>,
    /// If present, progress notifications are sent to this peer.
    pub progress_token: Option<ProgressToken>,
    pub peer: Option<Peer<RoleServer>>,
    /// Optional embedding configuration. When `Some`, embeddings are generated
    /// after indexing. Passed programmatically — not part of the MCP tool schema.
    pub embed_config: Option<Arc<EmbedConfig>>,
}

pub async fn index_project(mut params: IndexProjectParams) -> anyhow::Result<Value> {
    let path = Path::new(&params.path);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", params.path))?;

    info!(path = %canonical.display(), "index_project: start");

    let force = params.force.unwrap_or(false);
    let mut exclude = params.exclude.take().unwrap_or_default();
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
                    // Silently build the BM25 index if it is missing — this is the
                    // upgrade path for users who indexed before BM25 was added.
                    let tantivy_dir = idx_dir.join("tantivy");
                    if let Err(e) = crate::index::bm25::ensure(&index.symbols, &tantivy_dir) {
                        tracing::warn!(error = %e, "BM25 ensure failed; search will fall back to exact");
                    }
                    // Populate the in-memory cache so subsequent queries skip disk I/O.
                    let cached_index = crate::cache::insert(canonical.clone(), index);

                    // If embeddings are configured but the store is missing or empty,
                    // kick off background embedding so the cached index gets vectors
                    // without requiring a force re-index.
                    let store_path = idx_dir.join("embeddings.bin");
                    let embed_status = if let Some(cfg) = params.embed_config.clone() {
                        let needs_embed = !store_path.exists() || {
                            crate::embed::store::EmbedStore::load(&store_path)
                                .map(|s| s.vectors.is_empty())
                                .unwrap_or(true)
                        };
                        if needs_embed {
                            let index_for_embed = Arc::clone(&cached_index);
                            let cfg_clone = Arc::clone(&cfg);
                            tokio::spawn(async move {
                                let result = crate::embed::generate_embeddings(
                                    &index_for_embed,
                                    &cfg_clone,
                                    &store_path,
                                    false,
                                    None,
                                )
                                .await;
                                if let Some(err) = result.error {
                                    tracing::error!(error = %err, "embed: background embedding failed");
                                } else {
                                    tracing::info!(
                                        stored = result.stored,
                                        elapsed_ms = result.elapsed_ms,
                                        "embed: background embedding complete"
                                    );
                                }
                            });
                            "running"
                        } else {
                            "ok"
                        }
                    } else {
                        "disabled"
                    };

                    return Ok(json!({
                        "status": "cached",
                        "symbol_count": symbol_count,
                        "file_count": file_count,
                        "index_path": index_path.display().to_string(),
                        "embeddings": embed_status,
                    }));
                }
            }
        }
    }

    // Create index directory up-front so errors surface before we potentially
    // return "started" to the client.
    std::fs::create_dir_all(&idx_dir)?;

    // Always run synchronously so the caller can immediately use the index
    // after this call returns, regardless of whether a progress token or peer
    // is present.
    do_index_project(params, canonical, exclude, idx_dir, force).await
}

async fn do_index_project(
    params: IndexProjectParams,
    canonical: std::path::PathBuf,
    exclude: Vec<String>,
    idx_dir: std::path::PathBuf,
    force: bool,
) -> anyhow::Result<Value> {
    let start = Instant::now();

    let index_path = idx_dir.join("index.bin");
    let meta_path = idx_dir.join("meta.json");
    let max_files = params.max_files.unwrap_or(DEFAULT_MAX_FILES);
    let progress_token = params.progress_token.clone();
    let peer = params.peer.clone();

    let parsers = registry::build_default_registry();
    let indexer = Indexer::new(parsers);

    info!(path = %canonical.display(), "index_project: walking files");

    // Progress notifications bridge: the rayon callback sends (current, total)
    // into a channel; a concurrent async task drains it and calls notify_progress.
    // This avoids block_in_place (invalid inside tokio::spawn) while keeping
    // notifications live as parsing progresses.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<(usize, usize, String)>(256);

    // Spawn the drainer task before the blocking work starts.
    let drain_handle: Option<tokio::task::JoinHandle<()>> =
        if let (Some(token), Some(peer)) = (progress_token.clone(), peer.clone()) {
            Some(tokio::spawn(async move {
                while let Some((current, total, msg)) = progress_rx.recv().await {
                    tracing::debug!(
                        current,
                        total,
                        "index_project: sending progress notification"
                    );
                    let notif = ProgressNotificationParam::new(token.clone(), current as f64)
                        .with_total(total as f64)
                        .with_message(msg);
                    let _ = peer.notify_progress(notif).await;
                }
                tracing::debug!("index_project: progress drainer finished");
            }))
        } else {
            None
        };

    // Build the progress callback — sends into the channel, never blocks.
    let make_cb = {
        let has_token = progress_token.is_some() && drain_handle.is_some();
        let tx_for_cb = progress_tx.clone(); // clone for the callback; original dropped below
        move || -> Option<Box<dyn Fn(usize, usize) + Send + Sync>> {
            if has_token {
                let tx = tx_for_cb.clone();
                Some(Box::new(move |current, total| {
                    if current == 0 {
                        tracing::info!(total, "index_project: discovered files, starting parse");
                    }
                    let msg = if current == 0 {
                        format!("Indexing {total} files…")
                    } else if current == total {
                        format!("Parsed {total} files")
                    } else {
                        format!("Parsing files… {current}/{total}")
                    };
                    // Non-blocking send — drop the tick if the channel is full
                    // rather than stalling the rayon thread.
                    let result = tx.try_send((current, total, msg));
                    tracing::debug!(
                        current,
                        total,
                        sent = result.is_ok(),
                        "index_project: progress tick"
                    );
                }))
            } else {
                Some(Box::new(move |current, total| {
                    if current == 0 {
                        tracing::info!(total, "index_project: discovered files, starting parse");
                    }
                    let _ = (current, total);
                }))
            }
        }
    };

    let canonical_clone = canonical.clone();
    let (index, file_count) = tokio::task::spawn_blocking(move || {
        let cb = make_cb();
        indexer.index_project_with_progress(&canonical_clone, &exclude, max_files, cb.as_deref())
    })
    .await
    .context("Indexing task panicked")??;

    // Explicitly drop the original sender so the drainer sees EOF and exits.
    // The clone inside make_cb was moved into spawn_blocking and is already dropped.
    drop(progress_tx);

    // Wait for the drainer to flush all pending notifications before continuing.
    if let Some(handle) = drain_handle {
        let _ = handle.await;
    }

    info!(
        file_count,
        symbol_count = index.symbol_count(),
        "index_project: parsing complete"
    );

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

    let canonical_for_meta = canonical.clone();
    let mut meta = IndexMeta::new(&canonical_for_meta);
    meta.file_mtimes = file_mtimes;
    save_meta(&meta, &meta_path)?;

    // Build the BM25 index. Invalidate any stale cached reader first so the
    // next search opens the freshly written index.
    let tantivy_dir = idx_dir.join("tantivy");
    crate::index::bm25::invalidate(&canonical_for_meta);
    if let Err(e) = crate::index::bm25::build(&index.symbols, &tantivy_dir) {
        tracing::warn!(error = %e, "BM25 index build failed; search will fall back to exact");
    }

    let cached_index = crate::cache::insert(canonical_for_meta, index);

    // Embedding phase (opt-in) — runs in a background task so the MCP response
    // is returned immediately, avoiding client-side timeouts on large repos.
    let embed_status = if let Some(cfg) = params.embed_config.clone() {
        let store_path = idx_dir.join("embeddings.bin");
        let embed_peer = params.peer.clone();
        let embed_token = params.progress_token.clone();
        let index_for_embed = Arc::clone(&cached_index);
        let cfg_clone = Arc::clone(&cfg);

        tokio::spawn(async move {
            let progress_cb: Option<Box<dyn Fn(usize, usize) + Send + Sync>> =
                if let (Some(peer), Some(token)) = (embed_peer, embed_token) {
                    let (embed_tx, mut embed_rx) =
                        tokio::sync::mpsc::channel::<(usize, usize)>(256);
                    tokio::spawn(async move {
                        while let Some((completed, total)) = embed_rx.recv().await {
                            let notif =
                                ProgressNotificationParam::new(token.clone(), completed as f64)
                                    .with_total(total as f64)
                                    .with_message(format!("Embedding symbols… {completed}/{total}"));
                            let _ = peer.notify_progress(notif).await;
                        }
                    });
                    Some(Box::new(move |completed, total| {
                        let _ = embed_tx.try_send((completed, total));
                    }) as Box<dyn Fn(usize, usize) + Send + Sync>)
                } else {
                    None
                };

            let result = crate::embed::generate_embeddings(
                &index_for_embed,
                &cfg_clone,
                &store_path,
                force,
                progress_cb.as_deref(),
            )
            .await;

            if let Some(err) = result.error {
                tracing::error!(error = %err, "embed: background embedding failed");
            } else {
                tracing::info!(
                    stored = result.stored,
                    skipped = result.skipped,
                    elapsed_ms = result.elapsed_ms,
                    "embed: background embedding complete"
                );
            }
        });

        "running"
    } else {
        "disabled"
    };

    let elapsed = start.elapsed().as_millis() as u64;

    Ok(json!({
        "status": "indexed",
        "symbol_count": symbol_count,
        "file_count": file_count,
        "index_path": index_path.display().to_string(),
        "elapsed_ms": elapsed,
        "embeddings": embed_status,
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

    // Refuse to serve a partial index while background indexing is running.
    if crate::indexing::is_indexing(&canonical) {
        return Err(ToolError::IndexingInProgress {
            project: project.to_string(),
        }
        .into());
    }

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
    use crate::embed::EmbedConfig;
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

    #[tokio::test]
    async fn test_index_project_default_max_files_enforced() {
        // Verify that max_files: None uses DEFAULT_MAX_FILES (not usize::MAX).
        // We create DEFAULT_MAX_FILES + 1 stub files and expect FILE_LIMIT_EXCEEDED.
        let dir = TempDir::new().unwrap();
        for i in 0..=DEFAULT_MAX_FILES {
            std::fs::write(dir.path().join(format!("f{i}.rs")), b"fn f() {}").unwrap();
        }

        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: None,
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        };
        let err = index_project(params).await.unwrap_err();
        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");
        assert!(
            matches!(
                tool_err,
                crate::error::ToolError::FileLimitExceeded {
                    limit: DEFAULT_MAX_FILES,
                    ..
                }
            ),
            "expected FileLimitExceeded with limit={DEFAULT_MAX_FILES}, got: {tool_err:?}"
        );
    }

    // ── Task 6.3: embedding response field tests ──────────────────────────────

    /// Requirements 9.3: response includes `"embeddings": "disabled"` when embed_config is None.
    #[tokio::test]
    async fn test_index_project_embeddings_disabled() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();

        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        };

        let response = index_project(params).await.unwrap();
        assert_eq!(
            response["embeddings"],
            serde_json::json!("disabled"),
            "expected embeddings=disabled when embed_config is None"
        );
        // count/skipped/ms fields must be absent when disabled
        assert!(
            response.get("embeddings_count").is_none(),
            "embeddings_count should be absent when disabled"
        );
        assert!(
            response.get("embeddings_skipped").is_none(),
            "embeddings_skipped should be absent when disabled"
        );
    }

    /// Requirements 9.1: response includes `"embeddings": "ok"` and correct `embeddings_count`
    /// when all symbols are embedded successfully.
    #[tokio::test]
    async fn test_index_project_embeddings_ok() {
        use httpmock::prelude::*;

        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn foo() {}\npub fn bar() {}",
        )
        .unwrap();

        // Mock server returns a valid OpenAI-format response with BATCH_SIZE items.
        let embedding: Vec<f32> = vec![1.0_f32, 0.0_f32, 0.0_f32];
        let data_items: Vec<serde_json::Value> = (0..crate::embed::client::BATCH_SIZE)
            .map(|_| serde_json::json!({ "embedding": embedding }))
            .collect();
        let response_body = serde_json::json!({ "data": data_items }).to_string();

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let embed_config = Some(Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test".to_string(),
        }));

        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config,
        };

        let response = index_project(params).await.unwrap();
        assert_eq!(
            response["embeddings"],
            serde_json::json!("ok"),
            "expected embeddings=ok when all symbols embedded; response={response}"
        );
        let count = response["embeddings_count"]
            .as_u64()
            .expect("embeddings_count should be a number");
        assert!(count > 0, "embeddings_count should be > 0");
    }

    /// Requirements 9.2: response includes `"embeddings": "partial"` and `embeddings_skipped > 0`
    /// when the mock server returns HTTP 500 for all requests.
    #[tokio::test]
    async fn test_index_project_embeddings_partial() {
        use httpmock::prelude::*;

        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn foo() {}\npub fn bar() {}",
        )
        .unwrap();

        // Mock server always returns HTTP 500 → all symbols skipped.
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(500).body("internal server error");
        });

        let embed_config = Some(Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test".to_string(),
        }));

        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config,
        };

        let response = index_project(params).await.unwrap();
        assert_eq!(
            response["embeddings"],
            serde_json::json!("partial"),
            "expected embeddings=partial when server returns 500; response={response}"
        );
        let skipped = response["embeddings_skipped"]
            .as_u64()
            .expect("embeddings_skipped should be a number");
        assert!(skipped > 0, "embeddings_skipped should be > 0");
    }
}
