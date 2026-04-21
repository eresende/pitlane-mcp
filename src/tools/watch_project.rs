use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::embed::EmbedConfig;
use crate::error::ToolError;
use crate::index::format::index_dir;
use crate::path_policy::resolve_project_path;
use crate::sync_utils::mutex_lock;
use crate::tools::index_project::load_project_index;
use crate::watcher::ProjectWatcher;

pub struct WatchProjectParams {
    pub project: String,
    pub stop: Option<bool>,
    pub status_only: Option<bool>,
    pub embed_config: Option<Arc<EmbedConfig>>,
}

pub struct WatcherRegistry {
    watchers: Mutex<HashMap<String, ProjectWatcher>>,
}

impl Default for WatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn watch(
        &self,
        project: &str,
        embed_config: Option<Arc<EmbedConfig>>,
    ) -> anyhow::Result<Value> {
        let canonical = resolve_project_path(project)?;
        let key = canonical.display().to_string();

        // Check if already watching
        {
            let watchers = mutex_lock(&self.watchers);
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

        let watcher = ProjectWatcher::start(
            canonical.clone(),
            project_index_arc,
            index_path,
            meta_path,
            embed_config,
        )?;

        {
            let mut watchers = mutex_lock(&self.watchers);
            watchers.insert(key.clone(), watcher);
        }

        Ok(json!({
            "status": "started",
            "project": key,
            "watched_path": key,
        }))
    }

    pub fn stop(&self, project: &str) -> Value {
        let canonical = match resolve_project_path(project) {
            Ok(canonical) => canonical,
            Err(err) => match err.downcast::<ToolError>() {
                Ok(err) => return err.to_json(),
                Err(err) => {
                    return ToolError::Internal {
                        message: err.to_string(),
                    }
                    .to_json()
                }
            },
        };
        let key = canonical.display().to_string();

        let mut watchers = mutex_lock(&self.watchers);
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

    pub fn status(&self, project: &str) -> Value {
        let canonical = match resolve_project_path(project) {
            Ok(canonical) => canonical,
            Err(err) => match err.downcast::<ToolError>() {
                Ok(err) => return err.to_json(),
                Err(err) => {
                    return ToolError::Internal {
                        message: err.to_string(),
                    }
                    .to_json()
                }
            },
        };
        let key = canonical.display().to_string();
        let watchers = mutex_lock(&self.watchers);
        let watching = watchers.contains_key(&key);
        json!({
            "project": key,
            "status": if watching { "watching" } else { "not_watching" },
        })
    }
}

pub async fn watch_project(
    params: WatchProjectParams,
    registry: &WatcherRegistry,
) -> anyhow::Result<Value> {
    if params.status_only.unwrap_or(false) {
        Ok(registry.status(&params.project))
    } else if params.stop.unwrap_or(false) {
        Ok(registry.stop(&params.project))
    } else {
        registry.watch(&params.project, params.embed_config).await
    }
}
