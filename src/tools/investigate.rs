//! `investigate` — single-call composite tool that answers a code question.
//!
//! Runs multiple discovery strategies in parallel, deduplicates results,
//! reads the top symbol bodies, and returns a prose answer with code inlined.
//! Designed to collapse 10-20 tool calls into one.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::graph::read_symbol_source;
use crate::index::format::load_project_meta;
use crate::index::repo_profile::profile_entrypoints;
use crate::path_policy::resolve_project_path;
use crate::session;
use crate::tools::index_project::load_project_index;
use crate::tools::orchestrator::{locate_code, LocateCodeParams};
use crate::tools::search_symbols::{search_symbols, SearchSymbolsParams};

const MAX_INLINE_SYMBOLS: usize = 6;
const MAX_INLINE_LINES: usize = 120;

pub struct InvestigateParams {
    pub project: String,
    pub query: String,
    pub language: Option<String>,
    pub scope: Option<String>,
}

/// Split a query into sub-queries that attack the question from different angles.
/// E.g. "How does ripgrep implement gitignore handling?" becomes:
///   - the original query (for locate_code normalization)
///   - extracted key terms for direct symbol search
fn build_discovery_queries(query: &str) -> Vec<String> {
    let mut queries = vec![query.to_string()];

    // Extract words that look like symbol names (CamelCase, snake_case)
    let symbol_terms: Vec<&str> = query
        .split_whitespace()
        .filter(|w| {
            let has_upper = w.chars().any(|c| c.is_uppercase());
            let has_lower = w.chars().any(|c| c.is_lowercase());
            let is_ident = w.chars().all(|c| c.is_alphanumeric() || c == '_');
            (has_upper && has_lower && is_ident)
                || (w.contains('_') && is_ident)
                || w.len() <= 3 && w.chars().all(|c| c.is_uppercase())
        })
        .collect();

    for term in &symbol_terms {
        if !queries.iter().any(|q| q == *term) {
            queries.push(term.to_string());
        }
    }

    // Extract non-stop-word terms for broader search
    let stop: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "have",
        "has", "had", "do", "does", "did", "will", "would", "could", "should",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "and", "but", "or", "if", "how", "what", "where", "when", "why",
        "this", "that", "which", "it", "its", "does", "show", "find",
        "implement", "implementation", "handling", "logic", "main", "the",
        "using", "uses", "used",
    ];
    let key_terms: Vec<&str> = query
        .split_whitespace()
        .filter(|w| w.len() > 3 && !stop.contains(&w.to_lowercase().as_str()))
        .take(4)
        .collect();

    if key_terms.len() >= 2 {
        let combined = key_terms.join(" ");
        if !queries.contains(&combined) {
            queries.push(combined);
        }
    }
    // Also try individual key terms
    for term in &key_terms {
        if !queries.iter().any(|q| q == *term) && !symbol_terms.contains(term) {
            queries.push(term.to_string());
        }
    }

    queries.truncate(5); // Don't do more than 5 discovery queries
    queries
}

pub async fn investigate(params: InvestigateParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;
    let query = params.query.trim().to_string();
    if query.is_empty() {
        return Err(anyhow::anyhow!("query must not be empty"));
    }

    let index = load_project_index(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);

    let discovery_queries = build_discovery_queries(&query);
    let mut discovered_ids: Vec<String> = Vec::new();

    // Phase 1: Run locate_code with the primary query (benefits from normalization).
    if let Ok(locate) = locate_code(LocateCodeParams {
        project: params.project.clone(),
        query: query.clone(),
        intent: None,
        kind: None,
        language: params.language.clone(),
        scope: params.scope.clone(),
        limit: Some(MAX_INLINE_SYMBOLS),
    })
    .await
    {
        if let Some(results) = locate["results"].as_array() {
            for r in results {
                if let Some(id) = r["id"].as_str() {
                    if !discovered_ids.contains(&id.to_string()) {
                        discovered_ids.push(id.to_string());
                    }
                }
            }
        }
    }

    // Phase 2: Run semantic/bm25 search with each sub-query to fill gaps.
    if discovered_ids.len() < MAX_INLINE_SYMBOLS {
        let semantic_cfg = EmbedConfig::from_env().map(Arc::new);
        let mode = if semantic_cfg.is_some() {
            "semantic"
        } else {
            "bm25"
        };

        for sub_query in &discovery_queries {
            if discovered_ids.len() >= MAX_INLINE_SYMBOLS {
                break;
            }
            let remaining = MAX_INLINE_SYMBOLS - discovered_ids.len();
            if let Ok(result) = search_symbols(SearchSymbolsParams {
                project: params.project.clone(),
                query: sub_query.clone(),
                kind: None,
                language: params.language.clone(),
                file: params.scope.clone(),
                limit: Some(remaining.max(2)),
                offset: Some(0),
                mode: Some(mode.to_string()),
                embed_config: semantic_cfg.clone(),
            })
            .await
            {
                if let Some(results) = result["results"].as_array() {
                    for r in results {
                        if let Some(id) = r["id"].as_str() {
                            if !discovered_ids.contains(&id.to_string()) {
                                discovered_ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Phase 3: For struct/class results, also pull their key methods.
    let mut extra_ids: Vec<String> = Vec::new();
    for id in &discovered_ids {
        if let Some(sym) = index.symbols.get(id.as_str()) {
            let is_container = matches!(
                sym.kind,
                crate::indexer::language::SymbolKind::Struct
                    | crate::indexer::language::SymbolKind::Class
                    | crate::indexer::language::SymbolKind::Trait
                    | crate::indexer::language::SymbolKind::Interface
            );
            if is_container {
                // Find the most important methods of this struct
                let prefix = format!("{}::", sym.name);
                let mut methods: Vec<&crate::indexer::language::Symbol> = index
                    .symbols
                    .values()
                    .filter(|s| {
                        s.file == sym.file
                            && s.id != sym.id
                            && s.qualified.starts_with(&prefix)
                            && matches!(
                                s.kind,
                                crate::indexer::language::SymbolKind::Method
                                    | crate::indexer::language::SymbolKind::Function
                            )
                    })
                    .collect();
                // Sort by line number, take first 2 non-trivial methods
                methods.sort_by_key(|m| m.line_start);
                for m in methods.iter().take(2) {
                    let body_lines = (m.line_end - m.line_start) as usize;
                    if body_lines > 3
                        && !discovered_ids.contains(&m.id)
                        && !extra_ids.contains(&m.id)
                    {
                        extra_ids.push(m.id.clone());
                    }
                }
            }
        }
    }
    discovered_ids.extend(extra_ids);
    discovered_ids.truncate(MAX_INLINE_SYMBOLS);

    // Phase 4: Read symbol bodies.
    let mut sections: Vec<String> = Vec::new();
    let mut files_seen: Vec<String> = Vec::new();
    let mut symbols_seen: Vec<Value> = Vec::new();

    for symbol_id in &discovered_ids {
        let Some(sym) = index.symbols.get(symbol_id.as_str()) else {
            continue;
        };

        let file_str = sym.file.to_string_lossy().replace('\\', "/");
        let short_file = file_str
            .rfind("/crates/")
            .or_else(|| file_str.rfind("/src/"))
            .map(|pos| &file_str[pos + 1..])
            .unwrap_or(&file_str);

        let source = match read_symbol_source(sym, false) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let lines: Vec<&str> = source.lines().collect();
        let truncated = lines.len() > MAX_INLINE_LINES;
        let body = if truncated {
            let mut t = lines[..MAX_INLINE_LINES].join("\n");
            t.push_str(&format!(
                "\n// ... ({} more lines)",
                lines.len() - MAX_INLINE_LINES
            ));
            t
        } else {
            source.clone()
        };

        sections.push(format!(
            "### {} `{}` in {}\n```{}\n{}\n```",
            sym.kind, sym.qualified, short_file, sym.language, body,
        ));

        if !files_seen.contains(&file_str) {
            files_seen.push(file_str.clone());
        }
        symbols_seen.push(json!({
            "id": sym.id,
            "name": sym.name,
            "file": file_str,
            "kind": sym.kind.to_string(),
        }));

        session::record_symbol(&canonical, &sym.id, Some(sym.file.as_ref()));
        session::record_file(&canonical, &sym.file);
    }

    // Phase 5: Build prose response.
    let mut answer = String::new();

    if sections.is_empty() {
        answer.push_str(&format!(
            "No relevant symbols found for \"{}\".\n",
            query
        ));
        if let Some(ref profile) = profile {
            let entrypoints = profile_entrypoints(Some(profile));
            if let Some(first) = entrypoints.first() {
                answer.push_str(&format!(
                    "Try a more specific query, or start from the entrypoint: `{}`\n",
                    first
                ));
            }
        }
    } else {
        answer.push_str(&format!(
            "## Investigation: \"{}\"\n\nFound {} relevant symbol(s) across {} file(s).\n\n",
            query,
            sections.len(),
            files_seen.len(),
        ));
        answer.push_str(&sections.join("\n\n"));
        answer.push_str(
            "\n\n---\nThis response contains the source code of the most relevant symbols. \
             You should be able to answer from the code above. \
             If you need more detail on a specific symbol, use read_code_unit(symbol_id=...).",
        );
    }

    session::record_query(&canonical, &query);

    Ok(json!({
        "query": query,
        "answer": answer,
        "symbols_read": symbols_seen.len(),
        "files_covered": files_seen.len(),
    }))
}
