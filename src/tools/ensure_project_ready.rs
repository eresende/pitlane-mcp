use std::sync::Arc;

use rmcp::{model::ProgressToken, Peer, RoleServer};
use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::tools::index_project::{index_project, IndexProjectParams};
use crate::tools::watch_project::{watch_project, WatchProjectParams, WatcherRegistry};

pub struct EnsureProjectReadyParams {
    pub path: String,
    pub exclude: Option<Vec<String>>,
    pub force: Option<bool>,
    pub max_files: Option<usize>,
    pub poll_interval_ms: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub progress_token: Option<ProgressToken>,
    pub peer: Option<Peer<RoleServer>>,
    pub embed_config: Option<Arc<EmbedConfig>>,
    /// Start a background watcher after indexing so edits are picked up
    /// incrementally (issue #82). Defaults to true when a watcher registry is
    /// available (MCP server); the CLI passes no registry and skips watching.
    pub watch: Option<bool>,
    pub watcher_registry: Option<Arc<WatcherRegistry>>,
}

pub async fn ensure_project_ready(params: EnsureProjectReadyParams) -> anyhow::Result<Value> {
    let indexed = index_project(IndexProjectParams {
        path: params.path.clone(),
        exclude: params.exclude,
        force: params.force,
        max_files: params.max_files,
        progress_token: params.progress_token.clone(),
        peer: params.peer.clone(),
        embed_config: params.embed_config.clone(),
        on_index_progress: None,
        on_phase3_progress: None,
    })
    .await?;

    let embeddings_status = indexed["embeddings"].as_str().unwrap_or("disabled");

    // Optional watching (issue #82): after a successful index, keep it fresh
    // incrementally. Never fails startup if the watcher cannot start.
    let watching = if params.watch.unwrap_or(true) {
        match &params.watcher_registry {
            Some(registry) => match watch_project(
                WatchProjectParams {
                    project: params.path.clone(),
                    stop: Some(false),
                    status_only: Some(false),
                    embed_config: params.embed_config.clone(),
                },
                registry,
            )
            .await
            {
                Ok(status) => status,
                Err(err) => json!({
                    "status": "failed",
                    "message": format!("watcher could not start; index remains valid but will not self-update: {err}"),
                }),
            },
            None => json!({
                "status": "unavailable",
                "message": "Watcher registry not available in this context (CLI mode). Use watch_project on the MCP server for live updates.",
            }),
        }
    } else {
        json!({ "status": "disabled", "message": "watch=false; call watch_project to enable live index updates." })
    };

    let mut ignored_parameters = Vec::new();
    if params.poll_interval_ms.is_some() {
        ignored_parameters.push("poll_interval_ms");
    }
    if params.timeout_secs.is_some() {
        ignored_parameters.push("timeout_secs");
    }

    let mut response = json!({
        "status": "ready",
        "index": indexed,
        "waited_for_embeddings": false,
        "watching": watching,
        "embeddings": {
            "status": embeddings_status,
            "message": if embeddings_status == "running" {
                "Index is ready. Embeddings are still running in the background; call wait_for_embeddings only if your client wants to block for semantic search readiness."
            } else if embeddings_status == "disabled" {
                "Embeddings are disabled. Non-semantic tools are ready immediately."
            } else {
                "Embeddings are ready for semantic search."
            }
        },
        "guidance": {
            "next_step": if embeddings_status == "running" {
                "Project is indexed and ready for non-semantic tools. Use locate_code for ambiguous discovery, trace_path for flow questions, read_code_unit to inspect a known target, or get_index_stats for repo orientation. Call wait_for_embeddings later only if semantic search is required."
            } else {
                "Project is ready. Use locate_code for ambiguous discovery, trace_path for flow questions, read_code_unit for focused inspection, or get_index_stats for repo orientation."
            },
            "avoid": "Avoid blocking startup on wait_for_embeddings unless your client explicitly needs semantic search to be ready."
        }
    });

    if !ignored_parameters.is_empty() {
        response["ignored_parameters"] = json!(ignored_parameters);
        response["compatibility"] = json!({
            "note": "poll_interval_ms and timeout_secs are accepted for compatibility but ignored; ensure_project_ready does not wait for embeddings. Use wait_for_embeddings when blocking is required."
        });
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_ensure_project_ready_without_embeddings_returns_ready() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let result = ensure_project_ready(EnsureProjectReadyParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            poll_interval_ms: None,
            timeout_secs: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            watch: Some(false),
            watcher_registry: None,
        })
        .await
        .unwrap();

        assert_eq!(result["status"], json!("ready"));
        assert_eq!(result["index"]["embeddings"], json!("disabled"));
        assert_eq!(result["waited_for_embeddings"], json!(false));
        assert_eq!(result["embeddings"]["status"], json!("disabled"));
        assert_eq!(
            result["guidance"]["next_step"],
            json!("Project is ready. Use locate_code for ambiguous discovery, trace_path for flow questions, read_code_unit for focused inspection, or get_index_stats for repo orientation.")
        );
        assert_eq!(
            result["guidance"]["avoid"],
            json!("Avoid blocking startup on wait_for_embeddings unless your client explicitly needs semantic search to be ready.")
        );
    }

    #[tokio::test]
    async fn test_ensure_project_ready_reports_ignored_wait_parameters() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let result = ensure_project_ready(EnsureProjectReadyParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            poll_interval_ms: Some(500),
            timeout_secs: Some(60),
            progress_token: None,
            peer: None,
            embed_config: None,
            watch: Some(false),
            watcher_registry: None,
        })
        .await
        .unwrap();

        let ignored = result["ignored_parameters"].as_array().unwrap();
        assert_eq!(ignored.len(), 2);
        assert!(result["compatibility"]["note"].is_string());
    }

    #[tokio::test]
    async fn test_ensure_project_ready_watch_false_reports_disabled() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let result = ensure_project_ready(EnsureProjectReadyParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            poll_interval_ms: None,
            timeout_secs: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            watch: Some(false),
            watcher_registry: None,
        })
        .await
        .unwrap();

        assert_eq!(result["watching"]["status"], json!("disabled"));
    }

    #[tokio::test]
    async fn test_ensure_project_ready_without_registry_reports_unavailable() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();

        let result = ensure_project_ready(EnsureProjectReadyParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            poll_interval_ms: None,
            timeout_secs: None,
            progress_token: None,
            peer: None,
            embed_config: None,
            watch: Some(true),
            watcher_registry: None,
        })
        .await
        .unwrap();

        // CLI-style invocation: no registry, watching degrades gracefully.
        assert_eq!(result["watching"]["status"], json!("unavailable"));
    }
}
