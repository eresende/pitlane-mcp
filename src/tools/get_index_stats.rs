use std::collections::HashMap;

use serde_json::{json, Value};

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

    Ok(json!({
        "project": params.project,
        "total_files": index.file_count(),
        "total_symbols": index.symbol_count(),
        "by_language": by_language_map,
        "by_kind": by_kind_map,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;

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
    }
}
