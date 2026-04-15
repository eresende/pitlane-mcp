use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::embed::EmbedConfig;
use crate::graph::{collect_direct_callable_references, is_callable_kind, read_symbol_source};
use crate::index::format::load_project_meta;
use crate::index::repo_profile::role_boost_for_path;
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
}

#[derive(Clone)]
struct TraceEdge {
    from_id: String,
    to_id: String,
    relation: &'static str,
    evidence: String,
    confidence: f32,
}

struct TraceContext<'a> {
    project_path: &'a std::path::Path,
    profile: Option<&'a crate::index::repo_profile::RepoProfile>,
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
    let mut seen_edges: HashSet<(String, String, &'static str)> = HashSet::new();
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
            1.0,
            "discovered as a strong seed for the requested behavior".to_string(),
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
                "relation": edge.relation,
                "evidence": edge.evidence,
                "confidence": edge.confidence,
            })
        })
        .collect();
    let shortest_path = build_shortest_path(&important, &important_edge_records);
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

async fn discover_seed_ids(
    params: &TraceExecutionPathParams,
) -> anyhow::Result<(&'static str, Vec<String>)> {
    let mut ids = Vec::new();
    let mut discovered_modes = Vec::new();

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
            kind: Some("function".to_string()),
            language: params.language.clone(),
            file: params.file.clone(),
            limit: Some(DEFAULT_SEED_COUNT),
            offset: Some(0),
            mode: Some("semantic".to_string()),
            embed_config: params.embed_config.clone(),
        })
        .await;

        if let Ok(response) = semantic {
            let candidate_ids = extract_symbol_ids(&response);
            if !candidate_ids.is_empty() {
                discovered_modes.push("semantic");
                ids.extend(candidate_ids);
                continue;
            }
        }

        let bm25 = search_symbols(SearchSymbolsParams {
            project: params.project.clone(),
            query: query.to_string(),
            kind: Some("function".to_string()),
            language: params.language.clone(),
            file: params.file.clone(),
            limit: Some(DEFAULT_SEED_COUNT),
            offset: Some(0),
            mode: Some("bm25".to_string()),
            embed_config: params.embed_config.clone(),
        })
        .await?;
        let candidate_ids = extract_symbol_ids(&bm25);
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

fn trace_callers(
    index: &Arc<SymbolIndex>,
    seed: &Symbol,
    nodes: &mut HashMap<String, TraceNode>,
    edges: &mut Vec<TraceEdge>,
    seen_edges: &mut HashSet<(String, String, &'static str)>,
    ctx: &TraceContext<'_>,
    depth: usize,
) {
    if depth > 1 {
        return;
    }
    for candidate in index.symbols.values() {
        if candidate.id == seed.id || !is_callable_kind(&candidate.kind) {
            continue;
        }
        if is_noise_symbol(candidate) && !is_entry_symbol(candidate) {
            continue;
        }
        let source_text = match read_symbol_source(candidate, false) {
            Ok(source) => source,
            Err(_) => continue,
        };
        let refs = collect_direct_callable_references(index, candidate, &source_text);
        if let Some(reference) = refs.iter().find(|reference| reference.id == seed.id) {
            upsert_node(
                nodes,
                candidate,
                adjusted_score(
                    candidate,
                    90 - depth as i32 * 10 + (reference.confidence * 10.0) as i32,
                    &seed.qualified,
                    ctx.profile,
                    ctx.project_path,
                ),
                classify_symbol(candidate),
                depth + 1,
                reference.confidence,
                format!(
                    "direct caller of {}: {}",
                    seed.qualified, reference.evidence
                ),
            );
            push_edge(
                seen_edges,
                edges,
                &candidate.id,
                &seed.id,
                "calls",
                reference.evidence.clone(),
                reference.confidence,
            );
        }
    }
}

fn trace_callees(
    index: &Arc<SymbolIndex>,
    seed: &Symbol,
    max_depth: usize,
    nodes: &mut HashMap<String, TraceNode>,
    edges: &mut Vec<TraceEdge>,
    seen_edges: &mut HashSet<(String, String, &'static str)>,
    ctx: &TraceContext<'_>,
) {
    let mut queue = VecDeque::from([(seed.id.clone(), 0usize)]);
    let mut seen = HashSet::from([seed.id.clone()]);

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let Some(current) = index.symbols.get(&current_id) else {
            continue;
        };
        let source_text = match read_symbol_source(current, false) {
            Ok(source) => source,
            Err(_) => continue,
        };
        for reference in collect_direct_callable_references(index, current, &source_text) {
            let Some(target) = index.symbols.get(&reference.id) else {
                continue;
            };
            if is_noise_symbol(target) && !is_entry_symbol(target) {
                continue;
            }
            upsert_node(
                nodes,
                target,
                adjusted_score(
                    target,
                    80 - depth as i32 * 10 + (reference.confidence * 10.0) as i32,
                    &current.qualified,
                    ctx.profile,
                    ctx.project_path,
                ),
                classify_symbol(target),
                depth + 1,
                reference.confidence,
                format!(
                    "direct callee of {}: {}",
                    current.qualified, reference.evidence
                ),
            );
            push_edge(
                seen_edges,
                edges,
                &current.id,
                &target.id,
                "calls",
                reference.evidence.clone(),
                reference.confidence,
            );
            if seen.insert(target.id.clone()) {
                queue.push_back((target.id.clone(), depth + 1));
            }
        }
    }
}

fn push_edge(
    seen_edges: &mut HashSet<(String, String, &'static str)>,
    edges: &mut Vec<TraceEdge>,
    from_id: &str,
    to_id: &str,
    relation: &'static str,
    evidence: String,
    confidence: f32,
) {
    let key = (from_id.to_string(), to_id.to_string(), relation);
    if seen_edges.insert(key.clone()) {
        edges.push(TraceEdge {
            from_id: key.0,
            to_id: key.1,
            relation,
            evidence,
            confidence,
        });
    }
}

fn upsert_node(
    nodes: &mut HashMap<String, TraceNode>,
    sym: &Symbol,
    score: i32,
    category: &'static str,
    distance: usize,
    evidence_confidence: f32,
    why: String,
) {
    nodes
        .entry(sym.id.clone())
        .and_modify(|node| {
            if score > node.score {
                node.score = score;
                node.category = category;
                node.why = why.clone();
            }
            node.distance = node.distance.min(distance);
            node.evidence_hits += 1;
            node.score = node.score.max(score + (evidence_confidence * 10.0) as i32);
        })
        .or_insert_with(|| TraceNode {
            symbol_id: sym.id.clone(),
            score: score + (evidence_confidence * 10.0) as i32,
            category,
            why,
            distance,
            evidence_hits: 1,
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
                score: node.score - (node.distance as i32 * 4),
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
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
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

fn build_shortest_path(symbols: &[ImportantSymbol], edges: &[TraceEdge]) -> Vec<Value> {
    let symbol_by_id: HashMap<&str, &ImportantSymbol> = symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect();
    let start = symbols
        .iter()
        .find(|symbol| symbol.hot_path && symbol.category == "entry")
        .or_else(|| symbols.first());
    let goal = symbols
        .iter()
        .rev()
        .find(|symbol| symbol.hot_path && symbol.category == "output")
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

    let mut queue = VecDeque::from([start.id.as_str()]);
    let mut visited: HashSet<&str> = HashSet::from([start.id.as_str()]);
    let mut parent: HashMap<&str, (&str, &TraceEdge)> = HashMap::new();

    while let Some(current) = queue.pop_front() {
        if current == goal.id.as_str() {
            break;
        }
        let Some(outgoing) = adjacency.get(current) else {
            continue;
        };
        for edge in outgoing {
            let next = edge.to_id.as_str();
            if visited.insert(next) {
                parent.insert(next, (current, edge));
                queue.push_back(next);
            }
        }
    }

    if !visited.contains(goal.id.as_str()) {
        return Vec::new();
    }

    let mut path = Vec::new();
    let mut cursor = goal.id.as_str();
    loop {
        let current = symbol_by_id.get(cursor).copied();
        if let Some(symbol) = current {
            let mut step = json!({
                "symbol_id": symbol.id,
                "name": symbol.name,
                "qualified": symbol.qualified,
                "category": symbol.category,
            });
            if let Some(&(prev, edge)) = parent.get(cursor) {
                step["relation"] = json!(edge.relation);
                step["evidence"] = json!(edge.evidence);
                step["confidence"] = json!(edge.confidence);
                cursor = prev;
            } else {
                path.push(step);
                break;
            }
            path.push(step);
        } else {
            break;
        }
        if cursor == start.id.as_str() {
            if let Some(symbol) = symbol_by_id.get(cursor) {
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
        assert!(result["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|edge| { edge["evidence"].is_string() && edge["confidence"].as_f64().is_some() }));
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
