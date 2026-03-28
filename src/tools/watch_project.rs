use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::index::SymbolIndex;
use crate::tools::index_project::load_project_index;
use crate::watcher::ProjectWatcher;

pub struct WatchProjectParams {
    pub project: String,
    pub stop: Option<bool>,
}

pub struct WatcherRegistry {
    watchers: Mutex<HashMap<String, ProjectWatcher>>,
    indexes: Arc<RwLock<HashMap<String, SymbolIndex>>>,
}

impl WatcherRegistry {
    pub fn new(indexes: Arc<RwLock<HashMap<String, SymbolIndex>>>) -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
            indexes,
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

        // Load existing index into registry
        let existing_index = load_project_index(project)?;
        {
            let mut indexes = self.indexes.write().await;
            indexes.insert(key.clone(), existing_index);
        }

        // Get a handle to the specific project's index for the watcher
        let project_index_arc = {
            // We need a per-project Arc<RwLock<SymbolIndex>>
            // For simplicity, create a new one that we'll sync
            Arc::new(RwLock::new(SymbolIndex::new()))
        };

        let watcher = ProjectWatcher::start(canonical.clone(), project_index_arc)?;

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
