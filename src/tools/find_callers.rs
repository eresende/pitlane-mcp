use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::graph::collect_incoming_callable_references;
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

pub struct FindCallersParams {
    pub project: String,
    pub symbol_id: String,
    pub scope: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn find_callers(params: FindCallersParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(8);
    let offset = params.offset.unwrap_or(0);

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| ToolError::SymbolNotFound {
            symbol_id: params.symbol_id.clone(),
        })?;

    let project_path = resolve_project_path(&params.project)?;
    let scope_set: Option<GlobSet> = params.scope.as_deref().map(|scope| {
        let mut builder = GlobSetBuilder::new();
        if let Ok(glob) = Glob::new(scope) {
            builder.add(glob);
        }
        builder
            .build()
            .unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
    });

    let mut callers = Vec::new();
    for caller in collect_incoming_callable_references(&index, sym) {
        let Some(candidate) = index.symbols.get(&caller.id) else {
            continue;
        };
        let path = candidate.file.as_ref().as_path();
        if !matches_scope(path, &project_path, scope_set.as_ref()) {
            continue;
        }
        callers.push(json!({
            "edge_kind": "calls",
            "from_id": candidate.id,
            "from_name": candidate.name,
            "from_kind": candidate.kind.to_string(),
            "file": candidate.file.to_string_lossy().replace('\\', "/"),
            "line_start": candidate.line_start,
            "reason": "indexed navigation graph links the caller to the target symbol",
            "evidence": caller.evidence,
            "confidence": caller.confidence,
        }));
    }

    callers.sort_by(|a, b| {
        b["confidence"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["confidence"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a["from_name"].as_str().cmp(&b["from_name"].as_str()))
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line_start"].as_u64().cmp(&b["line_start"].as_u64()))
    });

    let truncated = callers.len() > offset.saturating_add(limit);
    let page: Vec<Value> = callers.into_iter().skip(offset).take(limit).collect();
    let mut response = json!({
        "symbol_id": params.symbol_id,
        "symbol_name": sym.name,
        "callers": page,
        "count": page.len(),
        "truncated": truncated,
    });
    if truncated {
        response["next_page_message"] = json!(format!(
            "More results available. Call again with offset: {}",
            offset + limit
        ));
    }
    let steering = if page.is_empty() {
        build_steering(
            0.32,
            "No direct callers were recovered from the indexed symbol bodies, so this is a weak reverse-graph expansion result."
                .to_string(),
            "search_symbols",
            json!({ "symbol_id": params.symbol_id, "symbol_name": sym.name }),
            take_fallback_candidates(&page),
        )
    } else {
        build_steering(
            0.89,
            "The target symbol appears in indexed caller bodies and resolves to direct callers."
                .to_string(),
            "get_symbol",
            json!({
                "symbol_id": page[0]["from_id"],
                "name": page[0]["from_name"],
                "file": page[0]["file"],
            }),
            take_fallback_candidates(&page),
        )
    };
    attach_steering(&mut response, steering);
    Ok(response)
}

fn matches_scope(path: &Path, project_path: &Path, scope_set: Option<&GlobSet>) -> bool {
    let Some(set) = scope_set else {
        return true;
    };
    // Canonicalize so that symlink-based temp dirs (e.g. /tmp -> /private/tmp on macOS)
    // don't cause strip_prefix to fail against the already-canonicalized project_path.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let rel = canonical
        .strip_prefix(project_path)
        .unwrap_or(canonical.as_path());
    // Normalize to forward slashes so glob patterns like "nested/**" work on Windows
    // where Path separators are backslashes.
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    set.is_match(rel_str.as_str()) || set.is_match(canonical.as_path())
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

    fn symbol_id(project: &str, name: &str) -> String {
        let index = load_project_index(project).unwrap();
        index
            .symbols
            .values()
            .find(|symbol| symbol.name == name)
            .unwrap()
            .id
            .clone()
    }

    #[tokio::test]
    async fn test_find_callers_returns_direct_callers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn leaf() {}\nfn branch() { leaf(); }\nfn sibling() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = find_callers(FindCallersParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "leaf"),
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let callers = result["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 2);
        let names: Vec<&str> = callers
            .iter()
            .map(|caller| caller["from_name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"branch"));
        assert!(names.contains(&"sibling"));
    }

    #[tokio::test]
    async fn test_find_callers_respects_scope() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn leaf() {}\nfn branch() { leaf(); }\n",
        )
        .unwrap();
        std::fs::write(nested.join("extra.rs"), "fn another() { super::leaf(); }\n").unwrap();
        let project = setup_project(&dir).await;

        let result = find_callers(FindCallersParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "leaf"),
            scope: Some("nested/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let callers = result["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["from_name"].as_str().unwrap(), "another");
    }

    #[tokio::test]
    async fn test_find_callers_filters_non_callable_and_low_signal_callers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn helper() {}\nfn build() { helper(); }\nfn branch() { helper(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = find_callers(FindCallersParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "helper"),
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let callers = result["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["from_name"].as_str().unwrap(), "branch");
    }
}
