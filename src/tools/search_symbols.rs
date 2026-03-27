use serde_json::{Value, json};
use std::str::FromStr;

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
        .map(|k| SymbolKind::from_str(k))
        .transpose()?;

    let lang_filter: Option<Language> = params.language.as_deref().map(|l| match l.to_lowercase().as_str() {
        "rust" => Ok(Language::Rust),
        "python" => Ok(Language::Python),
        other => Err(anyhow::anyhow!("Unknown language: {}", other)),
    }).transpose()?;

    // File glob filter
    let file_glob = params.file.as_deref().map(|f| {
        globset::GlobBuilder::new(f)
            .case_insensitive(true)
            .build()
            .map(|g| g.compile_matcher())
    }).transpose()?;

    let mut results = Vec::new();

    for (_, sym) in &index.symbols {
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
