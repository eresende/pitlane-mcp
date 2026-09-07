use serde_json::{json, Value};
use std::path::Path;
use walkdir::WalkDir;

use crate::embed::document::document_fingerprint;
use crate::embed::store::EmbedStore;
use crate::embed::{endpoint_fingerprint, EmbedConfig};
use crate::index::format::{index_dir, load_meta};
use crate::indexer::{is_supported_extension, Indexer};
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::{
    index_project, is_index_up_to_date, load_project_index, IndexProjectParams,
};

pub struct DoctorParams {
    pub project: String,
    /// Apply safe repairs: force re-index when the index is stale, and kick
    /// off background embedding when vectors are incomplete. Repairs only
    /// touch rebuildable artifacts (index, embeddings) — never user files.
    pub repair: Option<bool>,
}

/// One diagnostic result. `status` is one of `ok`, `warn`, `error`, `info`.
/// A `repair` string describes the action `repair: true` (or the user) can take.
struct Check {
    name: &'static str,
    status: &'static str,
    detail: Value,
    repair: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: Value) -> Self {
        Self {
            name,
            status: "ok",
            detail,
            repair: None,
        }
    }

    fn info(name: &'static str, detail: Value) -> Self {
        Self {
            name,
            status: "info",
            detail,
            repair: None,
        }
    }

    fn warn(name: &'static str, detail: Value, repair: String) -> Self {
        Self {
            name,
            status: "warn",
            detail,
            repair: Some(repair),
        }
    }

    fn error(name: &'static str, detail: Value, repair: String) -> Self {
        Self {
            name,
            status: "error",
            detail,
            repair: Some(repair),
        }
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "check": self.name,
            "status": self.status,
            "detail": self.detail,
        });
        if let Some(repair) = &self.repair {
            v["repair"] = json!(repair);
        }
        v
    }
}

/// Count supported-source files above the indexer's size limit (they are
/// skipped at indexing time) by walking the project with the effective
/// exclusion policy applied.
fn count_oversized_files(root: &Path, exclude_patterns: &[String]) -> anyhow::Result<usize> {
    let exclude_set = Indexer::build_exclude_set(exclude_patterns)?;
    let mut count = 0;
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();
            let rel = match path.strip_prefix(root) {
                Ok(r) => r,
                Err(_) => return true,
            };
            if rel == Path::new("") {
                return true;
            }
            let rel_str = rel.to_string_lossy();
            !exclude_set.is_match(rel_str.as_ref())
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if is_supported_extension(ext)
            && entry.metadata().map(|m| m.len()).unwrap_or(0) > 1024 * 1024
        {
            count += 1;
        }
    }
    Ok(count)
}

pub async fn doctor(params: DoctorParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;
    let idx_dir = index_dir(&canonical)?;
    let mut checks: Vec<Check> = Vec::new();

    // 1. Index loads.
    let index = match load_project_index(&params.project) {
        Ok(index) => {
            checks.push(Check::ok(
                "index_loads",
                json!({
                    "symbol_count": index.symbol_count(),
                    "index_revision": index.revision,
                }),
            ));
            Some(index)
        }
        Err(err) => {
            checks.push(Check::error(
                "index_loads",
                json!({ "error": err.to_string() }),
                "Run `pitlane index --force <project>` (or call index_project with force=true) to build the index.".to_string(),
            ));
            None
        }
    };

    // 2. Meta validity: parses, version current, path matches.
    let meta = load_meta(&idx_dir.join("meta.json")).ok();
    match &meta {
        None => checks.push(Check::error(
            "meta_valid",
            json!({ "error": "meta.json missing or unreadable" }),
            "Re-index: `pitlane index --force <project>`.".to_string(),
        )),
        Some(meta) => {
            if meta.project_path != canonical.display().to_string() {
                checks.push(Check::warn(
                    "meta_valid",
                    json!({
                        "meta_project_path": meta.project_path,
                        "canonical_project_path": canonical.display().to_string(),
                    }),
                    "The index was built from a different path spelling. Re-index from the canonical path.".to_string(),
                ));
            } else {
                checks.push(Check::ok("meta_valid", json!({ "version": meta.version })));
            }
        }
    }

    // 3. Freshness: would a cached (non-forced) index run be used?
    let exclude = meta
        .as_ref()
        .map(|m| m.effective_excludes.clone())
        .unwrap_or_default();
    match (&index, &meta) {
        (Some(_), Some(meta)) => {
            if is_index_up_to_date(&canonical, meta, &exclude) {
                checks.push(Check::ok("index_fresh", json!({ "up_to_date": true })));
            } else {
                checks.push(Check::warn(
                    "index_fresh",
                    json!({ "up_to_date": false }),
                    "Source files changed since the last index. Re-index: `pitlane index --force <project>` (or rely on the watcher / ensure_project_ready).".to_string(),
                ));
            }
        }
        _ => checks.push(Check::info(
            "index_fresh",
            json!({ "skipped": "index or meta unavailable" }),
        )),
    }

    // 4. Effective exclusion policy parses.
    if Indexer::build_exclude_set(&exclude).is_ok() {
        checks.push(Check::ok(
            "excludes_valid",
            json!({ "pattern_count": exclude.len() }),
        ));
    } else {
        checks.push(Check::error(
            "excludes_valid",
            json!({ "pattern_count": exclude.len() }),
            "Persisted exclusion patterns no longer parse. Re-index with an explicit, valid `exclude` list.".to_string(),
        ));
    }

    // 5. Oversized source files (skipped at indexing time).
    match count_oversized_files(&canonical, &exclude) {
        Ok(0) => checks.push(Check::ok(
            "skipped_files",
            json!({ "oversized": 0, "note": "no supported source files above the 1 MiB indexing limit" }),
        )),
        Ok(count) => checks.push(Check::warn(
            "skipped_files",
            json!({ "oversized": count, "limit_bytes": 1024 * 1024 }),
            format!(
                "{count} supported source file(s) exceed the 1 MiB limit and are not indexed. Split the files or accept reduced coverage."
            ),
        )),
        Err(err) => checks.push(Check::info(
            "skipped_files",
            json!({ "error": err.to_string() }),
        )),
    }

    // 6. Embeddings: configured? store loads? format compatible? complete?
    let embed_config: Option<EmbedConfig> = EmbedConfig::try_from_env()?;
    match embed_config {
        None => checks.push(Check::info(
            "embeddings",
            json!({ "status": "disabled", "note": "non-semantic tools work without embeddings" }),
        )),
        Some(ref cfg) => {
            let store_path = idx_dir.join("embeddings.bin");
            let store = EmbedStore::load(&store_path);
            match store {
                Err(err) => {
                    checks.push(Check::warn(
                        "embeddings",
                        json!({ "status": "unreadable", "error": err.to_string() }),
                        "The embedding store failed to load (likely a format change). It will be rebuilt automatically on the next embedding run; or trigger it via wait_for_embeddings.".to_string(),
                    ));
                }
                Ok(store) => {
                    let fingerprint = document_fingerprint(&cfg.model);
                    let endpoint = endpoint_fingerprint(&cfg.url, &cfg.headers);
                    let store_metadata = crate::embed::store::EmbedStoreMetadata::load(&store_path)
                        .ok()
                        .flatten();
                    if let Some(meta) = store_metadata {
                        if !meta.is_compatible(&cfg.model, &fingerprint, &endpoint) {
                            checks.push(Check::warn(
                                "embeddings",
                                json!({
                                    "status": "incompatible",
                                    "store_format_version": meta.format_version,
                                    "current_format_version": crate::embed::document::DOCUMENT_FORMAT_VERSION,
                                }),
                                "Embedding store was built with a different model/format. It will be rebuilt automatically; semantic search may degrade until then.".to_string(),
                            ));
                        } else {
                            let stored = store.vectors.len();
                            let total = index.as_ref().map(|i| i.symbol_count()).unwrap_or(stored);
                            let pct = if total == 0 {
                                100.0
                            } else {
                                (stored as f64 / total as f64 * 100.0).min(100.0)
                            };
                            if stored >= total {
                                checks.push(Check::ok(
                                    "embeddings",
                                    json!({
                                        "status": "complete",
                                        "stored": stored,
                                        "total": total,
                                    }),
                                ));
                            } else {
                                checks.push(Check::warn(
                                    "embeddings",
                                    json!({
                                        "status": "incomplete",
                                        "stored": stored,
                                        "total": total,
                                        "percent": (pct * 100.0).round() / 100.0,
                                    }),
                                    "Embeddings are incomplete. Call wait_for_embeddings, or run with repair=true to kick off background embedding.".to_string(),
                                ));
                            }
                        }
                    } else {
                        checks.push(Check::warn(
                            "embeddings",
                            json!({ "status": "missing_metadata" }),
                            "Embedding store has no metadata; it will be rebuilt on the next embedding run.".to_string(),
                        ));
                    }
                }
            }
        }
    }

    // 7. Change log health (issue #82): can clients get a complete feed?
    if let Some(meta) = &meta {
        checks.push(Check::ok(
            "change_log",
            json!({
                "revision": meta.revision,
                "entries": meta.change_log.len(),
            }),
        ));
    }

    // Repairs.
    let mut repairs_applied: Vec<Value> = Vec::new();
    if params.repair.unwrap_or(false) {
        let stale = matches!(
            checks
                .iter()
                .find(|c| c.name == "index_fresh")
                .map(|c| c.status),
            Some("warn")
        );
        if stale {
            let result = index_project(IndexProjectParams {
                path: canonical.display().to_string(),
                exclude: None,
                force: Some(true),
                max_files: None,
                progress_token: None,
                peer: None,
                embed_config: None,
            })
            .await?;
            repairs_applied.push(json!({
                "repair": "reindex",
                "status": result["status"],
                "symbol_count": result["symbol_count"],
            }));
        }
        // Incomplete embeddings: reuse the normal index path, which kicks off
        // background embedding for a missing/incomplete store.
        if checks
            .iter()
            .any(|c| c.name == "embeddings" && c.status == "warn")
        {
            if let Some(cfg) = embed_config {
                let result = index_project(IndexProjectParams {
                    path: canonical.display().to_string(),
                    exclude: None,
                    force: Some(false),
                    max_files: None,
                    progress_token: None,
                    peer: None,
                    embed_config: Some(std::sync::Arc::new(cfg)),
                })
                .await?;
                repairs_applied.push(json!({
                    "repair": "reembed",
                    "index_status": result["status"],
                    "embeddings": result["embeddings"],
                }));
            }
        }
    }

    // If a re-index repair ran, re-derive freshness from the new meta so the
    // reported checks reflect the post-repair state.
    if repairs_applied.iter().any(|r| r["repair"] == "reindex") {
        if let Ok(meta) = load_meta(&idx_dir.join("meta.json")) {
            let up_to_date = is_index_up_to_date(&canonical, &meta, &meta.effective_excludes);
            if let Some(check) = checks.iter_mut().find(|c| c.name == "index_fresh") {
                check.status = if up_to_date { "ok" } else { "warn" };
                check.detail = json!({ "up_to_date": up_to_date });
            }
        }
    }

    let healthy = checks
        .iter()
        .all(|c| c.status != "error" && c.status != "warn");

    Ok(json!({
        "project": canonical.display().to_string(),
        "healthy": healthy,
        "checks": checks.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        "repairs_applied": repairs_applied,
        "note": "Parse failures are not persisted historically; doctor re-derives structural diagnostics (oversized files, exclusion validity, store compatibility) at call time. Repairs only touch rebuildable artifacts (index, embeddings), never user files.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn index(dir: &TempDir) -> String {
        index_project(IndexProjectParams {
            path: dir.path().to_string_lossy().to_string(),
            exclude: None,
            force: Some(true),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_healthy_project_reports_ok() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        let project = index(&dir).await;

        let result = doctor(DoctorParams {
            project,
            repair: None,
        })
        .await
        .unwrap();

        assert_eq!(result["healthy"], json!(true));
        let checks = result["checks"].as_array().unwrap();
        assert!(checks
            .iter()
            .any(|c| c["check"] == "index_loads" && c["status"] == "ok"));
        assert!(checks
            .iter()
            .any(|c| c["check"] == "meta_valid" && c["status"] == "ok"));
        assert!(checks
            .iter()
            .any(|c| c["check"] == "index_fresh" && c["status"] == "ok"));
        assert!(checks
            .iter()
            .any(|c| c["check"] == "embeddings" && c["status"] == "info"));
    }

    #[tokio::test]
    async fn test_unindexed_project_reports_error_with_repair_hint() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();

        let result = doctor(DoctorParams {
            project: dir.path().to_string_lossy().to_string(),
            repair: None,
        })
        .await
        .unwrap();

        assert_eq!(result["healthy"], json!(false));
        let loads = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "index_loads")
            .unwrap();
        assert_eq!(loads["status"], "error");
        assert!(loads["repair"].is_string());
    }

    #[tokio::test]
    async fn test_stale_index_flagged_and_repaired() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        let project = index(&dir).await;

        // Edit a source file so mtimes diverge from the stored snapshot.
        std::fs::write(dir.path().join("lib.rs"), b"pub fn changed() {}\n").unwrap();
        // Ensure the mtime actually differs.
        std::fs::write(dir.path().join("extra.rs"), b"pub fn extra() {}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let result = doctor(DoctorParams {
            project: project.clone(),
            repair: None,
        })
        .await
        .unwrap();
        assert_eq!(result["healthy"], json!(false));
        let fresh = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "index_fresh")
            .unwrap();
        assert_eq!(fresh["status"], "warn");

        // repair=true re-indexes and restores freshness.
        let repaired = doctor(DoctorParams {
            project,
            repair: Some(true),
        })
        .await
        .unwrap();
        assert!(repaired["repairs_applied"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["repair"] == "reindex"));
        let fresh = repaired["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "index_fresh")
            .unwrap();
        assert_eq!(fresh["status"], "ok", "fresh={fresh}");
    }

    #[tokio::test]
    async fn test_oversized_files_counted() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        // 1.1 MiB supported file → skipped by the indexer.
        let big = vec![b'x'; 1024 * 1024 + 1024];
        std::fs::write(dir.path().join("huge.rs"), &big).unwrap();
        let project = index(&dir).await;

        let result = doctor(DoctorParams {
            project,
            repair: None,
        })
        .await
        .unwrap();

        let skipped = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "skipped_files")
            .unwrap();
        assert_eq!(skipped["detail"]["oversized"], json!(1));
        assert_eq!(skipped["status"], "warn");
    }

    #[tokio::test]
    async fn test_change_log_check_present() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}\n").unwrap();
        let project = index(&dir).await;

        let result = doctor(DoctorParams {
            project,
            repair: None,
        })
        .await
        .unwrap();
        let log = result["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "change_log")
            .unwrap();
        assert_eq!(log["detail"]["revision"], json!(1));
        assert_eq!(log["detail"]["entries"], json!(1));
    }
}
