use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use crate::embed::EmbedConfig;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};

use crate::graph::{edge_evidence_quality, navigation_edge_metrics, EdgeRelation};
use crate::index::format::load_project_meta;
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

    let route = choose_locate_route(&params.intent, &query);
    let limit = params.limit.unwrap_or(5).clamp(1, 8);
    let mut route_used = route.as_str().to_string();
    let mut results = match route {
        LocateRoute::Project => locate_project(&params, limit).await?,
        LocateRoute::Files => locate_files(&params, limit).await?,
        LocateRoute::Content => locate_content(&params, limit).await?,
        LocateRoute::Symbols { ref mode } => locate_symbols(&params, limit, mode.as_str()).await?,
    };

    if results.is_empty() {
        let fallback = match route {
            LocateRoute::Project => LocateRoute::Files,
            LocateRoute::Files | LocateRoute::Content => LocateRoute::Symbols {
                mode: "semantic".to_string(),
            },
            LocateRoute::Symbols { .. } => LocateRoute::Symbols {
                mode: "semantic".to_string(),
            },
        };
        let fallback_route = fallback.as_str().to_string();
        let fallback_results = match fallback {
            LocateRoute::Project => locate_project(&params, limit).await?,
            LocateRoute::Files => locate_files(&params, limit).await?,
            LocateRoute::Content => locate_content(&params, limit).await?,
            LocateRoute::Symbols { ref mode } => {
                locate_symbols(&params, limit, mode.as_str()).await?
            }
        };
        if !fallback_results.is_empty() {
            route_used = fallback_route;
            results = fallback_results;
        }
    }

    let next_tool = if results.is_empty() {
        match route_used.as_str() {
            "search_files" => "search_symbols",
            "search_content" => "search_symbols",
            "get_project_outline" => "search_files",
            _ => "search_symbols",
        }
    } else {
        match results[0]["kind"].as_str().unwrap_or("symbol") {
            "directory" => "search_files",
            "file" | "content" => "read_code_unit",
            _ => "read_code_unit",
        }
    };

    let steering = if results.is_empty() {
        build_steering(
            0.2,
            "The router did not recover a strong code unit, so this is a weak discovery result."
                .to_string(),
            next_tool,
            json!({ "query": query, "route_used": route_used }),
            take_fallback_candidates(&results),
        )
    } else {
        build_steering(
            0.88,
            format!(
                "{} routed the query to the most likely code lookup path.",
                route_used
            ),
            next_tool,
            candidate_target(&results[0]),
            take_fallback_candidates(&results),
        )
    };

    let mut response = json!({
        "query": query,
        "intent": params.intent,
        "route_used": route_used,
        "results": results,
        "count": results.len(),
    });
    let canonical = resolve_project_path(&params.project)?;
    session::record_query(&canonical, &params.query);
    session::record_files(
        &canonical,
        response["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        &canonical,
        response["results"]
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
    Ok(response)
}

pub async fn read_code_unit(params: ReadCodeUnitParams) -> anyhow::Result<Value> {
    let canonical = resolve_project_path(&params.project)?;

    if let Some(symbol_id) = params.symbol_id {
        let response = get_symbol(GetSymbolParams {
            project: params.project,
            symbol_id,
            include_context: params.include_context,
            signature_only: params.signature_only,
        })
        .await?;
        return Ok(response);
    }

    let Some(file_path) = params.file_path else {
        return Err(anyhow::anyhow!(
            "read_code_unit requires symbol_id or file_path"
        ));
    };

    if let (Some(line_start), Some(line_end)) = (params.line_start, params.line_end) {
        let file_path_for_record = file_path.clone();
        let response = get_lines(GetLinesParams {
            project: params.project,
            file_path: file_path.clone(),
            line_start,
            line_end,
        })
        .await?;
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

    attach_steering(
        &mut response,
        build_steering(
            0.76,
            "File structure was returned; inspect a symbol next if you need implementation detail."
                .to_string(),
            "locate_code",
            json!({ "file_path": file_path }),
            take_fallback_candidates(&outline_symbols),
        ),
    );
    session::record_file(&canonical, Path::new(&file_path));

    Ok(response)
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

    let mut response = trace_execution_path(TraceExecutionPathParams {
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
    response["query"] = json!(query);
    if let Some(source) = source_hint {
        response["source_hint"] = json!(source);
    }
    if let Some(sink) = sink_hint {
        response["sink_hint"] = json!(sink);
    }
    session::record_query(&canonical, &query);
    session::record_files(
        &canonical,
        response["important_symbols"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["file"].as_str().map(ToOwned::to_owned)),
    );
    session::record_symbols(
        &canonical,
        response["important_symbols"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let id = item["id"].as_str()?.to_string();
                let file = item["file"].as_str().map(ToOwned::to_owned);
                Some((id, file))
            }),
    );
    Ok(response)
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
    let mut queue: VecDeque<(String, usize, i32, String)> = VecDeque::new();
    let mut best_paths: HashMap<String, i32> = HashMap::new();

    for seed in &seeds {
        queue.push_back((seed.id.clone(), 0, 120, "seed".to_string()));
        best_paths.insert(seed.id.clone(), 120);
        impacted_files
            .entry(seed.file.clone())
            .or_insert_with(|| ImpactFile::new(seed.file.clone()))
            .observe(100, 1.0, "seed symbol");
    }

    while let Some((symbol_id, depth, path_score, provenance)) = queue.pop_front() {
        if depth >= depth_limit {
            continue;
        }

        for neighbor in collect_impact_neighbors(&index, &symbol_id, &canonical, scope_set.as_ref())
        {
            let distance = depth + 1;
            let score = impact_edge_score(distance, &neighbor, &canonical, profile.as_ref(), query);
            let next_path_score = path_score + score - (distance as i32 * 8);
            let reason = format!(
                "{} {}: {}",
                neighbor.direction_label(),
                neighbor.relation.as_str(),
                neighbor.evidence
            );
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
            );
            impacted_files
                .entry(neighbor.file.clone())
                .or_insert_with(|| ImpactFile::new(neighbor.file.clone()))
                .observe(
                    score,
                    neighbor.confidence.max(neighbor.evidence_quality),
                    reason.clone(),
                );
            let should_expand = best_paths
                .get(&neighbor.id)
                .is_none_or(|best| next_path_score > *best);
            if should_expand {
                best_paths.insert(neighbor.id.clone(), next_path_score);
                queue.push_back((
                    neighbor.id.clone(),
                    distance,
                    next_path_score,
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
    Ok(response)
}

pub async fn navigate_code(params: NavigateCodeParams) -> anyhow::Result<Value> {
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
    }
    Ok(response)
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
    let words = query.split_whitespace().count();
    words >= 3 || query.contains('\"') || query.contains('\'') || query.contains("=>")
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

    let results = result["directories"]
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
    Ok(results)
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
    .await?;

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
                "language": sym["language"],
                "file": sym["file"],
                "line_start": sym["line_start"],
                "line_end": sym["line_end"],
                "signature": sym["signature"],
                "source_tool": "search_symbols",
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
                        "language": sym["language"],
                        "file": sym["file"],
                        "line_start": sym["line_start"],
                        "line_end": sym["line_end"],
                        "signature": sym["signature"],
                        "source_tool": "search_symbols",
                    })
                })
                .collect::<Vec<_>>();
        }
    }

    Ok(results)
}

async fn resolve_impact_seeds(params: &AnalyzeImpactParams) -> anyhow::Result<Vec<ImpactSeed>> {
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
        let seeds = outline["symbols"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(3)
            .filter_map(|sym| {
                Some(ImpactSeed {
                    id: sym["id"].as_str()?.to_string(),
                    name: sym["name"]
                        .as_str()
                        .unwrap_or(sym["id"].as_str()?)
                        .to_string(),
                    kind: sym["kind"].as_str().unwrap_or("symbol").to_string(),
                    file: file_path.to_string(),
                })
            })
            .collect::<Vec<_>>();
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
        let mut seeds = Vec::new();
        for candidate in located["results"].as_array().into_iter().flatten() {
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
                for sym in outline["symbols"].as_array().into_iter().flatten().take(3) {
                    if let Some(id) = sym["id"].as_str() {
                        seeds.push(ImpactSeed {
                            id: id.to_string(),
                            name: sym["name"].as_str().unwrap_or(id).to_string(),
                            kind: sym["kind"].as_str().unwrap_or("symbol").to_string(),
                            file: file.to_string(),
                        });
                    }
                }
            }
        }
        if !seeds.is_empty() {
            return Ok(seeds);
        }
    }

    Ok(Vec::new())
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
struct ImpactSymbol {
    id: String,
    name: String,
    kind: String,
    file: String,
    distance: usize,
    score: i32,
    confidence: f32,
    reasons: Vec<String>,
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
        }
    }

    fn observe(
        &mut self,
        score: i32,
        distance: usize,
        confidence: f32,
        reason: impl Into<String>,
        provenance: String,
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
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "kind": self.kind,
            "file": self.file,
            "distance": self.distance,
            "score": self.score,
            "confidence": self.confidence,
            "reasons": self.reasons,
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
}

impl ImpactFile {
    fn new(file: String) -> Self {
        Self {
            file,
            score: 0,
            usage_count: 0,
            confidence: 0.0,
            reasons: Vec::new(),
        }
    }

    fn observe(&mut self, score: i32, confidence: f32, reason: impl Into<String>) {
        self.usage_count += 1;
        self.score = self.score.max(score);
        self.confidence = self.confidence.max(confidence);
        self.reasons.push(reason.into());
    }

    fn to_json(&self) -> Value {
        json!({
            "file": self.file,
            "score": self.score,
            "usage_count": self.usage_count,
            "confidence": self.confidence,
            "reasons": self.reasons,
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

fn candidate_target(candidate: &Value) -> Value {
    if candidate["kind"].as_str() == Some("file") || candidate["kind"].as_str() == Some("directory")
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

        assert_eq!(result["route_used"].as_str().unwrap(), "search_symbols");
        assert_eq!(result["results"].as_array().unwrap().len(), 1);
        assert_eq!(result["results"][0]["kind"].as_str().unwrap(), "symbol");
        assert_eq!(
            result["steering"]["recommended_next_tool"]
                .as_str()
                .unwrap(),
            "read_code_unit"
        );
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
        assert_eq!(
            result["steering"]["recommended_next_tool"]
                .as_str()
                .unwrap(),
            "locate_code"
        );
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
        assert_eq!(result["source_hint"].as_str().unwrap(), "branch");
        assert_eq!(result["sink_hint"].as_str().unwrap(), "leaf");
        let shortest_path = result["shortest_path"].as_array().unwrap();
        assert_eq!(
            shortest_path[0]["symbol_id"],
            json!(symbol_id_by_name(&project, "branch"))
        );
        assert_eq!(
            shortest_path.last().unwrap()["symbol_id"],
            json!(symbol_id_by_name(&project, "leaf"))
        );
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
        assert_eq!(result["route_used"].as_str().unwrap(), "search_symbols");
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
