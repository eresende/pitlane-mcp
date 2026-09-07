use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

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

// ── Cross-process watcher lifecycle (issue #99) ──
// The watcher publishes `watch.json` under the project's index directory so
// any process can query status, and honors a `watch.stop` flag file so any
// `pitlane watch --stop` can shut it down cleanly. No signal handling, works
// on all platforms.

/// Path of the watcher status file inside an index directory.
pub fn watch_status_path(idx_dir: &Path) -> PathBuf {
    idx_dir.join("watch.json")
}

/// Write the status file for the current process. Called by
/// `ProjectWatcher::start`; returns an error when another live watcher
/// already owns the project (its recorded PID is still running).
pub fn write_watch_status(status_path: &Path) -> anyhow::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(status_path) {
        if let Ok(value) = serde_json::from_str::<Value>(&existing) {
            if let Some(pid) = value["pid"].as_u64() {
                if pid_alive(pid as i32) && pid as i32 != std::process::id() as i32 {
                    anyhow::bail!("another watcher (pid {pid}) is already watching this project");
                }
            }
        }
    }
    let body = json!({
        "pid": std::process::id(),
        "started_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    if let Some(parent) = status_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(status_path, serde_json::to_vec_pretty(&body)?)?;
    Ok(())
}

/// Remove the status file (idempotent).
pub fn remove_watch_status(status_path: &Path) {
    let _ = std::fs::remove_file(status_path);
}

/// Is a PID alive? `/proc` on Linux, `kill -0` on other Unix (a nonexistent
/// PID fails with an error we can observe). Elsewhere: assume alive
/// (conservative: a stale file then requires an explicit `--stop` to clear).
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Read the watcher status for a project across process boundaries:
/// `running` when the status file names a live PID, `stale` when the file
/// exists but the PID is gone, `not_running` when there is no file.
pub fn external_status(project: &str) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(project)?;
    let status_path = watch_status_path(&index_dir(&canonical)?);
    match std::fs::read_to_string(&status_path) {
        Err(_) => {
            Ok(json!({ "project": canonical.display().to_string(), "status": "not_running" }))
        }
        Ok(content) => {
            let value: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
            let pid = value["pid"].as_u64().unwrap_or(0) as i32;
            if pid_alive(pid) {
                Ok(json!({
                    "project": canonical.display().to_string(),
                    "status": "running",
                    "pid": pid,
                    "started_at": value["started_at"],
                }))
            } else {
                remove_watch_status(&status_path);
                Ok(json!({
                    "project": canonical.display().to_string(),
                    "status": "stale",
                    "pid": pid,
                }))
            }
        }
    }
}

/// Request a cross-process stop: write the stop flag and wait (up to ~5s)
/// for the status file to disappear.
pub async fn external_stop(project: &str) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(project)?;
    let idx_dir = index_dir(&canonical)?;
    let status_path = watch_status_path(&idx_dir);
    if !status_path.exists() {
        return Ok(json!({ "project": canonical.display().to_string(), "status": "not_running" }));
    }
    std::fs::write(idx_dir.join("watch.stop"), b"stop")?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if !status_path.exists() {
            return Ok(json!({ "project": canonical.display().to_string(), "status": "stopped" }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(json!({
        "project": canonical.display().to_string(),
        "status": "timeout",
        "note": "the watcher did not acknowledge the stop flag within 5s; it may be blocked or running as a different user",
    }))
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

        // Check if already watching (in-process registry).
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

        // Check if a watcher from another process owns this project
        // (cross-process guard via the status file, issue #99).
        let external = external_status(project)?;
        if external["status"] == "running" {
            return Ok(json!({
                "status": "already_running",
                "project": key,
                "watched_path": key,
                "owner_pid": external["pid"],
                "note": "another process is already watching this project; use watch_project with stop=true (or `pitlane watch --stop`) to stop it first",
            }));
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

    /// Take ownership of the watcher for a project, if present. Used by the
    /// CLI so it can await the watcher's shutdown (stop flag or error).
    pub fn watchers_lock(&self) -> MutexGuard<'_, HashMap<String, ProjectWatcher>> {
        mutex_lock(&self.watchers)
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
#[cfg(test)]
mod cross_process_tests {
    use super::*;
    use tempfile::TempDir;

    /// external_status reports not_running / running / stale from the status
    /// file, and cleans up stale entries.
    #[tokio::test]
    async fn test_external_status_lifecycle() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hi() {}\n").unwrap();
        let project = dir.path().to_string_lossy().to_string();
        let idx_dir = index_dir(dir.path()).unwrap();
        let status_path = watch_status_path(&idx_dir);

        // Not running.
        let status = external_status(&project).unwrap();
        assert_eq!(status["status"], json!("not_running"));

        // Running: this process's own PID.
        write_watch_status(&status_path).unwrap();
        let status = external_status(&project).unwrap();
        assert_eq!(status["status"], json!("running"));
        assert_eq!(status["pid"], json!(std::process::id()));

        // Stale: a PID that cannot exist. On Windows the liveness check is
        // conservative (assumes alive without a Win32 API dependency), so
        // stale detection is a Unix-only behavior.
        std::fs::write(&status_path, br#"{"pid": 99999999}"#).unwrap();
        #[cfg(not(windows))]
        {
            let status = external_status(&project).unwrap();
            assert_eq!(status["status"], json!("stale"));
            assert!(!status_path.exists(), "stale entry cleaned up");
        }
        #[cfg(windows)]
        {
            let status = external_status(&project).unwrap();
            assert_eq!(
                status["status"],
                json!("running"),
                "windows assumes unknown PIDs are alive (conservative fallback)"
            );
        }
    }

    /// external_stop writes the flag and reports stopped once the watcher
    /// consumes it and clears the status file.
    #[tokio::test]
    async fn test_external_stop_round_trip() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hi() {}\n").unwrap();
        let project = dir.path().to_string_lossy().to_string();
        let idx_dir = index_dir(dir.path()).unwrap();
        let status_path = watch_status_path(&idx_dir);

        // Simulate a watcher: publish status, then consume the stop flag by
        // watching for it in the background (as run_debounce_loop does).
        write_watch_status(&status_path).unwrap();
        let flag_path = idx_dir.join("watch.stop");
        let flag_for_task = flag_path.clone();
        let status_for_task = status_path.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                if flag_for_task.exists() {
                    let _ = std::fs::remove_file(&flag_for_task);
                    remove_watch_status(&status_for_task);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });

        let result = external_stop(&project).await.unwrap();
        assert_eq!(result["status"], json!("stopped"));
        assert!(!flag_path.exists(), "flag consumed by the watcher");

        // Stopping again reports not_running.
        let result = external_stop(&project).await.unwrap();
        assert_eq!(result["status"], json!("not_running"));
    }

    /// A live watcher from this process does not block a re-start (same PID),
    /// but the guard message path for foreign PIDs is exercised by write_watch_status.
    #[tokio::test]
    async fn test_write_status_allows_same_pid_restart() {
        let dir = TempDir::new().unwrap();
        let status_path = dir.path().join("watch.json");
        write_watch_status(&status_path).unwrap();
        // Same process: allowed to overwrite its own status file.
        write_watch_status(&status_path).unwrap();
        let content: Value = serde_json::from_slice(&std::fs::read(&status_path).unwrap()).unwrap();
        assert_eq!(content["pid"], json!(std::process::id()));
    }
}
