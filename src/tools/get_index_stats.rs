use std::collections::HashMap;

use serde_json::{json, Value};

use crate::embed::store::EmbedStore;
use crate::embed::EmbedConfig;
use crate::index::format::index_dir;
use crate::index::format::load_project_meta;
use crate::index::repo_profile::{
    archetype_label, compact_repo_map, path_role_for_file, profile_entrypoints,
    role_boost_for_path, role_label, summarize_role_counts,
};
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering};

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
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
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
    if let Some(ref profile) = profile {
        let architecture_anchors = build_architecture_anchors(&canonical, &index, Some(profile));
        result.insert(
            "repo_profile".into(),
            json!({
                "archetype": archetype_label(profile.archetype),
                "role_counts": summarize_role_counts(Some(profile)),
                "entrypoints": profile.entrypoints.clone(),
            }),
        );
        result.insert("repo_map".into(), compact_repo_map(Some(profile)));
        result.insert("architecture_anchors".into(), architecture_anchors);
    }

    let recommended_target = result
        .get("architecture_anchors")
        .and_then(|anchors| anchors.get("primary_file"))
        .filter(|value| !value.is_null())
        .map(|file_path| {
            json!({
                "project": params.project,
                "file_path": file_path,
            })
        })
        .unwrap_or_else(|| json!({ "project": params.project }));

    let steering = build_steering(
        if result.get("architecture_anchors").is_some() {
            0.74
        } else {
            0.58
        },
        if result.get("architecture_anchors").is_some() {
            "Repo oriented. Use read_code_unit on the primary file next.".to_string()
        } else {
            "Index stats only. Use locate_code for discovery.".to_string()
        },
        if result.get("architecture_anchors").is_some() {
            "read_code_unit"
        } else {
            "get_project_outline"
        },
        recommended_target,
        vec![],
    );
    let mut value = Value::Object(result);
    attach_steering(&mut value, steering);

    // Add explicit recommended_action for the primary file.
    if let Some(primary_file) = value
        .get("architecture_anchors")
        .and_then(|a| a.get("primary_file"))
        .and_then(|f| f.as_str())
    {
        value["recommended_action"] = json!({
            "tool": "read_code_unit",
            "file_path": primary_file,
        });
    }

    Ok(value)
}

fn build_architecture_anchors(
    project_path: &std::path::Path,
    index: &crate::index::SymbolIndex,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
) -> Value {
    let mut files = index
        .by_file
        .iter()
        .map(|(file_path, symbol_ids)| {
            let rel = file_path
                .strip_prefix(project_path)
                .unwrap_or(file_path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            let role = path_role_for_file(project_path, file_path, profile);
            let symbol_count = symbol_ids.len();
            let score =
                role_boost_for_path(project_path, file_path, profile, "architecture package map")
                    + symbol_count.min(12) as i32
                    + if profile_entrypoints(profile)
                        .iter()
                        .any(|entry| entry == &rel)
                    {
                        8
                    } else {
                        0
                    };
            json!({
                "file": rel,
                "role": role_label(role),
                "symbol_count": symbol_count,
                "score": score,
            })
        })
        .collect::<Vec<_>>();

    files.sort_by(|left, right| {
        right["score"]
            .as_i64()
            .unwrap_or_default()
            .cmp(&left["score"].as_i64().unwrap_or_default())
            .then_with(|| {
                right["symbol_count"]
                    .as_u64()
                    .unwrap_or_default()
                    .cmp(&left["symbol_count"].as_u64().unwrap_or_default())
            })
            .then_with(|| {
                left["file"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["file"].as_str().unwrap_or_default())
            })
    });
    files.truncate(5);

    let primary_file = files
        .first()
        .and_then(|item| item["file"].as_str())
        .map(str::to_owned);
    let workspace_roots = top_level_roots(index, project_path);

    json!({
        "primary_file": primary_file,
        "central_files": files,
        "workspace_roots": workspace_roots,
    })
}

fn top_level_roots(
    index: &crate::index::SymbolIndex,
    project_path: &std::path::Path,
) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for file_path in index.by_file.keys() {
        let rel = file_path
            .strip_prefix(project_path)
            .unwrap_or(file_path.as_path());
        let root = rel
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        if !root.is_empty() {
            *counts.entry(root).or_insert(0) += 1;
        }
    }
    let mut items = counts.into_iter().collect::<Vec<_>>();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items.truncate(6);
    items.into_iter().map(|(root, _)| root).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{build_index_meta, index_dir, save_index, save_meta};
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
        let meta = build_index_meta(&canonical, &index);
        save_meta(&meta, &idx_dir.join("meta.json")).unwrap();
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
        assert_eq!(result["repo_profile"]["archetype"], json!("library"));
        assert_eq!(result["repo_map"]["archetype"], json!("library"));
        assert!(result["repo_map"]["top_roles"].is_array());
        assert!(result["architecture_anchors"]["central_files"].is_array());
        assert_eq!(
            result["steering"]["recommended_next_tool"],
            json!("read_code_unit")
        );
        assert!(result["architecture_anchors"]["primary_file"].is_string());

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
