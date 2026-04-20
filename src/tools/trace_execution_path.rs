use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::graph::{
    collect_direct_callable_references, collect_incoming_callable_references,
    navigation_edge_metrics, read_symbol_source, EdgeRelation,
};
use crate::index::format::load_project_meta;
use crate::index::repo_profile::{
    profile_entrypoints, role_boost_for_path, role_label, summarize_role_counts, RepoProfile,
};
use crate::index::SymbolIndex;
use crate::indexer::language::Symbol;
use crate::path_policy::resolve_project_path;
use crate::session;
use crate::tools::index_project::load_project_index;
use crate::tools::search_symbols::{search_symbols, SearchSymbolsParams};
use crate::tools::steering::{attach_steering, build_steering};

const DEFAULT_MAX_SYMBOLS: usize = 6;
const DEFAULT_MAX_DEPTH: usize = 2;
const DEFAULT_SEED_COUNT: usize = 3;

pub struct TraceExecutionPathParams {
    pub project: String,
    pub query: String,
    pub source: Option<String>,
    pub sink: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub max_symbols: Option<usize>,
    pub max_depth: Option<usize>,
    pub embed_config: Option<Arc<EmbedConfig>>,
}

#[derive(Clone)]
struct TraceNode {
    symbol_id: String,
    score: i32,
    category: &'static str,
    why: String,
    distance: usize,
    evidence_hits: usize,
    best_path_cost: u32,
    best_path_priority: i32,
}

#[derive(Clone)]
struct TraceEdge {
    from_id: String,
    to_id: String,
    relation: EdgeRelation,
    evidence: String,
    confidence: f32,
    evidence_quality: f32,
    priority: i32,
    path_cost: u32,
}

struct TraceContext<'a> {
    project_path: &'a std::path::Path,
    profile: Option<&'a crate::index::repo_profile::RepoProfile>,
}

struct TraceNodeEvidence {
    confidence: f32,
    path_cost: u32,
    path_priority: i32,
    why: String,
}

pub async fn trace_execution_path(params: TraceExecutionPathParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);
    let max_symbols = params.max_symbols.unwrap_or(DEFAULT_MAX_SYMBOLS).max(1);
    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH).min(3);

    let (discovery_mode, seed_ids) = discover_seed_ids(&params).await?;
    let mut nodes: HashMap<String, TraceNode> = HashMap::new();
    let mut edges: Vec<TraceEdge> = Vec::new();
    let mut seen_edges: HashSet<(String, String, EdgeRelation)> = HashSet::new();
    let trace_ctx = TraceContext {
        project_path: &canonical,
        profile: profile.as_ref(),
    };

    let seeds: Vec<&Symbol> = seed_ids
        .iter()
        .filter_map(|id| index.symbols.get(id))
        .collect();

    for seed in &seeds {
        upsert_node(
            &mut nodes,
            seed,
            adjusted_score(seed, 100, &params.query, profile.as_ref(), &canonical),
            classify_symbol(seed),
            0,
            TraceNodeEvidence {
                confidence: 1.0,
                path_cost: 0,
                path_priority: 0,
                why: "discovered as a strong seed for the requested behavior".to_string(),
            },
        );
        trace_callers(
            &index,
            seed,
            &mut nodes,
            &mut edges,
            &mut seen_edges,
            &trace_ctx,
            0,
        );
        trace_callees(
            &index,
            seed,
            max_depth,
            &mut nodes,
            &mut edges,
            &mut seen_edges,
            &trace_ctx,
        );
    }

    let important = select_important_symbols(&index, &nodes, max_symbols);
    let important_ids: HashSet<&str> = important.iter().map(|item| item.id.as_str()).collect();
    let important_edge_records: Vec<TraceEdge> = edges
        .into_iter()
        .filter(|edge| {
            important_ids.contains(edge.from_id.as_str())
                && important_ids.contains(edge.to_id.as_str())
        })
        .collect();
    let important_edges: Vec<Value> = important_edge_records
        .iter()
        .map(|edge| {
            json!({
                "from_id": edge.from_id,
                "to_id": edge.to_id,
                "relation": edge.relation.as_str(),
                "evidence": edge.evidence,
                "confidence": edge.confidence,
                "evidence_quality": edge.evidence_quality,
                "priority": edge.priority,
                "path_cost": edge.path_cost,
            })
        })
        .collect();
    let shortest_path = build_shortest_path(
        &important,
        &important_edge_records,
        &params.query,
        params.source.as_deref(),
        params.sink.as_deref(),
    );
    let path_narrative = build_path_narrative(&important, Some(shortest_path.as_slice()));

    let fallback_candidates: Vec<Value> = important
        .iter()
        .take(3)
        .map(|item| {
            json!({
                "id": item.id,
                "name": item.name,
                "kind": item.kind,
                "file": item.file,
                "category": item.category,
                "confidence": item.confidence,
                "why": item.why,
            })
        })
        .collect();
    let steering = if let Some(top) = important.first() {
        build_steering(
            if important_edges.is_empty() {
                0.72
            } else {
                0.91
            },
            "The traced symbols and edges provide a grounded execution path for the requested behavior."
                .to_string(),
            "get_symbol",
            json!({
                "symbol_id": top.id,
                "name": top.name,
                "file": top.file,
            }),
            fallback_candidates,
        )
    } else {
        build_steering(
            0.24,
            "The path tracer did not recover a strong execution chain, so this is a weak discovery result."
                .to_string(),
            "search_symbols",
            json!({ "query": params.query }),
            fallback_candidates,
        )
    };

    let mut response = json!({
        "query": params.query,
        "discovery_mode": discovery_mode,
        "seed_symbol_ids": seed_ids,
        "summary": build_summary(&important, &important_edges),
        "path_narrative": path_narrative,
        "shortest_path": shortest_path,
        "important_symbols": important.into_iter().map(|item| item.into_json()).collect::<Vec<_>>(),
        "edges": important_edges,
        "guidance": {
            "next_step": "You likely have enough to answer from the traced symbols, snippets, and summary. Only call get_symbol for one or two symbols if you need to verify a specific implementation detail.",
            "avoid": "Avoid repeating search_symbols/search_content for adjacent concepts until you have used the traced symbols and edges in your answer.",
            "answer_now_hint": "Prefer answering from the returned important_symbols, edges, and summary before doing more discovery."
        }
    });
    session::record_query(&canonical, &params.query);
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
    attach_steering(&mut response, steering);

    Ok(response)
}

/// Extract likely symbol-name terms from a natural-language query.
///
/// BM25 search works poorly with long natural-language queries because the
/// signal gets diluted. This function extracts words that look like code
/// identifiers (CamelCase, snake_case, ALL_CAPS) and returns them as a
/// shorter, more focused query for BM25 fallback.
fn extract_symbol_terms(query: &str) -> Option<String> {
    let stop_words: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "shall", "can", "need", "dare", "ought",
        "used", "to", "of", "in", "for", "on", "with", "at", "by", "from",
        "as", "into", "through", "during", "before", "after", "above", "below",
        "between", "out", "off", "over", "under", "again", "further", "then",
        "once", "here", "there", "when", "where", "why", "how", "all", "each",
        "every", "both", "few", "more", "most", "other", "some", "such", "no",
        "nor", "not", "only", "own", "same", "so", "than", "too", "very",
        "and", "but", "or", "if", "while", "that", "which", "what", "this",
        "these", "those", "it", "its", "uses", "using", "main", "entry",
        "point", "execution", "path", "flow", "implementation",
    ];

    let terms: Vec<&str> = query
        .split_whitespace()
        .filter(|w| {
            let lower = w.to_lowercase();
            // Keep words that look like identifiers
            let is_camel = w.chars().any(|c| c.is_uppercase())
                && w.chars().any(|c| c.is_lowercase())
                && w.chars().all(|c| c.is_alphanumeric() || c == '_');
            let is_snake = w.contains('_') && w.chars().all(|c| c.is_alphanumeric() || c == '_');
            let is_short_code = w.len() <= 2 && w.chars().all(|c| c.is_uppercase());

            (is_camel || is_snake || is_short_code)
                && !stop_words.contains(&lower.as_str())
        })
        .collect();

    if terms.is_empty() {
        // Fall back to just non-stop-words
        let fallback: Vec<&str> = query
            .split_whitespace()
            .filter(|w| {
                let lower = w.to_lowercase();
                w.len() > 3 && !stop_words.contains(&lower.as_str())
            })
            .take(3)
            .collect();
        if fallback.is_empty() {
            None
        } else {
            Some(fallback.join(" "))
        }
    } else {
        Some(terms.join(" "))
    }
}

async fn discover_seed_ids(
    params: &TraceExecutionPathParams,
) -> anyhow::Result<(&'static str, Vec<String>)> {
    let mut ids = Vec::new();
    let mut discovered_modes = Vec::new();
    let canonical = resolve_project_path(&params.project)?;
    let profile = load_project_meta(&canonical)
        .ok()
        .map(|meta| meta.repo_profile);

    for query in [
        Some(params.query.as_str()),
        params.source.as_deref(),
        params.sink.as_deref(),
    ] {
        let Some(query) = query.filter(|q| !q.trim().is_empty()) else {
            continue;
        };
        let semantic = search_symbols(SearchSymbolsParams {
            project: params.project.clone(),
            query: query.to_string(),
            kind: None, // Search all symbol kinds — traces often involve structs, methods, and impls
            language: params.language.clone(),
            file: params.file.clone(),
            limit: Some(DEFAULT_SEED_COUNT),
            offset: Some(0),
            mode: Some("semantic".to_string()),
            embed_config: params.embed_config.clone(),
        })
        .await;

        if let Ok(mut response) = semantic {
            rerank_seed_response(&mut response, &canonical, query, profile.as_ref());
            let candidate_ids = extract_symbol_ids(&response);
            if !candidate_ids.is_empty() {
                discovered_modes.push("semantic");
                ids.extend(candidate_ids);
                continue;
            }
        }

        // Try BM25 with the full query first, then with extracted symbol terms.
        let bm25_query = query.to_string();
        let bm25 = search_symbols(SearchSymbolsParams {
            project: params.project.clone(),
            query: bm25_query.clone(),
            kind: None,
            language: params.language.clone(),
            file: params.file.clone(),
            limit: Some(DEFAULT_SEED_COUNT),
            offset: Some(0),
            mode: Some("bm25".to_string()),
            embed_config: params.embed_config.clone(),
        })
        .await?;
        let mut bm25 = bm25;
        rerank_seed_response(&mut bm25, &canonical, query, profile.as_ref());
        let mut candidate_ids = extract_symbol_ids(&bm25);

        // If BM25 with the full query found nothing, try with extracted symbol terms.
        if candidate_ids.is_empty() {
            if let Some(terms) = extract_symbol_terms(query) {
                // Try each extracted term individually — BM25 multi-word
                // queries require all terms to match, which often fails.
                for term in terms.split_whitespace().take(3) {
                    let term_bm25 = search_symbols(SearchSymbolsParams {
                        project: params.project.clone(),
                        query: term.to_string(),
                        kind: None,
                        language: params.language.clone(),
                        file: params.file.clone(),
                        limit: Some(DEFAULT_SEED_COUNT),
                        offset: Some(0),
                        mode: Some("bm25".to_string()),
                        embed_config: params.embed_config.clone(),
                    })
                    .await?;
                    let mut term_bm25 = term_bm25;
                    rerank_seed_response(&mut term_bm25, &canonical, query, profile.as_ref());
                    let term_ids = extract_symbol_ids(&term_bm25);
                    if !term_ids.is_empty() {
                        candidate_ids.extend(term_ids);
                        break; // One good term is enough for seeding
                    }
                }
            }
        }

        if !candidate_ids.is_empty() {
            discovered_modes.push("bm25");
            ids.extend(candidate_ids);
        }
    }

    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        Ok(("bm25", ids))
    } else if discovered_modes.contains(&"semantic") {
        Ok(("semantic", ids))
    } else {
        Ok(("bm25", ids))
    }
}

fn extract_symbol_ids(response: &Value) -> Vec<String> {
    response["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["id"].as_str().map(ToOwned::to_owned))
        .collect()
}

fn rerank_seed_response(
    response: &mut Value,
    project_path: &std::path::Path,
    query: &str,
    profile: Option<&RepoProfile>,
) {
    let Some(results) = response["results"].as_array_mut() else {
        return;
    };
    rerank_seed_candidates(results, project_path, query, profile);
}

fn rerank_seed_candidates(
    results: &mut [Value],
    project_path: &std::path::Path,
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
        let left_score = seed_repo_score(
            left,
            project_path,
            &query_lower,
            &dominant_roles,
            entrypoints.as_slice(),
        );
        let right_score = seed_repo_score(
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

fn seed_repo_score(
    candidate: &Value,
    project_path: &std::path::Path,
    query_lower: &str,
    dominant_roles: &[(String, usize)],
    entrypoints: &[String],
) -> i32 {
    let role = candidate["path_role"].as_str().unwrap_or("");
    let file = candidate["file"].as_str().unwrap_or("");
    let mut score = 0;

    for (rank, (dominant_role, count)) in dominant_roles.iter().enumerate() {
        if role == dominant_role {
            score += 16 - rank as i32 * 4 + (*count as i32).min(6);
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

    if let Some(symbol_id) = candidate["id"].as_str() {
        score += session::symbol_boost(project_path, symbol_id, Some(file.as_ref()));
    } else {
        score += session::file_boost(project_path, file.as_ref());
    }

    if role == role_label(crate::index::repo_profile::PathRole::Bootstrap)
        && (query_lower.contains("start")
            || query_lower.contains("boot")
            || query_lower.contains("init"))
    {
        score += 14;
    }

    score
}

fn trace_callers(
    index: &Arc<SymbolIndex>,
    seed: &Symbol,
    nodes: &mut HashMap<String, TraceNode>,
    edges: &mut Vec<TraceEdge>,
    seen_edges: &mut HashSet<(String, String, EdgeRelation)>,
    ctx: &TraceContext<'_>,
    depth: usize,
) {
    if depth > 1 {
        return;
    }
    for reference in collect_incoming_callable_references(index, seed) {
        let Some(candidate) = index.symbols.get(&reference.id) else {
            continue;
        };
        if is_noise_symbol(candidate) && !is_entry_symbol(candidate) {
            continue;
        }
        let metrics = navigation_edge_metrics(
            EdgeRelation::Calls,
            reference.confidence,
            &reference.evidence,
        );
        upsert_node(
            nodes,
            candidate,
            adjusted_score(
                candidate,
                90 - depth as i32 * 10
                    + (reference.confidence * 10.0) as i32
                    + metrics.priority / 4
                    - (metrics.path_cost as i32 / 12),
                &seed.qualified,
                ctx.profile,
                ctx.project_path,
            ),
            classify_symbol(candidate),
            depth + 1,
            TraceNodeEvidence {
                confidence: reference.confidence,
                path_cost: metrics.path_cost,
                path_priority: metrics.priority,
                why: format!(
                    "direct caller of {}: {}",
                    seed.qualified, reference.evidence
                ),
            },
        );
        push_edge(
            seen_edges,
            edges,
            &candidate.id,
            &seed.id,
            EdgeRelation::Calls,
            reference.evidence.clone(),
            reference.confidence,
        );
    }
}

fn trace_callees(
    index: &Arc<SymbolIndex>,
    seed: &Symbol,
    max_depth: usize,
    nodes: &mut HashMap<String, TraceNode>,
    edges: &mut Vec<TraceEdge>,
    seen_edges: &mut HashSet<(String, String, EdgeRelation)>,
    ctx: &TraceContext<'_>,
) {
    let mut frontier = vec![(seed.id.clone(), 0usize, 0_u32, 0_i32)];
    let mut best: HashMap<String, (u32, usize, i32)> =
        HashMap::from([(seed.id.clone(), (0, 0, 0))]);

    while !frontier.is_empty() {
        let (best_idx, _) = frontier
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                left.2
                    .cmp(&right.2)
                    .then(left.1.cmp(&right.1))
                    .then(right.3.cmp(&left.3))
                    .then(left.0.cmp(&right.0))
            })
            .unwrap_or((0, &(String::new(), 0, 0, 0)));
        let (current_id, depth, current_cost, current_priority) = frontier.swap_remove(best_idx);
        let Some(best_state) = best.get(&current_id) else {
            continue;
        };
        if *best_state != (current_cost, depth, current_priority) {
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        let Some(current) = index.symbols.get(&current_id) else {
            continue;
        };
        for reference in collect_direct_callable_references(index, current) {
            let Some(target) = index.symbols.get(&reference.id) else {
                continue;
            };
            if is_noise_symbol(target) && !is_entry_symbol(target) {
                continue;
            }
            let metrics = navigation_edge_metrics(
                EdgeRelation::Calls,
                reference.confidence,
                &reference.evidence,
            );
            let candidate_cost = current_cost + metrics.path_cost;
            let candidate_priority = current_priority + metrics.priority;
            let candidate_state = (candidate_cost, depth + 1, candidate_priority);
            upsert_node(
                nodes,
                target,
                adjusted_score(
                    target,
                    80 - depth as i32 * 10
                        + (reference.confidence * 10.0) as i32
                        + candidate_priority / 4
                        - (candidate_cost as i32 / 10),
                    &current.qualified,
                    ctx.profile,
                    ctx.project_path,
                ),
                classify_symbol(target),
                depth + 1,
                TraceNodeEvidence {
                    confidence: reference.confidence,
                    path_cost: candidate_cost,
                    path_priority: candidate_priority,
                    why: format!(
                        "direct callee of {}: {}",
                        current.qualified, reference.evidence
                    ),
                },
            );
            push_edge(
                seen_edges,
                edges,
                &current.id,
                &target.id,
                EdgeRelation::Calls,
                reference.evidence.clone(),
                reference.confidence,
            );
            let should_expand = best
                .get(&target.id)
                .is_none_or(|existing| better_path(candidate_state, *existing));
            if should_expand {
                best.insert(target.id.clone(), candidate_state);
                frontier.push((
                    target.id.clone(),
                    depth + 1,
                    candidate_cost,
                    candidate_priority,
                ));
            }
        }
    }
}

fn push_edge(
    seen_edges: &mut HashSet<(String, String, EdgeRelation)>,
    edges: &mut Vec<TraceEdge>,
    from_id: &str,
    to_id: &str,
    relation: EdgeRelation,
    evidence: String,
    confidence: f32,
) {
    let key = (from_id.to_string(), to_id.to_string(), relation);
    if seen_edges.insert(key.clone()) {
        let metrics = navigation_edge_metrics(relation, confidence, &evidence);
        edges.push(TraceEdge {
            from_id: key.0,
            to_id: key.1,
            relation,
            evidence,
            confidence,
            evidence_quality: metrics.evidence_quality,
            priority: metrics.priority,
            path_cost: metrics.path_cost,
        });
    }
}

fn upsert_node(
    nodes: &mut HashMap<String, TraceNode>,
    sym: &Symbol,
    score: i32,
    category: &'static str,
    distance: usize,
    evidence: TraceNodeEvidence,
) {
    nodes
        .entry(sym.id.clone())
        .and_modify(|node| {
            if score > node.score {
                node.score = score;
                node.category = category;
                node.why = evidence.why.clone();
            }
            node.distance = node.distance.min(distance);
            node.evidence_hits += 1;
            node.best_path_cost = node.best_path_cost.min(evidence.path_cost);
            node.best_path_priority = node.best_path_priority.max(evidence.path_priority);
            node.score = node.score.max(
                score + (evidence.confidence * 10.0) as i32 + evidence.path_priority / 5
                    - (evidence.path_cost as i32 / 12),
            );
        })
        .or_insert_with(|| TraceNode {
            symbol_id: sym.id.clone(),
            score: score + (evidence.confidence * 10.0) as i32 + evidence.path_priority / 5
                - (evidence.path_cost as i32 / 12),
            category,
            why: evidence.why,
            distance,
            evidence_hits: 1,
            best_path_cost: evidence.path_cost,
            best_path_priority: evidence.path_priority,
        });
}

fn classify_symbol(sym: &Symbol) -> &'static str {
    let file = sym.file.to_string_lossy().replace('\\', "/").to_lowercase();
    let name = sym.name.to_lowercase();
    let qualified = sym.qualified.to_lowercase();

    if file.contains("/main.")
        || matches!(name.as_str(), "main" | "run" | "search" | "search_parallel")
    {
        "entry"
    } else if file.contains("/ignore/")
        || file.contains("haystack")
        || qualified.contains("walk")
        || name.contains("walk")
        || name.contains("haystack")
        || name.contains("scan")
    {
        "scanning"
    } else if file.contains("/printer/")
        || file.contains("sink")
        || name.contains("print")
        || name.contains("json")
        || name.contains("summary")
        || name.contains("sink")
        || qualified.contains("printer")
    {
        "output"
    } else if file.contains("/regex/")
        || file.contains("/matcher/")
        || file.contains("/searcher/")
        || name.contains("regex")
        || name.contains("matcher")
        || name.contains("match")
        || name.contains("searcher")
    {
        "matching"
    } else {
        "orchestration"
    }
}

fn is_entry_symbol(sym: &Symbol) -> bool {
    let name = sym.name.to_lowercase();
    let file = sym.file.to_string_lossy().replace('\\', "/").to_lowercase();
    file.contains("/main.")
        || matches!(name.as_str(), "main" | "run" | "search" | "search_parallel")
}

fn is_noise_symbol(sym: &Symbol) -> bool {
    let file = sym.file.to_string_lossy().replace('\\', "/").to_lowercase();
    file.contains("/tests/")
        || file.ends_with("_test.rs")
        || file.ends_with("tests.rs")
        || file.contains("/examples/")
        || file.contains("/benches/")
        || file.contains("flags/defs")
        || file.contains("/docs/")
        || file.contains("generated")
}

fn adjusted_score(
    sym: &Symbol,
    base: i32,
    query: &str,
    profile: Option<&crate::index::repo_profile::RepoProfile>,
    project_path: &std::path::Path,
) -> i32 {
    let mut score = base;
    if is_noise_symbol(sym) {
        score -= 40;
    }
    let file = sym.file.to_string_lossy().replace('\\', "/").to_lowercase();
    if file.contains("/main.")
        || file.contains("/search.")
        || file.contains("/searcher/")
        || file.contains("/printer/")
        || file.contains("/regex/")
        || file.contains("/matcher/")
        || file.contains("/ignore/")
        || file.contains("haystack")
    {
        score += 8;
    }
    score += role_boost_for_path(project_path, sym.file.as_ref(), profile, query);
    score
}

fn confidence_label(score: i32) -> &'static str {
    if score >= 90 {
        "high"
    } else if score >= 65 {
        "medium"
    } else {
        "low"
    }
}

fn noise_reason(sym: &Symbol) -> Option<&'static str> {
    if is_noise_symbol(sym) {
        Some("symbol is in a lower-signal file such as flags/defs, tests, examples, benches, docs, or generated code")
    } else {
        None
    }
}

#[derive(Clone)]
struct ImportantSymbol {
    id: String,
    name: String,
    qualified: String,
    kind: String,
    language: String,
    file: String,
    line_start: u32,
    line_end: u32,
    signature: Option<String>,
    category: &'static str,
    why: String,
    score: i32,
    confidence: &'static str,
    noise_reason: Option<&'static str>,
    snippet: Option<String>,
    verified_by_source: bool,
    hot_path: bool,
}

impl ImportantSymbol {
    fn into_json(self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "qualified": self.qualified,
            "kind": self.kind,
            "language": self.language,
            "file": self.file,
            "line_start": self.line_start,
            "line_end": self.line_end,
            "signature": self.signature,
            "category": self.category,
            "why": self.why,
            "score": self.score,
            "confidence": self.confidence,
            "noise_reason": self.noise_reason,
            "snippet": self.snippet,
            "verified_by_source": self.verified_by_source,
            "hot_path": self.hot_path,
        })
    }
}

fn build_summary(symbols: &[ImportantSymbol], edges: &[Value]) -> Value {
    let mut by_category: HashMap<&str, Vec<&ImportantSymbol>> = HashMap::new();
    for symbol in symbols {
        by_category.entry(symbol.category).or_default().push(symbol);
    }

    fn first_symbol(items: Option<&Vec<&ImportantSymbol>>) -> Value {
        items
            .and_then(|items| items.first())
            .map_or(Value::Null, |item| {
                json!({
                    "id": item.id,
                    "qualified": item.qualified,
                    "file": item.file,
                    "line_start": item.line_start,
                    "signature": item.signature,
                })
            })
    }

    json!({
        "entry": first_symbol(by_category.get("entry")),
        "orchestration": first_symbol(by_category.get("orchestration")),
        "scanning": first_symbol(by_category.get("scanning")),
        "matching": first_symbol(by_category.get("matching")),
        "output": first_symbol(by_category.get("output")),
        "edge_count": edges.len(),
    })
}

fn select_important_symbols(
    index: &Arc<SymbolIndex>,
    nodes: &HashMap<String, TraceNode>,
    max_symbols: usize,
) -> Vec<ImportantSymbol> {
    let mut items: Vec<ImportantSymbol> = nodes
        .values()
        .filter_map(|node| {
            let sym = index.symbols.get(&node.symbol_id)?;
            Some(ImportantSymbol {
                id: sym.id.clone(),
                name: sym.name.clone(),
                qualified: sym.qualified.clone(),
                kind: sym.kind.to_string(),
                language: sym.language.to_string(),
                file: sym.file.to_string_lossy().replace('\\', "/"),
                line_start: sym.line_start,
                line_end: sym.line_end,
                signature: sym.signature.clone(),
                category: node.category,
                why: if node.evidence_hits > 1 {
                    format!("{} ({} evidence hits)", node.why, node.evidence_hits)
                } else {
                    node.why.clone()
                },
                score: node.score - (node.distance as i32 * 4) + node.best_path_priority / 6
                    - (node.best_path_cost as i32 / 14),
                confidence: confidence_label(node.score),
                noise_reason: noise_reason(sym),
                snippet: build_snippet(sym),
                verified_by_source: read_symbol_source(sym, false).is_ok(),
                hot_path: false,
            })
        })
        .collect();

    items.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.category.cmp(b.category))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });

    let category_order = ["entry", "orchestration", "scanning", "matching", "output"];
    let mut selected = Vec::new();
    let mut used_ids = HashSet::new();

    for category in category_order {
        if let Some(item) = items
            .iter()
            .find(|item| item.category == category && used_ids.insert(item.id.clone()))
            .cloned()
        {
            let mut item = item;
            item.hot_path = true;
            selected.push(item);
            if selected.len() >= max_symbols {
                return selected;
            }
        }
    }

    for item in items {
        if used_ids.insert(item.id.clone()) {
            selected.push(item);
            if selected.len() >= max_symbols {
                break;
            }
        }
    }
    selected
}

fn build_snippet(sym: &Symbol) -> Option<String> {
    let source = read_symbol_source(sym, false).ok()?;
    let snippet = source
        .lines()
        .map(|line| line.trim_end())
        .filter(|line: &&str| !line.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    if snippet.is_empty() {
        None
    } else if snippet.len() > 240 {
        Some(format!("{}...", &snippet[..240]))
    } else {
        Some(snippet)
    }
}

fn build_path_narrative(symbols: &[ImportantSymbol], shortest_path: Option<&[Value]>) -> String {
    if let Some(path) = shortest_path {
        if !path.is_empty() {
            let mut parts = Vec::new();
            for step in path {
                let name = step["name"].as_str().unwrap_or("unknown");
                if let Some(relation) = step["relation"].as_str() {
                    parts.push(format!("{} {}", relation, name));
                } else {
                    parts.push(name.to_string());
                }
            }
            return parts.join(" -> ");
        }
    }

    let category_order = ["entry", "orchestration", "scanning", "matching", "output"];
    let mut parts = Vec::new();
    for category in category_order {
        if let Some(symbol) = symbols
            .iter()
            .find(|symbol| symbol.hot_path && symbol.category == category)
        {
            parts.push(format!("{} ({})", symbol.qualified, category));
        }
    }
    if parts.is_empty() {
        "No compact path narrative available from the traced symbols.".to_string()
    } else {
        parts.join(" -> ")
    }
}

fn build_shortest_path(
    symbols: &[ImportantSymbol],
    edges: &[TraceEdge],
    query: &str,
    source_hint: Option<&str>,
    sink_hint: Option<&str>,
) -> Vec<Value> {
    let symbol_by_id: HashMap<&str, &ImportantSymbol> = symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect();
    let start = select_query_biased_start(symbols, query, source_hint)
        .or_else(|| select_path_endpoint(symbols, source_hint, true))
        .or_else(|| {
            symbols
                .iter()
                .find(|symbol| symbol.hot_path && symbol.category == "entry")
        })
        .or_else(|| symbols.first());
    let goal = select_path_endpoint(symbols, sink_hint, false)
        .or_else(|| select_query_biased_goal(symbols, query, sink_hint))
        .or_else(|| {
            symbols
                .iter()
                .rev()
                .find(|symbol| symbol.hot_path && symbol.category == "output")
        })
        .or_else(|| symbols.last());
    let (Some(start), Some(goal)) = (start, goal) else {
        return Vec::new();
    };
    if start.id == goal.id {
        return vec![json!({
            "symbol_id": start.id,
            "name": start.name,
            "qualified": start.qualified,
            "category": start.category,
        })];
    }

    let mut adjacency: HashMap<&str, Vec<&TraceEdge>> = HashMap::new();
    for edge in edges {
        adjacency
            .entry(edge.from_id.as_str())
            .or_default()
            .push(edge);
    }

    let mut frontier = vec![(start.id.clone(), 0_u32, 0_usize, 0_i32)];
    let mut best: HashMap<String, (u32, usize, i32)> =
        HashMap::from([(start.id.clone(), (0, 0, 0))]);
    let mut parent: HashMap<String, (String, TraceEdge)> = HashMap::new();

    while !frontier.is_empty() {
        let (best_idx, _) = frontier
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                left.1
                    .cmp(&right.1)
                    .then(left.2.cmp(&right.2))
                    .then(right.3.cmp(&left.3))
                    .then(left.0.cmp(&right.0))
            })
            .unwrap_or((0, &(String::new(), 0, 0, 0)));
        let (current, current_cost, current_hops, current_score) = frontier.swap_remove(best_idx);
        let Some(best_state) = best.get(&current) else {
            continue;
        };
        if *best_state != (current_cost, current_hops, current_score) {
            continue;
        }
        if current == goal.id {
            break;
        }
        let Some(outgoing) = adjacency.get(current.as_str()) else {
            continue;
        };
        for edge in outgoing {
            let next = edge.to_id.clone();
            let candidate = (
                current_cost + edge.path_cost,
                current_hops + 1,
                current_score + edge.priority,
            );
            let should_update = best
                .get(&next)
                .is_none_or(|existing| better_path(candidate, *existing));
            if should_update {
                best.insert(next.clone(), candidate);
                parent.insert(next.clone(), (current.clone(), (*edge).clone()));
                frontier.push((next, candidate.0, candidate.1, candidate.2));
            }
        }
    }

    if !best.contains_key(&goal.id) {
        return Vec::new();
    }

    let mut path = Vec::new();
    let mut cursor = goal.id.clone();
    loop {
        let current = symbol_by_id.get(cursor.as_str()).copied();
        if let Some(symbol) = current {
            let mut step = json!({
                "symbol_id": symbol.id,
                "name": symbol.name,
                "qualified": symbol.qualified,
                "category": symbol.category,
            });
            if let Some((prev, edge)) = parent.get(&cursor) {
                step["relation"] = json!(edge.relation.as_str());
                step["evidence"] = json!(edge.evidence);
                step["confidence"] = json!(edge.confidence);
                step["evidence_quality"] = json!(edge.evidence_quality);
                step["path_cost"] = json!(edge.path_cost);
                step["priority"] = json!(edge.priority);
                cursor = prev.clone();
            } else {
                path.push(step);
                break;
            }
            path.push(step);
        } else {
            break;
        }
        if cursor == start.id {
            if let Some(symbol) = symbol_by_id.get(cursor.as_str()) {
                path.push(json!({
                    "symbol_id": symbol.id,
                    "name": symbol.name,
                    "qualified": symbol.qualified,
                    "category": symbol.category,
                }));
            }
            break;
        }
    }
    path.reverse();
    path
}

fn select_query_biased_start<'a>(
    symbols: &'a [ImportantSymbol],
    query: &str,
    source_hint: Option<&str>,
) -> Option<&'a ImportantSymbol> {
    if source_hint.is_some() || !is_config_oriented_query(query) {
        return None;
    }
    symbols
        .iter()
        .filter_map(|symbol| {
            let score = config_biased_symbol_score(symbol);
            (score > 0).then_some((score, symbol))
        })
        .max_by(|(left_score, left_symbol), (right_score, right_symbol)| {
            left_score
                .cmp(right_score)
                .then(left_symbol.score.cmp(&right_symbol.score))
        })
        .map(|(_, symbol)| symbol)
}

fn select_query_biased_goal<'a>(
    symbols: &'a [ImportantSymbol],
    query: &str,
    sink_hint: Option<&str>,
) -> Option<&'a ImportantSymbol> {
    if sink_hint.is_some() || !is_config_oriented_query(query) {
        return None;
    }
    symbols
        .iter()
        .filter_map(|symbol| {
            let score = effect_biased_symbol_score(symbol);
            (score > 0).then_some((score, symbol))
        })
        .max_by(|(left_score, left_symbol), (right_score, right_symbol)| {
            left_score
                .cmp(right_score)
                .then(left_symbol.score.cmp(&right_symbol.score))
        })
        .map(|(_, symbol)| symbol)
}

fn is_config_oriented_query(query: &str) -> bool {
    let query_lower = query.to_ascii_lowercase();
    query_lower.contains("config")
        || query_lower.contains("setting")
        || query_lower.contains("env")
        || query_lower.contains("option")
}

fn config_biased_symbol_score(symbol: &ImportantSymbol) -> i32 {
    let file = symbol.file.to_ascii_lowercase();
    let qualified = symbol.qualified.to_ascii_lowercase();
    let name = symbol.name.to_ascii_lowercase();
    let mut score = 0;
    if file.contains("config") || file.contains("settings") || file.contains("env") {
        score += 30;
    }
    if file.contains("bootstrap") || file.contains("init") {
        score += 22;
    }
    if name.contains("config") || name.contains("setting") || name.contains("env") {
        score += 18;
    }
    if qualified.contains("config") || qualified.contains("setting") || qualified.contains("env") {
        score += 14;
    }
    score
}

fn effect_biased_symbol_score(symbol: &ImportantSymbol) -> i32 {
    let file = symbol.file.to_ascii_lowercase();
    let qualified = symbol.qualified.to_ascii_lowercase();
    let name = symbol.name.to_ascii_lowercase();
    let mut score = 0;
    if symbol.category == "output" {
        score += 30;
    }
    if file.contains("handler") || file.contains("route") || file.contains("http") {
        score += 18;
    }
    if name.contains("handle") || name.contains("route") || name.contains("serve") {
        score += 14;
    }
    if qualified.contains("handler") || qualified.contains("route") || qualified.contains("serve") {
        score += 10;
    }
    score
}

fn select_path_endpoint<'a>(
    symbols: &'a [ImportantSymbol],
    hint: Option<&str>,
    prefer_earlier: bool,
) -> Option<&'a ImportantSymbol> {
    let hint = hint?.trim();
    if hint.is_empty() {
        return None;
    }
    let hint_lower = hint.to_ascii_lowercase();
    let iter: Box<dyn Iterator<Item = &'a ImportantSymbol> + 'a> = if prefer_earlier {
        Box::new(symbols.iter())
    } else {
        Box::new(symbols.iter().rev())
    };

    iter.map(|symbol| (path_endpoint_match_score(symbol, &hint_lower), symbol))
        .filter(|(score, _)| *score > 0)
        .max_by(|(left_score, left_symbol), (right_score, right_symbol)| {
            left_score
                .cmp(right_score)
                .then(left_symbol.score.cmp(&right_symbol.score))
        })
        .map(|(_, symbol)| symbol)
}

fn path_endpoint_match_score(symbol: &ImportantSymbol, hint_lower: &str) -> i32 {
    let name = symbol.name.to_ascii_lowercase();
    let qualified = symbol.qualified.to_ascii_lowercase();
    let file = symbol.file.to_ascii_lowercase();
    if name == hint_lower || qualified == hint_lower {
        400
    } else if qualified.ends_with(&format!("::{hint_lower}")) {
        360
    } else if name.contains(hint_lower) {
        260
    } else if qualified.contains(hint_lower) {
        220
    } else if file.contains(hint_lower) {
        140
    } else {
        0
    }
}

fn better_path(candidate: (u32, usize, i32), existing: (u32, usize, i32)) -> bool {
    candidate.0 < existing.0
        || (candidate.0 == existing.0
            && (candidate.1 < existing.1
                || (candidate.1 == existing.1 && candidate.2 > existing.2)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::index::SymbolIndex;
    use crate::indexer::language::{Language, Symbol, SymbolKind};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_symbol(
        id: &str,
        name: &str,
        qualified: &str,
        file: &str,
        line_start: u32,
        byte_start: usize,
        byte_end: usize,
    ) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: name.to_string(),
            qualified: qualified.to_string(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file: Arc::new(PathBuf::from(file)),
            byte_start,
            byte_end,
            line_start,
            line_end: line_start,
            signature: Some(format!("fn {name}()")),
            doc: None,
        }
    }

    fn write_symbol_file(path: &Path, content: &str) -> Vec<(usize, usize)> {
        std::fs::write(path, content).unwrap();
        let mut ranges = Vec::new();
        for line in content.lines() {
            let start = content.find(line).unwrap();
            ranges.push((start, start + line.len()));
        }
        ranges
    }

    fn make_important_symbol(
        id: &str,
        qualified: &str,
        category: &'static str,
        hot_path: bool,
    ) -> ImportantSymbol {
        ImportantSymbol {
            id: id.to_string(),
            name: id.to_string(),
            qualified: qualified.to_string(),
            kind: "function".to_string(),
            language: "rust".to_string(),
            file: format!("{id}.rs"),
            line_start: 1,
            line_end: 1,
            signature: Some(format!("fn {id}()")),
            category,
            why: format!("{id} is relevant"),
            score: 100,
            confidence: "high",
            noise_reason: None,
            snippet: None,
            verified_by_source: true,
            hot_path,
        }
    }

    fn make_trace_node(
        symbol_id: &str,
        score: i32,
        category: &'static str,
        distance: usize,
        best_path_cost: u32,
        best_path_priority: i32,
    ) -> TraceNode {
        TraceNode {
            symbol_id: symbol_id.to_string(),
            score,
            category,
            why: format!("{symbol_id} is relevant"),
            distance,
            evidence_hits: 1,
            best_path_cost,
            best_path_priority,
        }
    }

    fn make_trace_edge(
        from_id: &str,
        to_id: &str,
        relation: EdgeRelation,
        evidence: &str,
        confidence: f32,
    ) -> TraceEdge {
        let metrics = navigation_edge_metrics(relation, confidence, evidence);
        TraceEdge {
            from_id: from_id.to_string(),
            to_id: to_id.to_string(),
            relation,
            evidence: evidence.to_string(),
            confidence,
            evidence_quality: metrics.evidence_quality,
            priority: metrics.priority,
            path_cost: metrics.path_cost,
        }
    }

    fn make_seed_candidate(name: &str, file: &str, path_role: &str) -> Value {
        json!({
            "id": name,
            "name": name,
            "qualified": name,
            "file": file,
            "path_role": path_role,
        })
    }

    #[test]
    fn test_build_shortest_path_prefers_higher_quality_edges() {
        let symbols = vec![
            make_important_symbol("entry", "entry", "entry", true),
            make_important_symbol("weak_mid", "weak_mid", "orchestration", false),
            make_important_symbol("strong_mid", "strong_mid", "orchestration", false),
            make_important_symbol("output", "output", "output", true),
        ];
        let edges = vec![
            make_trace_edge(
                "entry",
                "weak_mid",
                EdgeRelation::Calls,
                "identifier `weak_mid` was extracted from the source text",
                0.86,
            ),
            make_trace_edge("weak_mid", "output", EdgeRelation::Calls, "output();", 0.98),
            make_trace_edge(
                "entry",
                "strong_mid",
                EdgeRelation::Calls,
                "strong_mid();",
                0.99,
            ),
            make_trace_edge(
                "strong_mid",
                "output",
                EdgeRelation::Calls,
                "output();",
                0.98,
            ),
        ];

        let path = build_shortest_path(&symbols, &edges, "trace flow", None, None);
        let path_ids: Vec<&str> = path
            .iter()
            .filter_map(|step| step["symbol_id"].as_str())
            .collect();

        assert_eq!(path_ids, vec!["entry", "strong_mid", "output"]);
        assert!(path[1]["path_cost"].as_u64().is_some());
        assert!(path[1]["evidence_quality"].as_f64().is_some());
    }

    #[test]
    fn test_build_shortest_path_prefers_source_and_sink_hints() {
        let symbols = vec![
            make_important_symbol("entry", "entry", "entry", true),
            make_important_symbol("branch", "branch", "orchestration", true),
            make_important_symbol("leaf", "leaf", "output", true),
            make_important_symbol("printer", "printer", "output", true),
        ];
        let edges = vec![
            make_trace_edge("entry", "printer", EdgeRelation::Calls, "printer();", 0.99),
            make_trace_edge("branch", "leaf", EdgeRelation::Calls, "leaf();", 0.98),
        ];

        let path =
            build_shortest_path(&symbols, &edges, "trace flow", Some("branch"), Some("leaf"));
        let path_ids: Vec<&str> = path
            .iter()
            .filter_map(|step| step["symbol_id"].as_str())
            .collect();

        assert_eq!(path_ids, vec!["branch", "leaf"]);
    }

    #[test]
    fn test_build_shortest_path_biases_config_queries_toward_config_start() {
        let symbols = vec![
            make_important_symbol("entry", "entry", "entry", true),
            ImportantSymbol {
                id: "config_loader".to_string(),
                name: "config_loader".to_string(),
                qualified: "config_loader".to_string(),
                kind: "function".to_string(),
                language: "rust".to_string(),
                file: "config/settings.rs".to_string(),
                line_start: 1,
                line_end: 1,
                signature: Some("fn config_loader()".to_string()),
                category: "orchestration",
                why: "config loader is relevant".to_string(),
                score: 110,
                confidence: "high",
                noise_reason: None,
                snippet: None,
                verified_by_source: true,
                hot_path: false,
            },
            make_important_symbol("handler", "handler", "output", true),
        ];
        let edges = vec![make_trace_edge(
            "config_loader",
            "handler",
            EdgeRelation::Calls,
            "handler();",
            0.98,
        )];

        let path = build_shortest_path(&symbols, &edges, "config to effect", None, None);
        let path_ids: Vec<&str> = path
            .iter()
            .filter_map(|step| step["symbol_id"].as_str())
            .collect();

        assert_eq!(path_ids, vec!["config_loader", "handler"]);
    }

    #[test]
    fn test_rerank_seed_candidates_prefers_entrypoints_for_startup_query() {
        let project = std::path::Path::new("/tmp/trace-seed-startup");
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
            make_seed_candidate("load_settings", "config/settings.rs", "config"),
            make_seed_candidate("main", "main.rs", "entrypoint"),
        ];

        rerank_seed_candidates(&mut results, project, "startup flow", Some(&profile));

        assert_eq!(results[0]["name"], json!("main"));
    }

    #[test]
    fn test_rerank_seed_candidates_prefers_recent_symbol_context() {
        let project = std::path::Path::new("/tmp/trace-seed-session");
        crate::session::record_symbol(
            project,
            "handler_bootstrap",
            Some(std::path::Path::new("handlers/http.rs")),
        );
        let profile = RepoProfile {
            archetype: crate::index::repo_profile::RepoArchetype::Service,
            file_roles: HashMap::from([
                (
                    "handlers/http.rs".to_string(),
                    crate::index::repo_profile::PathRole::Handler,
                ),
                (
                    "handlers/router.rs".to_string(),
                    crate::index::repo_profile::PathRole::Handler,
                ),
            ]),
            role_counts: HashMap::from([(crate::index::repo_profile::PathRole::Handler, 2)]),
            entrypoints: vec![],
        };
        let mut results = vec![
            make_seed_candidate("handler_bootstrap", "handlers/http.rs", "handler"),
            make_seed_candidate("route_request", "handlers/router.rs", "handler"),
        ];

        rerank_seed_candidates(&mut results, project, "request flow", Some(&profile));

        assert_eq!(results[0]["name"], json!("handler_bootstrap"));
    }

    #[test]
    fn test_select_important_symbols_prefers_higher_priority_paths() {
        let mut index = SymbolIndex::new();
        index.insert(make_symbol("entry", "entry", "entry", "entry.rs", 1, 0, 12));
        index.insert(make_symbol(
            "strong_mid",
            "strong_mid",
            "strong_mid",
            "strong.rs",
            1,
            0,
            17,
        ));
        index.insert(make_symbol(
            "weak_mid", "weak_mid", "weak_mid", "weak.rs", 1, 0, 15,
        ));
        let index = Arc::new(index);

        let nodes = HashMap::from([
            (
                "entry".to_string(),
                make_trace_node("entry", 100, "entry", 0, 0, 0),
            ),
            (
                "strong_mid".to_string(),
                make_trace_node("strong_mid", 82, "orchestration", 1, 18, 48),
            ),
            (
                "weak_mid".to_string(),
                make_trace_node("weak_mid", 82, "orchestration", 1, 52, 10),
            ),
        ]);

        let important = select_important_symbols(&index, &nodes, 3);
        let ids: Vec<&str> = important.iter().map(|item| item.id.as_str()).collect();

        assert_eq!(ids, vec!["entry", "strong_mid", "weak_mid"]);
    }

    #[tokio::test]
    async fn test_trace_execution_path_returns_layered_symbols() {
        let dir = TempDir::new().unwrap();
        let main_src =
            "fn run() { search(); }\nfn search() { walk_builder(); search_path(); printer(); }\n";
        let main_path = dir.path().join("main.rs");
        let main_ranges = write_symbol_file(&main_path, main_src);
        let regex_src = "fn search_path() { regex_match(); }\nfn regex_match() {}\n";
        let regex_path = dir.path().join("regex.rs");
        let regex_ranges = write_symbol_file(&regex_path, regex_src);
        let printer_src = "fn printer() {}\n";
        let printer_path = dir.path().join("printer.rs");
        let printer_ranges = write_symbol_file(&printer_path, printer_src);
        let walk_src = "fn walk_builder() {}\n";
        let walk_path = dir.path().join("walk.rs");
        let walk_ranges = write_symbol_file(&walk_path, walk_src);

        let mut index = SymbolIndex::new();
        index.insert(make_symbol(
            "run",
            "run",
            "run",
            main_path.to_str().unwrap(),
            1,
            main_ranges[0].0,
            main_ranges[0].1,
        ));
        index.insert(make_symbol(
            "search",
            "search",
            "search",
            main_path.to_str().unwrap(),
            2,
            main_ranges[1].0,
            main_ranges[1].1,
        ));
        index.insert(make_symbol(
            "walk_builder",
            "walk_builder",
            "walk_builder",
            walk_path.to_str().unwrap(),
            1,
            walk_ranges[0].0,
            walk_ranges[0].1,
        ));
        index.insert(make_symbol(
            "search_path",
            "search_path",
            "search_path",
            regex_path.to_str().unwrap(),
            1,
            regex_ranges[0].0,
            regex_ranges[0].1,
        ));
        index.insert(make_symbol(
            "regex_match",
            "regex_match",
            "regex_match",
            regex_path.to_str().unwrap(),
            2,
            regex_ranges[1].0,
            regex_ranges[1].1,
        ));
        index.insert(make_symbol(
            "printer",
            "printer",
            "printer",
            printer_path.to_str().unwrap(),
            1,
            printer_ranges[0].0,
            printer_ranges[0].1,
        ));

        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        crate::cache::invalidate(&canonical);

        let result = trace_execution_path(TraceExecutionPathParams {
            project: dir.path().to_string_lossy().to_string(),
            query: "search".to_string(),
            source: None,
            sink: None,
            language: Some("rust".to_string()),
            file: None,
            max_symbols: Some(6),
            max_depth: Some(2),
            embed_config: None,
        })
        .await
        .unwrap();

        let important = result["important_symbols"].as_array().unwrap();
        let names: Vec<&str> = important
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert!(names.contains(&"search"));
        assert!(names.contains(&"walk_builder"));
        assert!(names.contains(&"regex_match"));
        assert!(names.contains(&"printer"));
        assert_eq!(result["summary"]["entry"]["qualified"], json!("search"));
        assert_eq!(
            result["summary"]["matching"]["qualified"],
            json!("regex_match")
        );
        assert_eq!(result["summary"]["output"]["qualified"], json!("printer"));
        assert_eq!(
            result["summary"]["entry"]["signature"],
            json!("fn search()")
        );
        assert_eq!(result["path_narrative"], json!("search -> calls printer"));
        assert!(result["shortest_path"].as_array().unwrap().len() >= 2);
        assert!(result["edges"].as_array().unwrap().iter().all(|edge| {
            edge["evidence"].is_string()
                && edge["confidence"].as_f64().is_some()
                && edge["evidence_quality"].as_f64().is_some()
                && edge["path_cost"].as_u64().is_some()
                && edge["priority"].as_i64().is_some()
        }));
        assert_eq!(important[0]["hot_path"], json!(true));
        assert_eq!(important[0]["verified_by_source"], json!(true));
        assert!(important.iter().any(|item| item["snippet"]
            .as_str()
            .is_some_and(|s| s.contains("search_path"))));
        assert_eq!(important[0]["confidence"], json!("high"));
        assert_eq!(
            result["guidance"]["next_step"],
            json!("You likely have enough to answer from the traced symbols, snippets, and summary. Only call get_symbol for one or two symbols if you need to verify a specific implementation detail.")
        );
        assert_eq!(
            result["guidance"]["answer_now_hint"],
            json!("Prefer answering from the returned important_symbols, edges, and summary before doing more discovery.")
        );
    }

    #[tokio::test]
    async fn test_trace_execution_path_filters_noisy_defs_symbols() {
        let dir = TempDir::new().unwrap();
        let main_src = "fn search() { search_path(); }\n";
        let main_path = dir.path().join("main.rs");
        let main_ranges = write_symbol_file(&main_path, main_src);
        let regex_src = "fn search_path() { regex_match(); }\nfn regex_match() {}\n";
        let regex_path = dir.path().join("regex.rs");
        let regex_ranges = write_symbol_file(&regex_path, regex_src);
        let defs_dir = dir.path().join("flags");
        std::fs::create_dir_all(&defs_dir).unwrap();
        let defs_src = "fn printer() { search_path(); }\n";
        let defs_path = defs_dir.join("defs.rs");
        let defs_ranges = write_symbol_file(&defs_path, defs_src);

        let mut index = SymbolIndex::new();
        index.insert(make_symbol(
            "search",
            "search",
            "search",
            main_path.to_str().unwrap(),
            1,
            main_ranges[0].0,
            main_ranges[0].1,
        ));
        index.insert(make_symbol(
            "search_path",
            "search_path",
            "search_path",
            regex_path.to_str().unwrap(),
            1,
            regex_ranges[0].0,
            regex_ranges[0].1,
        ));
        index.insert(make_symbol(
            "regex_match",
            "regex_match",
            "regex_match",
            regex_path.to_str().unwrap(),
            2,
            regex_ranges[1].0,
            regex_ranges[1].1,
        ));
        index.insert(make_symbol(
            "printer",
            "printer",
            "printer",
            defs_path.to_str().unwrap(),
            1,
            defs_ranges[0].0,
            defs_ranges[0].1,
        ));

        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        crate::cache::invalidate(&canonical);

        let result = trace_execution_path(TraceExecutionPathParams {
            project: dir.path().to_string_lossy().to_string(),
            query: "search".to_string(),
            source: None,
            sink: None,
            language: Some("rust".to_string()),
            file: None,
            max_symbols: Some(6),
            max_depth: Some(2),
            embed_config: None,
        })
        .await
        .unwrap();

        let important = result["important_symbols"].as_array().unwrap();
        let noisy = important
            .iter()
            .find(|item| item["name"] == json!("printer"))
            .cloned();
        if let Some(noisy) = noisy {
            assert_eq!(noisy["confidence"], json!("low"));
            assert_eq!(
                noisy["noise_reason"],
                json!("symbol is in a lower-signal file such as flags/defs, tests, examples, benches, docs, or generated code")
            );
        }
    }
}
