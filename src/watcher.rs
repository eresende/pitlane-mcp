use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::index::SymbolIndex;
use crate::indexer::{registry, Indexer};

pub struct ProjectWatcher {
    _watcher: RecommendedWatcher,
}

impl ProjectWatcher {
    pub fn start(project_path: PathBuf, index: Arc<RwLock<SymbolIndex>>) -> anyhow::Result<Self> {
        let project_path_clone = project_path.clone();

        let parsers = registry::build_default_registry();
        let indexer = Arc::new(Indexer::new(parsers));

        let handler = move |result: notify::Result<Event>| {
            if let Ok(event) = result {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        for path in &event.paths {
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

                            if ext == "rs" || ext == "py" {
                                let path = path.clone();
                                let root = project_path_clone.clone();
                                let index = index.clone();
                                let indexer = indexer.clone();

                                // Spawn a blocking task to re-index the file
                                tokio::spawn(async move {
                                    let mut idx = index.write().await;
                                    if let Err(e) = indexer.reindex_file(&path, &root, &mut idx) {
                                        eprintln!("Error re-indexing {:?}: {}", path, e);
                                    }
                                });
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
