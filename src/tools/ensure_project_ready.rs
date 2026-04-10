use std::sync::Arc;

use rmcp::{model::ProgressToken, Peer, RoleServer};
use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::tools::index_project::{index_project, IndexProjectParams};
use crate::tools::wait_for_embeddings::{wait_for_embeddings, WaitForEmbeddingsParams};

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
    let wait_result = if embeddings_status == "running" {
        Some(
            wait_for_embeddings(WaitForEmbeddingsParams {
                project: params.path.clone(),
                poll_interval_ms: params.poll_interval_ms,
                timeout_secs: params.timeout_secs,
                progress_token: params.progress_token,
                peer: params.peer,
                embed_config: params.embed_config,
            })
            .await?,
        )
    } else {
        None
    };

    let embeddings_ready = match wait_result
        .as_ref()
        .and_then(|value| value["status"].as_str())
        .unwrap_or(embeddings_status)
    {
        "ok" | "disabled" => true,
        "timeout" => false,
        _ => embeddings_status != "running",
    };

    Ok(json!({
        "status": if embeddings_ready { "ready" } else { "partial" },
        "index": indexed,
        "waited_for_embeddings": wait_result.is_some(),
        "embeddings": wait_result,
        "guidance": {
            "next_step": if embeddings_ready {
                "Project is ready. Proceed directly to trace_execution_path, search_symbols, or search_content without extra startup calls."
            } else {
                "Index is ready but embeddings are still incomplete. You can use non-semantic tools now or wait longer for semantic search."
            },
            "avoid": "Avoid calling wait_for_embeddings separately when ensure_project_ready already handled startup."
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
        assert_eq!(
            result["guidance"]["next_step"],
            json!("Project is ready. Proceed directly to trace_execution_path, search_symbols, or search_content without extra startup calls.")
        );
    }
}
