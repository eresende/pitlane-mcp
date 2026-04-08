use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};

use crate::error::ToolError;
use crate::graph::{
    collect_direct_callable_references, is_callable_kind, is_low_signal_name, read_symbol_source,
};
use crate::tools::index_project::load_project_index;

pub struct FindCallersParams {
    pub project: String,
    pub symbol_id: String,
    pub scope: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn find_callers(params: FindCallersParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| ToolError::SymbolNotFound {
            symbol_id: params.symbol_id.clone(),
        })?;

    let project_path = Path::new(&params.project)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&params.project).to_path_buf());
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
    for candidate in index.symbols.values() {
        if candidate.id == sym.id {
            continue;
        }
        if !is_callable_kind(&candidate.kind) || is_low_signal_name(&candidate.name) {
            continue;
        }

        let path = candidate.file.as_ref().as_path();
        if !matches_scope(path, &project_path, scope_set.as_ref()) {
            continue;
        }

        let source_text = match read_symbol_source(candidate, false) {
            Ok(source) => source,
            Err(_) => continue,
        };
        if !source_text.contains(sym.name.as_str()) {
            continue;
        }

        let direct_refs = collect_direct_callable_references(&index, candidate, &source_text);
        if direct_refs.iter().any(|reference| reference.id == sym.id) {
            callers.push(json!({
                "edge_kind": "calls",
                "from_id": candidate.id,
                "from_name": candidate.name,
                "from_kind": candidate.kind.to_string(),
                "file": candidate.file.to_string_lossy().replace('\\', "/"),
                "line_start": candidate.line_start,
                "reason": "indexed symbol source references the target symbol",
            }));
        }
    }

    callers.sort_by(|a, b| {
        a["from_name"]
            .as_str()
            .cmp(&b["from_name"].as_str())
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
    Ok(response)
}

fn matches_scope(path: &Path, project_path: &Path, scope_set: Option<&GlobSet>) -> bool {
    let Some(set) = scope_set else {
        return true;
    };
    let rel = path.strip_prefix(project_path).unwrap_or(path);
    set.is_match(rel) || set.is_match(path)
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
