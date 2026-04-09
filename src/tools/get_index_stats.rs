use std::collections::HashMap;

use serde_json::{json, Value};

use crate::embed::store::EmbedStore;
use crate::embed::EmbedConfig;
use crate::index::format::index_dir;
use crate::tools::index_project::load_project_index;

pub struct GetIndexStatsParams {
    pub project: String,
}

pub async fn get_index_stats(params: GetIndexStatsParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;

    let mut by_language: HashMap<String, usize> = HashMap::new();
    let mut by_kind: HashMap<String, usize> = HashMap::new();

    for sym in index.symbols.values() {
        *by_language.entry(sym.language.to_string()).or_insert(0) += 1;
        *by_kind.entry(sym.kind.to_string()).or_insert(0) += 1;
    }

    // Sort for deterministic output
    let mut by_language: Vec<(String, usize)> = by_language.into_iter().collect();
    by_language.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut by_kind: Vec<(String, usize)> = by_kind.into_iter().collect();
    by_kind.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let by_language_map: serde_json::Map<String, Value> = by_language
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect();
    let by_kind_map: serde_json::Map<String, Value> =
        by_kind.into_iter().map(|(k, v)| (k, json!(v))).collect();

    // Embedding progress
    let total_symbols = index.symbol_count();
    let embed_config = EmbedConfig::from_env();
    let canonical = std::path::Path::new(&params.project)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&params.project));
    let idx_dir = index_dir(&canonical)?;
    let store_path = idx_dir.join("embeddings.bin");

    let (embeddings_status, embeddings_stored, embeddings_total, embeddings_percent) =
        if embed_config.is_none() {
            ("disabled".to_string(), None, None, None)
        } else {
            // Prefer the in-memory registry (live progress from the background task).
            // Fall back to the on-disk store for the case where embedding already
            // finished before this call (registry entry was removed on completion).
            let (stored, total) = if let Some(p) = crate::embed::progress::get(&canonical) {
                (p.stored, p.total)
            } else {
                let stored = EmbedStore::load(&store_path)
                    .map(|s| s.vectors.len())
                    .unwrap_or(0);
                (stored, total_symbols)
            };
            let pct = if total == 0 {
                100.0
            } else {
                (stored as f64 / total as f64 * 100.0).min(100.0)
            };
            let status = if stored >= total { "ok" } else { "running" };
            (
                status.to_string(),
                Some(stored),
                Some(total),
                Some((pct * 10.0).round() / 10.0), // one decimal place
            )
        };

    let mut result = serde_json::Map::new();
    result.insert("project".into(), json!(params.project));
    result.insert("total_files".into(), json!(index.file_count()));
    result.insert("total_symbols".into(), json!(total_symbols));
    result.insert("by_language".into(), json!(by_language_map));
    result.insert("by_kind".into(), json!(by_kind_map));
    result.insert("embeddings".into(), json!(embeddings_status));
    if let Some(stored) = embeddings_stored {
        result.insert("embeddings_stored".into(), json!(stored));
    }
    if let Some(total) = embeddings_total {
        result.insert("embeddings_total".into(), json!(total));
    }
    if let Some(pct) = embeddings_percent {
        result.insert("embeddings_percent".into(), json!(pct));
    }

    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    /// Env-var mutations are process-global; serialize tests that touch them.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    async fn setup_project(dir: &TempDir) -> String {
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_get_index_stats_basic() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("PITLANE_EMBED_URL");
        std::env::remove_var("PITLANE_EMBED_MODEL");

        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn foo() {}\npub struct Bar {}",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = get_index_stats(GetIndexStatsParams { project })
            .await
            .unwrap();

        assert_eq!(result["total_symbols"].as_u64().unwrap(), 2);
        assert_eq!(result["total_files"].as_u64().unwrap(), 1);
        assert!(result["by_language"]["rust"].as_u64().unwrap() > 0);
        assert!(result["by_kind"]["function"].as_u64().unwrap() > 0);
        assert!(result["by_kind"]["struct"].as_u64().unwrap() > 0);

        // Embedding fields: env vars not set → disabled, no progress fields
        assert_eq!(result["embeddings"].as_str().unwrap(), "disabled");
        assert!(result.get("embeddings_stored").is_none());
        assert!(result.get("embeddings_total").is_none());
        assert!(result.get("embeddings_percent").is_none());
    }

    #[tokio::test]
    async fn test_get_index_stats_embeddings_progress() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var("PITLANE_EMBED_URL", "http://localhost:11434");
        std::env::set_var("PITLANE_EMBED_MODEL", "test-model");

        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn foo() {}\npub struct Bar {}",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        // Write a partial embeddings store (1 of 2 symbols)
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        let mut store = EmbedStore::new();
        store.update("fake_sym".to_string(), vec![0.1, 0.2, 0.3]);
        store.save(&idx_dir.join("embeddings.bin")).unwrap();

        let result = get_index_stats(GetIndexStatsParams {
            project: project.clone(),
        })
        .await
        .unwrap();

        // Clean up env vars
        std::env::remove_var("PITLANE_EMBED_URL");
        std::env::remove_var("PITLANE_EMBED_MODEL");

        assert_eq!(result["embeddings"].as_str().unwrap(), "running");
        assert_eq!(result["embeddings_stored"].as_u64().unwrap(), 1);
        assert_eq!(result["embeddings_total"].as_u64().unwrap(), 2);
        assert_eq!(result["embeddings_percent"].as_f64().unwrap(), 50.0);
    }
}
