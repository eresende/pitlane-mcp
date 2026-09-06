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
use crate::path_policy::{display_path_relative_to_project, resolve_project_path};
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
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "to",
        "of",
        "in",
        "for",
        "on",
        "with",
        "at",
        "by",
        "from",
        "as",
        "and",
        "but",
        "or",
        "if",
        "how",
        "what",
        "where",
        "when",
        "why",
        "this",
        "that",
        "which",
        "it",
        "its",
        "does",
        "show",
        "find",
        "implement",
        "implementation",
        "handling",
        "logic",
        "main",
        "the",
        "using",
        "uses",
        "used",
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

/// Normalize an investigate query into a stable key for deduplication.
/// Extracts sorted non-stop-words so rephrased questions match.
fn normalize_investigate_key(query: &str) -> String {
    let stop: &[&str] = &[
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "to",
        "of",
        "in",
        "for",
        "on",
        "with",
        "at",
        "by",
        "from",
        "as",
        "and",
        "but",
        "or",
        "if",
        "how",
        "what",
        "where",
        "when",
        "why",
        "this",
        "that",
        "which",
        "it",
        "its",
        "show",
        "find",
        "explain",
        "all",
        "each",
        "every",
        "some",
        "any",
        "no",
        "not",
        "only",
        "implement",
        "implementation",
        "implemented",
        "implements",
        "involved",
        "are",
        "there",
        "does",
        "about",
    ];
    let mut words: Vec<&str> = query
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() > 2 && !stop.contains(&w.to_lowercase().as_str()))
        .collect();
    words.sort();
    words.dedup();
    words.truncate(6);
    let key = words.join(" ");
    if key.is_empty() {
        query.trim().to_lowercase()
    } else {
        key
    }
}

fn investigate_cache_key(
    query: &str,
    language: Option<&str>,
    scope: Option<&str>,
    epoch: u64,
) -> String {
    // Language and scope filters change which symbols an investigation may
    // consider, so they must be part of the key. The epoch changes whenever
    // the index is rebuilt (reindex or watcher update), invalidating answers
    // that may reference stale source.
    let mut key = format!("investigate:v2:{}", epoch);
    if let Some(lang) = language {
        let lang = lang.trim().to_lowercase();
        if !lang.is_empty() {
            key.push_str(";lang=");
            key.push_str(&lang);
        }
    }
    if let Some(scope) = scope {
        let scope = scope.trim();
        if !scope.is_empty() {
            key.push_str(";scope=");
            key.push_str(scope);
        }
    }
    key.push(';');
    key.push_str(&normalize_investigate_key(query));
    key
}

fn mark_investigate_repeated(mut response: Value) -> Value {
    if let Some(obj) = response.as_object_mut() {
        obj.insert("repeated".to_string(), json!(true));
        obj.insert(
            "guidance".to_string(),
            json!(
                "Returning the cached investigation from this session. \
                 Use read_code_unit(symbol_id=...) if you need more detail on a specific symbol."
            ),
        );
    }
    response
}

pub async fn investigate(params: InvestigateParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;
    let query = params.query.trim().to_string();
    if query.is_empty() {
        return Err(anyhow::anyhow!("query must not be empty"));
    }

    let cache_key = investigate_cache_key(
        &query,
        params.language.as_deref(),
        params.scope.as_deref(),
        session::investigate_epoch(&canonical),
    );
    if let Some(cached) = session::get_investigate_cache(&canonical, &cache_key) {
        return Ok(mark_investigate_repeated(cached));
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
        let semantic_cfg = EmbedConfig::try_from_env()?.map(Arc::new);
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
    let mut omitted_ids: Vec<String> = Vec::new();
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
    // Symbols beyond the inline cap are reported as omissions rather than
    // silently dropped, so callers know the answer is not exhaustive.
    if discovered_ids.len() > MAX_INLINE_SYMBOLS {
        omitted_ids.extend(discovered_ids[MAX_INLINE_SYMBOLS..].iter().cloned());
        discovered_ids.truncate(MAX_INLINE_SYMBOLS);
    }

    // Phase 3b: If the query mentions tests, prioritize test functions.
    let query_lower = query.to_lowercase();
    let wants_tests = query_lower.contains("test")
        || query_lower.contains("behavior")
        || query_lower.contains("edge case")
        || query_lower.contains("scenario");

    if wants_tests {
        // Search for test functions related to the query's topic.
        let test_queries = build_discovery_queries(&query);
        let mut test_ids: Vec<String> = Vec::new();

        for sub_query in test_queries.iter().take(3) {
            if test_ids.len() >= MAX_INLINE_SYMBOLS {
                break;
            }
            // Search with file scope restricted to test files
            if let Ok(result) = search_symbols(SearchSymbolsParams {
                project: params.project.clone(),
                query: sub_query.clone(),
                kind: None,
                language: params.language.clone(),
                file: Some("**/tests/**".to_string()),
                limit: Some(MAX_INLINE_SYMBOLS),
                offset: Some(0),
                mode: Some("bm25".to_string()),
                embed_config: None,
            })
            .await
            {
                if let Some(results) = result["results"].as_array() {
                    for r in results {
                        if let Some(id) = r["id"].as_str() {
                            if !test_ids.contains(&id.to_string())
                                && !discovered_ids.contains(&id.to_string())
                            {
                                test_ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Also search in test files within src/ (Rust #[test] modules)
        if test_ids.is_empty() {
            // Look for functions with "test" in their name
            if let Ok(result) = search_symbols(SearchSymbolsParams {
                project: params.project.clone(),
                query: "test".to_string(),
                kind: Some("function".to_string()),
                language: params.language.clone(),
                file: params.scope.clone(),
                limit: Some(MAX_INLINE_SYMBOLS * 2),
                offset: Some(0),
                mode: Some("bm25".to_string()),
                embed_config: None,
            })
            .await
            {
                if let Some(results) = result["results"].as_array() {
                    // Filter to test functions that are relevant to the query topic
                    for r in results {
                        if let Some(name) = r["name"].as_str() {
                            let name_lower = name.to_lowercase();
                            // Check if the test name relates to any key term in the query
                            let key_terms: Vec<&str> = query_lower
                                .split_whitespace()
                                .filter(|w| w.len() > 3)
                                .collect();
                            let relevant = key_terms.iter().any(|term| {
                                name_lower.contains(term) || term.contains(&name_lower)
                            });
                            if relevant {
                                if let Some(id) = r["id"].as_str() {
                                    if !test_ids.contains(&id.to_string())
                                        && !discovered_ids.contains(&id.to_string())
                                    {
                                        test_ids.push(id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !test_ids.is_empty() {
            // Replace some implementation symbols with test symbols
            // Keep at most 2 implementation symbols, fill rest with tests
            if discovered_ids.len() > 2 {
                omitted_ids.extend(discovered_ids[2..].iter().cloned());
                discovered_ids.truncate(2);
            }
            for id in test_ids {
                if discovered_ids.len() >= MAX_INLINE_SYMBOLS {
                    omitted_ids.push(id);
                    continue;
                }
                if !discovered_ids.contains(&id) {
                    discovered_ids.push(id);
                }
            }
        }
    }

    // Phase 4: Read symbol bodies.
    let mut sections: Vec<String> = Vec::new();
    let mut files_seen: Vec<String> = Vec::new();
    let mut symbols_seen: Vec<Value> = Vec::new();
    let mut truncated_symbols: Vec<Value> = Vec::new();
    let mut unreadable_ids: Vec<String> = Vec::new();

    for symbol_id in &discovered_ids {
        let Some(sym) = index.symbols.get(symbol_id.as_str()) else {
            continue;
        };

        let file_str = sym.file.to_string_lossy().replace('\\', "/");
        let short_file = display_path_relative_to_project(&canonical, sym.file.as_ref());

        let source = match read_symbol_source(sym, false) {
            Ok(s) => s,
            Err(_) => {
                unreadable_ids.push(sym.id.clone());
                continue;
            }
        };

        let lines: Vec<&str> = source.lines().collect();
        let total_lines = lines.len();
        let truncated = total_lines > MAX_INLINE_LINES;
        let body = if truncated {
            let mut t = lines[..MAX_INLINE_LINES].join("\n");
            t.push_str(&format!(
                "\n// ... ({} more lines)",
                total_lines - MAX_INLINE_LINES
            ));
            t
        } else {
            source.clone()
        };

        sections.push(format!(
            "### {} `{}` in {} (lines {}-{})\n```{}\n{}\n```",
            sym.kind, sym.qualified, short_file, sym.line_start, sym.line_end, sym.language, body,
        ));

        if truncated {
            truncated_symbols.push(json!({
                "id": sym.id,
                "name": sym.name,
                "lines_shown": MAX_INLINE_LINES,
                "lines_total": total_lines,
            }));
        }

        if !files_seen.contains(&file_str) {
            files_seen.push(file_str.clone());
        }
        symbols_seen.push(json!({
            "id": sym.id,
            "name": sym.name,
            "file": file_str,
            "kind": sym.kind.to_string(),
            "line_start": sym.line_start,
            "line_end": sym.line_end,
            "lines_shown": total_lines.min(MAX_INLINE_LINES),
            "lines_total": total_lines,
        }));

        session::record_symbol(&canonical, &sym.id, Some(sym.file.as_ref()));
        session::record_file(&canonical, &sym.file);
    }

    // Omission metadata: discovered symbols that did not fit inline, plus
    // symbols whose source could not be read.
    let omitted_symbols: Vec<Value> = omitted_ids
        .iter()
        .filter_map(|id| index.symbols.get(id.as_str()))
        .map(|sym| {
            json!({
                "id": sym.id,
                "name": sym.name,
                "file": sym.file.to_string_lossy().replace('\\', "/"),
                "kind": sym.kind.to_string(),
                "line_start": sym.line_start,
                "reason": "inline symbol cap reached",
            })
        })
        .chain(unreadable_ids.iter().filter_map(|id| {
            index.symbols.get(id.as_str()).map(|sym| {
                json!({
                    "id": sym.id,
                    "name": sym.name,
                    "file": sym.file.to_string_lossy().replace('\\', "/"),
                    "kind": sym.kind.to_string(),
                    "line_start": sym.line_start,
                    "reason": "source could not be read",
                })
            })
        }))
        .collect();

    let is_complete = truncated_symbols.is_empty() && omitted_symbols.is_empty();

    // Phase 5: Build prose response.
    let mut answer = String::new();

    if sections.is_empty() {
        answer.push_str(&format!("No relevant symbols found for \"{}\".\n", query));
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
        answer.push_str("\n\n---\n");
        if is_complete {
            answer.push_str(
                "**IMPORTANT: Answer the user's question NOW from the code above.** \
                 Do NOT call investigate again. Do NOT call locate_code or read_code_unit \
                 unless the code above is clearly insufficient. \
                 The bodies shown above are complete for every symbol included.",
            );
        } else {
            answer.push_str(&format!(
                "**NOTE: This response is capped and may be incomplete.** {} symbol body(s) \
                 were truncated and {} discovered symbol(s) were omitted due to response limits. \
                 The claims below hold only for the lines shown. \
                 Use read_code_unit(symbol_id=...) for any symbol listed in `truncated_symbols` \
                 or `omitted_symbols` before concluding that something is absent.",
                truncated_symbols.len(),
                omitted_symbols.len(),
            ));
        }
    }

    session::record_query(&canonical, &query);

    let response = json!({
        "query": query,
        "answer": answer,
        "symbols_read": symbols_seen.len(),
        "files_covered": files_seen.len(),
        "symbols": symbols_seen,
        "truncated_symbols": truncated_symbols,
        "omitted_symbols": omitted_symbols,
        "complete": is_complete,
        "limits": {
            "max_symbols": MAX_INLINE_SYMBOLS,
            "max_lines_per_symbol": MAX_INLINE_LINES,
        },
        "repeated": false,
    });
    session::store_investigate_cache(&canonical, &cache_key, response.clone());

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::index_project::{index_project, IndexProjectParams};
    use tempfile::TempDir;

    #[test]
    fn test_normalize_investigate_key_dedupes_rephrasings() {
        let a = normalize_investigate_key("How does gitignore handling work?");
        let b = normalize_investigate_key("gitignore handling work");
        assert_eq!(a, b);
    }

    #[test]
    fn test_normalize_investigate_key_falls_back_to_raw_query() {
        let key = normalize_investigate_key("how to");
        assert_eq!(key, "how to");
    }

    async fn setup_project(dir: &TempDir) -> String {
        let project = dir.path().to_string_lossy().to_string();
        index_project(IndexProjectParams {
            path: project.clone(),
            exclude: None,
            force: Some(true),
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
    async fn test_investigate_repeat_returns_cached_answer() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn gitignore_match() {}\npub fn other() {}\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let first = investigate(InvestigateParams {
            project: project.clone(),
            query: "gitignore_match".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(first["repeated"], json!(false));
        assert!(first["answer"]
            .as_str()
            .unwrap()
            .contains("gitignore_match"));

        let second = investigate(InvestigateParams {
            project,
            query: "find gitignore_match".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(second["repeated"], json!(true));
        assert_eq!(second["answer"], first["answer"]);
        assert_eq!(second["symbols_read"], first["symbols_read"]);
        assert!(second["guidance"].as_str().is_some());
    }

    #[test]
    fn test_investigate_cache_key_separates_filters_and_epochs() {
        let base = investigate_cache_key("gitignore", None, None, 0);
        assert!(base.contains("gitignore"));
        // Language filter changes the key.
        assert_ne!(
            investigate_cache_key("gitignore", Some("rust"), None, 0),
            base
        );
        // Scope filter changes the key.
        assert_ne!(
            investigate_cache_key("gitignore", None, Some("src/tools"), 0),
            base
        );
        // A new epoch (index rebuild) changes the key.
        assert_ne!(investigate_cache_key("gitignore", None, None, 1), base);
        // Filters are normalized so casing/whitespace alone cannot bypass the cache.
        assert_eq!(
            investigate_cache_key("gitignore", Some(" Rust "), None, 0),
            investigate_cache_key("gitignore", Some("rust"), None, 0)
        );
    }

    #[tokio::test]
    async fn test_investigate_cache_distinguishes_language_filter() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("mod.py"), "def alpha():\n    pass\n").unwrap();
        let project = setup_project(&dir).await;

        let unfiltered = investigate(InvestigateParams {
            project: project.clone(),
            query: "alpha".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(unfiltered["repeated"], json!(false));

        // Same query but scoped to Python must not reuse the unfiltered answer.
        let py = investigate(InvestigateParams {
            project: project.clone(),
            query: "alpha".to_string(),
            language: Some("python".to_string()),
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(py["repeated"], json!(false));

        // And the exact same filter still hits the cache.
        let py_repeat = investigate(InvestigateParams {
            project,
            query: "alpha".to_string(),
            language: Some("python".to_string()),
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(py_repeat["repeated"], json!(true));
        assert_eq!(py_repeat["answer"], py["answer"]);
    }

    #[tokio::test]
    async fn test_investigate_cache_invalidated_by_reindex_after_content_change() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn stale_symbol() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let first = investigate(InvestigateParams {
            project: project.clone(),
            query: "stale_symbol".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(first["repeated"], json!(false));

        // Repeating before any change is a cache hit.
        let repeat = investigate(InvestigateParams {
            project: project.clone(),
            query: "stale_symbol".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(repeat["repeated"], json!(true));

        // Edit the source and reindex; the cached answer is now stale and
        // must not be returned.
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn fresh_symbol() {}\npub fn unrelated() {}\n",
        )
        .unwrap();
        setup_project(&dir).await;

        let after = investigate(InvestigateParams {
            project,
            query: "stale_symbol".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(after["repeated"], json!(false));
        assert!(after["answer"].as_str().unwrap().contains("fresh_symbol"));
    }

    #[tokio::test]
    async fn test_investigate_failure_does_not_block_retry() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let err = investigate(InvestigateParams {
            project: project.clone(),
            query: "hello".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("indexed"));

        let ready = setup_project(&dir).await;
        let ok = investigate(InvestigateParams {
            project: ready,
            query: "hello".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();
        assert_eq!(ok["repeated"], json!(false));
        assert!(ok["symbols_read"].as_u64().unwrap_or(0) > 0);
    }

    #[tokio::test]
    async fn test_investigate_reports_line_ranges_and_completeness() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn small_thing() {\n    let x = 1;\n    let _ = x;\n}\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let resp = investigate(InvestigateParams {
            project,
            query: "small_thing".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();

        assert_eq!(resp["complete"], json!(true));
        assert!(resp["truncated_symbols"].as_array().unwrap().is_empty());
        assert!(resp["omitted_symbols"].as_array().unwrap().is_empty());
        assert_eq!(resp["limits"]["max_lines_per_symbol"], json!(120));

        let symbols = resp["symbols"].as_array().unwrap();
        let sym = symbols
            .iter()
            .find(|s| s["name"] == "small_thing")
            .expect("symbol metadata present");
        assert!(sym["line_start"].as_u64().unwrap() >= 1);
        assert!(sym["line_end"].as_u64().unwrap() >= sym["line_start"].as_u64().unwrap());
        assert_eq!(sym["lines_total"], json!(4));
        assert_eq!(sym["lines_shown"], json!(4));
    }

    #[tokio::test]
    async fn test_investigate_flags_truncated_bodies_instead_of_claiming_completeness() {
        // One symbol with far more than MAX_INLINE_LINES (120) lines of body.
        let mut body = String::from("pub fn very_long_function() {\n");
        for i in 0..200 {
            body.push_str(&format!("    let _v{i} = {i}; // padding line\n"));
        }
        body.push_str("}\n");
        body.push_str("pub fn tiny() {}\n");

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), body).unwrap();
        let project = setup_project(&dir).await;

        let resp = investigate(InvestigateParams {
            project,
            query: "very_long_function".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();

        assert_eq!(resp["complete"], json!(false));

        let truncated = resp["truncated_symbols"].as_array().unwrap();
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0]["name"], "very_long_function");
        assert_eq!(truncated[0]["lines_shown"], json!(120));
        assert_eq!(truncated[0]["lines_total"], json!(202));

        let answer = resp["answer"].as_str().unwrap();
        assert!(
            answer.contains("capped and may be incomplete"),
            "answer must not claim completeness when source was truncated"
        );
        assert!(!answer.contains("complete relevant implementation"));
        assert!(answer.contains("read_code_unit"));

        // The inlined symbol metadata still preserves line ranges.
        let symbols = resp["symbols"].as_array().unwrap();
        let sym = symbols
            .iter()
            .find(|s| s["name"] == "very_long_function")
            .expect("truncated symbol present in symbols");
        assert_eq!(sym["lines_shown"], json!(120));
        assert_eq!(sym["lines_total"], json!(202));
    }

    #[tokio::test]
    async fn test_investigate_reports_omitted_symbols_beyond_inline_cap() {
        // A container struct pulls in its key methods as extras; when the
        // inline cap (6) is exceeded, the dropped symbols must be reported
        // as omissions instead of silently disappearing.
        let mut src = String::from("pub struct Widget;\n\nimpl Widget {\n");
        for name in [
            "widget_alpha",
            "widget_beta",
            "widget_gamma",
            "widget_delta",
            "widget_epsilon",
            "widget_zeta",
        ] {
            src.push_str(&format!(
                "    pub fn {name}(&self) {{\n        let a = 1;\n        let b = 2;\n        let _ = (a, b);\n    }}\n\n"
            ));
        }
        src.push_str("}\n\n");
        for name in ["widget_one", "widget_two", "widget_three", "widget_four"] {
            src.push_str(&format!("pub fn {name}() {{}}\n"));
        }

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), src).unwrap();
        let project = setup_project(&dir).await;

        let resp = investigate(InvestigateParams {
            project,
            query: "widget".to_string(),
            language: None,
            scope: None,
        })
        .await
        .unwrap();

        assert_eq!(resp["symbols_read"].as_u64().unwrap(), 6);
        let omitted = resp["omitted_symbols"].as_array().unwrap();
        assert!(
            !omitted.is_empty(),
            "dropped symbols must be reported as omissions, resp={resp}"
        );
        assert_eq!(resp["complete"], json!(false));
        // Every omission carries an actionable pointer.
        for entry in omitted {
            assert!(entry["id"].as_str().unwrap().contains("::"));
            assert!(entry["reason"].as_str().is_some());
        }
    }
}
