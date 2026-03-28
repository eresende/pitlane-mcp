use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::index::format::index_dir;
use crate::tools::index_project::load_project_index;
use crate::watcher::ProjectWatcher;

pub struct WatchProjectParams {
    pub project: String,
    pub stop: Option<bool>,
}

pub struct WatcherRegistry {
    watchers: Mutex<HashMap<String, ProjectWatcher>>,
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn watch(&self, project: &str) -> anyhow::Result<Value> {
        let canonical = Path::new(project)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(project).to_path_buf());
        let key = canonical.display().to_string();

        // Check if already watching
        {
            let watchers = self.watchers.lock().unwrap();
            if watchers.contains_key(&key) {
                return Ok(json!({
                    "status": "already_running",
                    "project": key,
                    "watched_path": key,
                }));
            }
        }

        // Load existing index for the watcher to maintain incrementally.
        // load_project_index returns an Arc<SymbolIndex> (immutable snapshot).
        // The watcher needs its own mutable copy to apply incremental updates,
        // so we clone the snapshot once here at startup.
        let existing_index = load_project_index(project)?;
        let project_index_arc = Arc::new(RwLock::new((*existing_index).clone()));

        // Resolve disk paths so the watcher can flush updates after each batch.
        let idx_dir = index_dir(&canonical)?;
        let index_path = idx_dir.join("index.bin");
        let meta_path = idx_dir.join("meta.json");

        let watcher =
            ProjectWatcher::start(canonical.clone(), project_index_arc, index_path, meta_path)?;

        {
            let mut watchers = self.watchers.lock().unwrap();
            watchers.insert(key.clone(), watcher);
        }

        Ok(json!({
            "status": "started",
            "project": key,
            "watched_path": key,
        }))
    }

    pub fn stop(&self, project: &str) -> Value {
        let canonical = Path::new(project)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(project).to_path_buf());
        let key = canonical.display().to_string();

        let mut watchers = self.watchers.lock().unwrap();
        if watchers.remove(&key).is_some() {
            json!({
                "status": "stopped",
                "project": key,
            })
        } else {
            json!({
                "status": "not_running",
                "project": key,
            })
        }
    }
}

pub async fn watch_project(
    params: WatchProjectParams,
    registry: &WatcherRegistry,
) -> anyhow::Result<Value> {
    if params.stop.unwrap_or(false) {
        Ok(registry.stop(&params.project))
    } else {
        registry.watch(&params.project).await
    }
}
