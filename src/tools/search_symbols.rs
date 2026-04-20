use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::embed::client::{cosine_similarity, EmbedClient};
use crate::embed::store::EmbedStore;
use crate::embed::EmbedConfig;
use crate::error::ToolError;
use crate::index::format::index_dir;
use crate::index::format::load_project_meta;
use crate::index::repo_profile::{archetype_label, role_boost_for_path, role_by_path, role_label};
use crate::indexer::language::{Language, Symbol, SymbolKind};
use crate::path_policy::resolve_project_path;
use crate::session;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

const DEFAULT_LIMIT: usize = 8;

/// Build the set of character trigrams for `s` (lowercased).
fn trigrams(s: &str) -> HashSet<[char; 3]> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return HashSet::new();
    }
    chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

/// Jaccard similarity over trigrams: |A ∩ B| / |A ∪ B|. Returns 0.0 for very short strings.
fn trigram_similarity(a: &str, b: &str) -> f32 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(&tb).count();
    let union = ta.len() + tb.len() - intersection;
    intersection as f32 / union as f32
}

pub struct SearchSymbolsParams {
    pub project: String,
    pub query: String,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// "bm25" (default), "exact", "fuzzy", or "semantic" (reserved)
    pub mode: Option<String>,
    /// Embedding config passed programmatically (not a serde field)
    pub embed_config: Option<Arc<EmbedConfig>>,
}

fn exact_sort_key(sym: &Symbol) -> (String, String, String, u32, u32, String) {
    (
        sym.name.to_lowercase(),
        sym.qualified.to_lowercase(),
        sym.file.to_string_lossy().replace('\\', "/"),
        sym.line_start,
        sym.line_end,
        sym.id.clone(),
    )
}

fn looks_broad_query(query: &str) -> bool {
    let trimmed = query.trim().to_lowercase();
    if trimmed.is_empty() {
        return false;
    }
    let common = [
        "search", "regex", "print", "printer", "walk", "matcher", "matching", "grep", "output",
        "scanner", "scanning",
    ];
    let tokens: Vec<&str> = trimmed
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return false;
    }
    if tokens.len() == 1 {
        return common.contains(&tokens[0]);
    }
    tokens.len() <= 3 && tokens.iter().all(|token| common.contains(token))
}

fn attach_guidance(
    response: &mut Value,
    mode: &str,
    query: &str,
    result_count: usize,
    truncated: bool,
) {
    let mut guidance = json!({
        "next_step": if result_count > 0 {
            "Call get_symbol on the top 1-3 results before launching more discovery searches."
        } else {
            "If no strong symbol matches appear, try one rephrased semantic query or use search_content for a text snippet."
        },
        "avoid": "Avoid running several broad discovery searches in parallel before inspecting the current results."
    });

    if mode == "semantic" && result_count > 0 {
        guidance["semantic_note"] = json!(
            "Semantic results are intended as discovery candidates. Inspect returned symbol IDs with get_symbol before searching again."
        );
    }
    if looks_broad_query(query) {
        guidance["query_hint"] = json!(
            "This query is broad. Prefer a specific intent phrase for semantic search or use search_content when you know a text snippet."
        );
    }
    if truncated {
        guidance["pagination_note"] = json!(
            "Do not fetch more pages until you inspect the current top results with get_symbol."
        );
    }
    response["guidance"] = guidance;
}

fn attach_search_steering(
    response: &mut Value,
    mode: &str,
    query: &str,
    results: &[Value],
    truncated: bool,
) {
    let (confidence, why_this_matched, recommended_next_tool, recommended_target) = if results
        .is_empty()
    {
        (
            0.18,
            "No strong symbol match was found, so this is a weak discovery result.".to_string(),
            if looks_broad_query(query) {
                "search_content"
            } else {
                "search_symbols"
            },
            json!({ "query": query }),
        )
    } else {
        let top = &results[0];
        let target = json!({
            "symbol_id": top["id"],
            "name": top["name"],
            "kind": top["kind"],
            "file": top["file"],
        });
        match mode {
            "exact" => (
                0.98,
                "Exact symbol-name or qualified-name match.".to_string(),
                "get_symbol",
                target,
            ),
            "bm25" => (
                0.87,
                "BM25 ranked the symbol by strong lexical overlap across indexed fields."
                    .to_string(),
                "get_symbol",
                target,
            ),
            "fuzzy" => (
                0.73,
                "Trigram similarity matched a nearby symbol name or path.".to_string(),
                "get_symbol",
                target,
            ),
            "semantic" => (
                0.66,
                "Semantic similarity matched the query intent to a candidate symbol.".to_string(),
                "get_symbol",
                target,
            ),
            _ => (
                0.8,
                "Search returned a plausible symbol candidate.".to_string(),
                "get_symbol",
                target,
            ),
        }
    };

    let mut why = why_this_matched;
    if truncated {
        why.push_str(" Results were truncated; inspect the top candidate before paging further.");
    }

    attach_steering(
        response,
        build_steering(
            confidence,
            why,
            recommended_next_tool,
            recommended_target,
            take_fallback_candidates(results),
        ),
    );
}

fn semantic_query_timeout_ms() -> u64 {
    std::env::var("PITLANE_SEMANTIC_QUERY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000)
}

pub async fn search_symbols(params: SearchSymbolsParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let explicit_mode = params.mode.as_deref();
    let mode = explicit_mode.unwrap_or("bm25");

    // Parse filters — shared across all modes (including semantic).
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
            "svelte" => Ok(Language::Svelte),
            "c" => Ok(Language::C),
            "cpp" | "c++" => Ok(Language::Cpp),
            "go" => Ok(Language::Go),
            "java" => Ok(Language::Java),
            "bash" | "sh" => Ok(Language::Bash),
            "csharp" | "c#" | "cs" => Ok(Language::CSharp),
            "ruby" | "rb" => Ok(Language::Ruby),
            "swift" => Ok(Language::Swift),
            "objc" | "objective-c" | "objectivec" => Ok(Language::ObjC),
            "kotlin" | "kt" => Ok(Language::Kotlin),
            "php" => Ok(Language::Php),
            "zig" => Ok(Language::Zig),
            "luau" | "lua" => Ok(Language::Lua),
            "solidity" | "sol" => Ok(Language::Solidity),
            other => Err(ToolError::InvalidArgument {
                param: "language".to_string(),
                message: format!(
                    "Unknown language '{}'. Supported: rust, python, javascript, typescript, svelte, c, cpp, go, java, bash, csharp, ruby, swift, objc, php, zig, kotlin, lua, solidity",
                    other
                ),
            }),
        })
        .transpose()?;

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

    match mode {
        "exact" | "fuzzy" | "bm25" => {}
        "semantic" => {
            // Sub-task 1: require embed_config
            let embed_cfg = params.embed_config.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Semantic search requires PITLANE_EMBED_URL and PITLANE_EMBED_MODEL to be set"
                )
            })?;

            let canonical = resolve_project_path(&params.project)?;

            // Sub-task 2: load EmbedStore
            let store_path = index_dir(&canonical)?.join("embeddings.bin");
            let store = EmbedStore::load(&store_path).unwrap_or_default();

            // Sub-task 3: error when store is empty
            if store.vectors.is_empty() {
                return Err(anyhow::anyhow!(
                    "No embeddings found for this project. Run index_project first"
                ));
            }

            // Sub-task 4: embed the query
            let client = EmbedClient::new(Arc::clone(embed_cfg));
            let timeout_ms = semantic_query_timeout_ms();
            let query_vec = match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                client.embed_query(&params.query),
            )
            .await
            {
                Ok(Ok(vec)) => vec,
                Ok(Err(err)) => {
                    return Err(anyhow::anyhow!(
                        "Semantic search query embedding failed: {err}. Try mode=\"bm25\" or mode=\"exact\", or increase PITLANE_SEMANTIC_QUERY_TIMEOUT_MS if your embedding backend is slow."
                    ));
                }
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "Semantic search query embedding timed out after {timeout_ms}ms. Try mode=\"bm25\" or mode=\"exact\", or increase PITLANE_SEMANTIC_QUERY_TIMEOUT_MS if your embedding backend is slow."
                    ));
                }
            };

            // Sub-task 5: verify dimension consistency
            if store.dimension() != Some(query_vec.len()) {
                return Err(anyhow::anyhow!(
                    "Embedding dimension mismatch: store has dimension {:?} but query produced dimension {}. Re-run index_project to rebuild embeddings.",
                    store.dimension(),
                    query_vec.len()
                ));
            }

            // Sub-task 6: apply kind/language/file filters
            let mut scored: Vec<(f32, Value)> = index
                .symbols
                .values()
                .filter(|sym| {
                    if let Some(ref kf) = kind_filter {
                        if &sym.kind != kf {
                            return false;
                        }
                    }
                    if let Some(ref lf) = lang_filter {
                        if &sym.language != lf {
                            return false;
                        }
                    }
                    if let Some(ref matcher) = file_glob {
                        let file_str = sym.file.to_string_lossy();
                        let file_path: &Path = file_str.as_ref().as_ref();
                        if !matcher.is_match(file_path) {
                            return false;
                        }
                    }
                    true
                })
                // Sub-task 7: score each symbol present in the store
                .filter_map(|sym| {
                    let vec = store.vectors.get(&sym.id)?;
                    let score = cosine_similarity(&query_vec, vec)
                        + session::symbol_boost(&canonical, &sym.id, Some(sym.file.as_ref()))
                            as f32
                            / 100.0
                        + session::query_boost(&canonical, &params.query) as f32 / 200.0;
                    Some((
                        score,
                        json!({
                            "id": sym.id,
                            "name": sym.name,
                            "qualified": sym.qualified,
                            "kind": sym.kind.to_string(),
                            "language": sym.language.to_string(),
                            "file": sym.file.to_string_lossy().replace('\\', "/"),
                            "line_start": sym.line_start,
                            "line_end": sym.line_end,
                            "signature": sym.signature,
                            "doc": sym.doc,
                        }),
                    ))
                })
                .collect();

            // Sort descending by score
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

            // Sub-task 8: apply offset/limit and return
            let total = scored.len();
            let page: Vec<Value> = scored
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(_, v)| v)
                .collect();

            let truncated = offset + page.len() < total;
            let steering_results = page.clone();
            let mut resp = json!({
                "results": page,
                "count": page.len(),
                "query": params.query,
                "truncated": truncated,
            });
            if truncated {
                resp["next_page_message"] = json!(format!(
                    "More results available. Call again with offset: {}",
                    offset + limit
                ));
            }
            record_search_session(&canonical, &params.query, &steering_results);
            attach_guidance(&mut resp, mode, &params.query, page.len(), truncated);
            attach_search_steering(&mut resp, mode, &params.query, &steering_results, truncated);
            return Ok(resp);
        }
        other => {
            return Err(ToolError::InvalidArgument {
                param: "mode".to_string(),
                message: format!(
                    "Unknown mode '{}'. Supported: bm25, exact, fuzzy, semantic",
                    other
                ),
            }
            .into());
        }
    }

    // ------------------------------------------------------------------
    // BM25 path — ranked full-text search over name, qualified, signature,
    // and doc fields. Falls back silently to exact when tantivy isn't ready
    // (e.g. first call after upgrade) and the mode wasn't set explicitly.
    // ------------------------------------------------------------------
    if mode == "bm25" {
        let bm25_result: anyhow::Result<Value> = (|| {
            let tantivy_dir = index_dir(&canonical)?.join("tantivy");
            crate::index::bm25::ensure(&index.symbols, &tantivy_dir)?;

            let has_glob = file_glob.is_some();
            // Fetch one extra to detect whether more results exist beyond this page.
            let fetch = if has_glob {
                (offset + limit) * 10 + 50
            } else {
                offset + limit + 1
            };

            let ids = crate::index::bm25::search(
                &params.query,
                &canonical,
                &tantivy_dir,
                kind_filter.as_ref(),
                lang_filter.as_ref(),
                fetch.max(1),
            )?;

            let mut results: Vec<(i32, Value)> = Vec::new();
            let mut skipped = 0usize;
            let mut truncated = false;
            for (rank, id) in ids.into_iter().enumerate() {
                let sym = match index.symbols.get(&id) {
                    Some(s) => s,
                    None => continue,
                };
                // Kind/lang filters are already applied inside tantivy; re-check
                // here as a safety net in case the tantivy index is slightly stale.
                if let Some(ref kf) = kind_filter {
                    if &sym.kind != kf {
                        continue;
                    }
                }
                if let Some(ref lf) = lang_filter {
                    if &sym.language != lf {
                        continue;
                    }
                }
                if let Some(ref matcher) = file_glob {
                    let file_str = sym.file.to_string_lossy();
                    let file_path: &Path = file_str.as_ref().as_ref();
                    if !matcher.is_match(file_path) {
                        continue;
                    }
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if results.len() >= limit {
                    truncated = true;
                    break;
                }
                let mut score = 1000_i32 - rank as i32 * 3;
                score += role_boost_for_path(
                    canonical.as_path(),
                    sym.file.as_ref(),
                    profile.as_ref(),
                    &params.query,
                );
                score += session::symbol_boost(&canonical, &sym.id, Some(sym.file.as_ref()));
                results.push((
                    score,
                    json!({
                        "id": sym.id,
                        "name": sym.name,
                        "qualified": sym.qualified,
                        "kind": sym.kind.to_string(),
                        "language": sym.language.to_string(),
                        "file": sym.file.to_string_lossy().replace('\\', "/"),
                        "path_role": role_label(role_by_path(
                            canonical.as_path(),
                            sym.file.as_ref(),
                            profile.as_ref(),
                        )),
                        "line_start": sym.line_start,
                        "line_end": sym.line_end,
                        "signature": sym.signature,
                    }),
                ));
            }

            results.sort_by_key(|r| std::cmp::Reverse(r.0));
            let results: Vec<Value> = results.into_iter().map(|(_, v)| v).collect();

            let mut resp = json!({
                "results": results,
                "count": results.len(),
                "query": params.query,
                "truncated": truncated,
            });
            if let Some(ref profile) = profile {
                resp["repo_profile"] = json!({
                    "archetype": archetype_label(profile.archetype),
                    "entrypoints": profile.entrypoints.clone(),
                });
            }
            let steering_results = results.clone();
            if truncated {
                resp["next_page_message"] = json!(format!(
                    "More results available. Call again with offset: {}",
                    offset + limit
                ));
            }
            record_search_session(&canonical, &params.query, &steering_results);
            attach_guidance(&mut resp, mode, &params.query, results.len(), truncated);
            attach_search_steering(&mut resp, mode, &params.query, &steering_results, truncated);
            Ok(resp)
        })();

        match bm25_result {
            Ok(v) => return Ok(v),
            Err(e) if explicit_mode.is_none() => {
                tracing::debug!(error = %e, "BM25 search failed, falling back to exact");
                // Fall through to exact below.
            }
            Err(e) => return Err(e),
        }
    }

    // ------------------------------------------------------------------
    // Exact path — substring match on name and qualified name.
    // Used directly when mode="exact", and as the BM25 fallback.
    // ------------------------------------------------------------------
    if mode == "exact" || mode == "bm25" {
        let query_lower = params.query.to_lowercase();
        let mut candidates: Vec<&Symbol> = Vec::new();

        for sym in index.symbols.values() {
            let name_lower = sym.name.to_lowercase();
            let qualified_lower = sym.qualified.to_lowercase();
            if !name_lower.contains(&query_lower) && !qualified_lower.contains(&query_lower) {
                continue;
            }
            if let Some(ref kf) = kind_filter {
                if &sym.kind != kf {
                    continue;
                }
            }
            if let Some(ref lf) = lang_filter {
                if &sym.language != lf {
                    continue;
                }
            }
            if let Some(ref matcher) = file_glob {
                let file_str = sym.file.to_string_lossy();
                let file_path: &Path = file_str.as_ref().as_ref();
                if !matcher.is_match(file_path) {
                    continue;
                }
            }
            candidates.push(sym);
        }

        candidates.sort_by(|a, b| {
            let a_boost = profile
                .as_ref()
                .map(|p| role_boost_for_path(&canonical, a.file.as_ref(), Some(p), &params.query))
                .unwrap_or(0);
            let b_boost = profile
                .as_ref()
                .map(|p| role_boost_for_path(&canonical, b.file.as_ref(), Some(p), &params.query))
                .unwrap_or(0);
            let a_session = session::symbol_boost(&canonical, &a.id, Some(a.file.as_ref()))
                + session::query_boost(&canonical, &params.query);
            let b_session = session::symbol_boost(&canonical, &b.id, Some(b.file.as_ref()))
                + session::query_boost(&canonical, &params.query);
            let a_total = a_boost + a_session;
            let b_total = b_boost + b_session;
            b_total
                .cmp(&a_total)
                .then_with(|| exact_sort_key(a).cmp(&exact_sort_key(b)))
        });

        let truncated = candidates.len() > offset + limit;
        let page: Vec<_> = candidates
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|sym| {
                json!({
                    "id": sym.id,
                    "name": sym.name,
                    "qualified": sym.qualified,
                    "kind": sym.kind.to_string(),
                    "language": sym.language.to_string(),
                    "file": sym.file.to_string_lossy().replace('\\', "/"),
                    "path_role": role_label(role_by_path(
                        canonical.as_path(),
                        sym.file.as_ref(),
                        profile.as_ref(),
                    )),
                    "line_start": sym.line_start,
                    "line_end": sym.line_end,
                    "signature": sym.signature,
                })
            })
            .collect();
        let steering_results = page.clone();
        let mut resp = json!({
            "results": page,
            "count": page.len(),
            "query": params.query,
            "truncated": truncated,
        });
        if let Some(ref profile) = profile {
            resp["repo_profile"] = json!({
                "archetype": archetype_label(profile.archetype),
                "entrypoints": profile.entrypoints.clone(),
            });
        }
        if truncated {
            resp["next_page_message"] = json!(format!(
                "More results available. Call again with offset: {}",
                offset + limit
            ));
        }
        record_search_session(&canonical, &params.query, &steering_results);
        attach_guidance(&mut resp, mode, &params.query, page.len(), truncated);
        attach_search_steering(&mut resp, mode, &params.query, &steering_results, truncated);
        return Ok(resp);
    }

    // ------------------------------------------------------------------
    // Fuzzy path — trigram Jaccard similarity, ranked by score.
    // Explicit opt-in via mode="fuzzy".
    // ------------------------------------------------------------------
    let query_lower = params.query.to_lowercase();
    const FUZZY_THRESHOLD: f32 = 0.2;

    let mut scored: Vec<(f32, Value)> = index
        .symbols
        .values()
        .filter(|sym| {
            if let Some(ref kf) = kind_filter {
                if &sym.kind != kf {
                    return false;
                }
            }
            if let Some(ref lf) = lang_filter {
                if &sym.language != lf {
                    return false;
                }
            }
            if let Some(ref matcher) = file_glob {
                let file_str = sym.file.to_string_lossy();
                let file_path: &Path = file_str.as_ref().as_ref();
                if !matcher.is_match(file_path) {
                    return false;
                }
            }
            true
        })
        .filter_map(|sym| {
            let name_lower = sym.name.to_lowercase();
            let qualified_lower = sym.qualified.to_lowercase();
            let base_score = trigram_similarity(&query_lower, &name_lower)
                .max(trigram_similarity(&query_lower, &qualified_lower));
            if base_score >= FUZZY_THRESHOLD {
                let role_boost = profile
                    .as_ref()
                    .map(|p| {
                        role_boost_for_path(&canonical, sym.file.as_ref(), Some(p), &params.query)
                    })
                    .unwrap_or(0) as f32
                    / 100.0;
                let score = base_score
                    + role_boost
                    + session::symbol_boost(&canonical, &sym.id, Some(sym.file.as_ref())) as f32
                        / 100.0
                    + session::query_boost(&canonical, &params.query) as f32 / 200.0;
                Some((
                    score,
                    json!({
                        "id": sym.id,
                        "name": sym.name,
                        "qualified": sym.qualified,
                        "kind": sym.kind.to_string(),
                        "language": sym.language.to_string(),
                        "file": sym.file.to_string_lossy().replace('\\', "/"),
                        "path_role": role_label(role_by_path(
                            canonical.as_path(),
                            sym.file.as_ref(),
                            profile.as_ref(),
                        )),
                        "line_start": sym.line_start,
                        "line_end": sym.line_end,
                        "signature": sym.signature,
                    }),
                ))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let page: Vec<_> = scored
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, v)| v)
        .collect();
    let steering_results = page.clone();

    let mut resp = json!({
        "results": page,
        "count": page.len(),
        "query": params.query,
        "truncated": false,
        "fuzzy": true,
        "fuzzy_note": "Results are fuzzy-matched by trigram similarity and ranked by score.",
    });
    if let Some(ref profile) = profile {
        resp["repo_profile"] = json!({
            "archetype": archetype_label(profile.archetype),
            "entrypoints": profile.entrypoints.clone(),
        });
    }
    record_search_session(&canonical, &params.query, &steering_results);
    attach_guidance(&mut resp, mode, &params.query, page.len(), false);
    attach_search_steering(&mut resp, mode, &params.query, &steering_results, false);
    Ok(resp)
}

fn record_search_session(project: &Path, query: &str, results: &[Value]) {
    session::record_query(project, query);
    session::record_files(
        project,
        results
            .iter()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        project,
        results.iter().filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let file = item["file"].as_str().map(ToOwned::to_owned);
            Some((id, file))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::store::EmbedStore;
    use crate::embed::EmbedConfig;
    use crate::index::format::{build_index_meta, index_dir, save_index, save_meta};
    use crate::index::SymbolIndex;
    use crate::indexer::language::{Language, Symbol, SymbolKind};
    use crate::indexer::{registry, Indexer};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper: write a simple Rust file, index it, and save the index to disk.
    /// Returns the project path string.
    fn setup_indexed_project(dir: &TempDir) -> String {
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let mut meta = build_index_meta(&canonical, &index);
        meta.file_mtimes.insert(
            "lib.rs".to_string(),
            std::fs::metadata(dir.path().join("lib.rs"))
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        save_meta(&meta, &idx_dir.join("meta.json")).unwrap();
        // Invalidate cache so load_project_index reads from disk.
        crate::cache::invalidate(&canonical);
        dir.path().to_string_lossy().to_string()
    }

    fn write_index_with_symbols(dir: &TempDir, symbols: Vec<Symbol>) -> String {
        let mut index = SymbolIndex::new();
        for symbol in symbols {
            index.insert(symbol);
        }
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let meta = build_index_meta(&canonical, &index);
        save_meta(&meta, &idx_dir.join("meta.json")).unwrap();
        crate::cache::invalidate(&canonical);
        dir.path().to_string_lossy().to_string()
    }

    fn make_symbol(id: &str, name: &str, file: &str, line_start: u32) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            qualified: format!("crate::{name}"),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::new(PathBuf::from(file)),
            byte_start: 0,
            byte_end: 0,
            line_start,
            line_end: line_start,
            signature: Some(format!("fn {name}()")),
            doc: None,
        }
    }

    /// Requirements 5.3: semantic search with embed_config=None returns the exact error message.
    #[tokio::test]
    async fn test_semantic_disabled_returns_exact_error() {
        let dir = TempDir::new().unwrap();
        let project = setup_indexed_project(&dir);

        let params = SearchSymbolsParams {
            project,
            query: "hello".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
            mode: Some("semantic".to_string()),
            embed_config: None,
        };

        let err = search_symbols(params).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Semantic search requires PITLANE_EMBED_URL and PITLANE_EMBED_MODEL to be set",
            "error message must match Requirement 5.3 exactly"
        );
    }

    /// Requirements 5.4: semantic search with a valid embed_config but no embeddings.bin
    /// (empty store) returns the exact error message.
    #[tokio::test]
    async fn test_semantic_missing_store_returns_exact_error() {
        let dir = TempDir::new().unwrap();
        let project = setup_indexed_project(&dir);

        // Provide a valid embed_config but do NOT write any embeddings.bin.
        let embed_config = Some(Arc::new(EmbedConfig {
            url: "http://localhost:11434/api/embeddings".to_string(),
            model: "nomic-embed-text".to_string(),
        }));

        let params = SearchSymbolsParams {
            project,
            query: "hello".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
            mode: Some("semantic".to_string()),
            embed_config,
        };

        let err = search_symbols(params).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "No embeddings found for this project. Run index_project first",
            "error message must match Requirement 5.4 exactly"
        );
    }

    // Feature: ollama-lmstudio-embeddings, Property 11: Non-semantic search modes are unaffected by embedding state
    // Validates: Requirements 4a.4, 8.1, 8.2
    //
    // For each of exact, fuzzy, and bm25 modes, call search_symbols twice with the same
    // query but different embed_config values (None vs Some pointing to a non-existent server).
    // Both calls must return identical results, proving embed_config has no effect on
    // non-semantic search modes.
    #[tokio::test]
    async fn test_non_semantic_modes_unaffected_by_embedding_state() {
        for mode in &["exact", "fuzzy", "bm25"] {
            let dir = TempDir::new().unwrap();
            let project = setup_indexed_project(&dir);

            // Call 1: embeddings disabled (None)
            let params_no_embed = SearchSymbolsParams {
                project: project.clone(),
                query: "hello".to_string(),
                kind: None,
                language: None,
                file: None,
                limit: None,
                offset: None,
                mode: Some(mode.to_string()),
                embed_config: None,
            };
            let result_no_embed = search_symbols(params_no_embed)
                .await
                .unwrap_or_else(|e| panic!("mode={mode}, embed_config=None failed: {e}"));

            // Call 2: embeddings enabled but pointing to a non-existent server
            // (enabled-but-broken state — the server is unreachable)
            let embed_config_broken = Some(Arc::new(EmbedConfig {
                url: "http://127.0.0.1:19999/api/embeddings".to_string(),
                model: "nomic-embed-text".to_string(),
            }));
            let params_with_embed = SearchSymbolsParams {
                project: project.clone(),
                query: "hello".to_string(),
                kind: None,
                language: None,
                file: None,
                limit: None,
                offset: None,
                mode: Some(mode.to_string()),
                embed_config: embed_config_broken,
            };
            let result_with_embed = search_symbols(params_with_embed)
                .await
                .unwrap_or_else(|e| panic!("mode={mode}, embed_config=Some(broken) failed: {e}"));

            assert_eq!(
                result_no_embed, result_with_embed,
                "mode={mode}: results must be identical regardless of embed_config"
            );
        }
    }

    /// Requirements 6.4: dimension mismatch between the store and the query vector
    /// returns a descriptive error containing "dimension mismatch".
    #[tokio::test]
    async fn test_semantic_dimension_mismatch_returns_descriptive_error() {
        use httpmock::prelude::*;

        let dir = TempDir::new().unwrap();
        let project = setup_indexed_project(&dir);

        // Write an embeddings.bin with vectors of dimension 3.
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        let store_path = idx_dir.join("embeddings.bin");

        let mut store = EmbedStore::new();
        store.update("some::symbol".to_string(), vec![0.1_f32, 0.2_f32, 0.3_f32]);
        store.save(&store_path).unwrap();

        // Mock server returns a vector of dimension 5 (mismatches the store's dim 3).
        let query_vec: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let response_body = serde_json::json!({
            "data": [{ "embedding": query_vec }]
        })
        .to_string();

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let embed_config = Some(Arc::new(EmbedConfig {
            url: server.url("/"),
            model: "test-model".to_string(),
        }));

        let params = SearchSymbolsParams {
            project,
            query: "hello".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
            mode: Some("semantic".to_string()),
            embed_config,
        };

        let err = search_symbols(params).await.unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("dimension mismatch"),
            "error should mention 'dimension mismatch', got: {err}"
        );
    }

    #[tokio::test]
    async fn test_exact_mode_pagination_is_deterministic() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let symbols = vec![
            make_symbol("gamma", "matchGamma", "gamma.rs", 30),
            make_symbol("alpha", "matchAlpha", "alpha.rs", 10),
            make_symbol("beta", "matchBeta", "beta.rs", 20),
        ];

        let project_a = write_index_with_symbols(&dir_a, symbols.clone());
        let mut reversed = symbols;
        reversed.reverse();
        let project_b = write_index_with_symbols(&dir_b, reversed);

        let result_a = search_symbols(SearchSymbolsParams {
            project: project_a,
            query: "match".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: Some(2),
            offset: Some(1),
            mode: Some("exact".to_string()),
            embed_config: None,
        })
        .await
        .unwrap();

        let result_b = search_symbols(SearchSymbolsParams {
            project: project_b,
            query: "match".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: Some(2),
            offset: Some(1),
            mode: Some("exact".to_string()),
            embed_config: None,
        })
        .await
        .unwrap();

        assert_eq!(result_a, result_b);
        assert_eq!(result_a["truncated"], json!(false));
        assert_eq!(result_a["count"], json!(2));
        assert_eq!(result_a["results"][0]["name"], json!("matchBeta"));
        assert_eq!(result_a["results"][1]["name"], json!("matchGamma"));
        assert_eq!(
            result_a["guidance"]["next_step"],
            json!(
                "Call get_symbol on the top 1-3 results before launching more discovery searches."
            )
        );
    }

    #[tokio::test]
    async fn test_exact_mode_prefers_entrypoint_role() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "pub fn hello() {}\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let meta = build_index_meta(&canonical, &index);
        save_meta(&meta, &idx_dir.join("meta.json")).unwrap();
        crate::cache::invalidate(&canonical);

        let result = search_symbols(SearchSymbolsParams {
            project: dir.path().to_string_lossy().to_string(),
            query: "hello".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: Some(2),
            offset: Some(0),
            mode: Some("exact".to_string()),
            embed_config: None,
        })
        .await
        .unwrap();

        assert_eq!(result["results"][0]["path_role"], json!("entrypoint"));
        assert!(result["results"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("main.rs"));
    }

    #[tokio::test]
    async fn test_broad_query_emits_query_hint() {
        let dir = TempDir::new().unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_symbols(SearchSymbolsParams {
            project,
            query: "search".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: Some(5),
            offset: Some(0),
            mode: Some("exact".to_string()),
            embed_config: None,
        })
        .await
        .unwrap();

        assert_eq!(
            result["guidance"]["query_hint"],
            json!("This query is broad. Prefer a specific intent phrase for semantic search or use search_content when you know a text snippet.")
        );
    }
}
