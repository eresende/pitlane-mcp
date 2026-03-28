use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

use crate::index::format::{load_meta, save_index, save_meta, IndexMeta};
use crate::index::SymbolIndex;
use crate::indexer::{registry, Indexer};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);
const CHANNEL_CAPACITY: usize = 1024;

pub struct ProjectWatcher {
    _watcher: RecommendedWatcher,
}

impl ProjectWatcher {
    pub fn start(
        project_path: PathBuf,
        index: Arc<RwLock<SymbolIndex>>,
        index_path: PathBuf,
        meta_path: PathBuf,
    ) -> anyhow::Result<Self> {
        let project_path_clone = project_path.clone();

        let parsers = registry::build_default_registry();
        let indexer = Arc::new(Indexer::new(parsers));

        let (tx, rx) = mpsc::channel::<PathBuf>(CHANNEL_CAPACITY);

        tokio::spawn(run_debounce_loop(
            rx,
            project_path_clone,
            indexer,
            index,
            DEBOUNCE_WINDOW,
            index_path,
            meta_path,
        ));

        let handler = move |result: notify::Result<Event>| {
            if let Ok(event) = result {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        for path in event.paths {
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            if ext == "rs" || ext == "py" {
                                // Non-blocking send; drop the event if the channel is full rather
                                // than blocking the notify callback thread.
                                let _ = tx.try_send(path);
                            }
                        }
                    }
                    _ => {}
                }
            }
        };

        let mut watcher = recommended_watcher(handler)?;
        watcher.watch(&project_path, RecursiveMode::Recursive)?;

        Ok(Self { _watcher: watcher })
    }
}

/// Collects file paths from `rx` into a `HashSet` (deduplication) and flushes
/// the batch through a single write-lock acquisition once the debounce window
/// expires without a new event, or when the channel is closed.
async fn run_debounce_loop(
    mut rx: mpsc::Receiver<PathBuf>,
    root: PathBuf,
    indexer: Arc<Indexer>,
    index: Arc<RwLock<SymbolIndex>>,
    debounce_window: Duration,
    index_path: PathBuf,
    meta_path: PathBuf,
) {
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;

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
                        if !pending.is_empty() {
                            reindex_batch(&pending, &root, &indexer, &index, &index_path, &meta_path).await;
                        }
                        return;
                    }
                }
            }

            // Debounce window expired — flush the batch.
            _ = tokio::time::sleep(timeout), if deadline.is_some() => {
                reindex_batch(&pending, &root, &indexer, &index, &index_path, &meta_path).await;
                pending.clear();
                deadline = None;
            }
        }
    }
}

async fn reindex_batch(
    paths: &HashSet<PathBuf>,
    root: &Path,
    indexer: &Arc<Indexer>,
    index: &Arc<RwLock<SymbolIndex>>,
    index_path: &Path,
    meta_path: &Path,
) {
    {
        let mut idx = index.write().await;
        for path in paths {
            if let Err(e) = indexer.reindex_file(path, root, &mut idx) {
                eprintln!("Error re-indexing {:?}: {}", path, e);
            }
        }

        // Flush updated index to disk while holding the write lock for consistency.
        if let Err(e) = save_index(&idx, index_path) {
            eprintln!("pitlane-mcp: failed to flush index to disk: {}", e);
        }
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
                        meta.file_mtimes.insert(key, dur.as_secs());
                    }
                }
            }
            Err(_) => {
                // File was deleted — remove its mtime so it isn't treated as fresh.
                meta.file_mtimes.remove(&key);
            }
        }
    }
    if let Err(e) = save_meta(&meta, meta_path) {
        eprintln!("pitlane-mcp: failed to flush meta to disk: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::registry;
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
        let root = dir.path().to_path_buf();
        let index_path = dir.path().join("index.bin");
        let meta_path = dir.path().join("meta.json");
        tokio::spawn(run_debounce_loop(
            rx,
            root,
            indexer,
            index,
            TEST_DEBOUNCE,
            index_path,
            meta_path,
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

        // Should complete well before the debounce window.
        assert!(
            elapsed < TEST_DEBOUNCE,
            "flush took {elapsed:?}, expected faster than {TEST_DEBOUNCE:?}"
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
}
