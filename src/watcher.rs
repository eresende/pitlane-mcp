use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};

use crate::index::SymbolIndex;
use crate::indexer::{registry, Indexer};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(500);
const CHANNEL_CAPACITY: usize = 1024;

pub struct ProjectWatcher {
    _watcher: RecommendedWatcher,
}

impl ProjectWatcher {
    pub fn start(project_path: PathBuf, index: Arc<RwLock<SymbolIndex>>) -> anyhow::Result<Self> {
        let project_path_clone = project_path.clone();

        let parsers = registry::build_default_registry();
        let indexer = Arc::new(Indexer::new(parsers));

        let (tx, mut rx) = mpsc::channel::<PathBuf>(CHANNEL_CAPACITY);

        // Debounce task: collects file paths over a quiet window, then reindexes in one pass.
        tokio::spawn(async move {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut deadline: Option<Instant> = None;

            loop {
                // Compute how long to wait before the debounce window expires.
                let timeout = deadline
                    .map(|d| d.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::MAX);

                tokio::select! {
                    // New path arrived — add to the pending set and push the deadline forward.
                    maybe_path = rx.recv() => {
                        match maybe_path {
                            Some(path) => {
                                pending.insert(path);
                                deadline = Some(Instant::now() + DEBOUNCE_WINDOW);
                            }
                            // Channel closed — flush whatever is pending and exit.
                            None => {
                                if !pending.is_empty() {
                                    reindex_batch(&pending, &project_path_clone, &indexer, &index).await;
                                }
                                return;
                            }
                        }
                    }

                    // Debounce window expired — flush the batch.
                    _ = tokio::time::sleep(timeout), if deadline.is_some() => {
                        reindex_batch(&pending, &project_path_clone, &indexer, &index).await;
                        pending.clear();
                        deadline = None;
                    }
                }
            }
        });

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

async fn reindex_batch(
    paths: &HashSet<PathBuf>,
    root: &PathBuf,
    indexer: &Arc<Indexer>,
    index: &Arc<RwLock<SymbolIndex>>,
) {
    let mut idx = index.write().await;
    for path in paths {
        if let Err(e) = indexer.reindex_file(path, root, &mut idx) {
            eprintln!("Error re-indexing {:?}: {}", path, e);
        }
    }
}
