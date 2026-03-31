use serde_json::{json, Value};
use std::str::FromStr;

use crate::error::ToolError;
use crate::indexer::language::{Language, SymbolKind};
use crate::tools::index_project::load_project_index;

pub struct SearchSymbolsParams {
    pub project: String,
    pub query: String,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
}

pub async fn search_symbols(params: SearchSymbolsParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(20);
    let query_lower = params.query.to_lowercase();

    // Parse optional filters
    let kind_filter: Option<SymbolKind> = params
        .kind
        .as_deref()
        .map(|k| {
            SymbolKind::from_str(k).map_err(|_| ToolError::InvalidArgument {
                param: "kind".to_string(),
                message: format!(
                    "Unknown kind '{}'. Supported: function, method, struct, enum, trait, impl, mod, macro, const, type_alias, class, interface",
                    k
                ),
            })
        })
        .transpose()?;

    let lang_filter: Option<Language> = params
        .language
        .as_deref()
        .map(|l| match l.to_lowercase().as_str() {
            "rust" => Ok(Language::Rust),
            "python" => Ok(Language::Python),
            "javascript" | "js" => Ok(Language::JavaScript),
            "typescript" | "ts" => Ok(Language::TypeScript),
            "c" => Ok(Language::C),
            "cpp" | "c++" => Ok(Language::Cpp),
            "go" => Ok(Language::Go),
            "java" => Ok(Language::Java),
            other => Err(ToolError::InvalidArgument {
                param: "language".to_string(),
                message: format!(
                    "Unknown language '{}'. Supported: rust, python, javascript, typescript, c, cpp, go, java",
                    other
                ),
            }),
        })
        .transpose()?;

    // File glob filter
    let file_glob = params
        .file
        .as_deref()
        .map(|f| {
            globset::GlobBuilder::new(f)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()?;

    let mut results = Vec::new();

    for sym in index.symbols.values() {
        // Apply query filter
        let name_lower = sym.name.to_lowercase();
        let qualified_lower = sym.qualified.to_lowercase();
        if !name_lower.contains(&query_lower) && !qualified_lower.contains(&query_lower) {
            continue;
        }

        // Apply kind filter
        if let Some(ref kf) = kind_filter {
            if &sym.kind != kf {
                continue;
            }
        }

        // Apply language filter
        if let Some(ref lf) = lang_filter {
            if &sym.language != lf {
                continue;
            }
        }

        // Apply file filter
        if let Some(ref matcher) = file_glob {
            let file_str = sym.file.to_string_lossy();
            let file_path: &std::path::Path = file_str.as_ref().as_ref();
            if !matcher.is_match(file_path) {
                continue;
            }
        }

        results.push(json!({
            "id": sym.id,
            "name": sym.name,
            "qualified": sym.qualified,
            "kind": sym.kind.to_string(),
            "language": sym.language.to_string(),
            "file": sym.file.display().to_string(),
            "line_start": sym.line_start,
            "line_end": sym.line_end,
            "signature": sym.signature,
        }));

        if results.len() >= limit {
            break;
        }
    }

    Ok(json!({
        "results": results,
        "count": results.len(),
        "query": params.query,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use crate::error::ToolError;
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
    async fn test_invalid_language_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
        let project = setup_project(&dir).await;

        let err = search_symbols(SearchSymbolsParams {
            project,
            query: "foo".to_string(),
            kind: None,
            language: Some("cobol".to_string()),
            file: None,
            limit: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        assert!(tool_err.to_string().contains("language"));
        assert!(tool_err.to_string().contains("cobol"));
    }

    #[tokio::test]
    async fn test_invalid_kind_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
        let project = setup_project(&dir).await;

        let err = search_symbols(SearchSymbolsParams {
            project,
            query: "foo".to_string(),
            kind: Some("widget".to_string()),
            language: None,
            file: None,
            limit: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        assert!(tool_err.to_string().contains("kind"));
        assert!(tool_err.to_string().contains("widget"));
    }

    #[tokio::test]
    async fn test_unindexed_project_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().to_string_lossy().to_string();
        let canonical = dir.path().canonicalize().unwrap();
        crate::cache::invalidate(&canonical);

        let err = search_symbols(SearchSymbolsParams {
            project: project.clone(),
            query: "foo".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "PROJECT_NOT_INDEXED");
        assert_eq!(tool_err.hint(), "Call index_project first.");
    }
}
