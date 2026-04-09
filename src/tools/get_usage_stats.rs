use serde_json::{json, Value};

use crate::path_policy::resolve_project_path;
use crate::stats::{load_stats, ProjectStats};

pub struct GetUsageStatsParams {
    pub project: Option<String>,
}

pub async fn get_usage_stats(params: GetUsageStatsParams) -> anyhow::Result<Value> {
    let stats = load_stats();

    if let Some(project) = params.project {
        let project = resolve_project_path(&project)?
            .to_string_lossy()
            .to_string();
        let proj = stats.by_project.get(&project).cloned().unwrap_or_default();
        return Ok(json!({
            "project": project,
            "stats": format_stats(&proj),
        }));
    }

    let by_project: serde_json::Map<String, Value> = stats
        .by_project
        .iter()
        .map(|(k, v)| (k.clone(), format_stats(v)))
        .collect();

    Ok(json!({
        "total": format_stats(&stats.total),
        "by_project": by_project,
    }))
}

fn format_stats(s: &ProjectStats) -> Value {
    json!({
        "get_symbol_calls": s.get_symbol_calls,
        "signature_only_applied": s.signature_only_applied,
        "full_source_bytes": s.full_source_bytes,
        "returned_bytes": s.returned_bytes,
        "tokens_saved_approx": s.tokens_saved_approx(),
    })
}
