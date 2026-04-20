use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::embed::EmbedConfig;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};

use crate::graph::{edge_evidence_quality, navigation_edge_metrics, EdgeRelation};
use crate::index::format::load_project_meta;
use crate::index::repo_profile::{
    compact_repo_map, profile_entrypoints, role_label, summarize_role_counts, RepoProfile,
};
use crate::path_policy::resolve_project_path;
use crate::session;
use crate::tools::get_file_outline::{get_file_outline, GetFileOutlineParams};
use crate::tools::get_lines::{get_lines, GetLinesParams};
use crate::tools::get_project_outline::{get_project_outline, GetProjectOutlineParams};
use crate::tools::get_symbol::{get_symbol, GetSymbolParams};
use crate::tools::index_project::load_project_index;
use crate::tools::search_content::{search_content, SearchContentParams};
use crate::tools::search_files::{search_files, SearchFilesParams};
use crate::tools::search_symbols::{search_symbols, SearchSymbolsParams};
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};
use crate::tools::trace_execution_path::{trace_execution_path, TraceExecutionPathParams};

const READ_CODE_UNIT_FILE_OUTLINE_LIMIT: usize = 12;

/// Maximum number of source lines returned for a single symbol body via
/// `read_code_unit(symbol_id=...)`.  Larger bodies are truncated and the
/// response includes a `body_truncated` flag plus guidance to narrow the read.
const READ_CODE_UNIT_SYMBOL_BODY_LINE_LIMIT: usize = 120;

pub struct LocateCodeParams {
    pub project: String,
    pub query: String,
    pub intent: Option<String>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub scope: Option<String>,
    pub limit: Option<usize>,
}

pub struct ReadCodeUnitParams {
    pub project: String,
    pub symbol_id: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub include_context: Option<bool>,
    pub signature_only: Option<bool>,
}

pub struct TracePathParams {
    pub project: String,
    pub query: String,
    pub source: Option<String>,
    pub sink: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub max_symbols: Option<usize>,
    pub max_depth: Option<usize>,
}

pub struct AnalyzeImpactParams {
    pub project: String,
    pub query: Option<String>,
    pub symbol_id: Option<String>,
    pub file_path: Option<String>,
    pub scope: Option<String>,
    pub depth: Option<usize>,
    pub limit: Option<usize>,
}

pub struct NavigateCodeParams {
    pub project: String,
    pub query: String,
    pub intent: Option<String>,
    pub symbol_id: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub include_context: Option<bool>,
    pub signature_only: Option<bool>,
    pub source: Option<String>,
    pub sink: Option<String>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub scope: Option<String>,
    pub limit: Option<usize>,
    pub max_symbols: Option<usize>,
    pub max_depth: Option<usize>,
    pub depth: Option<usize>,
}

pub async fn locate_code(params: LocateCodeParams) -> anyhow::Result<Value> {
    let query = params.query.trim().to_string();
    if query.is_empty() {
        return Err(anyhow::anyhow!("query must not be empty"));
    }
    let canonical = resolve_project_path(&params.project)?;

    // Load profile for query normalization and reranking.
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);

    // Normalize vague queries before routing.
    let normalized = normalize_locate_query(&query, profile.as_ref());
    let effective_query = normalized.as_deref().unwrap_or(&query);

    let route = choose_locate_route(&params.intent, effective_query);
    let limit = params.limit.unwrap_or(3).clamp(1, 8);
    let mut route_used = route.as_str().to_string();

    // Build a params copy with the effective (possibly normalized) query.
    let effective_params = LocateCodeParams {
        project: params.project.clone(),
        query: effective_query.to_string(),
        intent: params.intent.clone(),
        kind: params.kind.clone(),
        language: params.language.clone(),
        scope: params.scope.clone(),
        limit: params.limit,
    };

    let mut results = match route {
        LocateRoute::Project => locate_project(&effective_params, limit).await?,
        LocateRoute::Files => locate_files(&effective_params, limit).await?,
        LocateRoute::Content => locate_content(&effective_params, limit).await?,
        LocateRoute::Symbols { ref mode } => {
            locate_symbols(&effective_params, limit, mode.as_str()).await?
        }
    };

    if results.is_empty() {
        if let Some(fallback) = fallback_locate_route(&route, effective_query) {
            let fallback_route = fallback.as_str().to_string();
            let fallback_results = match fallback {
                LocateRoute::Project => locate_project(&effective_params, limit).await?,
                LocateRoute::Files => locate_files(&effective_params, limit).await?,
                LocateRoute::Content => locate_content(&effective_params, limit).await?,
                LocateRoute::Symbols { ref mode } => {
                    locate_symbols(&effective_params, limit, mode.as_str()).await?
                }
            };
            if !fallback_results.is_empty() {
                route_used = fallback_route;
                results = fallback_results;
            }
        }
    }

    let _novelty_bias = promote_nearby_unseen_locate_candidate(&mut results, &canonical);

    let mut response = json!({
        "query": query,
        "count": results.len(),
    });
    if normalized.is_some() {
        response["normalized_query"] = json!(effective_query);
    }

    // Build a prose summary the LLM can read directly instead of parsing JSON.
    if results.is_empty() {
        let sharper = suggest_sharper_query(&query, &route_used, profile.as_ref());
        response["summary"] = json!(format!(
            "No results found for \"{}\".{}",
            query,
            sharper.map(|s| format!(" {}", s)).unwrap_or_default()
        ));
    } else {
        let mut summary = String::new();
        summary.push_str(&format!(
            "Found {} result(s) for \"{}\".\n\n",
            results.len(),
            query
        ));
        for (i, r) in results.iter().enumerate() {
            let name = r["name"].as_str().unwrap_or("?");
            let file = r["file"].as_str().unwrap_or("?");
            // Strip the project prefix from file paths for readability
            let short_file = file
                .rfind("/crates/")
                .or_else(|| file.rfind("/src/"))
                .map(|pos| &file[pos + 1..])
                .unwrap_or(file);
            let sig = r["signature"].as_str().unwrap_or("");
            let kind = r["symbol_kind"]
                .as_str()
                .unwrap_or(r["kind"].as_str().unwrap_or("?"));
            let id = r["id"].as_str().unwrap_or("");
            summary.push_str(&format!(
                "{}. {} `{}` in {} — `{}`\n   → read_code_unit(symbol_id=\"{}\")\n",
                i + 1,
                kind,
                name,
                short_file,
                if sig.len() > 100 { &sig[..100] } else { sig },
                id
            ));
        }
        summary.push_str("\nUse read_code_unit with the symbol_id above to inspect any result. Do not use generic file read tools.");
        response["summary"] = json!(summary);
    }

    // Keep structured results for programmatic use but make them compact.
    let compact_results: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r["id"],
                "name": r["name"],
                "file": r["file"],
                "signature": r["signature"],
            })
        })
        .collect();
    response["results"] = json!(compact_results);

    session::record_query(&canonical, &params.query);
    session::record_files(
        &canonical,
        results
            .iter()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        &canonical,
        results.iter().filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let file = item["file"].as_str().map(ToOwned::to_owned);
            Some((id, file))
        }),
    );
    Ok(response)
}

#[allow(dead_code)]
fn build_locate_session_state(
    results: &[Value],
    project_path: &Path,
    novelty_bias: bool,
) -> Option<Value> {
    let top = results.first()?;
    let top_seen = locate_candidate_seen(project_path, top);

    // Only emit session_state when it carries actionable information.
    if !top_seen && !novelty_bias {
        return None;
    }

    let mut state = json!({
        "top_target_seen": top_seen,
    });
    if novelty_bias {
        state["novelty_bias_applied"] = json!(true);
        if let Some(alt) = results.get(1) {
            state["nearby_alternative"] = candidate_target(alt);
        }
    }
    Some(state)
}

fn promote_nearby_unseen_locate_candidate(results: &mut [Value], project_path: &Path) -> bool {
    if results.len() < 2 {
        return false;
    }
    if !locate_candidate_seen(project_path, &results[0]) {
        return false;
    }

    let anchor = results[0].clone();
    let mut best_index = None;
    let mut best_score = i32::MIN;
    for (idx, candidate) in results.iter().enumerate().skip(1) {
        if locate_candidate_seen(project_path, candidate) {
            continue;
        }
        let score = locate_candidate_novelty_score(project_path, candidate, &anchor);
        if score > best_score {
            best_score = score;
            best_index = Some(idx);
        }
    }

    let Some(best_index) = best_index else {
        return false;
    };
    if best_score < 8 {
        return false;
    }

    results[..=best_index].rotate_right(1);
    true
}

fn locate_candidate_seen(project_path: &Path, candidate: &Value) -> bool {
    let file = candidate["file"].as_str().unwrap_or("");
    if let Some(symbol_id) = candidate["id"].as_str() {
        return session::has_seen_symbol(project_path, symbol_id);
    }
    if file.is_empty() {
        return false;
    }
    session::has_seen_file(project_path, Path::new(file))
}

fn locate_candidate_novelty_score(project_path: &Path, candidate: &Value, anchor: &Value) -> i32 {
    let file = candidate["file"].as_str().unwrap_or("");
    let candidate_dir = Path::new(file)
        .parent()
        .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| ".".to_string());
    let anchor_file = anchor["file"].as_str().unwrap_or("");
    let anchor_dir = Path::new(anchor_file)
        .parent()
        .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| ".".to_string());
    let mut score = 0;

    if !candidate_dir.is_empty() && candidate_dir == anchor_dir {
        score += 10;
    }
    if candidate["path_role"].as_str().unwrap_or("") == anchor["path_role"].as_str().unwrap_or("") {
        score += 6;
    }
    score += session::directory_boost(project_path, Path::new(&candidate_dir));
    if let Some(symbol_id) = candidate["id"].as_str() {
        if session::symbol_boost(project_path, symbol_id, Some(Path::new(file))) == 0 {
            score += 4;
        }
    } else if session::file_boost(project_path, Path::new(file)) == 0 {
        score += 3;
    }

    score
}

pub async fn read_code_unit(params: ReadCodeUnitParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;

    if let Some(symbol_id) = params.symbol_id {
        let mut response = get_symbol(GetSymbolParams {
            project: params.project,
            symbol_id,
            include_context: params.include_context,
            signature_only: params.signature_only,
            include_references: None,
        })
        .await?;

        // Cap large symbol bodies to reduce token waste.
        let body_truncated = if let Some(source) = response["source"].as_str() {
            let line_count = source.lines().count();
            if line_count > READ_CODE_UNIT_SYMBOL_BODY_LINE_LIMIT {
                let truncated: String = source
                    .lines()
                    .take(READ_CODE_UNIT_SYMBOL_BODY_LINE_LIMIT)
                    .collect::<Vec<_>>()
                    .join("\n");
                response["source"] = json!(truncated);
                response["body_truncated"] = json!(true);
                response["body_total_lines"] = json!(line_count);
                response["body_returned_lines"] = json!(READ_CODE_UNIT_SYMBOL_BODY_LINE_LIMIT);
                response["guidance"] = json!({
                    "next_step": format!(
                        "This symbol body was truncated from {} to {} lines. Use read_code_unit with file_path and line_start/line_end to read specific sections, or use locate_code to find a narrower symbol within this file.",
                        line_count, READ_CODE_UNIT_SYMBOL_BODY_LINE_LIMIT
                    )
                });
                true
            } else {
                false
            }
        } else {
            false
        };

        let content_seen = response["content_seen"].as_bool();
        let target_seen = response["target_seen"].as_bool();
        let content_changed = response["content_changed"].as_bool();
        attach_read_state(
            &mut response,
            "symbol",
            content_seen,
            target_seen,
            content_changed,
        );

        // If body was truncated, override the steering to discourage broad reads.
        if body_truncated {
            let file = response["file"].as_str().unwrap_or("").to_string();
            attach_steering(
                &mut response,
                build_steering(
                    0.78,
                    "Body truncated. Use locate_code or read_code_unit with line ranges to narrow."
                        .to_string(),
                    "locate_code",
                    json!({ "file_path": file }),
                    Vec::new(),
                ),
            );
        }

        return Ok(response);
    }

    let Some(file_path) = params.file_path else {
        return Err(anyhow::anyhow!(
            "read_code_unit requires symbol_id or file_path"
        ));
    };

    // Reject directory-ish paths that lack a file extension.
    // Benchmark traces showed agents passing paths like "crates/core/flags"
    // which is not the intended usage pattern.
    let file_path_obj = Path::new(&file_path);
    if file_path_obj.extension().is_none() && !file_path.contains('.') {
        let resolved = canonical.join(&file_path);
        if resolved.is_dir() {
            return Err(anyhow::anyhow!(
                "read_code_unit file_path \"{}\" appears to be a directory, not a file. Use locate_code with intent=\"file\" and a query to find files within this directory.",
                file_path
            ));
        }
    }

    if let (Some(line_start), Some(line_end)) = (params.line_start, params.line_end) {
        let file_path_for_record = file_path.clone();
        let response = get_lines(GetLinesParams {
            project: params.project,
            file_path: file_path.clone(),
            line_start,
            line_end,
        })
        .await?;
        let mut response = response;
        let content_seen = response["content_seen"].as_bool();
        let target_seen = response["target_seen"].as_bool();
        let content_changed = response["content_changed"].as_bool();
        attach_read_state(
            &mut response,
            "line_slice",
            content_seen,
            target_seen,
            content_changed,
        );
        session::record_file(&canonical, Path::new(&file_path_for_record));
        return Ok(response);
    }

    if params.line_start.is_some() || params.line_end.is_some() {
        return Err(anyhow::anyhow!(
            "line_start and line_end must either both be set or both be omitted"
        ));
    }

    let mut response = get_file_outline(GetFileOutlineParams {
        project: params.project,
        file_path: file_path.clone(),
    })
    .await?;
    let outline_symbols = response["symbols"].as_array().cloned().unwrap_or_default();
    let compact_symbols =
        compact_outline_symbols(&outline_symbols, READ_CODE_UNIT_FILE_OUTLINE_LIMIT);
    let truncated = compact_symbols.len() < outline_symbols.len();

    response["symbols"] = json!(compact_symbols);
    response["returned_count"] = json!(response["symbols"].as_array().map_or(0, Vec::len));
    response["truncated"] = json!(truncated);

    // Build a prose summary for the file outline.
    let mut summary = format!(
        "File `{}` has {} symbol(s)",
        file_path,
        outline_symbols.len()
    );
    if truncated {
        summary.push_str(&format!(
            " (showing top {} of {})",
            compact_symbols.len(),
            outline_symbols.len()
        ));
    }
    summary.push_str(":\n\n");
    for sym in &compact_symbols {
        let name = sym["name"].as_str().unwrap_or("?");
        let kind = sym["kind"].as_str().unwrap_or("?");
        let id = sym["id"].as_str().unwrap_or("");
        let sig = sym["signature"].as_str().unwrap_or("");
        summary.push_str(&format!(
            "- {} `{}` — `{}`\n  → read_code_unit(symbol_id=\"{}\")\n",
            kind,
            name,
            if sig.len() > 90 { &sig[..90] } else { sig },
            id
        ));
    }
    if truncated {
        summary.push_str("\nUse locate_code to find additional members not shown.");
    }
    response["summary"] = json!(summary);

    // Remove the verbose symbols array — the summary replaces it.
    // Keep a compact version for programmatic use.
    let compact_for_response: Vec<Value> = compact_symbols
        .iter()
        .map(|s| {
            json!({
                "id": s["id"],
                "name": s["name"],
                "kind": s["kind"],
                "signature": s["signature"],
            })
        })
        .collect();
    response["symbols"] = json!(compact_for_response);

    session::record_file(&canonical, Path::new(&file_path));

    Ok(response)
}

fn compact_outline_symbols(symbols: &[Value], limit: usize) -> Vec<Value> {
    if symbols.len() <= limit {
        return symbols.to_vec();
    }

    let mut ranked: Vec<(usize, usize, u64)> = symbols
        .iter()
        .enumerate()
        .map(|(index, symbol)| {
            let kind = symbol["kind"].as_str().unwrap_or_default();
            let rank = match kind {
                "mod" | "struct" | "enum" | "trait" | "class" | "function" | "const"
                | "type_alias" => 0,
                "impl" => 1,
                "method" => 2,
                _ => 3,
            };
            let line_start = symbol["line_start"].as_u64().unwrap_or(u64::MAX);
            (index, rank, line_start)
        })
        .collect();

    ranked.sort_by_key(|(_, rank, line_start)| (*rank, *line_start));

    let mut selected: Vec<Value> = ranked
        .into_iter()
        .take(limit)
        .map(|(index, _, _)| symbols[index].clone())
        .collect();
    selected.sort_by_key(|symbol| symbol["line_start"].as_u64().unwrap_or(u64::MAX));
    selected
}

fn attach_read_state(
    response: &mut Value,
    read_kind: &str,
    content_seen: Option<bool>,
    target_seen: Option<bool>,
    content_changed: Option<bool>,
) {
    let repeated = content_seen.unwrap_or(false);
    let target_seen = target_seen.unwrap_or(repeated);
    let changed = content_changed.unwrap_or(false);
    let status = if repeated {
        "unchanged"
    } else if changed {
        "changed"
    } else {
        "new"
    };
    response["read_state"] = json!({
        "read_kind": read_kind,
        "content_seen": repeated,
        "target_seen": target_seen,
        "changed_since_last_read": changed,
        "repeat_read": repeated,
        "status": status,
        "guidance": if repeated {
            "This code unit was already returned in this session. Prefer expanding to related symbols, usages, or neighboring slices instead of rereading unchanged content."
        } else if changed {
            "This code unit changed since the previous read in this session. Prefer using this updated payload before expanding further."
        } else {
            "This code unit is new in the current session. Reuse this payload before issuing another read for the same target."
        },
    });
}

pub async fn trace_path(params: TracePathParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;
    let mut query = params.query.trim().to_string();
    let source_hint = params.source.clone();
    let sink_hint = params.sink.clone();
    if let Some(source) = params.source.as_deref() {
        if !source.is_empty() {
            query = format!("{source} {query}");
        }
    }
    if let Some(sink) = params.sink.as_deref() {
        if !sink.is_empty() {
            query = format!("{query} {sink}");
        }
    }

    let response = trace_execution_path(TraceExecutionPathParams {
        project: params.project,
        query: query.clone(),
        source: source_hint.clone(),
        sink: sink_hint.clone(),
        language: params.language,
        file: params.file,
        max_symbols: params.max_symbols,
        max_depth: params.max_depth,
        embed_config: None,
    })
    .await?;

    let important_symbols = response["important_symbols"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let edges = response["edges"].as_array().cloned().unwrap_or_default();
    let path_narrative = response["path_narrative"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Build a prose summary the LLM can read directly.
    let mut summary = String::new();

    if important_symbols.is_empty() {
        let profile = load_project_meta(&canonical)
            .ok()
            .map(|meta| meta.repo_profile);
        summary.push_str(&format!("No execution path found for \"{}\".\n", query));
        if let Some(ref profile) = profile {
            let entrypoints = profile_entrypoints(Some(profile));
            if let Some(first) = entrypoints.first() {
                summary.push_str(&format!(
                    "Try starting from the entrypoint: read_code_unit(file_path=\"{}\").\n",
                    first
                ));
            }
        }
        summary.push_str("Use locate_code with a more specific symbol name instead.");
    } else {
        summary.push_str(&format!(
            "Traced {} symbol(s) and {} edge(s) for \"{}\".\n\n",
            important_symbols.len(),
            edges.len(),
            query
        ));

        if !path_narrative.is_empty()
            && path_narrative != "No compact path narrative available from the traced symbols."
        {
            summary.push_str(&format!("Path: {}\n\n", path_narrative));
        }

        summary.push_str("Key symbols along the path:\n");
        for (i, sym) in important_symbols.iter().enumerate() {
            let name = sym["name"].as_str().unwrap_or("?");
            let file = sym["file"].as_str().unwrap_or("?");
            let short_file = file
                .rfind("/crates/")
                .or_else(|| file.rfind("/src/"))
                .map(|pos| &file[pos + 1..])
                .unwrap_or(file);
            let category = sym["category"].as_str().unwrap_or("?");
            let sig = sym["signature"].as_str().unwrap_or("");
            let id = sym["id"].as_str().unwrap_or("");
            summary.push_str(&format!(
                "{}. [{}] `{}` in {} — `{}`\n   → read_code_unit(symbol_id=\"{}\")\n",
                i + 1,
                category,
                name,
                short_file,
                if sig.len() > 100 { &sig[..100] } else { sig },
                id
            ));
        }

        if !edges.is_empty() {
            summary.push_str("\nCall edges:\n");
            for edge in edges.iter().take(5) {
                let from = edge["from_id"].as_str().unwrap_or("?");
                let to = edge["to_id"].as_str().unwrap_or("?");
                let rel = edge["relation"].as_str().unwrap_or("?");
                // Use just the symbol name part of the ID
                let from_short = from.rsplit("::").next().unwrap_or(from);
                let to_short = to.rsplit("::").next().unwrap_or(to);
                summary.push_str(&format!("  {} → {} ({})\n", from_short, to_short, rel));
            }
        }

        summary.push_str("\nUse read_code_unit with a symbol_id above to inspect implementations. Do not use generic file read tools.");
    }

    // Build a compact response with the summary as the primary content.
    let compact_symbols: Vec<Value> = important_symbols
        .iter()
        .map(|s| {
            json!({
                "id": s["id"],
                "name": s["name"],
                "file": s["file"],
                "category": s["category"],
                "signature": s["signature"],
            })
        })
        .collect();

    let mut result = json!({
        "query": query,
        "summary": summary,
        "important_symbols": compact_symbols,
        "edges": edges,
    });

    if let Some(source) = source_hint {
        result["source_hint"] = json!(source);
    }
    if let Some(sink) = sink_hint {
        result["sink_hint"] = json!(sink);
    }

    session::record_query(&canonical, &query);
    session::record_files(
        &canonical,
        compact_symbols
            .iter()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        &canonical,
        compact_symbols.iter().filter_map(|item| {
            let id = item["id"].as_str()?.to_string();
            let file = item["file"].as_str().map(ToOwned::to_owned);
            Some((id, file))
        }),
    );
    Ok(result)
}

pub async fn analyze_impact(params: AnalyzeImpactParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
    let depth_limit = params.depth.unwrap_or(2).clamp(1, 3);
    let limit = params.limit.unwrap_or(8).clamp(1, 12);
    let query = params.query.as_deref().unwrap_or("");
    let scope_set = build_scope_set(params.scope.as_deref());
    let seeds = resolve_impact_seeds(&params).await?;
    if seeds.is_empty() {
        return Err(anyhow::anyhow!(
            "analyze_impact could not resolve a seed symbol. Provide symbol_id, file_path, or a more specific query."
        ));
    }

    let mut impacted_symbols: HashMap<String, ImpactSymbol> = HashMap::new();
    let mut impacted_files: HashMap<String, ImpactFile> = HashMap::new();
    let mut frontier: Vec<(String, usize, i32, i32, String)> = Vec::new();
    let mut best_paths: HashMap<String, (i32, usize, i32)> = HashMap::new();

    for seed in &seeds {
        frontier.push((seed.id.clone(), 0, 120, 0, "seed".to_string()));
        best_paths.insert(seed.id.clone(), (120, 0, 0));
        impacted_files
            .entry(seed.file.clone())
            .or_insert_with(|| ImpactFile::new(seed.file.clone()))
            .observe(100, 1.0, "seed symbol", None);
    }

    while !frontier.is_empty() {
        let (best_idx, _) = frontier
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                left.2
                    .cmp(&right.2)
                    .then(right.1.cmp(&left.1))
                    .then(left.3.cmp(&right.3))
                    .then(right.0.cmp(&left.0))
            })
            .unwrap_or((0, &(String::new(), 0, 0, 0, String::new())));
        let (symbol_id, depth, path_score, path_priority, provenance) =
            frontier.swap_remove(best_idx);
        let Some(best_state) = best_paths.get(&symbol_id) else {
            continue;
        };
        if *best_state != (path_score, depth, path_priority) {
            continue;
        }
        if depth >= depth_limit {
            continue;
        }

        for neighbor in collect_impact_neighbors(&index, &symbol_id, &canonical, scope_set.as_ref())
        {
            let distance = depth + 1;
            let score = impact_edge_score(distance, &neighbor, &canonical, profile.as_ref(), query);
            let next_path_score = path_score + score - (distance as i32 * 8);
            let next_path_priority = path_priority + neighbor.priority;
            let candidate_state = (next_path_score, distance, next_path_priority);
            let reason = format!(
                "{} {}: {}",
                neighbor.direction_label(),
                neighbor.relation.as_str(),
                neighbor.evidence
            );
            let support_edge =
                ImpactSupportEdge::from_neighbor(&neighbor, score, &symbol_id, provenance.clone());
            let entry = impacted_symbols
                .entry(neighbor.id.clone())
                .or_insert_with(|| {
                    ImpactSymbol::new(
                        &neighbor.id,
                        &neighbor.name,
                        neighbor.kind.clone(),
                        neighbor.file.clone(),
                        distance,
                    )
                });
            entry.observe(
                score,
                distance,
                neighbor.confidence.max(neighbor.evidence_quality),
                reason.clone(),
                provenance.clone(),
                support_edge.clone(),
            );
            impacted_files
                .entry(neighbor.file.clone())
                .or_insert_with(|| ImpactFile::new(neighbor.file.clone()))
                .observe(
                    score,
                    neighbor.confidence.max(neighbor.evidence_quality),
                    reason.clone(),
                    Some(support_edge),
                );
            let should_expand = best_paths
                .get(&neighbor.id)
                .is_none_or(|existing| better_impact_path(candidate_state, *existing));
            if should_expand {
                best_paths.insert(neighbor.id.clone(), candidate_state);
                frontier.push((
                    neighbor.id.clone(),
                    distance,
                    next_path_score,
                    next_path_priority,
                    format!("{} of {}", neighbor.direction_label(), symbol_id),
                ));
            }
        }
    }

    let mut symbol_values: Vec<ImpactSymbol> = impacted_symbols.into_values().collect();
    symbol_values.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.distance.cmp(&b.distance))
            .then(a.name.cmp(&b.name))
    });
    symbol_values.truncate(limit);

    let mut file_values: Vec<ImpactFile> = impacted_files.into_values().collect();
    file_values.sort_by(|a, b| b.score.cmp(&a.score).then(a.file.cmp(&b.file)));
    file_values.truncate(limit);

    let steering = if symbol_values.is_empty() {
        build_steering(
            0.28,
            "No strong blast-radius candidates were recovered from weighted graph traversal."
                .to_string(),
            "locate_code",
            json!({ "query": params.query, "symbol_id": params.symbol_id, "file_path": params.file_path }),
            take_fallback_candidates(&file_values.iter().map(|f| f.to_json()).collect::<Vec<_>>()),
        )
    } else {
        build_steering(
            0.9,
            "Weighted graph traversal produced a ranked blast-radius view.".to_string(),
            "read_code_unit",
            json!({
                "symbol_id": symbol_values[0].id,
                "name": symbol_values[0].name,
                "file": symbol_values[0].file,
            }),
            take_fallback_candidates(
                &symbol_values
                    .iter()
                    .map(|s| s.to_json())
                    .collect::<Vec<_>>(),
            ),
        )
    };

    let mut response = json!({
        "query": params.query,
        "seed_symbols": seeds.iter().map(|seed| json!({
            "id": seed.id,
            "name": seed.name,
            "kind": seed.kind,
            "file": seed.file,
        })).collect::<Vec<_>>(),
        "depth_limit": depth_limit,
        "impact_symbols": symbol_values.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
        "impact_files": file_values.iter().map(|f| f.to_json()).collect::<Vec<_>>(),
        "edge_provenance_summary": build_edge_provenance_summary(&symbol_values, &file_values),
        "summary": if symbol_values.is_empty() {
            "No significant weighted graph neighbors were found."
        } else {
            "Weighted graph traversal identified the most likely blast-radius targets."
        },
    });
    let impact_followup = response["impact_symbols"]
        .as_array()
        .and_then(|items| items.first())
        .map(|top| expansion_followup_state(&canonical, top));
    session::record_query(&canonical, query);
    session::record_files(
        &canonical,
        response["impact_files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        &canonical,
        response["impact_symbols"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let id = item["id"].as_str()?.to_string();
                let file = item["file"].as_str().map(ToOwned::to_owned);
                Some((id, file))
            }),
    );
    attach_steering(&mut response, steering);
    if let Some((already_seen, target)) = impact_followup {
        apply_expansion_followup(
            &mut response,
            already_seen,
            "find_usages",
            target,
            "The highest-ranked blast-radius target is already in the current session, so expand through usages or neighboring impact nodes instead of rereading it.",
        );
    }
    Ok(response)
}

fn expansion_followup_state(project_path: &Path, candidate: &Value) -> (bool, Value) {
    let file = candidate["file"].as_str().unwrap_or("");
    let already_seen = candidate["id"]
        .as_str()
        .map(|symbol_id| session::has_seen_symbol(project_path, symbol_id))
        .unwrap_or_else(|| {
            if file.is_empty() {
                false
            } else {
                session::has_seen_file(project_path, Path::new(file))
            }
        });
    let target = if let Some(symbol_id) = candidate["id"].as_str() {
        json!({
            "symbol_id": symbol_id,
            "name": candidate["name"],
            "file": candidate["file"],
        })
    } else {
        json!({
            "file_path": candidate["file"],
            "name": candidate["name"],
        })
    };
    (already_seen, target)
}

fn apply_expansion_followup(
    response: &mut Value,
    already_seen: bool,
    next_tool: &str,
    target: Value,
    why: &str,
) {
    response["session_state"] = json!({
        "top_target_seen": already_seen,
        "guidance": if already_seen {
            "The top-ranked expansion target is already in session context."
        } else {
            "The top-ranked expansion target is new in the current session."
        },
    });
    if !already_seen {
        return;
    }

    if let Some(steering) = response.get_mut("steering").and_then(Value::as_object_mut) {
        steering.insert("why_this_matched".to_string(), json!(why));
        steering.insert("recommended_next_tool".to_string(), json!(next_tool));
        steering.insert("recommended_target".to_string(), target);
    }
}

pub async fn navigate_code(params: NavigateCodeParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
    let route = choose_navigation_route(NavigationRouteContext {
        intent: params.intent.as_deref(),
        has_symbol: params.symbol_id.is_some(),
        has_file: params.file_path.is_some(),
        has_line_start: params.line_start.is_some(),
        has_line_end: params.line_end.is_some(),
        query: &params.query,
        source: params.source.as_deref(),
        sink: params.sink.as_deref(),
    });

    let mut response = match route {
        NavigationRoute::Read => {
            read_code_unit(ReadCodeUnitParams {
                project: params.project.clone(),
                symbol_id: params.symbol_id.clone(),
                file_path: params.file_path.clone(),
                line_start: params.line_start,
                line_end: params.line_end,
                include_context: params.include_context,
                signature_only: params.signature_only,
            })
            .await?
        }
        NavigationRoute::Impact => {
            analyze_impact(AnalyzeImpactParams {
                project: params.project.clone(),
                query: Some(params.query.clone()),
                symbol_id: params.symbol_id.clone(),
                file_path: params.file_path.clone(),
                scope: params.scope.clone(),
                depth: params.depth,
                limit: params.limit,
            })
            .await?
        }
        NavigationRoute::Trace => {
            trace_path(TracePathParams {
                project: params.project.clone(),
                query: params.query.clone(),
                source: params.source.clone(),
                sink: params.sink.clone(),
                language: params.language.clone(),
                file: params.scope.clone(),
                max_symbols: params.max_symbols,
                max_depth: params.max_depth,
            })
            .await?
        }
        NavigationRoute::Locate => {
            locate_code(LocateCodeParams {
                project: params.project.clone(),
                query: params.query.clone(),
                intent: params.intent.clone(),
                kind: params.kind.clone(),
                language: params.language.clone(),
                scope: params.scope.clone(),
                limit: params.limit,
            })
            .await?
        }
    };

    if let Value::Object(map) = &mut response {
        map.insert("navigation_route".to_string(), json!(route.as_str()));
        map.insert(
            "navigation_reason".to_string(),
            json!(route.reason(&params)),
        );
        if let Some(ref profile) = profile {
            map.insert(
                "navigation_repo_context".to_string(),
                json!({
                    "repo_map": compact_repo_map(Some(profile)),
                    "route_bias": route.repo_bias(profile, &params.query),
                }),
            );
        }
    }
    apply_navigation_followup(&mut response);
    Ok(response)
}

fn apply_navigation_followup(response: &mut Value) {
    let status = response["read_state"]["status"].as_str().unwrap_or("new");
    if status == "new" {
        return;
    }

    if status == "changed" {
        response["session_state"] = json!({
            "top_target_seen": true,
            "target_changed": true,
            "guidance": "The explicit read target changed since the last read in this session. Use the refreshed payload before expanding further.",
        });
        response["read_state"]["guidance"] = json!(
            "This target changed since the last read in the current session. Use the refreshed payload before expanding to usages or neighboring code."
        );
        return;
    }

    let read_kind = response["read_state"]["read_kind"]
        .as_str()
        .unwrap_or("code_unit");
    let next_tool = match read_kind {
        "symbol" => "find_usages",
        "line_slice" => "locate_code",
        "file_outline" => "locate_code",
        _ => "locate_code",
    };
    let recommended_target = match read_kind {
        "symbol" => json!({
            "symbol_id": response["id"],
            "name": response["name"],
            "file": response["file"],
        }),
        _ => json!({
            "file_path": response["file"],
            "query": response["name"].as_str().unwrap_or("related symbol"),
        }),
    };

    if let Some(steering) = response.get_mut("steering").and_then(Value::as_object_mut) {
        steering.insert(
            "why_this_matched".to_string(),
            json!("The explicit read target was resolved, but this code unit was already returned in the current session."),
        );
        steering.insert("recommended_next_tool".to_string(), json!(next_tool));
        steering.insert("recommended_target".to_string(), recommended_target);
    }

    response["read_state"]["guidance"] = json!(
        "This target is already in session context. Expand to usages, related symbols, or a neighboring slice instead of rereading unchanged content."
    );
    response["session_state"] = json!({
        "top_target_seen": true,
        "target_changed": false,
        "guidance": "The explicit read target is unchanged since the last read in this session. Expand instead of rereading it again.",
    });
}

#[derive(Clone)]
enum LocateRoute {
    Project,
    Files,
    Content,
    Symbols { mode: String },
}

impl LocateRoute {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "get_project_outline",
            Self::Files => "search_files",
            Self::Content => "search_content",
            Self::Symbols { .. } => "search_symbols",
        }
    }
}

#[derive(Clone)]
enum NavigationRoute {
    Read,
    Trace,
    Impact,
    Locate,
}

impl NavigationRoute {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read_code_unit",
            Self::Trace => "trace_path",
            Self::Impact => "analyze_impact",
            Self::Locate => "locate_code",
        }
    }

    fn reason(&self, params: &NavigateCodeParams) -> String {
        match self {
            Self::Read => "explicit symbol/file selection was provided".to_string(),
            Self::Trace => format!(
                "the query/intent suggested a flow or execution-path question: {}",
                params.query
            ),
            Self::Impact => format!(
                "the query/intent suggested blast-radius analysis: {}",
                params.query
            ),
            Self::Locate => "no explicit read/trace/impact target was provided".to_string(),
        }
    }

    fn repo_bias(&self, profile: &RepoProfile, query: &str) -> String {
        let archetype = crate::index::repo_profile::archetype_label(profile.archetype);
        let query_lower = query.to_ascii_lowercase();
        match self {
            Self::Read => {
                format!("read route bypassed repo-role bias for explicit target selection in a {archetype} repo")
            }
            Self::Trace if query_lower.contains("config") || query_lower.contains("env") => {
                format!("trace route favored bootstrap/config-heavy paths in a {archetype} repo")
            }
            Self::Trace => {
                format!(
                    "trace route favored entrypoint/bootstrap-heavy paths in a {archetype} repo"
                )
            }
            Self::Impact => {
                format!("impact route favored direct callers/callees but kept repo-role priors for a {archetype} repo")
            }
            Self::Locate => {
                format!("locate route kept repo-role priors active for broad discovery in a {archetype} repo")
            }
        }
    }
}

/// Normalize vague natural-language queries into sharper discovery intents.
///
/// Many agent queries use colloquial phrases like "main function", "entry point",
/// "args clap", or "directory traversal walker" that don't map well to symbol
/// names or file paths.  This function rewrites them into queries that the
/// downstream search primitives handle more effectively.
fn normalize_locate_query(query: &str, profile: Option<&RepoProfile>) -> Option<String> {
    let lower = query.trim().to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    // Single-word or exact-symbol queries don't need normalization.
    if words.len() <= 1 && looks_like_exact_symbol(query) {
        return None;
    }

    // Entrypoint / main function patterns
    if matches!(
        lower.as_str(),
        "main function"
            | "main fn"
            | "entry point"
            | "entrypoint"
            | "program entry"
            | "app entry"
            | "application entry"
            | "start function"
    ) {
        if let Some(profile) = profile {
            let entrypoints = profile_entrypoints(Some(profile));
            if let Some(first) = entrypoints.first() {
                return Some(first.clone());
            }
        }
        return Some("main".to_string());
    }

    // CLI / argument parsing patterns
    if lower.contains("args") && (lower.contains("clap") || lower.contains("pars"))
        || lower == "cli arguments"
        || lower == "cli args"
        || lower == "argument parsing"
        || lower == "flag parsing"
        || lower == "command line"
        || lower == "command-line flag parsing"
        || lower.contains("command-line") && lower.contains("pars")
        || lower.contains("flag") && lower.contains("pars")
        || lower.contains("argument") && lower.contains("pars")
        || lower == "runtime configuration"
        || (lower.contains("runtime") && lower.contains("config"))
    {
        return Some("flags args parse config".to_string());
    }

    // Directory traversal / walker patterns
    if (lower.contains("directory") || lower.contains("dir"))
        && (lower.contains("travers") || lower.contains("walk") || lower.contains("iter"))
    {
        return Some("walk directory builder".to_string());
    }

    // Printer / output patterns
    if lower == "printer print results"
        || lower == "print results"
        || lower == "output printer"
        || lower == "result printer"
    {
        return Some("Printer".to_string());
    }

    // Ignore / gitignore handling patterns
    if lower == "ignore handling"
        || lower == "ignore logic"
        || lower == "gitignore"
        || lower == "ignore rules"
        || lower == "file ignore"
    {
        return Some("Ignore matched".to_string());
    }

    // Config / settings patterns
    if lower == "configuration"
        || lower == "config handling"
        || lower == "settings"
        || lower == "config flow"
    {
        return Some("Config".to_string());
    }

    None
}

/// Build a concrete query-sharpening suggestion for weak `locate_code` results.
///
/// Instead of generic "try again" advice, this returns one specific sharper
/// query the model can use immediately.
fn suggest_sharper_query(
    query: &str,
    route_used: &str,
    profile: Option<&RepoProfile>,
) -> Option<String> {
    let lower = query.trim().to_lowercase();

    // If the query was already normalized and still failed, suggest a different angle.
    if lower.contains("main") || lower.contains("entry") {
        if let Some(profile) = profile {
            let entrypoints = profile_entrypoints(Some(profile));
            if let Some(first) = entrypoints.first() {
                return Some(format!(
                    "Try reading the entrypoint file directly: read_code_unit(file_path=\"{}\")",
                    first
                ));
            }
        }
        return Some(
            "Try locate_code with intent=\"file\" and query matching the main source file"
                .to_string(),
        );
    }

    if lower.contains("config") || lower.contains("setting") || lower.contains("flag") {
        return Some(
            "Try locate_code with a concrete config struct or flag type name from the codebase"
                .to_string(),
        );
    }

    if lower.contains("ignore") || lower.contains("gitignore") {
        return Some(
            "Try locate_code with query=\"Ignore\" or query=\"gitignore_matched\" for the ignore subsystem"
                .to_string(),
        );
    }

    if lower.contains("walk") || lower.contains("travers") || lower.contains("directory") {
        return Some(
            "Try locate_code with query=\"WalkBuilder\" or query=\"WalkParallel\" for directory traversal"
                .to_string(),
        );
    }

    // Generic fallback based on route
    match route_used {
        "search_symbols" => Some(
            "Try a more specific symbol name or use search_content with a known code snippet"
                .to_string(),
        ),
        "search_files" => Some(
            "Try locate_code with intent=\"symbol\" and a concrete type or function name"
                .to_string(),
        ),
        _ => None,
    }
}

fn choose_locate_route(intent: &Option<String>, query: &str) -> LocateRoute {
    let intent_lower = intent.as_deref().unwrap_or("").to_lowercase();
    let query_lower = query.to_lowercase();

    if intent_lower.contains("project") || looks_broad_repo_query(&query_lower) {
        LocateRoute::Project
    } else if intent_lower.contains("content") || looks_like_text_snippet(query) {
        LocateRoute::Content
    } else if looks_like_path(query) || intent_lower.contains("file") {
        LocateRoute::Files
    } else {
        let mode = if looks_like_exact_symbol(query) {
            "bm25"
        } else {
            "semantic"
        };
        LocateRoute::Symbols {
            mode: mode.to_string(),
        }
    }
}

fn fallback_locate_route(route: &LocateRoute, query: &str) -> Option<LocateRoute> {
    match route {
        LocateRoute::Project => Some(LocateRoute::Files),
        LocateRoute::Files | LocateRoute::Content => Some(LocateRoute::Symbols {
            mode: fallback_symbol_mode(query).to_string(),
        }),
        LocateRoute::Symbols { .. } => None,
    }
}

fn fallback_symbol_mode(query: &str) -> &'static str {
    if prefers_semantic_discovery(query) {
        "semantic"
    } else if looks_like_exact_symbol(query) {
        "bm25"
    } else {
        "fuzzy"
    }
}

struct NavigationRouteContext<'a> {
    intent: Option<&'a str>,
    has_symbol: bool,
    has_file: bool,
    has_line_start: bool,
    has_line_end: bool,
    query: &'a str,
    source: Option<&'a str>,
    sink: Option<&'a str>,
}

fn choose_navigation_route(ctx: NavigationRouteContext<'_>) -> NavigationRoute {
    let intent_lower = ctx.intent.unwrap_or("").to_lowercase();
    let query_lower = ctx.query.to_lowercase();
    if ctx.has_symbol || ctx.has_file || ctx.has_line_start || ctx.has_line_end {
        NavigationRoute::Read
    } else if contains_term(&intent_lower, "impact")
        || contains_term(&intent_lower, "blast")
        || contains_term(&intent_lower, "refactor")
        || contains_term(&query_lower, "impact")
        || contains_term(&query_lower, "blast radius")
        || contains_term(&query_lower, "break")
    {
        NavigationRoute::Impact
    } else if contains_term(&intent_lower, "trace")
        || contains_term(&intent_lower, "path")
        || contains_term(&intent_lower, "flow")
        || contains_term(&intent_lower, "call")
        || ctx.source.is_some()
        || ctx.sink.is_some()
        || contains_term(&query_lower, "how does")
        || contains_term(&query_lower, "execution")
    {
        NavigationRoute::Trace
    } else {
        NavigationRoute::Locate
    }
}

fn looks_like_path(query: &str) -> bool {
    query.contains('/')
        || query.contains('\\')
        || query.contains('*')
        || query.contains('?')
        || query.contains("::")
        || query.ends_with(".rs")
        || query.ends_with(".ts")
        || query.ends_with(".js")
        || query.ends_with(".py")
}

fn looks_like_text_snippet(query: &str) -> bool {
    // A text snippet is actual code or log text, not a concept query.
    // Multi-word concept queries like "command-line flag parsing" should route
    // to symbol search (semantic or bm25), not content search.
    let has_code_signals = query.contains('\"')
        || query.contains('\'')
        || query.contains("=>")
        || query.contains("->")
        || query.contains("::")
        || query.contains("()")
        || query.contains('{')
        || query.contains('}')
        || query.contains(';');

    if has_code_signals {
        return true;
    }

    // Only treat as text snippet if it's long enough to be a real code fragment
    // (5+ words) AND contains no obvious concept/subsystem keywords.
    let words = query.split_whitespace().count();
    if words >= 5 {
        let lower = query.to_lowercase();
        // If it reads like a concept query, don't treat as text snippet.
        let concept_words = [
            "function",
            "method",
            "class",
            "struct",
            "module",
            "handler",
            "config",
            "parse",
            "build",
            "create",
            "implement",
            "logic",
            "handling",
            "execution",
            "path",
            "flow",
            "search",
            "find",
            "where",
            "how",
            "what",
            "which",
            "the",
            "this",
        ];
        let has_concept = concept_words.iter().any(|w| lower.contains(w));
        return !has_concept;
    }

    false
}

fn prefers_semantic_discovery(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty()
        || looks_like_exact_symbol(trimmed)
        || looks_like_path(trimmed)
        || looks_like_text_snippet(trimmed)
    {
        return false;
    }

    trimmed.split_whitespace().count() >= 2
}

fn looks_broad_repo_query(query: &str) -> bool {
    let lower = query.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "project"
            | "repo"
            | "repository"
            | "layout"
            | "structure"
            | "overview"
            | "repo layout"
            | "project layout"
            | "codebase overview"
            | "architecture"
            | "project structure"
            | "repo structure"
            | "codebase"
            | "codebase structure"
    )
}

fn contains_term(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    haystack.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(is_term_boundary);
        let after_ok = haystack[end..].chars().next().is_none_or(is_term_boundary);
        before_ok && after_ok
    })
}

fn is_term_boundary(ch: char) -> bool {
    !(ch.is_ascii_alphanumeric() || ch == '_')
}

fn looks_like_exact_symbol(query: &str) -> bool {
    let trimmed = query.trim();
    !trimmed.is_empty()
        && !trimmed.contains(' ')
        && !looks_like_path(trimmed)
        && trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '<' || c == '>')
}

async fn locate_project(params: &LocateCodeParams, limit: usize) -> anyhow::Result<Vec<Value>> {
    let result = get_project_outline(GetProjectOutlineParams {
        project: params.project.clone(),
        depth: Some(2),
        path: params.scope.clone(),
        max_dirs: Some(limit),
        summary: Some(true),
    })
    .await?;

    let mut results = Vec::new();
    if !result["repo_map"].is_null() {
        let primary_file = result["architecture_anchors"]["primary_file"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                result["repo_map"]["entrypoints"]
                    .as_array()
                    .and_then(|entries| entries.first())
                    .and_then(|entry| entry.as_str())
                    .map(str::to_owned)
            });
        results.push(json!({
            "kind": "repo_map",
            "name": "repo map",
            "file": "",
            "primary_file": primary_file,
            "repo_map": result["repo_map"],
            "architecture_anchors": result["architecture_anchors"],
            "source_tool": "get_project_outline",
        }));
    }

    let repo_map = result["repo_map"].clone();
    let mut directories = result["directories"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|dir| {
            json!({
                "kind": "directory",
                "dir": dir["dir"],
                "file": dir["dir"],
                "files": dir["files"],
                "symbols": dir["symbols"],
                "source_tool": "get_project_outline",
            })
        })
        .collect::<Vec<_>>();
    let canonical = resolve_project_path(&params.project)?;
    rerank_project_directories(&mut directories, &canonical, &params.query, &repo_map);
    results.extend(directories);
    results.truncate(limit);
    Ok(results)
}

fn rerank_project_directories(
    results: &mut [Value],
    project_path: &Path,
    query: &str,
    repo_map: &Value,
) {
    if results.is_empty() {
        return;
    }
    let top_roles = repo_map["top_roles"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let entrypoints = repo_map["entrypoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let query_lower = query.to_ascii_lowercase();

    results.sort_by(|left, right| {
        let left_score = project_directory_score(
            left,
            project_path,
            &query_lower,
            top_roles.as_slice(),
            entrypoints.as_slice(),
        );
        let right_score = project_directory_score(
            right,
            project_path,
            &query_lower,
            top_roles.as_slice(),
            entrypoints.as_slice(),
        );
        right_score.cmp(&left_score).then_with(|| {
            left["dir"]
                .as_str()
                .unwrap_or("")
                .cmp(right["dir"].as_str().unwrap_or(""))
        })
    });
}

fn project_directory_score(
    candidate: &Value,
    project_path: &Path,
    query_lower: &str,
    top_roles: &[Value],
    entrypoints: &[String],
) -> i32 {
    let dir = candidate["dir"].as_str().unwrap_or("").to_ascii_lowercase();
    let mut score = 0;

    for (rank, role) in top_roles.iter().enumerate() {
        let role_name = role["role"].as_str().unwrap_or("");
        if !role_name.is_empty() && dir.contains(role_name) {
            score += 20 - rank as i32 * 4;
        }
    }

    if entrypoints.iter().any(|entry| {
        entry.eq_ignore_ascii_case(dir.as_str()) || entry.starts_with(&format!("{dir}/"))
    }) {
        score += 12;
    }

    if query_lower.contains("config")
        || query_lower.contains("setting")
        || query_lower.contains("env")
    {
        if dir.contains("config") {
            score += 16;
        }
        if dir.contains("bootstrap") || dir.contains("init") {
            score += 8;
        }
    }
    if (query_lower.contains("start")
        || query_lower.contains("boot")
        || query_lower.contains("entry"))
        && (dir.contains("src") || dir.contains("bin") || dir.contains("main"))
    {
        score += 10;
    }
    if (query_lower.contains("route")
        || query_lower.contains("request")
        || query_lower.contains("http"))
        && (dir.contains("handler") || dir.contains("route") || dir.contains("http"))
    {
        score += 12;
    }

    score += session::directory_boost(project_path, Path::new(&dir));

    score
}

async fn locate_files(params: &LocateCodeParams, limit: usize) -> anyhow::Result<Vec<Value>> {
    let mode = if looks_like_path(&params.query) {
        "exact"
    } else if params.query.contains('*') || params.query.contains('?') {
        "glob"
    } else {
        "substring"
    };
    let result = search_files(SearchFilesParams {
        project: params.project.clone(),
        query: params.query.clone(),
        mode: Some(mode.to_string()),
        language: params.language.clone(),
        file: params.scope.clone(),
        limit: Some(limit),
        offset: Some(0),
    })
    .await?;

    Ok(result["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|file| {
            let score = file["score"].as_f64().unwrap_or(0.0) as f32;
            json!({
                "kind": "file",
                "file": file["file"],
                "name": file["file_name"],
                "match_type": file["match_type"],
                "score": score,
                "source_tool": "search_files",
            })
        })
        .collect())
}

async fn locate_content(params: &LocateCodeParams, limit: usize) -> anyhow::Result<Vec<Value>> {
    let regex = contains_regex_meta(&params.query);
    let result = search_content(SearchContentParams {
        project: params.project.clone(),
        query: params.query.clone(),
        regex: Some(regex),
        case_sensitive: Some(false),
        language: params.language.clone(),
        file: params.scope.clone(),
        limit: Some(limit),
        offset: Some(0),
        before_context: Some(0),
        after_context: Some(0),
    })
    .await?;

    Ok(result["matches"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            json!({
                "kind": "content",
                "file": m["file"],
                "line_start": m["line"],
                "line_end": m["line"],
                "column": m["column"],
                "snippet": m["line_text"],
                "source_tool": "search_content",
            })
        })
        .collect())
}

async fn locate_symbols(
    params: &LocateCodeParams,
    limit: usize,
    mode: &str,
) -> anyhow::Result<Vec<Value>> {
    let semantic_cfg = EmbedConfig::from_env().map(Arc::new);
    let query_mode = if mode == "semantic" {
        if semantic_cfg.is_some() {
            "semantic"
        } else {
            "bm25"
        }
    } else if looks_like_exact_symbol(&params.query) {
        "bm25"
    } else {
        mode
    };

    // Try the primary search mode. If semantic search fails (e.g. embedding
    // server is down), fall back to bm25 instead of propagating the error.
    let result = search_symbols(SearchSymbolsParams {
        project: params.project.clone(),
        query: params.query.clone(),
        kind: params.kind.clone(),
        language: params.language.clone(),
        file: params.scope.clone(),
        limit: Some(limit),
        offset: Some(0),
        mode: Some(query_mode.to_string()),
        embed_config: semantic_cfg.clone(),
    })
    .await;

    let result = match result {
        Ok(r) => r,
        Err(_) if query_mode == "semantic" => {
            // Semantic search failed; fall back to bm25 silently.
            search_symbols(SearchSymbolsParams {
                project: params.project.clone(),
                query: params.query.clone(),
                kind: params.kind.clone(),
                language: params.language.clone(),
                file: params.scope.clone(),
                limit: Some(limit),
                offset: Some(0),
                mode: Some("bm25".to_string()),
                embed_config: None,
            })
            .await?
        }
        Err(e) => return Err(e),
    };

    let mut results = result["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|sym| {
            json!({
                "kind": "symbol",
                "id": sym["id"],
                "name": sym["name"],
                "qualified": sym["qualified"],
                "symbol_kind": sym["kind"],
                "file": sym["file"],
                "line_start": sym["line_start"],
                "line_end": sym["line_end"],
                "signature": sym["signature"],
            })
        })
        .collect::<Vec<_>>();

    if results.is_empty() {
        let fallback_mode = match query_mode {
            "bm25" | "exact" => "fuzzy",
            "fuzzy" => "bm25",
            "semantic" => "bm25",
            _ => "bm25",
        };
        if fallback_mode != query_mode {
            let fallback = search_symbols(SearchSymbolsParams {
                project: params.project.clone(),
                query: params.query.clone(),
                kind: params.kind.clone(),
                language: params.language.clone(),
                file: params.scope.clone(),
                limit: Some(limit),
                offset: Some(0),
                mode: Some(fallback_mode.to_string()),
                embed_config: semantic_cfg,
            })
            .await?;
            results = fallback["results"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|sym| {
                    json!({
                        "kind": "symbol",
                        "id": sym["id"],
                        "name": sym["name"],
                        "qualified": sym["qualified"],
                        "symbol_kind": sym["kind"],
                        "file": sym["file"],
                        "line_start": sym["line_start"],
                        "line_end": sym["line_end"],
                        "signature": sym["signature"],
                    })
                })
                .collect::<Vec<_>>();
        }
    }

    if mode == "semantic" && !looks_like_exact_symbol(&params.query) && !results.is_empty() {
        let canonical = resolve_project_path(&params.project)?;
        let profile = load_project_meta(&canonical)
            .ok()
            .map(|meta| meta.repo_profile);
        rerank_locate_symbol_results(&mut results, &canonical, &params.query, profile.as_ref());
    }

    Ok(results)
}

fn rerank_locate_symbol_results(
    results: &mut [Value],
    project_path: &Path,
    query: &str,
    profile: Option<&RepoProfile>,
) {
    let Some(profile) = profile else {
        return;
    };
    let role_counts = summarize_role_counts(Some(profile));
    let mut dominant_roles: Vec<(String, usize)> = role_counts.into_iter().collect();
    dominant_roles.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    dominant_roles.truncate(3);
    let entrypoints = profile_entrypoints(Some(profile));
    let query_lower = query.to_ascii_lowercase();

    results.sort_by(|left, right| {
        let left_score = locate_symbol_repo_score(
            left,
            project_path,
            &query_lower,
            &dominant_roles,
            entrypoints.as_slice(),
        );
        let right_score = locate_symbol_repo_score(
            right,
            project_path,
            &query_lower,
            &dominant_roles,
            entrypoints.as_slice(),
        );
        right_score.cmp(&left_score).then_with(|| {
            left["name"]
                .as_str()
                .unwrap_or("")
                .cmp(right["name"].as_str().unwrap_or(""))
        })
    });
}

fn locate_symbol_repo_score(
    candidate: &Value,
    project_path: &Path,
    query_lower: &str,
    dominant_roles: &[(String, usize)],
    entrypoints: &[String],
) -> i32 {
    let role = candidate["path_role"].as_str().unwrap_or("");
    let file = candidate["file"].as_str().unwrap_or("");
    let mut score = 0;

    for (rank, (dominant_role, count)) in dominant_roles.iter().enumerate() {
        if role == dominant_role {
            score += 18 - rank as i32 * 4 + (*count as i32).min(6);
        }
    }

    if entrypoints.iter().any(|entry| entry == file) {
        score += 10;
        if query_lower.contains("start")
            || query_lower.contains("boot")
            || query_lower.contains("entry")
            || query_lower.contains("flow")
            || query_lower.contains("path")
        {
            score += 8;
        }
    }

    if role == role_label(crate::index::repo_profile::PathRole::Config)
        && (query_lower.contains("config")
            || query_lower.contains("setting")
            || query_lower.contains("env"))
    {
        score += 14;
    }
    if role == role_label(crate::index::repo_profile::PathRole::Bootstrap)
        && (query_lower.contains("start")
            || query_lower.contains("boot")
            || query_lower.contains("init"))
    {
        score += 14;
    }
    if role == role_label(crate::index::repo_profile::PathRole::Handler)
        && (query_lower.contains("request")
            || query_lower.contains("route")
            || query_lower.contains("http"))
    {
        score += 12;
    }

    if let Some(symbol_id) = candidate["id"].as_str() {
        score += session::symbol_boost(project_path, symbol_id, Some(Path::new(file)));
    } else {
        score += session::file_boost(project_path, Path::new(file));
    }

    score
}

async fn resolve_impact_seeds(params: &AnalyzeImpactParams) -> anyhow::Result<Vec<ImpactSeed>> {
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
    let profile_ref = profile.as_ref();

    if let Some(symbol_id) = params.symbol_id.as_deref() {
        let index = load_project_index(&params.project)?;
        if let Some(sym) = index.symbols.get(symbol_id) {
            return Ok(vec![ImpactSeed {
                id: sym.id.clone(),
                name: sym.name.clone(),
                kind: sym.kind.to_string(),
                file: sym.file.to_string_lossy().replace('\\', "/"),
            }]);
        }
        return Ok(vec![ImpactSeed {
            id: symbol_id.to_string(),
            name: symbol_id.to_string(),
            kind: "symbol".to_string(),
            file: params.file_path.clone().unwrap_or_default(),
        }]);
    }

    if let Some(file_path) = params.file_path.as_deref() {
        let outline = get_file_outline(GetFileOutlineParams {
            project: params.project.clone(),
            file_path: file_path.to_string(),
        })
        .await?;
        let seeds = impact_file_outline_seeds(
            &outline,
            file_path,
            params.query.as_deref(),
            &canonical,
            profile_ref,
        );
        if !seeds.is_empty() {
            return Ok(seeds);
        }
    }

    if let Some(query) = params.query.as_deref() {
        let located = locate_code(LocateCodeParams {
            project: params.project.clone(),
            query: query.to_string(),
            intent: Some("symbol".to_string()),
            kind: None,
            language: None,
            scope: params.scope.clone(),
            limit: Some(5),
        })
        .await?;
        let mut candidates = located["results"].as_array().cloned().unwrap_or_default();
        rerank_impact_seed_candidates(&mut candidates, query, &canonical, profile_ref);
        let mut seeds = Vec::new();
        for candidate in candidates {
            if let Some(id) = candidate["id"].as_str() {
                seeds.push(ImpactSeed {
                    id: id.to_string(),
                    name: candidate["name"].as_str().unwrap_or(id).to_string(),
                    kind: candidate["symbol_kind"]
                        .as_str()
                        .unwrap_or("symbol")
                        .to_string(),
                    file: candidate["file"].as_str().unwrap_or("").to_string(),
                });
            } else if let Some(file) = candidate["file"].as_str() {
                let outline = get_file_outline(GetFileOutlineParams {
                    project: params.project.clone(),
                    file_path: file.to_string(),
                })
                .await?;
                seeds.extend(impact_file_outline_seeds(
                    &outline,
                    file,
                    Some(query),
                    &canonical,
                    profile_ref,
                ));
            }
        }
        if !seeds.is_empty() {
            let mut deduped = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for seed in seeds {
                if seen.insert(seed.id.clone()) {
                    deduped.push(seed);
                }
                if deduped.len() >= 5 {
                    break;
                }
            }
            return Ok(deduped);
        }
    }

    Ok(Vec::new())
}

fn rerank_impact_seed_candidates(
    candidates: &mut [Value],
    query: &str,
    project_path: &Path,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
) {
    candidates.sort_by(|left, right| {
        let left_score = impact_seed_candidate_score(left, query, project_path, profile);
        let right_score = impact_seed_candidate_score(right, query, project_path, profile);
        right_score.cmp(&left_score).then_with(|| {
            left["name"]
                .as_str()
                .unwrap_or(left["file"].as_str().unwrap_or(""))
                .cmp(
                    right["name"]
                        .as_str()
                        .unwrap_or(right["file"].as_str().unwrap_or("")),
                )
        })
    });
}

fn impact_seed_candidate_score(
    candidate: &Value,
    query: &str,
    project_path: &Path,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
) -> i32 {
    let file = candidate["file"].as_str().unwrap_or("");
    let name = candidate["name"]
        .as_str()
        .or_else(|| candidate["qualified"].as_str())
        .unwrap_or(file);
    let mut score = impact_score(0, file, name, project_path, profile, query);
    match candidate["kind"].as_str().unwrap_or("symbol") {
        "symbol" => {
            score += 24;
            score += impact_symbol_kind_bonus(candidate["symbol_kind"].as_str().unwrap_or(""));
        }
        "file" => score += 8,
        "content" => score += 4,
        _ => {}
    }
    if let Some(symbol_id) = candidate["id"].as_str() {
        score += session::symbol_boost(project_path, symbol_id, Some(Path::new(file)));
    } else {
        score += session::file_boost(project_path, Path::new(file));
    }
    score
}

fn impact_file_outline_seeds(
    outline: &Value,
    file: &str,
    query: Option<&str>,
    project_path: &Path,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
) -> Vec<ImpactSeed> {
    let query = query.unwrap_or("");
    let mut ranked = outline["symbols"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|sym| {
            let id = sym["id"].as_str()?.to_string();
            let name = sym["name"].as_str().unwrap_or(&id).to_string();
            let kind = sym["kind"].as_str().unwrap_or("symbol").to_string();
            let score = impact_score(0, file, &name, project_path, profile, query)
                + 20
                + impact_symbol_kind_bonus(&kind);
            Some((
                score,
                ImpactSeed {
                    id,
                    name,
                    kind,
                    file: file.to_string(),
                },
            ))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
    });
    ranked.into_iter().take(3).map(|(_, seed)| seed).collect()
}

fn impact_symbol_kind_bonus(kind: &str) -> i32 {
    match kind {
        "function" | "method" => 14,
        "impl_method" | "associated_function" => 12,
        "trait_method" => 10,
        "class" | "struct" | "enum" | "trait" | "interface" => 4,
        "module" | "namespace" => -6,
        _ => 0,
    }
}

#[derive(Clone)]
struct ImpactSeed {
    id: String,
    name: String,
    kind: String,
    file: String,
}

#[derive(Clone, Copy)]
enum ImpactDirection {
    Incoming,
    Outgoing,
}

impl ImpactDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Incoming => "direct caller",
            Self::Outgoing => "direct callee",
        }
    }

    fn score_bonus(self) -> i32 {
        match self {
            Self::Incoming => 12,
            Self::Outgoing => 4,
        }
    }
}

#[derive(Clone)]
struct ImpactNeighbor {
    id: String,
    name: String,
    kind: String,
    file: String,
    direction: ImpactDirection,
    relation: EdgeRelation,
    evidence: String,
    confidence: f32,
    evidence_quality: f32,
    priority: i32,
}

impl ImpactNeighbor {
    fn direction_label(&self) -> &'static str {
        self.direction.label()
    }
}

#[derive(Clone)]
struct ImpactSupportEdge {
    direction: &'static str,
    relation: EdgeRelation,
    evidence: String,
    confidence: f32,
    evidence_quality: f32,
    priority: i32,
    score: i32,
    via_symbol_id: String,
    provenance: String,
}

impl ImpactSupportEdge {
    fn from_neighbor(
        neighbor: &ImpactNeighbor,
        score: i32,
        via_symbol_id: &str,
        provenance: String,
    ) -> Self {
        Self {
            direction: neighbor.direction_label(),
            relation: neighbor.relation,
            evidence: neighbor.evidence.clone(),
            confidence: neighbor.confidence,
            evidence_quality: neighbor.evidence_quality,
            priority: neighbor.priority,
            score,
            via_symbol_id: via_symbol_id.to_string(),
            provenance,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "direction": self.direction,
            "relation": self.relation.as_str(),
            "evidence": self.evidence,
            "confidence": self.confidence,
            "evidence_quality": self.evidence_quality,
            "priority": self.priority,
            "score": self.score,
            "via_symbol_id": self.via_symbol_id,
            "provenance": self.provenance,
        })
    }
}

#[derive(Clone)]
struct ImpactSymbol {
    id: String,
    name: String,
    kind: String,
    file: String,
    distance: usize,
    score: i32,
    confidence: f32,
    reasons: Vec<String>,
    support_edges: Vec<ImpactSupportEdge>,
}

impl ImpactSymbol {
    fn new(id: &str, name: &str, kind: String, file: String, distance: usize) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            file,
            distance,
            score: 0,
            confidence: 0.0,
            reasons: Vec::new(),
            support_edges: Vec::new(),
        }
    }

    fn observe(
        &mut self,
        score: i32,
        distance: usize,
        confidence: f32,
        reason: impl Into<String>,
        provenance: String,
        support_edge: ImpactSupportEdge,
    ) {
        if score > self.score {
            self.score = score;
        }
        if distance < self.distance {
            self.distance = distance;
        }
        if confidence > self.confidence {
            self.confidence = confidence;
        }
        self.reasons
            .push(format!("{} ({})", reason.into(), provenance));
        self.support_edges.push(support_edge);
        self.support_edges.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| b.priority.cmp(&a.priority))
                .then_with(|| {
                    b.evidence_quality
                        .partial_cmp(&a.evidence_quality)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        self.support_edges.truncate(3);
    }

    fn to_json(&self) -> Value {
        let direct_calls = self
            .reasons
            .iter()
            .filter(|reason| reason.contains(" calls: "))
            .count();
        let direct_references = self
            .reasons
            .iter()
            .filter(|reason| reason.contains(" references: "))
            .count();
        json!({
            "id": self.id,
            "name": self.name,
            "kind": self.kind,
            "file": self.file,
            "distance": self.distance,
            "score": self.score,
            "confidence": self.confidence,
            "reasons": self.reasons,
            "support_edges": self.support_edges.iter().map(|edge| edge.to_json()).collect::<Vec<_>>(),
            "provenance": {
                "direct_calls": direct_calls,
                "direct_references": direct_references,
                "dominant_signal": if direct_calls >= direct_references {
                    "calls"
                } else {
                    "references"
                },
            },
        })
    }
}

#[derive(Clone)]
struct ImpactFile {
    file: String,
    score: i32,
    usage_count: usize,
    confidence: f32,
    reasons: Vec<String>,
    support_edges: Vec<ImpactSupportEdge>,
}

impl ImpactFile {
    fn new(file: String) -> Self {
        Self {
            file,
            score: 0,
            usage_count: 0,
            confidence: 0.0,
            reasons: Vec::new(),
            support_edges: Vec::new(),
        }
    }

    fn observe(
        &mut self,
        score: i32,
        confidence: f32,
        reason: impl Into<String>,
        support_edge: Option<ImpactSupportEdge>,
    ) {
        self.usage_count += 1;
        self.score = self.score.max(score);
        self.confidence = self.confidence.max(confidence);
        self.reasons.push(reason.into());
        if let Some(edge) = support_edge {
            self.support_edges.push(edge);
            self.support_edges.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| b.priority.cmp(&a.priority))
                    .then_with(|| {
                        b.evidence_quality
                            .partial_cmp(&a.evidence_quality)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            self.support_edges.truncate(3);
        }
    }

    fn to_json(&self) -> Value {
        let direct_calls = self
            .reasons
            .iter()
            .filter(|reason| reason.contains(" calls: "))
            .count();
        let direct_references = self
            .reasons
            .iter()
            .filter(|reason| reason.contains(" references: "))
            .count();
        json!({
            "file": self.file,
            "score": self.score,
            "usage_count": self.usage_count,
            "confidence": self.confidence,
            "reasons": self.reasons,
            "support_edges": self.support_edges.iter().map(|edge| edge.to_json()).collect::<Vec<_>>(),
            "provenance": {
                "direct_calls": direct_calls,
                "direct_references": direct_references,
                "dominant_signal": if direct_calls >= direct_references {
                    "calls"
                } else {
                    "references"
                },
            },
        })
    }
}

fn build_scope_set(scope: Option<&str>) -> Option<GlobSet> {
    scope.map(|scope| {
        let mut builder = GlobSetBuilder::new();
        if let Ok(glob) = Glob::new(scope) {
            builder.add(glob);
        }
        builder
            .build()
            .unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
    })
}

fn matches_scope(path: &Path, project_path: &Path, scope_set: Option<&GlobSet>) -> bool {
    let Some(set) = scope_set else {
        return true;
    };
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let rel = canonical
        .strip_prefix(project_path)
        .unwrap_or(canonical.as_path());
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    set.is_match(rel_str.as_str()) || set.is_match(canonical.as_path())
}

fn collect_impact_neighbors(
    index: &Arc<crate::index::SymbolIndex>,
    symbol_id: &str,
    project_path: &Path,
    scope_set: Option<&GlobSet>,
) -> Vec<ImpactNeighbor> {
    let mut neighbors = Vec::new();

    for (direction, bucket) in [
        (
            ImpactDirection::Incoming,
            index.graph.incoming.get(symbol_id),
        ),
        (
            ImpactDirection::Outgoing,
            index.graph.outgoing.get(symbol_id),
        ),
    ] {
        for edge in bucket.into_iter().flatten() {
            let Some(symbol) = index.symbols.get(&edge.symbol_id) else {
                continue;
            };
            if !matches_scope(symbol.file.as_ref(), project_path, scope_set) {
                continue;
            }
            let metrics = navigation_edge_metrics(edge.relation, edge.confidence, &edge.evidence);
            neighbors.push(ImpactNeighbor {
                id: symbol.id.clone(),
                name: symbol.name.clone(),
                kind: symbol.kind.to_string(),
                file: symbol.file.to_string_lossy().replace('\\', "/"),
                direction,
                relation: edge.relation,
                evidence: edge.evidence.clone(),
                confidence: edge.confidence,
                evidence_quality: metrics.evidence_quality,
                priority: metrics.priority,
            });
        }
    }

    neighbors.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
    neighbors
}

fn impact_score(
    distance: usize,
    file: &str,
    name: &str,
    project_path: &std::path::Path,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
    query: &str,
) -> i32 {
    let mut score = 100 - distance as i32 * 20;
    if file.contains("/tests/") || file.ends_with("_test.rs") || file.ends_with("test.ts") {
        score += 10;
    }
    if name == "main" || name == "run" || name == "build" {
        score -= 10;
    }
    let file_path = std::path::Path::new(file);
    score +=
        crate::index::repo_profile::role_boost_for_path(project_path, file_path, profile, query);
    score
}

fn impact_edge_score(
    distance: usize,
    neighbor: &ImpactNeighbor,
    project_path: &std::path::Path,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
    query: &str,
) -> i32 {
    let metrics =
        navigation_edge_metrics(neighbor.relation, neighbor.confidence, &neighbor.evidence);
    impact_score(
        distance,
        &neighbor.file,
        &neighbor.name,
        project_path,
        profile,
        query,
    ) + metrics.priority
        + neighbor.direction.score_bonus()
        + (edge_evidence_quality(&neighbor.evidence) * 10.0).round() as i32
}

fn better_impact_path(candidate: (i32, usize, i32), existing: (i32, usize, i32)) -> bool {
    candidate.0 > existing.0
        || (candidate.0 == existing.0
            && (candidate.1 < existing.1
                || (candidate.1 == existing.1 && candidate.2 > existing.2)))
}

fn build_edge_provenance_summary(symbols: &[ImpactSymbol], files: &[ImpactFile]) -> Value {
    let mut direct_calls = 0;
    let mut direct_references = 0;
    for reason in symbols
        .iter()
        .flat_map(|symbol| symbol.reasons.iter())
        .chain(files.iter().flat_map(|file| file.reasons.iter()))
    {
        if reason.contains(" calls: ") {
            direct_calls += 1;
        } else if reason.contains(" references: ") {
            direct_references += 1;
        }
    }

    json!({
        "direct_calls": direct_calls,
        "direct_references": direct_references,
        "dominant_signal": if direct_calls >= direct_references {
            "calls"
        } else {
            "references"
        },
    })
}

#[allow(dead_code)]
fn candidate_target(candidate: &Value) -> Value {
    if candidate["kind"].as_str() == Some("repo_map") {
        if let Some(file_path) = candidate["primary_file"].as_str() {
            json!({
                "file_path": file_path,
                "kind": "file",
            })
        } else {
            json!({
                "intent": "project",
                "project_view": "repo_map",
            })
        }
    } else if candidate["kind"].as_str() == Some("file")
        || candidate["kind"].as_str() == Some("directory")
    {
        json!({
            "file_path": candidate["file"],
            "kind": candidate["kind"],
        })
    } else {
        json!({
            "symbol_id": candidate["id"],
            "name": candidate["name"],
            "file": candidate["file"],
        })
    }
}

fn contains_regex_meta(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return false;
    }

    trimmed.starts_with('^')
        || trimmed.ends_with('$')
        || trimmed.contains('\\')
        || (trimmed.contains('[') && trimmed.contains(']'))
        || (trimmed.contains('{') && trimmed.contains('}'))
        || trimmed.contains('|')
        || trimmed.contains(".*")
        || trimmed.contains(".+")
        || trimmed.contains(".?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::index_project::{index_project, load_project_index, IndexProjectParams};
    use tempfile::TempDir;

    async fn setup_project(dir: &TempDir) -> String {
        let project = dir.path().to_string_lossy().to_string();
        index_project(IndexProjectParams {
            path: project.clone(),
            exclude: None,
            force: Some(false),
            max_files: None,
            progress_token: None,
            peer: None,
            embed_config: None,
        })
        .await
        .unwrap();
        project
    }

    fn symbol_id_by_name(project: &str, name: &str) -> String {
        let index = load_project_index(project).unwrap();
        index
            .symbols
            .values()
            .find(|sym| sym.name == name)
            .unwrap()
            .id
            .clone()
    }

    #[test]
    fn test_choose_locate_route_prioritizes_repo_over_symbol() {
        let route = choose_locate_route(&Some("project".to_string()), "repo layout");
        assert!(matches!(route, LocateRoute::Project));
    }

    #[test]
    fn test_choose_locate_route_prefers_file_when_path_is_clear() {
        let route = choose_locate_route(&None, "src/lib.rs");
        assert!(matches!(route, LocateRoute::Files));
    }

    #[test]
    fn test_choose_navigation_route_prefers_read_when_target_known() {
        let route = choose_navigation_route(NavigationRouteContext {
            intent: Some("locate"),
            has_symbol: true,
            has_file: false,
            has_line_start: false,
            has_line_end: false,
            query: "hello",
            source: None,
            sink: None,
        });
        assert!(matches!(route, NavigationRoute::Read));
    }

    #[test]
    fn test_choose_navigation_route_prefers_trace_for_flow_questions() {
        let route = choose_navigation_route(NavigationRouteContext {
            intent: Some("trace"),
            has_symbol: false,
            has_file: false,
            has_line_start: false,
            has_line_end: false,
            query: "how does request flow",
            source: Some("router"),
            sink: Some("sink"),
        });
        assert!(matches!(route, NavigationRoute::Trace));
    }

    #[test]
    fn test_choose_navigation_route_keeps_identifier_like_queries_in_locate() {
        for query in ["impact_score", "callback", "path_policy"] {
            let route = choose_navigation_route(NavigationRouteContext {
                intent: None,
                has_symbol: false,
                has_file: false,
                has_line_start: false,
                has_line_end: false,
                query,
                source: None,
                sink: None,
            });
            assert!(matches!(route, NavigationRoute::Locate), "{query}");
        }
    }

    #[tokio::test]
    async fn test_locate_code_routes_to_symbol_search() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let result = locate_code(LocateCodeParams {
            project,
            query: "hello".to_string(),
            intent: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
        })
        .await
        .unwrap();

        assert!(result["summary"].as_str().is_some());
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
        assert!(result["results"][0]["id"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_locate_code_project_query_surfaces_repo_map() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "pub fn main() { bootstrap(); }\npub fn bootstrap() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config").join("settings.rs"),
            "pub fn load() {}\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = locate_code(LocateCodeParams {
            project,
            query: "repo layout".to_string(),
            intent: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
        })
        .await
        .unwrap();

        assert!(result["summary"].as_str().is_some());
        assert!(result["count"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_locate_code_project_query_prefers_config_directory_for_config_queries() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "pub fn main() { bootstrap(); }\npub fn bootstrap() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            dir.path().join("config").join("settings.rs"),
            "pub fn load() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("handlers")).unwrap();
        std::fs::write(
            dir.path().join("handlers").join("http.rs"),
            "pub fn serve() {}\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = locate_code(LocateCodeParams {
            project,
            query: "config overview".to_string(),
            intent: Some("project".to_string()),
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
        })
        .await
        .unwrap();

        assert!(result["summary"].as_str().is_some());
        assert!(result["count"].as_u64().unwrap() > 0);
    }

    #[test]
    fn test_build_locate_session_state_reports_novelty_bias() {
        let project = std::path::Path::new("/tmp/orchestrator-locate-session-state");
        crate::session::record_symbol(
            project,
            "known_handler",
            Some(std::path::Path::new("handlers/http.rs")),
        );
        let mut results = vec![
            json!({
                "kind": "symbol",
                "id": "known_handler",
                "name": "known_handler",
                "file": "handlers/http.rs",
                "path_role": "handler",
            }),
            json!({
                "kind": "symbol",
                "id": "fresh_handler",
                "name": "fresh_handler",
                "file": "handlers/router.rs",
                "path_role": "handler",
            }),
        ];
        let novelty_bias = promote_nearby_unseen_locate_candidate(&mut results, project);

        let state = build_locate_session_state(&results, project, novelty_bias).unwrap();

        assert_eq!(state["novelty_bias_applied"], json!(true));
        assert_eq!(state["top_target_seen"], json!(false));
        assert!(state["nearby_alternative"]["symbol_id"].is_string());
    }

    #[test]
    fn test_rerank_locate_symbol_results_prefers_entrypoints_for_startup_queries() {
        let project = std::path::Path::new("/tmp/orchestrator-rerank-startup");
        let profile = RepoProfile {
            archetype: crate::index::repo_profile::RepoArchetype::Cli,
            file_roles: HashMap::from([
                (
                    "main.rs".to_string(),
                    crate::index::repo_profile::PathRole::Entrypoint,
                ),
                (
                    "config/settings.rs".to_string(),
                    crate::index::repo_profile::PathRole::Config,
                ),
            ]),
            role_counts: HashMap::from([
                (crate::index::repo_profile::PathRole::Entrypoint, 1),
                (crate::index::repo_profile::PathRole::Config, 1),
            ]),
            entrypoints: vec!["main.rs".to_string()],
        };
        let mut results = vec![
            json!({
                "kind": "symbol",
                "id": "load_settings",
                "name": "load_settings",
                "file": "config/settings.rs",
                "path_role": "config",
            }),
            json!({
                "kind": "symbol",
                "id": "main",
                "name": "main",
                "file": "main.rs",
                "path_role": "entrypoint",
            }),
        ];

        rerank_locate_symbol_results(&mut results, project, "startup flow", Some(&profile));

        assert_eq!(results[0]["name"], json!("main"));
    }

    #[test]
    fn test_rerank_locate_symbol_results_prefers_recent_symbol_in_same_subsystem() {
        let project = std::path::Path::new("/tmp/orchestrator-rerank-session-symbol");
        crate::session::record_symbol(
            project,
            "config_loader",
            Some(std::path::Path::new("config/settings.rs")),
        );
        let profile = RepoProfile {
            archetype: crate::index::repo_profile::RepoArchetype::Service,
            file_roles: HashMap::from([
                (
                    "config/settings.rs".to_string(),
                    crate::index::repo_profile::PathRole::Config,
                ),
                (
                    "config/loader.rs".to_string(),
                    crate::index::repo_profile::PathRole::Config,
                ),
            ]),
            role_counts: HashMap::from([(crate::index::repo_profile::PathRole::Config, 2)]),
            entrypoints: vec![],
        };
        let mut results = vec![
            json!({
                "kind": "symbol",
                "id": "config_loader",
                "name": "config_loader",
                "file": "config/settings.rs",
                "path_role": "config",
            }),
            json!({
                "kind": "symbol",
                "id": "config_bootstrap",
                "name": "config_bootstrap",
                "file": "config/loader.rs",
                "path_role": "config",
            }),
        ];

        rerank_locate_symbol_results(&mut results, project, "config flow", Some(&profile));

        assert_eq!(results[0]["name"], json!("config_loader"));
    }

    #[test]
    fn test_fallback_symbol_mode_uses_semantic_only_for_vague_discovery_queries() {
        assert_eq!(fallback_symbol_mode("startup flow"), "semantic");
        assert_eq!(fallback_symbol_mode("impact_score"), "bm25");
        assert_eq!(fallback_symbol_mode("src/config/settings.rs"), "fuzzy");
        assert_eq!(fallback_symbol_mode("pub fn load()"), "fuzzy");
    }

    #[test]
    fn test_rerank_impact_seed_candidates_prefers_repo_aligned_symbols() {
        let profile = RepoProfile {
            archetype: crate::index::repo_profile::RepoArchetype::Service,
            file_roles: HashMap::from([
                (
                    "config/settings.rs".to_string(),
                    crate::index::repo_profile::PathRole::Config,
                ),
                (
                    "handlers/http.rs".to_string(),
                    crate::index::repo_profile::PathRole::Handler,
                ),
            ]),
            role_counts: HashMap::from([
                (crate::index::repo_profile::PathRole::Config, 1),
                (crate::index::repo_profile::PathRole::Handler, 1),
            ]),
            entrypoints: vec!["main.rs".to_string()],
        };
        let mut candidates = vec![
            json!({
                "kind": "file",
                "file": "config/settings.rs",
                "name": "settings.rs",
            }),
            json!({
                "kind": "symbol",
                "name": "serve_http",
                "file": "handlers/http.rs",
                "symbol_kind": "function",
            }),
            json!({
                "kind": "symbol",
                "name": "load_settings",
                "file": "config/settings.rs",
                "symbol_kind": "function",
            }),
        ];

        rerank_impact_seed_candidates(
            &mut candidates,
            "config impact",
            std::path::Path::new("."),
            Some(&profile),
        );

        assert_eq!(candidates[0]["name"], json!("load_settings"));
        assert_eq!(candidates[2]["file"], json!("config/settings.rs"));
    }

    #[test]
    fn test_rerank_project_directories_prefers_recent_subsystem_focus() {
        let project = std::path::Path::new("/tmp/orchestrator-rerank-session-dir");
        crate::session::record_file(project, std::path::Path::new("handlers/http.rs"));
        let repo_map = json!({
            "top_roles": [
                { "role": "config", "count": 1 },
                { "role": "handler", "count": 1 }
            ],
            "entrypoints": []
        });
        let mut results = vec![
            json!({ "kind": "directory", "dir": "config", "file": "config" }),
            json!({ "kind": "directory", "dir": "handlers", "file": "handlers" }),
        ];

        rerank_project_directories(&mut results, project, "overview", &repo_map);

        assert_eq!(results[0]["dir"], json!("handlers"));
    }

    #[test]
    fn test_promote_nearby_unseen_locate_candidate_prefers_unseen_sibling() {
        let project = std::path::Path::new("/tmp/orchestrator-locate-novelty");
        crate::session::record_symbol(
            project,
            "known_handler",
            Some(std::path::Path::new("handlers/http.rs")),
        );
        let mut results = vec![
            json!({
                "kind": "symbol",
                "id": "known_handler",
                "name": "known_handler",
                "file": "handlers/http.rs",
                "path_role": "handler",
            }),
            json!({
                "kind": "symbol",
                "id": "fresh_handler",
                "name": "fresh_handler",
                "file": "handlers/router.rs",
                "path_role": "handler",
            }),
            json!({
                "kind": "symbol",
                "id": "other_config",
                "name": "other_config",
                "file": "config/settings.rs",
                "path_role": "config",
            }),
        ];

        let promoted = promote_nearby_unseen_locate_candidate(&mut results, project);

        assert!(promoted);
        assert_eq!(results[0]["name"], json!("fresh_handler"));
    }

    #[tokio::test]
    async fn test_read_code_unit_returns_outline_for_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let result = read_code_unit(ReadCodeUnitParams {
            project,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert_eq!(result["file"].as_str().unwrap(), "lib.rs");
        assert_eq!(result["symbols"].as_array().unwrap().len(), 1);
        assert!(result["summary"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_read_code_unit_marks_repeated_outline_reads() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let first = read_code_unit(ReadCodeUnitParams {
            project: project.clone(),
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();
        let second = read_code_unit(ReadCodeUnitParams {
            project,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert!(first["summary"].as_str().is_some());
        assert!(second["summary"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_read_code_unit_compacts_large_file_outline() {
        let dir = TempDir::new().unwrap();
        let mut source = String::from("pub struct Root;\nimpl Root {\n");
        for i in 0..32 {
            source.push_str(&format!("    pub fn method_{i}(&self) {{}}\n"));
        }
        source.push_str("}\n");
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        let project = setup_project(&dir).await;

        let result = read_code_unit(ReadCodeUnitParams {
            project,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        let symbols = result["symbols"].as_array().unwrap();
        assert!(symbols.len() <= READ_CODE_UNIT_FILE_OUTLINE_LIMIT);
        assert_eq!(result["truncated"], json!(true));
        assert_eq!(result["returned_count"], json!(symbols.len()));
        assert!(result["count"].as_u64().unwrap() > symbols.len() as u64);
        assert_eq!(
            symbols[0]["kind"].as_str().unwrap(),
            "struct",
            "top-level declarations should be kept ahead of method-heavy noise"
        );
    }

    #[tokio::test]
    async fn test_read_code_unit_propagates_symbol_repeat_state() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = symbol_id_by_name(&project, "hello");

        let first = read_code_unit(ReadCodeUnitParams {
            project: project.clone(),
            symbol_id: Some(symbol_id.clone()),
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();
        let second = read_code_unit(ReadCodeUnitParams {
            project,
            symbol_id: Some(symbol_id),
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: Some(false),
        })
        .await
        .unwrap();

        assert!(first["source"].as_str().is_some());
        assert!(second["steering"]["why_this_matched"]
            .as_str()
            .unwrap()
            .contains("Repeat read"));
    }

    #[tokio::test]
    async fn test_read_code_unit_marks_changed_line_slice_reads() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let first = read_code_unit(ReadCodeUnitParams {
            project: project.clone(),
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: Some(1),
            line_end: Some(2),
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();
        std::fs::write(&file_path, "line1\nchanged\nline3\n").unwrap();
        let second = read_code_unit(ReadCodeUnitParams {
            project,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: Some(1),
            line_end: Some(2),
            include_context: None,
            signature_only: None,
        })
        .await
        .unwrap();

        assert_eq!(first["read_state"]["status"], json!("new"));
        assert_eq!(second["read_state"]["target_seen"], json!(true));
        assert_eq!(second["read_state"]["repeat_read"], json!(false));
        assert_eq!(second["read_state"]["changed_since_last_read"], json!(true));
        assert_eq!(second["read_state"]["status"], json!("changed"));
    }

    #[tokio::test]
    async fn test_analyze_impact_returns_seed_and_targets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = analyze_impact(AnalyzeImpactParams {
            project: project.clone(),
            query: None,
            symbol_id: Some(symbol_id_by_name(&project, "leaf")),
            file_path: None,
            scope: None,
            depth: Some(2),
            limit: Some(5),
        })
        .await
        .unwrap();

        assert!(!result["impact_files"].as_array().unwrap().is_empty());
        assert!(!result["seed_symbols"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_analyze_impact_prefers_direct_calls_over_reference_edges() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\npub fn wrapper(f: fn()) { f(); }\npub fn root() { wrapper(leaf); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = analyze_impact(AnalyzeImpactParams {
            project: project.clone(),
            query: None,
            symbol_id: Some(symbol_id_by_name(&project, "leaf")),
            file_path: None,
            scope: None,
            depth: Some(2),
            limit: Some(5),
        })
        .await
        .unwrap();

        let impact_symbols = result["impact_symbols"].as_array().unwrap();
        let names: Vec<&str> = impact_symbols
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert!(names.contains(&"branch"));
        assert!(names.contains(&"root"));
        assert_eq!(impact_symbols[0]["name"], json!("branch"));
        assert_eq!(
            impact_symbols[0]["support_edges"][0]["relation"],
            json!("calls")
        );
        assert_eq!(
            impact_symbols[0]["support_edges"][0]["direction"],
            json!("direct caller")
        );
        assert!(
            impact_symbols[0]["support_edges"][0]["evidence_quality"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.0
        );
    }

    #[tokio::test]
    async fn test_navigate_code_routes_to_read() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let result = navigate_code(NavigateCodeParams {
            project,
            query: "hello".to_string(),
            intent: None,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert_eq!(
            result["navigation_route"].as_str().unwrap(),
            "read_code_unit"
        );
        assert_eq!(result["file"].as_str().unwrap(), "lib.rs");
    }

    #[tokio::test]
    async fn test_navigate_code_repeat_read_prefers_expansion_over_reread() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;
        let symbol_id = symbol_id_by_name(&project, "hello");

        let first = navigate_code(NavigateCodeParams {
            project: project.clone(),
            query: "hello".to_string(),
            intent: None,
            symbol_id: Some(symbol_id.clone()),
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: Some(false),
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();
        let second = navigate_code(NavigateCodeParams {
            project,
            query: "hello".to_string(),
            intent: None,
            symbol_id: Some(symbol_id),
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: Some(false),
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert!(first["source"].as_str().is_some());
        assert!(second["steering"]["why_this_matched"]
            .as_str()
            .unwrap()
            .contains("Repeat read"));
    }

    #[tokio::test]
    async fn test_navigate_code_changed_read_keeps_focus_on_refreshed_payload() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let first = navigate_code(NavigateCodeParams {
            project: project.clone(),
            query: "lines".to_string(),
            intent: None,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: Some(1),
            line_end: Some(2),
            include_context: None,
            signature_only: None,
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();
        std::fs::write(&file_path, "line1\nchanged\nline3\n").unwrap();
        let second = navigate_code(NavigateCodeParams {
            project,
            query: "lines".to_string(),
            intent: None,
            symbol_id: None,
            file_path: Some("lib.rs".to_string()),
            line_start: Some(1),
            line_end: Some(2),
            include_context: None,
            signature_only: None,
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert_eq!(first["read_state"]["status"], json!("new"));
        assert_eq!(second["read_state"]["status"], json!("changed"));
        assert_eq!(second["session_state"]["target_changed"], json!(true));
        assert!(second["read_state"]["guidance"]
            .as_str()
            .unwrap()
            .contains("Use the refreshed payload"));
        assert!(second.get("steering").is_none() || second["steering"].is_null());
    }

    #[tokio::test]
    async fn test_trace_path_repeat_target_prefers_expansion_followup() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;
        let branch_id = symbol_id_by_name(&project, "branch");
        crate::session::record_symbol(
            std::path::Path::new(&project),
            &branch_id,
            Some(std::path::Path::new("lib.rs")),
        );

        let result = trace_path(TracePathParams {
            project,
            query: "branch flow".to_string(),
            source: Some("branch".to_string()),
            sink: Some("leaf".to_string()),
            language: None,
            file: None,
            max_symbols: Some(5),
            max_depth: Some(2),
        })
        .await
        .unwrap();

        assert!(result["summary"].as_str().is_some());
        assert!(result["important_symbols"].as_array().is_some());
    }

    #[tokio::test]
    async fn test_analyze_impact_repeat_target_prefers_expansion_followup() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;
        let branch_id = symbol_id_by_name(&project, "branch");
        let leaf_id = symbol_id_by_name(&project, "leaf");
        crate::session::record_symbol(
            std::path::Path::new(&project),
            &branch_id,
            Some(std::path::Path::new("lib.rs")),
        );

        let result = analyze_impact(AnalyzeImpactParams {
            project,
            query: None,
            symbol_id: Some(leaf_id),
            file_path: None,
            scope: None,
            depth: Some(2),
            limit: Some(5),
        })
        .await
        .unwrap();

        assert_eq!(result["session_state"]["top_target_seen"], json!(true));
        assert_eq!(
            result["steering"]["recommended_next_tool"],
            json!("find_usages")
        );
    }

    #[tokio::test]
    async fn test_navigate_code_routes_to_trace() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = navigate_code(NavigateCodeParams {
            project: project.clone(),
            query: "how does branch flow".to_string(),
            intent: Some("trace".to_string()),
            symbol_id: None,
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
            source: Some("branch".to_string()),
            sink: Some("leaf".to_string()),
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert_eq!(result["navigation_route"].as_str().unwrap(), "trace_path");
        assert!(result["summary"].as_str().is_some());
        assert!(result["important_symbols"].as_array().is_some());
    }

    #[tokio::test]
    async fn test_navigate_code_routes_to_impact() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn leaf() {}\npub fn branch() { leaf(); }\n",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        let result = navigate_code(NavigateCodeParams {
            project,
            query: "impact leaf".to_string(),
            intent: Some("impact".to_string()),
            symbol_id: None,
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert_eq!(
            result["navigation_route"].as_str().unwrap(),
            "analyze_impact"
        );
        assert!(!result["impact_symbols"].as_array().unwrap().is_empty());
        assert!(
            result["edge_provenance_summary"]["direct_calls"]
                .as_u64()
                .unwrap_or(0)
                >= 1
        );
        assert!(
            result["impact_symbols"][0]["provenance"]["direct_calls"]
                .as_u64()
                .unwrap_or(0)
                >= 1
        );
        assert_eq!(
            result["impact_symbols"][0]["provenance"]["dominant_signal"]
                .as_str()
                .unwrap(),
            "calls"
        );
    }

    #[tokio::test]
    async fn test_navigate_code_routes_to_locate() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        let project = setup_project(&dir).await;

        let result = navigate_code(NavigateCodeParams {
            project,
            query: "hello".to_string(),
            intent: None,
            symbol_id: None,
            file_path: None,
            line_start: None,
            line_end: None,
            include_context: None,
            signature_only: None,
            source: None,
            sink: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(5),
            max_symbols: Some(5),
            max_depth: Some(2),
            depth: Some(2),
        })
        .await
        .unwrap();

        assert_eq!(result["navigation_route"].as_str().unwrap(), "locate_code");
        assert!(result["summary"].as_str().is_some());
    }

    #[test]
    fn test_contains_regex_meta_requires_explicit_regex_signals() {
        for literal in ["C++", "foo?bar", "do(something)", "impact_score"] {
            assert!(!contains_regex_meta(literal), "{literal}");
        }

        for pattern in [r"^foo$", r"[A-Z]+", r"\bword\b", r"foo.*bar", r"(foo|bar)"] {
            assert!(contains_regex_meta(pattern), "{pattern}");
        }
    }
}
