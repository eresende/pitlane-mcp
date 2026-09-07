use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

use crate::embed::EmbedConfig;
use crate::index::format::{index_dir, load_meta, save_index, save_meta, IndexMeta};
use crate::index::SymbolIndex;
use crate::indexer::is_supported_extension;
use crate::indexer::{
    default_exclude_patterns, load_gitignore_patterns, path_is_excluded, registry, Indexer,
};
use crate::tools::index_project::current_source_snapshot;
use crate::tools::watch_project::{remove_watch_status, watch_status_path, write_watch_status};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);
const CHANNEL_CAPACITY: usize = 1024;

/// Effective exclusion patterns for the project: the patterns persisted by
/// the last indexing run, falling back to defaults plus `.gitignore` for
/// indexes created before exclusions were persisted.
fn effective_exclude_patterns(root: &Path, meta_path: &Path) -> Vec<String> {
    if let Ok(meta) = load_meta(meta_path) {
        if !meta.effective_excludes.is_empty() {
            return meta.effective_excludes;
        }
    }
    let mut patterns = default_exclude_patterns();
    patterns.extend(load_gitignore_patterns(root));
    patterns
}

pub struct ProjectWatcher {
    _watcher: RecommendedWatcher,
    /// Resolves when the debounce loop exits (stop flag, channel close, or
    /// error) so callers can await a clean shutdown.
    done: tokio::sync::watch::Receiver<bool>,
    /// Where the cross-process status file lives (removed on Drop).
    status_path: PathBuf,
}

impl Drop for ProjectWatcher {
    fn drop(&mut self) {
        // Best-effort cleanup of the cross-process status file; the debounce
        // loop also removes it on a stop-flag shutdown.
        remove_watch_status(&self.status_path);
    }
}

impl ProjectWatcher {
    pub fn done(&self) -> tokio::sync::watch::Receiver<bool> {
        self.done.clone()
    }

    pub fn start(
        project_path: PathBuf,
        index: Arc<RwLock<SymbolIndex>>,
        index_path: PathBuf,
        meta_path: PathBuf,
        embed_config: Option<Arc<EmbedConfig>>,
    ) -> anyhow::Result<Self> {
        let project_path_clone = project_path.clone();

        let parsers = registry::build_default_registry();
        let indexer = Arc::new(Indexer::new(parsers));

        let (tx, rx) = mpsc::channel::<PathBuf>(CHANNEL_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));

        // Cross-process lifecycle (issue #99): publish a status file under the
        // index directory and honor a stop-flag written by any other
        // `pitlane watch --stop` / `watch_project` invocation.
        let idx_dir = meta_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| project_path.clone());
        let status_path = watch_status_path(&idx_dir);
        write_watch_status(&status_path)?;
        let stop_flag_path = idx_dir.join("watch.stop");
        let _ = std::fs::remove_file(&stop_flag_path); // stale flag from a previous stop

        let (done_tx, done_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(run_debounce_loop(
            rx,
            project_path_clone,
            indexer,
            index,
            DEBOUNCE_WINDOW,
            index_path,
            meta_path,
            embed_config,
            Arc::clone(&overflowed),
            stop_flag_path,
            status_path.clone(),
            done_tx,
        ));

        let handler = move |result: notify::Result<Event>| {
            if let Ok(event) = result {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        for path in event.paths {
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            if is_supported_extension(ext) {
                                // Non-blocking send; drop the event if the channel is full rather
                                // than blocking the notify callback thread. If we do drop one,
                                // mark the project dirty so the debounce loop does a safe full
                                // resync instead of silently diverging.
                                if tx.try_send(path).is_err() {
                                    overflowed.store(true, Ordering::Release);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        };

        let mut watcher = recommended_watcher(handler)?;
        watcher.watch(&project_path, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: watcher,
            done: done_rx,
            status_path,
        })
    }
}

/// Collects file paths from `rx` into a `HashSet` (deduplication) and flushes
/// the batch through a single write-lock acquisition once the debounce window
/// expires without a new event, or when the channel is closed.
#[allow(clippy::too_many_arguments)]
async fn run_debounce_loop(
    mut rx: mpsc::Receiver<PathBuf>,
    root: PathBuf,
    indexer: Arc<Indexer>,
    index: Arc<RwLock<SymbolIndex>>,
    debounce_window: Duration,
    index_path: PathBuf,
    meta_path: PathBuf,
    embed_config: Option<Arc<EmbedConfig>>,
    overflowed: Arc<AtomicBool>,
    stop_flag_path: PathBuf,
    status_path: PathBuf,
    done_tx: tokio::sync::watch::Sender<bool>,
) {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;
    let mut stop_poll = tokio::time::interval(Duration::from_millis(500));
    stop_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let timeout = deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::MAX);

        tokio::select! {
            maybe_path = rx.recv() => {
                match maybe_path {
                    Some(path) => {
                        pending.insert(path);
                        deadline = Some(Instant::now() + debounce_window);
                    }
                    // Channel closed — flush whatever is pending and exit.
                    None => {
                        flush_pending(
                            &mut pending,
                            &root,
                            &indexer,
                            &index,
                            &index_path,
                            &meta_path,
                            embed_config.as_ref().map(Arc::clone),
                            &overflowed,
                        )
                        .await;
                        remove_watch_status(&status_path);
                        let _ = done_tx.send(true);
                        return;
                    }
                }
            }

            // Debounce window expired — flush the batch.
            _ = tokio::time::sleep(timeout), if deadline.is_some() => {
                flush_pending(
                    &mut pending,
                    &root,
                    &indexer,
                    &index,
                    &index_path,
                    &meta_path,
                    embed_config.as_ref().map(Arc::clone),
                    &overflowed,
                )
                .await;
                deadline = None;
            }

            // Cross-process stop request (issue #99).
            _ = stop_poll.tick() => {
                if stop_flag_path.exists() {
                    let _ = std::fs::remove_file(&stop_flag_path);
                    flush_pending(
                        &mut pending,
                        &root,
                        &indexer,
                        &index,
                        &index_path,
                        &meta_path,
                        embed_config.as_ref().map(Arc::clone),
                        &overflowed,
                    )
                    .await;
                    remove_watch_status(&status_path);
                    let _ = done_tx.send(true);
                    return;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn flush_pending(
    pending: &mut HashSet<PathBuf>,
    root: &Path,
    indexer: &Arc<Indexer>,
    index: &Arc<RwLock<SymbolIndex>>,
    index_path: &Path,
    meta_path: &Path,
    embed_config: Option<Arc<EmbedConfig>>,
    overflowed: &Arc<AtomicBool>,
) {
    if overflowed.swap(false, Ordering::AcqRel) {
        full_resync(
            root,
            indexer,
            index,
            index_path,
            meta_path,
            embed_config,
            effective_exclude_patterns(root, meta_path),
        )
        .await;
        pending.clear();
        return;
    }

    if pending.is_empty() {
        return;
    }

    reindex_batch(
        pending,
        root,
        indexer,
        index,
        index_path,
        meta_path,
        embed_config,
    )
    .await;
    pending.clear();
}

async fn full_resync(
    root: &Path,
    indexer: &Arc<Indexer>,
    index: &Arc<RwLock<SymbolIndex>>,
    index_path: &Path,
    meta_path: &Path,
    embed_config: Option<Arc<EmbedConfig>>,
    exclude_patterns: Vec<String>,
) {
    let old_removed_ids = {
        let idx = index.read().await;
        idx.symbols.keys().cloned().collect::<Vec<_>>()
    };

    let root_buf = root.to_path_buf();
    let indexer = Arc::clone(indexer);
    // Reuse the effective exclusions from initial indexing (issue #74): an
    // empty pattern list would silently re-index excluded directories.
    let rebuilt =
        tokio::task::spawn_blocking(move || indexer.index_project(&root_buf, &exclude_patterns))
            .await;

    let (rebuilt_index, source_file_count) = match rebuilt {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            eprintln!("pitlane-mcp: full watcher resync failed: {}", err);
            return;
        }
        Err(err) => {
            eprintln!("pitlane-mcp: full watcher resync task panicked: {}", err);
            return;
        }
    };

    let changed_files: HashSet<PathBuf> = rebuilt_index.by_file.keys().cloned().collect();

    {
        let mut idx = index.write().await;
        *idx = rebuilt_index;

        if let Err(e) = save_index(&idx, index_path) {
            eprintln!("pitlane-mcp: failed to flush index to disk: {}", e);
        }

        crate::cache::invalidate(root);

        if let Ok(tantivy_dir) = index_dir(root).map(|d| d.join("tantivy")) {
            let _ = crate::index::bm25::mark_stale(&tantivy_dir);
        }
        crate::index::bm25::invalidate(root);

        // Record the resync as a new revision with a symbol-level diff
        // against the pre-resync symbol set (issue #82).
        let old_ids: HashSet<&str> = old_removed_ids.iter().map(|s| s.as_str()).collect();
        let mut added = Vec::new();
        let mut modified = Vec::new();
        for id in idx.symbols.keys() {
            if old_ids.contains(id.as_str()) {
                modified.push(id.clone());
            } else {
                added.push(id.clone());
            }
        }
        let removed: Vec<String> = old_removed_ids
            .iter()
            .filter(|id| !idx.symbols.contains_key(id.as_str()))
            .cloned()
            .collect();
        let mut meta = load_meta(meta_path).unwrap_or_else(|_| IndexMeta::new(root));
        meta.record_change(added, removed, modified);
        idx.revision = meta.revision;
        drop(idx);

        if let Err(e) = save_meta(&meta, meta_path) {
            eprintln!("pitlane-mcp: failed to flush meta to disk: {}", e);
        }
    }

    if let Some(cfg) = &embed_config {
        let store_path = match index_dir(root) {
            Ok(d) => d.join("embeddings.bin"),
            Err(e) => {
                tracing::warn!("embed: cannot resolve store path: {e}");
                return;
            }
        };
        crate::embed::update_embeddings_for_files(
            &*index.read().await,
            &changed_files,
            &old_removed_ids,
            cfg,
            &store_path,
        )
        .await;
    }

    let mut meta = IndexMeta::new(root);
    if let Ok(snapshot) = current_source_snapshot(root, &[]) {
        meta.file_mtimes = snapshot.file_mtimes;
        meta.dir_mtimes = snapshot.dir_mtimes;
    }
    meta.source_file_count = source_file_count;
    if let Err(e) = save_meta(&meta, meta_path) {
        eprintln!("pitlane-mcp: failed to flush meta to disk: {}", e);
    }
}

async fn reindex_batch(
    paths: &HashSet<PathBuf>,
    root: &Path,
    indexer: &Arc<Indexer>,
    index: &Arc<RwLock<SymbolIndex>>,
    index_path: &Path,
    meta_path: &Path,
    embed_config: Option<Arc<EmbedConfig>>,
) {
    let removed_ids: Vec<crate::indexer::language::SymbolId>;
    {
        let mut idx = index.write().await;
        // Capture symbol IDs for changed files BEFORE reindex mutates by_file
        removed_ids = paths
            .iter()
            .flat_map(|p| idx.by_file.get(p).cloned().unwrap_or_default())
            .collect();

        // Apply the same exclusion policy as initial indexing (issue #74):
        // edits or new files inside excluded directories must not re-enter
        // the index, and previously indexed files that are now excluded are
        // dropped.
        let exclude_patterns = effective_exclude_patterns(root, meta_path);
        let exclude_set = Indexer::build_exclude_set(&exclude_patterns).unwrap_or_else(|_| {
            tracing::warn!("watcher: invalid persisted exclude patterns; applying none");
            globset::GlobSetBuilder::new().build().unwrap()
        });
        let extra_excluded_dirs = crate::indexer::extra_excluded_dir_names();
        let mut accepted: HashSet<PathBuf> = HashSet::new();
        for path in paths {
            if path_is_excluded(path, root, &exclude_set, &extra_excluded_dirs) {
                idx.remove_file(path);
                continue;
            }
            accepted.insert(path.clone());
        }

        // One graph rebuild for the whole batch, not one per changed file.
        indexer.reindex_files(&accepted, root, &mut idx);

        // Flush updated index to disk while holding the write lock for consistency.
        if let Err(e) = save_index(&idx, index_path) {
            eprintln!("pitlane-mcp: failed to flush index to disk: {}", e);
        }

        // Invalidate the in-memory cache so the next query reloads the fresh
        // snapshot from disk rather than serving stale data.
        crate::cache::invalidate(root);

        // Mark the BM25 index stale so the next BM25 search rebuilds it from
        // the updated symbol set. We remove the ready sentinel rather than rebuilding here
        // to avoid holding the write lock during tantivy I/O.
        if let Ok(tantivy_dir) = index_dir(root).map(|d| d.join("tantivy")) {
            let _ = crate::index::bm25::mark_stale(&tantivy_dir);
        }
        crate::index::bm25::invalidate(root);

        // Record the flush as a new revision (issue #82): added = symbols in
        // the affected files that were not there before, removed = prior
        // symbols no longer present, modified = surviving symbols in those
        // files (IDs are file/name/kind based, so a content edit usually
        // keeps the ID).
        let removed_set: HashSet<&str> = removed_ids.iter().map(|s| s.as_str()).collect();
        let mut added = Vec::new();
        let mut modified = Vec::new();
        for path in &accepted {
            if let Some(ids) = idx.by_file.get(path) {
                for id in ids {
                    if removed_set.contains(id.as_str()) {
                        modified.push(id.clone());
                    } else {
                        added.push(id.clone());
                    }
                }
            }
        }
        let removed: Vec<String> = removed_ids
            .iter()
            .filter(|id| !idx.symbols.contains_key(id.as_str()))
            .cloned()
            .collect();

        let mut meta = load_meta(meta_path).unwrap_or_else(|_| IndexMeta::new(root));
        meta.record_change(added, removed, modified);
        idx.revision = meta.revision;
        if let Err(e) = save_meta(&meta, meta_path) {
            eprintln!("pitlane-mcp: failed to flush meta to disk: {}", e);
        }
    }
    // write lock released — safe to re-acquire index.read() below

    if let Some(cfg) = &embed_config {
        let store_path = match index_dir(root) {
            Ok(d) => d.join("embeddings.bin"),
            Err(e) => {
                tracing::warn!("embed: cannot resolve store path: {e}");
                return;
            }
        };
        crate::embed::update_embeddings_for_files(
            &*index.read().await,
            paths,
            &removed_ids,
            cfg,
            &store_path,
        )
        .await;
    }

    // Update file_mtimes in meta for the changed paths so is_index_up_to_date
    // returns true on the next server start without a forced re-index.
    let mut meta = load_meta(meta_path).unwrap_or_else(|_| IndexMeta::new(root));
    for path in paths {
        let key = path.display().to_string();
        match std::fs::metadata(path) {
            Ok(fs_meta) => {
                if let Ok(modified) = fs_meta.modified() {
                    if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                        meta.file_mtimes.insert(key, dur.as_nanos() as u64);
                    }
                }
            }
            Err(_) => {
                // File was deleted — remove its mtime so it isn't treated as fresh.
                meta.file_mtimes.remove(&key);
            }
        }
        if let Some(parent) = path.parent() {
            if let Ok(fs_meta) = std::fs::metadata(parent) {
                if let Ok(modified) = fs_meta.modified() {
                    if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                        meta.dir_mtimes
                            .insert(parent.display().to_string(), dur.as_nanos() as u64);
                    }
                }
            }
        }
    }
    meta.source_file_count = meta.file_mtimes.len();
    if let Err(e) = save_meta(&meta, meta_path) {
        eprintln!("pitlane-mcp: failed to flush meta to disk: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::registry;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    const TEST_DEBOUNCE: Duration = Duration::from_millis(50);

    fn setup(dir: &TempDir) -> (Arc<RwLock<SymbolIndex>>, Arc<Indexer>) {
        let indexer = Arc::new(Indexer::new(registry::build_default_registry()));
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        (Arc::new(RwLock::new(index)), indexer)
    }

    fn spawn_loop(
        rx: mpsc::Receiver<PathBuf>,
        dir: &TempDir,
        indexer: Arc<Indexer>,
        index: Arc<RwLock<SymbolIndex>>,
    ) -> tokio::task::JoinHandle<()> {
        spawn_loop_with_overflow(rx, dir, indexer, index, Arc::new(AtomicBool::new(false)))
    }

    fn spawn_loop_with_overflow(
        rx: mpsc::Receiver<PathBuf>,
        dir: &TempDir,
        indexer: Arc<Indexer>,
        index: Arc<RwLock<SymbolIndex>>,
        overflowed: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        let root = dir.path().to_path_buf();
        let index_path = dir.path().join("index.bin");
        let meta_path = dir.path().join("meta.json");
        let (done_tx, _done_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(run_debounce_loop(
            rx,
            root,
            indexer,
            index,
            TEST_DEBOUNCE,
            index_path,
            meta_path,
            None,
            overflowed,
            dir.path().join("watch.stop"),
            dir.path().join("watch.json"),
            done_tx,
        ))
    }

    /// A single modified file is reindexed after the debounce window expires.
    #[tokio::test]
    async fn test_single_path_reindexed_after_window() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn original() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&file, b"fn updated() {}").unwrap();
        tx.send(file).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let idx = index.read().await;
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"updated"), "expected 'updated' in index");
        assert!(!names.contains(&"original"), "expected 'original' removed");
    }

    #[tokio::test]
    async fn test_reindex_marks_bm25_stale() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn original() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let tantivy_dir = index_dir(dir.path()).unwrap().join("tantivy");
        {
            let idx = index.read().await;
            crate::index::bm25::build(&idx.symbols, &tantivy_dir).unwrap();
        }

        let original_hits =
            crate::index::bm25::search("original", dir.path(), &tantivy_dir, None, None, 10)
                .unwrap();
        assert!(
            original_hits.iter().any(|id| id.contains("original")),
            "precondition: BM25 should find the original symbol"
        );

        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&file, b"fn updated() {}").unwrap();
        tx.send(file).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        {
            let idx = index.read().await;
            crate::index::bm25::ensure(&idx.symbols, &tantivy_dir).unwrap();
        }

        let updated_hits =
            crate::index::bm25::search("updated", dir.path(), &tantivy_dir, None, None, 10)
                .unwrap();
        assert!(
            updated_hits.iter().any(|id| id.contains("updated")),
            "BM25 should rebuild after watcher reindex and find the updated symbol"
        );
    }

    /// Multiple distinct paths sent in a burst are all batched into one flush.
    #[tokio::test]
    async fn test_burst_of_paths_all_reindexed() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let c = dir.path().join("c.rs");
        std::fs::write(&a, b"fn a_old() {}").unwrap();
        std::fs::write(&b, b"fn b_old() {}").unwrap();
        std::fs::write(&c, b"fn c_old() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&a, b"fn a_new() {}").unwrap();
        std::fs::write(&b, b"fn b_new() {}").unwrap();
        std::fs::write(&c, b"fn c_new() {}").unwrap();

        // Send all three paths without any delay between them.
        tx.send(a).await.unwrap();
        tx.send(b).await.unwrap();
        tx.send(c).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let idx = index.read().await;
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a_new"));
        assert!(names.contains(&"b_new"));
        assert!(names.contains(&"c_new"));
        assert!(!names.contains(&"a_old"));
        assert!(!names.contains(&"b_old"));
        assert!(!names.contains(&"c_old"));
    }

    /// Sending the same path multiple times within one window reindexes it only once
    /// (deduplication via HashSet), so the symbol count stays correct.
    #[tokio::test]
    async fn test_duplicate_paths_deduplicated() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn foo() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&file, b"fn bar() {}").unwrap();

        for _ in 0..10 {
            tx.send(file.clone()).await.unwrap();
        }
        drop(tx);
        handle.await.unwrap();

        let idx = index.read().await;
        // Exactly one symbol should be present despite 10 events for the same file.
        assert_eq!(idx.symbol_count(), 1);
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"bar"));
    }

    /// Closing the channel (sender dropped) flushes pending paths immediately
    /// without waiting for the debounce window to expire.
    #[tokio::test]
    async fn test_channel_close_flushes_pending() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn before() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&file, b"fn after() {}").unwrap();
        tx.send(file).await.unwrap();

        // Drop the sender immediately — loop should flush without sleeping the full window.
        let start = std::time::Instant::now();
        drop(tx);
        handle.await.unwrap();
        let elapsed = start.elapsed();

        // Should complete without waiting for a full debounce window. The
        // bound is generous: shared CI runners cannot guarantee hard
        // real-time scheduling (observed ~75ms on Windows for a 50ms window).
        assert!(
            elapsed < TEST_DEBOUNCE * 4,
            "flush took {elapsed:?}, expected well under the debounce window {TEST_DEBOUNCE:?}"
        );

        let idx = index.read().await;
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"after"));
        assert!(!names.contains(&"before"));
    }

    /// Events for files with unsupported extensions (.txt, .json) are not sent
    /// through the channel, so the index must not change.
    #[tokio::test]
    async fn test_unsupported_extensions_ignored() {
        let dir = TempDir::new().unwrap();
        let rs_file = dir.path().join("lib.rs");
        std::fs::write(&rs_file, b"fn keep() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        // Only send paths that the handler would filter out — the debounce loop
        // itself does not filter, but we verify the contract expected by the handler.
        let txt = dir.path().join("notes.txt");
        let json = dir.path().join("config.json");
        std::fs::write(&txt, b"hello").unwrap();
        std::fs::write(&json, b"{}").unwrap();

        // Simulate the handler's extension filter: neither path should be sent.
        for path in [&txt, &json] {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "rs" || ext == "py" {
                tx.send(path.clone()).await.unwrap();
            }
        }
        drop(tx);
        handle.await.unwrap();

        // Index must be unchanged — still only the original `keep` symbol.
        let idx = index.read().await;
        assert_eq!(idx.symbol_count(), 1);
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"keep"));
    }

    /// If the notify queue overflows, the debounce loop falls back to a full
    /// resync so dropped paths do not leave the index stale.
    #[tokio::test]
    async fn test_overflow_triggers_full_resync() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, b"fn a_old() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let overflowed = Arc::new(AtomicBool::new(false));
        let handle = spawn_loop_with_overflow(rx, &dir, indexer, index.clone(), overflowed.clone());

        std::fs::write(&a, b"fn a_new() {}").unwrap();
        std::fs::write(&b, b"fn b_new() {}").unwrap();

        tx.send(a).await.unwrap();
        overflowed.store(true, Ordering::Release);

        drop(tx);
        handle.await.unwrap();

        let idx = index.read().await;
        let names: Vec<_> = idx.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a_new"));
        assert!(names.contains(&"b_new"));
        assert!(!names.contains(&"a_old"));
    }

    // ── Issue #74: watcher applies the same exclusion policy as initial indexing ──

    fn write_meta_with_excludes(dir: &TempDir, excludes: &[&str]) {
        let mut meta = IndexMeta::new(dir.path());
        meta.effective_excludes = excludes.iter().map(|s| s.to_string()).collect();
        save_meta(&meta, &dir.path().join("meta.json")).unwrap();
    }

    async fn symbol_names(index: &Arc<RwLock<SymbolIndex>>) -> Vec<String> {
        index
            .read()
            .await
            .symbols
            .values()
            .map(|s| s.name.clone())
            .collect()
    }

    #[tokio::test]
    async fn test_watcher_ignores_edits_inside_excluded_directory() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("ignored")).unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"fn kept() {}").unwrap();
        std::fs::write(dir.path().join("ignored/secret.rs"), b"fn secret_old() {}").unwrap();

        let indexer = Arc::new(Indexer::new(registry::build_default_registry()));
        let (idx, _) = indexer
            .index_project(dir.path(), &["ignored/**".to_string()])
            .unwrap();
        assert!(!idx.symbols.values().any(|s| s.name == "secret_old"));
        let index = Arc::new(RwLock::new(idx));
        write_meta_with_excludes(&dir, &["ignored/**"]);

        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        // Edit inside the excluded directory: must not re-enter the index.
        std::fs::write(dir.path().join("ignored/secret.rs"), b"fn secret_new() {}").unwrap();
        // New file inside the excluded directory: must be ignored too.
        let added = dir.path().join("ignored/added.rs");
        std::fs::write(&added, b"fn added_fn() {}").unwrap();
        tx.send(dir.path().join("ignored/secret.rs")).await.unwrap();
        tx.send(added).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let names = symbol_names(&index).await;
        assert!(names.iter().any(|n| n == "kept"));
        assert!(!names.iter().any(|n| n == "secret_new"), "names={names:?}");
        assert!(!names.iter().any(|n| n == "added_fn"), "names={names:?}");
    }

    #[tokio::test]
    async fn test_watcher_drops_previously_indexed_file_now_excluded() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("legacy.rs");
        std::fs::write(&file, b"fn legacy_old() {}").unwrap();

        // Indexed WITHOUT exclusions, so the symbol is in the index...
        let (index, indexer) = setup(&dir);
        assert!(symbol_names(&index).await.iter().any(|n| n == "legacy_old"));

        // ...but the effective policy now excludes it.
        write_meta_with_excludes(&dir, &["legacy.rs"]);

        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(&file, b"fn legacy_new() {}").unwrap();
        tx.send(file).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let names = symbol_names(&index).await;
        assert!(!names.iter().any(|n| n == "legacy_old"), "names={names:?}");
        assert!(!names.iter().any(|n| n == "legacy_new"), "names={names:?}");
    }

    #[tokio::test]
    async fn test_full_resync_respects_persisted_excludes() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("ignored")).unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"fn kept() {}").unwrap();
        std::fs::write(dir.path().join("ignored/secret.rs"), b"fn secret_fn() {}").unwrap();

        let indexer = Arc::new(Indexer::new(registry::build_default_registry()));
        let (idx, _) = indexer
            .index_project(dir.path(), &["ignored/**".to_string()])
            .unwrap();
        let index = Arc::new(RwLock::new(idx));
        write_meta_with_excludes(&dir, &["ignored/**"]);

        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop_with_overflow(
            rx,
            &dir,
            indexer,
            index.clone(),
            Arc::new(AtomicBool::new(true)), // force full resync
        );

        std::fs::write(dir.path().join("ignored/secret.rs"), b"fn secret_fn2() {}").unwrap();
        tx.send(dir.path().join("ignored/secret.rs")).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let names = symbol_names(&index).await;
        assert!(names.iter().any(|n| n == "kept"), "names={names:?}");
        assert!(!names.iter().any(|n| n == "secret_fn"), "names={names:?}");
        assert!(!names.iter().any(|n| n == "secret_fn2"), "names={names:?}");
    }

    #[tokio::test]
    async fn test_watcher_falls_back_to_defaults_without_persisted_excludes() {
        // node_modules is a built-in excluded directory name: a file edited
        // there must be ignored even when no meta excludes were persisted.
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"fn kept() {}").unwrap();
        std::fs::write(
            dir.path().join("node_modules/pkg/vendor.rs"),
            b"fn vendor_old() {}",
        )
        .unwrap();

        let (index, indexer) = setup(&dir);
        write_meta_with_excludes(&dir, &[]); // empty: fall back to defaults

        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());

        std::fs::write(
            dir.path().join("node_modules/pkg/vendor.rs"),
            b"fn vendor_new() {}",
        )
        .unwrap();
        tx.send(dir.path().join("node_modules/pkg/vendor.rs"))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        let names = symbol_names(&index).await;
        assert!(names.iter().any(|n| n == "kept"));
        assert!(!names.iter().any(|n| n == "vendor_new"), "names={names:?}");
    }
    /// A watcher batch flush records a new revision with a symbol-level diff
    /// in meta, and the in-memory index revision advances with it (issue #82).
    #[tokio::test]
    async fn test_batch_flush_records_revision_and_change_log() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn old_fn() {}").unwrap();

        let indexer = Arc::new(Indexer::new(registry::build_default_registry()));
        let (idx, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let index = Arc::new(RwLock::new(idx));

        // Baseline meta at revision 1.
        let meta_path = dir.path().join("meta.json");
        let mut meta = IndexMeta::new(dir.path());
        meta.record_change(
            index.read().await.symbols.keys().cloned().collect(),
            Vec::new(),
            Vec::new(),
        );
        save_meta(&meta, &meta_path).unwrap();

        // Edit + flush through the debounce loop.
        let (tx, rx) = mpsc::channel(16);
        let handle = spawn_loop(rx, &dir, indexer, index.clone());
        std::fs::write(&file, b"fn new_fn() {}").unwrap();
        tx.send(file).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let meta = load_meta(&meta_path).unwrap();
        assert_eq!(meta.revision, 2, "flush should assign revision 2");
        let entry = meta.change_log.last().unwrap();
        assert_eq!(entry.revision, 2);
        assert!(
            entry.added.iter().any(|id| id.contains("new_fn")),
            "added={:?}",
            entry.added
        );
        assert!(
            entry.removed.iter().any(|id| id.contains("old_fn")),
            "removed={:?}",
            entry.removed
        );

        // In-memory index revision advanced so navigation responses see it.
        let idx = index.read().await;
        assert_eq!(idx.revision, 2);
        assert!(!idx.symbols.values().any(|s| s.name == "old_fn"));
        assert!(idx.symbols.values().any(|s| s.name == "new_fn"));
    }
    /// End-to-end issue #82: after a watcher flush, get_index_changes reports
    /// the new revision with the changed symbols.
    #[tokio::test]
    async fn test_get_index_changes_surfaces_watcher_flush() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn old_fn() {}").unwrap();

        // Index through the real tool so the index lands in the global store
        // where get_index_changes (and other tools) will read it.
        crate::tools::index_project::index_project(
            crate::tools::index_project::IndexProjectParams {
                path: dir.path().to_string_lossy().to_string(),
                exclude: None,
                force: Some(true),
                max_files: None,
                progress_token: None,
                peer: None,
                embed_config: None,
            },
        )
        .await
        .unwrap();

        let canonical =
            crate::path_policy::resolve_project_path(&dir.path().to_string_lossy()).unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        let (idx, _) = Indexer::new(registry::build_default_registry())
            .index_project(dir.path(), &[])
            .unwrap();
        let index = Arc::new(RwLock::new(idx));

        // Flush through the real on-disk index/meta paths.
        let root = dir.path().to_path_buf();
        let indexer2 = Arc::new(Indexer::new(registry::build_default_registry()));
        let (tx, rx) = mpsc::channel(16);
        let (done_tx, _done_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(run_debounce_loop(
            rx,
            root,
            indexer2,
            index.clone(),
            TEST_DEBOUNCE,
            idx_dir.join("index.bin"),
            idx_dir.join("meta.json"),
            None,
            Arc::new(AtomicBool::new(false)),
            idx_dir.join("watch.stop"),
            idx_dir.join("watch.json"),
            done_tx,
        ));
        std::fs::write(&file, b"fn new_fn() {}").unwrap();
        tx.send(file).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let changes = crate::tools::index_changes::get_index_changes(
            crate::tools::index_changes::GetIndexChangesParams {
                project: dir.path().to_string_lossy().to_string(),
                since_revision: Some(0),
                limit: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(changes["current_revision"], json!(2));
        assert_eq!(changes["complete"], json!(true));
        let symbols = changes["changed_symbols"].as_array().unwrap();
        assert!(
            symbols
                .iter()
                .any(|id| id.as_str().is_some_and(|s| s.contains("new_fn"))),
            "changed_symbols={symbols:?}"
        );
    }
    /// Issue #99: writing the stop flag shuts the debounce loop down and the
    /// status file is removed, so another process can observe the stop.
    #[tokio::test]
    async fn test_stop_flag_shuts_down_loop_and_clears_status() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, b"fn keep() {}").unwrap();

        let (index, indexer) = setup(&dir);
        let (tx, rx) = mpsc::channel(16);
        let (done_tx, mut done_rx) = tokio::sync::watch::channel(false);
        let status_path = dir.path().join("watch.json");
        std::fs::write(&status_path, b"{\"pid\": 1}").unwrap();
        let stop_flag = dir.path().join("watch.stop");

        let handle = tokio::spawn(run_debounce_loop(
            rx,
            dir.path().to_path_buf(),
            indexer,
            index.clone(),
            TEST_DEBOUNCE,
            dir.path().join("index.bin"),
            dir.path().join("meta.json"),
            None,
            Arc::new(AtomicBool::new(false)),
            stop_flag.clone(),
            status_path.clone(),
            done_tx,
        ));

        std::fs::write(&stop_flag, b"stop").unwrap();
        // Keep the channel open so the shutdown comes from the stop flag.
        assert!(done_rx.changed().await.is_ok(), "done signal expected");
        drop(tx);
        handle.await.unwrap();
        assert!(
            !status_path.exists(),
            "status file must be removed on stop-flag shutdown"
        );
        assert!(!stop_flag.exists(), "stop flag consumed");
        // Pending edits were flushed before shutdown.
        let names: Vec<_> = index
            .read()
            .await
            .symbols
            .values()
            .map(|s| s.name.to_string())
            .collect();
        assert!(names.contains(&"keep".to_string()));
    }
}
