use std::sync::Arc;

use rmcp::{model::ProgressToken, Peer, RoleServer};
use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::tools::index_project::{index_project, IndexProjectParams};

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
    })
    .await?;

    let embeddings_status = indexed["embeddings"].as_str().unwrap_or("disabled");

    Ok(json!({
        "status": "ready",
        "index": indexed,
        "waited_for_embeddings": false,
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
                "Project is indexed and ready for non-semantic tools. Use trace_execution_path, search_content, search_files, or non-semantic search_symbols now, or call wait_for_embeddings later if semantic search is required."
            } else {
                "Project is ready. Proceed directly to trace_execution_path, search_symbols, or search_content without extra startup calls."
            },
            "avoid": "Avoid blocking startup on wait_for_embeddings unless your client explicitly needs semantic search to be ready."
        }
    }))
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
        })
        .await
        .unwrap();

        assert_eq!(result["status"], json!("ready"));
        assert_eq!(result["index"]["embeddings"], json!("disabled"));
        assert_eq!(result["waited_for_embeddings"], json!(false));
        assert_eq!(result["embeddings"]["status"], json!("disabled"));
        assert_eq!(
            result["guidance"]["next_step"],
            json!("Project is ready. Proceed directly to trace_execution_path, search_symbols, or search_content without extra startup calls.")
        );
        assert_eq!(
            result["guidance"]["avoid"],
            json!("Avoid blocking startup on wait_for_embeddings unless your client explicitly needs semantic search to be ready.")
        );
    }
}
