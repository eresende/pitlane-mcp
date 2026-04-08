use serde_json::{json, Value};

use crate::error::ToolError;
use crate::graph::{collect_direct_callable_references, read_symbol_source};
use crate::tools::index_project::load_project_index;

pub struct FindCalleesParams {
    pub project: String,
    pub symbol_id: String,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn find_callees(params: FindCalleesParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| ToolError::SymbolNotFound {
            symbol_id: params.symbol_id.clone(),
        })?;

    let source_text = read_symbol_source(sym, false)?;
    let callees = collect_direct_callable_references(&index, sym, &source_text);
    let truncated = callees.len() > offset.saturating_add(limit);
    let page: Vec<Value> = callees
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|callee| {
            json!({
                "edge_kind": "calls",
                "from_id": sym.id,
                "to_id": callee.id,
                "to_name": callee.name,
                "to_kind": callee.kind,
                "file": callee.file,
                "line_start": callee.line_start,
                "reason": "identifier appears in source and resolves to an indexed symbol",
            })
        })
        .collect();

    let mut response = json!({
        "symbol_id": params.symbol_id,
        "symbol_name": sym.name,
        "callees": page,
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
    async fn test_find_callees_returns_direct_references() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn leaf() {}\nfn helper() {}\nfn branch() { leaf(); helper(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = find_callees(FindCalleesParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "branch"),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let callees = result["callees"].as_array().unwrap();
        assert_eq!(callees.len(), 2);
        let names: Vec<&str> = callees
            .iter()
            .map(|callee| callee["to_name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"leaf"));
        assert!(names.contains(&"helper"));
    }

    #[tokio::test]
    async fn test_find_callees_paginates_results() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn alpha() {}\nfn bravo() {}\nfn charlie() {}\nfn root() { alpha(); bravo(); charlie(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = find_callees(FindCalleesParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "root"),
            limit: Some(2),
            offset: Some(0),
        })
        .await
        .unwrap();

        assert_eq!(result["count"].as_u64().unwrap(), 2);
        assert!(result["truncated"].as_bool().unwrap());
        assert!(result["next_page_message"]
            .as_str()
            .unwrap()
            .contains("offset: 2"));
    }

    #[tokio::test]
    async fn test_find_callees_filters_low_signal_and_non_callable_symbols() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "struct Error;\nfn build() {}\nfn helper() {}\nfn root() { helper(); build(); let _ = Error; }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = find_callees(FindCalleesParams {
            project: project.clone(),
            symbol_id: symbol_id(&project, "root"),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let callees = result["callees"].as_array().unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["to_name"].as_str().unwrap(), "helper");
    }
}
