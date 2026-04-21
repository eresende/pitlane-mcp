use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rmcp::model::{
    LoggingLevel, LoggingMessageNotificationParam, ProgressNotificationParam, ProgressToken,
};
use rmcp::service::Peer;
use rmcp::RoleServer;
use serde_json::{json, Value};
use walkdir::WalkDir;

use tracing::info;

use crate::embed::EmbedConfig;
use crate::error::ToolError;
use crate::index::format::{
    build_index_meta, index_dir, load_meta, save_index, save_meta, IndexMeta,
};
use crate::index::SymbolIndex;
use crate::indexer::{
    is_declaration_file, is_excluded_dir_name, is_supported_extension, load_gitignore_patterns,
    registry, warn_walkdir_error, Indexer,
};
use crate::path_policy::resolve_project_path;

/// Default cap on the number of eligible source files per walk.
/// Prevents accidental full-filesystem indexing (e.g. `index_project("/")`).
pub const DEFAULT_MAX_FILES: usize = 100_000;

pub struct IndexProjectParams {
    pub path: String,
    pub exclude: Option<Vec<String>>,
    pub force: Option<bool>,
    /// Maximum source files to index. Defaults to `DEFAULT_MAX_FILES`.
    /// `Some(0)` is treated the same as omitting the field, matching the MCP schema.
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
    let canonical = resolve_project_path(&params.path)?;

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
            if is_index_up_to_date(&canonical, &meta, &exclude) {
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
                    let (embed_status, embed_started_at) = if let Some(cfg) =
                        params.embed_config.clone()
                    {
                        let needs_embed = !store_path.exists() || {
                            crate::embed::store::EmbedStore::load(&store_path)
                                .map(|s| s.vectors.is_empty())
                                .unwrap_or(true)
                        };
                        if needs_embed {
                            let started_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let index_for_embed = Arc::clone(&cached_index);
                            let canonical_for_embed = canonical.clone();
                            tokio::spawn(async move {
                                let result = crate::embed::generate_embeddings(
                                    &index_for_embed,
                                    &cfg,
                                    &store_path,
                                    false,
                                    None,
                                    Some(&canonical_for_embed),
                                )
                                .await;
                                if let Some(err) = result.error {
                                    tracing::error!(error = %err, "embed: background embedding failed (cached path)");
                                } else {
                                    tracing::info!(
                                        stored = result.stored,
                                        elapsed_ms = result.elapsed_ms,
                                        "embed: background embedding complete (cached path)"
                                    );
                                }
                            });
                            ("running", Some(started_at))
                        } else {
                            // Read timestamps from the progress registry if available.
                            let prog = crate::embed::progress::get(&canonical);
                            ("ok", prog.map(|p| p.started_at))
                        }
                    } else {
                        ("disabled", None)
                    };

                    let mut response = json!({
                        "status": "cached",
                        "symbol_count": symbol_count,
                        "file_count": file_count,
                        "index_path": index_path.display().to_string(),
                        "embeddings": embed_status,
                    });
                    if let Some(ts) = embed_started_at {
                        response["embeddings_started_at"] = json!(ts);
                    }
                    if let Some(prog) = crate::embed::progress::get(&canonical) {
                        if let Some(finished) = prog.finished_at {
                            response["embeddings_finished_at"] = json!(finished);
                        }
                    }
                    return Ok(response);
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
    let max_files = match params.max_files {
        Some(0) | None => DEFAULT_MAX_FILES,
        Some(limit) => limit,
    };
    let progress_token = params.progress_token.clone();
    let peer = params.peer.clone();

    let parsers = registry::build_default_registry();
    let indexer = Indexer::new(parsers);

    info!(path = %canonical.display(), "index_project: walking files");

    // Progress notifications bridge: the rayon callback sends (current, total)
    // into a channel; a concurrent async task drains it and sends notifications.
    // This avoids block_in_place (invalid inside tokio::spawn) while keeping
    // notifications live as parsing progresses.
    // Two notification channels are used:
    //   1. notifications/message (logging) — works for all clients
    //   2. notifications/progress — only when the client sent a progress_token
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<(usize, usize, String)>(256);

    // Spawn the drainer task before the blocking work starts.
    let drain_handle: Option<tokio::task::JoinHandle<()>> = if let Some(peer) = peer.clone() {
        let token = progress_token.clone();
        Some(tokio::spawn(async move {
            while let Some((current, total, msg)) = progress_rx.recv().await {
                tracing::debug!(
                    current,
                    total,
                    "index_project: sending progress notification"
                );
                // Always send a logging message.
                let log_notif = LoggingMessageNotificationParam::new(
                    LoggingLevel::Info,
                    serde_json::json!(msg),
                )
                .with_logger("pitlane-index".to_string());
                let _ = peer.notify_logging_message(log_notif).await;

                // Also send progress notification if the client provided a token.
                if let Some(ref token) = token {
                    let notif = ProgressNotificationParam::new(token.clone(), current as f64)
                        .with_total(total as f64)
                        .with_message(msg);
                    let _ = peer.notify_progress(notif).await;
                }
            }
            tracing::debug!("index_project: progress drainer finished");
        }))
    } else {
        None
    };

    // Build the progress callback — sends into the channel, never blocks.
    let make_cb = {
        let has_peer = drain_handle.is_some();
        let tx_for_cb = progress_tx.clone(); // clone for the callback; original dropped below
        move || -> Option<Box<dyn Fn(usize, usize) + Send + Sync>> {
            if has_peer {
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
    let mut meta = build_index_meta(&canonical_for_meta, &index);
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

    // Embedding phase (opt-in) — runs in a detached background task so the
    // MCP response returns as soon as the symbol index is ready. Semantic
    // search becomes available once the background task finishes.
    // Progress is reported via:
    //   1. notifications/progress (if the client sent a progress_token)
    //   2. notifications/message (logging) — works for all clients
    let (embed_status, embed_started_at) = if let Some(cfg) = params.embed_config.clone() {
        let store_path = idx_dir.join("embeddings.bin");
        let embed_peer = params.peer.clone();
        let embed_token = params.progress_token.clone();
        let index_for_embed = Arc::clone(&cached_index);

        // Record start timestamp before spawning so it's available in the response.
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Notify the agent immediately so it knows to wait before using semantic search.
        if let Some(ref peer) = params.peer {
            let kickoff = LoggingMessageNotificationParam::new(
                LoggingLevel::Info,
                serde_json::json!(format!(
                    "Embedding {symbol_count} symbols in background. \
                     Call get_index_stats and wait until embeddings_percent=100 \
                     before using mode=\"semantic\"."
                )),
            )
            .with_logger("pitlane-embed".to_string());
            let _ = peer.notify_logging_message(kickoff).await;
        }

        tokio::spawn(async move {
            let done_peer: Option<Peer<RoleServer>> = embed_peer.clone();
            let progress_cb: Option<Box<dyn Fn(usize, usize) + Send + Sync>> = if let Some(peer) =
                embed_peer
            {
                let (embed_tx, mut embed_rx) = tokio::sync::mpsc::channel::<(usize, usize)>(256);
                let token = embed_token;
                tokio::spawn(async move {
                    while let Some((completed, total)) = embed_rx.recv().await {
                        // Always send a logging message so all clients see progress.
                        let pct = if total > 0 {
                            (completed as f64 / total as f64 * 100.0).round() as u64
                        } else {
                            0
                        };
                        let msg = format!("Embedding symbols… {completed}/{total} ({pct}%)");
                        let log_notif = LoggingMessageNotificationParam::new(
                            LoggingLevel::Info,
                            serde_json::json!(msg),
                        )
                        .with_logger("pitlane-embed".to_string());
                        let _ = peer.notify_logging_message(log_notif).await;

                        // Also send progress notification if the client provided a token.
                        if let Some(ref token) = token {
                            let notif =
                                ProgressNotificationParam::new(token.clone(), completed as f64)
                                    .with_total(total as f64)
                                    .with_message(msg);
                            let _ = peer.notify_progress(notif).await;
                        }
                    }
                });
                Some(Box::new(move |completed, total| {
                    let _ = embed_tx.try_send((completed, total));
                })
                    as Box<dyn Fn(usize, usize) + Send + Sync>)
            } else {
                None
            };

            let result = crate::embed::generate_embeddings(
                &index_for_embed,
                &cfg,
                &store_path,
                force,
                progress_cb.as_deref(),
                Some(&canonical),
            )
            .await;

            if let Some(err) = result.error {
                tracing::error!(error = %err, "embed: background embedding failed");
                // Notify the agent that embedding failed.
                if let Some(peer) = done_peer {
                    let notif = LoggingMessageNotificationParam::new(
                        LoggingLevel::Warning,
                        serde_json::json!(format!(
                            "Embedding failed: {err}. Semantic search (mode=\"semantic\") is unavailable."
                        )),
                    )
                    .with_logger("pitlane-embed".to_string());
                    let _ = peer.notify_logging_message(notif).await;
                }
            } else {
                tracing::info!(
                    stored = result.stored,
                    skipped = result.skipped,
                    elapsed_ms = result.elapsed_ms,
                    "embed: background embedding complete"
                );
                // Notify the agent that semantic search is now ready.
                if let Some(peer) = done_peer {
                    let notif = LoggingMessageNotificationParam::new(
                        LoggingLevel::Info,
                        serde_json::json!(format!(
                            "Embedding complete: {} symbols stored in {}ms. \
                             Semantic search (mode=\"semantic\") is now ready.",
                            result.stored, result.elapsed_ms
                        )),
                    )
                    .with_logger("pitlane-embed".to_string());
                    let _ = peer.notify_logging_message(notif).await;
                }
            }
        });

        ("running", Some(started_at))
    } else {
        ("disabled", None)
    };

    let elapsed = start.elapsed().as_millis() as u64;

    let mut response = json!({
        "status": "indexed",
        "symbol_count": symbol_count,
        "file_count": file_count,
        "index_path": index_path.display().to_string(),
        "elapsed_ms": elapsed,
        "embeddings": embed_status,
    });
    if let Some(ts) = embed_started_at {
        response["embeddings_started_at"] = json!(ts);
    }
    Ok(response)
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

fn build_exclude_set(patterns: &[String]) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }
    Ok(builder.build()?)
}

fn current_file_mtimes(
    project_path: &Path,
    exclude_patterns: &[String],
) -> anyhow::Result<HashMap<String, u64>> {
    let exclude_set = build_exclude_set(exclude_patterns)?;
    let mut file_mtimes = HashMap::new();

    for entry in WalkDir::new(project_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let rel = match path.strip_prefix(project_path) {
                Ok(r) => r,
                Err(_) => return true,
            };
            if rel == Path::new("") {
                return true;
            }

            let rel_str = rel.to_string_lossy();
            if exclude_set.is_match(rel_str.as_ref()) {
                return false;
            }

            if e.file_type().is_dir() {
                if exclude_set.is_match(format!("{}/", rel_str).as_str()) {
                    return false;
                }
                if rel
                    .components()
                    .any(|c| c.as_os_str().to_str().is_some_and(is_excluded_dir_name))
                {
                    return false;
                }
            }
            true
        })
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn_walkdir_error(project_path, &err, "current_file_mtimes");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_supported_extension(ext) || is_declaration_file(path) {
            continue;
        }

        let rel = path.strip_prefix(project_path).unwrap_or(path);
        let rel_str = rel.to_string_lossy();
        if exclude_set.is_match(rel_str.as_ref()) || exclude_set.is_match(path) {
            continue;
        }

        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(_) => return Ok(HashMap::new()),
        };
        let modified = match meta.modified() {
            Ok(modified) => modified,
            Err(_) => return Ok(HashMap::new()),
        };
        let secs = match modified.duration_since(std::time::UNIX_EPOCH) {
            Ok(dur) => dur.as_secs(),
            Err(_) => return Ok(HashMap::new()),
        };

        file_mtimes.insert(path.display().to_string(), secs);
    }

    Ok(file_mtimes)
}

fn is_index_up_to_date(project_path: &Path, meta: &IndexMeta, exclude_patterns: &[String]) -> bool {
    if meta.project_path != project_path.display().to_string() {
        return false;
    }
    if meta.version != 3 {
        return false;
    }

    match current_file_mtimes(project_path, exclude_patterns) {
        Ok(current) => current == meta.file_mtimes,
        Err(_) => false,
    }
}

/// Load an index for a project path, returning a shared `Arc`.
///
/// Checks the in-memory cache first. On a miss, deserializes from disk,
/// populates the cache, and returns the new Arc. Subsequent calls for the
/// same project return the cached Arc immediately without any disk I/O.
pub fn load_project_index(project: &str) -> anyhow::Result<Arc<SymbolIndex>> {
    let canonical = resolve_project_path(project)?;

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
    use crate::path_policy::set_test_allowed_roots;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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

    #[cfg(unix)]
    struct RestrictedDir {
        path: std::path::PathBuf,
        original_mode: u32,
    }

    #[cfg(unix)]
    impl RestrictedDir {
        fn new(root: &TempDir, name: &str) -> Self {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("hidden.rs"), b"fn hidden() {}\n").unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let original_mode = metadata.permissions().mode();
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o0);
            std::fs::set_permissions(&path, permissions).unwrap();
            Self {
                path,
                original_mode,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for RestrictedDir {
        fn drop(&mut self) {
            if let Ok(metadata) = std::fs::metadata(&self.path) {
                let mut permissions = metadata.permissions();
                permissions.set_mode(self.original_mode);
                let _ = std::fs::set_permissions(&self.path, permissions);
            }
        }
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

    #[cfg(unix)]
    #[test]
    fn test_current_file_mtimes_skips_walkdir_permission_denied_entries() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn visible() {}\n").unwrap();
        let _restricted = RestrictedDir::new(&dir, "blocked");

        let mtimes = current_file_mtimes(dir.path(), &[]).unwrap();

        assert!(mtimes.keys().any(|path| path.ends_with("lib.rs")));
        assert_eq!(mtimes.len(), 1);
    }

    #[test]
    fn test_load_project_index_rejects_project_outside_allowed_roots() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        set_test_allowed_roots(Some(allowed.path().as_os_str().to_os_string()));

        let err = load_project_index(outside.path().to_string_lossy().as_ref()).unwrap_err();
        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");
        assert!(matches!(
            tool_err,
            crate::error::ToolError::AccessDenied { .. }
        ));

        set_test_allowed_roots(None);
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

    #[tokio::test]
    async fn test_index_project_zero_max_files_uses_default_limit() {
        let dir = TempDir::new().unwrap();
        for i in 0..=DEFAULT_MAX_FILES {
            std::fs::write(dir.path().join(format!("f{i}.rs")), b"fn f() {}").unwrap();
        }

        let params = IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: None,
            max_files: Some(0),
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

    #[test]
    fn test_index_project_rejects_project_outside_allowed_roots() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        set_test_allowed_roots(Some(allowed.path().as_os_str().to_os_string()));

        let err = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(index_project(IndexProjectParams {
                path: outside.path().to_string_lossy().to_string(),
                exclude: None,
                force: None,
                max_files: None,
                progress_token: None,
                peer: None,
                embed_config: None,
            }))
            .unwrap_err();

        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");
        assert!(matches!(
            tool_err,
            crate::error::ToolError::AccessDenied { .. }
        ));

        set_test_allowed_roots(None);
    }

    #[tokio::test]
    async fn test_index_project_reindexes_when_new_source_file_is_added() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();

        let initial = index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();
        assert_eq!(initial["status"], json!("indexed"));
        assert_eq!(initial["file_count"], json!(1));

        std::fs::write(dir.path().join("extra.rs"), b"pub fn bar() {}").unwrap();

        let refreshed = index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(false),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();

        assert_eq!(
            refreshed["status"],
            json!("indexed"),
            "adding a new supported file must invalidate the cached index"
        );
        assert_eq!(refreshed["file_count"], json!(2));
    }

    #[tokio::test]
    async fn test_index_project_ignores_new_file_inside_default_excluded_dir() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();

        let initial = index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();
        assert_eq!(initial["status"], json!("indexed"));

        let target_dir = dir.path().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("generated.rs"), b"pub fn generated() {}").unwrap();

        let cached = index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(false),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();

        assert_eq!(
            cached["status"],
            json!("cached"),
            "adding files only under default excluded directories should not invalidate the index"
        );
        assert_eq!(cached["file_count"], json!(1));
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

    /// Requirements 9.1: response includes `"embeddings": "running"` when embed_config is
    /// provided. The background task completes and writes the store to disk.
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
            serde_json::json!("running"),
            "expected embeddings=running (background); response={response}"
        );

        // Let the background task finish, then verify the store on disk.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = crate::index::format::index_dir(&canonical).unwrap();
        let store = crate::embed::store::EmbedStore::load(&idx_dir.join("embeddings.bin")).unwrap();
        assert!(
            !store.vectors.is_empty(),
            "background task should have written embeddings"
        );
    }

    /// Requirements 9.2: when the mock server returns HTTP 500, the background task
    /// still runs (response says "running") but the store ends up empty.
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
            serde_json::json!("running"),
            "expected embeddings=running (background); response={response}"
        );

        // Let the background task finish, then verify the store is empty (all skipped).
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = crate::index::format::index_dir(&canonical).unwrap();
        let store_path = idx_dir.join("embeddings.bin");
        let store = crate::embed::store::EmbedStore::load(&store_path).unwrap();
        assert!(
            store.vectors.is_empty(),
            "all symbols should have been skipped (server 500)"
        );
    }
}
