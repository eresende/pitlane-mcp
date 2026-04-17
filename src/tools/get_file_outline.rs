use serde_json::{json, Value};

use crate::path_policy::{resolve_project_file, resolve_project_path};
use crate::session;
use crate::tools::index_project::load_project_index;

pub struct GetFileOutlineParams {
    pub project: String,
    pub file_path: String,
}

pub async fn get_file_outline(params: GetFileOutlineParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let project_path = resolve_project_path(&params.project)?;
    let abs_file_path = resolve_project_file(&project_path, &params.file_path)?;

    // Find symbols for this file
    let file_ids = index.by_file.get(&abs_file_path);

    let mut symbols: Vec<Value> = match file_ids {
        None => {
            // Try to find by partial match
            let query_lower = params.file_path.to_lowercase();
            let mut found = Vec::new();
            for (file, ids) in &index.by_file {
                let file_str = file.to_string_lossy().to_lowercase();
                if file_str.ends_with(&query_lower) || file_str.contains(&query_lower) {
                    for id in ids {
                        if let Some(sym) = index.symbols.get(id) {
                            found.push(json!({
                                "id": sym.id,
                                "name": sym.name,
                                "qualified": sym.qualified,
                                "kind": sym.kind.to_string(),
                                "line_start": sym.line_start,
                                "line_end": sym.line_end,
                                "signature": sym.signature,
                            }));
                        }
                    }
                    break;
                }
            }
            found
        }
        Some(ids) => ids
            .iter()
            .filter_map(|id| index.symbols.get(id))
            .map(|sym| {
                json!({
                    "id": sym.id,
                    "name": sym.name,
                    "qualified": sym.qualified,
                    "kind": sym.kind.to_string(),
                    "line_start": sym.line_start,
                    "line_end": sym.line_end,
                    "signature": sym.signature,
                })
            })
            .collect(),
    };

    // Sort by line_start
    symbols.sort_by_key(|s| s["line_start"].as_u64().unwrap_or(0));

    let mut response = json!({
        "file": params.file_path,
        "symbols": symbols,
        "count": symbols.len(),
    });
    let observation = session::observe_content(
        &project_path,
        "outline",
        &params.file_path,
        &serde_json::to_string(&response["symbols"]).unwrap_or_default(),
    );
    response["content_seen"] = json!(observation.content_seen);
    response["target_seen"] = json!(observation.target_seen);
    response["content_changed"] = json!(observation.changed_since_last_read);

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::tools::index_project::{index_project, IndexProjectParams};

    async fn setup_project(dir: &TempDir) -> String {
        let project = dir.path().to_string_lossy().to_string();
        index_project(IndexProjectParams {
            path: project.clone(),
            exclude: None,
            force: None,
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();
        project
    }

    #[tokio::test]
    async fn test_get_file_outline_absolute_path_rejected() {
        let dir = TempDir::new().unwrap();
        let abs = dir.path().join("lib.rs");
        std::fs::write(&abs, "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let err = get_file_outline(GetFileOutlineParams {
            project,
            file_path: abs.to_string_lossy().to_string(),
        })
        .await
        .unwrap_err();

        let err = err.downcast::<crate::error::ToolError>().unwrap();
        assert!(matches!(err, crate::error::ToolError::AccessDenied { .. }));
    }

    #[tokio::test]
    async fn test_get_file_outline_parent_escape_rejected() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let err = get_file_outline(GetFileOutlineParams {
            project,
            file_path: "../secret.rs".to_string(),
        })
        .await
        .unwrap_err();

        let err = err.downcast::<crate::error::ToolError>().unwrap();
        assert!(matches!(err, crate::error::ToolError::AccessDenied { .. }));
    }

    #[tokio::test]
    async fn test_get_file_outline_marks_content_seen_on_repeat() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let first = get_file_outline(GetFileOutlineParams {
            project: project.clone(),
            file_path: "lib.rs".to_string(),
        })
        .await
        .unwrap();
        let second = get_file_outline(GetFileOutlineParams {
            project,
            file_path: "lib.rs".to_string(),
        })
        .await
        .unwrap();

        assert_eq!(first["content_seen"].as_bool(), Some(false));
        assert_eq!(second["content_seen"].as_bool(), Some(true));
        assert_eq!(second["content_changed"].as_bool(), Some(false));
    }
}
