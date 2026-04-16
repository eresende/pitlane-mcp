use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::index::SymbolIndex;
use crate::indexer::language::{Language, Symbol, SymbolKind};

#[derive(Debug, Clone, PartialEq)]
pub struct DirectReference {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line_start: u32,
    pub evidence: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeRelation {
    Calls,
    References,
}

impl EdgeRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::References => "references",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavigationEdgeMetrics {
    pub evidence_quality: f32,
    pub priority: i32,
    pub path_cost: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationEdge {
    pub symbol_id: String,
    pub relation: EdgeRelation,
    pub evidence: String,
    pub confidence: f32,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationGraph {
    pub built: bool,
    pub outgoing: HashMap<String, Vec<NavigationEdge>>,
    pub incoming: HashMap<String, Vec<NavigationEdge>>,
}

/// Extract unique identifier tokens from source text.
/// Splits on anything that is not alphanumeric or `_`, filters tokens shorter
/// than 3 chars (to skip operators, loop vars, etc.) and pure-numeric tokens.
pub fn extract_identifiers(source: &str) -> HashSet<&str> {
    source
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3 && !s.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

pub fn read_symbol_source(sym: &Symbol, include_context: bool) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(&*sym.file)
        .map_err(|e| anyhow::anyhow!("Cannot open file {:?}: {}", sym.file, e))?;

    if include_context {
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let lines: Vec<&str> = content.lines().collect();

        let context_before = 3usize;
        let context_after = 3usize;
        let start_line = sym.line_start.saturating_sub(1) as usize;
        let end_line = sym.line_end as usize;

        let from = start_line.saturating_sub(context_before);
        let to = (end_line + context_after).min(lines.len());

        Ok(lines[from..to].join("\n"))
    } else {
        file.seek(SeekFrom::Start(sym.byte_start as u64))?;
        let len = sym.byte_end - sym.byte_start;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).to_string())
    }
}

pub fn build_navigation_graph(index: &SymbolIndex) -> NavigationGraph {
    let mut graph = NavigationGraph {
        built: true,
        ..Default::default()
    };

    for sym in index.symbols.values() {
        let source_text = match read_symbol_source(sym, false) {
            Ok(source) => source,
            Err(_) => continue,
        };
        for reference in scan_direct_references(index, sym, &source_text) {
            let Some(target) = index.symbols.get(&reference.id) else {
                continue;
            };
            let relation = classify_relation(target, reference.confidence);

            graph
                .outgoing
                .entry(sym.id.clone())
                .or_default()
                .push(NavigationEdge {
                    symbol_id: reference.id.clone(),
                    relation,
                    evidence: reference.evidence.clone(),
                    confidence: reference.confidence,
                });
            graph
                .incoming
                .entry(reference.id)
                .or_default()
                .push(NavigationEdge {
                    symbol_id: sym.id.clone(),
                    relation,
                    evidence: reference.evidence,
                    confidence: reference.confidence,
                });
        }
    }

    for bucket in graph.outgoing.values_mut() {
        normalise_edge_bucket(bucket);
    }
    for bucket in graph.incoming.values_mut() {
        normalise_edge_bucket(bucket);
    }

    graph
}

pub fn collect_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: Option<&str>,
) -> Vec<DirectReference> {
    if index.graph.built {
        return resolve_edges(index, index.graph.outgoing.get(&sym.id), None);
    }

    let owned_source;
    let source_text = match source_text {
        Some(source_text) => source_text,
        None => {
            owned_source = match read_symbol_source(sym, false) {
                Ok(source) => source,
                Err(_) => return Vec::new(),
            };
            &owned_source
        }
    };
    scan_direct_references(index, sym, source_text)
}

pub fn collect_direct_callable_references(
    index: &SymbolIndex,
    sym: &Symbol,
) -> Vec<DirectReference> {
    if index.graph.built {
        return resolve_edges(
            index,
            index.graph.outgoing.get(&sym.id),
            Some(EdgeRelation::Calls),
        )
        .into_iter()
        .filter(|reference| {
            let Some(target) = index.symbols.get(&reference.id) else {
                return false;
            };
            is_callable_kind(&target.kind) && !is_low_signal_name(&target.name)
        })
        .collect();
    }

    collect_direct_references(index, sym, None)
        .into_iter()
        .filter(|reference| {
            let Some(target) = index.symbols.get(&reference.id) else {
                return false;
            };
            classify_relation(target, reference.confidence) == EdgeRelation::Calls
        })
        .collect()
}

pub fn collect_incoming_callable_references(
    index: &SymbolIndex,
    sym: &Symbol,
) -> Vec<DirectReference> {
    if index.graph.built {
        return resolve_edges(
            index,
            index.graph.incoming.get(&sym.id),
            Some(EdgeRelation::Calls),
        )
        .into_iter()
        .filter(|reference| {
            let Some(source) = index.symbols.get(&reference.id) else {
                return false;
            };
            is_callable_kind(&source.kind) && !is_low_signal_name(&source.name)
        })
        .collect();
    }

    let mut callers = Vec::new();
    for candidate in index.symbols.values() {
        if candidate.id == sym.id
            || !is_callable_kind(&candidate.kind)
            || is_low_signal_name(&candidate.name)
        {
            continue;
        }
        let direct_refs = collect_direct_callable_references(index, candidate);
        if let Some(reference) = direct_refs.iter().find(|reference| reference.id == sym.id) {
            callers.push(DirectReference {
                id: candidate.id.clone(),
                name: candidate.name.clone(),
                kind: candidate.kind.to_string(),
                file: candidate.file.to_string_lossy().replace('\\', "/"),
                line_start: candidate.line_start,
                evidence: reference.evidence.clone(),
                confidence: reference.confidence,
            });
        }
    }
    sort_direct_references(&mut callers);
    callers
}

fn scan_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
) -> Vec<DirectReference> {
    let mut refs = Vec::new();
    let cap_generic_confidence = if sym.language == Language::Rust {
        match scan_rust_direct_references(index, sym, source_text) {
            Some(mut rust_refs) => {
                refs.append(&mut rust_refs);
                Some(0.84)
            }
            None => None,
        }
    } else {
        None
    };

    refs.extend(scan_generic_direct_references(
        index,
        sym,
        source_text,
        cap_generic_confidence,
    ));
    sort_direct_references(&mut refs);
    refs
}

fn scan_generic_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
    max_confidence: Option<f32>,
) -> Vec<DirectReference> {
    let identifiers = extract_identifiers(source_text);
    let lines: Vec<&str> = source_text.lines().collect();
    index
        .symbols
        .values()
        .filter(|candidate| candidate.id != sym.id && identifiers.contains(candidate.name.as_str()))
        .map(|candidate| {
            let (evidence, mut confidence) =
                reference_evidence(&lines, candidate.name.as_str(), &candidate.kind);
            if let Some(max_confidence) = max_confidence {
                confidence = confidence.min(max_confidence);
            }
            DirectReference {
                id: candidate.id.clone(),
                name: candidate.name.clone(),
                kind: candidate.kind.to_string(),
                file: candidate.file.to_string_lossy().replace('\\', "/"),
                line_start: candidate.line_start,
                evidence,
                confidence,
            }
        })
        .collect()
}

fn resolve_edges(
    index: &SymbolIndex,
    edges: Option<&Vec<NavigationEdge>>,
    relation: Option<EdgeRelation>,
) -> Vec<DirectReference> {
    let mut refs: Vec<DirectReference> = edges
        .into_iter()
        .flatten()
        .filter(|edge| relation.is_none_or(|expected| edge.relation == expected))
        .filter_map(|edge| {
            let target = index.symbols.get(&edge.symbol_id)?;
            Some(DirectReference {
                id: target.id.clone(),
                name: target.name.clone(),
                kind: target.kind.to_string(),
                file: target.file.to_string_lossy().replace('\\', "/"),
                line_start: target.line_start,
                evidence: edge.evidence.clone(),
                confidence: edge.confidence,
            })
        })
        .collect();
    sort_direct_references(&mut refs);
    refs
}

fn normalise_edge_bucket(bucket: &mut Vec<NavigationEdge>) {
    bucket.sort_by(|a, b| {
        relation_rank(b.relation)
            .cmp(&relation_rank(a.relation))
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.symbol_id.cmp(&b.symbol_id))
    });
    bucket.dedup_by(|a, b| a.symbol_id == b.symbol_id);
}

fn relation_rank(relation: EdgeRelation) -> u8 {
    match relation {
        EdgeRelation::Calls => 2,
        EdgeRelation::References => 1,
    }
}

pub fn edge_evidence_quality(evidence: &str) -> f32 {
    let trimmed = evidence.trim();
    if trimmed.is_empty() {
        return 0.15;
    }

    let mut quality: f32 = if trimmed.starts_with("identifier `") {
        0.18
    } else {
        0.35
    };

    if trimmed.contains('(') {
        quality += 0.22;
    }
    if trimmed.contains("::") {
        quality += 0.12;
    }
    if trimmed.contains('.') {
        quality += 0.08;
    }
    if trimmed.len() >= 12 && trimmed.len() <= 160 {
        quality += 0.08;
    }
    if trimmed.starts_with("//") {
        quality -= 0.2;
    }

    quality.clamp(0.1, 1.0)
}

pub fn navigation_edge_metrics(
    relation: EdgeRelation,
    confidence: f32,
    evidence: &str,
) -> NavigationEdgeMetrics {
    let confidence = confidence.clamp(0.0, 1.0);
    let evidence_quality = edge_evidence_quality(evidence);
    let priority_base = match relation {
        EdgeRelation::Calls => 90,
        EdgeRelation::References => 45,
    };
    let path_base = match relation {
        EdgeRelation::Calls => 95,
        EdgeRelation::References => 170,
    };
    let priority = priority_base
        + (confidence * 25.0).round() as i32
        + (evidence_quality * 20.0).round() as i32;
    let path_cost =
        (path_base - (confidence * 40.0).round() as i32 - (evidence_quality * 30.0).round() as i32)
            .clamp(10, 220) as u32;

    NavigationEdgeMetrics {
        evidence_quality,
        priority,
        path_cost,
    }
}

fn classify_relation(target: &Symbol, confidence: f32) -> EdgeRelation {
    if is_callable_kind(&target.kind)
        && (confidence >= 0.97 || (!is_low_signal_name(&target.name) && confidence >= 0.86))
    {
        EdgeRelation::Calls
    } else {
        EdgeRelation::References
    }
}

fn sort_direct_references(refs: &mut Vec<DirectReference>) {
    refs.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.id.cmp(&b.id))
    });
    refs.dedup_by(|a, b| a.id == b.id);
}

fn reference_evidence(lines: &[&str], name: &str, kind: &SymbolKind) -> (String, f32) {
    for line in lines {
        if line.contains(name) {
            let trimmed = line.trim();
            let confidence = if trimmed.contains('(') && !trimmed.starts_with("//") {
                if matches!(kind, SymbolKind::Function | SymbolKind::Method) {
                    0.98
                } else {
                    0.9
                }
            } else if trimmed.contains("::") {
                0.86
            } else {
                0.8
            };
            return (trimmed.chars().take(240).collect::<String>(), confidence);
        }
    }

    (
        format!("identifier `{name}` was extracted from the source text"),
        0.72,
    )
}

fn scan_rust_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
) -> Option<Vec<DirectReference>> {
    let matches = collect_rust_call_matches(source_text)?;
    let mut refs = Vec::new();
    for matched in matches {
        refs.extend(
            index
                .symbols
                .values()
                .filter(|candidate| candidate.id != sym.id && candidate.name == matched.name)
                .map(|candidate| DirectReference {
                    id: candidate.id.clone(),
                    name: candidate.name.clone(),
                    kind: candidate.kind.to_string(),
                    file: candidate.file.to_string_lossy().replace('\\', "/"),
                    line_start: candidate.line_start,
                    evidence: matched.evidence.clone(),
                    confidence: matched.confidence,
                }),
        );
    }
    sort_direct_references(&mut refs);
    Some(refs)
}

#[derive(Debug, Clone)]
struct RustCallMatch {
    name: String,
    evidence: String,
    confidence: f32,
}

fn collect_rust_call_matches(source_text: &str) -> Option<Vec<RustCallMatch>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source_text, None)?;

    let mut matches = Vec::new();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        match node.kind() {
            "call_expression" => {
                let target = node
                    .child_by_field_name("function")
                    .or_else(|| node.named_child(0));
                if let Some(name) =
                    target.and_then(|target| rust_callable_name(source_text, target))
                {
                    matches.push(RustCallMatch {
                        name,
                        evidence: node_evidence(source_text, node),
                        confidence: 0.99,
                    });
                }
            }
            "method_call_expression" => {
                let name = node
                    .child_by_field_name("method")
                    .or_else(|| node.child_by_field_name("name"))
                    .and_then(|target| rust_callable_name(source_text, target))
                    .or_else(|| last_rust_identifier(source_text, node));
                if let Some(name) = name {
                    matches.push(RustCallMatch {
                        name,
                        evidence: node_evidence(source_text, node),
                        confidence: 0.99,
                    });
                }
            }
            "macro_invocation" => {
                let name = node
                    .child_by_field_name("macro")
                    .and_then(|target| rust_callable_name(source_text, target))
                    .or_else(|| last_rust_identifier(source_text, node));
                if let Some(name) = name {
                    matches.push(RustCallMatch {
                        name,
                        evidence: node_evidence(source_text, node),
                        confidence: 0.98,
                    });
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    Some(matches)
}

fn rust_callable_name(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(source_text, node)),
        "scoped_identifier" => node
            .child_by_field_name("name")
            .and_then(|child| rust_callable_name(source_text, child))
            .or_else(|| last_rust_identifier(source_text, node)),
        "field_expression" => node
            .child_by_field_name("field")
            .and_then(|child| rust_callable_name(source_text, child))
            .or_else(|| last_rust_identifier(source_text, node)),
        "generic_function"
        | "await_expression"
        | "try_expression"
        | "reference_expression"
        | "parenthesized_expression" => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            children
                .into_iter()
                .rev()
                .find_map(|child| rust_callable_name(source_text, child))
        }
        _ => None,
    }
}

fn last_rust_identifier(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(source_text, node)),
        _ => {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            children
                .into_iter()
                .rev()
                .find_map(|child| last_rust_identifier(source_text, child))
        }
    }
}

fn node_text(source_text: &str, node: Node<'_>) -> String {
    node.utf8_text(source_text.as_bytes())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn node_evidence(source_text: &str, node: Node<'_>) -> String {
    let row = node.start_position().row;
    source_text
        .lines()
        .nth(row)
        .map(|line| line.trim().chars().take(240).collect())
        .filter(|line: &String| !line.is_empty())
        .unwrap_or_else(|| node_text(source_text, node).chars().take(240).collect())
}

pub fn is_callable_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro | SymbolKind::Class
    )
}

pub fn is_low_signal_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "main"
            | "run"
            | "build"
            | "new"
            | "default"
            | "fmt"
            | "from"
            | "into"
            | "clone"
            | "copy"
            | "eq"
            | "ne"
            | "hash"
            | "len"
            | "clear"
            | "args"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;

    fn build_index(source: &str) -> SymbolIndex {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        index
    }

    fn outgoing_edge<'a>(
        index: &'a SymbolIndex,
        from_name: &str,
        to_name: &str,
    ) -> Option<&'a NavigationEdge> {
        let from = index
            .symbols
            .values()
            .find(|symbol| symbol.name == from_name)
            .unwrap();
        let to = index
            .symbols
            .values()
            .find(|symbol| symbol.name == to_name)
            .unwrap();
        index
            .graph
            .outgoing
            .get(&from.id)
            .into_iter()
            .flatten()
            .find(|edge| edge.symbol_id == to.id)
    }

    #[test]
    fn test_build_navigation_graph_keeps_callable_argument_as_reference() {
        let index = build_index(
            "fn helper() {}\nfn wrapper(f: fn()) { f(); }\nfn root() { wrapper(helper); }\n",
        );

        let wrapper_edge = outgoing_edge(&index, "root", "wrapper").unwrap();
        assert_eq!(wrapper_edge.relation, EdgeRelation::Calls);
        assert!(wrapper_edge.evidence.contains("wrapper(helper)"));

        let helper_edge = outgoing_edge(&index, "root", "helper").unwrap();
        assert_eq!(helper_edge.relation, EdgeRelation::References);
        assert!(helper_edge.evidence.contains("wrapper(helper)"));
    }

    #[test]
    fn test_build_navigation_graph_extracts_rust_method_calls_for_low_signal_names() {
        let index = build_index(
            "struct Worker;\nimpl Worker { fn run(&self) {} }\nfn root(worker: &Worker) { worker.run(); }\n",
        );

        let run_edge = outgoing_edge(&index, "root", "run").unwrap();
        assert_eq!(run_edge.relation, EdgeRelation::Calls);
        assert!(run_edge.evidence.contains("worker.run();"));
    }
}
