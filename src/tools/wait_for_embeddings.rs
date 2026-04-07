use std::sync::Arc;

use rmcp::{
    model::{LoggingLevel, LoggingMessageNotificationParam, ProgressNotificationParam},
    Peer, RoleServer,
};
use serde_json::{json, Value};

use crate::{
    embed::{progress as embed_progress, store::EmbedStore, EmbedConfig},
    index::format::index_dir,
    tools::index_project::load_project_index,
};

pub struct WaitForEmbeddingsParams {
    pub project: String,
    /// How often to poll, in milliseconds. Default: 2000.
    pub poll_interval_ms: Option<u64>,
    /// Maximum time to wait, in seconds. Default: 300 (5 min).
    pub timeout_secs: Option<u64>,
    pub progress_token: Option<rmcp::model::ProgressToken>,
    pub peer: Option<Peer<RoleServer>>,
    pub embed_config: Option<Arc<EmbedConfig>>,
}

pub async fn wait_for_embeddings(params: WaitForEmbeddingsParams) -> anyhow::Result<Value> {
    // Embeddings disabled — return immediately.
    if params.embed_config.is_none() {
        return Ok(json!({
            "status": "disabled",
            "message": "Embeddings are not configured (PITLANE_EMBED_URL / PITLANE_EMBED_MODEL not set)."
        }));
    }

    let index = load_project_index(&params.project)?;
    let total_symbols = index.symbol_count();

    let canonical = std::path::Path::new(&params.project)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&params.project));
    let idx_dir = index_dir(&canonical)?;
    let store_path = idx_dir.join("embeddings.bin");

    let poll_ms = params.poll_interval_ms.unwrap_or(2_000);
    let timeout_secs = params.timeout_secs.unwrap_or(300);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // Prefer the in-memory registry (live progress from the background task).
        // Fall back to the on-disk store for the case where embedding already
        // finished before this call (registry entry was removed on completion).
        let p = embed_progress::get(&canonical);
        let (stored, total) = if let Some(ref prog) = p {
            (prog.stored, prog.total)
        } else {
            let stored = EmbedStore::load(&store_path)
                .map(|s| s.vectors.len())
                .unwrap_or(0);
            (stored, total_symbols)
        };

        let msg = format!("Embeddings: {stored}/{total} symbols");

        // Send MCP logging notification so the agent sees the progress bar.
        if let Some(ref peer) = params.peer {
            let log_notif =
                LoggingMessageNotificationParam::new(LoggingLevel::Info, serde_json::json!(msg))
                    .with_logger("pitlane-embed".to_string());
            let _ = peer.notify_logging_message(log_notif).await;

            // Also send a structured progress notification if the client provided a token.
            if let Some(ref token) = params.progress_token {
                let notif = ProgressNotificationParam::new(token.clone(), stored as f64)
                    .with_total(total as f64)
                    .with_message(msg.clone());
                let _ = peer.notify_progress(notif).await;
            }
        }

        if stored >= total {
            return Ok(json!({
                "status": "ok",
                "embeddings_stored": stored,
                "embeddings_total": total,
                "embeddings_percent": 100.0,
                "embeddings_started_at": p.as_ref().map(|p| p.started_at),
                "embeddings_finished_at": p.as_ref().and_then(|p| p.finished_at),
                "message": format!("Embeddings complete: {stored}/{total} symbols ready for semantic search.")
            }));
        }

        if std::time::Instant::now() >= deadline {
            let pct = if total == 0 {
                100.0
            } else {
                (stored as f64 / total as f64 * 100.0).min(100.0)
            };
            return Ok(json!({
                "status": "timeout",
                "embeddings_stored": stored,
                "embeddings_total": total,
                "embeddings_percent": (pct * 10.0).round() / 10.0,
                "embeddings_started_at": p.as_ref().map(|p| p.started_at),
                "embeddings_finished_at": p.as_ref().and_then(|p| p.finished_at),
                "message": format!(
                    "Timed out after {timeout_secs}s. {stored}/{total} symbols embedded. \
                     Call get_index_stats to check progress later."
                )
            }));
        }

        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }
}
